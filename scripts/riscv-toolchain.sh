#!/usr/bin/env bash
# Shared bare-metal RISC-V toolchain resolution. Sourced, not executed.
#
# Resolution order:
#   1. $RISCV_TOOLPREFIX, if set.
#   2. A host riscv64-elf- / riscv64-unknown-elf- GCC.
#   3. The pinned container toolchain from docker/riscv-toolchain/Dockerfile.
#
# FORCE_TOOLCHAIN_CONTAINER=1 skips 1 and 2. Either path runs the same make and
# produces the same outputs in the same vendor trees.

TOOLCHAIN_IMAGE="${TOOLCHAIN_IMAGE:-emulate-riscv-toolchain:trixie}"

# Populates $toolchain_prefix and $toolchain_in_container (0 or 1).
resolve_riscv_toolchain() {
  toolchain_in_container=0

  if [[ "${FORCE_TOOLCHAIN_CONTAINER:-0}" != "1" ]]; then
    if [[ -n "${RISCV_TOOLPREFIX:-}" ]]; then
      toolchain_prefix="$RISCV_TOOLPREFIX"
      return 0
    elif command -v riscv64-elf-gcc > /dev/null 2>&1; then
      toolchain_prefix=riscv64-elf-
      return 0
    elif command -v riscv64-unknown-elf-gcc > /dev/null 2>&1; then
      toolchain_prefix=riscv64-unknown-elf-
      return 0
    fi
  fi

  if ! command -v docker > /dev/null 2>&1; then
    echo "error: no RISC-V bare-metal GCC found" >&2
    echo "set RISCV_TOOLPREFIX, install riscv64-unknown-elf-gcc, or install" >&2
    echo "Docker and run: just toolchain-image" >&2
    exit 2
  fi
  if ! docker image inspect "$TOOLCHAIN_IMAGE" > /dev/null 2>&1; then
    echo "error: no host RISC-V GCC and $TOOLCHAIN_IMAGE is not built" >&2
    echo "run: just toolchain-image" >&2
    exit 2
  fi

  toolchain_prefix=riscv64-unknown-elf-
  toolchain_in_container=1
}

# Identity of the resolved toolchain, used as the stamp contents below.
riscv_toolchain_id() {
  if [[ "$toolchain_in_container" == "1" ]]; then
    printf 'container:%s\n' "$TOOLCHAIN_IMAGE"
  else
    printf 'host:%s\n' "$toolchain_prefix"
  fi
}

# Clean a vendor tree when the toolchain it was last built with has changed.
#
# Both upstream Makefiles emit `.d` files holding absolute paths into the
# compiler's include directory, which exist on only one side of the host/
# container split; switching sides then dies at Makefile parse time. Deleting
# the `.d` files first is what makes the following `make clean` parseable.
#
# $1 is the vendor directory; remaining arguments are extra make variables the
# tree's clean target needs.
riscv_toolchain_guard() {
  local directory="$1"
  shift
  local stamp="$directory/.emulate-toolchain"
  local identity previous
  identity="$(riscv_toolchain_id)"
  previous="$(cat "$stamp" 2> /dev/null || true)"

  # An absent stamp counts as a mismatch.
  if [[ "$previous" != "$identity" ]]; then
    echo "toolchain changed (${previous:-unknown} -> $identity); cleaning $directory"
    find "$directory" -name '*.d' -delete
    riscv_toolchain_make -C "${directory#"$root"/}" "$@" clean > /dev/null 2>&1 || true
  fi
  printf '%s\n' "$identity" > "$stamp"
}

# Run make on the host or inside the toolchain container. Arguments are a make
# command line whose paths must be relative to the repository root.
riscv_toolchain_make() {
  if [[ "$toolchain_in_container" == "1" ]]; then
    docker run --rm \
      --user "$(id -u):$(id -g)" \
      -v "$root:/work" -w /work \
      "$TOOLCHAIN_IMAGE" \
      make "$@"
  else
    (cd "$root" && make "$@")
  fi
}
