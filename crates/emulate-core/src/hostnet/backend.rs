//! `NatBackend` — a slirp-style user-mode NAT that terminates the guest's TCP
//! in-process and turns each flow into requests against a [`SocketSubstrate`].
//!
//! Layering, from the guest inwards:
//!
//! ```text
//! Ethernet ──┬─ ARP        -> answered here (proxy-ARP for 10.0.2.0/24)
//!            └─ IPv4 ──┬─ ICMP  -> echo replies for .2/.3, else dropped
//!                      ├─ UDP ──┬─ :67  -> DHCP server
//!                      │        ├─ 10.0.2.3:53 -> DNS resolver
//!                      │        └─ else -> dropped (no generic UDP NAT)
//!                      └─ TCP  -> smoltcp (any-ip listener) -> substrate tcp flow
//! ```
//!
//! UDP is answered here or not at all: DHCP and the resolver at 10.0.2.3 are
//! synthesized above the substrate, everything else is dropped.

use std::collections::{HashMap, VecDeque};

use crate::devices::net::NetBackend;
use smoltcp::iface::{Config, Interface, SocketHandle, SocketSet};
use smoltcp::socket::tcp;
use smoltcp::time::Instant;
use smoltcp::wire::{
    ArpOperation, ArpPacket, ArpRepr, DhcpMessageType, DhcpPacket, DhcpRepr, EthernetAddress,
    EthernetFrame, EthernetProtocol, HardwareAddress, Icmpv4Message, Icmpv4Packet, Icmpv4Repr,
    IpCidr, IpProtocol, Ipv4Address, Ipv4Cidr, Ipv4Packet, TcpControl, TcpPacket, TcpRepr,
    TcpSeqNumber, UdpPacket,
};

use crate::hostnet::consts::*;
use crate::hostnet::device::QueueDevice;
use crate::hostnet::dns;
use crate::hostnet::substrate::{DnsError, FlowHandle, SocketSubstrate, SubstrateEvent};
use crate::hostnet::wire::{eth_frame, icmp_packet, tcp_segment, udp_datagram};

const DHCP_SERVER_PORT: u16 = 67;
const DHCP_CLIENT_PORT: u16 = 68;
const DNS_PORT: u16 = 53;

#[derive(Copy, Clone, PartialEq, Eq, Hash, Debug)]
struct TcpKey {
    guest_port: u16,
    dst: [u8; 4],
    dst_port: u16,
}

struct TcpFlow {
    sub: FlowHandle,
    /// `None` until the substrate reports the connection is up: the guest's
    /// SYN is held so we never SYN-ACK a connection that does not exist.
    sock: Option<SocketHandle>,
    /// The guest's SYN, replayed into smoltcp on `TcpConnected`.
    pending_syn: Option<Vec<u8>>,
    /// Sequence number of the held SYN, for building a RST by hand if the
    /// connect fails before there is ever a socket.
    syn_seq: u32,
    /// Bytes from the substrate that did not fit in smoltcp's send buffer.
    to_guest: VecDeque<u8>,
    /// The substrate reported EOF; send FIN once `to_guest` drains.
    sub_eof: bool,
    /// We have already told the substrate the guest closed.
    told_sub_closed: bool,
    /// Flow is dead; do not talk to the substrate about it again.
    dead: bool,
}

struct PendingDns {
    query: dns::Query,
    guest_ip: [u8; 4],
    guest_port: u16,
    server_ip: [u8; 4],
}

/// A user-mode NAT implementing [`NetBackend`].
pub struct NatBackend<S: SocketSubstrate> {
    sub: S,
    iface: Interface,
    device: QueueDevice,
    sockets: SocketSet<'static>,

    guest_mac: Option<EthernetAddress>,
    to_guest: VecDeque<Vec<u8>>,
    dropped_frames: u64,

    tcp: HashMap<TcpKey, TcpFlow>,
    tcp_by_handle: HashMap<FlowHandle, TcpKey>,
    dns_pending: HashMap<FlowHandle, PendingDns>,

    now_ms: u64,
    next_deadline: Option<u64>,
}

impl<S: SocketSubstrate> NatBackend<S> {
    pub fn new(sub: S) -> Self {
        let mut device = QueueDevice::new();
        let config = Config::new(HardwareAddress::Ip);
        let mut iface = Interface::new(config, &mut device, Instant::from_millis(0));
        iface.update_ip_addrs(|addrs| {
            addrs
                .push(IpCidr::Ipv4(Ipv4Cidr::new(ip(GATEWAY_IP), PREFIX_LEN)))
                .expect("one address fits");
        });
        // set_any_ip lets a socket listen() on the guest's real destination,
        // an address we do not own. smoltcp additionally requires a default
        // route whose next hop is one of our own addresses.
        iface.set_any_ip(true);
        iface
            .routes_mut()
            .add_default_ipv4_route(ip(GATEWAY_IP))
            .expect("route table has room");

        Self {
            sub,
            iface,
            device,
            sockets: SocketSet::new(Vec::new()),
            guest_mac: None,
            to_guest: VecDeque::new(),
            dropped_frames: 0,
            tcp: HashMap::new(),
            tcp_by_handle: HashMap::new(),
            dns_pending: HashMap::new(),
            now_ms: 0,
            next_deadline: None,
        }
    }

    /// The guest MAC learned from received frames, if any.
    pub fn guest_mac(&self) -> Option<[u8; 6]> {
        self.guest_mac.map(|m| m.0)
    }

    /// Frames dropped because the world -> guest queue was full.
    pub fn dropped_frames(&self) -> u64 {
        self.dropped_frames
    }

    /// Borrow the substrate (status lines, tests).
    pub fn substrate(&self) -> &S {
        &self.sub
    }

    pub fn substrate_mut(&mut self) -> &mut S {
        &mut self.sub
    }

    // ---------------------------------------------------------------- output

    fn push_to_guest(&mut self, frame: Vec<u8>) {
        // Only directly-pushed control traffic (ARP/DHCP/ICMP/DNS) can reach
        // this bound: TCP egress is gated at TO_GUEST_QUEUE_LIMIT by
        // drain_device_tx and waits in socket buffers instead. Dropping
        // smoltcp-emitted segments would force RTO retransmit stalls, so the
        // hard drop is reserved for a pathological 2x overrun.
        if self.to_guest.len() >= TO_GUEST_QUEUE_LIMIT * 2 {
            self.to_guest.pop_front();
            self.dropped_frames += 1;
        }
        self.to_guest.push_back(frame);
    }

    /// Wrap an IPv4 packet destined for the guest in an Ethernet header.
    fn emit_ip_to_guest(&mut self, packet: Vec<u8>) {
        let dst = self.guest_mac.unwrap_or(mac(BROADCAST_MAC));
        let frame = eth_frame(
            dst,
            mac(GATEWAY_MAC),
            EthernetProtocol::Ipv4,
            packet.len(),
            |buf| buf.copy_from_slice(&packet),
        );
        self.push_to_guest(frame);
    }

    fn emit_broadcast_ip_to_guest(&mut self, packet: Vec<u8>) {
        let frame = eth_frame(
            mac(BROADCAST_MAC),
            mac(GATEWAY_MAC),
            EthernetProtocol::Ipv4,
            packet.len(),
            |buf| buf.copy_from_slice(&packet),
        );
        self.push_to_guest(frame);
    }

    // ----------------------------------------------------------------- input

    fn handle_frame(&mut self, frame: &[u8]) {
        let Ok(eth) = EthernetFrame::new_checked(frame) else {
            return;
        };
        let src = eth.src_addr();
        if src.is_unicast() {
            self.guest_mac = Some(src);
        }
        match eth.ethertype() {
            EthernetProtocol::Arp => self.handle_arp(eth.payload()),
            EthernetProtocol::Ipv4 => self.handle_ipv4(eth.payload()),
            // 802.1Q, IPv6, LLDP, ...: nothing the guest needs on this link.
            _ => {}
        }
    }

    fn handle_arp(&mut self, payload: &[u8]) {
        let Ok(pkt) = ArpPacket::new_checked(payload) else {
            return;
        };
        let Ok(ArpRepr::EthernetIpv4 {
            operation,
            source_hardware_addr,
            source_protocol_addr,
            target_protocol_addr,
            ..
        }) = ArpRepr::parse(&pkt)
        else {
            return;
        };
        if operation != ArpOperation::Request {
            return;
        }
        if source_hardware_addr.is_unicast() {
            self.guest_mac = Some(source_hardware_addr);
        }
        let target = target_protocol_addr.octets();
        // Proxy-ARP for the whole subnet, minus the network/broadcast
        // addresses and the guest itself: udhcpc ARP-probes its own lease
        // before accepting it and must hear silence.
        if !in_subnet(target)
            || target == GUEST_IP
            || target == NETWORK
            || target == SUBNET_BROADCAST_IP
        {
            return;
        }
        let reply = ArpRepr::EthernetIpv4 {
            operation: ArpOperation::Reply,
            source_hardware_addr: mac(GATEWAY_MAC),
            source_protocol_addr: target_protocol_addr,
            target_hardware_addr: source_hardware_addr,
            target_protocol_addr: source_protocol_addr,
        };
        let frame = eth_frame(
            source_hardware_addr,
            mac(GATEWAY_MAC),
            EthernetProtocol::Arp,
            reply.buffer_len(),
            |buf| {
                let mut pkt = ArpPacket::new_unchecked(buf);
                reply.emit(&mut pkt);
            },
        );
        self.push_to_guest(frame);
    }

    fn handle_ipv4(&mut self, payload: &[u8]) {
        let Ok(pkt) = Ipv4Packet::new_checked(payload) else {
            return;
        };
        // Fragments are not reassembled; the guest never needs to send them.
        if pkt.more_frags() || pkt.frag_offset() != 0 {
            return;
        }
        let src = pkt.src_addr().octets();
        let dst = pkt.dst_addr().octets();
        let protocol = pkt.next_header();
        // Trim Ethernet padding: no parser should see bytes past the IPv4
        // total length.
        let total_len = pkt.total_len() as usize;
        let body = pkt.payload();

        match protocol {
            IpProtocol::Udp => self.handle_udp(src, dst, body),
            IpProtocol::Icmp => self.handle_icmp(src, dst, body),
            IpProtocol::Tcp => self.handle_tcp(src, dst, body, &payload[..total_len]),
            _ => {}
        }
    }

    fn handle_icmp(&mut self, src: [u8; 4], dst: [u8; 4], body: &[u8]) {
        if dst != GATEWAY_IP && dst != DNS_IP {
            // Forwarding ICMP is out of scope (docs/networking.md).
            return;
        }
        let Ok(pkt) = Icmpv4Packet::new_checked(body) else {
            return;
        };
        if pkt.msg_type() != Icmpv4Message::EchoRequest {
            return;
        }
        let Ok(repr) = Icmpv4Repr::parse(&pkt, &crate::hostnet::wire::checksums()) else {
            return;
        };
        let Icmpv4Repr::EchoRequest {
            ident,
            seq_no,
            data,
        } = repr
        else {
            return;
        };
        let reply = Icmpv4Repr::EchoReply {
            ident,
            seq_no,
            data,
        };
        let packet = icmp_packet(ip(dst), ip(src), &reply);
        self.emit_ip_to_guest(packet);
    }

    fn handle_udp(&mut self, src: [u8; 4], dst: [u8; 4], body: &[u8]) {
        let Ok(pkt) = UdpPacket::new_checked(body) else {
            return;
        };
        // The UDP checksum is optional in IPv4 (0 = not computed), and
        // UdpRepr::parse rejects that, so read the fields directly and
        // validate only the length.
        let src_port = pkt.src_port();
        let dst_port = pkt.dst_port();
        let payload = pkt.payload();

        if dst_port == DHCP_SERVER_PORT && src_port == DHCP_CLIENT_PORT {
            self.handle_dhcp(payload);
            return;
        }
        if dst == DNS_IP && dst_port == DNS_PORT {
            self.handle_dns(src, src_port, dst, payload);
        }
        // Everything else is dropped: there is no generic UDP NAT. That
        // includes DNS aimed at a resolver other than 10.0.2.3, which our DHCP
        // never hands out. The guest sees it as a network that filters UDP.
    }

    // ------------------------------------------------------------------ DHCP

    fn handle_dhcp(&mut self, payload: &[u8]) {
        let Ok(pkt) = DhcpPacket::new_checked(payload) else {
            return;
        };
        let Ok(req) = DhcpRepr::parse(&pkt) else {
            return;
        };
        let reply_type = match req.message_type {
            DhcpMessageType::Discover => DhcpMessageType::Offer,
            DhcpMessageType::Request => {
                // Only 10.0.2.15 is ever on offer.
                let asked = req.requested_ip.map(|a| a.octets());
                let claimed = req.client_ip.octets();
                if matches!(asked, Some(a) if a != GUEST_IP)
                    || (asked.is_none() && claimed != [0, 0, 0, 0] && claimed != GUEST_IP)
                {
                    DhcpMessageType::Nak
                } else {
                    DhcpMessageType::Ack
                }
            }
            // Release/Decline/Inform need no reply from a single-lease server.
            _ => return,
        };

        let nak = reply_type == DhcpMessageType::Nak;
        let mut dns_servers = heapless::Vec::new();
        let _ = dns_servers.push(ip(DNS_IP));

        let repr = DhcpRepr {
            message_type: reply_type,
            transaction_id: req.transaction_id,
            secs: 0,
            client_hardware_address: req.client_hardware_address,
            client_ip: Ipv4Address::UNSPECIFIED,
            your_ip: if nak {
                Ipv4Address::UNSPECIFIED
            } else {
                ip(GUEST_IP)
            },
            server_ip: ip(GATEWAY_IP),
            router: if nak { None } else { Some(ip(GATEWAY_IP)) },
            subnet_mask: if nak { None } else { Some(ip(SUBNET_MASK)) },
            relay_agent_ip: Ipv4Address::UNSPECIFIED,
            broadcast: req.broadcast,
            requested_ip: None,
            client_identifier: None,
            server_identifier: Some(ip(GATEWAY_IP)),
            parameter_request_list: None,
            dns_servers: if nak { None } else { Some(dns_servers) },
            max_size: None,
            lease_duration: if nak { None } else { Some(DHCP_LEASE_SECS) },
            renew_duration: None,
            rebind_duration: None,
            additional_options: &[],
        };

        let mut buf = vec![0u8; repr.buffer_len()];
        {
            let mut out = DhcpPacket::new_unchecked(&mut buf[..]);
            if repr.emit(&mut out).is_err() {
                return;
            }
        }
        // Always broadcast the reply (as slirp does): the client has no
        // address yet and its ARP table is empty.
        let packet = udp_datagram(
            ip(GATEWAY_IP),
            DHCP_SERVER_PORT,
            ip(LIMITED_BROADCAST_IP),
            DHCP_CLIENT_PORT,
            &buf,
        );
        self.emit_broadcast_ip_to_guest(packet);
    }

    // ------------------------------------------------------------------- DNS

    fn handle_dns(&mut self, guest_ip: [u8; 4], guest_port: u16, server_ip: [u8; 4], body: &[u8]) {
        let Some(query) = dns::parse_query(body) else {
            return;
        };
        if query.qclass != dns::CLASS_IN {
            let resp = dns::build_error(&query, dns::RCODE_NOTIMPL);
            self.send_dns_reply(guest_ip, guest_port, server_ip, &resp);
            return;
        }
        match query.qtype {
            dns::TYPE_A => {
                let handle = self.sub.dns_resolve(&query.name);
                self.dns_pending.insert(
                    handle,
                    PendingDns {
                        query,
                        guest_ip,
                        guest_port,
                        server_ip,
                    },
                );
            }
            // Empty NOERROR, not NOTIMPL: musl/busybox fall back to A on an
            // empty answer but treat NOTIMPL as a broken resolver.
            dns::TYPE_AAAA => {
                let resp = dns::build_answer(&query, &[]);
                self.send_dns_reply(guest_ip, guest_port, server_ip, &resp);
            }
            _ => {
                let resp = dns::build_error(&query, dns::RCODE_NOTIMPL);
                self.send_dns_reply(guest_ip, guest_port, server_ip, &resp);
            }
        }
    }

    fn send_dns_reply(&mut self, guest_ip: [u8; 4], guest_port: u16, server_ip: [u8; 4], b: &[u8]) {
        let packet = udp_datagram(ip(server_ip), DNS_PORT, ip(guest_ip), guest_port, b);
        self.emit_ip_to_guest(packet);
    }

    // ------------------------------------------------------------------- TCP

    fn handle_tcp(&mut self, src: [u8; 4], dst: [u8; 4], body: &[u8], ip_packet: &[u8]) {
        let Ok(pkt) = TcpPacket::new_checked(body) else {
            return;
        };
        if !in_subnet(src) {
            return;
        }
        let key = TcpKey {
            guest_port: pkt.src_port(),
            dst,
            dst_port: pkt.dst_port(),
        };
        let is_syn = pkt.syn() && !pkt.ack();
        let seq = pkt.seq_number().0 as u32;

        if let Some(flow) = self.tcp.get_mut(&key) {
            if flow.sock.is_none() {
                // Still connecting: refresh the held SYN, drop everything else.
                if is_syn {
                    flow.pending_syn = Some(ip_packet.to_vec());
                    flow.syn_seq = seq;
                }
                return;
            }
            self.device.rx.push_back(ip_packet.to_vec());
            return;
        }

        if !is_syn {
            // Unknown flow: RST so the guest gives up instead of
            // retransmitting for a minute.
            if !pkt.rst() {
                self.send_reset(src, dst, &pkt);
            }
            return;
        }

        let sub = self.sub.tcp_connect(dst, key.dst_port);
        self.tcp.insert(
            key,
            TcpFlow {
                sub,
                sock: None,
                pending_syn: Some(ip_packet.to_vec()),
                syn_seq: seq,
                to_guest: VecDeque::new(),
                sub_eof: false,
                told_sub_closed: false,
                dead: false,
            },
        );
        self.tcp_by_handle.insert(sub, key);
    }

    /// Hand-built RST for a segment with no socket behind it.
    fn send_reset(&mut self, guest_ip: [u8; 4], server_ip: [u8; 4], pkt: &TcpPacket<&[u8]>) {
        // RFC 793 §3.4: if the offending segment carried an ACK, the RST takes
        // its sequence number from that ACK and carries no ACK of its own;
        // otherwise seq is 0 and it ACKs past the offending segment.
        let (seq, ack) = if pkt.ack() {
            (pkt.ack_number(), None)
        } else {
            let consumed = pkt.payload().len() + usize::from(pkt.syn()) + usize::from(pkt.fin());
            (
                TcpSeqNumber(0),
                Some(TcpSeqNumber(
                    pkt.seq_number().0.wrapping_add(consumed as i32),
                )),
            )
        };
        let repr = TcpRepr {
            src_port: pkt.dst_port(),
            dst_port: pkt.src_port(),
            control: TcpControl::Rst,
            seq_number: seq,
            ack_number: ack,
            window_len: 0,
            window_scale: None,
            max_seg_size: None,
            sack_permitted: false,
            sack_ranges: [None; 3],
            timestamp: None,
            payload: &[],
        };
        let packet = tcp_segment(ip(server_ip), ip(guest_ip), &repr);
        self.emit_ip_to_guest(packet);
    }

    fn reset_pending_flow(&mut self, key: TcpKey) {
        let Some(flow) = self.tcp.get(&key) else {
            return;
        };
        let repr = TcpRepr {
            src_port: key.dst_port,
            dst_port: key.guest_port,
            control: TcpControl::Rst,
            seq_number: TcpSeqNumber(0),
            ack_number: Some(TcpSeqNumber(flow.syn_seq.wrapping_add(1) as i32)),
            window_len: 0,
            window_scale: None,
            max_seg_size: None,
            sack_permitted: false,
            sack_ranges: [None; 3],
            timestamp: None,
            payload: &[],
        };
        let packet = tcp_segment(ip(key.dst), ip(GUEST_IP), &repr);
        self.emit_ip_to_guest(packet);
    }

    fn open_socket(&mut self, key: TcpKey) {
        let Some(flow) = self.tcp.get_mut(&key) else {
            return;
        };
        if flow.sock.is_some() {
            return;
        }
        let Some(syn) = flow.pending_syn.take() else {
            return;
        };
        let mut socket = tcp::Socket::new(
            tcp::SocketBuffer::new(vec![0u8; TCP_BUFFER_BYTES]),
            tcp::SocketBuffer::new(vec![0u8; TCP_BUFFER_BYTES]),
        );
        // No keep-alive, no idle timeout: the substrate drives the lifetime.
        socket.set_nagle_enabled(false);
        if socket.listen((ip(key.dst), key.dst_port)).is_err() {
            flow.dead = true;
            return;
        }
        let handle = self.sockets.add(socket);
        if let Some(flow) = self.tcp.get_mut(&key) {
            flow.sock = Some(handle);
        }
        // Replaying the SYN turns the listener into an established connection;
        // `set_any_ip(true)` is what lets the interface accept a packet
        // addressed to a host it does not own.
        self.device.rx.push_back(syn);
    }

    // ------------------------------------------------------------ event pump

    fn drain_substrate(&mut self) {
        let events = self.sub.poll(self.now_ms);
        for event in events {
            match event {
                SubstrateEvent::TcpConnected(h) => {
                    if let Some(key) = self.tcp_by_handle.get(&h).copied() {
                        self.open_socket(key);
                    }
                }
                SubstrateEvent::TcpData(h, data) => {
                    if let Some(key) = self.tcp_by_handle.get(&h).copied() {
                        if let Some(flow) = self.tcp.get_mut(&key) {
                            flow.to_guest.extend(data);
                        }
                    }
                }
                SubstrateEvent::TcpClosed(h) => {
                    if let Some(key) = self.tcp_by_handle.get(&h).copied() {
                        if let Some(flow) = self.tcp.get_mut(&key) {
                            flow.sub_eof = true;
                        }
                    }
                }
                // Every failure becomes a RST; the `FlowError` taxonomy is
                // diagnostic-only.
                SubstrateEvent::TcpError(h, _err) => {
                    if let Some(key) = self.tcp_by_handle.get(&h).copied() {
                        self.abort_tcp(key, false);
                    }
                }
                SubstrateEvent::DnsResolved(h, addrs) => {
                    if let Some(p) = self.dns_pending.remove(&h) {
                        let resp = if addrs.is_empty() {
                            dns::build_error(&p.query, dns::RCODE_NXDOMAIN)
                        } else {
                            dns::build_answer(&p.query, &addrs)
                        };
                        self.send_dns_reply(p.guest_ip, p.guest_port, p.server_ip, &resp);
                    }
                }
                SubstrateEvent::DnsFailed(h, err) => {
                    if let Some(p) = self.dns_pending.remove(&h) {
                        let rcode = match err {
                            DnsError::NxDomain => dns::RCODE_NXDOMAIN,
                            DnsError::ServFail => dns::RCODE_SERVFAIL,
                        };
                        let resp = dns::build_error(&p.query, rcode);
                        self.send_dns_reply(p.guest_ip, p.guest_port, p.server_ip, &resp);
                    }
                }
            }
        }
    }

    /// Tear a TCP flow down hard. `tell_substrate` is false when the substrate
    /// is the one that reported the failure.
    fn abort_tcp(&mut self, key: TcpKey, tell_substrate: bool) {
        let Some(flow) = self.tcp.get_mut(&key) else {
            return;
        };
        flow.dead = true;
        let sub = flow.sub;
        let sock = flow.sock;
        let pending = flow.pending_syn.is_some();
        if tell_substrate {
            self.sub.tcp_abort(sub);
        }
        match sock {
            Some(handle) => {
                self.sockets.get_mut::<tcp::Socket>(handle).abort();
            }
            None if pending => self.reset_pending_flow(key),
            None => {}
        }
    }

    /// Move bytes in both directions and reap finished flows.
    fn pump_tcp(&mut self) {
        let keys: Vec<TcpKey> = self.tcp.keys().copied().collect();
        for key in keys {
            let Some(flow) = self.tcp.get_mut(&key) else {
                continue;
            };
            let Some(handle) = flow.sock else { continue };
            let sub = flow.sub;
            let dead = flow.dead;
            let socket = self.sockets.get_mut::<tcp::Socket>(handle);

            if dead {
                continue;
            }

            // guest -> substrate: consume only the accepted bytes. The rest
            // stays in smoltcp's receive buffer, shrinking the advertised
            // window.
            while socket.can_recv() {
                let taken = socket
                    .recv(|buf| {
                        let n = self.sub.tcp_send(sub, buf);
                        (n, n)
                    })
                    .unwrap_or(0);
                if taken == 0 {
                    break;
                }
            }

            // substrate -> guest.
            let flow = self.tcp.get_mut(&key).expect("flow present");
            if !flow.to_guest.is_empty() && socket.can_send() {
                let (a, b) = flow.to_guest.as_slices();
                let mut sent = 0usize;
                if !a.is_empty() {
                    sent += socket.send_slice(a).unwrap_or(0);
                }
                if sent == a.len() && !b.is_empty() {
                    sent += socket.send_slice(b).unwrap_or(0);
                }
                flow.to_guest.drain(..sent);
            }

            // Guest half-close: tell the substrate once its data is consumed.
            let state = socket.state();
            let guest_finished = matches!(
                state,
                tcp::State::CloseWait
                    | tcp::State::LastAck
                    | tcp::State::Closing
                    | tcp::State::TimeWait
                    | tcp::State::Closed
            );
            if guest_finished && socket.recv_queue() == 0 && !flow.told_sub_closed {
                flow.told_sub_closed = true;
                self.sub.tcp_close(sub);
            }

            // Substrate half-close: FIN once everything it gave us is queued.
            if flow.sub_eof && flow.to_guest.is_empty() && socket.may_send() {
                socket.close();
            }
        }

        // Reap.
        let finished: Vec<TcpKey> = self
            .tcp
            .iter()
            .filter(|(_, f)| match f.sock {
                Some(h) => self.sockets.get::<tcp::Socket>(h).state() == tcp::State::Closed,
                None => f.dead,
            })
            .map(|(k, _)| *k)
            .collect();
        for key in finished {
            if let Some(flow) = self.tcp.remove(&key) {
                if let Some(handle) = flow.sock {
                    self.sockets.remove(handle);
                }
                if !flow.dead && !flow.told_sub_closed {
                    self.sub.tcp_abort(flow.sub);
                }
                self.tcp_by_handle.remove(&flow.sub);
            }
        }
    }

    /// Move smoltcp's emitted packets into the frame queue while it has room.
    /// Leftovers stay in `device.tx`, which makes the device refuse smoltcp
    /// further tx tokens: backpressure instead of dropping frames the stack
    /// believes it delivered.
    fn drain_device_tx(&mut self) {
        while self.to_guest.len() < TO_GUEST_QUEUE_LIMIT {
            let Some(packet) = self.device.tx.pop_front() else {
                break;
            };
            self.emit_ip_to_guest(packet);
        }
    }

    /// One full turn: substrate events in, smoltcp advanced, bytes moved,
    /// frames out.
    fn pump(&mut self) {
        let now = Instant::from_millis(self.now_ms as i64);
        // Move packets parked in the device out first, so smoltcp gets tx
        // tokens back for this poll.
        self.drain_device_tx();
        self.drain_substrate();
        self.iface.poll(now, &mut self.device, &mut self.sockets);
        self.pump_tcp();
        // Anything the pump queued in a socket needs a second egress pass.
        self.iface.poll(now, &mut self.device, &mut self.sockets);
        self.drain_device_tx();

        let smoltcp_deadline = self
            .iface
            .poll_delay(now, &self.sockets)
            .map(|d| self.now_ms.saturating_add(d.total_millis()));
        let sub_deadline = self.sub.next_deadline_ms();
        self.next_deadline = [smoltcp_deadline, sub_deadline].into_iter().flatten().min();
    }
}

impl<S: SocketSubstrate> NetBackend for NatBackend<S> {
    fn transmit(&mut self, frame: &[u8]) {
        if frame.len() > MTU + 14 {
            return;
        }
        self.handle_frame(frame);
        self.pump();
    }

    fn receive(&mut self) -> Option<Vec<u8>> {
        self.to_guest.pop_front()
    }

    fn poll(&mut self, now_ms: u64) {
        self.now_ms = now_ms;
        self.pump();
    }

    fn next_deadline_ms(&self) -> Option<u64> {
        if !self.to_guest.is_empty() {
            return Some(self.now_ms);
        }
        self.next_deadline
    }
}
