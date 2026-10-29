#!/usr/bin/env bash
# Fails if crates/norn-checker/tests/non_code_literals.baseline.json gained any
# entry relative to a base git ref (default: origin/main). The non-code literal
# allowlist of the code-registry scan may only lose entries, so a new
# diagnostic code cannot bypass the registry by being listed as a non-code.
# Usage: non-code-literals-shrink-only.sh [base-ref]
set -euo pipefail

base="${1:-origin/main}"
file="crates/norn-checker/tests/non_code_literals.baseline.json"
if ! base_json="$(git show "${base}:${file}" 2>/dev/null)"; then
  echo "${file} does not exist on ${base}; nothing to compare (first introduction)."
  exit 0
fi
added="$(jq -rn --argjson old "${base_json}" --slurpfile new "${file}" '
  (($new[0].non_code_literals // []) - ($old.non_code_literals // []))[]')"
if [[ -n "${added}" ]]; then
  printf '::error::%s gained entries relative to %s (baseline is shrink-only; register new codes in crates/norn-checker/src/codes.rs):\n%s\n' "${file}" "${base}" "${added}"
  exit 1
fi
echo "${file}: no entries added relative to ${base}."
