const GZIP_INPUT_CHUNK_BYTES = 16 * 1024;

// Sniff gzip magic: browsers may already have decompressed Content-Encoding: gzip.
export async function gunzipIfNeeded(
  body: ReadableStream<Uint8Array<ArrayBuffer>>,
): Promise<ReadableStream<Uint8Array<ArrayBuffer>>> {
  const source = body.getReader();
  const head: Uint8Array<ArrayBuffer>[] = [];
  let peeked = 0;
  let ended = false;
  while (peeked < 2 && !ended) {
    const { done, value } = await source.read();
    if (done) {
      ended = true;
    } else if (value.length > 0) {
      head.push(value);
      peeked += value.length;
    }
  }
  const compressed =
    head.length > 0 &&
    head[0][0] === 0x1f &&
    (head[0][1] ?? head[1]?.[0]) === 0x8b;
  let pending: Uint8Array<ArrayBuffer> | undefined;
  let offset = 0;
  const raw = new ReadableStream<Uint8Array<ArrayBuffer>>({
    async pull(controller) {
      while (!pending) {
        pending = head.shift();
        if (!pending) {
          if (ended) {
            controller.close();
            source.releaseLock();
            return;
          }
          const { done, value } = await source.read();
          ended = done;
          if (value?.length) pending = value;
        }
        offset = 0;
      }
      const end = compressed
        ? Math.min(offset + GZIP_INPUT_CHUNK_BYTES, pending.length)
        : pending.length;
      controller.enqueue(pending.subarray(offset, end));
      offset = end;
      if (offset === pending.length) pending = undefined;
    },
    cancel(reason) {
      pending = undefined;
      head.length = 0;
      return source.cancel(reason).finally(() => source.releaseLock());
    },
  }, { highWaterMark: 0 });
  return compressed ? raw.pipeThrough(new DecompressionStream("gzip")) : raw;
}

// Report cumulative bytes as they arrive; count before any decompression so
// progress tracks the network transfer rather than the inflated payload.
export function countBytes(
  body: ReadableStream<Uint8Array<ArrayBuffer>>,
  onBytes: (loaded: number) => void,
): ReadableStream<Uint8Array<ArrayBuffer>> {
  let loaded = 0;
  return body.pipeThrough(
    new TransformStream<Uint8Array<ArrayBuffer>, Uint8Array<ArrayBuffer>>({
      transform(chunk, controller) {
        loaded += chunk.byteLength;
        onBytes(loaded);
        controller.enqueue(chunk);
      },
    }),
  );
}
