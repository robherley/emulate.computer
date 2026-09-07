//! Top-level machine: one hart + bus, bounded-slice run loop, host I/O.

use crate::bus::Bus;
use crate::cpu::{csr, Cpu, StepResult};
use crate::devices::framebuffer::{self, FramebufferView, FB_FIRST_PAGE, FB_RAM_OFFSET, FB_STRIDE};
use crate::devices::net::NetBackend;
use crate::devices::virtio_blk::{BlockBackend, BlockError};
use crate::devices::virtio_input::{ABS_X, ABS_Y, EV_ABS, EV_KEY, EV_REL, REL_WHEEL};
use crate::devices::virtio_net::FRAME_QUEUE_CAPACITY;
use crate::trap::irq;

/// mtime frequency advertised in the DTB (`timebase-frequency`), Hz.
pub const TIMEBASE_FREQ: u64 = 10_000_000;

/// Largest wall-clock advance accepted from one host scheduling slice.
///
/// Discarding excess elapsed time prevents a suspended browser tab or laptop
/// from injecting minutes of timer progress into the guest on resume.
pub(crate) const MAX_HOST_MTIME_ADVANCE: u64 = TIMEBASE_FREQ / 10;

/// mtime ticks per millisecond, for the network backend's `now_ms`.
const TICKS_PER_MS: u64 = TIMEBASE_FREQ / 1_000;

/// Instructions between network ticks inside [`Machine::run`]. Servicing the
/// backend is too expensive to do per instruction and too latency-sensitive to
/// do once per slice.
const NET_TICK_INTERVAL: u64 = 4096;

/// Version of the machine-level snapshot section (bus latches + timekeeping
/// mode). Independent of the container's own format version.
const MACHINE_SECTION_VERSION: u16 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunEvent {
    /// Executed the full instruction budget.
    BudgetExhausted,
    /// Guest is in WFI with no pending interrupt. The host should advance
    /// mtime (wall clock) and resume; `next_timer_deadline` says when the
    /// next timer interrupt is due.
    Wfi,
    /// Guest requested shutdown via the SiFive test finisher, with exit code.
    Shutdown(u32),
    /// Guest requested reset.
    Reset,
}

pub struct Machine {
    pub cpu: Cpu,
    pub bus: Bus,
    instruction_time: bool,
    mtime_phase: u8,
    last_host_mtime: u64,
    /// Host network attachment. `None` = link down: transmitted frames are
    /// discarded and nothing is ever delivered to the guest.
    net: Option<Box<dyn NetBackend>>,
    /// Which framebuffer rows the host has already been shown. Host-side view
    /// state, not guest state: never snapshotted, reset on restore.
    framebuffer: FramebufferView,
}

impl Machine {
    pub fn new(ram_size: usize) -> Self {
        Machine {
            cpu: Cpu::new(),
            bus: Bus::new(ram_size),
            instruction_time: false,
            mtime_phase: 0,
            last_host_mtime: 0,
            net: None,
            framebuffer: FramebufferView::new(),
        }
    }

    /// QEMU-virt-style system profile used for OS boots.
    ///
    /// Unlike [`Machine::new`], which reports zero PMP entries for
    /// architectural tests, this enables the compatibility PMP register bank:
    /// xv6's machine-mode startup assumes PMP CSRs exist and opens the whole
    /// address space before installing `mtvec`.
    pub fn new_system(ram_size: usize) -> Self {
        let mut machine = Self::new(ram_size);
        machine.cpu.csr.enable_pmp_stubs();
        machine
    }

    /// Load a blob at a physical address (must land in RAM).
    pub fn load_blob(&mut self, paddr: u64, blob: &[u8]) -> Result<(), ()> {
        if paddr < crate::bus::DRAM_BASE {
            return Err(());
        }
        self.bus.ram.load_blob(paddr - crate::bus::DRAM_BASE, blob)
    }

    pub fn read_phys(&mut self, paddr: u64, size: u8) -> Result<u64, ()> {
        self.bus.read(paddr, size)
    }

    pub fn write_phys(&mut self, paddr: u64, val: u64, size: u8) -> Result<(), ()> {
        self.bus.write(paddr, val, size)
    }

    /// Attach a volatile block image to the VirtIO-MMIO disk device.
    pub fn set_disk(&mut self, image: &[u8]) {
        self.bus.virtio_blk.set_disk(image);
    }

    /// Attach a host-provided block backend to the VirtIO-MMIO disk device.
    pub fn set_disk_backend(&mut self, backend: Box<dyn BlockBackend>) {
        self.bus.virtio_blk.set_backend(backend);
    }

    /// Flush guest disk writes through to the attached backend.
    pub fn flush_disk(&mut self) -> Result<(), BlockError> {
        self.bus.virtio_blk.flush()
    }

    /// Total guest RAM, in bytes.
    pub fn ram_bytes(&self) -> u64 {
        self.bus.ram.size()
    }

    /// Guest RAM ever written, as a high-water mark: pages are counted on
    /// first write and never uncounted, so this is *not* live usage.
    pub fn ram_touched_bytes(&self) -> u64 {
        self.bus.ram.touched_bytes()
    }

    /// Cumulative VirtIO-blk payload bytes read by the guest.
    pub fn disk_bytes_read(&self) -> u64 {
        self.bus.virtio_blk.bytes_read()
    }

    /// Cumulative VirtIO-blk payload bytes written by the guest.
    pub fn disk_bytes_written(&self) -> u64 {
        self.bus.virtio_blk.bytes_written()
    }

    /// Cumulative VirtIO-net frame bytes delivered to the guest.
    pub fn net_bytes_rx(&self) -> u64 {
        self.bus.virtio_net.rx_bytes()
    }

    /// Cumulative VirtIO-net frame bytes handed to the host backend.
    pub fn net_bytes_tx(&self) -> u64 {
        self.bus.virtio_net.tx_bytes()
    }

    // ---- network ----

    /// Attach a host network backend to the VirtIO-MMIO Ethernet device.
    ///
    /// Until one is attached the device behaves as an unplugged NIC.
    pub fn set_net_backend(&mut self, backend: Box<dyn NetBackend>) {
        self.net = Some(backend);
    }

    #[cfg(test)]
    pub fn has_net_backend(&self) -> bool {
        self.net.is_some()
    }

    /// Earliest millisecond deadline the network backend wants to be polled
    /// at, in the same milliseconds as [`NetBackend::poll`]'s `now_ms`.
    ///
    /// Hosts sleeping on WFI should cap that sleep here so retransmit and DHCP
    /// timers still fire while the guest is idle. `None` = network idle.
    pub fn net_deadline_ms(&self) -> Option<u64> {
        self.net.as_deref().and_then(NetBackend::next_deadline_ms)
    }

    /// Current guest time in the milliseconds handed to the backend.
    pub fn net_now_ms(&self) -> u64 {
        self.bus.clint.mtime / TICKS_PER_MS
    }

    /// Move frames across the host boundary in both directions and let the
    /// backend run its timers. Decoupled from the guest's queue notify so the
    /// bus, and every MMIO store through it, stays free of host I/O.
    pub fn tick_net(&mut self) {
        self.bus.device_irqs_dirty = true;
        let Some(backend) = self.net.as_deref_mut() else {
            return;
        };
        while let Some(frame) = self.bus.virtio_net.pop_transmitted() {
            backend.transmit(&frame);
        }
        backend.poll(self.bus.clint.mtime / TICKS_PER_MS);
        // Drain the inbox first so the pull below sees real free space, then
        // pull only what fits: frames left with the backend stay flow-
        // controlled there, while overfilling the inbox would drop segments
        // the NAT's TCP stack believes it delivered.
        self.bus.virtio_net.deliver_rx(&mut self.bus.ram);
        let space = FRAME_QUEUE_CAPACITY.saturating_sub(self.bus.virtio_net.pending_rx());
        for _ in 0..space {
            let Some(frame) = backend.receive() else {
                break;
            };
            self.bus.virtio_net.receive_frame(frame);
        }
        self.bus.virtio_net.deliver_rx(&mut self.bus.ram);
    }

    /// Number of host entropy bytes the bounded VirtIO RNG buffer can accept.
    pub fn entropy_needed(&self) -> usize {
        self.bus.virtio_rng.entropy_needed()
    }

    /// Inject host CSPRNG output and service pending VirtIO RNG requests.
    /// Returns the number of bytes accepted.
    pub fn add_entropy(&mut self, bytes: &[u8]) -> usize {
        self.bus.virtio_rng.add_entropy(bytes, &mut self.bus.ram)
    }

    // ---- display (docs/display.md) ----

    /// Rows of the framebuffer written since the last call, in ascending
    /// order, appended to `out`.
    ///
    /// Each row combines the generations of its two backing RAM pages.
    /// It is `&mut self` because reporting a row is what marks it seen.
    pub fn framebuffer_dirty_rows(&mut self, out: &mut Vec<u32>) {
        if self.bus.framebuffer_blitter.take_geometry_changed() {
            self.framebuffer.mark_all_dirty();
        }
        let height = self.bus.framebuffer_blitter.size().1 as usize;
        if self.bus.framebuffer_blitter.committed() {
            self.framebuffer
                .scan(&self.bus.framebuffer_blitter.generations()[..height], out);
            return;
        }
        let pages = self
            .bus
            .ram
            .page_generations(FB_FIRST_PAGE, height * framebuffer::FB_PAGES_PER_ROW);
        let generations: Vec<u64> = pages
            .chunks_exact(framebuffer::FB_PAGES_PER_ROW)
            .map(|pages| pages.iter().fold(0u64, |sum, n| sum.wrapping_add(*n)))
            .collect();
        self.framebuffer.scan(&generations, out);
    }

    /// Report every row as dirty at the next scan. Used after a restore, where
    /// the RAM under the framebuffer was replaced wholesale.
    pub fn framebuffer_mark_all_dirty(&mut self) {
        self.framebuffer.mark_all_dirty();
    }

    /// Whether this machine has enough DRAM to hold the framebuffer at all.
    /// A machine built for an architectural test does not.
    pub fn has_framebuffer(&self) -> bool {
        self.bus.ram.size() >= FB_RAM_OFFSET + framebuffer::FB_BYTES
    }

    /// One scanline of raw guest pixels (`x8r8g8b8`), or an empty slice for a
    /// row outside the framebuffer.
    pub fn framebuffer_row(&self, row: u32) -> &[u8] {
        if row >= self.bus.framebuffer_blitter.size().1 {
            return &[];
        }
        if self.bus.framebuffer_blitter.committed() {
            return self.bus.framebuffer_blitter.row(row);
        }
        let offset = FB_RAM_OFFSET + u64::from(row) * u64::from(FB_STRIDE);
        self.bus
            .ram
            .read_slice(offset, FB_STRIDE as usize)
            .unwrap_or(&[])
    }

    /// Convert the listed rows into RGBA, packed back to back into `dst` in
    /// the order given. Returns the number of bytes written.
    ///
    /// Host conversion avoids doing the same pixel swizzle in guest software.
    pub fn framebuffer_copy_rows_rgba(&self, rows: &[u32], dst: &mut [u8]) -> usize {
        let stride = self.bus.framebuffer_blitter.size().0 as usize * 4;
        let mut written = 0;
        for &row in rows {
            let Some(slot) = dst.get_mut(written..written + stride) else {
                break;
            };
            framebuffer::row_to_rgba(self.framebuffer_row(row), slot);
            written += stride;
        }
        written
    }

    // ---- input (docs/display.md) ----

    /// Press or release one evdev key code on the virtio keyboard.
    /// `code` is a `KEY_*` value; the browser maps `KeyboardEvent.code` to it.
    pub fn input_key(&mut self, code: u16, pressed: bool) {
        self.bus
            .virtio_kbd
            .queue_event(EV_KEY, code, u32::from(pressed));
        self.bus.virtio_kbd.flush(&mut self.bus.ram);
    }

    /// Move the tablet's absolute pointer. `x`/`y` are in the 0..=32767 range
    /// the device advertises (`ABS_MAX`), not pixels.
    pub fn input_pointer_abs(&mut self, x: u16, y: u16) {
        self.bus
            .virtio_tablet
            .queue_event(EV_ABS, ABS_X, u32::from(x));
        self.bus
            .virtio_tablet
            .queue_event(EV_ABS, ABS_Y, u32::from(y));
        self.bus.virtio_tablet.flush(&mut self.bus.ram);
    }

    /// Press or release a mouse button (`BTN_LEFT`/`BTN_RIGHT`/`BTN_MIDDLE`)
    /// on the tablet.
    pub fn input_button(&mut self, code: u16, pressed: bool) {
        self.bus
            .virtio_tablet
            .queue_event(EV_KEY, code, u32::from(pressed));
        self.bus.virtio_tablet.flush(&mut self.bus.ram);
    }

    /// Scroll by `delta` wheel detents (positive is away from the user).
    pub fn input_wheel(&mut self, delta: i32) {
        if delta == 0 {
            return;
        }
        self.bus
            .virtio_tablet
            .queue_event(EV_REL, REL_WHEEL, delta as u32);
        self.bus.virtio_tablet.flush(&mut self.bus.ram);
    }

    // ---- state snapshots (docs/disk.md) ----

    /// Serialize CPU + CSRs and every device. RAM and the disk overlay are
    /// written by [`Machine::snapshot`] around this.
    pub(crate) fn snapshot_cpu(&self, out: &mut crate::snapshot::Writer) {
        self.cpu.snapshot(out);
    }

    pub(crate) fn restore_cpu(
        &mut self,
        input: &mut crate::snapshot::Reader<'_>,
    ) -> Result<(), String> {
        self.cpu.restore(input)
    }

    /// Every device, in a fixed order, plus the two bus-level latches the run
    /// loop reads (`shutdown`, `reset_requested`) and the machine's own
    /// timekeeping mode.
    pub(crate) fn snapshot_devices(&self, out: &mut crate::snapshot::Writer) {
        self.bus.clint.snapshot(out);
        self.bus.plic.snapshot(out);
        self.bus.uart.snapshot(out);
        self.bus.rtc.snapshot(out);
        self.bus.virtio_blk.snapshot(out);
        self.bus.virtio_console.snapshot(out);
        self.bus.virtio_rng.snapshot(out);
        self.bus.virtio_net.snapshot(out);
        self.bus.virtio_kbd.snapshot(out);
        self.bus.virtio_tablet.snapshot(out);
        self.bus.simple_irq.snapshot(out);
        self.bus.framebuffer_blitter.snapshot(out);
        out.section(MACHINE_SECTION_VERSION, |w| {
            match self.bus.shutdown {
                Some(code) => {
                    w.bool(true);
                    w.u32(code);
                }
                None => {
                    w.bool(false);
                    w.u32(0);
                }
            }
            w.bool(self.bus.reset_requested);
            w.bool(self.instruction_time);
            w.u8(self.mtime_phase);
        });
    }

    pub(crate) fn restore_devices(
        &mut self,
        input: &mut crate::snapshot::Reader<'_>,
    ) -> Result<(), String> {
        self.bus.clint.restore(input)?;
        self.bus.plic.restore(input)?;
        self.bus.uart.restore(input)?;
        self.bus.rtc.restore(input)?;
        self.bus.virtio_blk.restore(input)?;
        self.bus.virtio_console.restore(input)?;
        self.bus.virtio_rng.restore(input)?;
        self.bus.virtio_net.restore(input)?;
        self.bus.virtio_kbd.restore(input)?;
        self.bus.virtio_tablet.restore(input)?;
        self.bus.simple_irq.restore(input)?;
        self.bus.framebuffer_blitter.restore(input)?;
        input.section("machine", MACHINE_SECTION_VERSION, |r| {
            let shutting_down = r.bool()?;
            let code = r.u32()?;
            self.bus.shutdown = shutting_down.then_some(code);
            self.bus.reset_requested = r.bool()?;
            self.instruction_time = r.bool()?;
            self.mtime_phase = r.u8()?;
            Ok(())
        })
    }

    /// Forget the host-clock baseline `advance_mtime_from_host` takes deltas
    /// against. A restored machine inherits the captured guest's `mtime` but
    /// none of its host's monotonic clock, so the next sample must seed a
    /// fresh baseline instead of being applied as an elapsed interval.
    pub(crate) fn reset_host_mtime_baseline(&mut self) {
        self.last_host_mtime = 0;
    }

    /// Reset-vector setup for the OpenSBI/bare-kernel handoff convention.
    pub fn set_boot(&mut self, pc: u64, a0_hartid: u64, a1_dtb: u64, a2: u64) {
        self.cpu.pc = pc;
        self.cpu.regs[10] = a0_hartid;
        self.cpu.regs[11] = a1_dtb;
        self.cpu.regs[12] = a2;
    }

    // ---- time ----

    pub fn mtime(&self) -> u64 {
        self.bus.clint.mtime
    }

    /// Host drives mtime (e.g. from wall clock scaled to TIMEBASE_FREQ).
    pub fn set_mtime(&mut self, t: u64) {
        self.bus.clint.mtime = t;
    }

    /// Initialize the guest real-time clock from Unix epoch nanoseconds.
    pub fn set_unix_time_ns(&mut self, time_ns: u64) {
        self.bus.rtc.set_time_ns(time_ns);
    }

    /// Advance `mtime` from a monotonic host-clock sample.
    ///
    /// `host_ticks` is cumulative elapsed host ticks at `TIMEBASE_FREQ`,
    /// measured from the caller's own start rather than a wall-clock epoch;
    /// the first sample only seeds the baseline, since applying it in full
    /// would jump the guest clock.
    ///
    /// Only the delta is applied, so a guest write to the CLINT mtime register
    /// becomes the new base rather than being overwritten by the next sample.
    /// Large host scheduling gaps are clamped to `MAX_HOST_MTIME_ADVANCE`.
    pub fn advance_mtime_from_host(&mut self, host_ticks: u64) {
        let elapsed = host_ticks.saturating_sub(self.last_host_mtime);
        self.last_host_mtime = host_ticks;
        self.bus.clint.mtime = self
            .bus
            .clint
            .mtime
            .wrapping_add(elapsed.min(MAX_HOST_MTIME_ADVANCE));
        // Wall-clock time must catch up after a suspension even though the
        // timer clock clamps that same gap.
        self.bus.rtc.advance_mtime_ticks(elapsed);
    }

    /// Advance `mtime` deterministically from retired instructions.
    ///
    /// System runners drive time from the host clock instead; architectural
    /// test harnesses enable this so timer behavior is reproducible.
    pub fn enable_instruction_time(&mut self) {
        self.instruction_time = true;
        self.mtime_phase = 0;
    }

    /// When is the next timer interrupt due (mtime units)?
    pub fn next_timer_deadline(&self) -> u64 {
        if self.cpu.csr.stce_enabled() {
            self.bus
                .clint
                .mtimecmp
                .min(self.cpu.csr.load_raw(csr::STIMECMP))
        } else {
            self.bus.clint.mtimecmp
        }
    }

    // ---- uart ----

    pub fn uart_input(&mut self, bytes: &[u8]) {
        for &b in bytes {
            self.bus.uart.push_rx(b);
        }
    }

    pub fn uart_output(&mut self) -> Vec<u8> {
        self.bus.uart.take_tx()
    }

    /// Send a complete message to the guest control port, without touching UART.
    pub fn control_input(&mut self, bytes: &[u8]) -> bool {
        self.bus.device_irqs_dirty = true;
        self.bus.virtio_console.input(bytes, &mut self.bus.ram)
    }

    pub fn control_output(&mut self) -> Vec<u8> {
        self.bus.device_irqs_dirty = true;
        self.bus.virtio_console.output(&mut self.bus.ram)
    }

    /// Sync device interrupt lines into mip.
    fn sync_irqs(&mut self) {
        self.bus.device_irqs_dirty = false;
        let uart_level = self.bus.uart.irq_pending();
        self.bus.plic.set_irq(crate::bus::UART_IRQ, uart_level);
        let rtc_level = self.bus.rtc.irq_pending();
        self.bus.plic.set_irq(crate::bus::RTC_IRQ, rtc_level);
        let virtio_level = self.bus.virtio_blk.irq_pending();
        self.bus.plic.set_irq(crate::bus::VIRTIO_IRQ, virtio_level);
        let rng_level = self.bus.virtio_rng.irq_pending();
        self.bus.plic.set_irq(crate::bus::VIRTIO_RNG_IRQ, rng_level);
        let net_level = self.bus.virtio_net.irq_pending();
        self.bus.plic.set_irq(crate::bus::VIRTIO_NET_IRQ, net_level);
        let kbd_level = self.bus.virtio_kbd.irq_pending();
        self.bus.plic.set_irq(crate::bus::VIRTIO_KBD_IRQ, kbd_level);
        let control_level = self.bus.virtio_console.irq_pending();
        self.bus
            .plic
            .set_irq(crate::bus::VIRTIO_CONSOLE_IRQ, control_level);
        let tablet_level = self.bus.virtio_tablet.irq_pending();
        self.bus
            .plic
            .set_irq(crate::bus::VIRTIO_TABLET_IRQ, tablet_level);

        self.sync_cpu_irqs();
    }

    fn sync_cpu_irqs(&mut self) {
        let mut pending = 0u64;
        let device_mask = irq::SSIP | irq::MSIP | irq::STIP | irq::MTIP | irq::SEIP | irq::MEIP;
        if self.bus.clint.mtip() {
            pending |= irq::MTIP
        }
        if self.bus.clint.msip {
            pending |= irq::MSIP
        }
        if self.cpu.csr.stce_enabled()
            && self.bus.clint.mtime >= self.cpu.csr.load_raw(csr::STIMECMP)
        {
            pending |= irq::STIP
        }
        if self.bus.plic.eip(0) {
            pending |= irq::MEIP
        }
        if self.bus.plic.eip(1) {
            pending |= irq::SEIP
        }
        pending |= self.bus.simple_irq.pending();
        self.cpu.csr.set_device_mip(device_mask, pending);
    }

    /// Run up to `budget` instructions; returns early on WFI/shutdown/reset.
    pub fn run(&mut self, budget: u64) -> RunEvent {
        // Host input can change devices between calls. Within a slice only
        // MMIO and network polling change their lines; RAM/ALU work cannot.
        self.bus.device_irqs_dirty = true;
        // Hoisted so an unnetworked run loop pays nothing for the network.
        let networked = self.net.is_some();
        if networked {
            self.tick_net();
        }
        let mut until_net_tick = NET_TICK_INTERVAL;
        for _ in 0..budget {
            if networked {
                until_net_tick -= 1;
                if until_net_tick == 0 {
                    until_net_tick = NET_TICK_INTERVAL;
                    self.tick_net();
                }
            }
            if self.bus.device_irqs_dirty {
                self.sync_irqs();
            } else {
                self.sync_cpu_irqs();
            }
            if let Some(i) = self.cpu.pending_interrupt() {
                self.cpu.take_interrupt(i);
            }
            match self.cpu.step(&mut self.bus) {
                StepResult::Executed => {
                    if self.instruction_time {
                        self.mtime_phase += 1;
                        if self.mtime_phase == 10 {
                            self.bus.clint.mtime = self.bus.clint.mtime.wrapping_add(1);
                            self.mtime_phase = 0;
                        }
                    }
                }
                StepResult::WaitingForInterrupt => {
                    // About to idle: one last chance for the backend to
                    // deliver something that wakes the guest.
                    if networked {
                        self.tick_net();
                    }
                    return RunEvent::Wfi;
                }
            }
            if let Some(code) = self.bus.shutdown {
                return RunEvent::Shutdown(code);
            }
            if self.bus.reset_requested {
                return RunEvent::Reset;
            }
        }
        RunEvent::BudgetExhausted
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cpu::Mode;
    use crate::devices::framebuffer::{self, FB_HEIGHT, FB_ROW_RGBA_BYTES};

    fn uart_irq_machine(code: &[u32]) -> Machine {
        use crate::bus::{DRAM_BASE, PLIC_BASE, UART_BASE, UART_IRQ};
        let mut machine = Machine::new(4096);
        let bytes: Vec<_> = code.iter().flat_map(|word| word.to_le_bytes()).collect();
        machine.load_blob(DRAM_BASE, &bytes).unwrap();
        machine.set_boot(DRAM_BASE, 0, UART_BASE, 0);
        machine
            .write_phys(PLIC_BASE + UART_IRQ as u64 * 4, 1, 4)
            .unwrap();
        machine
            .write_phys(PLIC_BASE + 0x2000, 1 << UART_IRQ, 4)
            .unwrap();
        machine
    }

    #[test]
    fn mmio_enabling_an_irq_interrupts_before_the_next_instruction() {
        use crate::bus::DRAM_BASE;
        // nop; sb t0,1(a1); addi t1,zero,1
        let mut machine = uart_irq_machine(&[0x13, 0x005580a3, 0x00100313]);
        machine.cpu.regs[5] = 1;
        machine.uart_input(b"a");
        machine
            .load_blob(DRAM_BASE + 0x100, &0x00100393u32.to_le_bytes())
            .unwrap();
        machine.cpu.csr.store_raw(csr::MTVEC, DRAM_BASE + 0x100);
        machine.cpu.csr.store_raw(csr::MIE, irq::MEIP);
        machine.cpu.csr.store_raw(csr::MSTATUS, csr::mstatus::MIE);

        machine.run(3);

        assert_eq!(machine.cpu.csr.load_raw(csr::MEPC), DRAM_BASE + 8);
        assert_eq!(machine.cpu.regs[6], 0);
        assert_eq!(machine.cpu.regs[7], 1);
    }

    #[test]
    fn mmio_read_acknowledgement_and_host_input_refresh_irq_levels() {
        use crate::bus::UART_BASE;
        // lbu t0,0(a1); csrr t1,mip; nop
        let mut machine = uart_irq_machine(&[0x0005c283, 0x34402373, 0x13]);
        machine.write_phys(UART_BASE + 1, 1, 1).unwrap();
        machine.uart_input(b"a");

        machine.run(2);

        assert_eq!(machine.cpu.regs[5], b'a' as u64);
        assert_eq!(machine.cpu.regs[6] & irq::MEIP, 0);
        // Public device access between slices also has to be observed.
        machine.bus.uart.push_rx(b'b');
        machine.run(1);
        assert_ne!(machine.cpu.csr.load_raw(csr::MIP) & irq::MEIP, 0);
    }

    #[test]
    fn instruction_timer_still_interrupts_at_the_exact_boundary_without_mmio() {
        use crate::bus::DRAM_BASE;
        let mut machine = uart_irq_machine(&[0x13; 12]);
        machine
            .load_blob(DRAM_BASE + 0x100, &0x00100393u32.to_le_bytes())
            .unwrap();
        machine.cpu.csr.store_raw(csr::MTVEC, DRAM_BASE + 0x100);
        machine.cpu.csr.store_raw(csr::MIE, irq::MTIP);
        machine.cpu.csr.store_raw(csr::MSTATUS, csr::mstatus::MIE);
        machine.bus.clint.mtimecmp = 1;
        machine.enable_instruction_time();

        machine.run(11);

        assert_eq!(machine.cpu.csr.load_raw(csr::MEPC), DRAM_BASE + 40);
        assert_eq!(machine.cpu.regs[7], 1);
    }

    #[test]
    fn stimecmp_drives_supervisor_timer_interrupt_and_deadline() {
        let mut machine = Machine::new(8);
        machine.cpu.csr.store_raw(csr::MENVCFG, csr::menvcfg::STCE);
        machine.cpu.csr.store_raw(csr::STIMECMP, 42);
        machine.set_mtime(41);
        machine.sync_irqs();
        assert_eq!(machine.cpu.csr.load_raw(csr::MIP) & irq::STIP, 0);

        machine.set_mtime(42);
        machine.sync_irqs();
        assert_eq!(machine.next_timer_deadline(), 42);
        assert_ne!(machine.cpu.csr.load_raw(csr::MIP) & irq::STIP, 0);
    }

    #[test]
    fn system_profile_exposes_pmp_csrs_required_by_xv6_startup() {
        let mut machine = Machine::new_system(8);
        machine
            .cpu
            .csr
            .write(csr::PMPADDR0, u64::MAX, Mode::Machine)
            .unwrap();
        machine
            .cpu
            .csr
            .write(csr::PMPCFG0, 0x0f, Mode::Machine)
            .unwrap();

        assert_eq!(
            machine.cpu.csr.read(csr::PMPADDR0, Mode::Machine, 0),
            Ok(u64::MAX)
        );
        assert_eq!(
            machine.cpu.csr.read(csr::PMPCFG0, Mode::Machine, 0),
            Ok(0x0f)
        );
    }

    #[test]
    fn stce_gates_supervisor_timer_pending_and_deadline() {
        let mut machine = Machine::new(8);
        machine.bus.clint.mtimecmp = 100;
        machine.cpu.csr.store_raw(csr::STIMECMP, 42);
        machine.set_mtime(50);

        machine.sync_irqs();
        assert_eq!(machine.cpu.csr.load_raw(csr::MIP) & irq::STIP, 0);
        assert_eq!(machine.next_timer_deadline(), 100);

        machine.cpu.csr.store_raw(csr::MENVCFG, csr::menvcfg::STCE);
        machine.sync_irqs();
        assert_ne!(machine.cpu.csr.load_raw(csr::MIP) & irq::STIP, 0);
        assert_eq!(machine.next_timer_deadline(), 42);

        machine.cpu.csr.store_raw(csr::MENVCFG, 0);
        machine.sync_irqs();
        assert_eq!(machine.cpu.csr.load_raw(csr::MIP) & irq::STIP, 0);
    }

    #[test]
    fn simple_interrupt_levels_are_combined_with_software_pending_bits() {
        let mut machine = Machine::new(8);
        machine
            .cpu
            .csr
            .write(csr::MIP, irq::SEIP, crate::cpu::Mode::Machine)
            .unwrap();
        machine
            .bus
            .simple_irq
            .write(4, (1u64 << 31) | irq::SSIP, 4)
            .unwrap();

        machine.sync_irqs();

        assert_eq!(
            machine.cpu.csr.load_raw(csr::MIP) & (irq::SSIP | irq::SEIP),
            irq::SSIP | irq::SEIP
        );
    }

    #[test]
    fn virtio_entropy_completion_drives_external_interrupt() {
        const DESC: u64 = crate::bus::DRAM_BASE + 0x1000;
        const AVAIL: u64 = crate::bus::DRAM_BASE + 0x2000;
        const USED: u64 = crate::bus::DRAM_BASE + 0x3000;
        const DATA: u64 = crate::bus::DRAM_BASE + 0x4000;
        let mut machine = Machine::new(0x5000);
        let device = &mut machine.bus.virtio_rng;
        let ram = &mut machine.bus.ram;
        device.write(0x038, 8, 4, ram).unwrap();
        for (low, high, address) in [
            (0x080, 0x084, DESC),
            (0x090, 0x094, AVAIL),
            (0x0a0, 0x0a4, USED),
        ] {
            device.write(low, address as u32 as u64, 4, ram).unwrap();
            device.write(high, address >> 32, 4, ram).unwrap();
        }
        device.write(0x044, 1, 4, ram).unwrap();
        for (offset, value, size) in [
            (DESC - crate::bus::DRAM_BASE, DATA, 8),
            (DESC + 8 - crate::bus::DRAM_BASE, 4, 4),
            (DESC + 12 - crate::bus::DRAM_BASE, 2, 2),
            (AVAIL + 2 - crate::bus::DRAM_BASE, 1, 2),
            (AVAIL + 4 - crate::bus::DRAM_BASE, 0, 2),
        ] {
            ram.write(offset, value, size).unwrap();
        }
        machine
            .bus
            .plic
            .write(u64::from(crate::bus::VIRTIO_RNG_IRQ) * 4, 1, 4)
            .unwrap();
        machine
            .bus
            .plic
            .write(0x2000, 1 << crate::bus::VIRTIO_RNG_IRQ, 4)
            .unwrap();

        machine.add_entropy(&[1, 2, 3, 4]);
        machine.sync_irqs();

        assert_ne!(machine.cpu.csr.load_raw(csr::MIP) & irq::MEIP, 0);
    }

    #[test]
    fn one_pixel_write_dirties_exactly_its_row() {
        let mut machine = Machine::new_system((framebuffer::FB_MACHINE_RAM_BYTES) as usize);
        assert!(machine.has_framebuffer());
        let mut rows = Vec::new();
        // The first scan is the initial full repaint.
        machine.framebuffer_dirty_rows(&mut rows);
        assert_eq!(rows.len(), FB_HEIGHT as usize);
        rows.clear();
        machine.framebuffer_dirty_rows(&mut rows);
        assert!(rows.is_empty());

        // Blue pixel at (0, 5), x8r8g8b8 = B,G,R,X in memory.
        let at = framebuffer::FB_BASE + 5 * u64::from(FB_STRIDE);
        machine.write_phys(at, 0x00_00_00_ff, 4).unwrap();

        machine.framebuffer_dirty_rows(&mut rows);
        assert_eq!(rows, vec![5]);

        rows.clear();
        machine.framebuffer_dirty_rows(&mut rows);
        assert!(rows.is_empty(), "a row is reported once per change");
    }

    #[test]
    fn clearing_the_whole_framebuffer_dirties_every_row() {
        let mut machine = Machine::new_system(framebuffer::FB_MACHINE_RAM_BYTES as usize);
        let mut rows = Vec::new();
        machine.framebuffer_dirty_rows(&mut rows);
        rows.clear();

        machine
            .load_blob(
                framebuffer::FB_BASE,
                &vec![0xff; framebuffer::FB_BYTES as usize],
            )
            .unwrap();

        machine.framebuffer_dirty_rows(&mut rows);
        assert_eq!(rows.len(), FB_HEIGHT as usize);

        // And it converts to opaque white in RGBA.
        let mut rgba = vec![0u8; FB_ROW_RGBA_BYTES];
        assert_eq!(
            machine.framebuffer_copy_rows_rgba(&[0], &mut rgba),
            FB_ROW_RGBA_BYTES
        );
        assert_eq!(&rgba[..4], &[0xff, 0xff, 0xff, 0xff]);
    }

    #[test]
    fn a_machine_too_small_for_a_framebuffer_reports_no_rows() {
        let mut machine = Machine::new(0x1000);
        assert!(!machine.has_framebuffer());
        let mut rows = Vec::new();
        machine.framebuffer_dirty_rows(&mut rows);
        assert!(rows.is_empty());
        assert!(machine.framebuffer_row(0).is_empty());
    }

    #[test]
    fn injected_input_lands_on_the_right_device_with_a_syn_report() {
        let mut machine = Machine::new_system(0x10000);

        machine.input_key(30, true);
        assert_eq!(machine.bus.virtio_kbd.pending_events(), 2);
        assert_eq!(machine.bus.virtio_tablet.pending_events(), 0);

        machine.input_pointer_abs(100, 200);
        machine.input_button(crate::devices::virtio_input::BTN_LEFT, true);
        machine.input_wheel(-1);
        // x, y, syn | button, syn | wheel, syn
        assert_eq!(machine.bus.virtio_tablet.pending_events(), 7);

        // A wheel event of zero detents is not an event at all.
        machine.input_wheel(0);
        assert_eq!(machine.bus.virtio_tablet.pending_events(), 7);
    }

    #[test]
    fn run_advances_mtime_once_per_ten_executed_instructions() {
        let mut machine = Machine::new(64);
        machine.enable_instruction_time();
        machine.cpu.pc = crate::bus::DRAM_BASE;
        machine
            .bus
            .ram
            .load_blob(0, &[0x13, 0, 0, 0].repeat(10))
            .unwrap();
        machine.set_mtime(u64::MAX);

        assert_eq!(machine.run(10), RunEvent::BudgetExhausted);

        assert_eq!(machine.mtime(), 0);
    }

    #[test]
    fn host_driven_time_does_not_advance_with_instructions() {
        let mut machine = Machine::new(64);
        machine.cpu.pc = crate::bus::DRAM_BASE;
        machine
            .bus
            .ram
            .load_blob(0, &[0x13, 0, 0, 0].repeat(10))
            .unwrap();
        machine.set_mtime(42);

        assert_eq!(machine.run(10), RunEvent::BudgetExhausted);

        assert_eq!(machine.mtime(), 42);
    }

    #[test]
    fn host_time_preserves_guest_mtime_writes_and_clamps_suspend_gaps() {
        let mut machine = Machine::new(8);
        machine.advance_mtime_from_host(25);
        machine.bus.clint.write(0xBFF8, 1_000, 8).unwrap();

        machine.advance_mtime_from_host(35);
        assert_eq!(machine.mtime(), 1_010);

        machine.advance_mtime_from_host(35 + MAX_HOST_MTIME_ADVANCE + 500);
        assert_eq!(machine.mtime(), 1_010 + MAX_HOST_MTIME_ADVANCE);
    }

    #[test]
    fn backward_host_sample_resets_baseline_without_rewinding_mtime() {
        let mut machine = Machine::new(8);
        machine.advance_mtime_from_host(100);
        machine.advance_mtime_from_host(50);
        machine.advance_mtime_from_host(51);

        assert_eq!(machine.mtime(), 101);
    }

    #[test]
    fn host_time_advances_rtc_from_initialized_unix_epoch() {
        let mut machine = Machine::new(8);
        machine.set_unix_time_ns(1_700_000_000_000_000_000);
        machine.advance_mtime_from_host(25);

        assert_eq!(
            machine.bus.rtc.read(0, 4),
            Ok(1_700_000_000_000_002_500_u64 as u32 as u64)
        );
    }

    #[test]
    fn rtc_catches_up_across_a_clamped_host_suspension_gap() {
        let mut machine = Machine::new(8);
        let epoch = 1_700_000_000_000_000_000_u64;
        machine.set_unix_time_ns(epoch);
        machine.advance_mtime_from_host(10);
        machine.advance_mtime_from_host(10 + MAX_HOST_MTIME_ADVANCE + 500);
        let low = machine.bus.rtc.read(0, 4).unwrap();
        let high = machine.bus.rtc.read(4, 4).unwrap();

        assert_eq!(
            (high << 32) | low,
            epoch.wrapping_add((MAX_HOST_MTIME_ADVANCE + 510).wrapping_mul(100))
        );
    }
}
