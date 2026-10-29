#!/usr/bin/env bash
# Fails if crates/norn-skeleton-guard/baseline.json gained any entry relative
# to a base git ref (default: origin/main). The known-violation baseline may
# only lose entries. Usage: baseline-shrink-only.sh [base-ref]
set -euo pipefail

base="${1:-origin/main}"
file="crates/norn-skeleton-guard/baseline.json"
if ! base_json="$(git show "${base}:${file}" 2>/dev/null)"; then
  echo "${file} does not exist on ${base}; nothing to compare (first introduction)."
  exit 0
fi
added="$(jq -n --argjson old "${base_json}" --slurpfile new "${file}" '
  [ "norn_edges", "third_party", "norn_ir_validators" ][] as $k
  | (($new[0][$k] // []) - ($old[$k] // []))[]
  | "\($k): \(. | tostring)"')"
if [[ -n "${added}" ]]; then
  printf '::error::%s gained entries relative to %s (baseline is shrink-only):\n%s\n' "${file}" "${base}" "${added}"
  exit 1
fi
echo "${file}: no entries added relative to ${base}."
