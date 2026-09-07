#!/usr/bin/env bash
# Build xv6 and its filesystem image into vendor/xv6-riscv.
#
# Uses a host riscv64-elf-/riscv64-unknown-elf- GCC when installed, otherwise
# the pinned container toolchain; FORCE_TOOLCHAIN_CONTAINER=1 forces the latter.

set -euo pipefail

root="$(cd "$(dirname "$0")/.." && pwd)"
xv6="$root/vendor/xv6-riscv"
jobs="${BUILD_JOBS:-4}"

# shellcheck source=scripts/riscv-toolchain.sh
source "$root/scripts/riscv-toolchain.sh"

if [[ ! -f "$xv6/Makefile" ]]; then
  echo "error: $xv6 is missing; run: just vendor xv6" >&2
  exit 2
fi

resolve_riscv_toolchain
riscv_toolchain_guard "$xv6" "TOOLPREFIX=$toolchain_prefix"

riscv_toolchain_make -C vendor/xv6-riscv "TOOLPREFIX=$toolchain_prefix" -j"$jobs"
riscv_toolchain_make -C vendor/xv6-riscv "TOOLPREFIX=$toolchain_prefix" fs.img -j"$jobs"
