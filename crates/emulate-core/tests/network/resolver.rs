//! DNS at 10.0.2.3:53.

use super::common;

use common::*;
use emulate_core::hostnet::consts::{DNS_IP, GATEWAY_MAC, GUEST_IP};
use emulate_core::hostnet::dns;
use emulate_core::hostnet::substrate::{DnsError, SubstrateEvent};

fn query_frame(id: u16, name: &str, qtype: u16) -> Vec<u8> {
    guest_udp_frame(
        GATEWAY_MAC,
        GUEST_IP,
        45_000,
        DNS_IP,
        53,
        &dns::build_query(id, name, qtype),
    )
}

/// Minimal response reader: header fields plus the answer records.
struct Response {
    id: u16,
    flags: u16,
    qdcount: u16,
    ancount: u16,
    question: Vec<u8>,
    answers: Vec<(u16, u16, u32, Vec<u8>)>,
}

fn read_response(bytes: &[u8]) -> Response {
    let id = u16::from_be_bytes([bytes[0], bytes[1]]);
    let flags = u16::from_be_bytes([bytes[2], bytes[3]]);
    let qdcount = u16::from_be_bytes([bytes[4], bytes[5]]);
    let ancount = u16::from_be_bytes([bytes[6], bytes[7]]);
    let mut i = 12;
    while bytes[i] != 0 {
        i += 1 + bytes[i] as usize;
    }
    i += 1 + 4; // root label + qtype/qclass
    let question = bytes[12..i].to_vec();
    let mut answers = Vec::new();
    for _ in 0..ancount {
        assert_eq!(
            &bytes[i..i + 2],
            &[0xc0, 0x0c],
            "answer NAME is a pointer back to the question"
        );
        i += 2;
        let rtype = u16::from_be_bytes([bytes[i], bytes[i + 1]]);
        let rclass = u16::from_be_bytes([bytes[i + 2], bytes[i + 3]]);
        let ttl = u32::from_be_bytes([bytes[i + 4], bytes[i + 5], bytes[i + 6], bytes[i + 7]]);
        let rdlen = u16::from_be_bytes([bytes[i + 8], bytes[i + 9]]) as usize;
        i += 10;
        answers.push((rtype, rclass, ttl, bytes[i..i + rdlen].to_vec()));
        i += rdlen;
    }
    Response {
        id,
        flags,
        qdcount,
        ancount,
        question,
        answers,
    }
}

#[test]
fn an_a_query_is_handed_to_the_substrate_and_answered() {
    let mut h = Harness::new();
    let query = dns::build_query(0xabcd, "example.com", dns::TYPE_A);
    h.send(guest_udp_frame(
        GATEWAY_MAC,
        GUEST_IP,
        45_000,
        DNS_IP,
        53,
        &query,
    ));

    // The NAT layer asked the substrate, and answered nothing yet.
    let (flow, name) = h.sub().nth_dns_resolve(0).expect("dns_resolve was called");
    assert_eq!(name, "example.com", "name is normalised and unqualified");
    assert!(
        h.drain().is_empty(),
        "no answer before the substrate replies"
    );

    h.sub().inject(SubstrateEvent::DnsResolved(
        flow,
        vec![[93, 184, 216, 34], [93, 184, 216, 35]],
    ));
    let frames = h.pump();
    assert_eq!(frames.len(), 1);

    let udp = parse_udp(&frames[0]).expect("a UDP reply");
    assert_eq!(udp.ip.src, DNS_IP, "reply comes from 10.0.2.3");
    assert_eq!(udp.ip.dst, GUEST_IP);
    assert_eq!(udp.src_port, 53);
    assert_eq!(udp.dst_port, 45_000, "reply goes back to the guest's port");

    let r = read_response(&udp.payload);
    assert_eq!(r.id, 0xabcd, "transaction id echoed");
    assert_eq!(r.flags & 0x8000, 0x8000, "QR set");
    assert_eq!(r.flags & 0x000f, 0, "NOERROR");
    assert_eq!(r.flags & 0x0080, 0x0080, "RA set");
    assert_eq!(r.flags & 0x0100, 0x0100, "RD echoed");
    assert_eq!(r.qdcount, 1);
    assert_eq!(r.ancount, 2, "one A record per address");
    assert_eq!(r.question, query[12..], "question section echoed verbatim");
    assert_eq!(r.answers[0].0, dns::TYPE_A);
    assert_eq!(r.answers[0].1, dns::CLASS_IN);
    assert_eq!(r.answers[0].2, 60, "TTL 60");
    assert_eq!(r.answers[0].3, vec![93, 184, 216, 34]);
    assert_eq!(r.answers[1].3, vec![93, 184, 216, 35], "order preserved");
}

#[test]
fn a_failed_lookup_becomes_servfail() {
    let mut h = Harness::new();
    h.send(query_frame(1, "nope.invalid", dns::TYPE_A));
    let (flow, _) = h.sub().nth_dns_resolve(0).unwrap();
    h.sub()
        .inject(SubstrateEvent::DnsFailed(flow, DnsError::ServFail));
    let frames = h.pump();
    assert_eq!(frames.len(), 1);
    let r = read_response(&parse_udp(&frames[0]).unwrap().payload);
    assert_eq!(r.flags & 0x000f, dns::RCODE_SERVFAIL as u16);
    assert_eq!(r.ancount, 0);
    assert_eq!(r.qdcount, 1, "question is still echoed on an error");
}

#[test]
fn nxdomain_is_passed_through() {
    let mut h = Harness::new();
    h.send(query_frame(2, "gone.example", dns::TYPE_A));
    let (flow, _) = h.sub().nth_dns_resolve(0).unwrap();
    h.sub()
        .inject(SubstrateEvent::DnsFailed(flow, DnsError::NxDomain));
    let frames = h.pump();
    let r = read_response(&parse_udp(&frames[0]).unwrap().payload);
    assert_eq!(r.flags & 0x000f, dns::RCODE_NXDOMAIN as u16);
}

#[test]
fn an_empty_resolution_is_nxdomain() {
    let mut h = Harness::new();
    h.send(query_frame(3, "empty.example", dns::TYPE_A));
    let (flow, _) = h.sub().nth_dns_resolve(0).unwrap();
    h.sub().inject(SubstrateEvent::DnsResolved(flow, vec![]));
    let frames = h.pump();
    let r = read_response(&parse_udp(&frames[0]).unwrap().payload);
    assert_eq!(r.flags & 0x000f, dns::RCODE_NXDOMAIN as u16);
}

#[test]
fn aaaa_is_answered_immediately_with_an_empty_noerror() {
    let mut h = Harness::new();
    h.send(query_frame(9, "example.com", dns::TYPE_AAAA));
    // Never reaches the substrate.
    assert!(h.sub().nth_dns_resolve(0).is_none());
    let frames = h.drain();
    assert_eq!(frames.len(), 1, "answered synchronously");
    let r = read_response(&parse_udp(&frames[0]).unwrap().payload);
    assert_eq!(r.flags & 0x000f, 0, "NOERROR, so musl falls back to A");
    assert_eq!(r.ancount, 0);
    assert_eq!(r.id, 9);
}

#[test]
fn an_unsupported_qtype_is_notimpl() {
    let mut h = Harness::new();
    h.send(query_frame(4, "example.com", 33)); // SRV
    let frames = h.drain();
    let r = read_response(&parse_udp(&frames[0]).unwrap().payload);
    assert_eq!(r.flags & 0x000f, dns::RCODE_NOTIMPL as u16);
}

#[test]
fn names_are_lowercased_before_they_reach_the_substrate() {
    let mut h = Harness::new();
    h.send(query_frame(5, "ExAmPlE.CoM", dns::TYPE_A));
    let (_, name) = h.sub().nth_dns_resolve(0).unwrap();
    assert_eq!(name, "example.com");
}

#[test]
fn malformed_dns_is_dropped_not_answered() {
    let mut h = Harness::new();
    h.send(guest_udp_frame(
        GATEWAY_MAC,
        GUEST_IP,
        45_000,
        DNS_IP,
        53,
        &[0u8; 4],
    ));
    assert!(h.drain().is_empty());
    assert!(h.sub().calls.is_empty());
}

#[test]
fn dns_to_an_external_resolver_gets_no_answer() {
    // 10.0.2.3 is the only resolver DHCP hands out. A query aimed anywhere
    // else is plain outbound UDP, which the NAT drops.
    let mut h = Harness::new();
    let query = dns::build_query(6, "example.com", dns::TYPE_A);
    h.send(guest_udp_frame(
        GATEWAY_MAC,
        GUEST_IP,
        45_001,
        [8, 8, 8, 8],
        53,
        &query,
    ));
    assert!(h.sub().nth_dns_resolve(0).is_none(), "not our resolver");
    assert!(h.sub().calls.is_empty(), "nothing reached the substrate");
    assert!(h.drain().is_empty(), "and nothing came back");
}
