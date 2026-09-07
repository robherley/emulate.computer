import { test } from "node:test";
import assert from "node:assert/strict";
import { DisplayBuffer } from "../src/display-buffer.ts";

test("multiple guest draws before presentation retain the newest pixels for every dirty row", () => {
  const buffer = new DisplayBuffer(1, 3);
  buffer.update(new Uint32Array([0, 2]), new Uint8Array([1, 2, 3, 255, 4, 5, 6, 255]));
  buffer.update(new Uint32Array([0, 1]), new Uint8Array([7, 8, 9, 255, 10, 11, 12, 255]));
  const rows = buffer.takeRows();
  assert.deepEqual([...rows], [0, 1, 2]);
  assert.deepEqual([...buffer.copyRows(rows)], [7, 8, 9, 255, 10, 11, 12, 255, 4, 5, 6, 255]);
  assert.equal(buffer.takeRows().length, 0);
});

test("sparse updates preserve unchanged rows across presentations", () => {
  const buffer = new DisplayBuffer(1, 3);
  buffer.update(new Uint32Array([0, 1, 2]), new Uint8Array(12).fill(42));
  buffer.takeRows();
  buffer.update(new Uint32Array([2]), new Uint8Array([1, 2, 3, 255]));
  assert.deepEqual([...buffer.takeRows()], [2]);
  assert.deepEqual([...buffer.pixels], [42, 42, 42, 42, 42, 42, 42, 42, 1, 2, 3, 255]);
});
