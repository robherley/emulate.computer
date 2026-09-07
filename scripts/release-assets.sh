#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."

case "${1:-}" in
  pack)
    mkdir -p target/release-assets
    COPYFILE_DISABLE=1 tar -czf target/release-assets/emulate-assets.tar.gz \
      web/generated-public/guest web/src/generated/guest.json web/src/wasm
    cd target/release-assets
    printf '%s  emulate-assets.tar.gz\n' "$(openssl dgst -sha256 -r emulate-assets.tar.gz | awk '{print $1}')" > emulate-assets.tar.gz.sha256
    ;;
  fetch)
    : "${ASSET_RELEASE:?Set ASSET_RELEASE to a published assets-<commit> GitHub release tag}"
    [[ "$ASSET_RELEASE" =~ ^assets-[a-f0-9]{40}$ ]] || { echo 'Invalid ASSET_RELEASE tag' >&2; exit 2; }
    base="https://github.com/${ASSET_REPOSITORY:-robherley/emulate.computer}/releases/download/$ASSET_RELEASE"
    scratch="$(mktemp -d)"
    trap 'rm -rf "$scratch"' EXIT
    for file in emulate-assets.tar.gz emulate-assets.tar.gz.sha256; do
      curl --fail --location --retry 3 --proto '=https' --proto-redir '=https' "$base/$file" -o "$scratch/$file"
    done
    read -r expected filename < "$scratch/emulate-assets.tar.gz.sha256"
    [[ "$expected" =~ ^[a-f0-9]{64}$ && "$filename" == emulate-assets.tar.gz ]] || exit 1
    [[ "$(openssl dgst -sha256 -r "$scratch/emulate-assets.tar.gz" | awk '{print $1}')" == "$expected" ]] || { echo "Release checksum mismatch" >&2; exit 1; }
    tar -tzf "$scratch/emulate-assets.tar.gz" | while IFS= read -r path; do
      [[ "$path" != *../* ]] || exit 1
      case "$path" in
        web/generated-public/guest|web/generated-public/guest/*|web/src/generated/guest.json|web/src/wasm|web/src/wasm/*) ;;
        *) echo "Unexpected release entry: $path" >&2; exit 1 ;;
      esac
    done
    tar -tvzf "$scratch/emulate-assets.tar.gz" | awk 'substr($0,1,1) !~ /[-d]/ {exit 1}'
    rm -rf web/generated-public/guest web/src/generated web/src/wasm
    tar -xzf "$scratch/emulate-assets.tar.gz"
    ;;
  *) echo 'Usage: release-assets.sh pack|fetch' >&2; exit 2 ;;
esac
