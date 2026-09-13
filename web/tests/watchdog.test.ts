import { test } from "node:test";
import assert from "node:assert/strict";
import { RunLoopWatchdog } from "../src/session/watchdog.ts";

test("a progressing emulator stays healthy across watchdog checks", () => {
  const watchdog = new RunLoopWatchdog();
  watchdog.reset(0);
  for (const now of [1_000, 2_000, 3_000]) {
    watchdog.progressed(now - 50);
    assert.equal(watchdog.check(now), "healthy");
  }
});

test("a suspended worker resumes quietly regardless of callback order", () => {
  for (const iterationFirst of [false, true]) {
    const watchdog = new RunLoopWatchdog();
    watchdog.reset(0);
    assert.equal(watchdog.check(1_000), "healthy");
    if (iterationFirst) watchdog.progressed(60_000);
    assert.equal(watchdog.check(60_000), iterationFirst ? "healthy" : "resume");
    watchdog.progressed(60_050);
    assert.equal(watchdog.check(61_000), "healthy");
  }
});

test("regular watchdog checks still detect a lost emulator wake-up", () => {
  const watchdog = new RunLoopWatchdog();
  watchdog.reset(0);
  assert.equal(watchdog.check(1_000), "healthy");
  assert.equal(watchdog.check(2_000), "stalled");
  assert.equal(watchdog.check(3_000), "healthy");
  assert.equal(watchdog.check(4_000), "stalled");
});

test("quiet resume gives the loop time to run but does not hide a persistent stall", () => {
  const watchdog = new RunLoopWatchdog();
  watchdog.reset(0);
  assert.equal(watchdog.check(60_000), "resume");
  assert.equal(watchdog.check(61_000), "healthy");
  assert.equal(watchdog.check(62_000), "stalled");
  watchdog.reset(90_000);
  assert.equal(watchdog.check(91_000), "healthy");
});
