#!/usr/bin/env bash
set -euo pipefail
repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
reference="$repo_root/verification/kernel-l1"
result="${NORN_VERIFICATION_RESULT_ROOT:-$repo_root/target/verification-results}/kernel-l1"
mkdir -p "$result"
cd "$reference"
[[ "$(lean --version)" == "Lean (version 4.34.1,"* ]]
lean -DwarningAsError=true -o "$result/Reference.olean" Reference.lean
LEAN_PATH="$result" lean -DwarningAsError=true Audit.lean > "$result/axioms.txt"
# Exact axiom output is inspected, not a source grep presented as soundness.
diff -u axioms.txt "$result/axioms.txt"
lean -DwarningAsError=true --run Reference.lean > "$result/corpus.tsv"
diff -u corpus.tsv "$result/corpus.tsv"
printf 'kernel-l1: reference, exact axiom inventory and executable corpus passed\n'
