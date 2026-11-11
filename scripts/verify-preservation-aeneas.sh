#!/usr/bin/env bash
# #718 U7b: Charon/Aeneas extract the production path-mapping preservation
# check (crates/norn-checker/src/consistency/preservation/check.rs) and Lean
# proves that it accepts only certificates NornSem.Exchange.check accepts, hence
# (by NornSem.Exchange.check_sound) only mappings that preserve the target EGD.
# Every seeded mutant must then extract, elaborate, fail the proof and be
# refuted by its concrete witness.
set -euo pipefail
repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
tool_root="${NORN_VERIFICATION_TOOL_ROOT:-$repo_root/target/verification-tools}"
aeneas_root="$tool_root/sources/aeneas"
charon="$aeneas_root/charon/bin/charon"
aeneas="$aeneas_root/bin/aeneas"
lean_root="$aeneas_root/backends/lean"
semantics="$repo_root/verification/semantics-v1"
proof_root="$semantics/Checker/Preservation"
check_source="crates/norn-checker/src/consistency/preservation/check.rs"
result="${NORN_VERIFICATION_RESULT_ROOT:-$repo_root/target/verification-results}/preservation-aeneas"
# The exact Lean sources this gate elaborates (with the NornSem module it imports).
proof_files=(
  verification/semantics-v1/Checker/Preservation/Audit.lean
  verification/semantics-v1/Checker/Preservation/Proof.lean
)
test "$(cd "$repo_root" && find verification/semantics-v1/Checker/Preservation -name '*.lean' | LC_ALL=C sort)" = \
  "$(printf '%s\n' "${proof_files[@]}")"
semantics_modules=(NornSem/Exchange)

test "$(git -C "$aeneas_root" rev-parse HEAD)" = 74a460a2f80ecea481bbdf1a08f881633c3bb097
test "$(git -C "$aeneas_root/charon" rev-parse HEAD)" = a1d989057b50b0429ccd96c5b4c4428b9b218b21
test "$("$aeneas" -version)" = 'aeneas 74a460a2'
test "$("$charon" version)" = '0.1.240'
test "$(tr -d '\r\n' < "$lean_root/lean-toolchain")" = leanprover/lean4:v4.31.0
export RUSTUP_HOME="$tool_root/rustup" CARGO_HOME="$tool_root/cargo" CARGO_INCREMENTAL=0
rm -rf "$result"
mkdir -p "$result/common"

# extract <tree> <out>: translate the production file from <tree>.
extract() {
  local tree="$1" out="$2"
  mkdir -p "$out" || return
  (cd "$tree" && "$charon" rustc --preset=aeneas --error-on-warnings --start-from crate::check \
    --dest-file "$out/check.llbc" -- --crate-name norn_checker_preservation \
    --crate-type lib --edition 2024 "$check_source") || return
  "$aeneas" -backend lean -dest "$out/Check" -emit-json -split-files -warnings-as-errors \
    "$out/check.llbc"
}

# lean_check <out> <root> <module>: elaborate one module into <out>.
lean_check() {
  local out="$1" root="$2" module="$3"
  mkdir -p "$(dirname "$out/$module.olean")"
  (cd "$lean_root" && LEAN_PATH="$out:$result/common" lake env lean -DwarningAsError=true \
    -R "$root" -o "$out/$module.olean" "$root/$module.lean")
}

# generated <out>: the extracted code must elaborate.
generated() {
  local out="$1" module
  for module in Check/Types Check/Funs; do
    lean_check "$out" "$out" "$module" || return
  done
}

(cd "$lean_root" && lake build Aeneas)
# The written semantics never import the extraction.
for module in "${semantics_modules[@]}"; do
  lean_check "$result/common" "$semantics" "$module"
done

baseline="$result/baseline"
extract "$repo_root" "$baseline"
python3 - "$baseline" "$check_source" <<'PYMANIFEST'
import json
import sys
from pathlib import Path

out, source = Path(sys.argv[1]), sys.argv[2]
manifest = json.loads((out / "Check" / "translation.json").read_text(encoding="utf-8"))
if (manifest.get("aeneas_version"), manifest.get("charon_version"), manifest.get("crate")) != (
    "74a460a2", "0.1.240", "norn_checker_preservation"
):
    raise SystemExit("unexpected extraction identity")
prefix = "norn_checker_preservation."
plain = ("rep_of", "value_at", "width", "body_matches", "apply", "width_of", "case_elem",
         "initial", "prepend", "produces", "case_choices", "check", "same", "merged",
         "max_arg", "max_var", "contains", "contains_var", "args_below", "vars_below",
         "elements", "atoms_held", "head_args", "with_heads", "extended", "run",
         "rule_width", "case_args", "with_case_body", "case_bodies", "merged_pairs",
         "merged_heads", "with_cases", "cases_for", "traces_merge")
looped = ("same", "has", "merged", "max_arg", "max_var", "contains", "contains_var",
          "args_below", "vars_below", "elements", "atoms_held", "head_args", "with_heads",
          "extended", "run", "rule_width", "case_args", "with_case_body", "case_bodies",
          "merged_pairs", "merged_heads", "prepend", "with_cases", "cases_for",
          "case_choices", "traces_merge")
expected = {prefix + name for name in plain + ("has",)}
expected |= {prefix + name + suffix for name in looped for suffix in ("_loop", "_loop.body")}
functions = manifest.get("functions", [])
own = {f["lean_name"] for f in functions if f.get("is_local")}
models = {f["lean_name"] for f in functions if not f.get("is_local")}
if own != expected:
    raise SystemExit(f"unexpected functions {sorted(own ^ expected)}")
for function in functions:
    if function.get("is_local") and (
        function.get("is_opaque") or function.get("source", {}).get("file") != source
    ):
        raise SystemExit(f"{function['lean_name']} is not the production body")
if manifest.get("globals", []) != []:
    raise SystemExit("unexpected extracted globals")
for collection in ("trait_decls", "trait_impls"):
    if manifest.get(collection) != []:
        raise SystemExit(f"unexpected extracted {collection}")
if (out / "Check" / "FunsExternal_Template.lean").exists():
    raise SystemExit("the extraction needs external models")
if "sorry" in "".join(p.read_text(encoding="utf-8") for p in (out / "Check").glob("*.lean")):
    raise SystemExit("extraction contains sorry")
print("extraction models:", ", ".join(sorted(models)) or "none")
PYMANIFEST
generated "$baseline"
lean_check "$baseline" "$semantics" Checker/Preservation/Proof
proof_output="$(lean_check "$baseline" "$semantics" Checker/Preservation/Audit)"
printf '%s\n' "$proof_output"
expected_axioms="$(printf '%s\n' \
  "'Norn.Checker.Preservation.check_correct' depends on axioms: [propext, Classical.choice, Quot.sound]" \
  "'Norn.Checker.Preservation.check_sound' depends on axioms: [propext, Classical.choice, Quot.sound]")"
test "$proof_output" = "$expected_axioms"
# Check the exact propositions, not only the theorem names or an exit code.
printf '%s\n' \
  'import Checker.Preservation.Proof' \
  'open Aeneas Aeneas.Std Norn.Sem Norn.Checker.Preservation norn_checker_preservation in' \
  'example (theory : Slice Dep) (rules : Slice Rule) (body : Slice Atom) (l r : Usize)' \
  '    (traces : Slice (alloc.vec.Vec Step)) (h : check theory rules body l r traces = Result.ok true) :' \
  '    Exchange.check (theory.val.map depSem) (rules.val.map ruleSem) (atomsSem body.val) l.val r.val' \
  '      (traces.val.map traceSem) = true :=' \
  '  check_correct theory rules body l r traces h' \
  'open Aeneas Aeneas.Std Norn.Sem Norn.Checker.Preservation norn_checker_preservation in' \
  'example (theory : Slice Dep) (rules : Slice Rule) (body : Slice Atom) (l r : Usize)' \
  '    (traces : Slice (alloc.vec.Vec Step)) (h : check theory rules body l r traces = Result.ok true) :' \
  '    Exchange.Preserves (theory.val.map depSem) (rules.val.map ruleSem) (atomsSem body.val)' \
  '      l.val r.val :=' \
  '  check_sound theory rules body l r traces h' \
  | (cd "$lean_root" && LEAN_PATH="$baseline:$result/common" lake env lean -DwarningAsError=true --stdin)
echo "preservation: the production check is proved against NornSem.Exchange"

# witness <check.rs> <arg>: compile the witnesses beside <check.rs> and run them.
witness() {
  local source="$1" arg="$2" dir
  dir="$(mktemp -d "$result/witness.XXXXXX")" || return
  cp "$source" "$dir/check.rs" && cp "$proof_root/witnesses.rs" "$dir/witnesses.rs" || return
  rustup run nightly-2026-08-18 rustc --edition 2024 -A warnings -o "$dir/witnesses" \
    "$dir/witnesses.rs" || return
  "$dir/witnesses" "$arg"
}
witness "$repo_root/$check_source" all
echo "preservation: the production file agrees with every witness"

# Mutants are applied to a copy of the production file; the checkout is untouched.
python3 - "$repo_root/$check_source" "$result/mutants" <<'PYMUTANTS' > "$result/mutants.list"
import sys
from pathlib import Path

source, root = Path(sys.argv[1]), Path(sys.argv[2])
mutants = [
    ("egd-skips-body", "if body_matches(&state, body, &step.asg) && a < next && b < next {",
     "if a < next && b < next {"),
    ("has-ignores-relation", "if atom.rel == rel && same(&state.rep, &atom.args, es) {",
     "if same(&state.rep, &atom.args, es) {"),
    ("head-not-fresh", "let Some(fresh) = base.checked_add(*v) else {",
     "let Some(fresh) = (*v).checked_add(0) else {"),
    ("case-count-unchecked", "all.len() == traces.len() && traces_merge(", "traces_merge("),
    ("left-unchecked", "if !contains_var(body, left) || !contains_var(body, right) {",
     "if !contains_var(body, right) {"),
]
text = source.read_text(encoding="utf-8")
for name, old, new in mutants:
    if text.count(old) != 1:
        raise SystemExit(f"{name}: mutation site not found exactly once")
    tree = root / name / "tree" / "crates/norn-checker/src/consistency/preservation"
    tree.mkdir(parents=True)
    (tree / "check.rs").write_text(text.replace(old, new), encoding="utf-8")
    print(name)
PYMUTANTS
survivors=0
while read -r name; do
  tree="$result/mutants/$name/tree"
  out="$result/mutants/$name/out"
  # A mutant must extract and its code must elaborate: only the proof may fail.
  if ! extract "$tree" "$out" > "$result/mutants/$name/extract.log" 2>&1 ||
    ! generated "$out" >> "$result/mutants/$name/extract.log" 2>&1; then
    tail -n 40 "$result/mutants/$name/extract.log" >&2
    echo "mutant $name: did not extract and elaborate; it cannot test the proof" >&2
    exit 1
  fi
  if lean_check "$out" "$semantics" Checker/Preservation/Proof > "$result/mutants/$name/proof.log" 2>&1; then
    echo "mutant $name: proof still holds (survived)" >&2
    survivors=$((survivors + 1))
  elif witness "$tree/$check_source" "$name" > "$result/mutants/$name/witness.log" 2>&1; then
    echo "mutant $name: proof fails and its witness refutes it (detected)"
  else
    echo "mutant $name: proof fails but its witness does not refute it" >&2
    survivors=$((survivors + 1))
  fi
done < "$result/mutants.list"
test "$survivors" -eq 0
echo "preservation: every seeded mutant fails the proof and is refuted by its witness"
