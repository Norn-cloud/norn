#!/usr/bin/env bash
# Fails if crates/norn-skeleton-guard/baseline.json gained any entry or raised
# any `ratchets` count relative to a base git ref (default: origin/main), or if
# crates/norn-skeleton-guard/loc-baseline.json gained any file, raised any
# ceiling, or raised `max_non_test_loc`.
#
# All guard baselines are shrink-only. A file or ratchet rule absent on the
# base ref is treated as a first introduction and is not compared (its schema is
# still validated when present on disk).
# Usage: baseline-shrink-only.sh [base-ref]
set -euo pipefail

base="${1:-origin/main}"
baseline_file="crates/norn-skeleton-guard/baseline.json"
loc_file="crates/norn-skeleton-guard/loc-baseline.json"

validate_baseline_schema() {
  local old_json="$1"
  if ! jq -ne --argjson old "${old_json}" --slurpfile new "${baseline_file}" '
    ($new | length == 1)
    and ($new[0] | type == "object")
    and ([ "norn_edges", "third_party", "norn_ir_validators" ]
         | all(.[]; ($new[0][.] | type == "array")))
    and (if ($old != null and $old.ring0_ui_modules != null) or ($new[0].ring0_ui_modules != null) then
           ($new[0].ring0_ui_modules | type == "array")
           and ($new[0].ring0_ui_modules | all(.[]; type == "string"))
         else true end)
    and (if ($old != null and $old.ratchets != null) or ($new[0].ratchets != null) then
           ($new[0].ratchets | type == "object")
           and ($new[0].ratchets | to_entries | all(.[];
                 (.value | type == "object")
                 and (.value | to_entries | all(.[]; (.value | type == "number")))))
           and (if $old != null and $old.ratchets != null then
                  ($old.ratchets | keys | all(.[]; ($new[0].ratchets[.] | type == "object")))
                else true end)
         else true end)
  ' >/dev/null 2>&1; then
    printf '::error::%s is empty, malformed, or violates expected baseline schema.\n' "${baseline_file}"
    exit 1
  fi
}

validate_loc_schema() {
  if ! jq -ne --slurpfile new "${loc_file}" '
    ($new | length == 1)
    and ($new[0] | type == "object")
    and ($new[0].max_non_test_loc | type == "number")
    and ($new[0].files | type == "object")
    and ($new[0].files | to_entries | all(.[]; (.value | type == "number")))
  ' >/dev/null 2>&1; then
    printf '::error::%s is empty, malformed, or violates expected LOC baseline schema.\n' "${loc_file}"
    exit 1
  fi
}

if ! git rev-parse --verify "${base}^{commit}" >/dev/null 2>&1; then
  printf '::error::base ref %s does not resolve to a commit.\n' "${base}"
  exit 1
fi

if [[ -z "$(git ls-tree --name-only "${base}" -- "${baseline_file}")" ]]; then
  if [[ -f "${baseline_file}" ]]; then
    validate_baseline_schema "null"
  fi
  echo "${baseline_file} does not exist on ${base}; nothing to compare (first introduction)."
else
  base_json="$(git show "${base}:${baseline_file}")"
  validate_baseline_schema "${base_json}"
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

if [[ -z "$(git ls-tree --name-only "${base}" -- "${loc_file}")" ]]; then
  if [[ -f "${loc_file}" ]]; then
    validate_loc_schema
  fi
  echo "${loc_file} does not exist on ${base}; nothing to compare (first introduction)."
else
  loc_base_json="$(git show "${base}:${loc_file}")"
  validate_loc_schema
  loc_added="$(jq -n --argjson old "${loc_base_json}" --slurpfile new "${loc_file}" '
    (if $new[0].max_non_test_loc > $old.max_non_test_loc then
      "max_non_test_loc rose from \($old.max_non_test_loc) to \($new[0].max_non_test_loc)"
    else empty end),
    ($old.files as $was
      | $new[0].files | to_entries[]
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
