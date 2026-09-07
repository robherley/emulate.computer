//! Operating-system behavior, using explicitly prepared images.
//! Run `just test-guest` or `just test-stress`; see docs/testing.md.

mod artifacts;
mod support;

#[path = "guest/boot.rs"]
mod boot;
#[path = "guest/display.rs"]
mod display;
#[path = "guest/network.rs"]
mod network;
#[path = "guest/snapshot.rs"]
mod snapshot;
#[path = "guest/storage.rs"]
mod storage;
#[path = "guest/xv6.rs"]
mod xv6;
