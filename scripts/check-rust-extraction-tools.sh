#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
tool_root="${NORN_VERIFICATION_TOOL_ROOT:-$repo_root/target/verification-tools}"
export CARGO_HOME="$tool_root/cargo"
export RUSTUP_HOME="$tool_root/rustup"
export OPAMROOT="$tool_root/opam-root"
export OPAMSWITCH="norn-verification-5.4.1"
export PATH="$tool_root/bin:$CARGO_HOME/bin:$PATH"

expected_opam_repository_commit="45220c31b03bbad5f368a3e01c8e3da44844c5db"

require_clean_source_tree() {
  local label="$1"
  local source="$2"
  if [[ -n "$(git -C "$source" status --porcelain=v1 --untracked-files=all)" ]]; then
    printf '%s source tree is dirty: %s\n' "$label" "$source" >&2
    exit 1
  fi
}

printf 'Aeneas\n'
test "$(aeneas -version)" = 'aeneas 74a460a2'
test "$(git -C "$tool_root/sources/aeneas" rev-parse HEAD)" = 74a460a2f80ecea481bbdf1a08f881633c3bb097
require_clean_source_tree "Aeneas" "$tool_root/sources/aeneas"
printf '\nCharon\n'
test "$(charon version)" = '0.1.240'
test "$(git -C "$tool_root/sources/aeneas/charon" rev-parse HEAD)" = a1d989057b50b0429ccd96c5b4c4428b9b218b21
require_clean_source_tree "Charon" "$tool_root/sources/aeneas/charon"
printf '\nopam repository\n'
rg -F "github.com/ocaml/opam-repository.git#$expected_opam_repository_commit" \
  "$OPAMROOT/repo/repos-config"
