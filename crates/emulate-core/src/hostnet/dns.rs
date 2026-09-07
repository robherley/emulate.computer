//! Minimal RFC 1035 query parsing and A-record response synthesis.
//!
//! Hand-rolled because smoltcp's `wire::dns` is a client (emits queries, parses
//! responses) and this is the mirror image.

/// Query types we care about.
pub const TYPE_A: u16 = 1;
pub const TYPE_AAAA: u16 = 28;
pub const CLASS_IN: u16 = 1;

const RCODE_NOERROR: u8 = 0;
pub const RCODE_SERVFAIL: u8 = 2;
pub const RCODE_NXDOMAIN: u8 = 3;
pub const RCODE_NOTIMPL: u8 = 4;

/// A parsed single-question DNS query.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Query {
    pub id: u16,
    /// Recursion Desired bit, echoed back into the response.
    pub recursion_desired: bool,
    /// Lowercased, no trailing dot.
    pub name: String,
    pub qtype: u16,
    pub qclass: u16,
    /// The raw question section, byte for byte, so the response can echo it.
    pub question_raw: Vec<u8>,
}

/// Parse a DNS query. Returns `None` for anything that is not a well-formed
/// standard query with exactly one question (responses, opcodes we do not
/// implement, truncated messages, or names using compression pointers).
pub fn parse_query(buf: &[u8]) -> Option<Query> {
    if buf.len() < 12 {
        return None;
    }
    let id = u16::from_be_bytes([buf[0], buf[1]]);
    let flags = u16::from_be_bytes([buf[2], buf[3]]);
    if flags & 0x8000 != 0 {
        return None; // response, not a query
    }
    if (flags >> 11) & 0xf != 0 {
        return None; // non-standard opcode
    }
    let qdcount = u16::from_be_bytes([buf[4], buf[5]]);
    if qdcount != 1 {
        return None;
    }

    let start = 12;
    let mut i = start;
    let mut name = String::new();
    loop {
        let len = *buf.get(i)? as usize;
        if len & 0xc0 != 0 {
            return None; // compression pointer in a question: reject
        }
        i += 1;
        if len == 0 {
            break;
        }
        let label = buf.get(i..i + len)?;
        if !name.is_empty() {
            name.push('.');
        }
        for &b in label {
            name.push((b as char).to_ascii_lowercase());
        }
        i += len;
        if name.len() > 255 {
            return None;
        }
    }
    let qtype = u16::from_be_bytes([*buf.get(i)?, *buf.get(i + 1)?]);
    let qclass = u16::from_be_bytes([*buf.get(i + 2)?, *buf.get(i + 3)?]);
    i += 4;

    Some(Query {
        id,
        recursion_desired: flags & 0x0100 != 0,
        name,
        qtype,
        qclass,
        question_raw: buf[start..i].to_vec(),
    })
}

fn header(query: &Query, rcode: u8, ancount: u16) -> [u8; 12] {
    // QR=1, AA=0, TC=0, RD=echo, RA=1.
    let mut flags: u16 = 0x8000 | 0x0080 | (rcode as u16 & 0xf);
    if query.recursion_desired {
        flags |= 0x0100;
    }
    let mut h = [0u8; 12];
    h[0..2].copy_from_slice(&query.id.to_be_bytes());
    h[2..4].copy_from_slice(&flags.to_be_bytes());
    h[4..6].copy_from_slice(&1u16.to_be_bytes()); // QDCOUNT, question echoed
    h[6..8].copy_from_slice(&ancount.to_be_bytes());
    h
}

/// Build a NOERROR response carrying one A record per address.
///
/// An empty `addrs` yields an empty NOERROR ("the name exists but has no
/// records of this type"), which is also the AAAA answer, so guests fall
/// straight back to A.
pub fn build_answer(query: &Query, addrs: &[[u8; 4]]) -> Vec<u8> {
    let mut out = Vec::with_capacity(12 + query.question_raw.len() + addrs.len() * 16);
    out.extend_from_slice(&header(query, RCODE_NOERROR, addrs.len() as u16));
    out.extend_from_slice(&query.question_raw);
    for addr in addrs {
        out.extend_from_slice(&[0xc0, 0x0c]); // NAME: pointer to the question
        out.extend_from_slice(&TYPE_A.to_be_bytes());
        out.extend_from_slice(&CLASS_IN.to_be_bytes());
        out.extend_from_slice(&crate::hostnet::consts::DNS_TTL.to_be_bytes());
        out.extend_from_slice(&4u16.to_be_bytes()); // RDLENGTH
        out.extend_from_slice(addr);
    }
    out
}

/// Build an answer-less response with the given RCODE.
pub fn build_error(query: &Query, rcode: u8) -> Vec<u8> {
    let mut out = Vec::with_capacity(12 + query.question_raw.len());
    out.extend_from_slice(&header(query, rcode, 0));
    out.extend_from_slice(&query.question_raw);
    out
}

/// Serialize a name into wire label form.
fn encode_name(name: &str) -> Vec<u8> {
    let mut out = Vec::with_capacity(name.len() + 2);
    for label in name.split('.').filter(|l| !l.is_empty()) {
        out.push(label.len() as u8);
        out.extend_from_slice(label.as_bytes());
    }
    out.push(0);
    out
}

/// Build a query message.
pub fn build_query(id: u16, name: &str, qtype: u16) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(&id.to_be_bytes());
    out.extend_from_slice(&0x0100u16.to_be_bytes()); // standard query, RD
    out.extend_from_slice(&1u16.to_be_bytes());
    out.extend_from_slice(&[0, 0, 0, 0, 0, 0]);
    out.extend_from_slice(&encode_name(name));
    out.extend_from_slice(&qtype.to_be_bytes());
    out.extend_from_slice(&CLASS_IN.to_be_bytes());
    out
}
