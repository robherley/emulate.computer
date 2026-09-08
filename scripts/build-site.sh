#!/usr/bin/env bash
set -euo pipefail
export LC_ALL=C
cd "$(dirname "$0")/.."

input=web/public/guest
output=web/generated-public
guest_files=(fw.bin kernel.bin initrd.bin dtb.bin dtb-desktop.bin dtb-rootfs.bin rootfs.ext4.gz snapshot.bin.gz)

fail() { echo "error: $*" >&2; exit 1; }
hash() { shasum -a 256 "$1" | awk '{print $1}'; }
size() { wc -c < "$1" | tr -d '[:space:]'; }

copy_public() {
  mkdir -p "$output"
  shopt -s dotglob nullglob
  for path in "$output"/*; do
    [[ "${path##*/}" = guest ]] || rm -rf "$path"
  done
  for path in web/public/*; do
    [[ "${path##*/}" = guest ]] || cp -R "$path" "$output/"
  done
  shopt -u dotglob nullglob
}

# Snapshot lengths and image-hash prefixes are little-endian on every host.
u32() {
  od -An -tu1 -j "$2" -N 4 "$1" | awk 'NF {printf "%.0f\n", $1 + $2*256 + $3*65536 + $4*16777216}'
}
u64_prefix() {
  local value="$1" byte
  for byte in 0 1 2 3 4 5 6 7; do
    printf '%b' "\\$(printf '%03o' "$((value & 255))")"
    value=$((value >> 8))
  done
}

prepare_guest() {
  command -v jq >/dev/null || fail "jq is required to generate the guest manifest"
  work="$(mktemp -d)"
  trap 'rm -rf "$work"' EXIT
  local name disk_hash logical_bytes image_hash disk_id snapshot_bytes offset length sha filename
  for name in "${guest_files[@]}"; do
    [[ -f "$input/$name" ]] || fail "Missing $input/$name; run just guest-rootfs"
  done
  disk_hash="$(tr -d '[:space:]' < "$input/rootfs.sha256")"
  gzip -dc "$input/rootfs.ext4.gz" > "$work/disk"
  logical_bytes="$(size "$work/disk")"
  [[ "$logical_bytes" = 536870912 && "$disk_hash" =~ ^[a-f0-9]{64}$ \
    && "$(hash "$work/disk")" = "$disk_hash" ]] \
    || fail "Rootfs size or checksum mismatch; run just guest-rootfs"
  rm "$work/disk"

  for name in fw.bin kernel.bin initrd.bin dtb-desktop.bin; do
    u64_prefix "$(size "$input/$name")"
    cat "$input/$name"
  done > "$work/images"
  image_hash="$(hash "$work/images")"
  disk_id="sha256:$disk_hash"
  gzip -dc "$input/snapshot.bin.gz" > "$work/snapshot"
  snapshot_bytes="$(size "$work/snapshot")"
  [[ "$snapshot_bytes" -ge 173 \
    && "$(head -c 8 "$work/snapshot")" = EMUSNAP1 \
    && "$(u32 "$work/snapshot" 50)" = "${#disk_id}" \
    && "$(dd if="$work/snapshot" bs=1 skip=54 count=71 2>/dev/null)" = "$disk_id" \
    && "$(od -An -v -tx1 -j 18 -N 32 "$work/snapshot" | tr -d '[:space:]')" = "$image_hash" ]] \
    || fail "Snapshot does not match the disk and boot images; run just guest-rootfs"
  offset=12
  while ((offset < snapshot_bytes)); do
    ((offset + 6 <= snapshot_bytes)) || fail "Truncated snapshot section"
    length="$(u32 "$work/snapshot" "$((offset + 2))")"
    offset=$((offset + 6 + length))
    ((offset <= snapshot_bytes)) || fail "Truncated snapshot section"
  done

  rm -rf "$output/guest"
  mkdir -p "$output/guest" web/src/generated
  for name in "${guest_files[@]}"; do
    sha="$(hash "$input/$name")"
    filename="${name%%.*}.$sha.${name#*.}"
    cp "$input/$name" "$output/guest/$filename"
    jq -n --arg key "$name" --arg url "/guest/$filename" --arg sha "$sha" \
      --argjson bytes "$(size "$input/$name")" \
      '{key: $key, value: {url: $url, bytes: $bytes, sha256: $sha}}'
  done | jq -s 'from_entries' > "$work/files.json"
  jq -n --arg id "$disk_id" --arg hash "$disk_hash" --argjson bytes "$logical_bytes" \
    --slurpfile files "$work/files.json" \
    '{disk: {id: $id, hash: $hash, logicalBytes: $bytes}, files: $files[0]}' \
    > web/src/generated/guest.json
}

case "${1:-local}" in
  local)
    wasm-pack build crates/emulate-wasm --target web --out-dir ../../web/src/wasm
    prepare_guest
    npm run build --workspace web
    ;;
  release)
    bash scripts/release-assets.sh fetch
    npm run build --workspace web
    ;;
  prepare) prepare_guest; copy_public ;;
  public) copy_public ;;
  *) fail "Usage: bash scripts/build-site.sh [local|release|prepare|public]" ;;
esac
