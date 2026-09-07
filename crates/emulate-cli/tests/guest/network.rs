//! Guest networking end to end: DHCP, ICMP, TCP and DNS through the CLI's
//! user-mode NAT and a Bun relay (`relay/server.ts`) spawned for the test.

use crate::support as common;

use common::{BunRelay, Guest, Pattern, CURSOR_QUERY, MOTD, SHELL_PROMPT};
use std::io::{Read, Write};
use std::net::TcpListener;

const MARKER: &str = "EMULATE_NET_MARKER_OK";
const GUEST_IP: &str = "10.0.2.15";
const GATEWAY_IP: &str = "10.0.2.2";
const DNS_IP: &str = "10.0.2.3";

/// A localhost HTTP server that answers every request with the marker. The
/// guest reaches it at 10.0.2.2, which the relay maps to 127.0.0.1.
fn marker_server() -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind marker server");
    let port = listener.local_addr().expect("marker server port").port();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            // Read and discard the request headers; every path answers.
            let mut request = Vec::new();
            let mut byte = [0u8; 1];
            while !request.ends_with(b"\r\n\r\n") && !request.ends_with(b"\n\n") {
                match stream.read(&mut byte) {
                    Ok(1) => request.push(byte[0]),
                    _ => break,
                }
            }
            let body = format!("{MARKER}\n");
            let _ = write!(
                stream,
                "HTTP/1.0 200 OK\r\nContent-Type: text/plain\r\nContent-Length: {}\r\n\r\n{body}",
                body.len()
            );
            let _ = stream.flush();
        }
    });
    port
}

#[test]
#[ignore = "requires prepared guest artifacts; run just test-guest"]
fn guest_takes_a_dhcp_lease_and_reaches_the_host_through_the_gateway() {
    common::require_guest();

    let port = marker_server();
    let url = format!("http://{GATEWAY_IP}:{port}/marker");
    println!("[linux-net] host marker server on 127.0.0.1:{port}");

    // The relay maps the gateway alias 10.0.2.2 to its own loopback, so the
    // marker server is reachable without relaxing the private-address rules.
    let relay = BunRelay::start();
    let mut guest = Guest::boot_linux(None, Some(relay.url()));

    guest.run_until(&MOTD, "the motd banner");
    guest.run_until(&SHELL_PROMPT, "the BusyBox ash prompt");
    guest.run_until(&Pattern::Text(CURSOR_QUERY), "the terminal cursor query");

    // 1. DHCP: `--net user` flips the bootargs placeholder to
    //    emulate.net=dhcp, so init has already taken the lease on eth0. No
    //    manual udhcpc here, on purpose.
    guest.shell("ip addr show eth0; echo EMULATE_IP_DONE");
    let addresses = guest.run_until(
        &Pattern::Line(b"EMULATE_IP_DONE"),
        "the eth0 address listing",
    );
    assert!(
        common::contains(&addresses, GUEST_IP.as_bytes()),
        "eth0 did not receive {GUEST_IP}:\n{}",
        String::from_utf8_lossy(&addresses)
    );

    // 2. ICMP: the gateway answers echo requests.
    guest.shell(&format!(
        "ping -c 1 -W 5 {GATEWAY_IP} && echo EMULATE_PING_OK"
    ));
    guest.run_until(
        &Pattern::Line(b"EMULATE_PING_OK"),
        "an ICMP echo reply from the gateway",
    );

    // 3. TCP: fetch the marker from the host through 10.0.2.2.
    guest.shell(&format!("wget -q -O - {url}; echo EMULATE_WGET_DONE"));
    let fetched = guest.run_until(&Pattern::Line(b"EMULATE_WGET_DONE"), "the guest HTTP fetch");
    assert!(
        common::contains(&fetched, MARKER.as_bytes()),
        "wget did not return the marker:\n{}",
        String::from_utf8_lossy(&fetched)
    );

    guest.shell(&format!(
        "for n in 1 2 3; do wget -q -O /tmp/net-$n {url} & done; wait; \
         cat /tmp/net-1 /tmp/net-2 /tmp/net-3; echo EMULATE_PARALLEL_DONE"
    ));
    let parallel = guest.run_until(
        &Pattern::Line(b"EMULATE_PARALLEL_DONE"),
        "concurrent guest HTTP fetches",
    );
    assert_eq!(
        String::from_utf8_lossy(&parallel).matches(MARKER).count(),
        3
    );

    guest.shell(&format!(
        "nslookup localhost {DNS_IP}; echo EMULATE_LOCAL_DNS_DONE"
    ));
    let dns = guest.run_until(
        &Pattern::Line(b"EMULATE_LOCAL_DNS_DONE"),
        "DNS over the shared relay socket",
    );
    assert!(common::ipv4_addresses(&dns)
        .iter()
        .any(|address| address == "127.0.0.1"));

    common::pass(&format!(
        "linux-net: guest took {GUEST_IP} by DHCP, pinged {GATEWAY_IP}, and fetched \
         {MARKER} over HTTP from the host in {:.1}s",
        guest.elapsed().as_secs_f64()
    ));
}

#[test]
#[ignore = "requires internet access, Bun and guest images; run just test-online"]
fn outbound_dns_resolves_an_address() {
    common::require_guest();
    let relay = BunRelay::start();
    let mut guest = Guest::boot_linux(None, Some(relay.url()));
    guest.run_until(&SHELL_PROMPT, "the BusyBox ash prompt");
    guest.shell(&format!(
        "nslookup example.com {DNS_IP} 2>&1; echo EMULATE_DNS_DONE"
    ));
    let lookup = guest.run_until(&Pattern::Line(b"EMULATE_DNS_DONE"), "the DNS lookup");
    // nslookup prints the resolver address even when resolution fails.
    let answers: Vec<_> = common::ipv4_addresses(&lookup)
        .into_iter()
        .filter(|address| address != DNS_IP)
        .collect();
    assert!(
        !answers.is_empty(),
        "no DNS A record returned:\n{}",
        String::from_utf8_lossy(&lookup)
    );
}

#[test]
#[ignore = "requires Alpine rootfs and Bun; run just test-guest"]
fn emuctl_net_disconnect_clears_the_interface_and_connect_restores_networking() {
    common::require_rootfs();
    let scratch = common::scratch_dir("emuctl-connect");
    let disk = scratch.join("rootfs.ext4");
    common::sparse_copy(
        &common::repo_root().join("guest/out/alpine-rootfs.ext4"),
        &disk,
    );
    let relay = BunRelay::start();
    let port = marker_server();
    let mut guest = Guest::boot_linux(Some(&disk), Some(relay.url()));
    guest.run_until(&common::ROOTFS_PROMPT, "the Alpine shell");
    for _ in 0..2 {
        guest.shell(
            "emuctl net disconnect && emuctl net disconnect && sleep 3 && \
             test -z \"$(ip -4 -o addr show dev eth0)\" && \
             test -z \"$(ip route show dev eth0)\" && \
             test \"$(($(cat /sys/class/net/eth0/flags) & 1))\" -eq 0 && \
             echo EMUCTL_NET_'DOWN'",
        );
        guest.run_until(
            &Pattern::Line(b"EMUCTL_NET_DOWN"),
            "eth0 down without addresses or routes",
        );
        guest.shell(&format!(
            "emuctl net connect && wget -q -O - http://{GATEWAY_IP}:{port}/marker"
        ));
        let output = guest.run_until(
            &Pattern::Line(MARKER.as_bytes()),
            "HTTP after emuctl net connect",
        );
        assert!(common::contains(&output, GUEST_IP.as_bytes()));
    }
    drop(guest);
    std::fs::remove_dir_all(scratch).expect("remove emuctl scratch disk");
}
