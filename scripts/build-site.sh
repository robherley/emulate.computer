#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."

for artifact in web/src/generated/guest.json web/src/wasm/emulate_wasm.js web/src/wasm/emulate_wasm_bg.wasm; do
  [[ -f "$artifact" ]] || { echo "error: $artifact is missing; run just wasm site-prepare" >&2; exit 1; }
done
bash scripts/prepare-site.sh public
cd web
npm exec --no -- vite build
