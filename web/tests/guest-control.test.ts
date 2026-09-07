import { test } from "node:test";
import assert from "node:assert/strict";
import { GuestControl, type GuestStatus } from "../src/session/guest-control.ts";
const encode = (value: string) => new TextEncoder().encode(value);

test("control reports survive arbitrary chunks and reject malformed lines", () => {
  const received: GuestStatus[] = [];
  const control = new GuestControl(() => {}, value => received.push(value));
  for (const byte of encode("0 desktop starting\n0 desktop ready\n0 network 10.0.2.15/24\n")) control.push(new Uint8Array([byte]));
  control.push(encode("0 desktop ready extra\n0 network 999.0.0.1/24\n0 network 1.2.3.4/33\n"));
  control.push(encode("x".repeat(1024) + "0 desktop failed\n0 network none\n"));
  assert.deepEqual(received, [
    { kind: "desktop", state: "starting" }, { kind: "desktop", state: "ready" },
    { kind: "network", address: "10.0.2.15/24" }, { kind: "network", address: null },
  ]);
});

test("requests wait for guest readiness and acknowledgements, with one handshake per session", async () => {
  const sent: string[] = [];
  let ready = 0;
  const control = new GuestControl(bytes => sent.push(new TextDecoder().decode(bytes)), () => {}, () => ready++);
  const request = control.request("desktop.start");
  assert.deepEqual(sent, []);
  control.push(encode("0 ready\n0 ready\n"));
  assert.deepEqual(sent, ["1 desktop.start\n"]);
  assert.equal(ready, 1);
  control.push(encode("1 ok\n"));
  await request;
  const failure = assert.rejects(control.request("network.connect"), /could not complete/);
  control.push(encode("2 error\n"));
  await failure;
});

test("restart rejects pending actions and discards stale replies and partial status", async () => {
  const statuses: GuestStatus[] = [];
  const control = new GuestControl(() => {}, value => statuses.push(value));
  const aborted = assert.rejects(control.request("desktop.start"), /session ended/);
  control.push(encode("0 desktop "));
  control.reset();
  await aborted;
  const next = control.request("status");
  control.push(encode("ready\n0 ready\n1 ok\n0 desktop stopped\n2 ok\n"));
  await next;
  assert.deepEqual(statuses, [{ kind: "desktop", state: "stopped" }]);
});

test("unresponsive guests time out and cannot accumulate unlimited requests", async () => {
  const control = new GuestControl(() => {}, () => {}, () => {}, 10);
  const pending = Array.from({ length: 16 }, () => assert.rejects(control.request("status"), /did not respond/));
  await assert.rejects(control.request("status"), /Too many/);
  await Promise.all(pending);
});
