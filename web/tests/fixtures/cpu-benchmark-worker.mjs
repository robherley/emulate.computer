import { createMachine, runSample, workloads } from './cpu-workloads.mjs';

self.onmessage = async ({ data: { moduleUrl, instructions = 5_000_000, samples = 7, linux = false } }) => {
  try {
    const { default: init, WasmMachine } = await import(/* @vite-ignore */ moduleUrl);
    await init();
    if (linux) {
      const { linuxBenchmark } = await import('./cpu-linux.mjs');
      self.postMessage({ results: await linuxBenchmark(WasmMachine, samples,
        progress => self.postMessage({ progress })) });
      return;
    }
    const results = {};
    for (const workload of workloads) {
      const machine = createMachine(WasmMachine, workload);
      try {
        runSample(machine, 1_000_000);
        results[workload] = Array.from({ length: samples }, () => runSample(machine, instructions));
      } finally {
        machine.free();
      }
    }
    self.postMessage({ results });
  } catch (error) {
    self.postMessage({ error: String(error.stack ?? error) });
  }
};
