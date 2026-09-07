//! Frame-level test harness: builds guest-side Ethernet frames and picks
//! apart the ones the NAT layer sends back.

#![allow(dead_code)]

use emulate_core::devices::net::NetBackend;
use emulate_core::hostnet::consts::{BROADCAST_MAC, GATEWAY_MAC, GUEST_IP};
use emulate_core::hostnet::testing::FakeSubstrate;
use emulate_core::hostnet::NatBackend;
use smoltcp::phy::ChecksumCapabilities;
use smoltcp::wire::{
    ArpOperation, ArpPacket, ArpRepr, EthernetAddress, EthernetFrame, EthernetProtocol,
    EthernetRepr, Icmpv4Packet, Icmpv4Repr, IpProtocol, Ipv4Address, Ipv4Packet, Ipv4Repr,
    TcpPacket, TcpRepr, UdpPacket, UdpRepr,
};

pub const GUEST_MAC: [u8; 6] = [0x52, 0x54, 0x00, 0x12, 0x34, 0x56];

pub fn ip(a: [u8; 4]) -> Ipv4Address {
    Ipv4Address::new(a[0], a[1], a[2], a[3])
}

pub fn mac(a: [u8; 6]) -> EthernetAddress {
    EthernetAddress(a)
}

pub fn ck() -> ChecksumCapabilities {
    ChecksumCapabilities::default()
}

/// The NAT under test plus a few conveniences.
pub struct Harness {
    pub nat: NatBackend<FakeSubstrate>,
    pub now_ms: u64,
}

impl Harness {
    pub fn new() -> Self {
        Self {
            nat: NatBackend::new(FakeSubstrate::new()),
            now_ms: 0,
        }
    }

    pub fn sub(&mut self) -> &mut FakeSubstrate {
        self.nat.substrate_mut()
    }

    pub fn send(&mut self, frame: Vec<u8>) {
        self.nat.transmit(&frame);
    }

    pub fn tick(&mut self, ms: u64) {
        self.now_ms += ms;
        self.nat.poll(self.now_ms);
    }

    /// Every frame currently queued for the guest.
    pub fn drain(&mut self) -> Vec<Vec<u8>> {
        let mut out = Vec::new();
        while let Some(f) = self.nat.receive() {
            out.push(f);
        }
        out
    }

    /// Poll once and drain.
    pub fn pump(&mut self) -> Vec<Vec<u8>> {
        self.tick(1);
        self.drain()
    }
}

// ------------------------------------------------------------------ builders

pub fn eth(dst: [u8; 6], src: [u8; 6], ty: EthernetProtocol, payload: &[u8]) -> Vec<u8> {
    let repr = EthernetRepr {
        src_addr: mac(src),
        dst_addr: mac(dst),
        ethertype: ty,
    };
    let mut buf = vec![0u8; repr.buffer_len() + payload.len()];
    let mut frame = EthernetFrame::new_unchecked(&mut buf[..]);
    repr.emit(&mut frame);
    frame.payload_mut().copy_from_slice(payload);
    buf
}

pub fn ipv4(src: [u8; 4], dst: [u8; 4], proto: IpProtocol, payload: &[u8]) -> Vec<u8> {
    let repr = Ipv4Repr {
        src_addr: ip(src),
        dst_addr: ip(dst),
        next_header: proto,
        payload_len: payload.len(),
        hop_limit: 64,
    };
    let mut buf = vec![0u8; repr.buffer_len() + payload.len()];
    let mut pkt = Ipv4Packet::new_unchecked(&mut buf[..]);
    repr.emit(&mut pkt, &ck());
    pkt.payload_mut().copy_from_slice(payload);
    buf
}

pub fn udp(src: [u8; 4], sp: u16, dst: [u8; 4], dp: u16, payload: &[u8]) -> Vec<u8> {
    let repr = UdpRepr {
        src_port: sp,
        dst_port: dp,
    };
    let mut body = vec![0u8; repr.header_len() + payload.len()];
    {
        let mut pkt = UdpPacket::new_unchecked(&mut body[..]);
        repr.emit(
            &mut pkt,
            &ip(src).into(),
            &ip(dst).into(),
            payload.len(),
            |p| p.copy_from_slice(payload),
            &ck(),
        );
    }
    ipv4(src, dst, IpProtocol::Udp, &body)
}

/// A complete guest -> NAT UDP frame.
pub fn guest_udp_frame(
    dst_mac: [u8; 6],
    src: [u8; 4],
    sp: u16,
    dst: [u8; 4],
    dp: u16,
    p: &[u8],
) -> Vec<u8> {
    eth(
        dst_mac,
        GUEST_MAC,
        EthernetProtocol::Ipv4,
        &udp(src, sp, dst, dp, p),
    )
}

pub fn arp_request(target: [u8; 4]) -> Vec<u8> {
    let repr = ArpRepr::EthernetIpv4 {
        operation: ArpOperation::Request,
        source_hardware_addr: mac(GUEST_MAC),
        source_protocol_addr: ip(GUEST_IP),
        target_hardware_addr: mac([0; 6]),
        target_protocol_addr: ip(target),
    };
    let mut body = vec![0u8; repr.buffer_len()];
    let mut pkt = ArpPacket::new_unchecked(&mut body[..]);
    repr.emit(&mut pkt);
    eth(BROADCAST_MAC, GUEST_MAC, EthernetProtocol::Arp, &body)
}

pub fn icmp_echo(dst: [u8; 4], ident: u16, seq: u16, data: &[u8]) -> Vec<u8> {
    let repr = Icmpv4Repr::EchoRequest {
        ident,
        seq_no: seq,
        data,
    };
    let mut body = vec![0u8; repr.buffer_len()];
    {
        let mut pkt = Icmpv4Packet::new_unchecked(&mut body[..]);
        repr.emit(&mut pkt, &ck());
    }
    eth(
        GATEWAY_MAC,
        GUEST_MAC,
        EthernetProtocol::Ipv4,
        &ipv4(GUEST_IP, dst, IpProtocol::Icmp, &body),
    )
}

pub fn tcp_frame(dst: [u8; 4], repr: &TcpRepr<'_>) -> Vec<u8> {
    let mut body = vec![0u8; repr.buffer_len()];
    {
        let mut pkt = TcpPacket::new_unchecked(&mut body[..]);
        repr.emit(&mut pkt, &ip(GUEST_IP).into(), &ip(dst).into(), &ck());
    }
    eth(
        GATEWAY_MAC,
        GUEST_MAC,
        EthernetProtocol::Ipv4,
        &ipv4(GUEST_IP, dst, IpProtocol::Tcp, &body),
    )
}

// ------------------------------------------------------------------- parsers

pub struct ParsedIp {
    pub src: [u8; 4],
    pub dst: [u8; 4],
    pub protocol: IpProtocol,
    pub payload: Vec<u8>,
    pub eth_src: [u8; 6],
    pub eth_dst: [u8; 6],
}

/// Parse a NAT -> guest frame, verifying checksums along the way.
pub fn parse_ipv4(frame: &[u8]) -> Option<ParsedIp> {
    let eth = EthernetFrame::new_checked(frame).ok()?;
    if eth.ethertype() != EthernetProtocol::Ipv4 {
        return None;
    }
    let pkt = Ipv4Packet::new_checked(eth.payload()).ok()?;
    assert!(pkt.verify_checksum(), "bad IPv4 header checksum");
    let total = pkt.total_len() as usize;
    let hdr = pkt.header_len() as usize;
    Some(ParsedIp {
        src: pkt.src_addr().octets(),
        dst: pkt.dst_addr().octets(),
        protocol: pkt.next_header(),
        payload: eth.payload()[hdr..total].to_vec(),
        eth_src: eth.src_addr().0,
        eth_dst: eth.dst_addr().0,
    })
}

pub struct ParsedUdp {
    pub ip: ParsedIp,
    pub src_port: u16,
    pub dst_port: u16,
    pub payload: Vec<u8>,
}

pub fn parse_udp(frame: &[u8]) -> Option<ParsedUdp> {
    let ipp = parse_ipv4(frame)?;
    if ipp.protocol != IpProtocol::Udp {
        return None;
    }
    let pkt = UdpPacket::new_checked(&ipp.payload[..]).ok()?;
    // Round-trip through UdpRepr::parse, which validates the checksum.
    let repr = UdpRepr::parse(&pkt, &ip(ipp.src).into(), &ip(ipp.dst).into(), &ck())
        .expect("bad UDP checksum");
    let payload = pkt.payload().to_vec();
    Some(ParsedUdp {
        src_port: repr.src_port,
        dst_port: repr.dst_port,
        payload,
        ip: ipp,
    })
}

pub struct ParsedTcp {
    pub ip: ParsedIp,
    pub src_port: u16,
    pub dst_port: u16,
    pub seq: i32,
    pub ack: Option<i32>,
    pub syn: bool,
    pub fin: bool,
    pub rst: bool,
    pub ack_flag: bool,
    pub payload: Vec<u8>,
}

pub fn parse_tcp(frame: &[u8]) -> Option<ParsedTcp> {
    let ipp = parse_ipv4(frame)?;
    if ipp.protocol != IpProtocol::Tcp {
        return None;
    }
    let pkt = TcpPacket::new_checked(&ipp.payload[..]).ok()?;
    assert!(
        pkt.verify_checksum(&ip(ipp.src).into(), &ip(ipp.dst).into()),
        "bad TCP checksum"
    );
    Some(ParsedTcp {
        src_port: pkt.src_port(),
        dst_port: pkt.dst_port(),
        seq: pkt.seq_number().0,
        ack: if pkt.ack() {
            Some(pkt.ack_number().0)
        } else {
            None
        },
        syn: pkt.syn(),
        fin: pkt.fin(),
        rst: pkt.rst(),
        ack_flag: pkt.ack(),
        payload: pkt.payload().to_vec(),
        ip: ipp,
    })
}

pub fn parse_icmp(frame: &[u8]) -> Option<(ParsedIp, Icmpv4Repr<'static>)> {
    let ipp = parse_ipv4(frame)?;
    if ipp.protocol != IpProtocol::Icmp {
        return None;
    }
    let pkt = Icmpv4Packet::new_checked(&ipp.payload[..]).ok()?;
    assert!(pkt.verify_checksum(), "bad ICMP checksum");
    let ident = pkt.echo_ident();
    let seq_no = pkt.echo_seq_no();
    let data: &'static [u8] = Box::leak(pkt.data().to_vec().into_boxed_slice());
    let is_reply = pkt.msg_type() == smoltcp::wire::Icmpv4Message::EchoReply;
    if !is_reply {
        return None;
    }
    Some((
        ipp,
        Icmpv4Repr::EchoReply {
            ident,
            seq_no,
            data,
        },
    ))
}

pub fn parse_arp(frame: &[u8]) -> Option<(ArpRepr, [u8; 6], [u8; 6])> {
    let eth = EthernetFrame::new_checked(frame).ok()?;
    if eth.ethertype() != EthernetProtocol::Arp {
        return None;
    }
    let pkt = ArpPacket::new_checked(eth.payload()).ok()?;
    let repr = ArpRepr::parse(&pkt).ok()?;
    Some((repr, eth.src_addr().0, eth.dst_addr().0))
}
