# Project guide

emulate.computer runs Alpine Linux on a RISC-V machine emulated in Rust and
compiled to WebAssembly. The browser experience is the primary product; the
native CLI supports development, compliance testing, snapshots, and benchmarks.
Prefer small, understandable implementations and keep dependencies lean.

## Layout

| Path | Responsibility |
| --- | --- |
| `crates/emulate-core/` | Portable CPU interpreter, MMU, devices, snapshots, and user-mode networking |
| `crates/emulate-wasm/` | Browser bindings and host adapters |
| `crates/emulate-cli/` | Native boot, ELF loading, guest-driving logic, and test runners |
| `web/` | React UI, emulator worker, terminal, desktop rendering, and session storage |
| `relay/` | Bun WebSocket-to-TCP relay, deployment guards, and protocol tests |
| `api/` | Same-origin Vercel relay endpoint |
| `guest/` | Pinned guest builds, overlays, source patches, and devicetrees |
| `test/compliance/` | Project-owned ACT4 platform profiles |
| `vendor/` | Prepared upstream sources and the project-owned `libc-shim` headers |
| `scripts/`, `docker/` | Build, test preparation, and toolchain workflows |

Read the relevant guide before changing a subsystem:
[emulation](docs/emulation.md), [testing](docs/testing.md),
[disk](docs/disk.md), [networking](docs/networking.md),
[display](docs/display.md). Keep these focused on current behavior; update them
when that behavior changes.

## Implementation conventions

- Keep host-specific I/O out of the portable emulator core. Share machine-driving
  and VirtIO transport logic rather than duplicating it between hosts or devices.
- Preserve instruction, trap, memory, and device semantics when optimizing.
  CPU and DMA writes must retain decoded-code invalidation and memory tracking.
- Measure performance in the browser. Native release benchmarks help isolate CPU
  costs, but do not establish browser responsiveness or rendering performance.
- Keep React components and state ownership focused. The console is the primary
  view, with the desktop as a secondary tab. Preserve useful performance graphs.
- Use the VirtIO console control channel for desktop and network actions; do not
  inject commands into the user's interactive UART shell.
- Preserve per-tab guest network state even when tabs share a relay WebSocket.
- Check enabled dependency features before adding or replacing a library. Keep
  Cargo and the root npm workspace lockfile consistent with their manifests.
- Comments should explain non-obvious behavior or constraints. Avoid decorative
  banners, iteration history, obvious narration, and temporary tracking documents.
- Preserve unrelated working-tree changes. Keep refactors within the requested
  scope and report material limitations or checks that could not run.

## Build and verify

Use `just --list` for workflows. Run checks appropriate to the change; add tests
for meaningful behavior and regressions rather than mirroring implementation.

| Change | Verification |
| --- | --- |
| Rust | `just check` and relevant `cargo test -p <crate> --locked`; `just test` runs the workspace |
| Web | `just test-web`, `npm run typecheck --prefix web`, `npm run build --prefix web` |
| Rust used by the browser | `just web-build` rebuilds Wasm and the web app |
| Relay | `just test-relay` |
| CPU/privilege semantics | Relevant ISA/ACT4 tests; see preparation below |
| Guest or integration behavior | Relevant `just test-guest` cases and browser verification |
| Documentation or shell | Check links, paths, shell syntax, and `git diff --check` |

Use `mise install` for the pinned repo tools in `mise.toml`; activate mise or
prefix commands with `mise exec --`. Docker remains a separate prerequisite.
Run `npm ci` at the repository root to install both workspaces. The web uses
Node 24+ for tests; the relay uses Bun. `npm run typecheck` checks both hosts and
API entrypoints. Native network tests bind localhost sockets. The Redis integration test requires `REDIS_URL`.
Explain environment-related failures separately from test regressions.

Compliance and guest suites are explicitly ignored by ordinary Cargo test runs.
Prepare ISA inputs with `just prepare-isa`, ACT4 with `just act4-generate` and
`just act4-generate --profile priv`, and xv6 with `just prepare-xv6`.
`just test-conformance` runs the prepared architectural corpora. Generated test
coverage is not full-profile certification. Do not silently skip failures or
missing artifacts to claim a pass.

For browser-facing changes, exercise the affected behavior in an isolated browser
session: boot/restore, input, resizing, tab switching, networking, or disk
lifecycle as appropriate. Node tests do not exercise real OPFS or canvas behavior.

## Guest artifacts

Edit `guest/Dockerfile`, overlays, devicetrees, or checked-in patches as the source
of truth. Keep package versions and downloaded source checksums pinned. Keep the
guest graphics stack lightweight and retain the build's Mesa/LLVM dependency guard.
Normal guest builds consume `guest/prebuilt/desktop.tar.gz`. After changing its
recipe, patches, or Alpine base, run `just guest-prebuilt` and include the bundle,
manifest, and checksums together. `bash scripts/prebuilt.sh verify` checks freshness.

`just guest-rootfs` builds guest artifacts, formats the ext4 disk, publishes browser
assets, and captures boot snapshots. It requires jq, Docker Buildx with RISC-V
emulation and a Rust toolchain. `just guest-snapshot` recaptures existing artifacts.
Rebuild the web app after publishing guest assets when using the production preview.
`npm run build` uses Bash, jq, and shasum to build Wasm, verifies the guest artifact pairing, generates the
asset manifest, and builds Vite. Actions publishes the heavy build as a GitHub release; Vercel downloads the
pinned `ASSET_RELEASE` and builds the frontend remotely. See README.

Disk identity is `sha256:<hash>` of the uncompressed ext4 seed, published through
`web/public/guest/rootfs.sha256`. No manual version bump is needed. Snapshot capture
checks that its disk matches the published seed. Capture uses a scratch disk;
do not boot interactively against the pristine seed when validating snapshots.

`guest/out/`, `web/public/guest/`, `web/generated-public/`, `web/src/generated/`,
`web/src/wasm/`, `web/dist/`, `target/`, and
downloaded upstream vendor trees are generated and ignored. Do not hand-edit or
commit them. Project-owned files such as `vendor/libc-shim/` are tracked sources.

The Open Graph source is `web/assets/social-preview.svg`. After editing it,
regenerate the tracked PNG with:

```sh
rsvg-convert web/assets/social-preview.svg -o web/public/og-image.png
```
