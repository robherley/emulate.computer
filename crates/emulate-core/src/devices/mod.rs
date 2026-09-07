//! Emulated devices in the QEMU `virt` memory map.

pub mod clint;
pub mod framebuffer;
pub mod framebuffer_blitter;
pub mod goldfish_rtc;
pub mod net;
pub mod plic;
pub mod simple_irq;
pub mod sparse_block;
pub mod uart;
pub mod virtio_blk;
pub mod virtio_input;
pub(crate) mod virtio_mmio;
pub mod virtio_net;
pub mod virtio_rng;
pub(crate) mod virtqueue;

pub mod virtio_console;
