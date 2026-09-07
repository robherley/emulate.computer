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
