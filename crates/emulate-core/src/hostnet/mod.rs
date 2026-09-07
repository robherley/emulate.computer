//! Host-agnostic user-mode NAT (feature `hostnet`), docs/networking.md.
//!
//! Implements [`crate::devices::net::NetBackend`] over a [`SocketSubstrate`]:
//! ARP, DHCP, ICMP, DNS and the TCP flow table live here, and each host
//! supplies only the substrate (in practice [`RelayClient`] on both).
//!
//! TCP is NAT'd; UDP is answered in-process (DHCP, resolver at 10.0.2.3) or
//! dropped. Nothing here touches `std::net`, so it builds for
//! `wasm32-unknown-unknown` unchanged.
//!
//! ```no_run
//! use emulate_core::hostnet::{testing::FakeSubstrate, NatBackend};
//! use emulate_core::devices::net::NetBackend;
//!
//! let mut nat = NatBackend::new(FakeSubstrate::new());
//! nat.poll(0);
//! ```

mod backend;
mod device;
mod wire;

pub mod consts;
pub mod dns;
pub mod relay_client;
pub mod substrate;
#[cfg(feature = "testing")]
pub mod testing;

pub use backend::NatBackend;
pub use consts::GATEWAY_IP;
pub use relay_client::{
    parse_ipv4, ConnId, RelayClient, RelayTransport, TransportEvent, ACTIVE_POLL_INTERVAL_MS,
    SEND_CAP_BYTES,
};
pub use substrate::{DnsError, FlowError, FlowHandle, SocketSubstrate, SubstrateEvent};
