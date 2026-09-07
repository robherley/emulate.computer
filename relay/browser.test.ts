import { afterEach, expect, test } from "bun:test";
import { Relay } from "./relay.ts";
import { MemoryLimits } from "./limits.ts";
import { RelayBroker, relayUrl } from "../web/src/network/broker.ts";
import { GuestTransport, NetworkPort } from "../web/src/network/transport.ts";
import { MUX_WINDOW } from "./mux-protocol.ts";

const cleanup: (() => void)[] = [];
test("browser transport preserves the configured endpoint", () => {
  expect(relayUrl("ws://127.0.0.1:7654")).toBe("ws://127.0.0.1:7654/");
  expect(relayUrl("wss://emulate.computer/api/relay?example=1")).toBe("wss://emulate.computer/api/relay?example=1");
  expect(() => relayUrl("https://emulate.computer/api/relay")).toThrow();
});
afterEach(() => { while (cleanup.length) cleanup.pop()!(); });
async function until(condition: () => boolean) {
  const deadline = Date.now() + 3000;
  while (!condition()) {
    if (Date.now() > deadline) throw new Error("condition timed out");
    await Bun.sleep(5);
  }
}
function setup() {
  const relay = new Relay({ path: "/", origins: ["*"], allowPrivate: true, ports: [], maxBytes: 1e8,
    maxConns: 64, idleTimeoutMs: 30000, finIdleMs: 30000, trustProxy: false, quiet: true,
  }, new MemoryLimits({ maxPerIp: 0, bytesPerDay: 0 }));
  const server = Bun.serve({ hostname: "127.0.0.1", port: 0, ...relay.serveOptions() });
  cleanup.push(() => { relay.stop(); server.stop(true); });
  const sockets: WebSocket[] = [];
  const broker = new RelayBroker(relayUrl(`ws://127.0.0.1:${server.port}`), () => {}, url => {
    const socket = new WebSocket(url); sockets.push(socket); return socket;
  });
  cleanup.push(() => broker.stop());
  function guest() {
    const channel = new MessageChannel();
    const network = new NetworkPort(channel.port1, false);
    broker.attach(channel.port2);
    const transport = new GuestTransport(network);
    const pending: ReturnType<GuestTransport["poll"]> = [];
    cleanup.push(() => { transport.dispose(); network.dispose(); });
    async function take(kind: number, id: number, data?: string) {
      let index = -1;
      await until(() => {
        pending.push(...transport.poll());
        index = pending.findIndex(e => e[0] === kind && e[1] === id && (data === undefined || e[2] === data));
        return index >= 0;
      });
      return pending.splice(index, 1)[0];
    }
    return { network, transport, take };
  }
  return { guest, sockets, broker };
}

test("tabs share one socket with isolated IDs, guest disposal, and reconnect", async () => {
  const { guest, sockets } = setup();
  const a = guest(), b = guest();
  const first = a.transport.open(), second = b.transport.open();
  expect(first).toBe(second);
  await a.take(0, first); await b.take(0, second);
  // The second tab can send its command before the first tab is scheduled.
  b.transport.sendText(second, "RESOLVE 8.8.8.8");
  a.transport.sendText(first, "RESOLVE 1.1.1.1");
  await a.take(1, first, "IP 1.1.1.1");
  await b.take(1, second, "IP 8.8.8.8");
  expect(sockets.length).toBe(1);
  a.transport.dispose(); a.network.dispose();
  const next = b.transport.open(); await b.take(0, next);
  b.transport.sendText(next, "RESOLVE 9.9.9.9");
  await b.take(1, next, "IP 9.9.9.9");
  expect(sockets.length).toBe(1);
  const interrupted = b.transport.open(); await b.take(0, interrupted);
  sockets[0].close();
  await b.take(3, interrupted);
  await until(() => sockets.length === 2 && b.network.connected);
  const restored = b.transport.open(); await b.take(0, restored);
  b.transport.sendText(restored, "RESOLVE 1.0.0.1");
  await b.take(1, restored, "IP 1.0.0.1");
});

test("guest transport bounds writes and returns receive credit when polled", async () => {
  const { guest, sockets } = setup();
  const listener = Bun.listen({ hostname: "127.0.0.1", port: 0, socket: {
    data(socket, bytes) { socket.write(bytes); },
  } });
  cleanup.push(() => listener.stop(true));
  const a = guest();
  const id = a.transport.open(); await a.take(0, id);
  a.transport.sendText(id, `CONNECT 10.0.2.2:${listener.port}`);
  await a.take(1, id, "OK");
  const payload = new Uint8Array(MUX_WINDOW).fill(17);
  expect(a.transport.sendBinary(id, payload)).toBe(true);
  expect(a.transport.sendBinary(id, new Uint8Array([1]))).toBe(false);
  let received = 0;
  while (received < payload.length) received += (await a.take(2, id))[2]!.length;
  await until(() => a.transport.bufferedAmount(id) === 0);
  expect(a.transport.sendBinary(id, new Uint8Array([23]))).toBe(true);
  expect(Array.from((await a.take(2, id))[2] as Uint8Array)).toEqual([23]);
  expect(sockets.length).toBe(1);
});
