import { test } from "node:test";
import assert from "node:assert/strict";
import { gzipSync } from "node:zlib";
import { gunzipIfNeeded } from "../src/session/streams.ts";

function chunks(...values: Uint8Array<ArrayBuffer>[]) {
  return new ReadableStream<Uint8Array<ArrayBuffer>>({
    start(controller) {
      values.forEach((value) => controller.enqueue(value));
      controller.close();
    },
  });
}
async function text(stream: ReadableStream<Uint8Array<ArrayBuffer>>) {
  return new Response(await gunzipIfNeeded(stream)).text();
}

test("streaming assets handles a gzip header split across chunks", async () => {
  const compressed = new Uint8Array(gzipSync("guest image"));
  assert.equal(
    await text(chunks(compressed.slice(0, 1), compressed.slice(1))),
    "guest image",
  );
});

test("a server-decompressed asset is not decompressed twice", async () => {
  assert.equal(
    await text(chunks(new TextEncoder().encode("guest image"))),
    "guest image",
  );
});

test("empty and one-byte streams retain their contents", async () => {
  assert.equal(await text(chunks()), "");
  assert.equal(await text(chunks(new Uint8Array([65]))), "A");
});

test("gzip detection ignores empty chunks around the magic bytes", async () => {
  const compressed = new Uint8Array(gzipSync("guest image"));
  assert.equal(await text(chunks(
    new Uint8Array(), compressed.slice(0, 1), new Uint8Array(), compressed.slice(1),
  )), "guest image");
});

test("large network chunks reach the gzip decoder in bounded pieces", async (t) => {
  const NativeDecoder = globalThis.DecompressionStream;
  const sizes: number[] = [];
  globalThis.DecompressionStream = class {
    readonly readable: ReadableStream<Uint8Array<ArrayBuffer>>;
    readonly writable: WritableStream<Uint8Array<ArrayBuffer>>;
    constructor(format: CompressionFormat) {
      const input = new TransformStream<Uint8Array<ArrayBuffer>, Uint8Array<ArrayBuffer>>({
        transform(chunk, controller) {
          sizes.push(chunk.length);
          controller.enqueue(chunk);
        },
      });
      this.writable = input.writable;
      this.readable = input.readable.pipeThrough(new NativeDecoder(format));
    }
  };
  t.after(() => { globalThis.DecompressionStream = NativeDecoder; });
  // Zero-filled disk space has a very large expansion ratio. Deliver the
  // entire compressed payload in one read, as a network/cache response may.
  const expected = new Uint8Array(32 * 1024 * 1024);
  const compressed = new Uint8Array(gzipSync(expected));
  assert(compressed.length > 16 * 1024);
  const actual = new Uint8Array(await new Response(
    await gunzipIfNeeded(chunks(compressed)),
  ).arrayBuffer());
  assert.deepEqual(actual, expected);
  assert(sizes.length > 1);
  assert(Math.max(...sizes) <= 16 * 1024);
  assert.equal(sizes.reduce((sum, size) => sum + size, 0), compressed.length);
});

test("cancelling an asset stops upstream reads and releases its reader", async () => {
  const reason = new Error("download stopped");
  let cancelled: unknown;
  let reads = 0;
  const source = new ReadableStream<Uint8Array<ArrayBuffer>>({
    pull(controller) {
      reads++;
      controller.enqueue(new Uint8Array([65, 66, 67]));
    },
    cancel(value) { cancelled = value; },
  }, { highWaterMark: 0 });
  const output = await gunzipIfNeeded(source);
  assert.equal(reads, 1);
  const reader = output.getReader();
  assert.deepEqual((await reader.read()).value, new Uint8Array([65, 66, 67]));
  await reader.cancel(reason);
  assert.equal(reads, 1);
  assert.equal(cancelled, reason);
  assert.equal(source.locked, false);
});
