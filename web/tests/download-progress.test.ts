import { test, type TestContext } from "node:test";
import assert from "node:assert/strict";
import { createDownloadIndicator } from "../src/download-progress.ts";

const MB = 1024 * 1024;

function harness(t: TestContext) {
  t.mock.timers.enable({ apis: ["setTimeout"] });
  const writes: string[] = [];
  const indicator = createDownloadIndicator((text) => writes.push(text));
  return { writes, indicator, tick: (ms: number) => t.mock.timers.tick(ms) };
}

test("nothing renders when downloads finish within the show delay", (t) => {
  const { writes, indicator, tick } = harness(t);
  indicator.update("kernel.bin", "kernel.bin", 0, 4 * MB, false);
  indicator.update("kernel.bin", "kernel.bin", 4 * MB, 4 * MB, true);
  tick(1000);
  assert.deepEqual(writes, []);
});

test("a slow download renders progress and clears the line when done", (t) => {
  const { writes, indicator, tick } = harness(t);
  indicator.update("kernel.bin", "kernel.bin", MB, 4 * MB, false);
  tick(250);
  assert.equal(writes.length, 1);
  assert.match(writes[0], /downloading kernel\.bin: 1\.0\/4\.0 MB \(25%\)/);
  indicator.update("kernel.bin", "kernel.bin", 2 * MB, 4 * MB, false);
  tick(100);
  assert.match(writes[1], /2\.0\/4\.0 MB \(50%\)/);
  indicator.update("kernel.bin", "kernel.bin", 4 * MB, 4 * MB, true);
  assert.equal(writes.at(-1), "\r\x1b[K");
});

test("concurrent downloads aggregate under a shared label", (t) => {
  const { writes, indicator, tick } = harness(t);
  indicator.update("fw.bin", "fw.bin", MB, 2 * MB, false);
  indicator.update("kernel.bin", "kernel.bin", MB, 6 * MB, false);
  tick(250);
  assert.match(writes[0], /downloading guest images: 2\.0\/8\.0 MB \(25%\)/);
  indicator.update("fw.bin", "fw.bin", 2 * MB, 2 * MB, true);
  tick(100);
  assert.match(writes.at(-1)!, /downloading kernel\.bin: 3\.0\/8\.0 MB/);
});

test("a later download starts a fresh burst instead of inheriting old totals", (t) => {
  const { writes, indicator, tick } = harness(t);
  indicator.update("kernel.bin", "kernel.bin", MB, MB, true);
  indicator.update("disk-seed", "root filesystem", MB, 4 * MB, false);
  tick(250);
  assert.match(
    writes.at(-1)!,
    /downloading root filesystem: 1\.0\/4\.0 MB \(25%\)/,
  );
});

test("received bytes beyond the expected total stay clamped", (t) => {
  const { writes, indicator, tick } = harness(t);
  indicator.update("disk-seed", "root filesystem", 5 * MB, 4 * MB, false);
  tick(250);
  assert.match(writes.at(-1)!, /4\.0\/4\.0 MB \(100%\)/);
});

test("hide erases a visible line and cancels a pending show", (t) => {
  const { writes, indicator, tick } = harness(t);
  indicator.update("kernel.bin", "kernel.bin", MB, 4 * MB, false);
  indicator.hide();
  tick(1000);
  assert.deepEqual(writes, []);
  indicator.update("kernel.bin", "kernel.bin", 2 * MB, 4 * MB, false);
  tick(250);
  assert.equal(writes.length, 1);
  indicator.hide();
  assert.equal(writes.at(-1), "\r\x1b[K");
});
