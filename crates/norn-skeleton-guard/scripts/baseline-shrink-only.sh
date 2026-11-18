#!/usr/bin/env bash
# Fails if crates/norn-skeleton-guard/baseline.json gained any entry relative
# to a base git ref (default: origin/main), or any `ratchets` count rose. The
# known-violation baseline may only lose entries and its counts only go down.
# A ratchet rule absent on the base is a first introduction and is not compared.
# Usage: baseline-shrink-only.sh [base-ref]
set -euo pipefail

base="${1:-origin/main}"
file="crates/norn-skeleton-guard/baseline.json"
if ! base_json="$(git show "${base}:${file}" 2>/dev/null)"; then
  echo "${file} does not exist on ${base}; nothing to compare (first introduction)."
  exit 0
fi
added="$(jq -n --argjson old "${base_json}" --slurpfile new "${file}" '
  ([ "norn_edges", "third_party", "norn_ir_validators" ][] as $k
    | (($new[0][$k] // []) - ($old[$k] // []))[]
    | "\($k): \(. | tostring)"),
  (($old.ratchets // {}) as $was
    | ($new[0].ratchets // {}) | to_entries[]
    | select($was[.key] != null) | .key as $rule
    | .value | to_entries[]
    | select(.value > ($was[$rule][.key] // 0))
    | "ratchets.\($rule): \(.key) rose from \($was[$rule][.key] // 0) to \(.value)")')"
if [[ -n "${added}" ]]; then
  printf '::error::%s gained entries relative to %s (baseline is shrink-only):\n%s\n' "${file}" "${base}" "${added}"
  exit 1
fi
echo "${file}: no entries added and no ratchet raised relative to ${base}."
