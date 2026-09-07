import { test } from 'node:test';
import assert from 'node:assert/strict';
import { drawSparkline, HISTORY, SPARK_HEIGHT } from '../src/stats.ts';

function plot(samples: number[]): number[] {
  const y: number[] = [];
  const context = new Proxy({}, {
    get: (_target, name) => (...args: number[]) => {
      if (name === 'moveTo') y.push(args[1]);
      if (name === 'bezierCurveTo') y.push(args[1], args[3], args[5]);
    },
    set: () => true,
  });
  const canvas = {
    clientWidth: 120,
    width: 120,
    height: SPARK_HEIGHT,
    getContext: () => context,
  } as unknown as HTMLCanvasElement;
  const previous = Object.getOwnPropertyDescriptor(globalThis, 'window');
  Object.defineProperty(globalThis, 'window', { value: { devicePixelRatio: 1 }, configurable: true });
  try {
    drawSparkline(canvas, samples);
  } finally {
    if (previous) Object.defineProperty(globalThis, 'window', previous);
    else Reflect.deleteProperty(globalThis, 'window');
  }
  return y;
}

test('idle sparklines stay flat at the baseline', () => {
  const y = plot(Array(HISTORY).fill(0));
  assert.ok(y.length > 0);
  assert.ok(y.every(value => value === SPARK_HEIGHT - 4));
});

test('tiny residual rates do not expand to a full-height curve', () => {
  const y = plot(Array.from({ length: HISTORY }, (_, i) => 0.001 * 0.7 ** i));
  assert.ok(y.every(value => value >= SPARK_HEIGHT - 4 - 0.1));
});
