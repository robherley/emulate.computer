# Testing

Tests separate instruction correctness, device behavior, and complete guest
workflows. Native and browser hosts use the same emulator core, but browser
lifecycle, storage, and rendering also need host-specific coverage.

## Routine checks

| Command | What it checks |
| --- | --- |
| `just check` | Rust formatting and strict Clippy, shell syntax, TypeScript types |
| `just test` | Workspace unit tests and fast integration tests, including devices, network protocols, snapshots, and devicetree geometry |
| `just test-web` | Browser session lifecycle, disk ownership, asset streaming, control, input, metrics, and network state using Node 24+ |
| `just test-relay` | Bun relay framing, connections, backpressure, and resource limits |

`just test` requires no guest images, Docker, cross compiler, or internet. Some
native transport tests bind localhost sockets. Relay tests require Bun; the
Redis integration case is skipped when `REDIS_URL` is unset.

## Instruction and privilege compliance

[architecture.rs](../crates/emulate-cli/tests/architecture.rs) runs prepared ELF
corpora through the shared native HTIF runner. The runner loads each ELF, executes
the machine within a budget, and reports its guest pass/fail result through
`tohost`. Missing artifacts and incomplete pinned corpora fail rather than
silently reducing coverage.

Two complementary inputs exercise the implementation:

- **riscv-tests:** pinned instruction, privilege, and bit-manipulation regressions.
  These provide quick architectural checks after CPU, CSR, MMU, trap, or cache changes.
- **ACT4:** self-checking tests generated using a Sail reference model and the
  checked-in [DUT profiles](../test/compliance/act4/). Separate unprivileged and
  privileged profiles describe the platform and instruction set under test.

Prepare sources and generated binaries explicitly:

```sh
just prepare-isa
just act4-generate
just act4-generate --profile priv
```

Then run:

```sh
just test-isa
just test-conformance
just test-act4 --profile priv
just test-act4 --list 'D-*'
just test-act4 'D-fmadd*'
```

`test-conformance` runs the prepared ISA corpus and both ACT4 profiles.
`test-act4` provides per-ELF diagnostics, filters, and timeout/RAM options using
the CLI runner. Failures remain failures; there is no blanket ACT4 exemption.
ACT4 generation can select extensions, so results establish coverage of the
**generated corpus**, not full-profile certification. Report the source revision,
profile, generation selection, and executed count with compliance results.

Pinned upstream inputs live under `vendor/`; generated ACT4 ELFs live under
`target/act4/`. Generation requires Docker. ISA and xv6 builds use a host RISC-V
cross compiler or the Docker fallback built by `just toolchain-image`.
Rebuild artifacts after changing sources or DUT configuration.

## Guest integration

Prepare complete guest workloads separately from test execution:

```sh
just prepare-xv6
just guest-rootfs
```

The image build requires Docker Buildx with RISC-V emulation. Guest networking
fixtures also require Bun. See [disk.md](disk.md) for image and snapshot creation.

| Command | What it proves |
| --- | --- |
| `just test-guest` | Linux boot/shell/time/entropy, storage persistence, local networking, display/input, snapshots/reboot, and xv6 shell behavior |
| `just test-stress` | Long-running xv6 `usertests -q` kernel/user-space coverage |
| `just test-online` | Guest DNS through the real relay and a public resolver; requires internet |

Guest and architecture suites are explicitly ignored by ordinary Cargo test
runs. The dedicated commands enable them. `test-guest` excludes stress and online
cases and runs serially to bound resource usage. Test execution does not download
sources or build missing images.

[Headless](../crates/emulate-cli/src/headless.rs) shares machine execution, console
capture, entropy, and network-aware idle handling between tests and snapshot
capture. Guest fixtures supply deadlines, reboot handling, and private disk and
relay instances. Tests check behavior visible to the OS: a file surviving reboot,
a working TCP exchange, X11 pointer/button/key state, or a restored desktop.
xv6 supplies an independent kernel workload; it does not replace Linux integration
or instruction-level compliance.

## Test layout and focused runs

| Location | Responsibility |
| --- | --- |
| [Core source](../crates/emulate-core/src/) | Focused unit tests beside CPU, MMU, and device implementations |
| [Core integration tests](../crates/emulate-core/tests/) | Network contracts, whole-machine snapshot invariants, devicetree agreement |
| [Architecture runner](../crates/emulate-cli/tests/architecture.rs) | ISA and ACT4 corpora through one execution harness |
| [Guest tests](../crates/emulate-cli/tests/guest/) | Boot, storage, network, display, snapshot, and xv6 behavior |
| [Web tests](../web/tests/) | Browser host logic with Node's test runner |
| [Relay](../relay/) | Bun protocol, browser broker, deployment guards, and resource-limit tests |

```sh
cargo test -p emulate-core --test network resolver::
cargo test -p emulate-cli --test guest storage:: -- --ignored
cargo test -p emulate-cli --test guest snapshot:: -- --ignored --test-threads=1
cargo test -p emulate-cli --test architecture act4_privileged -- --ignored
```

Use a small deterministic test to reproduce a core bug. Add a guest scenario
when it demonstrates integration that the smaller test cannot establish.

## Browser and performance verification

Node tests do not run a real browser's OPFS, worker scheduling, or canvas.
For changes to those paths, build with `just web-build` and exercise the app:

- Boot and restore, switch Console/Desktop, and confirm both remain usable.
- Resize the viewport and check desktop geometry, pointer alignment, dragging,
  keyboard input, and input release after focus loss.
- Restart with a modified disk, Reset to a fresh disk, and open two tabs to
  verify disk independence.
- Connect/disconnect networking, check per-tab indicators and connection lists,
  and verify relay loss and recovery across tabs.
- Close tabs and revisit to check abandoned session cleanup; check fallback paths
  when OPFS, SharedWorker, or OffscreenCanvas are unavailable.

`just bench` emits release-mode native core measurements as JSON Lines.
`just bench-web` opens dedicated-worker CPU benchmarks and optional Linux
workloads. Compare identical workloads, guest images, build settings, and host
conditions. Use browser measurements as the primary evidence for product
performance; native timings help isolate core costs. Neither benchmark is a
correctness test or a substitute for interactive display verification.

## Relay deployment checks

Run `npm ci` at the root and `just typecheck` after preparing guest artifacts.
Use `just test-web` for the Node suite and
`REDIS_URL=redis://127.0.0.1:6379 just test-relay` for the complete Bun suite with
an isolated Redis database. Without Redis, shared-counter integration tests
are explicitly skipped.

Deployment tests cover origin checks, trusted client IP validation, WebSocket
upgrades, and endpoint restrictions. Broker tests cover socket sharing, tab
isolation, reconnects, and receive credit.
See [networking](networking.md#vercel-deployment) for live preview checks.

The native transport's unit tests use local WebSocket peers to verify socket
sharing, channel isolation, credits, bounded buffers, and reconnects. The guest
network fixture runs concurrent HTTP fetches and a local DNS lookup through
the Bun relay, without depending on internet access.
