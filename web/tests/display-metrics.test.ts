import {test} from 'node:test';
import assert from 'node:assert/strict';
import {DisplayMetrics} from '../src/display-metrics.ts';
test('frame intervals exclude idle gaps and resume without stale slow frames', () => {
  const metrics = new DisplayMetrics();
  metrics.painted(1000); metrics.painted(1100);
  assert.equal(metrics.sample(1101).displayFrameP95, 100);
  assert.equal(metrics.sample(1500).displayFrameP95, 0);
  metrics.painted(2000); metrics.painted(2016);
  assert.equal(metrics.sample(2017).displayFrameP95, 16);
});
test('capture and upload counters reset independently of frame history', () => {
  const metrics = new DisplayMetrics();
  metrics.copied(2.5); metrics.uploaded(1024); metrics.executed(10);
  const first = metrics.sample();
  assert.equal(first.displayCopyMs, 2.5);
  assert.ok(first.displayUploadBytesPerSec > 0);
  const second = metrics.sample();
  assert.equal(second.displayCopyMs, 0);
  assert.equal(second.displayUploadBytesPerSec, 0);
  metrics.reset();
  assert.equal(metrics.sample().displayFrameP95, 0);
});

test('input timing uses bounded samples and resets between machines', () => {
  const metrics = new DisplayMetrics();
  metrics.executed(100);
  assert.equal(metrics.sample().workerSliceP95, 0);
  metrics.inputReceived(1000);
  metrics.inputInjected(1000);
  for (let i = 0; i < 120; i++) {
    metrics.inputReceived(2);
    metrics.inputInjected(4);
    metrics.executed(1);
  }
  const sample = metrics.sample();
  assert.equal(sample.inputQueueP95, 2);
  assert.equal(sample.inputToGuestP95, 4);
  assert.equal(sample.workerSliceP95, 1);
  metrics.reset();
  assert.equal(metrics.sample().inputToGuestP95, 0);
});
