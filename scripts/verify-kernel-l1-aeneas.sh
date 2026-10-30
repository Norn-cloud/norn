#!/usr/bin/env bash
set -euo pipefail
repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
tool_root="${NORN_VERIFICATION_TOOL_ROOT:-$repo_root/target/verification-tools}"
aeneas_root="$tool_root/sources/aeneas"
result="${NORN_VERIFICATION_RESULT_ROOT:-$repo_root/target/verification-results}/kernel-l1-aeneas"
mkdir -p "$result"
rm -f "$result/version.llbc" "$result/Version.lean" "$result/Version.olean" \
  "$result/Reference.olean" "$result/translation.json"
test "$(git -C "$aeneas_root" rev-parse HEAD)" = 74a460a2f80ecea481bbdf1a08f881633c3bb097
test "$(git -C "$aeneas_root/charon" rev-parse HEAD)" = a1d989057b50b0429ccd96c5b4c4428b9b218b21
test "$("$aeneas_root/bin/aeneas" -version)" = 'aeneas 74a460a2'
test "$("$aeneas_root/charon/bin/charon" version)" = '0.1.240'
# This is the production file itself, compiled standalone without substituting
# its implementation or extracting the surrounding kernel dependency graph.
"$aeneas_root/charon/bin/charon" rustc --preset=aeneas \
  --start-from crate::checked_next_version --error-on-warnings \
  --dest-file "$result/version.llbc" -- \
  --crate-name norn_kernel_version --crate-type lib \
  "$repo_root/crates/norn-kernel/src/version_check.rs"
"$aeneas_root/bin/aeneas" -backend lean -dest "$result" -emit-json \
  -warnings-as-errors "$result/version.llbc"
python3 - "$result/translation.json" <<'PYMANIFEST'
import json
import sys
manifest = json.load(open(sys.argv[1], encoding="utf-8"))
if (manifest.get("aeneas_version"), manifest.get("charon_version"), manifest.get("crate")) != (
    "74a460a2", "0.1.240", "norn_kernel_version"
):
    raise SystemExit("unexpected version extraction identity")
functions = manifest.get("functions", [])
if len(functions) != 1:
    raise SystemExit("expected exactly one extracted production function")
function = functions[0]
expected = {
    "lean_name": "norn_kernel_version.checked_next_version", "lean_file": "Version.lean",
    "rust_name": "norn_kernel_version::checked_next_version", "is_local": True,
    "is_opaque": False, "can_fail": True, "can_diverge": False, "is_rec": False,
}
if any(function.get(key) != value for key, value in expected.items()):
    raise SystemExit("unexpected extracted function metadata")
if function.get("source", {}).get("file") != "crates/norn-kernel/src/version_check.rs":
    raise SystemExit("extraction is not bound to the production version source")
for collection in ("types", "globals", "trait_decls", "trait_impls"):
    if manifest.get(collection) != []:
        raise SystemExit(f"unexpected extracted {collection}")
PYMANIFEST
test -s "$result/Version.lean"
lean_root="$aeneas_root/backends/lean"
test "$(tr -d '\r\n' < "$lean_root/lean-toolchain")" = leanprover/lean4:v4.31.0
(
  cd "$lean_root"
  lake build Aeneas
  # Re-elaborate the exact reference source for this older compatibility lane.
  # It does not replace the separate patched-Lean 4.34.1 reference gate.
  LEAN_PATH="$result" lake env lean -DwarningAsError=true -R "$repo_root/verification/kernel-l1" \
    -o "$result/Reference.olean" "$repo_root/verification/kernel-l1/Reference.lean"
  LEAN_PATH="$result" lake env lean -DwarningAsError=true -R "$result" \
    -o "$result/Version.olean" "$result/Version.lean"
  if proof_output="$(LEAN_PATH="$result" lake env lean -DwarningAsError=true \
    -R "$repo_root/verification/kernel-l1" -o "$result/VersionProof.olean" \
    "$repo_root/verification/kernel-l1/VersionProof.lean" 2>&1)"; then
    :
  else
    proof_status=$?
    printf '%s\n' "$proof_output" >&2
    exit "$proof_status"
  fi
  printf '%s\n' "$proof_output"
  test "$proof_output" = "'Norn.KernelL1.version_correspondence' depends on axioms: [propext, Classical.choice, Quot.sound]"
  # Check the exact proposition, not only the theorem name or an exit code.
  printf '%s\n' \
    'import VersionProof' \
    'example (version : Aeneas.Std.U64) :' \
    '  Norn.KernelL1.projectVersion (norn_kernel_version.checked_next_version version) =' \
    '    some (Norn.KernelL1.nextVersion version.val) :=' \
    '  Norn.KernelL1.version_correspondence version' \
    | LEAN_PATH="$result" lake env lean -DwarningAsError=true --stdin
)
