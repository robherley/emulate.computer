import guestAssets from "../../src/generated/guest.json";
import { imageIdentityHash, DISK_LOGICAL_BYTES, DEFAULT_RAM_MB } from '../../src/protocol.ts';
import { gunzipIfNeeded } from '../../src/session/streams.ts';

async function fetchBytes(url, compressed = false) {
  const response = await fetch(url);
  if (!response.ok) throw new Error(`${url}: HTTP ${response.status}`);
  const body = compressed ? await gunzipIfNeeded(response.body) : response.body;
  return new Uint8Array(await new Response(body).arrayBuffer());
}

export async function linuxBenchmark(WasmMachine, samples, progress) {
  progress('Loading guest images');
  const images = await Promise.all(['fw.bin', 'kernel.bin', 'initrd.bin', 'dtb-desktop.bin']
    .map(name => fetchBytes(guestAssets.files[name].url)));
  const hash = new Uint8Array(await imageIdentityHash(images.map(bytes => bytes.buffer)));
  const snapshot = await fetchBytes(guestAssets.files['snapshot.bin.gz'].url, true);
  const header = new DataView(snapshot.buffer);
  const diskIdLength = header.getUint32(50, true);
  const diskId = new TextDecoder().decode(snapshot.subarray(54, 54 + diskIdLength));
  const machine = new WasmMachine(DEFAULT_RAM_MB * 1024 * 1024);
  try {
    progress('Seeding disposable disk');
    machine.seed_disk_begin_volatile(DISK_LOGICAL_BYTES);
    const seed = await fetch(guestAssets.files['rootfs.ext4.gz'].url);
    if (!seed.ok) throw new Error(`rootfs: HTTP ${seed.status}`);
    for await (const chunk of await gunzipIfNeeded(seed.body)) {
      machine.seed_disk_write(chunk);
    }
    machine.seed_disk_finish();
    progress('Restoring desktop snapshot');
    machine.restore(snapshot, hash, diskId);
    const epoch = performance.now();
    const encoder = new TextEncoder(), decoder = new TextDecoder();
    async function command(text, expected) {
      machine.uart_output();
      machine.uart_input(encoder.encode(text + '\n'));
      const started = performance.now();
      const hits = machine.decode_cache_hits(), misses = machine.decode_cache_misses();
      let output = '', executionMs = 0, calls = 0;
      while (performance.now() - started < 60_000) {
        machine.advance_mtime_from_host((performance.now() - epoch) * 10_000);
        const begin = performance.now();
        const status = machine.run(50_000);
        executionMs += performance.now() - begin;
        calls++;
        if (status > 1) throw new Error(`Linux exited: ${status}`);
        output += decoder.decode(machine.uart_output());
        if (output.replaceAll('\r', '').includes(expected + '\n')) {
          return { ms: performance.now() - started, executionMs, calls, output,
            cacheHits: machine.decode_cache_hits() - hits, cacheMisses: machine.decode_cache_misses() - misses };
        }
        if (status === 1) await new Promise(resolve => setTimeout(resolve, 1));
      }
      throw new Error(`Linux command timed out: ${output}`);
    }
    await command("stty -echo; printf '\\nCPU_READY\\n'", 'CPU_READY');
    const cases = {
      'linux-shell': ["i=0; s=0; while [ $i -lt 1000 ]; do s=$((s+i)); i=$((i+1)); done; printf '\\nSUM=%s\\n' $s", 'SUM=499500'],
      'linux-python': ["python3 -c 'print(\"\\n\" + str(sum(i*i for i in range(10000))))'", '333283335000'],
      'linux-sha256': ["printf '\\n'; dd if=/dev/zero bs=64k count=32 2>/dev/null | sha256sum", '5647f05ec18958947d32874eeb788fa396a05d0bab7c1b71f112ceb7e9b31eee  -'],
    };
    const results = {};
    for (const [name, [text, expected]] of Object.entries(cases)) {
      progress(`${name}: warmup`);
      await command(text, expected);
      results[name] = [];
      for (let sample = 0; sample < samples; sample++) {
        progress(`${name}: sample ${sample + 1}/${samples}`);
        results[name].push(await command(text, expected));
      }
    }
    return { diskId, results };
  } finally {
    machine.free();
  }
}
