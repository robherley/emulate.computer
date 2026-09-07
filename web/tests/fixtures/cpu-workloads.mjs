const addi = (rd, rs, imm) => (imm & 4095) << 20 | rs << 15 | rd << 7 | 0x13;
const reg = (rd, a, b, fn = 0) => b << 20 | a << 15 | fn << 12 | rd << 7 | 0x33;
const jump = imm => (imm >>> 20 & 1) << 31 | (imm >>> 1 & 1023) << 21 |
  (imm >>> 11 & 1) << 20 | (imm >>> 12 & 255) << 12 | 0x6f;
const csr = (address, rs) => address << 20 | rs << 15 | 0x1073;
const bytes = words => {
  const data = new Uint8Array(words.length * 4);
  const view = new DataView(data.buffer);
  words.forEach((word, i) => view.setUint32(i * 4, word, true));
  return data;
};

export const workloads = ['alu', 'ram', 'ram-unaligned', 'sv39', 'code-footprint'];
const states = new WeakMap();

export function createMachine(WasmMachine, workload) {
  if (!workloads.includes(workload)) throw new Error(`Unknown workload: ${workload}`);
  const machine = new WasmMachine(2 * 1024 * 1024);
  let code = workload === 'alu'
    ? [addi(5, 5, 1), addi(6, 6, 3), reg(7, 5, 6), 0x00139413, reg(9, 8, 5, 4), jump(-20)]
    : [reg(13, 11, 12), 0x0056b023, 0x0006b303, addi(5, 6, 1),
      addi(12, 12, 8), 0x7ff67613, jump(-24)];
  if (workload === 'code-footprint') {
    code = Array.from({ length: 4096 }, (_, i) => [
      addi(5, 5, 1), addi(6, 6, 3), reg(7, 5, 6, 4), jump(i === 4095 ? -65532 : 4),
    ]).flat();
  }
  machine.load_blob(0x80004000, bytes(code));
  machine.set_boot(0x80004000, 0, 0x80010000 + (workload === 'ram-unaligned' ? 1 : 0), 0);
  if (workload === 'sv39') {
    // Supervisor code and data share a 1-GiB leaf; fetch and load/store exercise Sv39.
    machine.load_blob(0x80002008, bytes([0x200000cf, 0]));
    machine.load_blob(0x80000000, bytes([
      addi(5, 0, 0x80), 0x00c29293, addi(5, 5, 2),
      addi(6, 0, 1), 0x03f31313, reg(5, 5, 6, 6), csr(0x180, 5),
      0x000012b7, 0x0012d293, csr(0x300, 5), csr(0x341, 10), 0x30200073,
    ]));
    machine.set_boot(0x80000000, 0x40004000, 0x40010000, 0);
    if (machine.run(12) !== 0 || machine.cpu_pc() !== 0x40004000n) {
      throw new Error('Supervisor setup failed');
    }
  }
  states.set(machine, { workload, instructions: 0 });
  return machine;
}

export function runSample(machine, instructions, chunk = 50_000) {
  const start = performance.now();
  for (let left = instructions; left > 0; left -= chunk) {
    if (machine.run(Math.min(left, chunk)) !== 0) throw new Error('Unexpected guest exit');
  }
  const ms = performance.now() - start;
  const state = states.get(machine);
  state.instructions += instructions;
  const registers = machine.cpu_registers();
  const { workload, instructions: total } = state;
  const expected = workload === 'alu' ? Math.ceil(total / 6)
    : workload === 'code-footprint' ? Math.ceil(total / 4)
    : Math.floor((total + 3) / 7) + (workload === 'sv39' ? 2048 : 0);
  if (registers[5] !== BigInt(expected)) throw new Error(`${workload}: incorrect accumulator`);
  return {
    ms, mips: instructions / ms / 1000,
    pc: String(machine.cpu_pc()), registers: Array.from(registers, String),
  };
}
