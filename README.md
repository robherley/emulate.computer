# emulate.computer

An Alpine Linux computer running on a RISC-V emulator in the browser.

- `crates/emulate-core`: portable CPU, MMU, devices, snapshots and user-mode networking.
- `crates/emulate-wasm` and `web`: browser host, session disk and terminal/display UI.
- `crates/emulate-cli`: native boot, snapshot and architectural-test harness.
- `relay`: WebSocket/TCP networking bridge.
- `guest`: reproducible firmware, Linux, filesystem and devicetree builds.

Guest and conformance suites have explicit preparation and execution commands in
[the testing guide](docs/testing.md).

## Documentation

- [Deployment](docs/deployment.md): build locally and deploy to Vercel.
- [Build stages](scripts/README.md): local commands, stage inputs, and outputs.

- [Emulation](docs/emulation.md): CPU, platform devices, guest control, and performance optimizations.
- [Testing](docs/testing.md): instruction compliance, emulator tests, and browser verification.
- [Disk](docs/disk.md): rootfs builds, installed software, session storage, and snapshots.
- [Networking](docs/networking.md): guest packets, shared WebSockets, and relay TCP connections.
- [Display](docs/display.md): desktop session, framebuffer, rendering, resizing, and input.

## Inspiration

- [v86](https://github.com/copy/v86): x86 PC emulator and x86-to-wasm JIT emulation.
- [JSLinux](https://bellard.org/jslinux/) and [TinyEMU](https://bellard.org/tinyemu/): Fabrice Bellard's projects emulation projects.
