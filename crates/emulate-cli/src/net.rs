//! Native user-mode networking through the multiplexed Bun relay.

use std::net::{SocketAddr, TcpStream, ToSocketAddrs};
use std::time::Duration;

use emulate_core::hostnet::RelayClient;

mod transport;
pub use transport::TungsteniteTransport;

const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);

/// `--net` mode selector.
#[derive(Copy, Clone, PartialEq, Eq, Debug, clap::ValueEnum)]
pub enum NetMode {
    /// No NIC backend: the guest's virtio-net link stays down.
    None,
    /// User-mode NAT through an external relay (no privileges, no TAP).
    User,
}

/// Where `--net user` looks for a relay when none is named. Matches the Bun
/// relay's local listener (`relay/server.ts`).
pub const DEFAULT_RELAY_URL: &str = "ws://127.0.0.1:7654";
/// Environment override for [`DEFAULT_RELAY_URL`], read when `--relay` is absent.
pub const RELAY_URL_ENV: &str = "EMULATE_RELAY_URL";

/// What a relay that cannot be reached should tell the user.
fn unreachable(url: &str, error: &str) -> String {
    format!("cannot reach relay at {url}: {error}; start one with 'npm run relay'")
}

/// Fail early if nothing is listening at `url`, so a boot reports a missing
/// relay before the guest silently loses its network.
pub fn check_relay(url: &str) -> Result<(), String> {
    let addr = authority(url).map_err(|error| unreachable(url, &error))?;
    TcpStream::connect_timeout(&addr, HANDSHAKE_TIMEOUT)
        .map(|_| ())
        .map_err(|error| unreachable(url, &error.to_string()))
}

/// A substrate pointed at the relay at `url`, ready for `NatBackend::new`.
pub fn substrate(url: &str) -> RelayClient<TungsteniteTransport> {
    let mut client = RelayClient::new(TungsteniteTransport::default(), url);
    let describe = format!("user-mode NAT (relay {})", client.url());
    client.set_describe(describe);
    client
}

/// The `host:port` a `ws://host:port/path` URL points at.
fn authority(url: &str) -> Result<SocketAddr, String> {
    let rest = url
        .strip_prefix("ws://")
        .ok_or_else(|| format!("unsupported relay scheme: {url}"))?;
    let end = rest.find(['/', '?']).unwrap_or(rest.len());
    rest[..end]
        .to_socket_addrs()
        .map_err(|error| error.to_string())?
        .next()
        .ok_or_else(|| format!("cannot resolve relay address: {url}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use emulate_core::hostnet::{FlowError, SocketSubstrate, SubstrateEvent};
    use std::net::{Ipv4Addr, TcpListener};
    use std::time::Instant;

    #[test]
    fn a_substrate_names_the_relay_it_dials() {
        let client = substrate("ws://127.0.0.1:7654");
        assert_eq!(client.url(), "ws://127.0.0.1:7654");
        assert_eq!(
            client.describe(),
            "user-mode NAT (relay ws://127.0.0.1:7654)"
        );
    }

    #[test]
    fn check_relay_reaches_a_listener_and_says_how_to_start_one_otherwise() {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).expect("bind");
        let url = format!(
            "ws://127.0.0.1:{}",
            listener.local_addr().expect("addr").port()
        );
        check_relay(&url).expect("a bound port is reachable");

        // Bind then drop: the port is free and nothing is listening.
        let port = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
            .expect("bind")
            .local_addr()
            .expect("addr")
            .port();
        let error = check_relay(&format!("ws://127.0.0.1:{port}")).expect_err("nothing listening");
        assert!(error.starts_with(&format!("cannot reach relay at ws://127.0.0.1:{port}: ")));
        assert!(error.ends_with("start one with 'npm run relay'"), "{error}");
    }

    #[test]
    fn an_unreachable_relay_fails_the_flow_rather_than_hanging() {
        // A port with nothing on it: the dial fails and the flow dies.
        let port = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
            .expect("bind")
            .local_addr()
            .expect("addr")
            .port();
        let mut client = substrate(&format!("ws://127.0.0.1:{port}"));
        let flow = client.tcp_connect([1, 1, 1, 1], 80);
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            let events = client.poll(0);
            if !events.is_empty() {
                assert_eq!(
                    events,
                    vec![SubstrateEvent::TcpError(flow, FlowError::Unreachable)]
                );
                break;
            }
            assert!(Instant::now() < deadline, "no terminal event");
            std::thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(client.next_deadline_ms(), None);
    }

    #[test]
    fn authority_reads_the_host_and_port_out_of_a_relay_url() {
        assert_eq!(
            authority("ws://127.0.0.1:7654/").unwrap(),
            "127.0.0.1:7654".parse::<SocketAddr>().unwrap()
        );
        assert_eq!(
            authority("ws://127.0.0.1:7654/api/relay?example=1").unwrap(),
            "127.0.0.1:7654".parse::<SocketAddr>().unwrap()
        );
        assert!(authority("wss://relay.example/").is_err());
        assert!(authority("127.0.0.1:7654").is_err());
    }
}
