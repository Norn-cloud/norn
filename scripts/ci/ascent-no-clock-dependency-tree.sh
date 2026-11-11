#!/usr/bin/env bash
# Qualify the entire checker/Datalog normal + build graph, including
# all targets. Metadata resolution only; run on Boat after fetching dependencies.
# Usage: ascent-no-clock-dependency-tree.sh [--self-test]
set -euo pipefail

check_tree() {
  awk '
    BEGIN {
      number = "(0|[1-9][0-9]*)"
      identifier = "[0-9A-Za-z-]+"
      prerelease = "(" number "|[0-9A-Za-z-]*[A-Za-z-][0-9A-Za-z-]*)"
      version = number "\\." number "\\." number \
        "(-" prerelease "(\\." prerelease ")*)?(\\+" identifier "(\\." identifier ")*)?"
      # Cargo annotates proc macros before the optional absolute source path,
      # and marks deduplicated packages last. Paths may contain spaces.
      package = "^[A-Za-z0-9_][A-Za-z0-9_-]* v" version \
        "( \\(proc-macro\\))?( \\(/[^[:cntrl:]]*\\))?( \\(\\*\\))?$"
    }
    # Cargo separates selected root trees with empty lines.
    /^$/ { next }
    /^\[build-dependencies\]$/ { next }
    $0 !~ package {
      print "unexpected cargo tree output at line " NR ": " $0 > "/dev/stderr"
      failed = 1
      next
    }
    $1 == "norn-checker" { checker = 1 }
    $1 == "norn-datalog" { datalog = 1 }
    $1 == "ascent" && $2 == "v0.8.1" { ascent = 1 }
    $1 == "web-time" || $1 == "web_time" {
      print "forbidden ambient clock package: " $0 > "/dev/stderr"
      failed = 1
    }
    END {
      if (!checker) { print "missing norn-checker root" > "/dev/stderr"; failed = 1 }
      if (!datalog) { print "missing norn-datalog root" > "/dev/stderr"; failed = 1 }
      if (!ascent) { print "missing ascent v0.8.1" > "/dev/stderr"; failed = 1 }
      exit failed
    }
  '
}

run_gate() {
  local tree tool
  for tool in cargo awk dirname; do
    if ! command -v "${tool}" >/dev/null 2>&1; then
      printf 'required tool missing: %s\n' "${tool}" >&2
      return 127
    fi
  done
  cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../.." || return "$?"
  tree="$(cargo tree --locked --offline --target all \
    -p norn-checker -p norn-datalog --edges normal,build \
    --prefix none --format '{p}')" || return "$?"
  check_tree <<<"${tree}" || return "$?"
  echo 'Ascent no-clock dependency-tree gate passed.'
}

self_test() {
  local valid=$'norn-checker v0.2.0 (/repo/crates/norn-checker)\nnorn-datalog v0.1.0 (/repo/crates/norn-datalog)\nascent v0.8.1 (/repo/vendor/ascent)'
  local count=0 status
  assert_tree() {
    local expected="$1" label="$2" tree="$3" actual=0
    check_tree <<<"${tree}" >/dev/null 2>&1 || actual="$?"
    if [[ "${actual}" -ne "${expected}" ]]; then
      printf 'FAIL: %s (status %s, expected %s)\n' "${label}" "${actual}" "${expected}" >&2
      return 1
    fi
    count=$((count + 1))
  }
  assert_tree 0 'valid whole graph' "${valid}"
  assert_tree 1 'forbidden normal dependency' "${valid}"$'\nweb-time v1.1.0'
  assert_tree 1 'forbidden build dependency' "${valid}"$'\n[build-dependencies]\nweb-time v1.1.0'
  assert_tree 1 'forbidden underscore package' "${valid}"$'\nweb_time v1.1.0'
  assert_tree 0 'similarly named package allowed' "${valid}"$'\nweb-timepiece v1.0.0'
  assert_tree 1 'missing checker root' $'norn-datalog v0.1.0\nascent v0.8.1'
  assert_tree 1 'missing Datalog root' $'norn-checker v0.2.0\nascent v0.8.1'
  assert_tree 1 'missing Ascent' $'norn-checker v0.2.0\nnorn-datalog v0.1.0'
  assert_tree 1 'wrong Ascent version' $'norn-checker v0.2.0\nnorn-datalog v0.1.0\nascent v0.8.10'
  assert_tree 0 'R15 proc-macro, source, duplicate, and root separator shape' \
    "${valid}"$'\nascent_base v0.8.1\npastey v0.2.3 (proc-macro)\nascent_macro v0.8.1 (proc-macro) (/repo/third_party/ascent/ascent_macro)\nascent_base v0.8.1 (*)\nserde_derive v1.0.229 (proc-macro) (*)\nnorn-ir v0.1.0 (/repo/crates/norn-ir) (*)\n\nnorn-datalog v0.1.0 (/repo/crates/norn-datalog) (*)'
  assert_tree 0 'path spaces, prerelease and build metadata' \
    "${valid}"$'\nexample v1.2.3-alpha.1+build.42 (proc-macro) (/repo with spaces/example) (*)'
  assert_tree 1 'prefixed clock package' "${valid}"$'\n|-- web-time v1.1.0'
  assert_tree 1 'malformed root despite valid roots' "${valid}"$'\nnorn-checker garbage'
  assert_tree 1 'missing root version' $'norn-checker\nnorn-datalog v0.1.0\nascent v0.8.1'
  assert_tree 1 'truncated package line' "${valid}"$'\nnorn-data'
  assert_tree 1 'arbitrary garbage' "${valid}"$'\nnot cargo output'
  assert_tree 1 'unexpected header' "${valid}"$'\n[dev-dependencies]'
  assert_tree 1 'unexpected suffix' "${valid}"$'\nserde v1.0.229 garbage'
  assert_tree 1 'invalid semver' "${valid}"$'\nexample v1.2.3-01'
  assert_tree 1 'truncated version' "${valid}"$'\nexample v1.2'
  assert_tree 1 'indented dependency' "${valid}"$'\n  web_time v1.1.0'
  assert_tree 1 'truncated path' "${valid}"$'\nexample v1.2.3 (/repo/example'
  assert_tree 1 'empty output' ''
  # Only explicit self-test mode installs this function, inside a subshell.
  # Valid stdout must never hide a nonzero Cargo status. No real Cargo runs.
  status=0
  (
    cargo() { printf '%s\n' "${valid}"; return 23; }
    run_gate
  ) >/dev/null 2>&1 || status="$?"
  if [[ "${status}" -ne 23 ]]; then
    printf 'FAIL: Cargo failure propagation (status %s, expected 23)\n' "${status}" >&2
    return 1
  fi
  count=$((count + 1))
  # Simulate unavailable tools without changing PATH or launching real Cargo.
  local missing
  for missing in cargo awk dirname; do
    status=0
    (
      command() {
        if [[ "$1" == -v && "$2" == "${missing}" ]]; then return 1; fi
        builtin command "$@"
      }
      cargo() { return 99; }
      run_gate
    ) >/dev/null 2>&1 || status="$?"
    if [[ "${status}" -ne 127 ]]; then
      printf 'FAIL: missing %s (status %s, expected 127)\n' "${missing}" "${status}" >&2
      return 1
    fi
    count=$((count + 1))
  done
  printf 'Ascent no-clock self-tests passed: %s\n' "${count}"
}

case "$#:${1:-}" in
  0:) run_gate ;;
  1:--self-test) self_test ;;
  *) echo 'Usage: ascent-no-clock-dependency-tree.sh [--self-test]' >&2; exit 2 ;;
esac
