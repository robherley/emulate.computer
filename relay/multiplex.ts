import { credit, decodeControl, decodeData, encodeData, MUX_FRAME, MUX_SOCKET_BUFFER, MUX_WINDOW } from "./mux-protocol.ts";

export interface FlowSocket<T> {
  data: T;
  readonly cancelled?: boolean;
  sendText(text: string): unknown;
  sendBinary(data: Uint8Array): unknown;
  close(code?: number): unknown;
  ack(bytes: number): void;
}
interface WireSocket {
  sendText(text: string): unknown;
  sendBinary(data: Uint8Array): unknown;
  getBufferedAmount(): number;
  close(code?: number): unknown;
}
interface Hooks<T> {
  open(socket: FlowSocket<T>, command: string): Promise<void>;
  message(socket: FlowSocket<T>, message: string | Uint8Array): void;
  close(socket: FlowSocket<T>): void;
  pause(socket: FlowSocket<T>, paused: boolean): void;
}

export class MultiplexPeer<T> {
  private channels = new Map<number, Channel<T>>();
  private lastId = 0;
  private pendingBytes = 0;
  private closed = false;
  ws: WireSocket | null = null;

  constructor(readonly ip: string, private maxChannels: number, readonly hooks: Hooks<T>) {}

  message(message: string | Uint8Array): void {
    if (this.closed) return;
    if (message === "PING") { this.ws?.sendText("PONG"); return; }
    try {
      if (typeof message !== "string") {
        const { id, data } = decodeData(message);
        this.channels.get(id)?.receive(data);
        return;
      }
      const { id, command } = decodeControl(message);
      if (command.startsWith("CONNECT ") || command.startsWith("RESOLVE ")) {
        if (id <= this.lastId) throw new Error("channel reuse");
        this.lastId = id;
        if (this.channels.size >= this.maxChannels) {
          this.control(id, "ERR connection cap reached");
          this.control(id, "CLOSE");
          return;
        }
        const channel = new Channel(id, this);
        this.channels.set(id, channel);
        void this.hooks.open(channel, command).catch(() => channel.abort());
      } else {
        const channel = this.channels.get(id);
        if (!channel) return;
        if (command === "CLOSE") channel.abort();
        else {
          const bytes = credit(command, "WINDOW");
          if (bytes !== null) channel.window(bytes);
          else if (command === "FIN") this.hooks.message(channel, command);
          else throw new Error("unknown control");
        }
      }
    } catch {
      this.stop();
    }
  }

  reserve(bytes: number): boolean {
    if (this.pendingBytes + bytes > MUX_SOCKET_BUFFER) return false;
    this.pendingBytes += bytes;
    return true;
  }
  release(bytes: number): void { this.pendingBytes -= bytes; }
  control(id: number, text: string): void {
    if (this.ws && this.ws.getBufferedAmount() >= MUX_SOCKET_BUFFER) this.stop();
    if (!this.closed) this.ws?.sendText(`${id} ${text}`);
  }
  writable(): boolean {
    return !this.closed && this.ws !== null && this.ws.getBufferedAmount() < MUX_SOCKET_BUFFER;
  }
  binary(id: number, bytes: Uint8Array): void { this.ws?.sendBinary(encodeData(id, bytes)); }
  remove(id: number): void { this.channels.delete(id); }
  drain(): void { for (const channel of this.channels.values()) channel.drain(); }
  stop(): void {
    if (this.closed) return;
    this.closed = true;
    for (const channel of [...this.channels.values()]) channel.abort();
    this.ws?.close(1000);
  }
}

class Channel<T> implements FlowSocket<T> {
  data!: T;
  private queue: (string | Uint8Array)[] = [];
  private queued = 0;
  private available = MUX_WINDOW;
  private incoming = 0;
  private closing = false;
  private dead = false;

  constructor(private id: number, private peer: MultiplexPeer<T>) {}

  receive(bytes: Uint8Array): void {
    if (this.dead || this.closing || !this.data) return;
    this.incoming += bytes.length;
    if (this.incoming > MUX_WINDOW) { this.abort(); return; }
    this.peer.hooks.message(this, bytes);
  }
  ack(bytes: number): void {
    this.incoming -= bytes;
    this.peer.control(this.id, `ACK ${bytes}`);
  }
  window(bytes: number): void {
    if (this.available + bytes > MUX_WINDOW) throw new Error("excess credit");
    this.available += bytes;
    this.drain();
  }
  sendText(text: string): void {
    if (this.dead) return;
    if (this.queue.length >= 32) { this.abort(); return; }
    this.queue.push(text);
    this.drain();
  }
  sendBinary(bytes: Uint8Array): void {
    if (this.dead) return;
    if (this.queued + bytes.length > MUX_WINDOW + MUX_FRAME || !this.peer.reserve(bytes.length)) { this.abort(); return; }
    this.queued += bytes.length;
    this.queue.push(bytes.slice());
    this.drain();
  }
  drain(): void {
    if (this.dead) return;
    while (this.queue.length && this.peer.writable()) {
      const message = this.queue[0];
      if (typeof message === "string") {
        this.peer.control(this.id, message);
        this.queue.shift();
      } else {
        const count = Math.min(message.length, this.available, MUX_FRAME);
        if (!count) break;
        this.peer.binary(this.id, message.subarray(0, count));
        this.available -= count;
        this.queued -= count;
        this.peer.release(count);
        if (count === message.length) this.queue.shift();
        else this.queue[0] = message.subarray(count);
      }
    }
    if (this.data) this.peer.hooks.pause(this, this.available === 0 || this.queue.length > 0 || !this.peer.writable());
    if (this.closing && this.queue.length === 0) {
      this.peer.control(this.id, "CLOSE");
      this.dead = true;
      this.peer.remove(this.id);
    }
  }
  close(): void { this.closing = true; this.drain(); }
  abort(): void {
    if (this.dead) return;
    this.dead = true;
    this.peer.release(this.queued);
    this.queued = 0;
    this.queue = [];
    this.peer.control(this.id, "CLOSE");
    this.peer.remove(this.id);
    if (this.data) this.peer.hooks.close(this);
  }
  get cancelled(): boolean { return this.dead; }
}
