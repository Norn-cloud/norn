#!/usr/bin/env bash
set -euo pipefail
repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
export ELAN_TOOLCHAIN=leanprover/lean4:v4.34.1
proof="$repo_root/verification/datalog-stratification"
result="${NORN_VERIFICATION_RESULT_ROOT:-$repo_root/target/verification-results}/datalog-stratification"
mkdir -p "$result"
cd "$proof"
[[ "$(cat lean-toolchain)" == "$ELAN_TOOLCHAIN" ]]
[[ "$(lean --version)" == "Lean (version 4.34.1,"* ]]
lean -DwarningAsError=true -o "$result/Stratification.olean" "$repo_root/verification/datalog-stratification/Stratification.lean"
LEAN_PATH="$result" lean -DwarningAsError=true "$repo_root/verification/datalog-stratification/Audit.lean" > "$result/axioms.txt"
# Exact axiom output is inspected, not a source grep presented as soundness.
diff -u axioms.txt "$result/axioms.txt"
if grep -q sorryAx "$result/axioms.txt"; then echo "sorryAx in axiom closure" >&2; exit 1; fi
printf 'datalog-stratification: model and exact axiom inventory passed\n'
