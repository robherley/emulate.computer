//! Frame construction helpers.
//!
//! Everything is built through smoltcp's `Repr::emit` rather than hand-packed,
//! so IPv4/UDP/TCP/ICMP checksums are right by construction.

use smoltcp::phy::ChecksumCapabilities;
use smoltcp::wire::{
    EthernetAddress, EthernetFrame, EthernetProtocol, EthernetRepr, Icmpv4Packet, Icmpv4Repr,
    IpProtocol, Ipv4Address, Ipv4Packet, Ipv4Repr, TcpPacket, TcpRepr, UdpPacket, UdpRepr,
};

pub(crate) fn checksums() -> ChecksumCapabilities {
    ChecksumCapabilities::default()
}

/// Wrap `payload_len` bytes of `ethertype` payload in an Ethernet header.
pub(crate) fn eth_frame(
    dst: EthernetAddress,
    src: EthernetAddress,
    ethertype: EthernetProtocol,
    payload_len: usize,
    emit_payload: impl FnOnce(&mut [u8]),
) -> Vec<u8> {
    let repr = EthernetRepr {
        src_addr: src,
        dst_addr: dst,
        ethertype,
    };
    let mut buf = vec![0u8; repr.buffer_len() + payload_len];
    {
        let mut frame = EthernetFrame::new_unchecked(&mut buf[..]);
        repr.emit(&mut frame);
        emit_payload(frame.payload_mut());
    }
    buf
}

/// Build an IPv4 packet (header + payload) with a correct header checksum.
pub(crate) fn ipv4_packet(
    src: Ipv4Address,
    dst: Ipv4Address,
    protocol: IpProtocol,
    payload_len: usize,
    emit_payload: impl FnOnce(&mut [u8]),
) -> Vec<u8> {
    let repr = Ipv4Repr {
        src_addr: src,
        dst_addr: dst,
        next_header: protocol,
        payload_len,
        hop_limit: 64,
    };
    let mut buf = vec![0u8; repr.buffer_len() + payload_len];
    {
        let mut pkt = Ipv4Packet::new_unchecked(&mut buf[..]);
        repr.emit(&mut pkt, &checksums());
        emit_payload(pkt.payload_mut());
    }
    buf
}

/// IPv4 + UDP datagram carrying `payload`, with both checksums filled in.
pub(crate) fn udp_datagram(
    src: Ipv4Address,
    src_port: u16,
    dst: Ipv4Address,
    dst_port: u16,
    payload: &[u8],
) -> Vec<u8> {
    let udp = UdpRepr { src_port, dst_port };
    let udp_len = udp.header_len() + payload.len();
    ipv4_packet(src, dst, IpProtocol::Udp, udp_len, |buf| {
        let mut pkt = UdpPacket::new_unchecked(buf);
        udp.emit(
            &mut pkt,
            &src.into(),
            &dst.into(),
            payload.len(),
            |p| p.copy_from_slice(payload),
            &checksums(),
        );
    })
}

/// IPv4 + ICMPv4 message.
pub(crate) fn icmp_packet(src: Ipv4Address, dst: Ipv4Address, repr: &Icmpv4Repr<'_>) -> Vec<u8> {
    let len = repr.buffer_len();
    ipv4_packet(src, dst, IpProtocol::Icmp, len, |buf| {
        let mut pkt = Icmpv4Packet::new_unchecked(buf);
        repr.emit(&mut pkt, &checksums());
    })
}

/// IPv4 + TCP segment. Only for what smoltcp cannot generate for us: a RST for
/// a flow that never got a socket.
pub(crate) fn tcp_segment(src: Ipv4Address, dst: Ipv4Address, repr: &TcpRepr<'_>) -> Vec<u8> {
    let len = repr.buffer_len();
    ipv4_packet(src, dst, IpProtocol::Tcp, len, |buf| {
        let mut pkt = TcpPacket::new_unchecked(buf);
        repr.emit(&mut pkt, &src.into(), &dst.into(), &checksums());
    })
}
