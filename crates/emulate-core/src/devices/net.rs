//! Host-side attachment point for the virtio-net device (see
//! docs/networking.md).
//!
//! The device hands the backend raw Ethernet frames (no virtio_net_hdr) and
//! polls it for frames to deliver to the guest; the trait is synchronous and
//! poll-based to match the machine run loop.
/// An Ethernet segment attached to the guest's virtio-net device.
pub trait NetBackend {
    /// Guest -> world. Best-effort delivery (like real Ethernet): a backend
    /// may drop frames under pressure.
    fn transmit(&mut self, frame: &[u8]);

    /// World -> guest. Returns one pending frame per call (`None` = nothing
    /// right now). Frames must be whole Ethernet frames, no virtio header.
    fn receive(&mut self) -> Option<Vec<u8>>;

    /// Housekeeping tick (retransmits, socket polling, timers), once per
    /// execution slice, with the current mtime-derived milliseconds.
    fn poll(&mut self, now_ms: u64);

    /// Earliest ms deadline at which `poll` must run again, if any. Caps WFI
    /// sleeps while flows are active without hot-spinning when idle.
    fn next_deadline_ms(&self) -> Option<u64> {
        None
    }
}
