#!/usr/bin/env bash
# Build the physical RV64 riscv-tests corpus into vendor/riscv-tests/isa.
#
# Uses a host riscv64-elf-/riscv64-unknown-elf- GCC when installed, otherwise
# the pinned container toolchain; FORCE_TOOLCHAIN_CONTAINER=1 forces the latter.

set -euo pipefail

root="$(cd "$(dirname "$0")/.." && pwd)"
isa_dir="$root/vendor/riscv-tests/isa"
jobs="${BUILD_JOBS:-4}"

# shellcheck source=scripts/riscv-toolchain.sh
source "$root/scripts/riscv-toolchain.sh"

if [[ ! -d "$isa_dir" ]]; then
  echo "error: $isa_dir is missing; run: just vendor riscv-tests" >&2
  exit 2
fi

resolve_riscv_toolchain
riscv_toolchain_guard "$isa_dir" XLEN=64

riscv_toolchain_make -C vendor/riscv-tests/isa \
  "RISCV_PREFIX=$toolchain_prefix" \
  XLEN=64 \
  "RISCV_GCC_OPTS=-static -mcmodel=medany -fvisibility=hidden -nostdlib -nostartfiles -ffreestanding -I../../libc-shim/include" \
  rv64ui rv64um rv64ua rv64uf rv64ud rv64uc rv64mi rv64si rv64uzbb rv64uzbc rv64uzbkb \
  -j"$jobs"
