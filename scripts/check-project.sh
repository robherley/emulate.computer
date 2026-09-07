#!/usr/bin/env bash
# Run formatting, shell syntax and workspace compile checks.

set -euo pipefail

root="$(cd "$(dirname "$0")/.." && pwd)"
cd "$root"

cargo fmt --all -- --check
just --fmt --check

while IFS= read -r script; do
  bash -n "$script"
done < <(find scripts guest -type f -name '*.sh' | sort)

sh -n guest/overlays/rootfs/usr/local/bin/emuctl

cargo clippy --workspace --all-targets --all-features --locked -- -D warnings
