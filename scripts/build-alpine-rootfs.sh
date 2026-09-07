#!/usr/bin/env bash
# Turn guest/out/rootfs.tar into guest/out/alpine-rootfs.ext4, and install its
# gzipped browser seed into web/public/guest/rootfs.ext4.gz.
#
# The root filesystem's *content* is built by the `rootfs` stage of
# guest/Dockerfile (packages and the /etc overlay) and handed
# over as guest/out/rootfs.tar by `scripts/fetch-guest.sh`. All this script owns
# is the filesystem: normalization, mke2fs, gzip and content identity.
#
# The mke2fs step runs in a pinned native-architecture Alpine container, via
# scripts/build-alpine-rootfs-container.sh, so macOS needs neither root nor host
# e2fsprogs.
#
# The image is byte-reproducible for an unchanged tar; ROOTFS_VERIFY_REPRODUCIBLE=1
# formats twice and compares SHA-256. The post-boot snapshot installed at the
# end is deliberately outside that property: it carries a wall clock and a timer
# value, so it is deterministic in behaviour but not byte-identical between
# runs. The ext4 stays reproducible because the capture runs on a scratch copy. Upstream is the one thing not pinnable:
# Alpine rebuilding a package at a new -rN changes the content, which the pinned
# top-level versions in guest/Dockerfile turn into a build failure rather than
# silent drift.

set -euo pipefail

root="$(cd "$(dirname "$0")/.." && pwd)"
image="$root/guest/out/alpine-rootfs.ext4"
rootfs_tar="$root/guest/out/rootfs.tar"
size_mb="${ROOTFS_SIZE_MB:-512}"
cache="$root/guest/out/.cache"

if ! command -v docker >/dev/null 2>&1; then
  echo "error: docker is required to build the guest root filesystem" >&2
  echo "install Docker Desktop (or another Docker engine) and retry" >&2
  exit 2
fi

# The mke2fs stage runs natively; only the guest content is emulated.
case "$(uname -m)" in
  x86_64 | amd64)
    platform="linux/amd64"
    builder="alpine@sha256:7c8cb692ae09657cbc4a3f3cbd0e8d5a2690ba38386aaaf252dbb060bf5eb2e6"
    ;;
  arm64 | aarch64)
    platform="linux/arm64"
    builder="alpine@sha256:2c9d26f410d032d5b1525aa8a873e238b05b90c4ae8618743d4311f0cc827e37"
    ;;
  *)
    echo "error: unsupported builder architecture: $(uname -m)" >&2
    exit 2
    ;;
esac

mkdir -p "$cache"

make_ext4() {
  local tar_path="$1" out_path="$2"
  rm -f "$out_path"
  truncate -s "${size_mb}M" "$out_path"
  docker run --rm --platform "$platform" \
    -v "$root:/work" -w /work "$builder" \
    sh scripts/build-alpine-rootfs-container.sh \
    "${tar_path#"$root"/}" "${out_path#"$root"/}"
}

# ROOTFS_REUSE_IMAGE=1 skips the format when the ext4 is already built, so
# re-installing the browser seed (the gzip + copy below) costs a few seconds.
if [[ "${ROOTFS_REUSE_IMAGE:-0}" == "1" && -f "$image" ]]; then
  echo "reusing existing guest/out/alpine-rootfs.ext4"
else
  if [[ ! -f "$rootfs_tar" ]]; then
    echo "guest/out/rootfs.tar is missing; building the guest artifacts first ..."
    bash "$root/scripts/fetch-guest.sh"
  fi
  make_ext4 "$rootfs_tar" "$image"
fi

if [[ "${ROOTFS_VERIFY_REPRODUCIBLE:-0}" == "1" ]]; then
  echo "verifying byte-for-byte reproducibility ..."
  second_image="$cache/alpine-rootfs-verify.ext4"
  make_ext4 "$rootfs_tar" "$second_image"
  first_sum="$(shasum -a 256 "$image" | awk '{print $1}')"
  second_sum="$(shasum -a 256 "$second_image" | awk '{print $1}')"
  rm -f "$second_image"
  if [[ "$first_sum" != "$second_sum" ]]; then
    echo "error: reformat is not byte-identical ($first_sum != $second_sum)" >&2
    exit 2
  fi
  echo "reproducible: $first_sum"
fi

echo "wrote guest/out/alpine-rootfs.ext4 (${size_mb} MiB logical, sparse on disk)"

mkdir -p "$root/web/public/guest"
seed="$root/web/public/guest/rootfs.ext4.gz"
seed_hash="$(shasum -a 256 "$image" | awk '{print $1}')"
# Hash the uncompressed disk so compression settings do not affect identity.
gzip -9 -n -c "$image" > "$seed.tmp"
mv -f "$seed.tmp" "$seed"
printf '%s\n' "$seed_hash" > "$root/web/public/guest/rootfs.sha256.tmp"
mv -f "$root/web/public/guest/rootfs.sha256.tmp" "$root/web/public/guest/rootfs.sha256"
cp "$root/guest/out/virt-rootfs.dtb" "$root/web/public/guest/dtb-rootfs.bin"
echo "wrote web/public/guest/rootfs.ext4.gz ($(du -h "$seed" | cut -f1) compressed; sha256:$seed_hash)"

if [[ "${ROOTFS_SKIP_SNAPSHOT:-0}" == "1" ]]; then
  rm -f "$root/web/public/guest/snapshot.bin.gz"
  echo "note: ROOTFS_SKIP_SNAPSHOT=1, not capturing a boot snapshot"
else
  bash "$root/scripts/build-snapshot.sh"
fi
