//! [`RelayClient`]: the relay protocol as a [`SocketSubstrate`], shared by
//! every host. The wire format is documented in `relay/README.md`.
//!
//! Handshake, half-close, the `ERR` taxonomy, RESOLVE, backpressure and the
//! state machine guaranteeing exactly one terminal event per handle live here;
//! a host supplies only a [`RelayTransport`].
//!
//! Nothing here does I/O or blocks: transport events surface only from
//! [`SocketSubstrate::poll`]. While a flow is live the client asks to be polled
//! again within [`ACTIVE_POLL_INTERVAL_MS`], so a host sleeping on WFI still
//! moves bytes.

use std::collections::{HashMap, VecDeque};

use crate::hostnet::substrate::{DnsError, FlowError, FlowHandle, SocketSubstrate, SubstrateEvent};

/// Ceiling on a connection's buffered-but-unsent bytes. Past it `tcp_send` is
/// short and the NAT layer keeps the remainder, shrinking the guest's window
/// instead of growing an unbounded queue in the transport.
pub const SEND_CAP_BYTES: u64 = 256 * 1024;

/// While any flow is live, poll again this soon: a transport callback cannot
/// wake a host that is sleeping on WFI, so the deadline has to.
pub const ACTIVE_POLL_INTERVAL_MS: u64 = 5;

/// A transport-minted connection id. One connection carries exactly one flow.
#[derive(Copy, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub struct ConnId(pub u64);

/// What a transport reports about one of its connections.
///
/// Modelled on the `WebSocket` event set. `Closed` and `Error` both mean
/// "gone" to the client; distinguishing them only keeps logs honest.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum TransportEvent {
    /// The connection is established; the client sends its command now.
    Opened(ConnId),
    /// A text frame.
    Text(ConnId, String),
    /// A binary frame.
    Binary(ConnId, Vec<u8>),
    /// The connection closed, cleanly or not.
    Closed(ConnId),
    /// The connection failed. Equivalent to `Closed` for protocol purposes.
    Error(ConnId),
}

impl TransportEvent {
    /// The connection every event belongs to.
    pub fn conn(&self) -> ConnId {
        match self {
            Self::Opened(id)
            | Self::Text(id, _)
            | Self::Binary(id, _)
            | Self::Closed(id)
            | Self::Error(id) => *id,
        }
    }
}

/// One logical channel per flow, carried over a shared WebSocket. Every method must be
/// non-blocking, with results surfacing from [`Self::poll`].
pub trait RelayTransport {
    /// Start connecting to `url`. `None` means the attempt failed outright
    /// (malformed URL, no fd). Otherwise the connection is pending until the
    /// transport reports [`TransportEvent::Opened`].
    fn open(&mut self, url: &str) -> Option<ConnId>;

    /// Queue a text frame. `false` means the connection is unusable; the client
    /// treats that as the connection having gone away.
    fn send_text(&mut self, conn: ConnId, text: &str) -> bool;

    /// Queue a binary frame. `false` is fatal for the connection, as above.
    /// Only ever called with a length the transport just said it had room for.
    fn send_binary(&mut self, conn: ConnId, data: &[u8]) -> bool;

    /// Outstanding upload bytes, including credit and shared-buffer pressure.
    fn buffered_amount(&self, conn: ConnId) -> u64;

    /// Tear the connection down. The transport must emit no further events for
    /// `conn` except, optionally, a final `Closed`.
    fn close(&mut self, conn: ConnId);

    /// Everything that has happened since the last call, in order. Never
    /// blocks. `now_ms` is for transports running their own timeouts.
    fn poll(&mut self, now_ms: u64) -> Vec<TransportEvent>;
}

#[derive(Copy, Clone, PartialEq, Eq, Debug)]
enum FlowKind {
    Tcp,
    Dns,
}

#[derive(Copy, Clone, PartialEq, Eq, Debug)]
enum FlowState {
    /// Connection opening, or command sent and no `OK` yet.
    Connecting,
    /// The relay said `OK`; bytes flow.
    Open,
    /// A terminal event has been emitted; emit nothing more, reap on next poll.
    Dead,
}

struct Flow {
    conn: ConnId,
    kind: FlowKind,
    state: FlowState,
    /// The first frame, sent once the transport reports `Opened`.
    command: String,
}

/// Map a relay `ERR <reason>` onto the NAT layer's error taxonomy.
fn connect_error(reason: &str) -> FlowError {
    match reason {
        "ECONNREFUSED" => FlowError::ConnectionRefused,
        "ECONNRESET" => FlowError::Reset,
        "ETIMEDOUT" => FlowError::TimedOut,
        _ => FlowError::Unreachable,
    }
}

/// Strict dotted-quad parser for the relay's `IP a.b.c.d …` answer: no octal,
/// no shorthand, no trailing junk.
pub fn parse_ipv4(text: &str) -> Option<[u8; 4]> {
    let mut octets = [0u8; 4];
    let mut parts = text.trim().split('.');
    for octet in &mut octets {
        let part = parts.next()?;
        if part.is_empty() || part.len() > 3 || !part.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        *octet = part.parse().ok()?;
    }
    if parts.next().is_some() {
        return None;
    }
    Some(octets)
}

/// The relay protocol as a [`SocketSubstrate`], over any [`RelayTransport`].
pub struct RelayClient<T: RelayTransport> {
    transport: T,
    url: String,
    describe: String,
    next_handle: u64,
    flows: HashMap<u64, Flow>,
    /// Reverse index, so a transport event finds its flow in one step.
    by_conn: HashMap<ConnId, u64>,
    events: VecDeque<SubstrateEvent>,
    last_poll_ms: u64,
}

impl<T: RelayTransport> RelayClient<T> {
    /// `url` is the full relay endpoint, e.g. `ws://127.0.0.1:7654/`.
    pub fn new(transport: T, url: &str) -> Self {
        let url = url.trim().to_owned();
        Self {
            transport,
            describe: format!("relay {url}"),
            url,
            next_handle: 1,
            flows: HashMap::new(),
            by_conn: HashMap::new(),
            events: VecDeque::new(),
            last_poll_ms: 0,
        }
    }

    /// The normalised relay URL this client dials.
    pub fn url(&self) -> &str {
        &self.url
    }

    /// Override the status-line text.
    pub fn set_describe(&mut self, describe: String) {
        self.describe = describe;
    }

    fn alloc(&mut self) -> FlowHandle {
        let handle = FlowHandle(self.next_handle);
        self.next_handle += 1;
        handle
    }

    /// Open one connection and arrange for `command` to be its first frame.
    fn open(&mut self, kind: FlowKind, command: String) -> FlowHandle {
        let handle = self.alloc();
        let Some(conn) = self.transport.open(&self.url.clone()) else {
            // Never reached the relay: same terminal event as a socket that
            // died before answering.
            self.events.push_back(match kind {
                FlowKind::Tcp => SubstrateEvent::TcpError(handle, FlowError::Unreachable),
                FlowKind::Dns => SubstrateEvent::DnsFailed(handle, DnsError::ServFail),
            });
            return handle;
        };
        self.by_conn.insert(conn, handle.0);
        self.flows.insert(
            handle.0,
            Flow {
                conn,
                kind,
                state: FlowState::Connecting,
                command,
            },
        );
        handle
    }

    /// Digest one transport event. Split out of `poll` so the borrow of
    /// `self.flows` never overlaps the push into `self.events`.
    fn on_transport(&mut self, event: TransportEvent) {
        let conn = event.conn();
        let Some(&handle) = self.by_conn.get(&conn) else {
            return;
        };
        match event {
            TransportEvent::Opened(_) => {
                let Some(flow) = self.flows.get(&handle) else {
                    return;
                };
                if flow.state != FlowState::Connecting {
                    return;
                }
                let command = flow.command.clone();
                if !self.transport.send_text(conn, &command) {
                    self.gone(handle);
                }
            }
            TransportEvent::Text(_, text) => {
                self.on_text(handle, text.trim_end_matches(['\r', '\n']));
            }
            TransportEvent::Binary(_, data) => {
                let Some(flow) = self.flows.get(&handle) else {
                    return;
                };
                if flow.state == FlowState::Open && flow.kind == FlowKind::Tcp && !data.is_empty() {
                    self.events
                        .push_back(SubstrateEvent::TcpData(FlowHandle(handle), data));
                }
            }
            TransportEvent::Closed(_) | TransportEvent::Error(_) => self.gone(handle),
        }
    }

    /// A text frame from the relay.
    fn on_text(&mut self, handle: u64, text: &str) {
        let Some(flow) = self.flows.get_mut(&handle) else {
            return;
        };
        let id = FlowHandle(handle);
        match (flow.state, flow.kind) {
            (FlowState::Dead, _) => {}
            (FlowState::Connecting, FlowKind::Tcp) => {
                if text == "OK" {
                    flow.state = FlowState::Open;
                    self.events.push_back(SubstrateEvent::TcpConnected(id));
                } else {
                    let reason = text.strip_prefix("ERR ").unwrap_or(text);
                    flow.state = FlowState::Dead;
                    self.events
                        .push_back(SubstrateEvent::TcpError(id, connect_error(reason)));
                }
            }
            (FlowState::Connecting, FlowKind::Dns) => {
                flow.state = FlowState::Dead;
                if let Some(list) = text.strip_prefix("IP ") {
                    let addrs: Vec<[u8; 4]> =
                        list.split_whitespace().filter_map(parse_ipv4).collect();
                    self.events.push_back(if addrs.is_empty() {
                        SubstrateEvent::DnsFailed(id, DnsError::ServFail)
                    } else {
                        SubstrateEvent::DnsResolved(id, addrs)
                    });
                } else {
                    // `ERR nx` is "no such name"; anything else is the
                    // resolver itself breaking.
                    let error = if text.contains("nx") {
                        DnsError::NxDomain
                    } else {
                        DnsError::ServFail
                    };
                    self.events.push_back(SubstrateEvent::DnsFailed(id, error));
                }
            }
            (FlowState::Open, FlowKind::Tcp) => {
                if text == "EOF" {
                    flow.state = FlowState::Dead;
                    self.events.push_back(SubstrateEvent::TcpClosed(id));
                }
            }
            // Reserved by the protocol: post-handshake text, and anything on
            // a DNS flow that already answered.
            (FlowState::Open, FlowKind::Dns) => {}
        }
    }

    /// The connection went away, whatever the reason.
    fn gone(&mut self, handle: u64) {
        let Some(flow) = self.flows.get_mut(&handle) else {
            return;
        };
        let id = FlowHandle(handle);
        match (flow.state, flow.kind) {
            (FlowState::Dead, _) => {}
            (FlowState::Connecting, FlowKind::Tcp) => {
                flow.state = FlowState::Dead;
                // Never reached the relay, or it hung up before answering.
                self.events
                    .push_back(SubstrateEvent::TcpError(id, FlowError::Unreachable));
            }
            (FlowState::Connecting, FlowKind::Dns) => {
                flow.state = FlowState::Dead;
                self.events
                    .push_back(SubstrateEvent::DnsFailed(id, DnsError::ServFail));
            }
            (FlowState::Open, _) => {
                flow.state = FlowState::Dead;
                // A clean end arrives as TEXT "EOF" above, so reaching here on
                // an established flow means it was cut short.
                self.events
                    .push_back(SubstrateEvent::TcpError(id, FlowError::Reset));
            }
        }
    }

    /// Drop a flow and its connection. Only ever called from `poll`, never
    /// from inside a transport callback, so a transport may own closures that
    /// would otherwise be dropped while running.
    fn reap(&mut self, handle: u64) {
        if let Some(flow) = self.flows.remove(&handle) {
            self.by_conn.remove(&flow.conn);
            self.transport.close(flow.conn);
        }
    }
}

fn event_flow(event: &SubstrateEvent) -> FlowHandle {
    match event {
        SubstrateEvent::TcpConnected(f)
        | SubstrateEvent::TcpData(f, _)
        | SubstrateEvent::TcpClosed(f)
        | SubstrateEvent::TcpError(f, _)
        | SubstrateEvent::DnsResolved(f, _)
        | SubstrateEvent::DnsFailed(f, _) => *f,
    }
}

impl<T: RelayTransport> SocketSubstrate for RelayClient<T> {
    fn tcp_connect(&mut self, dst: [u8; 4], port: u16) -> FlowHandle {
        let [a, b, c, d] = dst;
        // The relay owns the address policy (denylist, and the 10.0.2.2
        // host-loopback convention), so send the guest's literal.
        self.open(FlowKind::Tcp, format!("CONNECT {a}.{b}.{c}.{d}:{port}"))
    }

    fn tcp_send(&mut self, flow: FlowHandle, data: &[u8]) -> usize {
        let Some(entry) = self.flows.get(&flow.0) else {
            return 0;
        };
        if entry.state != FlowState::Open || data.is_empty() {
            return 0;
        }
        let conn = entry.conn;
        // Accept only what keeps the transport under the cap; the remainder
        // stays in the NAT layer's receive buffer.
        let room = SEND_CAP_BYTES.saturating_sub(self.transport.buffered_amount(conn)) as usize;
        let accepted = data.len().min(room);
        if accepted == 0 {
            // No notification needed: the NAT layer retries on every pump.
            return 0;
        }
        if !self.transport.send_binary(conn, &data[..accepted]) {
            self.gone(flow.0);
            return 0;
        }
        accepted
    }

    fn tcp_close(&mut self, flow: FlowHandle) {
        let Some(entry) = self.flows.get(&flow.0) else {
            return;
        };
        let conn = entry.conn;
        match entry.state {
            // Half-close: the relay shuts its write side and keeps streaming
            // the response, ending with TEXT "EOF".
            FlowState::Open => {
                if !self.transport.send_text(conn, "FIN") {
                    self.gone(flow.0);
                }
            }
            // Nothing to half-close yet: end the flow here rather than wait
            // for the transport. A transport need not report a final `Closed`
            // for a connection it was asked to close (the browser's
            // `WsTransport` does not), so waiting would leak the flow.
            FlowState::Connecting => {
                self.transport.close(conn);
                self.gone(flow.0);
            }
            FlowState::Dead => {}
        }
    }

    fn tcp_abort(&mut self, flow: FlowHandle) {
        // Dead first, so nothing is emitted for an aborted handle even if the
        // transport reports something on its way out.
        if let Some(entry) = self.flows.get_mut(&flow.0) {
            entry.state = FlowState::Dead;
        }
        self.reap(flow.0);
        // Anything already queued is discarded: the handle is dead on return.
        self.events.retain(|event| event_flow(event) != flow);
    }

    fn dns_resolve(&mut self, name: &str) -> FlowHandle {
        self.open(FlowKind::Dns, format!("RESOLVE {name}"))
    }

    fn poll(&mut self, now_ms: u64) -> Vec<SubstrateEvent> {
        self.last_poll_ms = now_ms;

        for event in self.transport.poll(now_ms) {
            self.on_transport(event);
        }

        let drained: Vec<SubstrateEvent> = self.events.drain(..).collect();

        let dead: Vec<u64> = self
            .flows
            .iter()
            .filter(|(_, flow)| flow.state == FlowState::Dead)
            .map(|(&handle, _)| handle)
            .collect();
        for handle in dead {
            self.reap(handle);
        }

        drained
    }

    fn next_deadline_ms(&self) -> Option<u64> {
        if !self.events.is_empty() {
            return Some(self.last_poll_ms);
        }
        (!self.flows.is_empty()).then(|| self.last_poll_ms + ACTIVE_POLL_INTERVAL_MS)
    }

    fn describe(&self) -> String {
        self.describe.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A [`RelayTransport`] with no I/O: the test drives both halves by hand.
    #[derive(Default)]
    struct FakeTransport {
        next: u64,
        /// Everything the client sent, in order: `(conn, frame)`.
        sent: Vec<(ConnId, Frame)>,
        /// Connections the client asked to close.
        closed: Vec<ConnId>,
        /// Events the next `poll` will hand back.
        inbox: VecDeque<TransportEvent>,
        /// Per-connection buffered-byte answer, for backpressure tests.
        buffered: HashMap<ConnId, u64>,
        /// Connections whose sends fail (a broken socket).
        broken: Vec<ConnId>,
        /// When set, `open` fails outright.
        refuse_open: bool,
        opens: Vec<String>,
    }

    #[derive(Clone, PartialEq, Eq, Debug)]
    enum Frame {
        Text(String),
        Binary(Vec<u8>),
    }

    impl FakeTransport {
        fn deliver(&mut self, event: TransportEvent) {
            self.inbox.push_back(event);
        }
        /// The text frames sent on `conn`, in order.
        fn texts(&self, conn: ConnId) -> Vec<String> {
            self.sent
                .iter()
                .filter_map(|(c, f)| match f {
                    Frame::Text(t) if *c == conn => Some(t.clone()),
                    _ => None,
                })
                .collect()
        }
        fn binaries(&self, conn: ConnId) -> Vec<Vec<u8>> {
            self.sent
                .iter()
                .filter_map(|(c, f)| match f {
                    Frame::Binary(b) if *c == conn => Some(b.clone()),
                    _ => None,
                })
                .collect()
        }
    }

    impl RelayTransport for FakeTransport {
        fn open(&mut self, url: &str) -> Option<ConnId> {
            self.opens.push(url.to_string());
            if self.refuse_open {
                return None;
            }
            self.next += 1;
            Some(ConnId(self.next))
        }
        fn send_text(&mut self, conn: ConnId, text: &str) -> bool {
            if self.broken.contains(&conn) {
                return false;
            }
            self.sent.push((conn, Frame::Text(text.to_string())));
            true
        }
        fn send_binary(&mut self, conn: ConnId, data: &[u8]) -> bool {
            if self.broken.contains(&conn) {
                return false;
            }
            self.sent.push((conn, Frame::Binary(data.to_vec())));
            *self.buffered.entry(conn).or_default() += data.len() as u64;
            true
        }
        fn buffered_amount(&self, conn: ConnId) -> u64 {
            self.buffered.get(&conn).copied().unwrap_or(0)
        }
        fn close(&mut self, conn: ConnId) {
            self.closed.push(conn);
        }
        fn poll(&mut self, _now_ms: u64) -> Vec<TransportEvent> {
            self.inbox.drain(..).collect()
        }
    }

    const C1: ConnId = ConnId(1);

    fn client() -> RelayClient<FakeTransport> {
        RelayClient::new(FakeTransport::default(), "ws://127.0.0.1:7654")
    }

    /// Open a TCP flow and drive it all the way to `Open`.
    fn connected(client: &mut RelayClient<FakeTransport>) -> FlowHandle {
        let flow = client.tcp_connect([93, 184, 216, 34], 80);
        client.transport.deliver(TransportEvent::Opened(C1));
        assert!(client.poll(0).is_empty(), "no events until OK");
        client
            .transport
            .deliver(TransportEvent::Text(C1, "OK".into()));
        assert_eq!(client.poll(1), vec![SubstrateEvent::TcpConnected(flow)]);
        flow
    }

    #[test]
    fn connect_sends_the_command_on_open_and_reports_ok() {
        let mut client = client();
        let flow = connected(&mut client);
        assert_eq!(client.transport.opens, vec!["ws://127.0.0.1:7654"]);
        assert_eq!(
            client.transport.texts(C1),
            vec!["CONNECT 93.184.216.34:80".to_string()]
        );
        assert_eq!(flow, FlowHandle(1));
    }

    #[test]
    fn data_flows_both_ways_and_eof_closes_cleanly() {
        let mut client = client();
        let flow = connected(&mut client);

        assert_eq!(client.tcp_send(flow, b"GET / HTTP/1.0\r\n\r\n"), 18);
        assert_eq!(
            client.transport.binaries(C1),
            vec![b"GET / HTTP/1.0\r\n\r\n".to_vec()]
        );

        client
            .transport
            .deliver(TransportEvent::Binary(C1, b"HTTP/1.0 200".to_vec()));
        assert_eq!(
            client.poll(2),
            vec![SubstrateEvent::TcpData(flow, b"HTTP/1.0 200".to_vec())]
        );

        client
            .transport
            .deliver(TransportEvent::Text(C1, "EOF".into()));
        assert_eq!(client.poll(3), vec![SubstrateEvent::TcpClosed(flow)]);
        // Reaped: the terminal event went out, so the connection is released.
        assert_eq!(client.transport.closed, vec![C1]);
        assert_eq!(client.next_deadline_ms(), None);
    }

    #[test]
    fn empty_binary_frames_and_reserved_text_are_ignored() {
        let mut client = client();
        let flow = connected(&mut client);
        client.transport.deliver(TransportEvent::Binary(C1, vec![]));
        client
            .transport
            .deliver(TransportEvent::Text(C1, "SOMETHING ELSE".into()));
        assert!(client.poll(2).is_empty());
        assert_eq!(client.tcp_send(flow, b""), 0);
    }

    #[test]
    fn tcp_close_half_closes_with_fin_and_keeps_reading() {
        let mut client = client();
        let flow = connected(&mut client);
        client.tcp_close(flow);
        assert_eq!(
            client.transport.texts(C1),
            vec!["CONNECT 93.184.216.34:80".to_string(), "FIN".to_string()]
        );
        // The response still arrives after the half-close.
        client
            .transport
            .deliver(TransportEvent::Binary(C1, b"late".to_vec()));
        assert_eq!(
            client.poll(2),
            vec![SubstrateEvent::TcpData(flow, b"late".to_vec())]
        );
        assert!(client.transport.closed.is_empty(), "not reaped before EOF");
    }

    #[test]
    fn close_before_ok_drops_the_connection_and_fails_the_flow() {
        let mut client = client();
        let flow = client.tcp_connect([1, 1, 1, 1], 80);
        client.tcp_close(flow);
        assert_eq!(client.transport.closed, vec![C1]);
        // The transport's close still reports the connection gone.
        client.transport.deliver(TransportEvent::Closed(C1));
        assert_eq!(
            client.poll(1),
            vec![SubstrateEvent::TcpError(flow, FlowError::Unreachable)]
        );
    }

    #[test]
    fn err_reasons_map_onto_the_flow_error_taxonomy() {
        for (reason, want) in [
            ("ERR ECONNREFUSED", FlowError::ConnectionRefused),
            ("ERR ECONNRESET", FlowError::Reset),
            ("ERR ETIMEDOUT", FlowError::TimedOut),
            ("ERR ENETUNREACH", FlowError::Unreachable),
            ("ERR EHOSTUNREACH", FlowError::Unreachable),
            ("ERR EUNKNOWN", FlowError::Unreachable),
            ("ERR destination not allowed", FlowError::Unreachable),
            ("ERR nx", FlowError::Unreachable),
        ] {
            let mut client = client();
            let flow = client.tcp_connect([1, 1, 1, 1], 80);
            client.transport.deliver(TransportEvent::Opened(C1));
            client
                .transport
                .deliver(TransportEvent::Text(C1, reason.into()));
            assert_eq!(
                client.poll(1),
                vec![SubstrateEvent::TcpError(flow, want)],
                "{reason}"
            );
        }
    }

    #[test]
    fn gone_before_ok_is_unreachable_and_gone_after_is_reset() {
        let mut before = client();
        let flow = before.tcp_connect([1, 1, 1, 1], 80);
        before.transport.deliver(TransportEvent::Error(C1));
        // A real WebSocket follows `error` with `close`; only the first may
        // produce a terminal event.
        before.transport.deliver(TransportEvent::Closed(C1));
        assert_eq!(
            before.poll(1),
            vec![SubstrateEvent::TcpError(flow, FlowError::Unreachable)]
        );

        let mut after = client();
        let flow = connected(&mut after);
        after.transport.deliver(TransportEvent::Closed(C1));
        assert_eq!(
            after.poll(2),
            vec![SubstrateEvent::TcpError(flow, FlowError::Reset)]
        );
    }

    #[test]
    fn a_transport_that_cannot_open_fails_the_flow_immediately() {
        let mut client = client();
        client.transport.refuse_open = true;
        let tcp = client.tcp_connect([1, 1, 1, 1], 80);
        let dns = client.dns_resolve("example.com");
        // An event is queued with no live flow, so poll is due right away.
        assert_eq!(client.next_deadline_ms(), Some(0));
        assert_eq!(
            client.poll(7),
            vec![
                SubstrateEvent::TcpError(tcp, FlowError::Unreachable),
                SubstrateEvent::DnsFailed(dns, DnsError::ServFail),
            ]
        );
        assert_eq!(client.next_deadline_ms(), None);
    }

    #[test]
    fn a_broken_send_kills_the_flow() {
        let mut client = client();
        let flow = connected(&mut client);
        client.transport.broken.push(C1);
        assert_eq!(client.tcp_send(flow, b"x"), 0);
        assert_eq!(
            client.poll(2),
            vec![SubstrateEvent::TcpError(flow, FlowError::Reset)]
        );
    }

    #[test]
    fn send_is_capped_by_the_transports_buffered_amount() {
        let mut client = client();
        let flow = connected(&mut client);

        // Room for all of it.
        assert_eq!(client.tcp_send(flow, &[0u8; 1024]), 1024);
        // Nearly full: only the remainder fits, and it is the *leading* bytes.
        client.transport.buffered.insert(C1, SEND_CAP_BYTES - 10);
        let data: Vec<u8> = (0..64u8).collect();
        assert_eq!(client.tcp_send(flow, &data), 10);
        assert_eq!(client.transport.binaries(C1).last().unwrap(), &data[..10]);

        // Completely full: a short send of zero, and no event about it.
        client.transport.buffered.insert(C1, SEND_CAP_BYTES);
        assert_eq!(client.tcp_send(flow, &data), 0);
        assert!(client.poll(3).is_empty());
    }

    #[test]
    fn send_on_an_unknown_or_unopened_flow_accepts_nothing() {
        let mut client = client();
        assert_eq!(client.tcp_send(FlowHandle(99), b"x"), 0);
        let flow = client.tcp_connect([1, 1, 1, 1], 80);
        // Still Connecting: the relay has not said OK.
        assert_eq!(client.tcp_send(flow, b"x"), 0);
    }

    #[test]
    fn closing_a_still_connecting_flow_ends_it_without_a_transport_close_event() {
        let mut client = client();
        let flow = client.tcp_connect([1, 1, 1, 1], 80);
        client.transport.deliver(TransportEvent::Opened(C1));
        assert!(client.poll(0).is_empty(), "no events until OK");
        assert_eq!(client.next_deadline_ms(), Some(ACTIVE_POLL_INTERVAL_MS));

        // The guest gives up before OK. The transport is told to close but,
        // like the browser's `WsTransport`, never reports a final `Closed`.
        client.tcp_close(flow);
        assert!(client.transport.closed.contains(&C1));
        assert_eq!(
            client.poll(1),
            vec![SubstrateEvent::TcpError(flow, FlowError::Unreachable)]
        );
        assert!(client.flows.is_empty(), "flow was not reaped");
        assert_eq!(client.next_deadline_ms(), None);
    }

    #[test]
    fn abort_kills_the_handle_and_discards_queued_events() {
        let mut client = client();
        let flow = connected(&mut client);
        client
            .transport
            .deliver(TransportEvent::Binary(C1, b"pending".to_vec()));
        // Digest the event into the queue without draining it.
        for event in client.transport.poll(0) {
            client.on_transport(event);
        }
        client.tcp_abort(flow);
        assert!(client.transport.closed.contains(&C1));

        // Nothing more is ever reported for the handle, including late frames.
        client
            .transport
            .deliver(TransportEvent::Binary(C1, b"later".to_vec()));
        client.transport.deliver(TransportEvent::Closed(C1));
        assert!(client.poll(4).is_empty());
        assert_eq!(client.next_deadline_ms(), None);
    }

    #[test]
    fn dns_resolves_and_preserves_the_resolvers_order() {
        let mut client = client();
        let flow = client.dns_resolve("example.com");
        client.transport.deliver(TransportEvent::Opened(C1));
        assert!(client.poll(0).is_empty());
        assert_eq!(client.transport.texts(C1), vec!["RESOLVE example.com"]);

        client.transport.deliver(TransportEvent::Text(
            C1,
            "IP 93.184.216.34 1.2.3.4\n".into(),
        ));
        assert_eq!(
            client.poll(1),
            vec![SubstrateEvent::DnsResolved(
                flow,
                vec![[93, 184, 216, 34], [1, 2, 3, 4]]
            )]
        );
        assert_eq!(client.transport.closed, vec![C1]);
    }

    #[test]
    fn dns_failures_split_nxdomain_from_servfail() {
        for (reply, want) in [
            ("ERR nx", DnsError::NxDomain),
            ("ERR something broke", DnsError::ServFail),
            // An `IP` line with nothing parseable is a broken resolver.
            ("IP not-an-address", DnsError::ServFail),
        ] {
            let mut client = client();
            let flow = client.dns_resolve("example.com");
            client.transport.deliver(TransportEvent::Opened(C1));
            client
                .transport
                .deliver(TransportEvent::Text(C1, reply.into()));
            assert_eq!(
                client.poll(1),
                vec![SubstrateEvent::DnsFailed(flow, want)],
                "{reply}"
            );
        }
    }

    #[test]
    fn a_dns_connection_that_dies_before_answering_is_servfail() {
        let mut client = client();
        let flow = client.dns_resolve("example.com");
        client.transport.deliver(TransportEvent::Closed(C1));
        assert_eq!(
            client.poll(1),
            vec![SubstrateEvent::DnsFailed(flow, DnsError::ServFail)]
        );
    }

    #[test]
    fn handles_are_unique_across_tcp_and_dns() {
        let mut client = client();
        let mut handles = vec![
            client.tcp_connect([1, 1, 1, 1], 80),
            client.dns_resolve("a.example"),
            client.tcp_connect([1, 1, 1, 1], 443),
            client.dns_resolve("b.example"),
        ];
        handles.sort();
        handles.dedup();
        assert_eq!(handles.len(), 4);
    }

    #[test]
    fn a_live_flow_asks_for_a_short_deadline_and_an_idle_one_asks_for_none() {
        let mut client = client();
        assert_eq!(client.next_deadline_ms(), None);
        let flow = connected(&mut client);
        let _ = client.poll(1_000);
        assert_eq!(
            client.next_deadline_ms(),
            Some(1_000 + ACTIVE_POLL_INTERVAL_MS)
        );
        client.tcp_abort(flow);
        assert_eq!(client.next_deadline_ms(), None);
    }

    #[test]
    fn describe_names_the_relay_and_can_be_overridden() {
        let mut client = client();
        assert_eq!(client.describe(), "relay ws://127.0.0.1:7654");
        assert_eq!(client.url(), "ws://127.0.0.1:7654");
        client.set_describe("user-mode NAT (in-process relay)".into());
        assert_eq!(client.describe(), "user-mode NAT (in-process relay)");
    }

    #[test]
    fn preserves_the_configured_endpoint() {
        for url in [
            "ws://127.0.0.1:7654",
            "ws://127.0.0.1:7654/",
            "wss://relay.example/api/relay?example=1",
        ] {
            let client = RelayClient::new(FakeTransport::default(), &format!(" {url} "));
            assert_eq!(client.url(), url);
        }
    }

    #[test]
    fn parses_dotted_quads_strictly() {
        assert_eq!(parse_ipv4("10.0.2.15"), Some([10, 0, 2, 15]));
        assert_eq!(parse_ipv4(" 1.2.3.4 "), Some([1, 2, 3, 4]));
        assert_eq!(parse_ipv4("1.2.3"), None);
        assert_eq!(parse_ipv4("1.2.3.4.5"), None);
        assert_eq!(parse_ipv4("1.2.3.256"), None);
        assert_eq!(parse_ipv4("1.2.3.a"), None);
        assert_eq!(parse_ipv4(""), None);
    }
}
