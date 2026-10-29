#!/usr/bin/env bash
# T11-ZEN-1 (#2771, #3404): integrated-consumer dependency-tree gate for the
# vendored zen crate set (vendor/zen). Resolves the tree_gate consumers from zen-pin.json
# together (as the kernel build unifies them) with `cargo tree --no-dedupe`,
# natively and for wasm32-unknown-unknown, normal + build edges (dev-only test
# tooling is not shipped). Fails if:
#   - any crate in tree_gate.forbidden appears (tokio, rquickjs, HTTP clients),
#     or tree_gate.forbidden_wasm32 appears on wasm32 (getrandom);
#   - any zen-* package's *resolved* feature set (after Cargo feature
#     unification) differs from its profile entry, or a zen-* package has no
#     profile entry (so `regex-deprecated`, `js`, `http`, `runtime-rng`,
#     `schema-resolvers`, or `default` coming back through any crate fails);
#   - any zen-* package resolves to anything but its vendored path crate
#     (zen-pin.json `vendor.path` + `vendor.crates`): the former fork tag,
#     another git ref, crates.io, or a path elsewhere;
#   - the pin file lacks a consumer list or ban lists (no vacuous pass).
# Resolution only, no compilation.
#
# Usage: zen-dependency-tree.sh [pin-file]
# Test hook: ZEN_TREE_FIXTURE=<file> uses that file (raw
# `cargo tree --prefix none -f '{p}|{f}'` output) for every target instead of
# running cargo; norn-skeleton-guard's `zen_pin_tree_gate_*` tests use it.
set -euo pipefail

pin="${1:-crates/norn-skeleton-guard/zen-pin.json}"

# Reads a required, non-empty string array from the pin file into the named
# array variable; any jq failure or an empty list aborts the gate.
read_list() {
  local -n out="$1"
  local raw
  raw="$(jq -er "$2 | if type == \"array\" and length > 0 and all(type == \"string\") then .[] else error(\"$2 must be a non-empty string array\") end" "${pin}")"
  mapfile -t out <<<"${raw}"
  if [[ "${#out[@]}" -eq 0 ]]; then
    echo "::error::${pin}: $2 is empty" >&2
    exit 1
  fi
}

tag="$(jq -er .fork.tag "${pin}")"
commit="$(jq -er .fork.commit "${pin}")"
vendor="$(jq -er '.vendor.path | select(type == "string" and length > 0)' "${pin}")"
read_list consumers .tree_gate.consumers
read_list bans_all .tree_gate.forbidden
read_list bans_wasm32 .tree_gate.forbidden_wasm32

if [[ -n "${ZEN_TREE_FIXTURE:-}" ]]; then
  host="native-fixture"
  root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd -P)"
else
  host="$(rustc -vV | sed -n 's/^host: //p')"
  [[ -n "${host}" ]] || { echo "::error::cannot determine the rustc host triple" >&2; exit 1; }
  # cargo tree prints path packages under cargo's own workspace root.
  root="$(cargo metadata --locked --no-deps --format-version 1 | jq -er .workspace_root)"
fi

pkg_args=()
for consumer in "${consumers[@]}"; do
  pkg_args+=(-p "${consumer}")
done

resolve() {
  if [[ -n "${ZEN_TREE_FIXTURE:-}" ]]; then
    cat "${ZEN_TREE_FIXTURE}"
  else
    cargo tree --locked --no-dedupe -e normal,build --target "$1" \
      --prefix none -f '{p}|{f}' "${pkg_args[@]}"
  fi
}

fail=0
err() {
  printf '::error::%s\n' "$*"
  fail=1
}

for target in "${host}" wasm32-unknown-unknown; do
  echo "== ${target}: ${consumers[*]}"
  raw="$(resolve "${target}")"
  # Defensive: drop a dedupe marker if one ever appears, then collapse
  # repeated rows (identical under --no-dedupe).
  tree="$(sed -E 's/ \(\*\)$//' <<<"${raw}" | sort -u)"

  bans=("${bans_all[@]}")
  if [[ "${target}" == wasm32-* ]]; then
    bans+=("${bans_wasm32[@]}")
  fi
  if [[ "${#bans[@]}" -eq 0 ]]; then
    echo "::error::no forbidden crates configured; refusing a vacuous pass" >&2
    exit 1
  fi
  echo "checking ${#bans[@]} banned crates: ${bans[*]}"
  for crate in "${bans[@]}"; do
    if hits="$(grep -E "^${crate} v" <<<"${tree}")"; then
      err "${target}: forbidden crate \`${crate}\` in the integrated tree: ${hits//$'\n'/; }"
      if [[ -z "${ZEN_TREE_FIXTURE:-}" ]]; then
        cargo tree --locked -e normal,build --target "${target}" "${pkg_args[@]}" \
          -i "${crate}" 2>/dev/null | head -40 || true
      fi
    fi
  done

  zen_lines="$(grep -E '^zen[-_]' <<<"${tree}" || true)"
  if [[ -z "${zen_lines}" ]]; then
    err "${target}: no zen-* package in the integrated tree; the gate would be vacuous"
  fi
  while IFS= read -r line; do
    [[ -z "${line}" ]] && continue
    name="${line%% *}"
    actual="$(tr ',' '\n' <<<"${line##*|}" | sed '/^$/d' | sort | paste -sd, -)"
    if ! expected="$(jq -er --arg n "${name}" '.profile[$n] // error("unprofiled") | sort | join(",")' "${pin}" 2>/dev/null)" \
      || ! dir="$(jq -er --arg n "${name}" '.vendor.crates[$n] // error("unvendored")' "${pin}" 2>/dev/null)"; then
      err "${target}: \`${name}\` is not a package of the Norn zen profile (zen-pin.json)"
      continue
    fi
    ok=1
    if [[ "${actual}" != "${expected}" ]]; then
      err "${target}: \`${name}\` resolved features [${actual}] != Norn profile [${expected}]"
      ok=0
    fi
    if [[ "${line}" != *" (${root}/${vendor}/${dir})|"* ]]; then
      err "${target}: \`${name}\` is not the vendored crate ${root}/${vendor}/${dir}: ${line}"
      ok=0
    fi
    if [[ "${ok}" -eq 1 ]]; then
      echo "ok ${name} [${actual}]"
    fi
  done <<<"${zen_lines}"
done

if [[ "${fail}" -ne 0 ]]; then
  echo "zen dependency-tree gate FAILED (see zen-fork-ledger.md, D32)."
  exit 1
fi
echo "zen dependency-tree gate passed for ${vendor} (imported from ${tag} = ${commit})."
