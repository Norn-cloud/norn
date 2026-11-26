#!/usr/bin/env bash
set -euo pipefail

if [[ $# != 1 || ( $1 != all && $1 != overlay_core ) ]]; then
  echo "usage: $0 {overlay_core|all}" >&2
  exit 2
fi
cd "$(dirname "$0")/.."
repo_root="$PWD"
tool_root="$(realpath -m "${NORN_VERIFICATION_TOOL_ROOT:-$repo_root/target/verification-tools}")"
scripts/bootstrap-verification-tools.sh --check-verus
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT

python3 -I -B -X "pycache_prefix=$work/pycache" "$repo_root/scripts/kernel_overlay_verus.py" run "$repo_root" "$tool_root" "$work"
