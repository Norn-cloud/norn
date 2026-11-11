#!/usr/bin/env bash
# #718 U1 (owner D78/D83): Charon/Aeneas extract the production exception-tree
# core (crates/norn-checker/src/law_exceptions/shape.rs) and Lean proves it
# against the shared semantics (verification/semantics-v1, NornSem.Law): the
# shape check decides well-formedness, and the fold computes the defeasible
# meaning. Every seeded mutant must then extract, elaborate, fail the proof and
# be refuted by its concrete witness.
set -euo pipefail
repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
tool_root="${NORN_VERIFICATION_TOOL_ROOT:-$repo_root/target/verification-tools}"
aeneas_root="$tool_root/sources/aeneas"
charon="$aeneas_root/charon/bin/charon"
aeneas="$aeneas_root/bin/aeneas"
lean_root="$aeneas_root/backends/lean"
semantics="$repo_root/verification/semantics-v1"
proof_root="$semantics/Checker/LawExceptions"
shape_source="crates/norn-checker/src/law_exceptions/shape.rs"
result="${NORN_VERIFICATION_RESULT_ROOT:-$repo_root/target/verification-results}/law-exceptions-aeneas"
# The exact Lean sources this gate elaborates (with the NornSem modules it imports).
proof_files=(
  verification/semantics-v1/Checker/LawExceptions/Audit.lean
  verification/semantics-v1/Checker/LawExceptions/Proof.lean
)
test "$(cd "$repo_root" && find verification/semantics-v1/Checker/LawExceptions -name '*.lean' | LC_ALL=C sort)" = \
  "$(printf '%s\n' "${proof_files[@]}")"
semantics_modules=(NornSem/Exact NornSem/Value NornSem/Law NornSem/Obligation)

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
  (cd "$tree" && "$charon" rustc --preset=aeneas --error-on-warnings \
    --start-from crate::check_shape --start-from crate::lifted \
    --dest-file "$out/shape.llbc" -- --crate-name norn_checker_law_exceptions \
    --crate-type lib --edition 2024 "$shape_source") || return
  "$aeneas" -backend lean -dest "$out/Shape" -emit-json -split-files -warnings-as-errors \
    "$out/shape.llbc"
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
  for module in Shape/Types Shape/Funs; do
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
python3 - "$baseline" "$shape_source" <<'PYMANIFEST'
import json
import sys
from pathlib import Path

out, source = Path(sys.argv[1]), sys.argv[2]
manifest = json.loads((out / "Shape" / "translation.json").read_text(encoding="utf-8"))
if (manifest.get("aeneas_version"), manifest.get("charon_version"), manifest.get("crate")) != (
    "74a460a2", "0.1.240", "norn_checker_law_exceptions"
):
    raise SystemExit("unexpected extraction identity")
prefix = "norn_checker_law_exceptions."
expected = {prefix + name for name in (
    "check_shape", "first_invalid", "first_invalid_loop", "first_invalid_loop.body",
    "first_dangling", "first_dangling_loop", "first_dangling_loop.body",
    "first_cycle", "first_cycle_loop", "first_cycle_loop.body",
    "reaches_law", "reaches_law_loop", "reaches_law_loop.body",
    "position", "position_loop", "position_loop.body",
    "lifted", "lifted_loop", "lifted_loop.body",
    "overridden", "overridden_loop", "overridden_loop.body",
    "next_level", "next_level_loop", "next_level_loop.body",
    "copy_level", "copy_level_loop", "copy_level_loop.body",
    "clear_level", "clear_level_loop", "clear_level_loop.body",
)}
functions = manifest.get("functions", [])
own = {f["lean_name"] for f in functions if f.get("is_local")}
models = {f["lean_name"] for f in functions if not f.get("is_local")}
if own != expected or models:
    raise SystemExit(f"unexpected functions {sorted(own ^ expected)} / {sorted(models)}")
for function in functions:
    if function.get("is_opaque") or function.get("source", {}).get("file") != source:
        raise SystemExit(f"{function['lean_name']} is not the production body")
if [g.get("lean_name") for g in manifest.get("globals", [])] != [prefix + "MAX_LAW_EXCEPTIONS"]:
    raise SystemExit("unexpected extracted globals")
for collection in ("trait_decls", "trait_impls"):
    if manifest.get(collection) != []:
        raise SystemExit(f"unexpected extracted {collection}")
if (out / "Shape" / "FunsExternal_Template.lean").exists():
    raise SystemExit("the extraction needs external models")
if "sorry" in "".join(p.read_text(encoding="utf-8") for p in (out / "Shape").glob("*.lean")):
    raise SystemExit("extraction contains sorry")
PYMANIFEST
generated "$baseline"
lean_check "$baseline" "$semantics" Checker/LawExceptions/Proof
proof_output="$(lean_check "$baseline" "$semantics" Checker/LawExceptions/Audit)"
printf '%s\n' "$proof_output"
expected_axioms="$(printf '%s\n' \
  "'Norn.Checker.LawExceptions.check_shape_correct' depends on axioms: [propext, Classical.choice, Quot.sound]" \
  "'Norn.Checker.LawExceptions.check_shape_refusal' depends on axioms: [propext, Classical.choice, Quot.sound]" \
  "'Norn.Checker.LawExceptions.check_shape_sound' depends on axioms: [propext, Classical.choice, Quot.sound]" \
  "'Norn.Checker.LawExceptions.lifted_correct' depends on axioms: [propext, Classical.choice, Quot.sound]" \
  "'Norn.Checker.LawExceptions.lifted_sound' depends on axioms: [propext, Classical.choice, Quot.sound]")"
test "$proof_output" = "$expected_axioms"
# Check the exact propositions, not only the theorem names or an exit code.
printf '%s\n' \
  'import Checker.LawExceptions.Proof' \
  'open Aeneas Aeneas.Std Norn.Sem.Law Norn.Checker.LawExceptions norn_checker_law_exceptions in' \
  'example {T : Type} [DecidableEq T] {inst : core.cmp.PartialEq T T} (hl : Lawful inst)' \
  '    (law : T) (ids parents : Slice T) (declared : Slice Bool)' \
  '    (hp : parents.val.length = ids.val.length) (hd : declared.val.length = ids.val.length)' \
  '    {σ : Type} (cond : ℕ → σ → Bool) :' \
  '    ∃ r, check_shape inst law ids parents declared = Result.ok r ∧' \
  '      (r = .Ok () ↔ (∀ b ∈ declared.val, b = true) ∧' \
  '        (treeWith law ids.val parents.val cond).WellFormed) :=' \
  '  check_shape_correct hl law ids parents declared hp hd cond' \
  'open Aeneas Aeneas.Std Norn.Sem.Law Norn.Checker.LawExceptions norn_checker_law_exceptions in' \
  'example {T : Type} [DecidableEq T] {inst : core.cmp.PartialEq T T} (hl : Lawful inst)' \
  '    (law : T) (ids parents : Slice T) (matched level next : Slice Bool)' \
  '    (hp : parents.val.length = ids.val.length) (hlv : level.val.length = ids.val.length)' \
  '    (hnx : next.val.length = ids.val.length) :' \
  '    ∃ b l n, lifted inst law ids parents matched level next = Result.ok (b, l, n) ∧' \
  '      b = (treeWith law ids.val parents.val (fun k _ => matched.val.getD k false)).lifts ()' \
  '        (ids.val.length + 1) law :=' \
  '  lifted_correct hl law ids parents matched level next hp hlv hnx' \
  | (cd "$lean_root" && LEAN_PATH="$baseline:$result/common" lake env lean -DwarningAsError=true --stdin)
echo "law-exceptions: the production core is proved against NornSem.Law"

# witness <shape.rs> <arg>: compile the witnesses beside <shape.rs> and run them.
witness() {
  local shape="$1" arg="$2" dir
  dir="$(mktemp -d "$result/witness.XXXXXX")" || return
  cp "$shape" "$dir/shape.rs" && cp "$proof_root/witnesses.rs" "$dir/witnesses.rs" || return
  rustup run nightly-2026-08-18 rustc --edition 2024 -A warnings -o "$dir/witnesses" \
    "$dir/witnesses.rs" || return
  "$dir/witnesses" "$arg"
}
witness "$repo_root/$shape_source" all
echo "law-exceptions: the production file agrees with every witness"

# Mutants are applied to a copy of the production file; the checkout is untouched.
python3 - "$repo_root/$shape_source" "$result/mutants" <<'PYMUTANTS' > "$result/mutants.list"
import sys
from pathlib import Path

source, root = Path(sys.argv[1]), Path(sys.argv[2])
reaches = ("fn reaches_law<T: PartialEq>(law: &T, ids: &[T], parents: &[T], start: usize) -> bool {\n"
           "    let mut node = start;\n    let mut steps = 0_usize;\n")
mutants = [
    ("invalid-allows-law-id", "if !valid || id == law || !first {", "if !valid || !first {"),
    ("invalid-allows-duplicates", "if !valid || id == law || !first {", "if !valid || id == law {"),
    ("cycle-walk-too-short", reaches + "    while steps <= ids.len() {",
     reaches + "    while steps + 1 < ids.len() {"),
    ("limit-off-by-one", "if ids.len() > MAX_LAW_EXCEPTIONS {", "if ids.len() >= MAX_LAW_EXCEPTIONS {"),
    ("dangling-reported-as-cycle", "return Err(ExceptionTreeRefusal::DanglingOverride(index));",
     "return Err(ExceptionTreeRefusal::Cycle(index));"),
    ("fold-negation-dropped", "let (Some(true), Some(false)) = (matched.get(index), level.get(index))",
     "let (Some(true), Some(true)) = (matched.get(index), level.get(index))"),
    ("fold-ignores-condition", "let (Some(true), Some(false)) = (matched.get(index), level.get(index))",
     "let (Some(_), Some(false)) = (matched.get(index), level.get(index))"),
    ("fold-one-round-short", "while round < ids.len() {", "while round + 1 < ids.len() {"),
]
text = source.read_text(encoding="utf-8")
for name, old, new in mutants:
    if text.count(old) != 1:
        raise SystemExit(f"{name}: mutation site not found exactly once")
    tree = root / name / "tree" / "crates/norn-checker/src/law_exceptions"
    tree.mkdir(parents=True)
    (tree / "shape.rs").write_text(text.replace(old, new), encoding="utf-8")
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
  if lean_check "$out" "$semantics" Checker/LawExceptions/Proof > "$result/mutants/$name/proof.log" 2>&1; then
    echo "mutant $name: proof still holds (survived)" >&2
    survivors=$((survivors + 1))
  elif witness "$tree/$shape_source" "$name" > "$result/mutants/$name/witness.log" 2>&1; then
    echo "mutant $name: proof fails and its witness refutes it (detected)"
  else
    echo "mutant $name: proof fails but its witness does not refute it" >&2
    survivors=$((survivors + 1))
  fi
done < "$result/mutants.list"
test "$survivors" -eq 0
echo "law-exceptions: every seeded mutant fails the proof and is refuted by its witness"
