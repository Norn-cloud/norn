#!/usr/bin/env bash
# #718 U5/U6 (lane C-D): Charon/Aeneas extract the production certificate
# checkers (finite lens laws, DBSP algebra, linear conservation) and the
# induction frame decision, and Lean proves them against the written semantics
# in verification/semantics-v1/Checker/{Certificates,Obligations}. Every seeded
# mutant must then extract and elaborate, and fail its proof: by a kernel
# `decide` refutation where the mutated code is loop-free, otherwise by an
# error inside the correspondence proof itself (never a timeout).
set -euo pipefail
repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
tool_root="${NORN_VERIFICATION_TOOL_ROOT:-$repo_root/target/verification-tools}"
aeneas_root="$tool_root/sources/aeneas"
charon="$aeneas_root/charon/bin/charon"
aeneas="$aeneas_root/bin/aeneas"
proof_root="$repo_root/verification/semantics-v1"
result="${NORN_VERIFICATION_RESULT_ROOT:-$repo_root/target/verification-results}/checker-certificates-aeneas"
lens_source="crates/norn-checker/src/mapping/lens/decision.rs"
algebra_source="crates/norn-checker/src/metric/compute/certificate/decision.rs"
conservation_source="crates/norn-checker/src/obligations/conservation/decision.rs"
frame_source="crates/norn-checker/src/obligations/induction/frame.rs"
# The exact Lean sources this gate elaborates; nothing else may live there.
proof_files=(
  verification/semantics-v1/Checker/Certificates/AlgebraProof.lean
  verification/semantics-v1/Checker/Certificates/AlgebraReference.lean
  verification/semantics-v1/Checker/Certificates/ConservationProof.lean
  verification/semantics-v1/Checker/Certificates/ConservationReference.lean
  verification/semantics-v1/Checker/Certificates/LensProof.lean
  verification/semantics-v1/Checker/Certificates/LensReference.lean
  verification/semantics-v1/Checker/Certificates/Support.lean
  verification/semantics-v1/Checker/Obligations/FrameProof.lean
  verification/semantics-v1/Checker/Obligations/InductionReference.lean
)
test "$(cd "$repo_root" && find verification/semantics-v1/Checker/Certificates \
  verification/semantics-v1/Checker/Obligations -name '*.lean' | LC_ALL=C sort)" = \
  "$(printf '%s\n' "${proof_files[@]}")"

test "$(git -C "$aeneas_root" rev-parse HEAD)" = 74a460a2f80ecea481bbdf1a08f881633c3bb097
test "$(git -C "$aeneas_root/charon" rev-parse HEAD)" = a1d989057b50b0429ccd96c5b4c4428b9b218b21
test "$("$aeneas" -version)" = 'aeneas 74a460a2'
test "$("$charon" version)" = '0.1.240'
lean_root="$aeneas_root/backends/lean"
test "$(tr -d '\r\n' < "$lean_root/lean-toolchain")" = leanprover/lean4:v4.31.0
export RUSTUP_HOME="$tool_root/rustup" CARGO_HOME="$tool_root/cargo" CARGO_INCREMENTAL=0
rm -rf "$result/baseline" "$result/common" "$result/mutants"
mkdir -p "$result/common"

# fragment <name>: "<crate> <source> <start-from>..." for one extracted core.
fragment() {
  case "$1" in
    LensCore) echo "norn_checker_lens $lens_source crate::check_laws crate::get crate::put" ;;
    AlgebraCore) echo "norn_checker_algebra $algebra_source crate::check" ;;
    ConservationCore) echo "norn_checker_conservation $conservation_source crate::check_writer" ;;
    FrameCore) echo "norn_checker_frame $frame_source crate::discharges" ;;
    *) return 1 ;;
  esac
}
fragments=(LensCore AlgebraCore ConservationCore FrameCore)

# extract <workspace> <out> <fragment>: translate one production file standalone.
# Each step returns on failure explicitly: callers also run this under `if !`.
extract() {
  local tree="$1" out="$2" name="$3" spec crate source start
  spec="$(fragment "$name")" || return
  read -r crate source _ <<< "$spec"
  local -a roots=()
  for start in $(cut -d' ' -f3- <<< "$spec"); do roots+=(--start-from "$start"); done
  mkdir -p "$out/$name" || return
  (cd "$tree" && "$charon" rustc --preset=aeneas --error-on-warnings "${roots[@]}" \
    --dest-file "$out/$name.llbc" -- --crate-name "$crate" --crate-type lib --edition 2024 \
    "$source") || return
  "$aeneas" -backend lean -dest "$out/$name" -emit-json -split-files -warnings-as-errors \
    "$out/$name.llbc"
}

# lean_check <search> <root> <module> <olean>: elaborate one module.
lean_check() {
  local search="$1" root="$2" module="$3" olean="$4"
  mkdir -p "$(dirname "$olean")"
  (cd "$lean_root" && LEAN_PATH="$search:$result/common" lake env lean -DwarningAsError=true \
    -R "$root" -o "$olean" "$root/$module.lean")
}

# generated <out> <fragment>: the extracted code must elaborate.
generated() {
  local out="$1" name="$2" module
  for module in Types Funs; do
    lean_check "$out" "$out" "$name/$module" "$out/$name/$module.olean" || return
  done
}

# expect_output <module> <olean> <search> <expected>: elaborate, then require the
# exact `#print axioms` output (the axiom set of each named proposition).
expect_output() {
  local module="$1" olean="$2" search="$3" expected="$4" output status
  if output="$(lean_check "$search" "$proof_root" "$module" "$olean" 2>&1)"; then
    printf '%s\n' "$output"
  else
    status=$?
    printf '%s\n' "$output" >&2
    exit "$status"
  fi
  test "$output" = "$expected"
}

(cd "$lean_root" && lake build Aeneas)
common="$result/common"
# The written semantics and the generic support lemmas never import the extraction.
expect_output Checker/Certificates/Support "$common/Checker/Certificates/Support.olean" "$common" ""
expect_output Checker/Certificates/LensReference \
  "$common/Checker/Certificates/LensReference.olean" "$common" "$(printf '%s\n' \
  "'Norn.Checker.Certificates.Lens.Lens.comp_wellBehaved' depends on axioms: [propext]" \
  "'Norn.Checker.Certificates.Lens.LawsOn.toLens_wellBehaved' depends on axioms: [Classical.choice]")"
expect_output Checker/Certificates/AlgebraReference \
  "$common/Checker/Certificates/AlgebraReference.olean" "$common" "$(printf '%s\n' \
  "'Norn.Checker.Certificates.Algebra.incremental_sound' depends on axioms: [propext, Quot.sound]" \
  "'Norn.Checker.Certificates.Algebra.join_refuted_witness' depends on axioms: [propext, Classical.choice, Quot.sound]")"
expect_output Checker/Certificates/ConservationReference \
  "$common/Checker/Certificates/ConservationReference.olean" "$common" "$(printf '%s\n' \
  "'Norn.Checker.Certificates.Conservation.step_conserves' depends on axioms: [propext, Classical.choice, Quot.sound]" \
  "'Norn.Checker.Certificates.Conservation.run_conserves' depends on axioms: [propext, Classical.choice, Quot.sound]")"
expect_output Checker/Obligations/InductionReference \
  "$common/Checker/Obligations/InductionReference.olean" "$common" "$(printf '%s\n' \
  "'Norn.Checker.Obligations.inductive_safety' does not depend on any axioms" \
  "'Norn.Checker.Obligations.inductive_safety_writers' does not depend on any axioms" \
  "'Norn.Checker.Obligations.frame_preserves' does not depend on any axioms" \
  "'Norn.Checker.Obligations.exhaustive_finite' depends on axioms: [propext, Quot.sound]")"

baseline="$result/baseline"
for name in "${fragments[@]}"; do extract "$repo_root" "$baseline" "$name"; done
python3 - "$baseline" "$lens_source" "$algebra_source" "$conservation_source" "$frame_source" \
  <<'PYMANIFEST'
import json
import sys
from pathlib import Path

out = Path(sys.argv[1])
lens, algebra, conservation, frame = sys.argv[2:6]
# (crate, source, local functions, types, globals, trait declarations). No
# fragment needs a library model: every function in the closure is production code.
expected = {
    "LensCore": ("norn_checker_lens", lens, {
        "same", "position_loop.body", "position_loop", "position", "get", "put", "check_view",
        "check_laws_loop.body", "check_laws_loop", "check_laws",
    }, ["Refusal"], [], []),
    "AlgebraCore": ("norn_checker_algebra", algebra, {
        "Verdict.Insts.CoreCmpPartialEqVerdict.eq", "proved_before", "homomorphism", "join",
        "node", "over_budget_loop.body", "over_budget_loop", "over_budget", "check_loop.body",
        "check_loop", "check",
    }, ["Homomorphism", "Operator", "Verdict"], ["OPERATOR_BUDGET"], []),
    "ConservationCore": ("norn_checker_conservation", conservation, {
        "labels_in_range_loop.body", "labels_in_range_loop", "labels_in_range",
        "rows_have_width_loop.body", "rows_have_width_loop", "rows_have_width",
        "zero_row_loop.body", "zero_row_loop", "zero_row", "zero_totals_loop.body",
        "zero_totals_loop", "zero_totals", "add_weighted_row_loop.body", "add_weighted_row_loop",
        "add_weighted_row", "weighted_totals_loop.body", "weighted_totals_loop", "weighted_totals",
        "row_is_zero_loop.body", "row_is_zero_loop", "row_is_zero", "all_zero_loop.body",
        "all_zero_loop", "all_zero", "check_writer",
    }, ["Refusal"], [], ["ExactArithmetic"]),
    "FrameCore": ("norn_checker_frame", frame, {
        "same", "reads_field_loop.body", "reads_field_loop", "reads_field", "discharges",
    }, [], [], []),
}
for prefix, (crate, source, functions, types, globals_, traits) in expected.items():
    manifest = json.loads((out / prefix / "translation.json").read_text(encoding="utf-8"))
    if (manifest.get("aeneas_version"), manifest.get("charon_version"), manifest.get("crate")) != (
        "74a460a2", "0.1.240", crate
    ):
        raise SystemExit(f"{prefix}: unexpected extraction identity")
    found = manifest.get("functions", [])
    if sorted(f["lean_name"] for f in found) != sorted(f"{crate}.{name}" for name in functions):
        raise SystemExit(f"{prefix}: unexpected functions {sorted(f['lean_name'] for f in found)}")
    for function in found:
        if (not function.get("is_local") or function.get("is_opaque")
                or function.get("source", {}).get("file") != source):
            raise SystemExit(f"{prefix}: {function['lean_name']} is not the production body")
    for collection, names in (("types", types), ("globals", globals_), ("trait_decls", traits),
                              ("trait_impls", [])):
        items = manifest.get(collection, [])
        if [i.get("lean_name") for i in items] != [f"{crate}.{name}" for name in names]:
            raise SystemExit(f"{prefix}: unexpected extracted {collection}")
        if any(i.get("source", {}).get("file", source) != source for i in items):
            raise SystemExit(f"{prefix}: {collection} not bound to {source}")
    if any((out / prefix / name).exists() for name in ("FunsExternal_Template.lean",
                                                      "TypesExternal_Template.lean")):
        raise SystemExit(f"{prefix}: extraction needs an unmodelled external")
    if "sorry" in "".join(p.read_text(encoding="utf-8") for p in (out / prefix).glob("*.lean")):
        raise SystemExit(f"{prefix}: extraction contains sorry")
PYMANIFEST
for name in "${fragments[@]}"; do generated "$baseline" "$name"; done

expect_output Checker/Certificates/LensProof "$common/Checker/Certificates/LensProof.olean" \
  "$baseline" "$(printf '%s\n' \
  "'Norn.Checker.Certificates.Lens.get_correct' depends on axioms: [propext, Classical.choice, Quot.sound]" \
  "'Norn.Checker.Certificates.Lens.put_correct' depends on axioms: [propext, Classical.choice, Quot.sound]" \
  "'Norn.Checker.Certificates.Lens.check_laws_sound' depends on axioms: [propext, Classical.choice, Quot.sound]" \
  "'Norn.Checker.Certificates.Lens.check_laws_wellBehaved' depends on axioms: [propext, Classical.choice, Quot.sound]")"
expect_output Checker/Certificates/AlgebraProof "$common/Checker/Certificates/AlgebraProof.olean" \
  "$baseline" "$(printf '%s\n' \
  "'Norn.Checker.Certificates.Algebra.check_correct' depends on axioms: [propext, Classical.choice, Quot.sound]" \
  "'Norn.Checker.Certificates.Algebra.check_sound' depends on axioms: [propext, Classical.choice, Quot.sound]" \
  "'Norn.Checker.Certificates.Algebra.algebra_witnesses' depends on axioms: [propext, Classical.choice, Quot.sound]")"
expect_output Checker/Certificates/ConservationProof \
  "$common/Checker/Certificates/ConservationProof.olean" "$baseline" "$(printf '%s\n' \
  "'Norn.Checker.Certificates.Conservation.check_writer_sound' depends on axioms: [propext, Classical.choice, Quot.sound]" \
  "'Norn.Checker.Certificates.Conservation.check_writer_conserves' depends on axioms: [propext," \
  " Classical.choice," \
  " Quot.sound]")"
expect_output Checker/Obligations/FrameProof "$common/Checker/Obligations/FrameProof.olean" \
  "$baseline" "$(printf '%s\n' \
  "'Norn.Checker.Obligations.discharges_correct' depends on axioms: [propext, Classical.choice, Quot.sound]" \
  "'Norn.Checker.Obligations.discharges_sound' depends on axioms: [propext, Classical.choice, Quot.sound]" \
  "'Norn.Checker.Obligations.frame_witnesses' depends on axioms: [propext, Classical.choice, Quot.sound]")"

# Check the exact propositions, not only the theorem names or an exit code. The
# extractions share generated instance names, so each family is checked alone.
check_props() {
  (cd "$lean_root" && LEAN_PATH="$baseline:$common" lake env lean -DwarningAsError=true --stdin)
}
printf '%s\n' \
  'import Checker.Certificates.LensProof' \
  'open Aeneas.Std Norn.Checker.Certificates.Lens in' \
  'example {S : Type} {inst : core.cmp.PartialEq S S} (hEq : LawfulEq inst) {sources : Slice S}' \
  '    {gets : Slice Bool} {puts : Slice (Aeneas.Std.Array Usize 2#usize)}' \
  '    (h : norn_checker_lens.check_laws inst sources gets puts = .ok (.Ok ())) :' \
  '    LawsOn sources.val (prodGet inst sources gets) (prodPut inst sources puts) :=' \
  '  check_laws_sound hEq h' \
  'open Aeneas.Std Norn.Checker.Certificates.Lens in' \
  'example {S : Type} {inst : core.cmp.PartialEq S S} (hEq : LawfulEq inst) (sources : Slice S)' \
  '    (gets : Slice Bool) (s : S) : ∃ res, norn_checker_lens.get inst sources gets s = .ok res ∧' \
  '      TableGet sources.val gets.val s (okPart (.ok res)) := get_correct hEq sources gets s' \
  'open Aeneas.Std Norn.Checker.Certificates.Lens in' \
  'example {S : Type} {inst : core.cmp.PartialEq S S} (hEq : LawfulEq inst) (sources : Slice S)' \
  '    (puts : Slice (Aeneas.Std.Array Usize 2#usize)) (s : S) (v : Bool) :' \
  '    ∃ res, norn_checker_lens.put inst sources puts s v = .ok res ∧' \
  '      TablePut sources.val (puts.val.map pairOf) s v (okPart (.ok res)) :=' \
  '  put_correct hEq sources puts s v' \
  'open Norn.Checker.Certificates.Lens in' \
  'example {S V W : Type} {l : Lens S V} {m : Lens V W} (hl : l.WellBehaved)' \
  '    (hm : m.WellBehaved) : (l.comp m).WellBehaved := Lens.comp_wellBehaved hl hm' \
  | check_props
printf '%s\n' \
  'import Checker.Certificates.AlgebraProof' \
  'open Aeneas.Std Norn.Checker.Certificates.Algebra in' \
  'example (operators : Slice norn_checker_algebra.Operator) (signed : Bool) :' \
  '    ∃ v, norn_checker_algebra.check operators signed = .ok v ∧' \
  '      v.val.map verdictOf = verdicts (operators.val.map opOf) signed :=' \
  '  check_correct operators signed' \
  'open Aeneas.Std Norn.Checker.Certificates.Algebra in' \
  'example {V : Type} [AddCommMonoid V] {operators : Slice norn_checker_algebra.Operator}' \
  '    {signed : Bool} {v : alloc.vec.Vec norn_checker_algebra.Verdict}' \
  '    (h : norn_checker_algebra.check operators signed = .ok v) (I : Interp V)' \
  '    (hI : I.Lawful signed) (inp dinp : Nat → V) (i : Nat) (hi : v.val[i]? = some .Proved) :' \
  '    evalAt I (operators.val.map opOf) (fun k => inp k + dinp k) i =' \
  '      evalAt I (operators.val.map opOf) inp i +' \
  '        deltaAt I (operators.val.map opOf) inp dinp i :=' \
  '  check_sound h I hI inp dinp i hi' \
  | check_props
printf '%s\n' \
  'import Checker.Certificates.ConservationProof' \
  'open Aeneas.Std Norn.Checker.Certificates.Conservation in' \
  'example {N : Type} {inst : norn_checker_conservation.ExactArithmetic N} {val : N → ℚ}' \
  '    (hL : Lawful inst val) {partition : Slice Usize} {weights : Slice N}' \
  '    {rows : Slice (alloc.vec.Vec N)} {columns : Usize}' \
  '    (h : norn_checker_conservation.check_writer inst partition weights rows columns =' \
  '      .ok (.Ok ())) :' \
  '    Balanced partition.length columns.val (fun i => (partition.val.getD i 0#usize).val)' \
  '      (fun i => entry val weights.val i) (fun i c => cell val rows.val i c) :=' \
  '  check_writer_sound hL h' \
  'open Norn.Checker.Certificates.Conservation in' \
  'example {n columns : Nat} {part : Nat → Nat} {w : Nat → ℚ} {rows : Nat → Nat → ℚ}' \
  '    (hb : Balanced n columns part w rows) (x σ : Nat → ℚ) (l : Nat) :' \
  '    classSum n part w l (step columns rows x σ) = classSum n part w l σ :=' \
  '  step_conserves hb x σ l' \
  | check_props
printf '%s\n' \
  'import Checker.Obligations.FrameProof' \
  'open Aeneas.Std Norn.Checker.Obligations in' \
  'example {F : Type} {inst : core.cmp.PartialEq F F} (hEq : LawfulEq inst)' \
  '    (subject object written : F) (reads : Option (Slice F)) :' \
  '    ∃ b, norn_checker_frame.discharges inst subject object written reads = .ok b ∧' \
  '      (b = true ↔ ∃ fs, reads = some fs ∧ object = subject ∧ written ∉ fs.val) :=' \
  '  discharges_correct hEq subject object written reads' \
  'open Norn.Checker.Obligations in' \
  'example {S : Type} {init : S → Prop} {step : S → S → Prop} {I : S → Prop}' \
  '    (hinit : ∀ s, init s → I s) (hpres : ∀ s t, I s → step s t → I t) :' \
  '    ∀ s, Reachable init step s → I s := inductive_safety hinit hpres' \
  'open Norn.Checker.Obligations in' \
  'example {bs : List (Int × Int)} {P : List Int → Prop} (h : ∀ xs ∈ enumerate bs, P xs) :' \
  '    ∀ xs, InBounds bs xs → P xs := exhaustive_finite h' \
  | check_props
echo "checker-certificates: production certificate checkers and frame decision proved against the written semantics"

# Mutants are applied to a hard-linked copy of the checker sources; the edited
# file is replaced, never written through, so the checkout is untouched.
tree="$result/mutants/tree"
mkdir -p "$tree/crates/norn-checker"
cp -al "$repo_root/crates/norn-checker/src" "$tree/crates/norn-checker/"
python3 - "$tree" "$result/mutants" "$lens_source" "$algebra_source" "$conservation_source" \
  "$frame_source" <<'PYMUTANTS' > "$result/mutants/list"
import sys
from pathlib import Path

tree, root = Path(sys.argv[1]), Path(sys.argv[2])
lens, algebra, conservation, frame = sys.argv[3:7]
# (name, file, fragment, proof module, detection, old, new). `decide`: the
# kernel refutes a concrete witness; `proof`: the correspondence proof fails.
mutants = [
    ("lens-skip-getput", lens, "LensCore", "LensProof", "proof",
     "if !same(put(sources, put_table, source, current)?, source) {",
     "if !same(put(sources, put_table, source, current)?, put(sources, put_table, source, current)?) {"),
    ("lens-putget-inverted", lens, "LensCore", "LensProof", "proof",
     "if get(sources, get_table, updated)? != view {",
     "if get(sources, get_table, updated)? == view {"),
    ("lens-skip-putput-true", lens, "LensCore", "LensProof", "proof",
     "    if !same(\n        put(sources, put_table, updated, true)?,\n"
     "        put(sources, put_table, source, true)?,\n    ) {\n"
     "        return Err(Refusal::PutPut(row, view, true));\n    }\n", ""),
    ("lens-skip-view-true", lens, "LensCore", "LensProof", "proof",
     "        check_view(sources, get_table, put_table, row, true)?;\n", ""),
    ("lens-position-mismatch", lens, "LensCore", "LensProof", "proof",
     "if same(&sources[row], source) {", "if !same(&sources[row], source) {"),
    ("lens-put-views-swapped", lens, "LensCore", "LensProof", "proof",
     "if view { put[row][1] } else { put[row][0] }", "if view { put[row][0] } else { put[row][1] }"),
    ("lens-skip-first-row", lens, "LensCore", "LensProof", "proof",
     "    let mut row = 0;\n    while row < sources.len() {\n        let source = &sources[row];",
     "    let mut row = 1;\n    while row < sources.len() {\n        let source = &sources[row];"),
    ("algebra-join-drops-cross-term", algebra, "AlgebraCore", "AlgebraProof", "decide",
     "} else if coefficients[2] != 1 {", "} else if coefficients[2] != 1 && coefficients[2] != 0 {"),
    ("algebra-monoid-under-deletions", algebra, "AlgebraCore", "AlgebraProof", "decide",
     "Some(Homomorphism::MonoidSum) if signed_changes =>",
     "Some(Homomorphism::MonoidSum) if !signed_changes =>"),
    ("algebra-undeclared-fold-proved", algebra, "AlgebraCore", "AlgebraProof", "decide",
     "None => Verdict::UndeclaredFold,", "None => Verdict::Proved,"),
    ("algebra-forward-dependency", algebra, "AlgebraCore", "AlgebraProof", "decide",
     "dependency < results.len() && results[dependency] == Verdict::Proved",
     "dependency <= results.len() && results[dependency] == Verdict::Proved"),
    ("algebra-dependency-unproved", algebra, "AlgebraCore", "AlgebraProof", "proof",
     "dependency < results.len() && results[dependency] == Verdict::Proved",
     "dependency < results.len()"),
    ("algebra-budget-off-by-one", algebra, "AlgebraCore", "AlgebraProof", "proof",
     "if operators.len() > OPERATOR_BUDGET {", "if operators.len() >= OPERATOR_BUDGET {"),
    ("conservation-accept-unbalanced", conservation, "ConservationCore", "ConservationProof",
     "proof", "if all_zero(&totals) {", "if true || all_zero(&totals) {"),
    ("conservation-ignore-weight", conservation, "ConservationCore", "ConservationProof", "proof",
     "row[c].exact_mul(weight)", "row[c].exact_mul(N::exact_zero())"),
    ("conservation-overwrite-total", conservation, "ConservationCore", "ConservationProof",
     "proof", "total[c].exact_add(product)", "product.exact_add(N::exact_zero())"),
    ("conservation-zero-check-last-column", conservation, "ConservationCore",
     "ConservationProof", "proof", "if !row[c].exact_is_zero() {",
     "if c + 1 == row.len() && !row[c].exact_is_zero() {"),
    ("conservation-skip-last-coordinate", conservation, "ConservationCore", "ConservationProof",
     "proof", "    while i < partition.len() {\n        add_weighted_row",
     "    while i + 1 < partition.len() {\n        add_weighted_row"),
    ("conservation-skip-first-class", conservation, "ConservationCore", "ConservationProof",
     "proof", "    let mut r = 0;\n    while r < totals.len() {",
     "    let mut r = 1;\n    while r < totals.len() {"),
    ("frame-ignores-writer-type", frame, "FrameCore", "FrameProof", "proof",
     "Some(fields) => same(writer_object, law_subject) && !reads_field(fields, written),",
     "Some(fields) => !reads_field(fields, written),"),
    ("frame-unknown-reads-discharge", frame, "FrameCore", "FrameProof", "decide",
     "None => false,", "None => true,"),
    ("frame-misses-last-read", frame, "FrameCore", "FrameProof", "proof",
     "while i < reads.len() {", "while i + 1 < reads.len() {"),
]
for name, path, fragment, proof, detection, old, new in mutants:
    source = (tree / path).read_text(encoding="utf-8")
    if source.count(old) != 1:
        raise SystemExit(f"{name}: mutation site not found exactly once")
    staged = root / name / Path(path).name
    staged.parent.mkdir(parents=True)
    staged.write_text(source.replace(old, new), encoding="utf-8")
    print(name, path, fragment, proof, detection)
PYMUTANTS
test "$(wc -l < "$result/mutants/list")" -eq 22
survivors=0
while read -r name path name_fragment proof detection; do
  pristine="$result/mutants/$name.pristine"
  cp "$tree/$path" "$pristine"
  rm "$tree/$path"
  cp "$result/mutants/$name/$(basename "$path")" "$tree/$path"
  out="$result/mutants/$name/out"
  # A mutant must extract and its code must elaborate: only the proof may fail.
  if ! extract "$tree" "$out" "$name_fragment" > "$result/mutants/$name/extract.log" 2>&1 ||
    ! generated "$out" "$name_fragment" >> "$result/mutants/$name/extract.log" 2>&1; then
    tail -n 40 "$result/mutants/$name/extract.log" >&2
    echo "mutant $name: did not extract and elaborate; it cannot test the proof" >&2
    exit 1
  fi
  rm "$tree/$path"
  cp "$pristine" "$tree/$path"
  family="$(dirname "$(cd "$proof_root" && find Checker -name "$proof.lean")")"
  log="$result/mutants/$name/proof.log"
  if lean_check "$out" "$proof_root" "$family/$proof" "$out/$proof.olean" > "$log" 2>&1; then
    echo "mutant $name: proof still holds (survived)" >&2
    survivors=$((survivors + 1))
  elif grep -q "maximum number of heartbeats" "$log"; then
    tail -n 20 "$log" >&2
    echo "mutant $name: proof fails only by timeout; it does not test the proof" >&2
    survivors=$((survivors + 1))
  elif [[ $detection == decide ]] &&
    grep -q "$proof.lean:[0-9]*:[0-9]*: error: Tactic \`decide\` proved that the proposition" "$log"; then
    echo "mutant $name: a concrete witness is refuted by decide (detected)"
  elif [[ $detection == proof ]] && grep -q "$proof.lean:[0-9]*:[0-9]*: error" "$log"; then
    echo "mutant $name: the correspondence proof fails (detected)"
  else
    tail -n 20 "$log" >&2
    echo "mutant $name: proof fails without the expected $detection detection" >&2
    survivors=$((survivors + 1))
  fi
done < "$result/mutants/list"
test "$survivors" -eq 0
echo "checker-certificates: every seeded mutant is refuted or fails its correspondence proof"
