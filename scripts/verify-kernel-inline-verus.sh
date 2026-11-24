#!/usr/bin/env bash
set -euo pipefail

if [[ $# != 1 || ( $1 != all && $1 != version_check ) ]]; then
  echo "usage: $0 {version_check|all}" >&2
  exit 2
fi
cd "$(dirname "$0")/.."
repo_root="$PWD"
tool_root="$(realpath -m "${NORN_VERIFICATION_TOOL_ROOT:-$repo_root/target/verification-tools}")"
scripts/bootstrap-verification-tools.sh --check-verus
export RUSTUP_HOME="$tool_root/rustup"
export VERUS_ROOT="$tool_root/verus-x86-linux"
export VERUS_Z3_PATH="$VERUS_ROOT/z3"
export VERUS_USE_RUSTUP=1
unset VARGO_TARGET_DIR __VERUS_DRIVER_VIA_CARGO__
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT

python3 -I -B -X "pycache_prefix=$work/pycache" "$repo_root/scripts/kernel_inline_verus.py" "$repo_root" "$tool_root" "$work"
