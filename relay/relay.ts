import type { Server, ServerWebSocket, Socket } from "bun";
import { lookup } from "node:dns/promises";
import ipaddr from "ipaddr.js";
import { MultiplexPeer, type FlowSocket } from "./multiplex.ts";
import type { Limits } from "./limits.ts";

export interface Config {
  path: string;
  /** Match the request URL's origin, or use an allowlist ("*" allows all). */
  origins: "same-origin" | string[];
  /** Permit loopback / RFC1918 / link-local / CGNAT destinations. */
  allowPrivate: boolean;
  /** Destination port allowlist; empty means any. */
  ports: number[];
  /** Per-flow byte ceiling, both directions. */
  maxBytes: number;
  /** Concurrent flows for this process. */
  maxConns: number;
  /** Drop a flow with no traffic either way for this long. */
  idleTimeoutMs: number;
  /** After the client's FIN, a peer silent for this long is treated as closed. */
  finIdleMs: number;
  /** Take the client IP from X-Forwarded-For's first hop. */
  trustProxy: boolean;
  quiet: boolean;
}

export const CONNECT_TIMEOUT_MS = 15_000;
export const FLOW_BUFFER_LIMIT = 1024 * 1024;
export const RELAY_BUFFER_LIMIT = 8 * FLOW_BUFFER_LIMIT;
const QUOTA_TIMEOUT_MS = 10_000;

type State = "dialing" | "open" | "closed";

interface Flow {
  ip: string;
  peer: string;
  started: number;
  state: State;
  target: string;
  ws: FlowSocket<Flow> | null;
  tcp: Socket<Flow> | null;
  /** Client bytes that arrived before the TCP socket could take them. */
  backlog: Uint8Array[];
  /** When the client sent FIN, or 0. */
  finAt: number;
  bytes: number;
  buffered: number;
  pending: { bytes: Uint8Array; upload: boolean }[];
  charging: boolean;
  quotaTimer: ReturnType<typeof setTimeout> | null;
  wsPaused: boolean;
  peerEnded: boolean;
  lastActive: number;
  why: string | null;
}

export function originAllowed(origin: string | null, allowed: Config["origins"], requestOrigin: string): boolean {
  if (allowed === "same-origin") return origin === requestOrigin;
  if (allowed.includes("*")) return true;
  if (!origin) return false;
  const norm = (s: string) => s.replace(/\/+$/, "").toLowerCase();
  return allowed.some((a) => norm(a) === norm(origin));
}

export type Command =
  | { kind: "connect"; host: string; port: number }
  | { kind: "resolve"; name: string };

export function parseCommand(line: string): Command {
  const text = line.trim();
  const sp = text.indexOf(" ");
  const verb = sp < 0 ? text : text.slice(0, sp);
  const arg = sp < 0 ? "" : text.slice(sp + 1).trim();
  switch (verb) {
    case "CONNECT": {
      const colon = arg.lastIndexOf(":");
      if (colon <= 0) throw new Error("bad CONNECT syntax");
      let host = arg.slice(0, colon);
      const port = Number(arg.slice(colon + 1));
      if (host.startsWith("[") && host.endsWith("]")) host = host.slice(1, -1);
      if (!host || !/^\d+$/.test(arg.slice(colon + 1)) || port < 1 || port > 65535)
        throw new Error("bad CONNECT syntax");
      return { kind: "connect", host, port };
    }
    case "RESOLVE":
      if (!arg || /\s/.test(arg)) throw new Error("bad RESOLVE syntax");
      return { kind: "resolve", name: arg };
    default:
      throw new Error("unknown command");
  }
}

/** The guest's NAT gateway is the relay host itself. */
const GATEWAY_ALIAS = "10.0.2.2";

/** Only ordinary public unicast addresses are allowed by default. */
export function blocked(ip: string): boolean {
  return !ipaddr.isValid(ip) || ipaddr.process(ip).range() !== "unicast";
}

/** The gateway alias is exact; all other addresses use the destination policy. */
export function admitAddress(ip: string, allowPrivate: boolean): string | null {
  if (ip === GATEWAY_ALIAS) return allowPrivate ? "127.0.0.1" : null;
  if (!ipaddr.isValid(ip)) return null;
  const address = ipaddr.process(ip);
  if (!allowPrivate && address.range() !== "unicast") return null;
  return address.toString();
}

/** IPv4 addresses for name (or the literal itself), order-preserving, deduplicated. */
export async function resolveV4(name: string): Promise<string[]> {
  if (ipaddr.isValid(name)) return [name];
  try {
    const hits = await lookup(name, { family: 4, all: true });
    return [...new Set(hits.map((h) => h.address))];
  } catch {
    return [];
  }
}

export class Relay {
  private live = 0;
  private buffered = 0;
  private flows = new Set<Flow>();
  private peers = new Set<MultiplexPeer<Flow>>();
  private sweeper: ReturnType<typeof setInterval>;
  private pressureTimer: ReturnType<typeof setInterval>;

  constructor(
    public readonly cfg: Config,
    private limits: Limits,
  ) {
    // Vercel does not invoke the WebSocket drain callback.
    this.pressureTimer = setInterval(() => {
      for (const peer of this.peers) peer.drain();
    }, 25);
    this.sweeper = setInterval(() => this.sweepIdle(), Math.min(1_000, cfg.finIdleMs));
  }

  /** The options for Bun.serve(); the caller adds port/hostname. */
  serveOptions() {
    return {
      fetch: (req: Request, server: Server<MultiplexPeer<Flow>>) => this.fetch(req, server),
      websocket: {
        data: {} as MultiplexPeer<Flow>,
        // Bun's own idle timer would race ours; pings keep dead peers visible.
        idleTimeout: 960,
        maxPayloadLength: FLOW_BUFFER_LIMIT,
        backpressureLimit: FLOW_BUFFER_LIMIT,
        closeOnBackpressureLimit: true,
        open: (ws: ServerWebSocket<MultiplexPeer<Flow>>) => {
          ws.data.ws = ws;
          this.log(ws.data.ip, "multiplex", "open", "", 0, 0);
        },
        message: (ws: ServerWebSocket<MultiplexPeer<Flow>>, msg: string | Buffer) => ws.data.message(msg),
        drain: (ws: ServerWebSocket<MultiplexPeer<Flow>>) => ws.data.drain(),
        close: (ws: ServerWebSocket<MultiplexPeer<Flow>>) => {
          ws.data.stop();
          this.peers.delete(ws.data);
          this.log(ws.data.ip, "multiplex", "closed", "", 0, 0);
        },
      },
    };
  }

  stop() {
    clearInterval(this.sweeper);
    clearInterval(this.pressureTimer);
    for (const peer of this.peers) peer.stop();
    this.peers.clear();
    for (const flow of this.flows) this.finish(flow, "shutdown");
  }

  private clientIp(req: Request, server: Server<MultiplexPeer<Flow>>): string {
    if (this.cfg.trustProxy) {
      const fwd = req.headers.get("x-forwarded-for");
      const first = fwd?.split(",")[0]?.trim();
      if (first) return first;
    }
    return server.requestIP(req)?.address ?? "unknown";
  }

  private async fetch(req: Request, server: Server<MultiplexPeer<Flow>>): Promise<Response | undefined> {
    const url = new URL(req.url);
    if (req.headers.get("upgrade")?.toLowerCase() !== "websocket") {
      return new Response("emulate.computer relay\n", { status: url.pathname === "/" ? 200 : 404 });
    }
    if (url.pathname !== this.cfg.path) return new Response("unknown path\n", { status: 404 });
    if (!originAllowed(req.headers.get("origin"), this.cfg.origins, url.origin))
      return new Response("origin not allowed\n", { status: 403 });

    const ip = this.clientIp(req, server);
    if (this.peers.size >= this.cfg.maxConns || [...this.peers].filter(p => p.ip === ip).length >= 16)
      return new Response("too many sessions\n", { status: 429 });
    const peer = new MultiplexPeer<Flow>(ip, this.cfg.maxConns, {
      open: async (socket, command) => {
        const flow = await this.admitFlow(ip);
        if (!flow) {
          socket.sendText("ERR connection limit");
          socket.close();
          return;
        }
        socket.data = flow;
        if (socket.cancelled) { this.finish(flow, "cancelled before admission"); return; }
        flow.ws = socket;
        void this.command(flow, command);
      },
      message: (socket, message) => { if (socket.data) this.message(socket, message); },
      close: socket => this.finish(socket.data, "client closed"),
      pause: (socket, paused) => {
        socket.data.wsPaused = paused;
        this.updateReadPressure(socket.data);
      },
    });
    this.peers.add(peer);
    if (server.upgrade(req, { data: peer })) return;
    this.peers.delete(peer);
    return new Response("upgrade failed\n", { status: 400 });
  }

  private async admitFlow(ip: string): Promise<Flow | null> {
    let refusal;
    try { refusal = await this.limits.admit(ip); }
    catch { return null; }
    if (refusal) {
      this.log(ip, "-", "reject", `per-IP ${refusal.reason}`, 0, 0);
      return null;
    }
    if (this.live >= this.cfg.maxConns) {
      await this.limits.release(ip);
      this.log(ip, "-", "reject", "connection cap reached", 0, 0);
      return null;
    }

    const flow: Flow = {
      ip,
      peer: ip,
      started: Date.now(),
      state: "dialing",
      target: "-",
      ws: null,
      tcp: null,
      backlog: [],
      finAt: 0,
      bytes: 0,
      buffered: 0,
      pending: [],
      charging: false,
      quotaTimer: null,
      wsPaused: false,
      peerEnded: false,
      lastActive: Date.now(),
      why: null,
    };
    this.live++;
    this.flows.add(flow);
    return flow;
  }

  private message(ws: FlowSocket<Flow>, msg: string | Uint8Array) {
    const flow = ws.data;
    if (flow.state === "closed") return;
    flow.lastActive = Date.now();
    if (typeof msg === "string") {
      if (msg === "FIN") this.fin(flow);
    } else this.enqueue(flow, msg, true);
  }

  private async command(flow: Flow, line: string) {
    let cmd: Command;
    try {
      cmd = parseCommand(line);
    } catch (err) {
      this.sendText(flow, `ERR ${(err as Error).message}`);
      this.finish(flow, (err as Error).message);
      return;
    }
    flow.state = "dialing";
    if (cmd.kind === "resolve") {
      flow.target = `RESOLVE ${cmd.name}`;
      const addrs = (await resolveV4(cmd.name))
        .map((a) => admitAddress(a, this.cfg.allowPrivate))
        .filter((a): a is string => a !== null);
      if (flow.state !== "dialing") return;
      if (addrs.length === 0) {
        this.sendText(flow, "ERR nx");
        this.finish(flow, "nx");
      } else {
        this.sendText(flow, `IP ${[...new Set(addrs)].join(" ")}`);
        this.finish(flow, "ok");
      }
      return;
    }

    flow.target = `CONNECT ${cmd.host}:${cmd.port}`;
    flow.state = "dialing";
    if (this.cfg.ports.length > 0 && !this.cfg.ports.includes(cmd.port)) return this.refuse(flow, "port not allowed");
    const addr = (await resolveV4(cmd.host)).map((a) => admitAddress(a, this.cfg.allowPrivate)).find((a) => a);
    if (flow.state !== "dialing") return;
    if (!addr) return this.refuse(flow, ipaddr.isValid(cmd.host) || cmd.host === GATEWAY_ALIAS ? "destination not allowed" : "nx");
    flow.target = `CONNECT ${addr}:${cmd.port}`;

    const relay = this;
    let timer: ReturnType<typeof setTimeout> | null = null;
    try {
      const connecting = Bun.connect<Flow>({
        hostname: addr,
        port: cmd.port,
        data: flow,
        socket: {
          open(tcp) {
            if (timer) clearTimeout(timer);
            if (flow.state !== "dialing") {
              tcp.terminate();
              return;
            }
            flow.tcp = tcp;
            flow.state = "open";
            relay.sendText(flow, "OK");
            relay.drainBacklog(flow);
            relay.updateReadPressure(flow);
          },
          data(_tcp, chunk) {
            if (flow.state === "closed") return;
            flow.lastActive = Date.now();
            relay.enqueue(flow, chunk, false);
          },
          drain() {
            relay.drainBacklog(flow);
          },
          end() {
            flow.peerEnded = true;
            relay.completePeer(flow);
          },
          close() {
            flow.peerEnded = true;
            relay.completePeer(flow);
          },
          error(_tcp, err) {
            relay.finish(flow, `peer error: ${err.message}`);
          },
        },
      });
      await Promise.race([
        connecting,
        new Promise((_, reject) => {
          timer = setTimeout(() => reject(Object.assign(new Error("timeout"), { code: "ETIMEDOUT" })), CONNECT_TIMEOUT_MS);
        }),
      ]);
    } catch (err) {
      if (timer) clearTimeout(timer);
      if (flow.state === "dialing") this.refuse(flow, (err as { code?: string })?.code ?? "EUNKNOWN");
    }
  }

  private refuse(flow: Flow, reason: string) {
    this.sendText(flow, `ERR ${reason}`);
    this.finish(flow, reason);
  }

  private reserve(flow: Flow, n: number): boolean {
    if (flow.buffered + n > FLOW_BUFFER_LIMIT || this.buffered + n > RELAY_BUFFER_LIMIT
        || flow.pending.length + flow.backlog.length >= 1024) {
      this.finish(flow, "buffer limit");
      return false;
    }
    flow.buffered += n;
    this.buffered += n;
    return true;
  }

  private releaseBuffer(flow: Flow, n: number) {
    flow.buffered -= n;
    this.buffered -= n;
  }

  private updateReadPressure(flow: Flow) {
    if (flow.charging || flow.wsPaused) flow.tcp?.pause();
    else if (flow.state === "open") flow.tcp?.resume();
  }

  private enqueue(flow: Flow, bytes: Uint8Array, upload: boolean) {
    if (bytes.length === 0) return;
    flow.bytes += bytes.length;
    if (flow.bytes > this.cfg.maxBytes) return this.finish(flow, "byte cap");
    if (!this.reserve(flow, bytes.length)) return;
    flow.pending.push({ bytes: bytes.slice(), upload });
    this.drainQuota(flow);
  }

  private drainQuota(flow: Flow) {
    if (flow.charging || flow.state === "closed") return;
    while (flow.pending.length > 0) {
      const item = flow.pending[0];
      let verdict: boolean | Promise<boolean>;
      try {
        verdict = this.limits.charge(flow.ip, item.bytes.length);
      } catch {
        return this.finish(flow, "quota unavailable");
      }
      if (typeof verdict !== "boolean") {
        flow.charging = true;
        this.updateReadPressure(flow);
        flow.quotaTimer = setTimeout(() => this.finish(flow, "quota timeout"), QUOTA_TIMEOUT_MS);
        void verdict.then(ok => {
          if (flow.state === "closed") return;
          if (flow.quotaTimer) clearTimeout(flow.quotaTimer);
          flow.charging = false;
          if (!ok) return this.finish(flow, "byte quota");
          this.forwardCharged(flow);
          this.drainQuota(flow);
          this.updateReadPressure(flow);
        }, () => this.finish(flow, "quota unavailable"));
        return;
      }
      if (!verdict) return this.finish(flow, "byte quota");
      this.forwardCharged(flow);
      if (flow.why !== null) return;
    }
    this.completePeer(flow);
  }

  private forwardCharged(flow: Flow) {
    const item = flow.pending.shift()!;
    this.releaseBuffer(flow, item.bytes.length);
    if (item.upload) {
      if (flow.state === "dialing" || flow.backlog.length > 0) {
        if (this.reserve(flow, item.bytes.length)) flow.backlog.push(item.bytes);
      } else if (flow.state === "open") this.writeTcp(flow, item.bytes);
    } else {
      flow.ws?.sendBinary(item.bytes);
    }
  }

  private completePeer(flow: Flow) {
    if (!flow.peerEnded || flow.pending.length > 0 || flow.state === "closed") return;
    this.sendText(flow, "EOF");
    this.finish(flow, "peer eof");
  }

  private writeTcp(flow: Flow, bytes: Uint8Array) {
    const tcp = flow.tcp!;
    const wrote = tcp.write(bytes);
    if (wrote < 0) return this.finish(flow, "peer write failed");
    if (wrote > 0) flow.ws?.ack(wrote);
    if (wrote < bytes.byteLength && this.reserve(flow, bytes.byteLength - wrote))
      flow.backlog.unshift(bytes.slice(wrote));
  }

  private drainBacklog(flow: Flow) {
    while (flow.state === "open" && flow.backlog.length > 0) {
      const before = flow.backlog.length;
      const bytes = flow.backlog.shift()!;
      this.releaseBuffer(flow, bytes.length);
      this.writeTcp(flow, bytes);
      if (flow.backlog.length >= before) return; // still congested; wait for drain
    }
  }

  /**
   * The guest has nothing more to send. Bun cannot half-close a TCP socket
   * (shutdown/end reset it and drop the reply, see README), so the socket
   * stays open: the peer's own close becomes EOF, and a peer that goes quiet
   * after our FIN is treated as finished after finIdleMs.
   */
  private fin(flow: Flow) {
    if (!flow.finAt) flow.finAt = Date.now();
  }

  private sendText(flow: Flow, text: string) {
    flow.ws?.sendText(text);
  }

  private finish(flow: Flow, why: string) {
    if (flow.state === "closed") return;
    flow.state = "closed";
    flow.why = why;
    if (flow.quotaTimer) clearTimeout(flow.quotaTimer);
    this.releaseBuffer(flow, flow.buffered);
    flow.pending.length = 0;
    flow.backlog.length = 0;
    const tcp = flow.tcp;
    if (tcp) {
      tcp.end();
      setTimeout(() => tcp.terminate(), 5_000).unref?.();
    }
    flow.ws?.close(1000);
    this.flows.delete(flow);
    this.live--;
    void this.limits.release(flow.ip);
    const outcome = why === "ok" ? "ok" : why === "nx" ? "err" : "closed";
    this.log(flow.peer, flow.target, outcome, why === "ok" ? "" : why, flow.bytes, Date.now() - flow.started);
  }

  private sweepIdle() {
    const now = Date.now();
    for (const flow of this.flows) {
      if (flow.lastActive < now - this.cfg.idleTimeoutMs) this.finish(flow, "idle timeout");
      else if (flow.finAt && flow.state === "open" && flow.lastActive < now - this.cfg.finIdleMs) {
        this.sendText(flow, "EOF");
        this.finish(flow, "quiet after fin");
      }
    }
  }

  private log(peer: string, target: string, outcome: string, detail: string, bytes: number, ms: number) {
    if (this.cfg.quiet) return;
    const extra = detail ? ` (${detail})` : "";
    console.error(`[relay] ${peer} ${target} ${outcome}${extra} bytes=${bytes} ms=${ms}`);
  }
}
