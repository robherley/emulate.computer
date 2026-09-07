import { afterEach, describe, expect, test } from "bun:test";
import { encodeData, decodeData, MUX_FRAME, MUX_WINDOW } from "./mux-protocol.ts";
import { MemoryLimits, type Limits } from "./limits.ts";
import { Relay, admitAddress, blocked, parseCommand, type Config } from "./relay.ts";

const cleanups: (() => void)[] = [];
afterEach(() => {
  while (cleanups.length) cleanups.pop()!();
});

function devConfig(over: Partial<Config> = {}): Config {
  return {
    path: "/",
    origins: ["*"],
    allowPrivate: true,
    ports: [],
    maxBytes: 512 * 1024 * 1024,
    maxConns: 64,
    idleTimeoutMs: 30_000,
    finIdleMs: 30_000,
    trustProxy: false,
    quiet: true,
    ...over,
  };
}

function startRelay(cfg = devConfig(), limits: Limits = new MemoryLimits({ maxPerIp: 0, bytesPerDay: 0 })) {
  const relay = new Relay(cfg, limits);
  const server = Bun.serve({ hostname: "127.0.0.1", port: 0, ...relay.serveOptions() });
  cleanups.push(() => {
    relay.stop();
    server.stop(true);
  });
  return `ws://127.0.0.1:${server.port}`;
}

/**
 * TCP echo that honours backpressure and closes after echoing a chunk whose
 * last byte is 0x04 (EOT). The relay cannot forward FIN as a half-close (see
 * relay.ts), so the peer has to decide when it is done.
 */
const EOT = 0x04;
function startEcho(): number {
  type Echo = { pending: Uint8Array[]; endPending: boolean };
  const flush = (socket: import("bun").Socket<Echo>) => {
    while (socket.data.pending.length > 0) {
      const chunk = socket.data.pending[0];
      const wrote = socket.write(chunk);
      if (wrote < chunk.byteLength) {
        socket.data.pending[0] = chunk.slice(Math.max(wrote, 0));
        return;
      }
      socket.data.pending.shift();
    }
    if (socket.data.endPending) socket.end();
  };
  const listener = Bun.listen<Echo>({
    hostname: "127.0.0.1",
    port: 0,
    socket: {
      open(socket) {
        socket.data = { pending: [], endPending: false };
      },
      data(socket, chunk) {
        socket.data.pending.push(new Uint8Array(chunk));
        if (chunk[chunk.byteLength - 1] === EOT) socket.data.endPending = true;
        flush(socket);
      },
      drain(socket) {
        flush(socket);
      },
    },
  });
  cleanups.push(() => listener.stop(true));
  return listener.port;
}

/** A TCP server that closes as soon as the client connects. */
function startSlammer(): number {
  const listener = Bun.listen({
    hostname: "127.0.0.1",
    port: 0,
    socket: {
      open(socket) {
        socket.end();
      },
      data() {},
    },
  });
  cleanups.push(() => listener.stop(true));
  return listener.port;
}

type Frame = { text?: string; bin?: Uint8Array; closed?: true };

class Client {
  ws: WebSocket;
  private queue: Frame[] = [];
  private waiters: ((f: Frame) => void)[] = [];
  opened: Promise<void>;
  private available = MUX_WINDOW;
  private outgoing: (string | Uint8Array)[] = [];
  private closed = false;

  constructor(url: string, headers: Record<string, string> = {}) {
    // Bun's WebSocket accepts extra handshake headers.
    this.ws = new WebSocket(url, { headers } as never);
    this.ws.binaryType = "arraybuffer";
    this.opened = new Promise((resolve, reject) => {
      this.ws.addEventListener("open", () => resolve());
      this.ws.addEventListener("error", () => reject(new Error("handshake failed")));
    });
    this.ws.addEventListener("message", (e) => {
      const data = e.data as string | ArrayBuffer;
      if (typeof data !== "string") this.push({ bin: decodeData(new Uint8Array(data)).data });
      else if (data.startsWith("1 ACK ")) { this.available += Number(data.slice(6)); this.drain(); }
      else if (data === "1 CLOSE") this.finish();
      else this.push({ text: data.slice(2) });
    });
    this.ws.addEventListener("close", () => this.finish());
    cleanups.push(() => this.ws.close());
  }

  private push(f: Frame) {
    const w = this.waiters.shift();
    if (w) w(f);
    else this.queue.push(f);
  }

  private finish() {
    if (this.closed) return;
    this.closed = true;
    this.outgoing.length = 0;
    this.push({ closed: true });
    this.ws.close();
  }

  private consumed(frame: Frame): Frame {
    if (frame.bin && !this.closed) this.ws.send(`1 WINDOW ${frame.bin.length}`);
    return frame;
  }

  next(timeoutMs = 5000): Promise<Frame> {
    const q = this.queue.shift();
    if (q) return Promise.resolve(this.consumed(q));
    return new Promise((resolve, reject) => {
      const t = setTimeout(() => reject(new Error("timed out waiting for a frame")), timeoutMs);
      this.waiters.push((f) => {
        clearTimeout(t);
        resolve(this.consumed(f));
      });
    });
  }

  async text(): Promise<string> {
    const f = await this.next();
    if (f.text === undefined) throw new Error(`expected text, got ${JSON.stringify(f)}`);
    return f.text;
  }

  send(x: string | Uint8Array) {
    this.outgoing.push(x);
    this.drain();
  }

  private drain() {
    while (this.outgoing.length && !this.closed) {
      const message = this.outgoing[0];
      if (typeof message === "string") {
        this.ws.send(`1 ${message}`);
        this.outgoing.shift();
      } else {
        const count = Math.min(message.length, MUX_FRAME, this.available);
        if (!count) return;
        this.ws.send(encodeData(1, message.subarray(0, count)));
        this.available -= count;
        if (count === message.length) this.outgoing.shift();
        else this.outgoing[0] = message.subarray(count);
      }
    }
  }
}

async function open(base: string, query = "", headers: Record<string, string> = {}) {
  const c = new Client(`${base}/${query}`, headers);
  await c.opened;
  return c;
}

/** "ok" when a real WebSocket handshake succeeds, else the HTTP status it was refused with. */
async function handshake(base: string, path = "/", headers: Record<string, string> = {}): Promise<"ok" | number> {
  const opened = await new Promise<boolean>((resolve) => {
    const ws = new WebSocket(`${base}${path || "/"}`, { headers } as never);
    ws.addEventListener("open", () => {
      ws.close();
      resolve(true);
    });
    ws.addEventListener("error", () => resolve(false));
  });
  if (opened) return "ok";
  const res = await fetch(`${base.replace("ws", "http")}${path || "/"}`, {
    headers: {
      Connection: "Upgrade",
      Upgrade: "websocket",
      "Sec-WebSocket-Version": "13",
      "Sec-WebSocket-Key": "dGhlIHNhbXBsZSBub25jZQ==",
      ...headers,
    },
  });
  return res.status;
}

async function connectEcho(base: string, echo: number, query = "") {
  const c = await open(base, query);
  c.send(`CONNECT 127.0.0.1:${echo}`);
  expect(await c.text()).toBe("OK");
  return c;
}

describe("protocol", () => {
  test("echo both ways, peer close becomes EOF", async () => {
    const echo = startEcho();
    const c = await connectEcho(startRelay(), echo);
    c.send(new TextEncoder().encode("hello"));
    const f = await c.next();
    expect(new TextDecoder().decode(f.bin)).toBe("hello");
    c.send(new Uint8Array([EOT]));
    expect((await c.next()).bin).toEqual(new Uint8Array([EOT]));
    expect(await c.text()).toBe("EOF");
    expect((await c.next()).closed).toBe(true);
  });

  test("FIN with a quiet peer ends the flow with EOF after finIdleMs", async () => {
    const echo = startEcho();
    const c = await connectEcho(startRelay(devConfig({ finIdleMs: 100 })), echo);
    c.send(new TextEncoder().encode("hello"));
    expect((await c.next()).bin?.length).toBe(5);
    c.send("FIN");
    expect(await c.text()).toBe("EOF");
    expect((await c.next()).closed).toBe(true);
  });

  test("a 512 KiB burst arrives byte-identical", async () => {
    const echo = startEcho();
    const c = await connectEcho(startRelay(), echo);
    const payload = new Uint8Array(512 * 1024);
    for (let i = 0; i < payload.length; i++) payload[i] = (i * 7) & 0xff;
    payload[payload.length - 1] = EOT;
    c.send(payload);
    c.send("FIN");
    const got: Uint8Array[] = [];
    let total = 0;
    while (total < payload.length) {
      const f = await c.next();
      if (!f.bin) throw new Error(`unexpected frame ${JSON.stringify(f)}`);
      got.push(f.bin);
      total += f.bin.length;
    }
    expect(Buffer.concat(got).equals(Buffer.from(payload))).toBe(true);
    expect(await c.text()).toBe("EOF");
  });

  test("peer closing first yields EOF", async () => {
    const c = await open(startRelay());
    c.send(`CONNECT 127.0.0.1:${startSlammer()}`);
    expect(await c.text()).toBe("OK");
    expect(await c.text()).toBe("EOF");
  });

  test("connection failures preserve the socket error code", async () => {
    const port = startEcho();
    cleanups.pop()!(); // stop it so the port is dead
    const c = await open(startRelay());
    c.send(`CONNECT 127.0.0.1:${port}`);
    expect(await c.text()).toBe("ERR ECONNREFUSED");
  });

  test("RESOLVE", async () => {
    const base = startRelay(devConfig({ allowPrivate: false }));
    let c = await open(base);
    c.send("RESOLVE localhost");
    expect(await c.text()).toBe("ERR nx"); // loopback is private
    c = await open(base);
    c.send("RESOLVE 10.0.2.2");
    expect(await c.text()).toBe("ERR nx"); // gateway follows the same policy
    c = await open(base);
    c.send("RESOLVE no-such-host.invalid");
    expect(await c.text()).toBe("ERR nx");
    c = await open(startRelay());
    c.send("RESOLVE localhost");
    expect((await c.text()).startsWith("IP 127.0.0.1")).toBe(true);
  });

  test("bad commands", async () => {
    const base = startRelay();
    let c = await open(base);
    c.ws.send("FROB x");
    expect(await c.next()).toEqual({ closed: true });
    c = await open(base);
    c.ws.send(new Uint8Array([1, 2, 3]));
    expect(await c.next()).toEqual({ closed: true });
    c = await open(base);
    c.send("CONNECT nohost");
    expect(await c.text()).toBe("ERR bad CONNECT syntax");
  });
});

describe("guardrails", () => {
  test("private destinations and gateway require explicit opt-in", async () => {
    const echo = startEcho();
    const strict = startRelay(devConfig({ allowPrivate: false }));
    for (const host of ["10.0.2.2", "127.0.0.1", "10.0.2.3", "192.168.1.1", "172.16.0.1", "100.64.0.1", "::ffff:7f00:1", "0:0:0:0:0:0:0:1"]) {
      const c = await open(strict);
      c.send(`CONNECT ${host}:${echo}`);
      expect(await c.text()).toBe("ERR destination not allowed");
    }
    const c = await open(startRelay());
    c.send(`CONNECT 10.0.2.2:${echo}`);
    expect(await c.text()).toBe("OK");
  });

  test("origin allowlist", async () => {
    const base = startRelay(devConfig({ origins: ["https://good.example"] }));
    expect(await handshake(base, "", { Origin: "https://good.example" })).toBe("ok");
    expect(await handshake(base, "", { Origin: "https://good.example/" })).toBe("ok");
    expect(await handshake(base, "", { Origin: "https://evil.example" })).toBe(403);
    expect(await handshake(base)).toBe(403);
    expect(await handshake(startRelay())).toBe("ok"); // wildcard
  });

  test("same-origin policy is enforced by the relay itself", async () => {
    const base = startRelay(devConfig({ origins: "same-origin" }));
    const origin = base.replace("ws:", "http:");
    for (const path of ["/"]) {
      expect(await handshake(base, path, { Origin: origin })).toBe("ok");
      for (const foreign of ["null", "https://evil.example", origin + "/", origin.replace("http:", "https:"), "http://localhost:" + new URL(origin).port])
        expect(await handshake(base, path, { Origin: foreign })).toBe(403);
      expect(await handshake(base, path)).toBe(403);
    }
  });

  test("unknown endpoint", async () => {
    for (const path of ["/nowhere", "/t", "/m", "/m?v=1", "/other/m"])
      expect(await handshake(startRelay(), path)).toBe(404);
  });

  test("the deployed path works directly without a rewrite or version parameter", async () => {
    const base = startRelay(devConfig({ path: "/api/relay", origins: "same-origin" }));
    const headers = { Origin: base.replace("ws:", "http:") };
    expect(await handshake(base, "/api/relay", headers)).toBe("ok");
    for (const path of ["/", "/m", "/api/relay/m"])
      expect(await handshake(base, path, headers)).toBe(404);
    expect(await handshake(base, "/api/relay", { Origin: "https://evil.example" })).toBe(403);
  });

  test("port allowlist", async () => {
    const echo = startEcho();
    const c = await open(startRelay(devConfig({ ports: [echo + 1] })));
    c.send(`CONNECT 127.0.0.1:${echo}`);
    expect(await c.text()).toBe("ERR port not allowed");
    await connectEcho(startRelay(devConfig({ ports: [echo] })), echo);
  });

  test("per-flow byte cap closes the flow", async () => {
    const echo = startEcho();
    const c = await connectEcho(startRelay(devConfig({ maxBytes: 1000 })), echo);
    c.send(new Uint8Array(600)); // 600 out + 600 echoed back > 1000
    let echoed = 0;
    for (;;) {
      const f = await c.next();
      if (f.closed) break;
      if (f.bin) echoed += f.bin.length;
    }
    expect(echoed).toBeLessThan(600);
  });

  test("max-conns", async () => {
    const echo = startEcho();
    const base = startRelay(devConfig({ maxConns: 1 }));
    await connectEcho(base, echo);
    expect(await handshake(base)).toBe(429);
  });
});

describe("per-IP limits", () => {
  test("concurrent flows per IP trip and release", async () => {
    const echo = startEcho();
    const base = startRelay(devConfig(), new MemoryLimits({ maxPerIp: 2, bytesPerDay: 0 }));
    const a = await connectEcho(base, echo);
    await connectEcho(base, echo);
    const refused = await open(base);
    refused.send(`CONNECT 127.0.0.1:${echo}`);
    expect(await refused.text()).toBe("ERR connection limit");
    expect(await refused.next()).toEqual({ closed: true });
    a.ws.close();
    await a.next();
    for (let i = 0; i < 50; i++) {
      const next = await open(base);
      next.send(`CONNECT 127.0.0.1:${echo}`);
      if (await next.text() === "OK") return;
      await next.next();
      await Bun.sleep(20);
    }
    throw new Error("slot was never released");
  });

  test("byte quota closes the flow, refuses new channels, then rolls", async () => {
    const echo = startEcho();
    const limits = new MemoryLimits({ maxPerIp: 0, bytesPerDay: 1000 });
    const clock = { at: 1_700_000_000 };
    limits.now = () => clock.at;
    const base = startRelay(devConfig(), limits);
    const c = await connectEcho(base, echo);
    c.send(new Uint8Array(600));
    let echoed = 0;
    for (;;) {
      const f = await c.next();
      if (f.closed) break;
      if (f.bin) echoed += f.bin.length;
    }
    expect(echoed).toBeLessThan(600);
    const refused = await open(base);
    refused.send(`CONNECT 127.0.0.1:${echo}`);
    expect(await refused.text()).toBe("ERR connection limit");
    expect(await refused.next()).toEqual({ closed: true });
    clock.at += 86400;
    await connectEcho(base, echo);
  });
});

describe("units", () => {
  test("parseCommand", () => {
    expect(parseCommand("CONNECT example.com:443")).toEqual({ kind: "connect", host: "example.com", port: 443 });
    expect(parseCommand("CONNECT [::1]:80")).toEqual({ kind: "connect", host: "::1", port: 80 });
    expect(parseCommand("RESOLVE example.com ")).toEqual({ kind: "resolve", name: "example.com" });
    for (const bad of ["CONNECT", "CONNECT host", "CONNECT host:0", "CONNECT host:70000", "CONNECT :80", "RESOLVE", "RESOLVE a b", "PING"])
      expect(() => parseCommand(bad)).toThrow();
  });

  test("blocked", () => {
    for (const ip of ["0.0.0.0", "10.1.2.3", "100.64.0.1", "127.0.0.1", "169.254.1.1", "172.16.0.1", "172.31.255.255", "192.0.0.1", "192.0.2.1", "192.168.0.1", "198.18.0.1", "198.19.255.255", "198.51.100.1", "203.0.113.1", "224.0.0.1", "240.0.0.1", "255.255.255.255", "::1", "::", "fc00::1", "fd12::1", "fe80::1", "ff02::1", "::ffff:10.0.0.1"])
      expect(blocked(ip)).toBe(true);
    for (const ip of ["1.1.1.1", "8.8.8.8", "93.184.216.34", "172.32.0.1", "100.128.0.1", "2606:4700::1111"]) expect(blocked(ip)).toBe(false);
  });

  test("address policy handles alternate spellings and invalid input", () => {
    for (const ip of [
      "::ffff:7f00:1", "::FFFF:c0a8:101", "0:0:0:0:0:0:0:1",
      "0:0:0:0:0:0:0:0", "2001:db8::1", "2002:7f00:1::", "64:ff9b::7f00:1",
      "127.1", "0x7f000001", "2130706433", "::ffff:a00:202",
    ]) {
      expect(blocked(ip)).toBe(true);
      expect(admitAddress(ip, false)).toBeNull();
    }
    for (const ip of ["", "invalid", "999.1.2.3", "1.2.3.4.5", "1::2::3", "::ffff:999.1.2.3"]) {
      expect(blocked(ip)).toBe(true);
      expect(admitAddress(ip, false)).toBeNull();
      expect(admitAddress(ip, true)).toBeNull();
    }
    expect(admitAddress("::ffff:808:808", false)).toBe("8.8.8.8");
    expect(admitAddress("::ffff:7f00:1", true)).toBe("127.0.0.1");
    expect(admitAddress("0:0:0:0:0:0:0:1", true)).toBe("::1");
  });

  test("admitAddress", () => {
    expect(admitAddress("10.0.2.2", false)).toBeNull();
    expect(admitAddress("10.0.2.2", true)).toBe("127.0.0.1");
    expect(admitAddress("10.0.2.3", false)).toBeNull();
    expect(admitAddress("10.0.2.3", true)).toBe("10.0.2.3");
    expect(admitAddress("1.1.1.1", false)).toBe("1.1.1.1");
  });

});

async function mux(base: string) {
  const ws = new WebSocket(`${base}/`);
  ws.binaryType = "arraybuffer";
  const messages: (string | ArrayBuffer)[] = [];
  ws.onmessage = event => messages.push(event.data);
  await new Promise<void>((resolve, reject) => { ws.onopen = () => resolve(); ws.onerror = reject; });
  cleanups.push(() => ws.close());
  const take = async (matches: (message: string | ArrayBuffer) => boolean) => {
    const deadline = Date.now() + 3000;
    while (Date.now() < deadline) {
      const index = messages.findIndex(matches);
      if (index >= 0) return messages.splice(index, 1)[0];
      await Bun.sleep(5);
    }
    throw new Error(`missing multiplex response; queued: ${messages.filter(m => typeof m === "string")}`);
  };
  return { ws, take, messages };
}

describe("multiplexed relay", () => {
  test("TCP and DNS share one socket and closing a channel leaves the others usable", async () => {
    const base = startRelay();
    const port = startEcho();
    const { ws, take } = await mux(base);
    ws.send(`1 CONNECT 10.0.2.2:${port}`);
    ws.send(`2 CONNECT 10.0.2.2:${port}`);
    ws.send("3 RESOLVE 8.8.8.8");
    await take(m => m === "1 OK");
    await take(m => m === "2 OK");
    await take(m => m === "3 IP 8.8.8.8");
    await take(m => m === "3 CLOSE");
    ws.send(encodeData(1, new Uint8Array([11])));
    ws.send(encodeData(2, new Uint8Array([22])));
    const data = (id: number) => (m: string | ArrayBuffer) => m instanceof ArrayBuffer && decodeData(new Uint8Array(m)).id === id;
    expect(decodeData(new Uint8Array(await take(data(1)) as ArrayBuffer)).data[0]).toBe(11);
    expect(decodeData(new Uint8Array(await take(data(2)) as ArrayBuffer)).data[0]).toBe(22);
    await take(m => m === "1 ACK 1");
    ws.send("1 CLOSE");
    await take(m => m === "1 CLOSE");
    ws.send(encodeData(2, new Uint8Array([33])));
    expect(decodeData(new Uint8Array(await take(data(2)) as ArrayBuffer)).data[0]).toBe(33);
    ws.send("PING");
    await take(m => m === "PONG");
    expect(ws.readyState).toBe(WebSocket.OPEN);
  });

  test("flow limits apply to channels and released slots can be reused", async () => {
    const limits = new MemoryLimits({ maxPerIp: 1, bytesPerDay: 0 });
    const { ws, take } = await mux(startRelay(devConfig(), limits));
    ws.send(`1 CONNECT 10.0.2.2:${startEcho()}`);
    await take(m => m === "1 OK");
    ws.send("2 RESOLVE 8.8.8.8");
    await take(m => m === "2 ERR connection limit");
    await take(m => m === "2 CLOSE");
    ws.send("1 CLOSE");
    await take(m => m === "1 CLOSE");
    ws.send("3 RESOLVE 8.8.8.8");
    await take(m => m === "3 IP 8.8.8.8");
  });

  test("a stalled receive window does not stall another channel and preserves trailing EOF", async () => {
    const payload = new Uint8Array(MUX_WINDOW * 2).fill(42);
    const flush = (socket: import("bun").Socket<number>) => {
      const written = socket.write(payload.subarray(socket.data));
      socket.data += Math.max(0, written);
      if (socket.data === payload.length) socket.end();
    };
    const listener = Bun.listen<number>({ hostname: "127.0.0.1", port: 0, socket: {
      open(socket) { socket.data = 0; flush(socket); }, data() {}, drain: flush,
    } });
    cleanups.push(() => listener.stop(true));
    const { ws, take, messages } = await mux(startRelay());
    ws.send(`1 CONNECT 10.0.2.2:${listener.port}`);
    await take(m => m === "1 OK");
    let received = 0;
    const data = (m: string | ArrayBuffer) => m instanceof ArrayBuffer;
    while (received < MUX_WINDOW) received += decodeData(new Uint8Array(await take(data) as ArrayBuffer)).data.length;
    await Bun.sleep(30);
    expect(messages.filter(data).length).toBe(0);
    ws.send("2 RESOLVE 8.8.8.8");
    await take(m => m === "2 IP 8.8.8.8");
    ws.send(`1 WINDOW ${MUX_WINDOW}`);
    while (received < payload.length) received += decodeData(new Uint8Array(await take(data) as ArrayBuffer)).data.length;
    ws.send(`1 WINDOW ${MUX_WINDOW}`);
    await take(m => m === "1 EOF");
    await take(m => m === "1 CLOSE");
    expect(received).toBe(payload.length);
  });

  test("closing during asynchronous admission releases its flow slot", async () => {
    let releaseAdmission!: () => void;
    let admitted!: () => void;
    const started = new Promise<void>(resolve => { admitted = resolve; });
    let releases = 0;
    const limits: Limits = {
      admit: async () => { admitted(); await new Promise<void>(resolve => { releaseAdmission = resolve; }); return null; },
      release: async () => { releases++; }, charge: () => true,
    };
    const { ws, take } = await mux(startRelay(devConfig(), limits));
    ws.send("1 RESOLVE 8.8.8.8");
    await started;
    ws.send("1 CLOSE");
    await take(m => m === "1 CLOSE");
    releaseAdmission();
    await Bun.sleep(30);
    expect(releases).toBe(1);
  });
});

describe("asynchronous quota enforcement", () => {
  function delayedLimits() {
    const checks: { bytes: number; resolve(ok: boolean): void }[] = [];
    let releases = 0;
    const limits: Limits = {
      admit: async () => null,
      release: async () => { releases++; },
      charge: (_ip, bytes) => new Promise<boolean>(resolve => checks.push({ bytes, resolve })),
    };
    return { checks, limits, get releases() { return releases; } };
  }
  async function until(predicate: () => boolean) {
    for (let i = 0; i < 100; i++) {
      if (predicate()) return;
      await Bun.sleep(5);
    }
    throw new Error("condition did not become true");
  }

  test("upload waits for approval and rejected bytes never reach TCP", async () => {
    let received = 0;
    const listener = Bun.listen({ hostname: "127.0.0.1", port: 0, socket: {
      data(_socket, bytes) { received += bytes.length; },
    } });
    cleanups.push(() => listener.stop(true));
    const h = delayedLimits();
    const c = await connectEcho(startRelay(devConfig(), h.limits), listener.port);
    for (let i = 0; i < 4; i++) c.send(new Uint8Array(65536));
    await until(() => h.checks.length > 0);
    await Bun.sleep(20);
    expect(h.checks.length).toBe(1);
    expect(received).toBe(0);
    h.checks[0].resolve(false);
    expect(await c.next()).toEqual({ closed: true });
    expect(received).toBe(0);
    expect(h.releases).toBe(1);
  });

  test("download and EOF wait for quota approval in order", async () => {
    const h = delayedLimits();
    const listener = Bun.listen({ hostname: "127.0.0.1", port: 0, socket: {
      open(socket) { socket.write("hello"); socket.end(); }, data() {},
    } });
    cleanups.push(() => listener.stop(true));
    const c = await connectEcho(startRelay(devConfig(), h.limits), listener.port);
    await until(() => h.checks.length > 0);
    expect(h.checks[0].bytes).toBe(5);
    h.checks[0].resolve(true);
    expect(new TextDecoder().decode((await c.next()).bin)).toBe("hello");
    expect(await c.text()).toBe("EOF");
  });

  test("pending uploads are bounded and closing ignores a late quota approval", async () => {
    const h = delayedLimits();
    const base = startRelay(devConfig(), h.limits);
    const c = await connectEcho(base, startEcho());
    for (let i = 0; i < 5; i++) c.ws.send(encodeData(1, new Uint8Array(MUX_FRAME)));
    expect(await c.next()).toEqual({ closed: true });
    expect(h.checks.length).toBe(1);
    expect(h.releases).toBe(1);
    h.checks[0].resolve(true);
    await Bun.sleep(10);
    expect(h.releases).toBe(1);
    // Closing the rejected flow releases its queued byte budget.
    const next = await connectEcho(base, startEcho());
    next.send(new Uint8Array([1]));
    await until(() => h.checks.length === 2);
    h.checks[1].resolve(false);
    expect(await next.next()).toEqual({ closed: true });
  });

  test("aggregate pending traffic is bounded across flows", async () => {
    const h = delayedLimits();
    const base = startRelay(devConfig(), h.limits);
    const port = startEcho();
    const { ws, take } = await mux(base);
    for (let id = 1; id <= 32; id++) {
      ws.send(`${id} CONNECT 127.0.0.1:${port}`);
      await take(m => m === `${id} OK`);
      for (let i = 0; i < 4; i++) ws.send(encodeData(id, new Uint8Array(MUX_FRAME)));
      await until(() => h.checks.length === id);
    }
    ws.send(`33 CONNECT 127.0.0.1:${port}`);
    await take(m => m === "33 OK");
    ws.send(encodeData(33, new Uint8Array([1])));
    await take(m => m === "33 CLOSE");
    expect(h.checks.length).toBe(32);
    for (const check of h.checks) check.resolve(false);
  });
});

test("upload credit bounds traffic when the TCP peer stops reading", async () => {
  const listener = Bun.listen({ hostname: "127.0.0.1", port: 0, socket: {
    open(socket) { socket.pause(); }, data() {},
  } });
  cleanups.push(() => listener.stop(true));
  const c = await connectEcho(startRelay(), listener.port);
  for (let i = 0; i < 128; i++) c.ws.send(encodeData(1, new Uint8Array(MUX_FRAME)));
  expect(await c.next()).toEqual({ closed: true });
});

test("a rejected quota-store promise closes the flow without forwarding", async () => {
  const limits: Limits = {
    admit: async () => null,
    release: async () => {},
    charge: async () => { throw new Error("store unavailable"); },
  };
  const c = await connectEcho(startRelay(devConfig(), limits), startEcho());
  c.send(new Uint8Array([42]));
  expect(await c.next()).toEqual({ closed: true });
});

test("multiplexed output resumes when buffers clear without a drain callback", async () => {
  const relay = new Relay(devConfig(), new MemoryLimits({ maxPerIp: 0, bytesPerDay: 0 }));
  cleanups.push(() => relay.stop());
  const options = relay.serveOptions();
  type Socket = Parameters<typeof options.websocket.open>[0];
  const sent: string[] = [];
  let buffered = 1024 * 1024;
  const socket = {
    data: undefined as unknown as Socket["data"],
    getBufferedAmount: () => buffered,
    sendText: (text: string) => { sent.push(text); return text.length; },
    sendBinary: () => 1,
    close() {},
  } as unknown as Socket;
  const server = {
    requestIP: () => ({ address: "127.0.0.1" }),
    upgrade: (_: Request, value: { data: Socket["data"] }) => { socket.data = value.data; return true; },
  } as unknown as Parameters<typeof options.fetch>[1];
  await options.fetch(new Request("http://localhost/", { headers: { upgrade: "websocket" } }), server);
  options.websocket.open(socket);
  options.websocket.message(socket, "1 RESOLVE 1.1.1.1");
  await Bun.sleep(60);
  expect(sent).toEqual([]);
  buffered = 0;
  const deadline = Date.now() + 1000;
  while (!sent.includes("1 CLOSE") && Date.now() < deadline) await Bun.sleep(5);
  expect(sent).toEqual(["1 IP 1.1.1.1", "1 CLOSE"]);
});
