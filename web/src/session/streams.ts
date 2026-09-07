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
    } else {
      head.push(value);
      peeked += value.length;
    }
  }
  const compressed =
    head.length > 0 &&
    head[0][0] === 0x1f &&
    (head[0][1] ?? head[1]?.[0]) === 0x8b;
  const raw = new ReadableStream<Uint8Array<ArrayBuffer>>({
    start(controller) {
      for (const chunk of head) controller.enqueue(chunk);
      if (ended) controller.close();
    },
    async pull(controller) {
      const { done, value } = await source.read();
      if (done) controller.close();
      else controller.enqueue(value);
    },
    cancel: (reason) => source.cancel(reason),
  });
  return compressed ? raw.pipeThrough(new DecompressionStream("gzip")) : raw;
}
