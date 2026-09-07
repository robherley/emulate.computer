//! `FakeSubstrate`: a recording [`SocketSubstrate`] that does no I/O, so the
//! whole NAT layer can be driven from plain `cargo test`.

use std::collections::VecDeque;

use crate::hostnet::substrate::{FlowHandle, SocketSubstrate, SubstrateEvent};

/// Every call the NAT layer can make on a substrate, in order.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Call {
    TcpConnect {
        flow: FlowHandle,
        dst: [u8; 4],
        port: u16,
    },
    TcpSend {
        flow: FlowHandle,
        data: Vec<u8>,
    },
    TcpClose(FlowHandle),
    TcpAbort(FlowHandle),
    DnsResolve {
        flow: FlowHandle,
        name: String,
    },
}

/// Records every call and replays injected events.
pub struct FakeSubstrate {
    next_handle: u64,
    /// Everything the NAT layer asked for, oldest first.
    pub calls: Vec<Call>,
    /// Events to hand back on the next `poll`.
    pending: VecDeque<SubstrateEvent>,
    /// When set, `tcp_send` accepts at most this many bytes per call.
    pub tcp_send_limit: Option<usize>,
    /// Millisecond of the most recent `poll`.
    pub last_poll_ms: u64,
}

impl Default for FakeSubstrate {
    fn default() -> Self {
        Self::new()
    }
}

impl FakeSubstrate {
    pub fn new() -> Self {
        Self {
            next_handle: 1,
            calls: Vec::new(),
            pending: VecDeque::new(),
            tcp_send_limit: None,
            last_poll_ms: 0,
        }
    }

    fn alloc(&mut self) -> FlowHandle {
        let h = FlowHandle(self.next_handle);
        self.next_handle += 1;
        h
    }

    /// Queue an event for the next poll.
    pub fn inject(&mut self, event: SubstrateEvent) {
        self.pending.push_back(event);
    }

    /// Handle of the nth `tcp_connect`, if it happened.
    pub fn nth_tcp_connect(&self, n: usize) -> Option<(FlowHandle, [u8; 4], u16)> {
        self.calls
            .iter()
            .filter_map(|c| match c {
                Call::TcpConnect { flow, dst, port } => Some((*flow, *dst, *port)),
                _ => None,
            })
            .nth(n)
    }

    /// Handle of the nth `dns_resolve`, if it happened.
    pub fn nth_dns_resolve(&self, n: usize) -> Option<(FlowHandle, String)> {
        self.calls
            .iter()
            .filter_map(|c| match c {
                Call::DnsResolve { flow, name } => Some((*flow, name.clone())),
                _ => None,
            })
            .nth(n)
    }

    /// Concatenation of everything sent on `flow` via `tcp_send`.
    pub fn tcp_sent(&self, flow: FlowHandle) -> Vec<u8> {
        let mut out = Vec::new();
        for call in &self.calls {
            if let Call::TcpSend { flow: f, data } = call {
                if *f == flow {
                    out.extend_from_slice(data);
                }
            }
        }
        out
    }

    pub fn saw(&self, call: &Call) -> bool {
        self.calls.contains(call)
    }
}

impl SocketSubstrate for FakeSubstrate {
    fn tcp_connect(&mut self, dst: [u8; 4], port: u16) -> FlowHandle {
        let flow = self.alloc();
        self.calls.push(Call::TcpConnect { flow, dst, port });
        flow
    }

    fn tcp_send(&mut self, flow: FlowHandle, data: &[u8]) -> usize {
        let n = self
            .tcp_send_limit
            .map_or(data.len(), |l| l.min(data.len()));
        if n > 0 {
            self.calls.push(Call::TcpSend {
                flow,
                data: data[..n].to_vec(),
            });
        }
        n
    }

    fn tcp_close(&mut self, flow: FlowHandle) {
        self.calls.push(Call::TcpClose(flow));
    }

    fn tcp_abort(&mut self, flow: FlowHandle) {
        self.calls.push(Call::TcpAbort(flow));
    }

    fn dns_resolve(&mut self, name: &str) -> FlowHandle {
        let flow = self.alloc();
        self.calls.push(Call::DnsResolve {
            flow,
            name: name.to_string(),
        });
        flow
    }

    fn poll(&mut self, now_ms: u64) -> Vec<SubstrateEvent> {
        self.last_poll_ms = now_ms;
        self.pending.drain(..).collect()
    }

    fn describe(&self) -> String {
        String::from("fake substrate (tests only)")
    }
}
