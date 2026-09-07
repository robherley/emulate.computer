import { test } from "node:test";
import assert from "node:assert/strict";
import { ProcessorActivity } from "../src/processor-activity.ts";

test("brief CPU bursts stay quiet; sustained execution shows activity", () => {
  const activity = new ProcessorActivity();
  assert.equal(activity.update(90, 0), false);
  assert.equal(activity.update(90, 200), false);
  assert.equal(activity.update(5, 250), false);
  assert.equal(activity.update(90, 300), false);
  assert.equal(activity.update(90, 600), true);
});

test("activity survives brief pauses and clears after sustained idle", () => {
  const activity = new ProcessorActivity();
  activity.update(90, 0);
  activity.update(90, 300);
  assert.equal(activity.update(45, 400), true);
  assert.equal(activity.update(5, 500), true);
  assert.equal(activity.update(90, 700), true);
  assert.equal(activity.update(5, 800), true);
  assert.equal(activity.update(5, 1400), true);
  assert.equal(activity.update(5, 1500), false);
});

test("sample gaps and machine resets cannot carry over stale activity", () => {
  const activity = new ProcessorActivity();
  activity.update(90, 0);
  assert.equal(activity.update(90, 300), true);
  assert.equal(activity.update(90, 2000), false);
  assert.equal(activity.update(90, 2300), true);
  activity.reset();
  assert.equal(activity.update(90, 2400), false);
  assert.equal(activity.update(undefined, 2500), false);
  assert.equal(activity.update(NaN, 2600), false);
});
