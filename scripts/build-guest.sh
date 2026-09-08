#!/usr/bin/env bash
# Build guest images using verified prebuilt desktop binaries, then stage browser assets.

set -euo pipefail

root="$(cd "$(dirname "$0")/.." && pwd)"
cd "$root"

bash scripts/prebuilt.sh verify

if ! command -v docker >/dev/null 2>&1; then
  echo "error: docker is required to build the guest artifacts" >&2
  echo "install Docker Desktop (or another Docker engine) and retry" >&2
  exit 2
fi
if ! docker buildx version >/dev/null 2>&1; then
  echo "error: docker buildx is required to build for linux/riscv64" >&2
  echo "install the buildx plugin and retry" >&2
  exit 2
fi

mkdir -p guest/out
build_args=(--platform linux/riscv64 --target out --output type=local,dest=guest/out)
if [[ -n "${GUEST_BUILD_CACHE:-}" ]]; then
  if [[ -f "$GUEST_BUILD_CACHE/index.json" ]]; then
    build_args+=(--cache-from "type=local,src=$GUEST_BUILD_CACHE")
  fi
  rm -rf "$GUEST_BUILD_CACHE-next"
  build_args+=(--cache-to "type=local,dest=$GUEST_BUILD_CACHE-next,mode=max")
fi
if ! docker buildx build "${build_args[@]}" guest; then
  echo "error: docker buildx build failed" >&2
  echo "riscv64 emulation must be registered; check: docker run --rm --platform linux/riscv64 alpine:3.22.5 uname -m" >&2
  exit 2
fi

# Replace the cache rather than retaining unreferenced layers from previous builds.
if [[ -n "${GUEST_BUILD_CACHE:-}" ]]; then
  rm -rf "$GUEST_BUILD_CACHE"
  mv "$GUEST_BUILD_CACHE-next" "$GUEST_BUILD_CACHE"
fi

for artifact in fw.bin Image initramfs.cpio.gz virt.dtb virt-rootfs.dtb virt-desktop.dtb rootfs.tar; do
  [[ -f "guest/out/$artifact" ]] || {
    echo "error: the out stage did not produce guest/out/$artifact" >&2
    exit 2
  }
done

mkdir -p web/public/guest
cp guest/out/Image web/public/guest/kernel.bin
cp guest/out/virt.dtb web/public/guest/dtb.bin
cp guest/out/virt-rootfs.dtb web/public/guest/dtb-rootfs.bin
cp guest/out/virt-desktop.dtb web/public/guest/dtb-desktop.bin
cp guest/out/initramfs.cpio.gz web/public/guest/initrd.bin
cp guest/out/fw.bin web/public/guest/fw.bin
echo 'installed: web/public/guest/{fw,kernel,dtb,dtb-rootfs,dtb-desktop,initrd}.bin'
