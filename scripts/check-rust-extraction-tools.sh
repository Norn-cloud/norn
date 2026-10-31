#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
tool_root="${NORN_VERIFICATION_TOOL_ROOT:-$repo_root/target/verification-tools}"
export CARGO_HOME="$tool_root/cargo"
export RUSTUP_HOME="$tool_root/rustup"
export OPAMROOT="$tool_root/opam-root"
export OPAMSWITCH="norn-verification-5.4.1"
export PATH="$tool_root/bin:$CARGO_HOME/bin:$PATH"

expected_fstar="2026.08.16"
expected_hax_commit="37ecb8240fc237377f63614622650190a45617ff"
expected_opam_repository_commit="45220c31b03bbad5f368a3e01c8e3da44844c5db"
expected_hax_rustc="rustc 1.93.0-nightly (843f8ce2e 2025-11-07)"

require_clean_source_tree() {
  local label="$1"
  local source="$2"
  if [[ -n "$(git -C "$source" status --porcelain=v1 --untracked-files=all)" ]]; then
    printf '%s source tree is dirty: %s\n' "$label" "$source" >&2
    exit 1
  fi
}

printf 'F*\n'
fstar_version="$(fstar.exe --version)"
printf '%s\n' "$fstar_version"
grep -Fq "F* $expected_fstar" <<<"$fstar_version"
printf '\nhax\n'
hax_version="$(cargo-hax --version)"
printf '%s\n' "$hax_version"
grep -Fq "commit=$expected_hax_commit" <<<"$hax_version"
test "$(git -C "$tool_root/sources/hax" rev-parse HEAD)" = "$expected_hax_commit"
require_clean_source_tree "hax" "$tool_root/sources/hax"
printf '\nhax Rust toolchain\n'
hax_rustc="$RUSTUP_HOME/toolchains/nightly-2025-11-08-x86_64-unknown-linux-gnu/bin/rustc"
test "$($hax_rustc --version)" = "$expected_hax_rustc"
printf '%s\n' "$expected_hax_rustc"
printf '\nAeneas\n'
test "$(aeneas -version)" = 'aeneas 74a460a2'
test "$(git -C "$tool_root/sources/aeneas" rev-parse HEAD)" = 74a460a2f80ecea481bbdf1a08f881633c3bb097
require_clean_source_tree "Aeneas" "$tool_root/sources/aeneas"
printf '\nCharon\n'
test "$(charon version)" = '0.1.240'
test "$(git -C "$tool_root/sources/aeneas/charon" rev-parse HEAD)" = a1d989057b50b0429ccd96c5b4c4428b9b218b21
printf '\nopam repository\n'
rg -F "github.com/ocaml/opam-repository.git#$expected_opam_repository_commit" \
  "$OPAMROOT/repo/repos-config"
