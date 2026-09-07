//! What the NAT layer is allowed to do to the outside world: the
//! [`SocketSubstrate`] trait and its event taxonomy.
//!
//! No `std::net` types cross this boundary (a browser substrate has no
//! `SocketAddr`), nothing here blocks, and the substrate owns handle
//! allocation. IPv4 only; IPv6 is out of scope per docs/networking.md.

/// An opaque per-flow identifier minted by the substrate.
///
/// Unique across TCP and DNS flows; never reused while the flow is live.
#[derive(Copy, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub struct FlowHandle(pub u64);

/// Why a TCP flow failed, in terms the NAT layer can answer without knowing
/// the host's error taxonomy.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum FlowError {
    /// The peer actively refused (RST / ECONNREFUSED). -> RST to the guest.
    ConnectionRefused,
    /// No route, DNS-less literal, blocked destination.
    Unreachable,
    /// Connect or transfer timed out.
    TimedOut,
    /// Established connection was reset mid-flight.
    Reset,
}

/// Why a name lookup failed.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum DnsError {
    /// Authoritative "no such name" -> NXDOMAIN.
    NxDomain,
    /// Resolver broke -> SERVFAIL.
    ServFail,
}

/// Something that happened out in the world, reported at poll time.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum SubstrateEvent {
    /// The TCP connection is up. The NAT layer holds the guest's SYN until
    /// this arrives, so a SYN-ACK never promises a connection that failed.
    TcpConnected(FlowHandle),
    /// Bytes from the peer, in order.
    TcpData(FlowHandle, Vec<u8>),
    /// The peer closed cleanly (EOF). -> FIN to the guest.
    TcpClosed(FlowHandle),
    /// The flow failed. -> RST to the guest. The handle is dead afterwards;
    /// the NAT layer will not call `tcp_close` on it.
    TcpError(FlowHandle, FlowError),
    /// Name resolved to one or more A records (order preserved).
    DnsResolved(FlowHandle, Vec<[u8; 4]>),
    /// Name lookup failed.
    DnsFailed(FlowHandle, DnsError),
}

/// What the NAT layer may do to the outside world: TCP and name resolution
/// only (docs/networking.md).
///
/// Implemented by [`RelayClient`](crate::hostnet::RelayClient) on both hosts,
/// and by `FakeSubstrate` in tests.
pub trait SocketSubstrate {
    /// Begin connecting to `dst:port`. Returns immediately with a handle; the
    /// outcome arrives later as [`SubstrateEvent::TcpConnected`] or
    /// [`SubstrateEvent::TcpError`]. Never blocks.
    fn tcp_connect(&mut self, dst: [u8; 4], port: u16) -> FlowHandle;

    /// Queue `data` for the peer. Returns how many **leading** bytes were
    /// accepted; a short accept is backpressure, and the NAT layer keeps the
    /// remainder in its receive buffer, shrinking the guest's window. Bytes
    /// are never reordered or dropped.
    fn tcp_send(&mut self, flow: FlowHandle, data: &[u8]) -> usize;

    /// Half-close: the guest sent FIN. Previously accepted bytes must still be
    /// delivered. The flow stays live until the substrate reports
    /// `TcpClosed` / `TcpError`.
    fn tcp_close(&mut self, flow: FlowHandle);

    /// Hard teardown (guest sent RST, or the NAT layer is dropping the flow).
    /// Discard buffered bytes; the handle is dead on return and the substrate
    /// must emit no further events for it. Defaults to `tcp_close`.
    fn tcp_abort(&mut self, flow: FlowHandle) {
        self.tcp_close(flow);
    }

    /// Resolve `name` to A records. Result arrives as
    /// [`SubstrateEvent::DnsResolved`] / [`SubstrateEvent::DnsFailed`].
    /// `name` is already normalised: lowercase, no trailing dot.
    fn dns_resolve(&mut self, name: &str) -> FlowHandle;

    /// Drain everything that has happened since the last call. `now_ms` is the
    /// same monotonic clock the machine loop feeds
    /// [`NetBackend::poll`](crate::devices::net::NetBackend::poll), so a
    /// substrate can run its own timeouts without a clock of its own.
    fn poll(&mut self, now_ms: u64) -> Vec<SubstrateEvent>;

    /// Earliest millisecond at which `poll` should run again. Folded into
    /// `NetBackend::next_deadline_ms` so WFI sleeps shorten only while the
    /// substrate has pending work.
    fn next_deadline_ms(&self) -> Option<u64> {
        None
    }

    /// Human-readable description for the status line, e.g. "relay ws://…".
    fn describe(&self) -> String {
        String::from("substrate")
    }
}
