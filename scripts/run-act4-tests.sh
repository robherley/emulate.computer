#!/usr/bin/env bash
# Run the generated ACT4 ELFs in target/act4/<profile>/elfs against emulate.

set -uo pipefail

root="$(cd "$(dirname "$0")/.." && pwd)"
emulate="$root/target/release/emulate-computer"
profile="${ACT4_PROFILE:-unpriv}"
timeout_mins="${TIMEOUT_MINS:-1}"
ram_mb="${ACT4_RAM_MB:-64}"
build=true
list_only=false
filters=()

usage() {
  echo "usage: $0 [--profile unpriv|priv] [--list] [--no-build] [--timeout-mins N] [--ram MB] [FILTER ...]"
  echo
  echo "Run generated ACT4 ELFs. FILTER is matched as a shell glob and substring"
  echo "against each ELF's basename and path relative to the ELF directory."
  echo
  echo "examples:"
  echo "  $0 I-add-01"
  echo "  $0 'M-*' 'F-*'"
  echo "  $0 --list Zicsr"
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    --profile)
      [[ $# -ge 2 ]] || { echo "error: --profile requires a value" >&2; exit 2; }
      profile="$2"
      shift 2
      ;;
    --list)
      list_only=true
      shift
      ;;
    --no-build)
      build=false
      shift
      ;;
    --timeout-mins)
      [[ $# -ge 2 ]] || { echo "error: --timeout-mins requires a value" >&2; exit 2; }
      timeout_mins="$2"
      shift 2
      ;;
    --ram)
      [[ $# -ge 2 ]] || { echo "error: --ram requires a value" >&2; exit 2; }
      ram_mb="$2"
      shift 2
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    --)
      shift
      filters+=("$@")
      break
      ;;
    -* )
      echo "error: unknown option: $1" >&2
      usage >&2
      exit 2
      ;;
    *)
      filters+=("$1")
      shift
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
elf_dir="$root/target/act4/$profile_name/elfs"

if [[ ! -d "$elf_dir" ]]; then
  echo "error: no generated ACT4 ELFs at $elf_dir" >&2
  if [[ "$profile_name" == emulate-rv64gc-priv ]]; then
    echo "run: just act4-generate --profile priv --extensions Sm" >&2
  else
    echo "run: just act4-generate" >&2
  fi
  exit 2
fi

matches_filter() {
  local relative="$1"
  local base="${relative##*/}"
  local filter

  [[ ${#filters[@]} -eq 0 ]] && return 0
  for filter in "${filters[@]}"; do
    if [[ "$relative" == $filter || "$base" == $filter || "$relative" == *"$filter"* || "$base" == *"$filter"* ]]; then
      return 0
    fi
  done
  return 1
}

tests=()
while IFS= read -r elf; do
  relative="${elf#"$elf_dir"/}"
  matches_filter "$relative" && tests+=("$elf")
done < <(find "$elf_dir" -type f -name '*.elf' | sort)

if [[ ${#tests[@]} -eq 0 ]]; then
  echo "error: no ACT4 ELFs matched${filters[*]:+: ${filters[*]}}" >&2
  exit 2
fi

if [[ "$list_only" == true ]]; then
  for elf in "${tests[@]}"; do
    echo "${elf#"$elf_dir"/}"
  done
  exit 0
fi

if [[ "$build" == true ]]; then
  echo "building emulate (release)..."
  cargo build --release --locked -p emulate-cli --manifest-path "$root/Cargo.toml" || exit $?
elif [[ ! -x "$emulate" ]]; then
  echo "error: $emulate does not exist; omit --no-build or build emulate first" >&2
  exit 2
fi

pass=0
fail=0
failed_tests=()
printf '\nrunning %d ACT4 tests (timeout %s min each)\n\n' "${#tests[@]}" "$timeout_mins"
for elf in "${tests[@]}"; do
  relative="${elf#"$elf_dir"/}"
  printf '%-52s ' "$relative"
  output="$("$emulate" test "$elf" --timeout-mins "$timeout_mins" --ram "$ram_mb" 2>&1)"
  rc=$?
  if [[ $rc -eq 0 ]] && grep -Eq '(^|[[:space:]])PASS([[:space:](]|$)' <<< "$output"; then
    echo PASS
    ((pass += 1))
  else
    summary="$(tail -n 1 <<< "$output")"
    [[ -n "$summary" ]] || summary="no output"
    echo "FAIL (rc=$rc: $summary)"
    if [[ -n "$output" ]]; then
      sed 's/^/    | /' <<< "$output"
    fi
    ((fail += 1))
    failed_tests+=("$relative")
  fi
done

echo
echo '==============================================='
printf 'total: %d   pass: %d   fail: %d\n' "$((pass + fail))" "$pass" "$fail"
if [[ $fail -gt 0 ]]; then
  echo
  echo 'failed:'
  printf '  %s\n' "${failed_tests[@]}"
  exit 1
fi
echo 'all ACT4 tests passed'
