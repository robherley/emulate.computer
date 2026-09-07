//! TCP NAT: handshake, data both ways, teardown, RST — plus the proof that
//! non-DHCP/DNS UDP is dropped rather than forwarded.

use super::common;

use common::*;
use emulate_core::devices::net::NetBackend;
use emulate_core::hostnet::consts::{GATEWAY_MAC, GUEST_IP};
use emulate_core::hostnet::substrate::{FlowError, FlowHandle, SubstrateEvent};
use emulate_core::hostnet::testing::Call;
use smoltcp::wire::{TcpControl, TcpRepr, TcpSeqNumber};

const PEER: [u8; 4] = [93, 184, 216, 34];
const PEER_PORT: u16 = 80;
const GUEST_PORT: u16 = 40_000;

fn seg<'a>(control: TcpControl, seq: i32, ack: Option<i32>, payload: &'a [u8]) -> TcpRepr<'a> {
    TcpRepr {
        src_port: GUEST_PORT,
        dst_port: PEER_PORT,
        control,
        seq_number: TcpSeqNumber(seq),
        ack_number: ack.map(TcpSeqNumber),
        window_len: 32_768,
        window_scale: None,
        max_seg_size: if control == TcpControl::Syn {
            Some(1460)
        } else {
            None
        },
        sack_permitted: false,
        sack_ranges: [None; 3],
        timestamp: None,
        payload,
    }
}

/// Drives a flow up to ESTABLISHED. Returns (substrate handle, server ISN).
fn establish(h: &mut Harness) -> (FlowHandle, i32) {
    h.send(tcp_frame(PEER, &seg(TcpControl::Syn, 1000, None, &[])));
    let (flow, dst, port) = h
        .sub()
        .nth_tcp_connect(0)
        .expect("the SYN triggered tcp_connect");
    assert_eq!(dst, PEER);
    assert_eq!(port, PEER_PORT);
    assert!(
        h.drain().is_empty(),
        "no SYN-ACK before the substrate confirms the connection"
    );

    h.sub().inject(SubstrateEvent::TcpConnected(flow));
    let frames = h.pump();
    assert_eq!(frames.len(), 1, "one SYN-ACK");
    let synack = parse_tcp(&frames[0]).expect("a TCP segment");
    assert!(synack.syn && synack.ack_flag, "SYN-ACK");
    assert_eq!(synack.ack, Some(1001), "acks the guest's ISN + 1");
    assert_eq!(synack.src_port, PEER_PORT, "source is the real destination");
    assert_eq!(synack.dst_port, GUEST_PORT);
    assert_eq!(synack.ip.src, PEER, "impersonates the peer address");
    assert_eq!(synack.ip.dst, GUEST_IP);
    assert_eq!(synack.ip.eth_src, GATEWAY_MAC);

    let isn = synack.seq;
    // Guest completes the handshake.
    h.send(tcp_frame(
        PEER,
        &seg(TcpControl::None, 1001, Some(isn + 1), &[]),
    ));
    h.drain();
    (flow, isn)
}

#[test]
fn a_syn_becomes_a_substrate_connect_and_the_handshake_completes() {
    let mut h = Harness::new();
    let (flow, _) = establish(&mut h);
    assert!(h.sub().saw(&Call::TcpConnect {
        flow,
        dst: PEER,
        port: PEER_PORT
    }));
}

#[test]
fn guest_data_reaches_the_substrate_and_is_acked() {
    let mut h = Harness::new();
    let (flow, isn) = establish(&mut h);

    let body = b"GET / HTTP/1.0\r\nHost: example.com\r\n\r\n";
    h.send(tcp_frame(
        PEER,
        &seg(TcpControl::None, 1001, Some(isn + 1), body),
    ));
    assert_eq!(
        h.sub().tcp_sent(flow),
        body.to_vec(),
        "bytes forwarded verbatim"
    );

    // smoltcp uses a 10 ms delayed ACK; the machine loop learns about it via
    // next_deadline_ms and polls again.
    assert!(
        h.nat.next_deadline_ms().is_some(),
        "a delayed ACK is pending"
    );
    h.tick(20);
    let frames = h.drain();
    let ack = frames
        .iter()
        .filter_map(|f| parse_tcp(f))
        .find(|s| s.ack_flag)
        .expect("an ACK for the request");
    assert_eq!(
        ack.ack,
        Some(1001 + body.len() as i32),
        "acks every byte the guest sent"
    );
}

#[test]
fn substrate_data_reaches_the_guest_with_a_correct_sequence() {
    let mut h = Harness::new();
    let (flow, isn) = establish(&mut h);

    h.sub().inject(SubstrateEvent::TcpData(
        flow,
        b"HTTP/1.0 200 OK\r\n".to_vec(),
    ));
    let frames = h.pump();
    let first = frames
        .iter()
        .filter_map(|f| parse_tcp(f))
        .find(|s| !s.payload.is_empty())
        .expect("a data segment");
    assert_eq!(first.seq, isn + 1, "first data byte follows the SYN");
    assert_eq!(first.payload, b"HTTP/1.0 200 OK\r\n");
    assert_eq!(first.ip.src, PEER);
    assert_eq!(first.src_port, PEER_PORT);

    // Guest acks it; the next chunk continues the sequence.
    let next_seq = first.seq + first.payload.len() as i32;
    h.send(tcp_frame(
        PEER,
        &seg(TcpControl::None, 1001, Some(next_seq), &[]),
    ));
    h.drain();
    h.sub()
        .inject(SubstrateEvent::TcpData(flow, b"\r\nbody".to_vec()));
    let frames = h.pump();
    let second = frames
        .iter()
        .filter_map(|f| parse_tcp(f))
        .find(|s| !s.payload.is_empty())
        .expect("a second data segment");
    assert_eq!(second.seq, next_seq, "sequence progresses without gaps");
    assert_eq!(second.payload, b"\r\nbody");
}

#[test]
fn tcp_send_backpressure_leaves_the_rest_in_the_receive_buffer() {
    let mut h = Harness::new();
    let (flow, isn) = establish(&mut h);
    h.sub().tcp_send_limit = Some(4);

    let body = b"0123456789";
    h.send(tcp_frame(
        PEER,
        &seg(TcpControl::None, 1001, Some(isn + 1), body),
    ));
    // Four bytes per call, so the whole payload still gets through, but only
    // in accepted-size chunks and never reordered.
    assert_eq!(h.sub().tcp_sent(flow), body.to_vec());
    let chunks: Vec<Vec<u8>> = h
        .sub()
        .calls
        .iter()
        .filter_map(|c| match c {
            Call::TcpSend { data, .. } => Some(data.clone()),
            _ => None,
        })
        .collect();
    assert!(chunks.len() >= 3, "split across several accepted chunks");
    assert!(
        chunks.iter().all(|c| c.len() <= 4),
        "never more than the accepted count"
    );
}

#[test]
fn a_substrate_close_sends_a_fin_to_the_guest() {
    let mut h = Harness::new();
    let (flow, isn) = establish(&mut h);
    h.sub()
        .inject(SubstrateEvent::TcpData(flow, b"bye".to_vec()));
    h.sub().inject(SubstrateEvent::TcpClosed(flow));
    let frames = h.pump();
    let segs: Vec<_> = frames.iter().filter_map(|f| parse_tcp(f)).collect();
    assert!(
        segs.iter().any(|s| s.payload == b"bye"),
        "buffered data is flushed first"
    );
    assert!(segs.iter().any(|s| s.fin), "then FIN");
    assert!(!segs.iter().any(|s| s.rst), "a clean close is not a reset");
    let _ = isn;
}

#[test]
fn a_guest_fin_closes_the_substrate_flow_and_the_socket_is_reaped() {
    let mut h = Harness::new();
    let (flow, isn) = establish(&mut h);
    h.send(tcp_frame(
        PEER,
        &seg(TcpControl::Fin, 1001, Some(isn + 1), &[]),
    ));
    h.pump();
    assert!(
        h.sub().saw(&Call::TcpClose(flow)),
        "the guest's FIN is a half-close on the substrate"
    );

    // Substrate closes back; the NAT layer finishes the teardown.
    h.sub().inject(SubstrateEvent::TcpClosed(flow));
    let frames = h.pump();
    assert!(
        frames.iter().filter_map(|f| parse_tcp(f)).any(|s| s.fin),
        "FIN back to the guest"
    );
    // Guest acks our FIN; flow drains out of the table.
    let our_fin = frames
        .iter()
        .filter_map(|f| parse_tcp(f))
        .find(|s| s.fin)
        .unwrap();
    h.send(tcp_frame(
        PEER,
        &seg(TcpControl::None, 1002, Some(our_fin.seq + 1), &[]),
    ));
    for _ in 0..5 {
        h.tick(1000);
    }
    // A fresh SYN on the same 5-tuple starts a brand new substrate flow,
    // which is only possible if the old one was reaped.
    h.send(tcp_frame(PEER, &seg(TcpControl::Syn, 5000, None, &[])));
    assert!(
        h.sub().nth_tcp_connect(1).is_some(),
        "the tuple was released"
    );
}

#[test]
fn a_connect_error_before_the_handshake_produces_a_rst() {
    let mut h = Harness::new();
    h.send(tcp_frame(PEER, &seg(TcpControl::Syn, 1000, None, &[])));
    let (flow, _, _) = h.sub().nth_tcp_connect(0).unwrap();
    h.sub()
        .inject(SubstrateEvent::TcpError(flow, FlowError::ConnectionRefused));
    let frames = h.pump();
    assert_eq!(frames.len(), 1);
    let rst = parse_tcp(&frames[0]).expect("a TCP segment");
    assert!(rst.rst, "connection refused becomes a RST");
    assert!(!rst.syn);
    assert_eq!(rst.ack, Some(1001), "acks the SYN we never accepted");
    assert_eq!(rst.src_port, PEER_PORT);
    assert_eq!(rst.ip.src, PEER);
}

#[test]
fn an_error_on_an_established_flow_resets_the_guest() {
    let mut h = Harness::new();
    let (flow, _) = establish(&mut h);
    h.sub()
        .inject(SubstrateEvent::TcpError(flow, FlowError::Reset));
    let frames = h.pump();
    assert!(
        frames.iter().filter_map(|f| parse_tcp(f)).any(|s| s.rst),
        "an established flow that fails is reset, not FINed"
    );
}

#[test]
fn a_stray_segment_for_an_unknown_flow_is_reset() {
    let mut h = Harness::new();
    h.send(tcp_frame(
        PEER,
        &seg(TcpControl::None, 500, Some(900), b"stray"),
    ));
    let frames = h.drain();
    assert_eq!(frames.len(), 1);
    let rst = parse_tcp(&frames[0]).unwrap();
    assert!(rst.rst);
    assert_eq!(rst.seq, 900, "RFC 793: seq comes from the offending ACK");
    assert!(h.sub().calls.is_empty(), "no substrate flow was created");
}

#[test]
fn a_stray_rst_is_not_answered_with_another_rst() {
    let mut h = Harness::new();
    let mut repr = seg(TcpControl::Rst, 500, None, &[]);
    repr.window_len = 0;
    h.send(tcp_frame(PEER, &repr));
    assert!(h.drain().is_empty());
}

#[test]
fn two_guest_ports_to_the_same_peer_are_separate_flows() {
    let mut h = Harness::new();
    h.send(tcp_frame(PEER, &seg(TcpControl::Syn, 1000, None, &[])));
    let mut other = seg(TcpControl::Syn, 2000, None, &[]);
    other.src_port = GUEST_PORT + 1;
    h.send(tcp_frame(PEER, &other));

    let a = h.sub().nth_tcp_connect(0).unwrap();
    let b = h.sub().nth_tcp_connect(1).unwrap();
    assert_ne!(a.0, b.0, "distinct substrate flows");
}

// --------------------------------------------------------------------- UDP
//
// No generic UDP NAT: DHCP and the resolver are answered in-process (see
// `link_layer.rs` / `resolver.rs`), everything else is dropped.

#[test]
fn outbound_udp_is_dropped_at_the_nat() {
    let mut h = Harness::new();
    for (dst, port, payload) in [
        // An NTP query to a public server.
        (
            [162, 159, 200, 123].as_slice(),
            123u16,
            b"ntp-request".as_slice(),
        ),
        // Broadcast/link-local chatter.
        (&[10, 0, 2, 255], 137, b"netbios"),
    ] {
        let dst: [u8; 4] = dst.try_into().unwrap();
        h.send(guest_udp_frame(
            GATEWAY_MAC,
            GUEST_IP,
            50_000,
            dst,
            port,
            payload,
        ));
        assert!(h.sub().calls.is_empty(), "no substrate call for {dst:?}");
        assert!(h.drain().is_empty(), "no reply for {dst:?}");
    }
}

#[test]
fn the_backend_reports_a_deadline_only_when_there_is_work() {
    let mut h = Harness::new();
    h.tick(1);
    assert_eq!(h.nat.next_deadline_ms(), None, "idle link, no deadline");

    let (flow, _) = establish(&mut h);
    h.tick(1);
    assert_eq!(
        h.nat.next_deadline_ms(),
        None,
        "an established but quiet flow still needs no timer"
    );

    // Unacknowledged data arms smoltcp's retransmit timer.
    h.sub()
        .inject(SubstrateEvent::TcpData(flow, b"payload".to_vec()));
    h.tick(1);
    h.drain();
    let deadline = h.nat.next_deadline_ms().expect("a retransmit deadline");
    assert!(deadline >= h.now_ms, "deadline is in the future");
}

/// A substrate can hand the NAT megabytes in one poll (a WebSocket delivers
/// whole bursts at once). Every byte must reach the guest in order with zero
/// dropped frames: backpressure has to reach into smoltcp's socket buffers
/// rather than shed emitted segments, which stalls on RTO and truncates.
#[test]
fn a_multi_hundred_kilobyte_burst_reaches_the_guest_without_loss() {
    let mut h = Harness::new();
    let (flow, isn) = establish(&mut h);

    let payload: Vec<u8> = (0..512 * 1024u32)
        .map(|i| (i.wrapping_mul(31) % 251) as u8)
        .collect();
    h.sub()
        .inject(SubstrateEvent::TcpData(flow, payload.clone()));

    let expected_start = isn + 1;
    let mut received: Vec<u8> = Vec::new();
    for _ in 0..20_000 {
        if received.len() == payload.len() {
            break;
        }
        h.tick(1);
        // Mimic the machine loop: a bounded pull per tick, not a full drain.
        let mut frames = Vec::new();
        for _ in 0..64 {
            let Some(f) = h.nat.receive() else { break };
            frames.push(f);
        }
        for f in &frames {
            let Some(s) = parse_tcp(f) else { continue };
            if s.payload.is_empty() {
                continue;
            }
            let offset = (s.seq - expected_start) as usize;
            assert!(
                offset <= received.len(),
                "gap in the delivered stream: segment at offset {offset}, contiguous up to {}",
                received.len()
            );
            let end = offset + s.payload.len();
            if end > received.len() {
                received.resize(end, 0);
            }
            received[offset..end].copy_from_slice(&s.payload);
        }
        // Cumulative ACK so smoltcp keeps the window moving.
        let ack = expected_start + received.len() as i32;
        h.send(tcp_frame(
            PEER,
            &seg(TcpControl::None, 1001, Some(ack), &[]),
        ));
    }

    assert_eq!(received.len(), payload.len(), "every byte was delivered");
    assert_eq!(received, payload, "bytes arrived intact and in order");
    assert_eq!(
        h.nat.dropped_frames(),
        0,
        "no smoltcp-emitted frame was dropped on the way to the guest"
    );
}
