#!/usr/bin/env bash
# Capture separate console and desktop snapshots without modifying the seed disk.

set -euo pipefail

root="$(cd "$(dirname "$0")/.." && pwd)"
cd "$root"

image="guest/out/alpine-rootfs.ext4"
out="guest/out/snapshot.bin"
seed="web/public/guest/snapshot.bin.gz"

if ! command -v cargo >/dev/null 2>&1; then
  echo "error: cargo is required to capture the post-boot snapshot" >&2
  echo "install Rust (https://rustup.rs) and retry, or set ROOTFS_SKIP_SNAPSHOT=1" >&2
  exit 2
fi

for artifact in guest/out/fw.bin guest/out/Image guest/out/initramfs.cpio.gz \
  guest/out/virt-rootfs.dtb guest/out/virt-desktop.dtb "$image"; do
  [[ -f "$artifact" ]] || {
    echo "error: $artifact is missing; run \`just guest-rootfs\` first" >&2
    exit 2
  }
done

# Refuse to publish snapshots from a disk that differs from the browser seed.
seed_hash="$(shasum -a 256 "$image" | awk '{print $1}')"
if [[ ! -f web/public/guest/rootfs.sha256 ]] || \
   [[ "$(cat web/public/guest/rootfs.sha256)" != "$seed_hash" ]]; then
  echo "error: root disk differs from the published seed; run just guest-rootfs" >&2
  exit 2
fi
disk_id="sha256:$seed_hash"

echo "capturing console and desktop snapshots (two guest boots) ..."
cargo run --release --quiet -p emulate-cli -- snapshot \
  --bios guest/out/fw.bin \
  --kernel guest/out/Image \
  --initrd guest/out/initramfs.cpio.gz \
  --dtb guest/out/virt-rootfs.dtb \
  --disk "$image" \
  --disk-id "$disk_id" \
  --out "$out"

out="guest/out/snapshot-desktop.bin"
cargo run --release --quiet -p emulate-cli -- snapshot \
  --bios guest/out/fw.bin \
  --kernel guest/out/Image \
  --initrd guest/out/initramfs.cpio.gz \
  --dtb guest/out/virt-desktop.dtb \
  --disk "$image" --disk-id "$disk_id" --desktop --out "$out"

mkdir -p web/public/guest
# -n drops the timestamp and source name; the container is not reproducible
# anyway, but a stable gzip envelope keeps diffs about the payload.
gzip -9 -n -c guest/out/snapshot-desktop.bin > "$seed.tmp"
mv -f "$seed.tmp" "$seed"
echo "wrote $seed ($(du -h "$seed" | cut -f1) compressed)"
