#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."

environment="${1:-production}"
case "$environment" in
  preview|production) ;;
  *) echo "Usage: bash scripts/publish-site.sh [preview|production]" >&2; exit 1 ;;
esac

if [[ -z "${BLOB_READ_WRITE_TOKEN:-}" ]]; then
  env_file=".vercel/.env.$environment.local"
  [[ -f "$env_file" ]] || { echo "Run vc pull --environment=$environment first" >&2; exit 1; }
  set -a
  source "$env_file"
  set +a
fi
: "${BLOB_READ_WRITE_TOKEN:?Connect a public Blob store and run vc pull again}"
# Pulled deployment credentials can include an unrelated OIDC token.
unset VERCEL_OIDC_TOKEN BLOB_STORE_ID

bash scripts/prepare-site.sh
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT
manifest=web/src/generated/guest.json
cp "$manifest" "$work/guest.json"

for name in rootfs.ext4.gz snapshot.bin.gz; do
  local_url="$(jq -r --arg name "$name" '.files[$name].url' "$work/guest.json")"
  pathname="${local_url#/}"
  file="web/generated-public/$pathname"
  vc blob list --prefix "$pathname" --limit 2 --no-color > "$work/result" 2>&1 \
    || { cat "$work/result" >&2; exit 1; }
  url="$(sed -nE 's|.*(https://[a-zA-Z0-9]+\.public\.blob\.vercel-storage\.com/guest/[^[:space:]]+).*|\1|p' "$work/result")"
  if [[ -z "$url" ]]; then
    vc blob put "$file" --pathname "$pathname" --access public \
      --cache-control-max-age 31536000 --content-type application/gzip --no-color \
      > "$work/result" 2>&1 || { cat "$work/result" >&2; exit 1; }
    url="$(sed -nE 's|.*(https://[a-zA-Z0-9]+\.public\.blob\.vercel-storage\.com/guest/[^[:space:]]+).*|\1|p' "$work/result")"
  fi
  [[ "$url" = https://*.public.blob.vercel-storage.com/"$pathname" && "$url" != *$'\n'* ]] \
    || { echo "Invalid Blob URL for $pathname" >&2; exit 1; }
  jq --arg name "$name" --arg url "$url" '.files[$name].url = $url' \
    "$work/guest.json" > "$work/next.json"
  mv "$work/next.json" "$work/guest.json"
  echo "$name: $url"
done

mv "$work/guest.json" "$manifest"
for name in rootfs.ext4.gz snapshot.bin.gz; do
  pathname="$(jq -r --arg name "$name" '.files[$name].url | split("/")[-1]' "$manifest")"
  rm "web/generated-public/guest/$pathname"
done
