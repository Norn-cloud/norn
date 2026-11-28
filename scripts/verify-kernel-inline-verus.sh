#!/usr/bin/env bash
set -euo pipefail

if [[ $# != 1 || ( $1 != all && $1 != version_check && $1 != artifact_governance ) ]]; then
  echo "usage: $0 {version_check|artifact_governance|all}" >&2
  exit 2
fi
cd "$(dirname "$0")/.."
repo_root="$PWD"
tool_root="$(realpath -m "${NORN_VERIFICATION_TOOL_ROOT:-$repo_root/target/verification-tools}")"
scripts/bootstrap-verification-tools.sh --check-verus
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT

python3 -I -B -X "pycache_prefix=$work/pycache" "$repo_root/scripts/kernel_inline_verus.py" "$repo_root" "$tool_root" "$work" "$1"
