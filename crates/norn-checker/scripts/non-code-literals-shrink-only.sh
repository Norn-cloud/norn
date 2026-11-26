#!/usr/bin/env bash
# Fails if crates/norn-checker/tests/non_code_literals.baseline.json gained any
# entry relative to a base git ref (default: origin/main). The non-code literal
# allowlist of the code-registry scan may only lose entries, so a new
# diagnostic code cannot bypass the registry by being listed as a non-code.
# Usage: non-code-literals-shrink-only.sh [base-ref]
set -euo pipefail

base="${1:-origin/main}"
file="crates/norn-checker/tests/non_code_literals.baseline.json"

if ! git rev-parse --verify "${base}^{commit}" >/dev/null 2>&1; then
  printf '::error::base ref %s does not resolve to a commit.\n' "${base}"
  exit 1
fi

if ! jq -ne --slurpfile new "${file}" '
  ($new | length == 1)
  and ($new[0] | type == "object")
  and ($new[0].non_code_literals | type == "array")
  and ($new[0].non_code_literals | all(.[]; type == "string"))
' >/dev/null 2>&1; then
  printf '::error::%s is empty, malformed, or violates expected baseline schema.\n' "${file}"
  exit 1
fi

if [[ -z "$(git ls-tree --name-only "${base}" -- "${file}")" ]]; then
  echo "${file} does not exist on ${base}; nothing to compare (first introduction)."
  exit 0
fi

base_json="$(git show "${base}:${file}")"
added="$(jq -rn --argjson old "${base_json}" --slurpfile new "${file}" '
  (($new[0].non_code_literals // []) - ($old.non_code_literals // []))[]')"
if [[ -n "${added}" ]]; then
  printf '::error::%s gained entries relative to %s (baseline is shrink-only; register new codes in crates/norn-checker/conformance/diagnostic-codes/):\n%s\n' "${file}" "${base}" "${added}"
  exit 1
fi
echo "${file}: no entries added relative to ${base}."
