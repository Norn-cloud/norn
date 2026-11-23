#!/usr/bin/env bash
# Fails if crates/norn-skeleton-guard/baseline.json gained any entry or raised
# any `ratchets` count relative to a base git ref (default: origin/main), or if
# crates/norn-skeleton-guard/loc-baseline.json gained any file, raised any
# ceiling, or raised `max_non_test_loc`.
#
# All guard baselines are shrink-only. A file or ratchet rule absent on the
# base ref is treated as a first introduction and is not compared.
# Usage: baseline-shrink-only.sh [base-ref]
set -euo pipefail

base="${1:-origin/main}"
baseline_file="crates/norn-skeleton-guard/baseline.json"
loc_file="crates/norn-skeleton-guard/loc-baseline.json"

if ! base_json="$(git show "${base}:${baseline_file}" 2>/dev/null)"; then
  echo "${baseline_file} does not exist on ${base}; nothing to compare (first introduction)."
else
  added="$(jq -n --argjson old "${base_json}" --slurpfile new "${baseline_file}" '
    ([ "norn_edges", "third_party", "norn_ir_validators" ][] as $k
      | (($new[0][$k] // []) - ($old[$k] // []))[]
      | "\($k): \(. | tostring)"),
    (if $old.ring0_ui_modules != null then
      (($new[0].ring0_ui_modules // []) - ($old.ring0_ui_modules // []))[]
      | "ring0_ui_modules: \(. | tostring)"
    else empty end),
    (($old.ratchets // {}) as $was
      | ($new[0].ratchets // {}) | to_entries[]
      | select($was[.key] != null) | .key as $rule
      | .value | to_entries[]
      | select(.value > ($was[$rule][.key] // 0))
      | "ratchets.\($rule): \(.key) rose from \($was[$rule][.key] // 0) to \(.value)")')"
  if [[ -n "${added}" ]]; then
    printf '::error::%s gained entries relative to %s (baseline is shrink-only):\n%s\n' "${baseline_file}" "${base}" "${added}"
    exit 1
  fi
  echo "${baseline_file}: no entries added and no ratchet raised relative to ${base}."
fi

if ! loc_base_json="$(git show "${base}:${loc_file}" 2>/dev/null)"; then
  echo "${loc_file} does not exist on ${base}; nothing to compare (first introduction)."
else
  loc_added="$(jq -n --argjson old "${loc_base_json}" --slurpfile new "${loc_file}" '
    (if ($new[0].max_non_test_loc // 0) > ($old.max_non_test_loc // 0) then
      "max_non_test_loc rose from \($old.max_non_test_loc) to \($new[0].max_non_test_loc)"
    else empty end),
    (($old.files // {}) as $was
      | ($new[0].files // {}) | to_entries[]
      | if $was[.key] == null then
          "files.\(.key): new entry (\(.value)) is forbidden"
        elif .value > $was[.key] then
          "files.\(.key): ceiling rose from \($was[.key]) to \(.value)"
        else empty end)')"
  if [[ -n "${loc_added}" ]]; then
    printf '::error::%s grew relative to %s (LOC baseline is shrink-only):\n%s\n' "${loc_file}" "${base}" "${loc_added}"
    exit 1
  fi
  echo "${loc_file}: no entries added and no ceiling raised relative to ${base}."
fi
