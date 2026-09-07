#!/usr/bin/env bash
# Clone/checkout a pinned upstream repository into vendor/<name>.

set -euo pipefail

root="$(cd "$(dirname "$0")/.." && pwd)"

case "${1:-}" in
  riscv-tests)
    name=riscv-tests
    url=https://github.com/riscv-software-src/riscv-tests.git
    commit=2ebecad997fa58cd9e5724340ba75aa4b59bd1d0
    ;;
  act4)
    name=riscv-arch-test
    url=https://github.com/riscv/riscv-arch-test.git
    commit=2c746647451151af837f09c8ee0f38a8f6127dbc
    ;;
  xv6)
    name=xv6-riscv
    url=https://github.com/mit-pdos/xv6-riscv.git
    commit=35b088427ef37611c38afdeed5a52a278cae38f9
    ;;
  *)
    echo "usage: $0 riscv-tests|act4|xv6" >&2
    exit 2
    ;;
esac

destination="$root/vendor/$name"
if [[ ! -d "$destination/.git" ]]; then
  if [[ -e "$destination" ]] && [[ -n "$(find "$destination" -mindepth 1 -maxdepth 1 -print -quit)" ]]; then
    echo "error: $destination exists but is not a Git checkout" >&2
    exit 2
  fi
  mkdir -p "$root/vendor"
  git clone --filter=blob:none "$url" "$destination"
fi

actual="$(git -C "$destination" rev-parse HEAD)"
if [[ "$actual" != "$commit" ]]; then
  git -C "$destination" fetch --depth 1 origin "$commit"
  git -C "$destination" checkout --detach "$commit"
fi

git -C "$destination" submodule update --init --recursive
actual="$(git -C "$destination" rev-parse HEAD)"
[[ "$actual" == "$commit" ]] || {
  echo "error: $name is at $actual, expected $commit" >&2
  exit 2
}
echo "$name pinned at $actual"
