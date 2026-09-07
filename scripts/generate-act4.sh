#!/usr/bin/env bash
# Generate ACT4 self-checking ELFs into target/act4/<profile>/ using the pinned
# upstream build image.

set -euo pipefail

root="$(cd "$(dirname "$0")/.." && pwd)"
source_dir="$root/vendor/riscv-arch-test"
work_dir="$root/target/act4"
container_lock="$work_dir/Gemfile.lock"
expected_commit="2c746647451151af837f09c8ee0f38a8f6127dbc"
image="ghcr.io/riscv/act4-build:act4@sha256:f937f8fd064b7be8108e7b940972eb38e732869f0b00a39889b9a371f5f39599"
profile="${ACT4_PROFILE:-unpriv}"
jobs="${ACT4_JOBS:-0}"
extensions="${ACT4_EXTENSIONS:-}"
fast="${ACT4_FAST:-}"

usage() {
  echo "usage: $0 [--profile unpriv|priv] [--extensions LIST] [--jobs N] [--fast]"
  echo
  echo "Generate ACT4 self-checking ELFs with the pinned official build image."
  echo "The default profile is unpriv; priv selects emulate-rv64gc-priv."
  echo
  echo "environment: ACT4_PROFILE, ACT4_EXTENSIONS, ACT4_JOBS, ACT4_FAST"
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    --profile)
      [[ $# -ge 2 ]] || { echo "error: --profile requires a value" >&2; exit 2; }
      profile="$2"
      shift 2
      ;;
    --extensions)
      [[ $# -ge 2 ]] || { echo "error: --extensions requires a value" >&2; exit 2; }
      extensions="$2"
      shift 2
      ;;
    --jobs)
      [[ $# -ge 2 ]] || { echo "error: --jobs requires a value" >&2; exit 2; }
      jobs="$2"
      shift 2
      ;;
    --fast)
      fast=True
      shift
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    *)
      echo "error: unknown argument: $1" >&2
      usage >&2
      exit 2
      ;;
  esac
done

case "$profile" in
  unpriv|emulate-rv64gc)
    profile_name="emulate-rv64gc"
    ;;
  priv|emulate-rv64gc-priv)
    profile_name="emulate-rv64gc-priv"
    ;;
  *)
    echo "error: unknown ACT4 profile: $profile" >&2
    echo "expected one of: unpriv, priv, emulate-rv64gc, emulate-rv64gc-priv" >&2
    exit 2
    ;;
esac
config_dir="$root/test/compliance/act4/$profile_name"

if [[ ! -d "$source_dir/.git" ]]; then
  echo "error: $source_dir is missing; clone riscv-arch-test first" >&2
  exit 2
fi

actual_commit="$(git -C "$source_dir" rev-parse HEAD)"
if [[ "$actual_commit" != "$expected_commit" ]]; then
  echo "error: riscv-arch-test is at $actual_commit" >&2
  echo "expected pinned ACT4 commit $expected_commit" >&2
  exit 2
fi

command -v docker >/dev/null 2>&1 || {
  echo "error: Docker is required to generate ACT4 ELFs" >&2
  exit 2
}

mkdir -p "$work_dir"
# Bundler 4 refreshes its own checksum in this pinned checkout. Give it a
# writable generated copy so running ACT4 never dirties the vendor tree.
cp "$source_dir/framework/src/act/data/Gemfile.lock" "$container_lock"

# ACT4's upstream test-generation stamp includes neither EXTENSIONS nor the
# external DUT configuration in its cache key. Keep selection- and
# configuration-specific stamps so neither a new test batch nor a profile
# change can silently reuse stale generated expectations.
stamp_selection="${extensions:-all}"
config_hash="$(shasum -a 256 "$config_dir"/* | shasum -a 256 | cut -c1-12)"
selection_hash="$(printf '%s' "$stamp_selection" | shasum -a 256 | cut -c1-12)"
stamp_key="$selection_hash-$config_hash"

make_args=(
  -C /act4
  CONFIG_FILES=/emulate-config/test_config.yaml
  WORKDIR=/work
  "STAMP_DIR=/work/stamps/$profile_name/$stamp_key"
  "JOBS=$jobs"
)
[[ -z "$extensions" ]] || make_args+=("EXTENSIONS=$extensions")
[[ -z "$fast" ]] || make_args+=("FAST=True")

echo "generating ACT4 ELFs for $profile_name with $image"
docker run --rm \
  --user "$(id -u):$(id -g)" \
  --volume "$source_dir:/act4" \
  --volume "$container_lock:/act4/framework/src/act/data/Gemfile.lock" \
  --volume "$config_dir:/emulate-config:ro" \
  --volume "$work_dir:/work" \
  "$image" \
  make "${make_args[@]}"
