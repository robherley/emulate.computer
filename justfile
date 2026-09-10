set shell := ["bash", "-eu", "-o", "pipefail", "-c"]
set positional-arguments

# List available project workflows.
default:
    @just --list

# Formatting, shell syntax, strict workspace lint, and browser type checks.
check:
    bash scripts/check-project.sh
    just typecheck

# Check TypeScript for the relay, API, and browser.
typecheck:
    npm exec --no -- tsc --noEmit
    cd web && npm exec --no -- tsc --noEmit

# Fast native tests: no Docker, downloads, guest images, or external network.
test:
    cargo test --workspace --locked

# Browser session lifecycle and asset-stream regression tests (Node 24+).
test-web:
    cd web && node --test tests/*.test.ts

# Pinned instruction and privilege regressions, including bit manipulation.
test-isa:
    cargo test -p emulate-cli --locked --test architecture riscv_ -- --ignored

# All prepared architectural corpora. ACT4 failures remain failures.
test-conformance:
    cargo test -p emulate-cli --locked --test architecture -- --ignored --test-threads=1

# Linux boot, storage, networking, display, snapshots, and xv6 shell behavior.
test-guest:
    cargo test -p emulate-cli --locked --test guest -- --ignored --skip xv6_usertests --skip outbound_dns --test-threads=1

# Guest DNS through the real relay and public resolver; requires internet access.
test-online:
    cargo test -p emulate-cli --locked --test guest outbound_dns -- --ignored

# Long-running xv6 kernel/user-space stress coverage.
test-stress:
    cargo test -p emulate-cli --locked --test guest xv6_usertests -- --ignored --test-threads=1

# Local WebSocket/TCP protocol and resource-limit regressions (requires Bun).
test-relay:
    cd relay && bun test

# Prepare the pinned ISA corpus separately from running it.
prepare-isa:
    bash scripts/ci-vendor.sh riscv-tests
    bash scripts/build-riscv-tests.sh

# Prepare the small OS used for kernel compatibility and stress tests.
prepare-xv6:
    bash scripts/ci-vendor.sh xv6
    bash scripts/build-xv6.sh

# Generate ACT4 self-checking ELFs. Add `--profile priv` for the privileged profile.
act4-generate *args:
    bash scripts/generate-act4.sh "$@"

# Run generated ACT4 ELFs. Add `--profile priv` for the privileged profile; quoted globs filter.
test-act4 *args:
    bash scripts/run-act4-tests.sh "$@"

# Materialize a pinned upstream source tree: riscv-tests, act4, or xv6.
vendor name:
    bash scripts/ci-vendor.sh "$1"

# Build the pinned riscv64-unknown-elf cross toolchain image (fallback when no host GCC).
toolchain-image:
    docker buildx build --file docker/riscv-toolchain/Dockerfile \
      --tag "${TOOLCHAIN_IMAGE:-emulate-riscv-toolchain:trixie}" \
      --load docker/riscv-toolchain

# Rebuild and verify the checked-in desktop binary bundle.
guest-prebuilt:
    bash scripts/prebuilt.sh build

# Fetch/build the Linux guest images (firmware, kernel, initramfs, DTBs) and install them into the web app.
guest:
    bash scripts/build-guest.sh

# `guest`, plus the pinned Alpine riscv64 ext4 root filesystem and its browser seed.
guest-rootfs: guest cli
    bash scripts/build-rootfs.sh
    bash scripts/capture-snapshot.sh

# Re-capture just the post-boot state snapshot from the artifacts already built.
guest-snapshot: cli
    bash scripts/capture-snapshot.sh

# Boot Linux natively by resuming the post-boot snapshot (docs/disk.md).
# Runs on a throwaway copy of the root image: a restore is only valid on a disk
# still in its seeded state, and the shipped browser seed is gzipped from the
# original, so this must not write to it.
run-linux-snapshot: guest-snapshot
    mkdir -p target
    cp -c guest/out/alpine-rootfs.ext4 target/snapshot-disk.ext4 \
      2>/dev/null || cp guest/out/alpine-rootfs.ext4 target/snapshot-disk.ext4
    cargo run --release -p emulate-cli -- boot \
      --bios guest/out/fw.bin --kernel guest/out/Image \
      --initrd guest/out/initramfs.cpio.gz --dtb guest/out/virt-rootfs.dtb \
      --disk target/snapshot-disk.ext4 --fw-dynamic true \
      --snapshot guest/out/snapshot.bin

[private]
xv6:
    bash scripts/build-xv6.sh

# Build the native CLI used by snapshots, tests, and benchmarks.
cli:
    cargo build --release --locked -p emulate-cli

# Build the browser emulator and bindings.
wasm:
    wasm-pack build crates/emulate-wasm --target web --out-dir ../../web/src/wasm -- --locked

# Format an existing rootfs tar and publish its browser seed.
guest-disk:
    bash scripts/build-rootfs.sh

# Validate guest assets and generate the content-addressed site manifest.
site-prepare:
    bash scripts/prepare-site.sh

# Publish the rootfs and snapshot to Blob and prepare deployment URLs.
site-publish environment="production":
    bash scripts/publish-site.sh {{ environment }}

# Build the frontend from prepared guest assets and Wasm.
site:
    bash scripts/build-site.sh

# Build the site with the existing guest disk and snapshot.
build: wasm site-prepare site

# Build the guest, snapshots, Wasm, and frontend from source.
build-all: guest-rootfs build

# Production build of the browser application, including Wasm.
web-build: build

# Run deterministic native core microbenchmarks and emit JSON Lines on stdout.
bench:
    cargo bench -p emulate-core --bench core

# Open the dedicated-worker CPU benchmarks, including optional Linux workloads.
bench-web: wasm site-prepare
    cd web && npm exec --no -- vite --open /tests/cpu-benchmark.html

# Start the Vite development server, rebuilding Wasm first.
web: wasm site-prepare
    cd web && npm exec --no -- vite

# Serve the production frontend build locally.
preview:
    cd web && npm exec --no -- vite preview

# Start the local WebSocket ⇄ TCP network relay (Bun), on ws://127.0.0.1:7654.
# Both the browser and the CLI's `boot --net user` dial it.
relay:
    cd relay && bun run server.ts

# Boot Linux natively on the persistent Alpine root (Ctrl-A x to exit).
run-linux: guest-rootfs
    cargo run --release -p emulate-cli -- boot \
      --bios guest/out/fw.bin --kernel guest/out/Image \
      --initrd guest/out/initramfs.cpio.gz --dtb guest/out/virt-rootfs.dtb \
      --disk guest/out/alpine-rootfs.ext4 --fw-dynamic true

# Boot upstream xv6 natively with its VirtIO block image.
run-xv6: xv6
    cargo run --release -p emulate-cli -- boot \
      --kernel vendor/xv6-riscv/kernel/kernel \
      --disk vendor/xv6-riscv/fs.img --disk-volatile
