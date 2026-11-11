#!/usr/bin/env bash
# #718 lane C-C (U3, U4): Charon/Aeneas extract norm precedence's decision core
# (crates/norn-checker/src/norm_precedence/decision.rs), and Lean proves it
# sound against the written semantics in verification/semantics-v1/NornSem:
# `admits` decides admissibility and an exact closure certificate,
# `resolution_holds` decides that a context's resolution is the unique stable
# set of survivors, and every admitted order satisfies the meta-theorems
# (downstream can't defeat upstream, strict-layer preservation, overlays
# preserve upstream survivors).
#
# Every seeded mutant must (a) extract and elaborate, (b) fail the proof, and
# (c) be refuted by its pinned witness in
# verification/semantics-v1/Checker/NormPrecedence/witnesses.rs: the mutated
# core, compiled and run on that input, disagrees with the oracle's recorded
# verdict. The refutation is concrete execution, not a closed `decide`
# theorem: the extracted loops are Aeneas `loop` (a `partial_fixpoint`, which
# the kernel cannot unfold) and every index is a `usize` whose width
# (`System.Platform.numBits`) is opaque to the kernel, so `decide` cannot
# evaluate this code, and `native_decide` is banned by check-proof-holes.
# Lanes in the same position can reuse this pattern.
set -euo pipefail
repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
tool_root="${NORN_VERIFICATION_TOOL_ROOT:-$repo_root/target/verification-tools}"
aeneas_root="$tool_root/sources/aeneas"
charon="$aeneas_root/charon/bin/charon"
aeneas="$aeneas_root/bin/aeneas"
proof_root="$repo_root/verification/semantics-v1"
result="${NORN_VERIFICATION_RESULT_ROOT:-$repo_root/target/verification-results}/norm-precedence-aeneas"
decision_source="crates/norn-checker/src/norm_precedence/decision.rs"
witnesses="$proof_root/Checker/NormPrecedence/witnesses.rs"
# The exact Lean sources this gate elaborates; nothing else may live in its
# directories.
proof_files=(
  verification/semantics-v1/Checker/NormPrecedence/Admits.lean
  verification/semantics-v1/Checker/NormPrecedence/AdmitsSound.lean
  verification/semantics-v1/Checker/NormPrecedence/DecisionProof.lean
  verification/semantics-v1/Checker/NormPrecedence/Loops.lean
  verification/semantics-v1/Checker/NormPrecedence/Resolution.lean
  verification/semantics-v1/NornSem/Meta/Precedence.lean
)
test "$(cd "$repo_root" && find verification/semantics-v1/Checker/NormPrecedence \
  verification/semantics-v1/NornSem/Meta -name '*.lean' | LC_ALL=C sort)" = \
  "$(printf '%s\n' "${proof_files[@]}")"
test -f "$repo_root/verification/semantics-v1/NornSem/Norm.lean"

test "$(git -C "$aeneas_root" rev-parse HEAD)" = 74a460a2f80ecea481bbdf1a08f881633c3bb097
test "$(git -C "$aeneas_root/charon" rev-parse HEAD)" = a1d989057b50b0429ccd96c5b4c4428b9b218b21
test "$("$aeneas" -version)" = 'aeneas 74a460a2'
test "$("$charon" version)" = '0.1.240'
lean_root="$aeneas_root/backends/lean"
test "$(tr -d '\r\n' < "$lean_root/lean-toolchain")" = leanprover/lean4:v4.31.0
charon_toolchain="$(sed -n 's/^channel = "\(.*\)"$/\1/p' "$aeneas_root/charon/rust-toolchain")"
test "$charon_toolchain" = nightly-2026-08-18
export RUSTUP_HOME="$tool_root/rustup" CARGO_HOME="$tool_root/cargo" CARGO_INCREMENTAL=0
rm -rf "$result/baseline" "$result/common" "$result/mutants"
mkdir -p "$result/common"

# extract <tree> <out>: translate the decision core from <tree>. The file
# depends on core and alloc alone, so it is compiled standalone as its own
# crate: nothing is substituted for it and no other checker code is extracted.
extract() {
  local tree="$1" out="$2"
  mkdir -p "$out/NormDecision" || return
  printf '#![allow(dead_code)]\n#[path = "%s"]\npub mod decision;\n' \
    "$tree/$decision_source" > "$out/decision_root.rs" || return
  (cd "$tree" && "$charon" rustc --preset=aeneas --error-on-warnings \
    --start-from crate::decision::admits --start-from crate::decision::resolution_holds \
    --dest-file "$out/norm_decision.llbc" -- --crate-name norn_checker_norm_decision \
    --crate-type lib --edition 2024 "$out/decision_root.rs") || return
  "$aeneas" -backend lean -dest "$out/NormDecision" -emit-json -split-files \
    -warnings-as-errors "$out/norm_decision.llbc"
}

# lean_check <search> <module> <olean>: elaborate one module of the proof root
# against the extraction in <search> and the shared written semantics. Each
# module's olean path is explicit, so no output directory shadows another's.
lean_check() {
  local search="$1" module="$2" olean="$3" root="${4:-$proof_root}"
  mkdir -p "$(dirname "$olean")"
  (cd "$lean_root" && LEAN_PATH="$search:$result/common" lake env lean -DwarningAsError=true \
    -R "$root" -o "$olean" "$root/$module.lean")
}

# generated <out>: the extracted code must elaborate.
generated() {
  local out="$1" module
  for module in Types Funs; do
    lean_check "$out" "NormDecision/$module" "$out/NormDecision/$module.olean" "$out" || return
  done
}

# witness <tree> <out> <mode>: compile the core from <tree> next to the pinned
# witnesses and run it. `--baseline` requires every verdict to equal the
# oracle's; a mutant name requires that mutant's witness to disagree.
witness() {
  local tree="$1" out="$2" mode="$3"
  mkdir -p "$out" || return
  cat > "$out/witness.rs" <<RUST || return
#![allow(dead_code)]
#[path = "$tree/$decision_source"]
mod decision;
mod witnesses {
    include!("$witnesses");
}
use decision::{Modality, Norm};

fn verdict(w: &witnesses::Witness) -> bool {
    let grades = [Modality::Obligation, Modality::Permission, Modality::Prohibition];
    let norms: Vec<Norm> = w
        .norms
        .iter()
        .map(|&(grade, layer, protected, active)| Norm {
            modality: grades[usize::from(grade)],
            layer,
            protected,
            active: active.to_vec(),
        })
        .collect();
    let reach: Vec<Vec<bool>> = w.reach.iter().map(|row| row.to_vec()).collect();
    let admitted = decision::admits(&norms, w.edges, &reach, w.contexts);
    match &w.resolution {
        None => admitted,
        Some(r) => {
            admitted
                && decision::resolution_holds(
                    &norms, &reach, r.context, r.survivors, r.obligations, r.conflicts, r.defeats,
                )
        }
    }
}

fn main() {
    let mode = std::env::args().nth(1).expect("mode");
    let mut seen = 0;
    for w in witnesses::WITNESSES {
        let got = verdict(w);
        if mode == "--baseline" {
            assert_eq!(got, w.expected, "baseline core disagrees with the oracle on {}", w.mutant);
            seen += 1;
        } else if w.mutant == mode {
            assert_ne!(got, w.expected, "witness {} does not refute its mutant", w.mutant);
            println!("witness {}: core {got}, oracle {}: refuted", w.mutant, w.expected);
            seen += 1;
        }
    }
    assert!(seen > 0, "no witness for {mode}");
}
RUST
  rustup run "$charon_toolchain" rustc --edition 2024 -o "$out/witness" "$out/witness.rs" &&
    "$out/witness" "$mode"
}

(cd "$lean_root" && lake build Aeneas)
# The written semantics never import the extraction. The loop lemmas don't
# either, but they are built next to the other Checker modules: Lean resolves
# the `Checker` prefix in one search root.
lean_check "$result/common" NornSem/Norm "$result/common/NornSem/Norm.olean"
lean_check "$result/common" NornSem/Meta/Precedence "$result/common/NornSem/Meta/Precedence.olean"

baseline="$result/baseline"
extract "$repo_root" "$baseline"
python3 - "$baseline" "$decision_source" <<'PYMANIFEST'
import json
import sys
from pathlib import Path

out, source = Path(sys.argv[1]), sys.argv[2]
manifest = json.loads((out / "NormDecision" / "translation.json").read_text(encoding="utf-8"))
if (manifest.get("aeneas_version"), manifest.get("charon_version"), manifest.get("crate")) != (
    "74a460a2", "0.1.240", "norn_checker_norm_decision"
):
    raise SystemExit("unexpected extraction identity")
looped = [
    "clashes_resolved", "clashes_resolved_from", "co_active", "context_in_range",
    "defeated_by_survivor", "defeats_cited", "defeats_complete", "edges_in_range",
    "edges_respect_layers", "indices_in_range", "irreflexive", "justified", "listed",
    "loser_recorded", "obligations_listed", "obligations_survive", "reach_contains_edges",
    "reach_justified", "reach_transitive", "row_justified", "row_transitive", "row_within",
    "rows_shaped", "survivors_stable",
]
plain = [
    "active_in", "admits", "clash_resolved", "defeat_cited", "defeat_recorded", "obliges",
    "opposed", "prohibits", "resolution_holds", "survives_exactly", "well_shaped",
]
prefix = "norn_checker_norm_decision.decision."
expected = {prefix + name for name in plain}
for name in looped:
    expected |= {prefix + name, prefix + name + "_loop", prefix + name + "_loop.body"}
functions = manifest.get("functions", [])
if {f.get("lean_name") for f in functions} != expected or len(functions) != len(expected):
    raise SystemExit(f"unexpected extracted functions {sorted(f.get('lean_name') for f in functions)}")
for function in functions:
    if not function.get("is_local") or function.get("is_opaque"):
        raise SystemExit(f"{function['lean_name']} is not a translated production body")
types = [t.get("lean_name") for t in manifest.get("types", [])]
if types != [prefix + "Modality", prefix + "Norm"]:
    raise SystemExit(f"unexpected extracted types {types}")
for item in [*functions, *manifest["types"]]:
    if item.get("source", {}).get("file") != source:
        raise SystemExit(f"{item.get('lean_name')} is not bound to {source}")
for collection in ("globals", "trait_decls", "trait_impls"):
    if manifest.get(collection) != []:
        raise SystemExit(f"unexpected extracted {collection}")
if any((out / "NormDecision" / name).exists() for name in ("FunsExternal_Template.lean",
                                                           "TypesExternal_Template.lean")):
    raise SystemExit("extraction needs an unmodelled external")
if "sorry" in "".join(p.read_text(encoding="utf-8") for p in (out / "NormDecision").glob("*.lean")):
    raise SystemExit("extraction contains sorry")
PYMANIFEST
generated "$baseline"
for module in Loops Admits AdmitsSound Resolution; do
  lean_check "$baseline" "Checker/NormPrecedence/$module" \
    "$baseline/Checker/NormPrecedence/$module.olean"
done
if proof_output="$(lean_check "$baseline" Checker/NormPrecedence/DecisionProof \
  "$baseline/Checker/NormPrecedence/DecisionProof.olean" 2>&1)"; then
  printf '%s\n' "$proof_output"
else
  proof_status=$?
  printf '%s\n' "$proof_output" >&2
  exit "$proof_status"
fi
test "$proof_output" = "$(printf '%s\n' \
  "'Norn.Checker.NormPrecedence.admits_sound' depends on axioms: [propext, Classical.choice, Quot.sound]" \
  "'Norn.Checker.NormPrecedence.resolution_sound' depends on axioms: [propext, Classical.choice, Quot.sound]" \
  "'Norn.Checker.NormPrecedence.admitted_meta' depends on axioms: [propext, Classical.choice, Quot.sound]" \
  "'Norn.Sem.Norm.stable_unique' depends on axioms: [propext, Classical.choice, Quot.sound]" \
  "'Norn.Sem.Norm.stable_no_conflict' does not depend on any axioms" \
  "'Norn.Sem.Norm.uncovered_not_permits' does not depend on any axioms" \
  "'Norn.Sem.Norm.downstream_cannot_defeat_upstream' does not depend on any axioms" \
  "'Norn.Sem.Norm.defeat_same_layer' does not depend on any axioms" \
  "'Norn.Sem.Norm.protected_survives' does not depend on any axioms" \
  "'Norn.Sem.Norm.permission_respects_invariants' depends on axioms: [propext]" \
  "'Norn.Sem.Norm.overlay_preserves_upstream' depends on axioms: [propext, Classical.choice, Quot.sound]")"
# Check the exact propositions, not only the theorem names or an exit code.
cat > "$baseline/Propositions.lean" <<'LEAN'
import Checker.NormPrecedence.DecisionProof
open Aeneas Aeneas.Std Result norn_checker_norm_decision Norn.Sem.Norm Norn.Checker.NormPrecedence

example (norms : Slice decision.Norm) (edges : Slice (Usize × Usize))
    (reach : Slice (alloc.vec.Vec Bool)) (contexts : Usize) :
    ∃ b, decision.admits norms edges reach contexts = ok b ∧
      (b = true → Admissible (catalog norms edges contexts) ∧
        ∀ i j, R reach i j = true ↔ Prec (catalog norms edges contexts) i j) :=
  admits_sound norms edges reach contexts

example (norms : Slice decision.Norm) (edges : Slice (Usize × Usize))
    (reach : Slice (alloc.vec.Vec Bool)) (contexts : Usize)
    (hadm : decision.admits norms edges reach contexts = ok true) (c : Usize)
    (survivors obligations : Slice Usize) (conflicts : Usize) (defeats : Slice (Usize × Usize)) :
    ∃ b, decision.resolution_holds norms reach c survivors obligations conflicts defeats = ok b ∧
      (b = true →
        let C := catalog norms edges contexts
        let S := survivorSet survivors
        conflicts.val = 0 ∧
        Stable C c.val S ∧
        (∀ S', Stable C c.val S' → ∀ i, S' i ↔ S i) ∧
        (∀ i j, S i → S j → ¬ (C.modality i).opposed (C.modality j)) ∧
        (∀ i, Mem obligations i ↔ S i ∧ C.modality i = .obligation) ∧
        (∀ d ∈ defeats.val, S d.1.val ∧ C.active d.2.val c.val = true ∧ Defeats C d.1.val d.2.val) ∧
        (∀ i, i < C.size → C.active i c.val = true → S i ∨ ∃ d ∈ defeats.val, d.2.val = i)) :=
  resolution_sound norms edges reach contexts hadm c survivors obligations conflicts defeats

example (norms : Slice decision.Norm) (edges : Slice (Usize × Usize))
    (reach : Slice (alloc.vec.Vec Bool)) (contexts : Usize)
    (hadm : decision.admits norms edges reach contexts = ok true) :
    let C := catalog norms edges contexts
    (∀ w l, Prec C w l → C.layer w ≤ C.layer l ∧ C.isProtected l = false) ∧
    (∀ c S, c < C.contexts → Stable C c S → ∀ q, S q → C.modality q = .permission →
      ∀ p, p < C.size → C.active p c = true → C.modality p = .prohibition →
        C.isProtected p = false ∧ C.layer q ≤ C.layer p) ∧
    (∀ L c S S', Stable C c S → Stable (C.upTo L) c S' → ∀ i, C.layer i ≤ L → (S i ↔ S' i)) :=
  admitted_meta norms edges reach contexts hadm

example (C : Catalog) (S : Nat → Prop) (h : Uncovered S) : ¬ Permits C S :=
  uncovered_not_permits h
LEAN
(cd "$lean_root" && LEAN_PATH="$baseline:$result/common" lake env lean -DwarningAsError=true \
  "$baseline/Propositions.lean")
witness "$repo_root" "$baseline/witness" --baseline
echo "norm-precedence: the production decision core is proved sound against the written semantics"

# Mutants are applied to a hard-linked copy of the source; the edited file is
# replaced, never written through, so the checkout is untouched.
tree="$result/mutants/tree"
mkdir -p "$tree/crates/norn-checker/src"
cp -al "$repo_root/crates/norn-checker/src/norm_precedence" "$tree/crates/norn-checker/src/"
python3 - "$tree" "$result/mutants" "$decision_source" <<'PYMUTANTS' > "$result/mutants/list"
import sys
from pathlib import Path

tree, root, path = Path(sys.argv[1]), Path(sys.argv[2]), sys.argv[3]
layer = "norms[high].layer > norms[low].layer || norms[low].protected"
clash = "(norms[i].layer == norms[j].layer && (reach[i][j] || reach[j][i]))"
mutants = [
    ("layer-edge-points-up", layer, "norms[low].protected"),
    ("protected-target-allowed", layer, "norms[high].layer > norms[low].layer"),
    ("cycle-admitted", "if reach[i][i] {\n            return false;",
     "if reach[i][i] {\n            return true;"),
    ("closure-transitivity-unchecked", "if reach[source][j] && !reach[sink][j] {",
     "if reach[source][j] && !reach[sink][j] && j > reach.len() {"),
    ("closure-justification-unchecked", "if reach[i][j] && !justified(edges, reach, i, j) {",
     "if reach[i][j] && !justified(edges, reach, i, j) && j > reach.len() {"),
    ("cross-layer-clash-admitted", "|| " + clash, "|| reach[i][j] || reach[j][i]"),
    ("unordered-clash-admitted", clash,
     "(norms[i].layer == norms[j].layer && (reach[i][j] || reach[j][i] || i < j))"),
    ("co-activity-ignored", "if active_in(left, c) && active_in(right, c) {",
     "if active_in(left, c) || active_in(right, c) {"),
    ("defeat-ignores-opposition",
     "if reach[winner][loser] && opposed(&norms[winner], &norms[loser]) {",
     "if reach[winner][loser] {"),
    ("survivors-unchecked", "if !survives_exactly(norms, reach, survivors, context, i) {",
     "if !survives_exactly(norms, reach, survivors, context, i) && i > norms.len() {"),
    ("obligation-grade-unchecked",
     "if !obliges(norms[norm].modality) || !listed(survivors, norm) {",
     "if !listed(survivors, norm) {"),
    ("conflicts-ignored", "    conflicts == 0\n", "    conflicts == conflicts\n"),
    ("defeat-priority-unchecked", "        && reach[winner][loser]\n", ""),
    ("defeats-incomplete",
     "if !defeat_recorded(survivors, defeats, active_in(&norms[i], context), i) {",
     "if !defeat_recorded(survivors, defeats, active_in(&norms[i], context), i) && i > norms.len() {"),
]
source = (tree / path).read_text(encoding="utf-8")
for name, old, new in mutants:
    if source.count(old) != 1:
        raise SystemExit(f"{name}: mutation site not found exactly once")
    staged = root / name / Path(path).name
    staged.parent.mkdir(parents=True)
    staged.write_text(source.replace(old, new), encoding="utf-8")
    print(name, path)
PYMUTANTS
test "$(wc -l < "$result/mutants/list")" -eq 14
survivors=0
while read -r name path; do
  pristine="$result/mutants/$name.pristine"
  cp "$tree/$path" "$pristine"
  rm "$tree/$path"
  cp "$result/mutants/$name/$(basename "$path")" "$tree/$path"
  out="$result/mutants/$name/out"
  # (a) A mutant must extract and its code must elaborate: only the proof may fail.
  if ! extract "$tree" "$out" > "$result/mutants/$name/extract.log" 2>&1 ||
    ! generated "$out" >> "$result/mutants/$name/extract.log" 2>&1; then
    tail -n 40 "$result/mutants/$name/extract.log" >&2
    echo "mutant $name: did not extract and elaborate; it cannot test the proof" >&2
    exit 1
  fi
  # (c) Its pinned witness must refute it by execution.
  if ! witness "$tree" "$result/mutants/$name/witness" "$name" \
    > "$result/mutants/$name/witness.log" 2>&1 ||
    ! grep -q "^witness $name: core .*: refuted$" "$result/mutants/$name/witness.log"; then
    tail -n 20 "$result/mutants/$name/witness.log" >&2
    echo "mutant $name: its witness does not refute it" >&2
    survivors=$((survivors + 1))
  fi
  rm "$tree/$path"
  cp "$pristine" "$tree/$path"
  # (b) The proof must fail on it.
  proved=true
  for module in Loops Admits AdmitsSound Resolution DecisionProof; do
    if ! lean_check "$out" "Checker/NormPrecedence/$module" \
      "$out/Checker/NormPrecedence/$module.olean" > "$result/mutants/$name/proof.log" 2>&1; then
      proved=false
      break
    fi
  done
  if $proved; then
    echo "mutant $name: proof still holds (survived)" >&2
    survivors=$((survivors + 1))
  else
    echo "mutant $name: $module fails; $(grep "^witness $name: " "$result/mutants/$name/witness.log")"
  fi
done < "$result/mutants/list"
test "$survivors" -eq 0
echo "norm-precedence: every seeded mutant fails the proof and is refuted by its witness"
