use std::collections::BTreeMap;
use std::io::ErrorKind;
use std::net::TcpStream;
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::time::{Duration, Instant};

use emulate_core::hostnet::{ConnId, RelayTransport, TransportEvent};
use tungstenite::{protocol::WebSocketConfig, Message, WebSocket};

use super::{authority, HANDSHAKE_TIMEOUT};

// Wire limits match relay/mux-protocol.ts.
const WINDOW: usize = 256 * 1024;
const FRAME: usize = 64 * 1024;
const BUFFER: usize = 1024 * 1024;
const MAX_CHANNELS: usize = 64;
const MAX_READS: usize = 64;

type Socket = WebSocket<TcpStream>;

#[derive(Default)]
enum Connection {
    #[default]
    Idle,
    Handshaking(Receiver<Result<Socket, String>>),
    Live(Box<Socket>),
}

#[derive(Default)]
struct Channel {
    opened: bool,
    started: bool,
    pending: usize,
    received: usize,
}

/// One nonblocking WebSocket carrying independent TCP and DNS channels.
#[derive(Default)]
pub struct TungsteniteTransport {
    connection: Connection,
    url: String,
    next: u32,
    channels: BTreeMap<ConnId, Channel>,
    pending: usize,
    wire_buffered: usize,
    events: Vec<TransportEvent>,
    received_at: Option<Instant>,
    ping_at: Option<Instant>,
}

fn dial(url: &str) -> Result<Socket, String> {
    let stream = TcpStream::connect_timeout(&authority(url)?, HANDSHAKE_TIMEOUT)
        .map_err(|error| error.to_string())?;
    stream
        .set_read_timeout(Some(HANDSHAKE_TIMEOUT))
        .map_err(|e| e.to_string())?;
    stream
        .set_write_timeout(Some(HANDSHAKE_TIMEOUT))
        .map_err(|e| e.to_string())?;
    let _ = stream.set_nodelay(true);
    let config = WebSocketConfig::default()
        .max_message_size(Some(FRAME + 4))
        .max_frame_size(Some(FRAME + 4))
        .max_write_buffer_size(2 * BUFFER);
    let (socket, _) = tungstenite::client::client_with_config(url, stream, Some(config))
        .map_err(|error| error.to_string())?;
    socket
        .get_ref()
        .set_read_timeout(None)
        .map_err(|e| e.to_string())?;
    socket
        .get_ref()
        .set_write_timeout(None)
        .map_err(|e| e.to_string())?;
    socket
        .get_ref()
        .set_nonblocking(true)
        .map_err(|e| e.to_string())?;
    Ok(socket)
}

fn would_block(error: &tungstenite::Error) -> bool {
    matches!(error, tungstenite::Error::Io(io) if io.kind() == ErrorKind::WouldBlock)
}

impl TungsteniteTransport {
    fn fail(&mut self) {
        self.connection = Connection::Idle;
        self.events
            .extend(self.channels.keys().copied().map(TransportEvent::Error));
        self.channels.clear();
        self.pending = 0;
        self.wire_buffered = 0;
        self.received_at = None;
        self.ping_at = None;
    }

    fn send(&mut self, message: Message) -> bool {
        let Connection::Live(socket) = &mut self.connection else {
            return false;
        };
        let len = message.len() + 14;
        if self.wire_buffered + len > 2 * BUFFER {
            self.fail();
            return false;
        }
        match socket.write(message) {
            Ok(()) => {}
            // Tungstenite retains the frame when the socket would block.
            Err(error) if would_block(&error) => {}
            Err(_) => {
                self.fail();
                return false;
            }
        }
        self.wire_buffered += len;
        true
    }

    fn control(&mut self, id: ConnId, text: &str) -> bool {
        self.send(Message::Text(format!("{} {text}", id.0).into()))
    }

    fn flush(&mut self) -> bool {
        let Connection::Live(socket) = &mut self.connection else {
            return false;
        };
        match socket.flush() {
            Ok(()) => {
                self.wire_buffered = 0;
                true
            }
            Err(error) if would_block(&error) => true,
            Err(_) => {
                self.fail();
                false
            }
        }
    }

    fn receive(&mut self, message: Message) -> Result<usize, ()> {
        match message {
            Message::Text(text) => {
                if text == "PONG" {
                    return Ok(0);
                }
                if text.len() > 4096 {
                    return Err(());
                }
                let (id, command) = text.split_once(' ').ok_or(())?;
                let id = ConnId(u64::from(positive(id, u32::MAX as usize)?));
                let Some(channel) = self.channels.get_mut(&id) else {
                    return Ok(0);
                };
                if let Some(bytes) = command.strip_prefix("ACK ") {
                    let bytes = positive(bytes, WINDOW)? as usize;
                    if bytes > channel.pending {
                        return Err(());
                    }
                    channel.pending -= bytes;
                    self.pending -= bytes;
                } else if command == "CLOSE" {
                    self.pending -= channel.pending;
                    self.channels.remove(&id);
                    self.events.push(TransportEvent::Closed(id));
                } else if command == "OK"
                    || command == "EOF"
                    || command.starts_with("ERR ")
                    || command.starts_with("IP ")
                {
                    self.events
                        .push(TransportEvent::Text(id, command.to_owned()));
                } else {
                    return Err(());
                }
                Ok(0)
            }
            Message::Binary(data) => {
                if !(5..=FRAME + 4).contains(&data.len()) {
                    return Err(());
                }
                let id = ConnId(u64::from(u32::from_be_bytes(
                    data[..4].try_into().map_err(|_| ())?,
                )));
                if id.0 == 0 {
                    return Err(());
                }
                let bytes = data.len() - 4;
                if let Some(channel) = self.channels.get_mut(&id) {
                    channel.received += bytes;
                    if channel.received > WINDOW {
                        return Err(());
                    }
                    self.events
                        .push(TransportEvent::Binary(id, data[4..].to_vec()));
                }
                Ok(bytes)
            }
            Message::Close(_) => Err(()),
            Message::Ping(_) | Message::Pong(_) => Ok(0),
            Message::Frame(_) => Err(()),
        }
    }
}

fn positive(text: &str, max: usize) -> Result<u32, ()> {
    if text.starts_with('0') || !text.bytes().all(|b| b.is_ascii_digit()) {
        return Err(());
    }
    text.parse::<u32>()
        .ok()
        .filter(|&n| n > 0 && n as usize <= max)
        .ok_or(())
}

impl RelayTransport for TungsteniteTransport {
    fn open(&mut self, url: &str) -> Option<ConnId> {
        if self.channels.len() >= MAX_CHANNELS || self.next == u32::MAX {
            return None;
        }
        if matches!(self.connection, Connection::Idle) {
            let (tx, rx) = mpsc::channel();
            let target = url.to_owned();
            std::thread::Builder::new()
                .name("emulate-relay-dial".into())
                .spawn(move || {
                    let _ = tx.send(dial(&target));
                })
                .ok()?;
            self.connection = Connection::Handshaking(rx);
            self.url = url.to_owned();
        } else if self.url != url {
            return None;
        }
        // An idle guest may stop polling; elapsed idle time is not a missed pong.
        if self.channels.is_empty() && matches!(self.connection, Connection::Live(_)) {
            self.received_at = Some(Instant::now());
        }
        self.next += 1;
        let id = ConnId(u64::from(self.next));
        self.channels.insert(id, Channel::default());
        Some(id)
    }

    fn send_text(&mut self, id: ConnId, text: &str) -> bool {
        let Some(channel) = self.channels.get_mut(&id) else {
            return false;
        };
        if !channel.opened || text.len() > 4000 {
            return false;
        }
        if !channel.started {
            if !text.starts_with("CONNECT ") && !text.starts_with("RESOLVE ") {
                return false;
            }
            channel.started = true;
        } else if text != "FIN" {
            return false;
        }
        self.control(id, text)
    }

    fn send_binary(&mut self, id: ConnId, data: &[u8]) -> bool {
        let Some(channel) = self.channels.get_mut(&id) else {
            return false;
        };
        if !channel.started
            || channel.pending + data.len() > WINDOW
            || self.pending + data.len() > BUFFER
        {
            return false;
        }
        channel.pending += data.len();
        self.pending += data.len();
        for chunk in data.chunks(FRAME) {
            let mut frame = Vec::with_capacity(chunk.len() + 4);
            frame.extend_from_slice(&(id.0 as u32).to_be_bytes());
            frame.extend_from_slice(chunk);
            if !self.send(Message::Binary(frame.into())) {
                return false;
            }
        }
        true
    }

    fn buffered_amount(&self, id: ConnId) -> u64 {
        self.channels.get(&id).map_or(WINDOW as u64, |channel| {
            channel
                .pending
                .max(WINDOW.saturating_sub(BUFFER - self.pending)) as u64
        })
    }

    fn close(&mut self, id: ConnId) {
        if let Some(channel) = self.channels.remove(&id) {
            self.pending -= channel.pending;
            if channel.started {
                self.control(id, "CLOSE");
                self.flush();
            }
        }
        self.events.retain(|event| event.conn() != id);
    }

    fn poll(&mut self, _now_ms: u64) -> Vec<TransportEvent> {
        if let Connection::Handshaking(rx) = &self.connection {
            match rx.try_recv() {
                Ok(Ok(socket)) => {
                    self.connection = Connection::Live(Box::new(socket));
                    self.received_at = Some(Instant::now());
                    self.ping_at = self.received_at;
                }
                Ok(Err(_)) | Err(TryRecvError::Disconnected) => self.fail(),
                Err(TryRecvError::Empty) => return std::mem::take(&mut self.events),
            }
        }
        if self.flush() {
            // Commands must reach the relay in increasing channel-ID order.
            for (&id, channel) in &mut self.channels {
                if !channel.opened {
                    channel.opened = true;
                    self.events.push(TransportEvent::Opened(id));
                }
            }
            let mut bytes = 0;
            for _ in 0..MAX_READS {
                if bytes >= BUFFER {
                    break;
                }
                let Connection::Live(socket) = &mut self.connection else {
                    break;
                };
                match socket.read() {
                    Ok(message) => {
                        self.received_at = Some(Instant::now());
                        match self.receive(message) {
                            Ok(count) => bytes += count,
                            Err(()) => {
                                self.fail();
                                break;
                            }
                        }
                    }
                    Err(error) if would_block(&error) => break,
                    Err(_) => {
                        self.fail();
                        break;
                    }
                }
            }
            let consumed: Vec<_> = self
                .channels
                .iter_mut()
                .filter_map(|(&id, channel)| {
                    let bytes = std::mem::take(&mut channel.received);
                    (bytes > 0).then_some((id, bytes))
                })
                .collect();
            for (id, bytes) in consumed {
                self.control(id, &format!("WINDOW {bytes}"));
            }
            if self
                .received_at
                .is_some_and(|at| at.elapsed() >= Duration::from_secs(60))
            {
                self.fail();
            } else if self
                .ping_at
                .is_some_and(|at| at.elapsed() >= Duration::from_secs(20))
            {
                self.send(Message::Text("PING".into()));
                self.ping_at = Some(Instant::now());
            }
            self.flush();
        }
        std::mem::take(&mut self.events)
    }
}

#[cfg(test)]
mod tests;
