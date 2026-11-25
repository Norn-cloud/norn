#!/usr/bin/env bash
set -euo pipefail

repo="$(cd "$(dirname "$0")/.." && pwd)"
tools="$(realpath -m "${NORN_VERIFICATION_TOOL_ROOT:-$repo/target/verification-tools}")"
export RUSTUP_HOME="$tools/rustup"
export KANI_HOME="$tools/kani-home"
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT
cd "$work"
"$tools/bin/kani" --fail-fast --output-format terse --target-dir "$work/target" \
  "$repo/verification/kernel-l1/version_kani.rs" | tee result.log
# A successful exit with a renamed or missing harness loses a second-lens obligation.
diff -u - <(sed -n 's/^Checking harness \(.*\)\.\.\.$/\1/p' result.log | LC_ALL=C sort) <<'HARNESSES'
exhaustion_is_exact
id_stable_genesis_and_live_rows_are_never_deleted
id_stable_retained_absence_is_a_deletion
successor_has_no_gaps
successor_never_wraps
HARNESSES
grep -Fx 'Complete - 5 successfully verified harnesses, 0 failures, 5 total.' result.log
