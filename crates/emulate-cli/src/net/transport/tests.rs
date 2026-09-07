use super::*;
use std::net::TcpListener;

fn until(
    transport: &mut TungsteniteTransport,
    wanted: impl Fn(&[TransportEvent]) -> bool,
) -> Vec<TransportEvent> {
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut events = Vec::new();
    loop {
        events.extend(transport.poll(0));
        if wanted(&events) {
            return events;
        }
        assert!(Instant::now() < deadline, "events: {events:?}");
        std::thread::sleep(Duration::from_millis(1));
    }
}

fn accept(listener: &TcpListener) -> std::thread::JoinHandle<Socket> {
    let listener = listener.try_clone().unwrap();
    std::thread::spawn(move || {
        let (stream, _) = listener.accept().unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        stream
            .set_write_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        tungstenite::accept(stream).unwrap()
    })
}

fn start() -> (TungsteniteTransport, Socket, ConnId, TcpListener, String) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("ws://{}/", listener.local_addr().unwrap());
    let server = accept(&listener);
    let mut transport = TungsteniteTransport::default();
    let id = transport.open(&url).unwrap();
    until(&mut transport, |events| {
        events.contains(&TransportEvent::Opened(id))
    });
    (transport, server.join().unwrap(), id, listener, url)
}

fn text(socket: &mut Socket) -> String {
    socket.read().unwrap().into_text().unwrap().to_string()
}

#[test]
fn channels_share_one_handshake_and_keep_commands_and_closure_isolated() {
    let (mut transport, mut server, first, listener, url) = start();
    let second = transport.open(&url).unwrap();
    let dns = transport.open(&url).unwrap();
    assert_eq!(
        transport.poll(0),
        vec![TransportEvent::Opened(second), TransportEvent::Opened(dns)]
    );
    assert!(transport.send_text(first, "CONNECT 1.1.1.1:80"));
    assert!(transport.send_text(second, "CONNECT 8.8.8.8:443"));
    assert!(transport.send_text(dns, "RESOLVE example.com"));
    transport.poll(0);
    assert_eq!(text(&mut server), "1 CONNECT 1.1.1.1:80");
    assert_eq!(text(&mut server), "2 CONNECT 8.8.8.8:443");
    assert_eq!(text(&mut server), "3 RESOLVE example.com");
    listener.set_nonblocking(true).unwrap();
    assert_eq!(listener.accept().unwrap_err().kind(), ErrorKind::WouldBlock);
    for reply in ["1 OK", "2 OK", "3 IP 93.184.216.34", "3 CLOSE"] {
        server.send(Message::Text(reply.into())).unwrap();
    }
    let events = until(&mut transport, |events| {
        events.contains(&TransportEvent::Closed(dns))
    });
    assert_eq!(
        events,
        vec![
            TransportEvent::Text(first, "OK".into()),
            TransportEvent::Text(second, "OK".into()),
            TransportEvent::Text(dns, "IP 93.184.216.34".into()),
            TransportEvent::Closed(dns)
        ]
    );
    transport.close(first);
    assert_eq!(text(&mut server), "1 CLOSE");
    assert!(transport.send_text(second, "FIN"));
    transport.poll(0);
    assert_eq!(text(&mut server), "2 FIN");
}

#[test]
fn upload_credit_and_frame_limits_preserve_bytes_and_resume_after_ack() {
    let (mut transport, mut server, id, _, _) = start();
    assert!(transport.send_text(id, "CONNECT 1.1.1.1:80"));
    let data: Vec<_> = (0..WINDOW).map(|i| (i % 251) as u8).collect();
    assert!(transport.send_binary(id, &data));
    assert_eq!(transport.buffered_amount(id), WINDOW as u64);
    assert!(!transport.send_binary(id, &[1]));
    transport.poll(0);
    assert_eq!(text(&mut server), "1 CONNECT 1.1.1.1:80");
    let mut received = Vec::new();
    for _ in 0..4 {
        let frame = server.read().unwrap().into_data();
        assert_eq!(frame.len(), FRAME + 4);
        assert_eq!(&frame[..4], &1u32.to_be_bytes());
        received.extend_from_slice(&frame[4..]);
    }
    assert_eq!(received, data);
    server
        .send(Message::Text(format!("1 ACK {FRAME}").into()))
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    while transport.buffered_amount(id) == WINDOW as u64 {
        transport.poll(0);
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(1));
    }
    assert_eq!(transport.buffered_amount(id), (WINDOW - FRAME) as u64);
    assert!(transport.send_binary(id, &data[..FRAME]));
}

#[test]
fn download_credit_is_returned_on_poll_and_data_precedes_eof() {
    let (mut transport, mut server, id, _, _) = start();
    assert!(transport.send_text(id, "CONNECT 1.1.1.1:80"));
    transport.poll(0);
    text(&mut server);
    let mut frame = 1u32.to_be_bytes().to_vec();
    frame.extend_from_slice(b"hello");
    server.send(Message::Binary(frame.into())).unwrap();
    assert_eq!(
        until(&mut transport, |events| !events.is_empty()),
        vec![TransportEvent::Binary(id, b"hello".to_vec())]
    );
    assert_eq!(text(&mut server), "1 WINDOW 5");
    server.send(Message::Text("1 EOF".into())).unwrap();
    server.send(Message::Text("1 CLOSE".into())).unwrap();
    assert_eq!(
        until(&mut transport, |events| events
            .contains(&TransportEvent::Closed(id))),
        vec![
            TransportEvent::Text(id, "EOF".into()),
            TransportEvent::Closed(id)
        ]
    );
}

#[test]
fn socket_loss_fails_all_channels_once_and_new_requests_reconnect() {
    let (mut transport, server, first, listener, url) = start();
    let second = transport.open(&url).unwrap();
    transport.poll(0);
    drop(server);
    assert_eq!(
        until(&mut transport, |events| events
            .contains(&TransportEvent::Error(first))),
        vec![TransportEvent::Error(first), TransportEvent::Error(second)]
    );
    assert!(transport.poll(0).is_empty());
    let accept = accept(&listener);
    let third = transport.open(&url).unwrap();
    assert!(third > second);
    until(&mut transport, |events| {
        events.contains(&TransportEvent::Opened(third))
    });
    let mut server = accept.join().unwrap();
    assert!(transport.send_text(third, "RESOLVE example.com"));
    transport.poll(0);
    assert_eq!(text(&mut server), "3 RESOLVE example.com");
}

#[test]
fn invalid_credit_and_framing_fail_the_shared_socket() {
    for message in [
        "1 ACK 1",
        "1 ACK 0",
        "1 ACK 01",
        "1 ACK +1",
        "0 OK",
        "4294967296 OK",
        "1 WINDOW 1",
    ] {
        let (mut transport, mut server, id, _, _) = start();
        server.send(Message::Text(message.into())).unwrap();
        let events = until(&mut transport, |events| {
            events.contains(&TransportEvent::Error(id))
        });
        assert_eq!(events, vec![TransportEvent::Error(id)], "{message}");
    }
}

#[test]
fn aggregate_upload_budget_and_channel_count_are_bounded() {
    let (mut transport, mut server, first, _, url) = start();
    let mut ids = vec![first];
    for _ in 1..MAX_CHANNELS {
        ids.push(transport.open(&url).unwrap());
    }
    assert!(transport.open(&url).is_none());
    transport.poll(0);
    for &id in &ids[..4] {
        assert!(transport.send_text(id, "CONNECT 1.1.1.1:80"));
        assert!(transport.send_binary(id, &vec![0; WINDOW]));
        transport.poll(0);
        text(&mut server);
        for _ in 0..4 {
            server.read().unwrap();
        }
    }
    assert_eq!(transport.buffered_amount(ids[4]), WINDOW as u64);
    transport.close(first);
    assert_eq!(transport.buffered_amount(ids[4]), 0);
    transport.close(ids[4]);
    assert!(!transport.poll(0).iter().any(|event| event.conn() == ids[4]));
}

#[test]
fn a_new_flow_after_an_unpolled_idle_period_keeps_the_socket() {
    let (mut transport, mut server, first, _, url) = start();
    transport.close(first);
    transport.received_at = Some(Instant::now() - Duration::from_secs(120));
    transport.ping_at = transport.received_at;
    let next = transport.open(&url).unwrap();
    assert_eq!(transport.poll(0), vec![TransportEvent::Opened(next)]);
    assert_eq!(text(&mut server), "PING");
    server.send(Message::Text("PONG".into())).unwrap();
    assert!(transport.send_text(next, "RESOLVE example.com"));
    transport.poll(0);
    assert_eq!(text(&mut server), "2 RESOLVE example.com");
}
