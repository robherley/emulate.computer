# Build stages

Run commands from the repository root. Stage scripts consume existing inputs;
they do not build missing prerequisites. `just` composes stages for local use.
Use `just` for project tasks; npm manages workspace dependencies with `npm ci`.
The package manifests do not define parallel task aliases.

| Stage | Command | Inputs | Outputs |
| --- | --- | --- | --- |
| Guest files | `bash scripts/build-guest.sh` | Guest Dockerfile, prebuilt bundle, overlays | `guest/out/` firmware, kernel, DTBs, initramfs, rootfs tar; browser boot files |
| Disk | `bash scripts/build-rootfs.sh` | Rootfs tar and DTBs | Ext4 seed, compressed browser disk and disk hash |
| Native emulator | `just cli` | Rust workspace | `target/release/emulate-computer` |
| Browser emulator | `just wasm` | Rust workspace | `web/src/wasm/` |
| Snapshots | `bash scripts/capture-snapshot.sh` | Native emulator, guest boot files, disk | Console and desktop snapshots; compressed browser snapshot |
| Prepare site | `bash scripts/prepare-site.sh` | Browser guest files and matching snapshot | Hashed assets in `web/generated-public/`, `web/src/generated/guest.json` |
| Site | `bash scripts/build-site.sh` | Prepared guest assets, Wasm, frontend | `web/dist/` |

`just cli wasm` builds both emulator targets. `prepare-site.sh public`
refreshes only static public files, preserving the prepared guest assets; the site
build uses this to include current icons and other public files.

## Local commands

- `just build`: rebuild Wasm, prepare assets, and build the site using the existing guest disk and snapshot.
- `just build-all`: also rebuild guest files, format the disk, and capture snapshots.
- `just guest-rootfs`: guest files, native CLI, disk, and snapshots, without the web build.
- `just guest-disk`: format an existing rootfs tar without capturing snapshots.
- `just guest-snapshot`: build the native CLI and recapture snapshots from the existing disk.
- `just web`: build Wasm and start Vite; asset preparation runs before the development server.
- `just guest-prebuilt`: regenerate desktop binaries only when their recipe or patches change.

Formatting the disk removes the published snapshot so a stale snapshot cannot be
shipped with a new seed. Snapshot capture uses scratch disk copies and checks the
published disk identity. Set `EMULATE_CLI` to use a different native executable instead of
`target/release/emulate-computer`.

The build scripts require Bash, jq, shasum, and the repo tools in `mise.toml`.
Guest stages also require Docker Buildx with RISC-V emulation.

Deploy locally using the [Vercel instructions](../docs/deployment.md).
CI runs tests and does not publish build artifacts or deployments.
