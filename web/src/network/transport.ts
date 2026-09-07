import { MUX_FRAME, MUX_WINDOW } from "../../../relay/mux-protocol.ts";
import { ConnectionHistory } from "./connections.ts";
import { PORT_LIMIT } from "./broker";

type RelayEvent = [number, number, (string | Uint8Array)?];
type Flow = { owner: GuestTransport; pending: number; live: boolean };

export class NetworkPort {
  readonly connections = new ConnectionHistory();
  private flows = new Map<number, Flow>();
  private next = 0;
  private pending = 0;
  private heartbeat: ReturnType<typeof setInterval>;
  connected: boolean;
  constructor(private port: MessagePort, connected: boolean, private status: (connected: boolean) => void = () => {}, private wake: () => void = () => {}) {
    this.connected = connected;
    this.status(connected);
    port.onmessage = event => this.receive(event.data);
    port.onmessageerror = () => this.dispose();
    port.start();
    this.heartbeat = setInterval(() => this.send({ type: "heartbeat" }), 10_000);
  }
  send(message: unknown, transfer: Transferable[] = []): void { this.port.postMessage(message, transfer); }
  open(owner: GuestTransport): number {
    if (this.next >= 0xffff_ffff || this.flows.size >= 64) return 0;
    const id = ++this.next;
    this.flows.set(id, { owner, pending: 0, live: true });
    this.send({ type: "open", id });
    return id;
  }
  private receive(message: any): void {
    if (message?.type === "status") { this.connected = message.connected === true; this.status(this.connected);
      if (!this.connected) for (const id of this.flows.keys()) this.connections.close(id, true);
      return; }
    const flow = this.flows.get(message?.id);
    if (!flow) return;
    if (message.type === "ack") {
      if (!Number.isInteger(message.bytes) || message.bytes <= 0 || message.bytes > flow.pending) {
        this.close(message.id); return;
      }
      flow.pending -= message.bytes;
      this.pending -= message.bytes;
    } else if (message.type === "event") {
      if (message.kind === 1 && typeof message.data === "string") this.connections.reply(message.id, message.data);
      if (message.kind >= 3) this.connections.close(message.id, message.kind === 4);
      if (message.kind >= 3) {
        this.pending -= flow.pending;
        flow.pending = 0;
        flow.live = false;
      }
      flow.owner.events.push([message.kind, message.id, message.data]);
      this.wake();
    }
  }
  sendText(id: number, text: string): boolean {
    if (!this.flows.get(id)?.live) return false;
    this.connections.start(id, text);
    this.send({ type: "text", id, text });
    return true;
  }
  sendBinary(id: number, bytes: Uint8Array): boolean {
    const flow = this.flows.get(id);
    if (!flow?.live || flow.pending + bytes.length > MUX_WINDOW || this.pending + bytes.length > PORT_LIMIT) return false;
    flow.pending += bytes.length;
    this.pending += bytes.length;
    for (let offset = 0; offset < bytes.length; offset += MUX_FRAME) {
      const data = bytes.slice(offset, offset + MUX_FRAME);
      this.send({ type: "binary", id, data }, [data.buffer]);
    }
    return true;
  }
  bufferedAmount(id: number): number {
    const flow = this.flows.get(id);
    return !flow?.live ? MUX_WINDOW : Math.max(flow.pending, MUX_WINDOW - (PORT_LIMIT - this.pending));
  }
  close(id: number): void {
    const flow = this.flows.get(id);
    if (!flow) return;
    this.pending -= flow.pending;
    this.flows.delete(id);
    this.connections.close(id);
    this.send({ type: "close", id });
  }
  closeGuest(owner: GuestTransport): void {
    for (const [id, flow] of this.flows) if (flow.owner === owner) this.close(id);
  }
  dispose(): void {
    clearInterval(this.heartbeat);
    this.connected = false;
    this.status(false);
    for (const [id, flow] of this.flows) {
      this.connections.close(id, true);
      flow.owner.events.push([3, id]);
    }
    this.flows.clear();
    this.pending = 0;
    this.send({ type: "detach" });
    this.port.close();
  }
}

export class GuestTransport {
  events: RelayEvent[] = [];
  constructor(private network: NetworkPort) {}
  open(): number { return this.network.open(this); }
  sendText(id: number, text: string): boolean { return this.network.sendText(id, text); }
  sendBinary(id: number, bytes: Uint8Array): boolean { return this.network.sendBinary(id, bytes); }
  bufferedAmount(id: number): number { return this.network.bufferedAmount(id); }
  close(id: number): void {
    this.network.close(id);
    this.events = this.events.filter(event => event[1] !== id);
  }
  poll(): RelayEvent[] {
    const events = this.events;
    this.events = [];
    for (const [kind, id, data] of events) {
      if (kind === 2 && data instanceof Uint8Array)
        this.network.send({ type: "consumed", id, bytes: data.length });
    }
    return events;
  }
  dispose(): void { this.network.closeGuest(this); this.events = []; }
}
