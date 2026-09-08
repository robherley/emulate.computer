#!/usr/bin/env bash
set -euo pipefail
export LC_ALL=C

root="$(cd "$(dirname "$0")/.." && pwd)"
cd "$root/guest"
recipe=prebuilt/Dockerfile
archive=prebuilt/desktop.tar.gz
manifest=prebuilt/manifest.json
platform=linux/riscv64

fail() { echo "error: $*" >&2; exit 1; }
command -v jq >/dev/null || fail "jq is required to read and generate the prebuilt manifest"
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT

hash() { shasum -a 256 "$1" | awk '{print $1}'; }

build_info() {
  local alpine
  alpine="$(sed -n 's/^ARG ALPINE_BASE=//p' "$recipe")"
  [[ -n "$alpine" && "$alpine" = "$(sed -n 's/^ARG ALPINE_BASE=//p' Dockerfile)" ]] \
    || fail "Guest and prebuilt Alpine bases differ; update both Dockerfiles and rebuild the binaries"
  { find patches -type f; printf '%s\n' "$recipe"; } | sort > "$work/paths"
  while IFS= read -r path; do
    jq -n --arg path "$path" --arg hash "$(hash "$path")" '{key: $path, value: $hash}'
  done < "$work/paths" | jq -s 'from_entries' > "$work/inputs.json"
  sed -n 's/^ARG \([A-Za-z0-9_]*\)=/\1=/p' "$recipe" | jq -Rn \
    '[inputs | capture("^(?<key>[^=]+)=(?<value>.*)$")] | from_entries' > "$work/sources.json"
  jq -n --arg platform "$platform" --arg base "$alpine" --arg recipe "$recipe" \
    --slurpfile sources "$work/sources.json" --slurpfile inputs "$work/inputs.json" \
    '{platform: $platform, base: $base, recipe: $recipe, sources: $sources[0], inputs: $inputs[0]}'
}

checksums() {
  jq -r '.inputs | to_entries[] | "\(.value)  \(.key)"' "$manifest"
  printf '%s  %s\n' "$(hash "$archive")" "$archive" "$(hash "$manifest")" "$manifest"
}

verify() {
  build_info > "$work/current.json"
  jq --arg path "$archive" --arg hash "$(hash "$archive")" \
    --argjson bytes "$(wc -c < "$archive")" \
    '. + {archive: {path: $path, bytes: $bytes, sha256: $hash}}' "$work/current.json" \
    | jq -S . > "$work/expected.json"
  jq -S . "$manifest" > "$work/actual.json"
  cmp -s "$work/expected.json" "$work/actual.json" \
    || fail "Prebuilt manifest or archive is stale; run just guest-prebuilt"
  checksums > "$work/SHA256SUMS"
  cmp -s "$work/SHA256SUMS" prebuilt/SHA256SUMS \
    || fail "Prebuilt checksum list differs from the manifest; run just guest-prebuilt"
  printf 'Verified %s (%s bytes)\n' "$archive" "$(jq -r '.archive.bytes' "$manifest")"
}

build() {
  build_info > "$work/before.json"
  mkdir -p out/prebuilt
  docker buildx build --platform "$platform" --file "$recipe" --target out \
    --output type=local,dest=out/prebuilt .
  build_info > "$work/after.json"
  cmp -s "$work/before.json" "$work/after.json" \
    || fail "Prebuilt inputs changed during compilation; run just guest-prebuilt again"
  cp out/prebuilt/desktop.tar.gz "$archive"
  jq --arg path "$archive" --arg hash "$(hash "$archive")" \
    --argjson bytes "$(wc -c < "$archive")" \
    '. + {archive: {path: $path, bytes: $bytes, sha256: $hash}}' "$work/before.json" > "$manifest"
  checksums > prebuilt/SHA256SUMS
  verify
}

case "${1:-verify}" in
  verify) verify ;;
  build) build ;;
  *) fail "Usage: bash scripts/prebuilt.sh [verify|build]" ;;
esac
