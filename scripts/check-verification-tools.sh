#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
tool_root="${NORN_VERIFICATION_TOOL_ROOT:-$repo_root/target/verification-tools}"
export PATH="$tool_root/bin:$PATH"
export ELAN_HOME="$tool_root/elan"
export KANI_HOME="$tool_root/kani-home"
export RUSTUP_HOME="$tool_root/rustup"

require_version() {
  local name="$1"
  local expected="$2"
  shift 2
  local output
  output="$("$@" 2>&1)"
  if [[ "$output" != *"$expected"* ]]; then
    printf '%s version mismatch: expected output containing %q, got:\n%s\n' "$name" "$expected" "$output" >&2
    exit 1
  fi
  printf '%-8s %s\n' "$name" "$output"
}

require_version Lean "4.33.1" lean --version
require_version Quint "0.32.0" quint --version
require_version Kani "0.67.0" kani --version
require_version Z3 "5.1.0" z3 --version
require_version Java "21.0.12" java -version
