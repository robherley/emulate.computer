//! Physical memory map and access dispatch (QEMU `virt`-compatible layout).

use crate::devices::clint::Clint;
use crate::devices::framebuffer_blitter::FramebufferBlitter;
use crate::devices::goldfish_rtc::GoldfishRtc;
use crate::devices::plic::Plic;
use crate::devices::simple_irq::SimpleInterruptGenerator;
use crate::devices::uart::Uart;
use crate::devices::virtio_blk::VirtioBlock;
use crate::devices::virtio_console::VirtioConsole;
use crate::devices::virtio_input::{InputKind, VirtioInput};
use crate::devices::virtio_net::VirtioNet;
use crate::devices::virtio_rng::VirtioEntropy;
use crate::ram::Ram;

pub(crate) const TEST_BASE: u64 = 0x10_0000;
pub(crate) const TEST_SIZE: u64 = 0x1000;
pub(crate) const CLINT_BASE: u64 = 0x200_0000;
pub(crate) const CLINT_SIZE: u64 = 0x10000;
pub(crate) const PLIC_BASE: u64 = 0xC00_0000;
pub(crate) const PLIC_SIZE: u64 = 0x60_0000;
pub(crate) const UART_BASE: u64 = 0x1000_0000;
pub(crate) const UART_SIZE: u64 = 0x100;
pub(crate) const RTC_BASE: u64 = 0x10_1000;
pub(crate) const RTC_SIZE: u64 = 0x1000;
pub(crate) const VIRTIO_BASE: u64 = 0x1000_1000;
pub(crate) const VIRTIO_SIZE: u64 = 0x1000;
pub(crate) const VIRTIO_RNG_BASE: u64 = 0x1000_2000;
pub(crate) const VIRTIO_NET_BASE: u64 = 0x1000_3000;
pub(crate) const VIRTIO_KBD_BASE: u64 = 0x1000_4000;
pub(crate) const VIRTIO_TABLET_BASE: u64 = 0x1000_5000;
pub(crate) const VIRTIO_CONSOLE_BASE: u64 = 0x1000_7000;
pub(crate) const VIRTIO_CONSOLE_IRQ: u32 = 6;
pub(crate) const FB_BLITTER_BASE: u64 = 0x1000_6000;
pub(crate) const FB_BLITTER_SIZE: u64 = 0x2c;
pub(crate) const SIMPLE_IRQ_BASE: u64 = 0x1001_0000;
pub(crate) const SIMPLE_IRQ_SIZE: u64 = 0x20;
pub const DRAM_BASE: u64 = 0x8000_0000;

/// UART interrupt line on the PLIC (matches QEMU virt).
pub(crate) const UART_IRQ: u32 = 10;
/// VirtIO block interrupt line on the PLIC (matches QEMU virt and xv6).
pub(crate) const VIRTIO_IRQ: u32 = 1;
/// VirtIO entropy interrupt line on the PLIC (second QEMU-virt MMIO slot).
pub(crate) const VIRTIO_RNG_IRQ: u32 = 2;
/// VirtIO network interrupt line on the PLIC (third QEMU-virt MMIO slot).
pub(crate) const VIRTIO_NET_IRQ: u32 = 3;
/// VirtIO input keyboard interrupt line (fourth QEMU-virt MMIO slot).
pub(crate) const VIRTIO_KBD_IRQ: u32 = 4;
/// VirtIO input tablet interrupt line (fifth QEMU-virt MMIO slot).
pub(crate) const VIRTIO_TABLET_IRQ: u32 = 5;
/// Goldfish RTC interrupt line on the PLIC (matches QEMU virt).
pub(crate) const RTC_IRQ: u32 = 11;

pub struct Bus {
    pub ram: Ram,
    pub clint: Clint,
    pub plic: Plic,
    pub uart: Uart,
    pub rtc: GoldfishRtc,
    pub virtio_blk: VirtioBlock,
    pub virtio_console: VirtioConsole,
    pub virtio_rng: VirtioEntropy,
    pub virtio_net: VirtioNet,
    /// Keyboard and absolute-pointer input; see `docs/display.md`.
    pub virtio_kbd: VirtioInput,
    pub virtio_tablet: VirtioInput,
    pub simple_irq: SimpleInterruptGenerator,
    pub framebuffer_blitter: FramebufferBlitter,
    /// Set when the guest writes the test-finisher device: Some(exit_code).
    pub shutdown: Option<u32>,
    /// Set on a reset request.
    pub reset_requested: bool,
    pub(crate) device_irqs_dirty: bool,
}

impl Bus {
    pub fn new(ram_size: usize) -> Self {
        Bus {
            ram: Ram::new(ram_size),
            clint: Clint::new(),
            plic: Plic::new(),
            uart: Uart::new(),
            rtc: GoldfishRtc::new(),
            virtio_blk: VirtioBlock::new(),
            virtio_console: VirtioConsole::default(),
            virtio_rng: VirtioEntropy::new(),
            virtio_net: VirtioNet::new(),
            virtio_kbd: VirtioInput::new(InputKind::Keyboard),
            virtio_tablet: VirtioInput::new(InputKind::Tablet),
            simple_irq: SimpleInterruptGenerator::new(),
            framebuffer_blitter: FramebufferBlitter::default(),
            shutdown: None,
            reset_requested: false,
            device_irqs_dirty: true,
        }
    }

    /// Whether the complete physical range is backed by ordinary RAM.
    ///
    /// Page-table walks use this PMA check before touching a PTE so a crafted
    /// table pointer cannot turn address translation into an MMIO access.
    pub fn is_main_memory(&self, addr: u64, size: u8) -> bool {
        let Some(offset) = addr.checked_sub(DRAM_BASE) else {
            return false;
        };
        offset
            .checked_add(size as u64)
            .is_some_and(|end| end <= self.ram.size())
    }

    /// Whether a physical range belongs to a writable platform region.
    ///
    /// SC uses this non-mutating PMA check even when its reservation fails;
    /// the instruction may fail spuriously, but it must still raise access
    /// faults for addresses that cannot be written.
    pub fn write_accessible(&self, addr: u64, size: u8) -> bool {
        let Some(end) = addr.checked_add(size as u64) else {
            return false;
        };
        if addr >= DRAM_BASE {
            return end <= DRAM_BASE + self.ram.size();
        }
        [
            (CLINT_BASE, CLINT_SIZE),
            (PLIC_BASE, PLIC_SIZE),
            (UART_BASE, UART_SIZE),
            (RTC_BASE, RTC_SIZE),
            (VIRTIO_BASE, VIRTIO_SIZE),
            (VIRTIO_CONSOLE_BASE, VIRTIO_SIZE),
            (VIRTIO_RNG_BASE, VIRTIO_SIZE),
            (VIRTIO_NET_BASE, VIRTIO_SIZE),
            (VIRTIO_KBD_BASE, VIRTIO_SIZE),
            (VIRTIO_TABLET_BASE, VIRTIO_SIZE),
            (SIMPLE_IRQ_BASE, SIMPLE_IRQ_SIZE),
            (FB_BLITTER_BASE, FB_BLITTER_SIZE),
            (TEST_BASE, TEST_SIZE),
        ]
        .into_iter()
        .any(|(base, len)| addr >= base && end <= base + len)
    }

    /// Little-endian read of 1/2/4/8 bytes at physical address `addr`.
    /// `Err(())` = access fault (caller maps to the right exception kind).
    #[inline]
    pub fn read(&mut self, addr: u64, size: u8) -> Result<u64, ()> {
        if addr >= DRAM_BASE {
            return self.ram.read(addr - DRAM_BASE, size);
        }
        self.read_mmio(addr, size)
    }

    #[cold]
    fn read_mmio(&mut self, addr: u64, size: u8) -> Result<u64, ()> {
        // Reads can acknowledge interrupts (UART IIR, PLIC claim).
        self.device_irqs_dirty = true;
        match addr {
            _ if (VIRTIO_CONSOLE_BASE..VIRTIO_CONSOLE_BASE + VIRTIO_SIZE).contains(&addr) => {
                self.virtio_console.read(addr - VIRTIO_CONSOLE_BASE, size)
            }
            _ if (CLINT_BASE..CLINT_BASE + CLINT_SIZE).contains(&addr) => {
                self.clint.read(addr - CLINT_BASE, size)
            }
            _ if (PLIC_BASE..PLIC_BASE + PLIC_SIZE).contains(&addr) => {
                self.plic.read(addr - PLIC_BASE, size)
            }
            _ if (UART_BASE..UART_BASE + UART_SIZE).contains(&addr) => {
                self.uart.read(addr - UART_BASE, size)
            }
            _ if (RTC_BASE..RTC_BASE + RTC_SIZE).contains(&addr) => {
                self.rtc.read(addr - RTC_BASE, size)
            }
            _ if (VIRTIO_BASE..VIRTIO_BASE + VIRTIO_SIZE).contains(&addr) => {
                self.virtio_blk.read(addr - VIRTIO_BASE, size)
            }
            _ if (VIRTIO_RNG_BASE..VIRTIO_RNG_BASE + VIRTIO_SIZE).contains(&addr) => {
                self.virtio_rng.read(addr - VIRTIO_RNG_BASE, size)
            }
            _ if (VIRTIO_NET_BASE..VIRTIO_NET_BASE + VIRTIO_SIZE).contains(&addr) => {
                self.virtio_net.read(addr - VIRTIO_NET_BASE, size)
            }
            _ if (VIRTIO_KBD_BASE..VIRTIO_KBD_BASE + VIRTIO_SIZE).contains(&addr) => {
                self.virtio_kbd.read(addr - VIRTIO_KBD_BASE, size)
            }
            _ if (VIRTIO_TABLET_BASE..VIRTIO_TABLET_BASE + VIRTIO_SIZE).contains(&addr) => {
                self.virtio_tablet.read(addr - VIRTIO_TABLET_BASE, size)
            }
            _ if (SIMPLE_IRQ_BASE..SIMPLE_IRQ_BASE + SIMPLE_IRQ_SIZE).contains(&addr) => {
                self.simple_irq.read(addr - SIMPLE_IRQ_BASE, size)
            }
            _ if (FB_BLITTER_BASE..FB_BLITTER_BASE + FB_BLITTER_SIZE).contains(&addr) => {
                self.framebuffer_blitter.read(addr - FB_BLITTER_BASE, size)
            }
            _ if (TEST_BASE..TEST_BASE + TEST_SIZE).contains(&addr) => Ok(0),
            _ => Err(()),
        }
    }

    #[inline]
    pub fn write(&mut self, addr: u64, val: u64, size: u8) -> Result<(), ()> {
        if addr >= DRAM_BASE {
            return self.ram.write(addr - DRAM_BASE, val, size);
        }
        self.write_mmio(addr, val, size)
    }

    #[cold]
    fn write_mmio(&mut self, addr: u64, val: u64, size: u8) -> Result<(), ()> {
        self.device_irqs_dirty = true;
        match addr {
            _ if (VIRTIO_CONSOLE_BASE..VIRTIO_CONSOLE_BASE + VIRTIO_SIZE).contains(&addr) => self
                .virtio_console
                .write(addr - VIRTIO_CONSOLE_BASE, val, size, &mut self.ram),
            _ if (CLINT_BASE..CLINT_BASE + CLINT_SIZE).contains(&addr) => {
                self.clint.write(addr - CLINT_BASE, val, size)
            }
            _ if (PLIC_BASE..PLIC_BASE + PLIC_SIZE).contains(&addr) => {
                self.plic.write(addr - PLIC_BASE, val, size)
            }
            _ if (UART_BASE..UART_BASE + UART_SIZE).contains(&addr) => {
                self.uart.write(addr - UART_BASE, val, size)
            }
            _ if (RTC_BASE..RTC_BASE + RTC_SIZE).contains(&addr) => {
                self.rtc.write(addr - RTC_BASE, val, size)
            }
            _ if (VIRTIO_BASE..VIRTIO_BASE + VIRTIO_SIZE).contains(&addr) => {
                self.virtio_blk
                    .write(addr - VIRTIO_BASE, val, size, &mut self.ram)
            }
            _ if (VIRTIO_RNG_BASE..VIRTIO_RNG_BASE + VIRTIO_SIZE).contains(&addr) => self
                .virtio_rng
                .write(addr - VIRTIO_RNG_BASE, val, size, &mut self.ram),
            _ if (VIRTIO_NET_BASE..VIRTIO_NET_BASE + VIRTIO_SIZE).contains(&addr) => self
                .virtio_net
                .write(addr - VIRTIO_NET_BASE, val, size, &mut self.ram),
            _ if (VIRTIO_KBD_BASE..VIRTIO_KBD_BASE + VIRTIO_SIZE).contains(&addr) => self
                .virtio_kbd
                .write(addr - VIRTIO_KBD_BASE, val, size, &mut self.ram),
            _ if (VIRTIO_TABLET_BASE..VIRTIO_TABLET_BASE + VIRTIO_SIZE).contains(&addr) => self
                .virtio_tablet
                .write(addr - VIRTIO_TABLET_BASE, val, size, &mut self.ram),
            _ if (SIMPLE_IRQ_BASE..SIMPLE_IRQ_BASE + SIMPLE_IRQ_SIZE).contains(&addr) => {
                self.simple_irq.write(addr - SIMPLE_IRQ_BASE, val, size)
            }
            _ if (FB_BLITTER_BASE..FB_BLITTER_BASE + FB_BLITTER_SIZE).contains(&addr) => self
                .framebuffer_blitter
                .write(addr - FB_BLITTER_BASE, val, size, &mut self.ram),
            _ if (TEST_BASE..TEST_BASE + TEST_SIZE).contains(&addr) => {
                // SiFive test finisher: 0x5555 pass, 0x3333|code<<16 fail, 0x7777 reset.
                match val as u32 & 0xFFFF {
                    0x5555 => self.shutdown = Some(0),
                    0x3333 => self.shutdown = Some((val as u32) >> 16),
                    0x7777 => self.reset_requested = true,
                    _ => {}
                }
                Ok(())
            }
            _ => Err(()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn second_virtio_slot_dispatches_to_entropy_device() {
        let mut bus = Bus::new(8);

        assert_eq!(bus.read(VIRTIO_RNG_BASE + 0x008, 4), Ok(4));

        bus.write(VIRTIO_RNG_BASE + 0x070, 15, 4).unwrap();
        assert_eq!(bus.read(VIRTIO_RNG_BASE + 0x070, 4), Ok(15));
        bus.write(VIRTIO_RNG_BASE + 0x070, 0, 4).unwrap();
        assert_eq!(bus.read(VIRTIO_RNG_BASE + 0x070, 4), Ok(0));
    }

    #[test]
    fn third_virtio_slot_dispatches_to_network_device() {
        let mut bus = Bus::new(8);

        assert_eq!(bus.read(VIRTIO_NET_BASE + 0x008, 4), Ok(1));
        // MAC lives in configuration space and is read byte-wise by Linux.
        assert_eq!(bus.read(VIRTIO_NET_BASE + 0x100, 1), Ok(0x52));
        assert_eq!(bus.read(VIRTIO_NET_BASE + 0x105, 1), Ok(0x56));

        bus.write(VIRTIO_NET_BASE + 0x070, 15, 4).unwrap();
        assert_eq!(bus.read(VIRTIO_NET_BASE + 0x070, 4), Ok(15));
        assert!(bus.write_accessible(VIRTIO_NET_BASE, 4));
    }

    #[test]
    fn fourth_and_fifth_virtio_slots_dispatch_to_the_input_devices() {
        let mut bus = Bus::new(8);

        // Device ID 18 = virtio input, at both slots.
        assert_eq!(bus.read(VIRTIO_KBD_BASE + 0x008, 4), Ok(18));
        assert_eq!(bus.read(VIRTIO_TABLET_BASE + 0x008, 4), Ok(18));
        assert!(bus.write_accessible(VIRTIO_KBD_BASE, 4));
        assert!(bus.write_accessible(VIRTIO_TABLET_BASE, 4));

        // Configuration space is byte-addressed and tells the two apart.
        bus.write(VIRTIO_KBD_BASE + 0x100, 0x01, 1).unwrap();
        assert_eq!(bus.read(VIRTIO_KBD_BASE + 0x108, 1), Ok(u64::from(b'e')));
        bus.write(VIRTIO_TABLET_BASE + 0x100, 0x01, 1).unwrap();
        assert_eq!(bus.read(VIRTIO_TABLET_BASE + 0x102, 1), Ok(14));
    }
}
