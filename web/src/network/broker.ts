import { credit, decodeControl, decodeData, encodeData, MUX_FRAME, MUX_SOCKET_BUFFER, MUX_WINDOW } from "../../../relay/mux-protocol.ts";

export const PORT_LIMIT = 1024 * 1024;
const TOTAL_LIMIT = 8 * PORT_LIMIT;
type Client = { port: MessagePort; channels: Map<number, Channel>; bytes: number; seen: number; leased: boolean };
type Channel = { id: number; local: number; client: Client; opened: boolean; bytes: number; received: number };

export function relayUrl(value: string): string {
  const url = new URL(value);
  if (url.protocol !== "ws:" && url.protocol !== "wss:") throw new Error("invalid relay URL");
  return url.href;
}

export class RelayBroker {
  private clients = new Set<Client>();
  private channels = new Map<number, Channel>();
  private socket: WebSocket | null = null;
  private nextId = 0;
  private queue: { channel: Channel; message: string | Uint8Array }[] = [];
  private queued = 0;
  private retry = 0;
  private retryAt = 0;
  private receivedAt = 0;
  private pingAt = 0;
  private timer: ReturnType<typeof setInterval>;

  constructor(readonly url: string, private empty: () => void = () => {}, private makeSocket = (url: string) => new WebSocket(url)) {
    this.timer = setInterval(() => this.tick(), 25);
  }

  attach(port: MessagePort, leased = false): () => void {
    const client: Client = { port, channels: new Map(), bytes: 0, seen: Date.now(), leased };
    this.clients.add(client);
    port.onmessage = event => {
      client.seen = Date.now();
      if (event.data?.type === "detach") this.detach(client);
      else if (event.data?.type !== "heartbeat") this.message(client, event.data);
    };
    port.onmessageerror = () => this.detach(client);
    port.start();
    this.connect();
    port.postMessage({ type: "ready", connected: this.connected });
    return () => this.detach(client);
  }

  get connected(): boolean { return this.socket?.readyState === 1; }

  private connect(): void {
    if (this.socket || !this.clients.size || Date.now() < this.retryAt) return;
    this.openSocket(this.url);
  }
  private retryLater(): void {
    this.retryAt = Date.now() + Math.min(30_000, 1000 * 2 ** this.retry++);
  }
  private openSocket(url: string): void {
    if (!this.clients.size || this.socket) return;
    let socket: WebSocket;
    try { socket = this.makeSocket(url); } catch { this.retryLater(); return; }
    this.socket = socket;
    socket.binaryType = "arraybuffer";
    this.receivedAt = this.pingAt = Date.now();
    socket.onopen = () => {
      if (this.socket !== socket) return;
      this.retry = 0;
      this.receivedAt = Date.now();
      this.broadcast(true);
      for (const client of this.clients) for (const channel of client.channels.values()) this.opened(channel);
    };
    socket.onmessage = event => {
      if (this.socket !== socket) return;
      this.receivedAt = Date.now();
      try { this.receive(event.data); } catch { this.lost(socket); }
    };
    socket.onerror = socket.onclose = () => this.lost(socket);
  }

  private broadcast(connected: boolean): void {
    for (const client of this.clients) client.port.postMessage({ type: "status", connected });
  }
  private opened(channel: Channel): void {
    if (!channel.opened && this.connected) {
      channel.opened = true;
      channel.client.port.postMessage({ type: "event", id: channel.local, kind: 0 });
    }
  }
  private message(client: Client, message: any): void {
    const local = message?.id;
    if (!Number.isInteger(local) || local <= 0 || local > 0xffff_ffff) { this.detach(client); return; }
    if (message.type === "open") {
      if (client.channels.has(local)) { this.detach(client); return; }
      if (client.channels.size >= 64 || [...this.clients].reduce((n, c) => n + c.channels.size, 0) >= 128 || this.nextId >= 0xffff_ffff) {
        client.port.postMessage({ type: "event", id: local, kind: 4 });
        return;
      }
      const channel: Channel = { id: 0, local, client, opened: false, bytes: 0, received: 0 };
      client.channels.set(local, channel);
      this.opened(channel);
      return;
    }
    const channel = client.channels.get(local);
    if (!channel) return;
    if (message.type === "close") { this.close(channel, true); return; }
    if (!channel.opened) { this.close(channel, true); return; }
    if (message.type === "text" && typeof message.text === "string" && message.text.length <= 4096) {
      if (!/^(CONNECT |RESOLVE |FIN$)/.test(message.text)) { this.close(channel, true); return; }
      if (channel.id === 0) {
        if (message.text === "FIN") { this.close(channel, false); return; }
        channel.id = ++this.nextId;
        this.channels.set(channel.id, channel);
      } else if (message.text !== "FIN") { this.close(channel, true); return; }
      this.enqueue(channel, `${channel.id} ${message.text}`);
    } else if (message.type === "binary" && message.data instanceof Uint8Array) {
      const bytes = message.data.length;
      if (!channel.id || !bytes || bytes > MUX_FRAME || channel.bytes + bytes > MUX_WINDOW || client.bytes + bytes > PORT_LIMIT) {
        this.close(channel, true); return;
      }
      channel.bytes += bytes;
      client.bytes += bytes;
      this.enqueue(channel, encodeData(channel.id, message.data));
    } else if (message.type === "consumed" && Number.isInteger(message.bytes) && message.bytes > 0 && message.bytes <= channel.received) {
      channel.received -= message.bytes;
      this.enqueue(channel, `${channel.id} WINDOW ${message.bytes}`);
    } else this.close(channel, true);
  }

  private enqueue(channel: Channel, message: string | Uint8Array): void {
    const size = message.length;
    if (this.queued + size > TOTAL_LIMIT || this.queue.length >= 4096) { this.close(channel, true); return; }
    this.queue.push({ channel, message });
    this.queued += size;
    this.flush();
  }
  private flush(): void {
    const socket = this.socket;
    if (!socket || socket.readyState !== 1) return;
    try {
      while (this.queue.length && socket.bufferedAmount < MUX_SOCKET_BUFFER) {
        const item = this.queue.shift()!;
        this.queued -= item.message.length;
        socket.send(item.message);
      }
    } catch { this.lost(socket); }
  }

  private receive(message: unknown): void {
    if (message === "PONG") return;
    if (typeof message === "string") {
      const { id, command } = decodeControl(message);
      const channel = this.channels.get(id);
      if (!channel) return;
      const bytes = credit(command, "ACK");
      if (bytes !== null) {
        if (bytes > channel.bytes) throw new Error("excess acknowledgement");
        channel.bytes -= bytes;
        channel.client.bytes -= bytes;
        channel.client.port.postMessage({ type: "ack", id: channel.local, bytes });
      } else if (command === "CLOSE") this.close(channel, false);
      else if (/^(OK$|ERR |EOF$|IP )/.test(command)) {
        channel.client.port.postMessage({ type: "event", id: channel.local, kind: 1, data: command });
      } else throw new Error("unknown response");
    } else if (message instanceof ArrayBuffer) {
      const { id, data } = decodeData(new Uint8Array(message));
      const channel = this.channels.get(id);
      if (!channel) return;
      channel.received += data.length;
      if (channel.received > MUX_WINDOW) throw new Error("receive window exceeded");
      const copy = data.slice();
      channel.client.port.postMessage({ type: "event", id: channel.local, kind: 2, data: copy }, [copy.buffer]);
    } else throw new Error("invalid frame");
  }

  private close(channel: Channel, notifyRelay: boolean): void {
    if (!channel.client.channels.delete(channel.local)) return;
    this.channels.delete(channel.id);
    channel.client.bytes -= channel.bytes;
    this.queue = this.queue.filter(item => {
      if (item.channel !== channel) return true;
      this.queued -= item.message.length;
      return false;
    });
    channel.client.port.postMessage({ type: "event", id: channel.local, kind: 3 });
    if (notifyRelay && channel.id && this.connected) {
      // Closing controls cannot accumulate behind a stalled transport.
      if (this.socket!.bufferedAmount >= MUX_SOCKET_BUFFER) this.lost(this.socket!);
      else this.socket!.send(`${channel.id} CLOSE`);
    }
  }
  private lost(socket: WebSocket): void {
    if (this.socket !== socket) return;
    this.socket = null;
    socket.onopen = socket.onclose = socket.onerror = socket.onmessage = null;
    socket.close();
    this.broadcast(false);
    for (const client of this.clients) for (const channel of [...client.channels.values()]) this.close(channel, false);
    this.retryAt = Date.now() + Math.min(30_000, 500 * 2 ** this.retry++);
  }
  private tick(): void {
    const now = Date.now();
    for (const client of this.clients) if (!client.leased && now - client.seen > 120_000) this.detach(client);
    this.connect();
    if (this.socket && now - this.receivedAt > 60_000) this.lost(this.socket);
    if (this.connected && now - this.pingAt > 20_000) {
      this.pingAt = now;
      if (this.socket!.bufferedAmount < MUX_SOCKET_BUFFER) this.socket!.send("PING");
    }
    this.flush();
  }
  private detach(client: Client): void {
    if (!this.clients.delete(client)) return;
    for (const channel of [...client.channels.values()]) this.close(channel, true);
    client.port.close();
    if (!this.clients.size) { this.stop(); this.empty(); }
  }
  stop(): void {
    clearInterval(this.timer);
    if (this.socket) this.lost(this.socket);
    for (const client of this.clients) client.port.close();
    this.clients.clear();
  }
}
