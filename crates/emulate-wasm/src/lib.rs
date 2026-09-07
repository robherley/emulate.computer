//! wasm-bindgen wrapper around `emulate_core::Machine`.
//!
//! Guest physical addresses, mtime values and Unix milliseconds cross the JS
//! boundary as `f64`: all fit in 2^53, so the conversion is exact. Values that
//! would not fit get a sentinel instead (see `next_timer_deadline`).

use std::rc::Rc;

/// Per-tab OPFS disk and its streaming seeder.
mod disk;

/// The browser network transport. wasm-only: built from `web_sys` primitives
/// that do not exist natively. The relay protocol it carries lives in
/// `emulate_core::hostnet`.
#[cfg(target_arch = "wasm32")]
pub mod net;

use disk::{DiskSeeder, OpfsBackend, SessionDiskStatus};
use emulate_core::machine::{Machine, RunEvent};
use emulate_core::snapshot::RestoreCheck;
use wasm_bindgen::prelude::*;
use web_sys::FileSystemSyncAccessHandle;

/// Status codes returned by [`WasmMachine::run`].
pub const STATUS_BUDGET_EXHAUSTED: u32 = 0;
pub const STATUS_WFI: u32 = 1;
pub const STATUS_SHUTDOWN: u32 = 2;
pub const STATUS_RESET: u32 = 3;

#[wasm_bindgen]
pub struct WasmMachine {
    machine: Machine,
    last_exit_code: u32,
    session_disk_status: Option<Rc<SessionDiskStatus>>,
    /// The seed in flight, between `seed_disk_begin_*` and `seed_disk_finish`.
    disk_seeder: Option<DiskSeeder<FileSystemSyncAccessHandle>>,
}

#[wasm_bindgen]
impl WasmMachine {
    /// Create a machine with `ram_bytes` of DRAM at 0x8000_0000.
    #[wasm_bindgen(constructor)]
    pub fn new(ram_bytes: u32) -> WasmMachine {
        console_error_panic_hook::set_once();
        WasmMachine {
            machine: Machine::new_system(ram_bytes as usize),
            last_exit_code: 0,
            session_disk_status: None,
            disk_seeder: None,
        }
    }

    /// Attach the per-guest logical transport supplied by the browser network worker.
    pub fn set_net_transport(&mut self, url: String, transport: JsValue) {
        #[cfg(target_arch = "wasm32")]
        {
            let transport: net::BrowserTransport = transport.unchecked_into();
            let substrate = net::relay_substrate(&url, transport);
            self.machine
                .set_net_backend(Box::new(emulate_core::hostnet::NatBackend::new(substrate)));
        }
        #[cfg(not(target_arch = "wasm32"))]
        let _ = (url, transport);
    }

    /// Earliest millisecond (on the same clock as [`Self::net_now_ms`]) the
    /// network backend wants to be polled again, or `-1` when it is idle.
    ///
    /// Hosts that sleep on WFI must cap the sleep at this deadline: a browser
    /// callback (a WebSocket message arriving) cannot itself wake
    /// a sleeping worker, so without this the guest would wait out the full
    /// timer sleep before seeing its packets.
    pub fn net_deadline_ms(&self) -> f64 {
        self.machine.net_deadline_ms().map_or(-1.0, |ms| ms as f64)
    }

    /// The machine's current millisecond clock, for comparing against
    /// [`Self::net_deadline_ms`].
    pub fn net_now_ms(&self) -> f64 {
        self.machine.net_now_ms() as f64
    }

    /// Load a blob at guest physical address `paddr` (must land in RAM).
    /// `paddr` is an f64 but must be an exact integer < 2^53.
    pub fn load_blob(&mut self, paddr: f64, data: &[u8]) -> Result<(), JsError> {
        self.machine
            .load_blob(paddr as u64, data)
            .map_err(|_| JsError::new("load_blob: address range outside RAM"))
    }

    // ---- seeding a fresh disk (docs/disk.md) ----
    //
    // The host streams decompressed image bytes in here in arbitrary chunks.
    // The seeder buffers across chunk boundaries and writes only non-zero 4 KiB
    // blocks, so no full copy of the image is ever held in memory.

    /// Begin seeding the flat disk image into `handle`, which is sized to the
    /// whole disk first so untouched regions read back as zeros.
    ///
    /// `handle` is normally on a staging file the host renames into place once
    /// [`Self::seed_disk_finish`] returns; that call closes the handle.
    pub fn seed_disk_begin_opfs(
        &mut self,
        handle: FileSystemSyncAccessHandle,
        logical_bytes: f64,
    ) -> Result<(), JsError> {
        self.disk_seeder = Some(
            DiskSeeder::begin_image(handle, logical_bytes as u64)
                .map_err(|why| JsError::new(&why))?,
        );
        Ok(())
    }

    /// Begin seeding the volatile in-memory disk, which
    /// [`Self::seed_disk_finish`] attaches to the machine.
    pub fn seed_disk_begin_volatile(&mut self, logical_bytes: f64) -> Result<(), JsError> {
        self.disk_seeder =
            Some(DiskSeeder::begin_memory(logical_bytes as u64).map_err(|why| JsError::new(&why))?);
        Ok(())
    }

    /// Feed the next slice of the decompressed image.
    pub fn seed_disk_write(&mut self, chunk: &[u8]) -> Result<(), JsError> {
        let seeder = self
            .disk_seeder
            .as_mut()
            .ok_or_else(|| JsError::new("seed: no seed in progress"))?;
        let result = seeder.write(chunk);
        match result {
            Ok(()) => Ok(()),
            Err(why) => {
                // A failed seed is not resumable; drop it rather than let the
                // host feed more bytes into a half-written image.
                self.disk_seeder = None;
                Err(JsError::new(&why))
            }
        }
    }

    /// Abandon a seed in progress, closing the OPFS handle it holds so the host
    /// can delete the half-written file. A no-op when no seed is in flight.
    pub fn seed_disk_abort(&mut self) {
        if let Some(seeder) = self.disk_seeder.take() {
            seeder.abort();
        }
    }

    /// Commit the seed. Errors unless exactly the declared number of bytes
    /// arrived, so a truncated download can never be mounted as a filesystem.
    pub fn seed_disk_finish(&mut self) -> Result<(), JsError> {
        let seeder = self
            .disk_seeder
            .take()
            .ok_or_else(|| JsError::new("seed: no seed in progress"))?;
        if let Some(backend) = seeder.finish().map_err(|why| JsError::new(&why))? {
            self.session_disk_status = None;
            self.machine.set_disk_backend(Box::new(backend));
        }
        Ok(())
    }

    /// Attach a synchronous OPFS handle. This API is Worker-only in browsers.
    ///
    /// The file must be exactly `logical_bytes` long — the flat image *is* the
    /// disk — which is the whole of the open-time validation.
    pub fn set_disk_opfs(
        &mut self,
        handle: FileSystemSyncAccessHandle,
        logical_bytes: f64,
    ) -> Result<(), JsError> {
        let (backend, status) =
            OpfsBackend::open(handle, logical_bytes as u64).map_err(|why| JsError::new(&why))?;
        self.machine.set_disk_backend(Box::new(backend));
        self.session_disk_status = Some(status);
        Ok(())
    }

    pub fn flush_disk(&mut self) -> Result<(), JsError> {
        self.machine.flush_disk().map_err(|_| {
            JsError::new(
                &self
                    .session_disk_status
                    .as_deref()
                    .and_then(SessionDiskStatus::take_error)
                    .unwrap_or_else(|| "disk flush failed".to_owned()),
            )
        })
    }

    /// Flush and detach the disk, closing an OPFS handle before reset/free.
    pub fn close_disk(&mut self) -> Result<(), JsError> {
        if self.session_disk_status.is_none() {
            return Ok(());
        }
        self.flush_disk()?;
        self.machine.set_disk(&[]);
        self.session_disk_status = None;
        Ok(())
    }

    /// One-line session-disk description for the status line.
    pub fn disk_status_line(&self) -> String {
        self.session_disk_status
            .as_deref()
            .map_or_else(String::new, SessionDiskStatus::line)
    }

    /// Take the most recent session-disk failure for UI reporting.
    pub fn take_disk_error(&mut self) -> Option<String> {
        self.session_disk_status.as_deref()?.take_error()
    }

    // ---- state snapshots (docs/disk.md) ----

    /// Resume from an `EMUSNAP1` container instead of booting.
    ///
    /// Call order matters and is the host's responsibility: the disk must
    /// already be attached (the container writes its disk overlay through it),
    /// and `set_unix_time_ms` / `set_net_transport` come *after* — the captured
    /// wall clock is as stale as the file, and a snapshot never carries a
    /// network backend. `set_boot` is not called at all: the reset vector is
    /// part of the captured state.
    ///
    /// `image_hash` is the 32-byte SHA-256 the page computed over the same
    /// firmware/kernel/initramfs/DTB bytes, and `disk_id` the versioned identity
    /// of the root image. A container that does not match either is
    /// refused, so a rebuilt guest can never be resumed into stale memory.
    pub fn restore(
        &mut self,
        snapshot: &[u8],
        image_hash: &[u8],
        disk_id: String,
    ) -> Result<(), JsError> {
        let hash: [u8; 32] = image_hash
            .try_into()
            .map_err(|_| JsError::new("restore: image hash must be 32 bytes"))?;
        self.machine
            .restore(
                snapshot,
                &RestoreCheck {
                    image_hash: Some(hash),
                    disk_id: Some(&disk_id),
                },
            )
            .map_err(|why| JsError::new(&why))
    }

    /// Set the reset vector: pc plus the a0/a1/a2 handoff registers.
    pub fn set_boot(&mut self, pc: f64, a0: f64, a1: f64, a2: f64) {
        self.machine
            .set_boot(pc as u64, a0 as u64, a1 as u64, a2 as u64);
    }

    /// Run up to `budget` instructions.
    /// Returns: 0 = budget exhausted, 1 = WFI, 2 = shutdown (see
    /// `last_exit_code`), 3 = reset requested.
    pub fn run(&mut self, budget: u32) -> u32 {
        match self.machine.run(budget as u64) {
            RunEvent::BudgetExhausted => STATUS_BUDGET_EXHAUSTED,
            RunEvent::Wfi => STATUS_WFI,
            RunEvent::Shutdown(code) => {
                self.last_exit_code = code;
                STATUS_SHUTDOWN
            }
            RunEvent::Reset => STATUS_RESET,
        }
    }

    pub fn cpu_registers(&self) -> Vec<u64> {
        self.machine.cpu.regs.to_vec()
    }
    pub fn cpu_pc(&self) -> u64 {
        self.machine.cpu.pc
    }

    /// Exit code of the most recent `run()` that returned status 2 (shutdown).
    pub fn last_exit_code(&self) -> u32 {
        self.last_exit_code
    }

    /// Send a complete control message, returning false when the port is full.
    pub fn control_input(&mut self, bytes: &[u8]) -> bool {
        self.machine.control_input(bytes)
    }

    pub fn control_output(&mut self) -> Vec<u8> {
        self.machine.control_output()
    }

    /// Feed bytes into the UART receive FIFO (guest stdin).
    pub fn uart_input(&mut self, bytes: &[u8]) {
        self.machine.uart_input(bytes);
    }

    /// Drain the UART transmit buffer (guest stdout). Returns a Uint8Array.
    pub fn uart_output(&mut self) -> Vec<u8> {
        self.machine.uart_output()
    }

    /// Initialize the Goldfish RTC from Unix epoch milliseconds.
    pub fn set_unix_time_ms(&mut self, time_ms: f64) {
        if time_ms.is_finite() && time_ms >= 0.0 {
            self.machine
                .set_unix_time_ns((time_ms * 1_000_000.0) as u64);
        }
    }

    /// Number of host entropy bytes the bounded VirtIO RNG reservoir can accept.
    pub fn entropy_needed(&self) -> u32 {
        self.machine.entropy_needed().min(u32::MAX as usize) as u32
    }

    /// Add cryptographically secure bytes supplied by the browser host.
    pub fn add_entropy(&mut self, bytes: &[u8]) -> u32 {
        self.machine.add_entropy(bytes).min(u32::MAX as usize) as u32
    }

    /// Advance guest time from a monotonic host tick count without rewinding it.
    pub fn advance_mtime_from_host(&mut self, host_ticks: f64) {
        self.machine.advance_mtime_from_host(host_ticks as u64);
    }

    /// Current mtime, in TIMEBASE_FREQ (10 MHz) ticks.
    pub fn mtime(&self) -> f64 {
        self.machine.mtime() as f64
    }

    /// Next timer interrupt deadline in mtime ticks, or `-1` if no timer is
    /// armed (the core reports that as `u64::MAX` / mtimecmp never set).
    pub fn next_timer_deadline(&self) -> f64 {
        let d = self.machine.next_timer_deadline();
        if d == u64::MAX {
            -1.0
        } else {
            d as f64
        }
    }

    /// Number of instructions served by the decoded-block cache.
    pub fn decode_cache_hits(&self) -> f64 {
        self.machine.cpu.decode_cache_stats().hits as f64
    }

    /// Number of instructions that required fetch/decode cache lookup misses.
    pub fn decode_cache_misses(&self) -> f64 {
        self.machine.cpu.decode_cache_stats().misses as f64
    }

    /// Total guest RAM, in bytes.
    pub fn ram_bytes(&self) -> f64 {
        self.machine.ram_bytes() as f64
    }

    /// Guest RAM ever written, as a high-water mark (never live usage: the
    /// guest freeing memory does not lower it).
    pub fn ram_touched_bytes(&self) -> f64 {
        self.machine.ram_touched_bytes() as f64
    }

    /// Cumulative VirtIO-blk payload bytes read by the guest.
    pub fn disk_bytes_read(&self) -> f64 {
        self.machine.disk_bytes_read() as f64
    }

    /// Cumulative VirtIO-blk payload bytes written by the guest.
    pub fn disk_bytes_written(&self) -> f64 {
        self.machine.disk_bytes_written() as f64
    }

    /// Cumulative VirtIO-net frame bytes delivered to the guest.
    pub fn net_bytes_rx(&self) -> f64 {
        self.machine.net_bytes_rx() as f64
    }

    /// Cumulative VirtIO-net frame bytes handed to the host backend.
    pub fn net_bytes_tx(&self) -> f64 {
        self.machine.net_bytes_tx() as f64
    }

    // ---- display (docs/display.md) ----
    //
    // No raw view of wasm memory ever crosses to JS: the worker's view would
    // be invalidated by any memory growth, and the canvas lives on the main
    // thread behind an OffscreenCanvas transfer anyway. The host passes a
    // buffer in and gets RGBA bytes back.

    pub fn fb_width(&self) -> u32 {
        self.machine.bus.framebuffer_blitter.size().0
    }

    pub fn fb_request_size(&mut self, width: u32, height: u32) -> bool {
        self.machine
            .bus
            .framebuffer_blitter
            .request_size(width, height)
            .is_ok()
    }

    pub fn fb_height(&self) -> u32 {
        self.machine.bus.framebuffer_blitter.size().1
    }

    /// Bytes per backing scanline, including padding outside the active width.
    pub fn fb_stride(&self) -> u32 {
        emulate_core::devices::framebuffer::FB_STRIDE
    }

    /// Row indices written since the last call, ascending. Reporting a row is
    /// what marks it seen, so each change is returned exactly once.
    pub fn fb_dirty_rows(&mut self) -> Vec<u32> {
        let mut rows = Vec::new();
        self.machine.framebuffer_dirty_rows(&mut rows);
        rows
    }

    pub fn fb_committed(&self) -> bool {
        self.machine.bus.framebuffer_blitter.committed()
    }

    pub fn fb_presents(&self) -> f64 {
        self.machine.bus.framebuffer_blitter.presents as f64
    }

    /// Force the next [`Self::fb_dirty_rows`] to report every row, e.g. when
    /// the host has just created the canvas it paints into.
    pub fn fb_mark_all_dirty(&mut self) {
        self.machine.framebuffer_mark_all_dirty();
    }

    /// The listed rows converted from the guest's `x8r8g8b8` into RGBA, packed
    /// back to back in the order given: `rows.len() * fb_width() * 4` bytes,
    /// exactly what a canvas `ImageData` takes.
    ///
    /// A fresh buffer per frame rather than one the caller keeps: a `&mut
    /// [u8]` parameter would make wasm-bindgen copy the whole 3 MiB in *and*
    /// out on every call, where this copies out only the rows that changed.
    pub fn fb_copy_rows(&self, rows: &[u32]) -> Vec<u8> {
        let mut out = vec![0u8; rows.len() * self.fb_width() as usize * 4];
        self.machine.framebuffer_copy_rows_rgba(rows, &mut out);
        out
    }

    // ---- input (docs/display.md) ----

    /// Press or release an evdev key code on the virtio keyboard.
    pub fn input_key(&mut self, code: u16, pressed: bool) {
        self.machine.input_key(code, pressed);
    }

    /// Move the absolute pointer. Both axes are 0..=32767, not pixels.
    pub fn input_pointer_abs(&mut self, x: u16, y: u16) {
        self.machine.input_pointer_abs(x, y);
    }

    /// Tablet events waiting for guest VirtIO buffers.
    pub fn input_pending_events(&self) -> u32 {
        self.machine.bus.virtio_tablet.pending_events() as u32
    }

    /// Press or release a mouse button (BTN_LEFT 0x110, RIGHT 0x111,
    /// MIDDLE 0x112).
    pub fn input_button(&mut self, code: u16, pressed: bool) {
        self.machine.input_button(code, pressed);
    }

    /// Scroll by whole wheel detents; positive is away from the user.
    pub fn input_wheel(&mut self, delta: i32) {
        self.machine.input_wheel(delta);
    }
}
