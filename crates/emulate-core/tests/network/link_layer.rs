//! ARP, DHCP, ICMP and frame-queue behaviour, all at the Ethernet level.

use super::common;

use common::*;
use emulate_core::hostnet::consts::*;
use smoltcp::wire::{
    ArpOperation, ArpRepr, DhcpMessageType, DhcpPacket, DhcpRepr, EthernetAddress,
    EthernetProtocol, Icmpv4Repr, Ipv4Address,
};

// --------------------------------------------------------------------- ARP

#[test]
fn arp_who_has_gateway_is_answered_with_the_gateway_mac() {
    let mut h = Harness::new();
    h.send(arp_request(GATEWAY_IP));
    let frames = h.drain();
    assert_eq!(frames.len(), 1, "exactly one ARP reply");

    let (repr, eth_src, eth_dst) = parse_arp(&frames[0]).expect("an ARP frame");
    assert_eq!(eth_src, GATEWAY_MAC);
    assert_eq!(eth_dst, GUEST_MAC);
    let ArpRepr::EthernetIpv4 {
        operation,
        source_hardware_addr,
        source_protocol_addr,
        target_hardware_addr,
        target_protocol_addr,
    } = repr
    else {
        panic!("not an ethernet/ipv4 ARP");
    };
    assert_eq!(operation, ArpOperation::Reply);
    assert_eq!(source_hardware_addr, EthernetAddress(GATEWAY_MAC));
    assert_eq!(source_protocol_addr, ip(GATEWAY_IP));
    assert_eq!(target_hardware_addr, EthernetAddress(GUEST_MAC));
    assert_eq!(target_protocol_addr, ip(GUEST_IP));
}

#[test]
fn arp_who_has_dns_is_answered_with_the_gateway_mac() {
    let mut h = Harness::new();
    h.send(arp_request(DNS_IP));
    let frames = h.drain();
    assert_eq!(frames.len(), 1);
    let (repr, _, _) = parse_arp(&frames[0]).unwrap();
    let ArpRepr::EthernetIpv4 {
        source_hardware_addr,
        source_protocol_addr,
        ..
    } = repr
    else {
        panic!()
    };
    assert_eq!(source_protocol_addr, ip(DNS_IP), "answers as 10.0.2.3");
    assert_eq!(source_hardware_addr, EthernetAddress(GATEWAY_MAC));
}

#[test]
fn arp_for_the_guests_own_address_is_never_answered() {
    // udhcpc ARP-probes its own lease before accepting it; a reply here would
    // look like a duplicate address and abort DHCP.
    let mut h = Harness::new();
    h.send(arp_request(GUEST_IP));
    assert!(h.drain().is_empty());
}

#[test]
fn arp_outside_the_subnet_is_not_answered() {
    let mut h = Harness::new();
    h.send(arp_request([8, 8, 8, 8]));
    assert!(h.drain().is_empty());
}

#[test]
fn the_guest_mac_is_learned_from_frames() {
    let mut h = Harness::new();
    assert_eq!(h.nat.guest_mac(), None);
    h.send(arp_request(GATEWAY_IP));
    assert_eq!(h.nat.guest_mac(), Some(GUEST_MAC));
}

// -------------------------------------------------------------------- DHCP

fn dhcp_message(kind: DhcpMessageType, requested: Option<[u8; 4]>, xid: u32) -> Vec<u8> {
    let repr = DhcpRepr {
        message_type: kind,
        transaction_id: xid,
        secs: 0,
        client_hardware_address: EthernetAddress(GUEST_MAC),
        client_ip: Ipv4Address::UNSPECIFIED,
        your_ip: Ipv4Address::UNSPECIFIED,
        server_ip: Ipv4Address::UNSPECIFIED,
        router: None,
        subnet_mask: None,
        relay_agent_ip: Ipv4Address::UNSPECIFIED,
        broadcast: true,
        requested_ip: requested.map(ip),
        client_identifier: Some(EthernetAddress(GUEST_MAC)),
        server_identifier: None,
        parameter_request_list: Some(&[1, 3, 6, 51]),
        dns_servers: None,
        max_size: Some(1500),
        lease_duration: None,
        renew_duration: None,
        rebind_duration: None,
        additional_options: &[],
    };
    let mut buf = vec![0u8; repr.buffer_len()];
    {
        let mut pkt = DhcpPacket::new_unchecked(&mut buf[..]);
        repr.emit(&mut pkt).unwrap();
    }
    eth(
        BROADCAST_MAC,
        GUEST_MAC,
        EthernetProtocol::Ipv4,
        &udp([0, 0, 0, 0], 68, [255, 255, 255, 255], 67, &buf),
    )
}

fn expect_dhcp_reply(frame: &[u8]) -> DhcpRepr<'static> {
    let udp = parse_udp(frame).expect("a UDP datagram");
    assert_eq!(udp.ip.src, GATEWAY_IP, "reply comes from the gateway");
    assert_eq!(udp.ip.dst, [255, 255, 255, 255], "broadcast reply");
    assert_eq!(udp.ip.eth_dst, BROADCAST_MAC);
    assert_eq!(udp.ip.eth_src, GATEWAY_MAC);
    assert_eq!(udp.src_port, 67);
    assert_eq!(udp.dst_port, 68);
    let owned: &'static [u8] = Box::leak(udp.payload.into_boxed_slice());
    let pkt: &'static DhcpPacket<&'static [u8]> = Box::leak(Box::new(
        DhcpPacket::new_checked(owned).expect("a DHCP packet"),
    ));
    DhcpRepr::parse(pkt).expect("parses")
}

#[test]
fn dhcp_discover_is_offered_the_full_lease() {
    let mut h = Harness::new();
    h.send(dhcp_message(DhcpMessageType::Discover, None, 0xdead_beef));
    let frames = h.drain();
    assert_eq!(frames.len(), 1, "one OFFER");

    let offer = expect_dhcp_reply(&frames[0]);
    assert_eq!(offer.message_type, DhcpMessageType::Offer);
    assert_eq!(offer.transaction_id, 0xdead_beef, "xid echoed");
    assert_eq!(offer.your_ip, ip(GUEST_IP));
    assert_eq!(offer.server_ip, ip(GATEWAY_IP));
    assert_eq!(offer.router, Some(ip(GATEWAY_IP)));
    assert_eq!(offer.subnet_mask, Some(ip(SUBNET_MASK)));
    assert_eq!(offer.server_identifier, Some(ip(GATEWAY_IP)));
    assert_eq!(offer.lease_duration, Some(DHCP_LEASE_SECS));
    assert_eq!(
        offer.dns_servers.as_ref().map(|v| v.as_slice()),
        Some(&[ip(DNS_IP)][..]),
        "10.0.2.3 is the only resolver"
    );
    assert_eq!(
        offer.client_hardware_address,
        EthernetAddress(GUEST_MAC),
        "chaddr echoed"
    );
}

#[test]
fn dhcp_request_is_acked() {
    let mut h = Harness::new();
    h.send(dhcp_message(DhcpMessageType::Discover, None, 7));
    h.drain();
    h.send(dhcp_message(DhcpMessageType::Request, Some(GUEST_IP), 7));
    let frames = h.drain();
    assert_eq!(frames.len(), 1, "one ACK");
    let ack = expect_dhcp_reply(&frames[0]);
    assert_eq!(ack.message_type, DhcpMessageType::Ack);
    assert_eq!(ack.your_ip, ip(GUEST_IP));
    assert_eq!(ack.lease_duration, Some(86_400));
    assert_eq!(ack.transaction_id, 7);
}

#[test]
fn dhcp_request_for_a_foreign_address_is_naked() {
    let mut h = Harness::new();
    h.send(dhcp_message(
        DhcpMessageType::Request,
        Some([10, 0, 2, 99]),
        1,
    ));
    let frames = h.drain();
    assert_eq!(frames.len(), 1);
    let nak = expect_dhcp_reply(&frames[0]);
    assert_eq!(nak.message_type, DhcpMessageType::Nak);
    assert_eq!(nak.your_ip, Ipv4Address::UNSPECIFIED);
}

#[test]
fn dhcp_release_gets_no_reply() {
    let mut h = Harness::new();
    h.send(dhcp_message(DhcpMessageType::Release, None, 1));
    assert!(h.drain().is_empty());
}

// -------------------------------------------------------------------- ICMP

#[test]
fn icmp_echo_to_the_gateway_is_replied() {
    let mut h = Harness::new();
    h.send(icmp_echo(GATEWAY_IP, 0x1234, 7, b"abcdefgh"));
    let frames = h.drain();
    assert_eq!(frames.len(), 1);
    let (ipp, repr) = parse_icmp(&frames[0]).expect("an echo reply");
    assert_eq!(ipp.src, GATEWAY_IP);
    assert_eq!(ipp.dst, GUEST_IP);
    assert_eq!(ipp.eth_src, GATEWAY_MAC);
    assert_eq!(ipp.eth_dst, GUEST_MAC);
    match repr {
        Icmpv4Repr::EchoReply {
            ident,
            seq_no,
            data,
        } => {
            assert_eq!(ident, 0x1234);
            assert_eq!(seq_no, 7);
            assert_eq!(data, b"abcdefgh");
        }
        other => panic!("expected echo reply, got {other:?}"),
    }
}

#[test]
fn icmp_echo_to_the_resolver_is_replied_from_the_resolver() {
    let mut h = Harness::new();
    h.send(icmp_echo(DNS_IP, 1, 1, b"x"));
    let frames = h.drain();
    assert_eq!(frames.len(), 1);
    let (ipp, _) = parse_icmp(&frames[0]).unwrap();
    assert_eq!(ipp.src, DNS_IP);
}

#[test]
fn icmp_echo_to_the_wider_internet_is_dropped() {
    let mut h = Harness::new();
    h.send(icmp_echo([1, 1, 1, 1], 1, 1, b"x"));
    assert!(h.drain().is_empty(), "no forward-and-fake ICMP");
}

// ------------------------------------------------------------- misc framing

#[test]
fn non_ip_ethertypes_are_ignored() {
    let mut h = Harness::new();
    // 802.1Q-tagged and IPv6 frames: both silently dropped.
    h.send(eth(
        GATEWAY_MAC,
        GUEST_MAC,
        EthernetProtocol::Unknown(0x8100),
        &[0; 60],
    ));
    h.send(eth(
        GATEWAY_MAC,
        GUEST_MAC,
        EthernetProtocol::Ipv6,
        &[0; 60],
    ));
    assert!(h.drain().is_empty());
}

#[test]
fn runt_frames_do_not_panic() {
    let mut h = Harness::new();
    h.send(vec![0u8; 3]);
    h.send(vec![]);
    h.send(eth(GATEWAY_MAC, GUEST_MAC, EthernetProtocol::Ipv4, &[0; 4]));
    h.send(eth(GATEWAY_MAC, GUEST_MAC, EthernetProtocol::Arp, &[0; 4]));
    assert!(h.drain().is_empty());
}

#[test]
fn the_to_guest_queue_is_bounded_and_drops_oldest() {
    let mut h = Harness::new();
    // One queued reply per ping, never drained in between. Directly-pushed
    // control replies may overshoot TO_GUEST_QUEUE_LIMIT (that gate applies to
    // smoltcp TCP egress); the hard drop bound is the 2x cap.
    let cap = TO_GUEST_QUEUE_LIMIT * 2;
    let total = cap + 50;
    for i in 0..total {
        h.send(icmp_echo(GATEWAY_IP, 1, i as u16, b"pad"));
    }
    let frames = h.drain();
    assert_eq!(
        frames.len(),
        cap,
        "queue is bounded at the pathological cap"
    );
    assert_eq!(h.nat.dropped_frames(), 50, "the overflow was counted");

    // Drop-oldest: the surviving window ends with the newest reply.
    let (_, last) = parse_icmp(frames.last().unwrap()).unwrap();
    match last {
        Icmpv4Repr::EchoReply { seq_no, .. } => assert_eq!(seq_no, (total - 1) as u16),
        _ => panic!(),
    }
    let (_, first) = parse_icmp(&frames[0]).unwrap();
    match first {
        Icmpv4Repr::EchoReply { seq_no, .. } => assert_eq!(seq_no, 50),
        _ => panic!(),
    }
}
