# Emulation

emulate.computer runs a complete Alpine Linux machine in the browser. The CPU,
RAM, memory translation, and devices live in the Rust `emulate-core` crate.
`emulate-wasm` exposes that machine to a dedicated browser worker; the React UI
provides the serial console, desktop, and controls. `emulate-cli` runs the same
core natively for development, guest tests, snapshot capture, and benchmarks.

## Processor and platform

The guest sees one 64-bit RISC-V hart with RV64IMAFDC, Zicsr, and Zifencei, plus
Zbb, Zbc, Zbkb, Zknd, Zkne, and Zknh bit-manipulation and scalar-cryptography
extensions. The interpreter implements machine, supervisor, and user privilege
levels, traps, CSRs, atomics, and Sv39 virtual memory. Floating-point operations
use `rustc_apfloat` for guest arithmetic and exception flags.

The platform follows the QEMU `virt` layout. RAM starts at `0x80000000`; the
browser allocates 128 MiB. OpenSBI starts Linux, which uses a devicetree describing
the platform. The graphical guest reserves 16 MiB of that RAM for its display.

Alongside VirtIO devices, the bus implements:

- CLINT software interrupts and the machine timer.
- PLIC external-interrupt priorities, enables, claiming, and completion.
- A 16550-compatible UART for the interactive serial console.
- Goldfish RTC for wall-clock time and alarms.
- A test finisher for shutdown/reset and an interrupt generator for ACT4 tests.
- A framebuffer and rectangle blitter, described in [display.md](display.md).

See the [CPU implementation](../crates/emulate-core/src/cpu/),
[MMU](../crates/emulate-core/src/mmu.rs), [bus](../crates/emulate-core/src/bus.rs),
and [guest devicetree](../guest/dts/virt.dts).

## VirtIO devices

All six devices use modern VirtIO MMIO, transport version 2, with split queues
in guest RAM. Shared [transport code](../crates/emulate-core/src/devices/virtio_mmio.rs)
handles feature selection, queue addresses, readiness, status, reset, and
interrupt acknowledgement. Shared [queue code](../crates/emulate-core/src/devices/virtqueue.rs)
validates descriptor chains and updates used rings. Device-specific code supplies
the payload behavior. Unacknowledged device interrupts remain asserted through
the PLIC. Indirect descriptors and event-index notifications are not negotiated.

| Device | MMIO base / IRQ | Queues | Implementation |
| --- | --- | --- | --- |
| Block | `0x10001000` / 1 | One request queue, 8 descriptors | 512-byte sectors, scatter/gather reads and writes, and FLUSH through a host block backend. See [disk.md](disk.md). |
| Entropy | `0x10002000` / 2 | One queue, 8 descriptors | Fills guest buffers with bytes injected from the host's cryptographic random source. Requests wait when entropy is unavailable. |
| Network | `0x10003000` / 3 | RX and TX, 256 descriptors each | Exchanges Ethernet frames through bounded inbox/outbox queues and a `NetBackend`. Advertises a MAC address and 1500-byte MTU, without checksum or segmentation offloads. See [networking.md](networking.md). |
| Keyboard | `0x10004000` / 4 | Event and status, 64 descriptors each | Reports Linux input key events and synchronization records from host keyboard input. |
| Absolute tablet | `0x10005000` / 5 | Event and status, 64 descriptors each | Reports absolute X/Y positions, three mouse buttons, and wheel events. See [display.md](display.md). |
| Console | `0x10007000` / 6 | Receive and transmit, 128 descriptors each | A single control port, separate from the interactive UART, with bounded byte buffers. |

Device DMA writes use the same RAM write tracking as CPU stores, so writes into
executable memory invalidate cached decoded instructions.

## Guest control

The interactive shell uses `/dev/ttyS0`. A separate `emuctl serve` service uses
the VirtIO console at `/dev/hvc0`, allowing the browser to start/stop the desktop
and connect/disconnect networking without typing into the user's shell.

The protocol is newline-delimited request IDs and allowlisted actions:
`desktop.start`, `desktop.stop`, `network.connect`, `network.disconnect`, and
`status`. Replies carry the request ID; unsolicited readiness and state messages
use ID zero. Periodic status messages let a new host recover state after a
snapshot restore. The desktop starts during boot, and the browser automatically
requests networking when the control service is ready.

See [emuctl](../guest/overlays/rootfs/usr/local/bin/emuctl) and the
[browser session code](../web/src/session/).

## Performance

The CPU is an interpreter with a decoded-block cache. Reusing decoded operations
avoids repeated instruction decoding. Cache entries are keyed by physical code
addresses and checked against RAM page-write generations and fetch permissions;
self-modifying code, DMA writes, and address-space changes must retain normal
instruction and translation semantics.

Sv39 translation uses a TLB and a fetch-translation memo. Common RAM loads and
stores take short paths; page walks, MMIO, and cross-page accesses use slower
paths. Cache indexing mixes upper physical-address bits to reduce conflicts
between aligned code regions.

Device interrupt sources are refreshed when relevant MMIO or network state
changes and at run entry. Timer state and interrupt delivery still receive
instruction-level checks. Idle execution can advance to the next relevant timer
or network deadline instead of spending host CPU spinning on a sleeping guest.

The browser worker executes bounded instruction slices and yields to handle
messages and input. Rendering uses dirty rows, committed frames, and bounded
transfers; snapshots avoid repeating guest startup. These optimizations address
work outside the instruction loop as well as CPU throughput.

The top bar shows a blue Working indicator during sustained guest CPU execution.
It uses the worker's execution-time metric, with a short activation delay and
cooldown to avoid flickering. This indicates CPU activity, not application launch
progress; a program waiting on I/O may be idle from the processor's perspective.

Use [testing.md](testing.md) for correctness checks and performance workflows.
Browser benchmarks are the primary performance check: native timings do not
capture Wasm compilation, worker scheduling, canvas presentation, or browser I/O.
