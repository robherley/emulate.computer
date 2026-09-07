//! RISC-V RV64GC system emulator core.
#![expect(
    clippy::result_unit_err,
    reason = "unit errors are deliberate zero-cost hardware fault signals mapped to architectural traps"
)]
//!
//! Builds both as a native library and inside the `emulate-wasm` wrapper, so it
//! stays dependency-free and free of any wasm-bindgen or host-OS dependency.
//!
//! Layout:
//! - [`cpu`]      — hart state, fetch/decode/execute, CSRs, traps
//! - [`mmu`]      — Sv39 translation + TLB
//! - [`bus`]      — physical memory map dispatch
//! - [`devices`]  — CLINT, PLIC, 16550 UART, Goldfish RTC, virtio block /
//!   network / entropy / input, the `simple-framebuffer` display geometry,
//!   sparse block backing store
//! - [`machine`]  — top-level machine: run loop, blob loading, host I/O
//! - [`snapshot`] — post-boot state capture/restore (docs/disk.md)
//! - `hostnet`    — host-agnostic user-mode NAT layer, behind feature `hostnet`

pub mod bus;
pub mod cpu;
pub mod devices;
#[cfg(feature = "hostnet")]
pub mod hostnet;
pub mod machine;
pub mod mmu;
pub mod ram;
pub mod snapshot;
pub mod trap;

pub use machine::{Machine, RunEvent};
pub use snapshot::{DiskOverlay, RestoreCheck, SnapshotHeader, SnapshotIdentity};
