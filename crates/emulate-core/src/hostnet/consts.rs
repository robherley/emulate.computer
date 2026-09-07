//! The slirp-style address plan (docs/networking.md).

use smoltcp::wire::{EthernetAddress, Ipv4Address};

/// `10.0.2.0/24` — the emulated LAN.
pub const NETWORK: [u8; 4] = [10, 0, 2, 0];
/// Prefix length of [`NETWORK`].
pub const PREFIX_LEN: u8 = 24;
/// `255.255.255.0`
pub const SUBNET_MASK: [u8; 4] = [255, 255, 255, 0];
/// The single address handed to the guest by DHCP.
pub const GUEST_IP: [u8; 4] = [10, 0, 2, 15];
/// Gateway / NAT address. Answers ARP, ICMP echo, and is the DHCP router.
pub const GATEWAY_IP: [u8; 4] = [10, 0, 2, 2];
/// Recursive resolver address. Answers ARP, ICMP echo and DNS on UDP 53.
pub const DNS_IP: [u8; 4] = [10, 0, 2, 3];
/// Directed broadcast for [`NETWORK`].
pub const SUBNET_BROADCAST_IP: [u8; 4] = [10, 0, 2, 255];
/// `255.255.255.255`
pub const LIMITED_BROADCAST_IP: [u8; 4] = [255, 255, 255, 255];

/// MAC for everything the NAT layer impersonates: gateway, DNS, and every
/// other in-subnet address it proxy-ARPs for.
pub const GATEWAY_MAC: [u8; 6] = [0x52, 0x55, 0x0a, 0x00, 0x02, 0x02];
/// Ethernet broadcast address.
pub const BROADCAST_MAC: [u8; 6] = [0xff; 6];

/// Link MTU. No offloads, no jumbo frames.
pub const MTU: usize = 1500;

/// DHCP lease handed out, in seconds.
pub const DHCP_LEASE_SECS: u32 = 86_400;
/// TTL stamped on synthesized DNS answers.
pub const DNS_TTL: u32 = 60;

/// Bound on the world -> guest frame queue; oldest frames are dropped first.
pub const TO_GUEST_QUEUE_LIMIT: usize = 256;

/// Per-direction smoltcp TCP buffer size.
pub const TCP_BUFFER_BYTES: usize = 64 * 1024;

pub(crate) fn ip(addr: [u8; 4]) -> Ipv4Address {
    Ipv4Address::new(addr[0], addr[1], addr[2], addr[3])
}

pub(crate) fn mac(addr: [u8; 6]) -> EthernetAddress {
    EthernetAddress(addr)
}

/// True when `addr` is inside `10.0.2.0/24`.
pub(crate) fn in_subnet(addr: [u8; 4]) -> bool {
    addr[0] == NETWORK[0] && addr[1] == NETWORK[1] && addr[2] == NETWORK[2]
}
