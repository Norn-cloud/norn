#!/usr/bin/env bash
# #718 P3 (owner D78/D83): elaborate the shared Lean semantics in
# verification/semantics-v1 (namespace Norn.Sem) in the pinned Aeneas Lean
# environment, diff the exact axiom inventory, and require every seeded
# definition mutant to break a theorem.
set -euo pipefail
repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
tool_root="${NORN_VERIFICATION_TOOL_ROOT:-$repo_root/target/verification-tools}"
aeneas_root="$tool_root/sources/aeneas"
lean_root="$aeneas_root/backends/lean"
semantics="$repo_root/verification/semantics-v1"
result="${NORN_VERIFICATION_RESULT_ROOT:-$repo_root/target/verification-results}/semantics-v1"
# Elaboration order, and the exact Lean sources of the semantics.
modules=(NornSem/Exact NornSem/Value NornSem/Law NornSem/Obligation)
proof_files=(
  verification/semantics-v1/Audit.lean
  verification/semantics-v1/NornSem/Exact.lean
  verification/semantics-v1/NornSem/Law.lean
  verification/semantics-v1/NornSem/Obligation.lean
  verification/semantics-v1/NornSem/Value.lean
)
# The coverage manifest binds exactly these sources to this gate; other gates
# own the rest of the directory (for example NornSem/Norm.lean).
test "$(python3 - "$repo_root" <<'PYGATE'
import json
import sys
from pathlib import Path

index = json.loads((Path(sys.argv[1]) / "verification/coverage-v1/manifest/index.json").read_text())
gates = index["leanCompositeGates"]
print("\n".join(sorted(p for p, g in gates.items() if g == "scripts/verify-semantics-lean.sh")))
PYGATE
)" = "$(printf '%s\n' "${proof_files[@]}" | LC_ALL=C sort)"
for file in "${proof_files[@]}"; do test -f "$repo_root/$file"; done

test "$(git -C "$aeneas_root" rev-parse HEAD)" = 74a460a2f80ecea481bbdf1a08f881633c3bb097
test "$(tr -d '\r\n' < "$lean_root/lean-toolchain")" = leanprover/lean4:v4.31.0
rm -rf "$result"
mkdir -p "$result"

# elaborate <source root> <out> [first module]: the modules from the first one
# on (default: all), in order, warnings as errors. Earlier modules resolve from
# the baseline build.
elaborate() {
  local root="$1" out="$2" from="${3:-${modules[0]}}" module started=false
  for module in "${modules[@]}"; do
    if [[ "$module" == "$from" ]]; then started=true; fi
    if ! $started; then continue; fi
    mkdir -p "$(dirname "$out/$module.olean")" || return
    (cd "$lean_root" && LEAN_PATH="$out:$result/baseline" lake env lean -DwarningAsError=true \
      -R "$root" -o "$out/$module.olean" "$root/$module.lean") || return
  done
}

(cd "$lean_root" && lake build Aeneas)
elaborate "$semantics" "$result/baseline"
# The exact axiom output is diffed, not a source grep.
(cd "$lean_root" && LEAN_PATH="$result/baseline" lake env lean -DwarningAsError=true \
  "$semantics/Audit.lean") > "$result/axioms.txt"
diff -u "$semantics/axioms.txt" "$result/axioms.txt"
echo "semantics-v1: every module elaborates; the axiom inventory is exact"

# Each mutant weakens one definition so that a stated theorem becomes false.
# It is refuted, not merely left unproved: the mutated module carries a closed
# theorem over a concrete instance proving the negation of the original
# statement, which must elaborate (no error at or after it) with no sorryAx.
python3 - "$semantics" "$result/mutants" <<'PYMUTANTS' > "$result/mutants.list"
import shutil
import sys
from pathlib import Path

source, root = Path(sys.argv[1]), Path(sys.argv[2])
mutants = [
    # A trailing zero may remain, so (1, 0) and (10, 1) are both canonical.
    ("canonical-keeps-trailing-zero", "NornSem/Exact.lean",
     "(d.mantissa = 0 → d.scale = 0) ∧ (0 < d.scale → ¬ (10 : ℤ) ∣ d.mantissa)",
     "(d.mantissa = 0 → d.scale = 0)",
     """open Norn.Sem.Exact in
theorem mutantRefuted : ¬ ∀ d₁ d₂ : Dec, d₁.Canonical → d₂.Canonical → d₁.val = d₂.val → d₁ = d₂ := by
  intro h
  have := h ⟨1, 0⟩ ⟨10, 1⟩ (fun _ => rfl) (fun h => absurd h (by decide)) (by norm_num [Dec.val])
  simp at this
"""),
    # Object keys need not be ordered, so a key may repeat.
    ("object-keys-unordered", "NornSem/Value.lean",
     "(fields.map Prod.fst).Pairwise (· < ·) → (∀ p ∈ fields, WF p.2) → WF (.object fields)",
     "(∀ p ∈ fields, WF p.2) → WF (.object fields)",
     """open Norn.Sem in
theorem mutantRefuted : ¬ ∀ (fields : List (String × Value)) (k : String) (v : Value),
    Value.WF (.object fields) → ((Value.object fields).field k = some v ↔ (k, v) ∈ fields) := by
  intro h
  have hwf : Value.WF (.object [("a", .null), ("a", .bool true)]) := by
    refine .object fun p hp => ?_
    simp only [List.mem_cons, List.not_mem_nil, or_false] at hp
    rcases hp with rfl | rfl
    · exact .null
    · exact .bool true
  have := (h _ "a" (.bool true) hwf).mpr (by simp)
  simp [Value.field, List.lookup] at this
"""),
    # An exception may reuse the law's id: a self-override has no stable labelling.
    ("tree-root-not-fresh", "NornSem/Law.lean",
     "  rootFresh : T.root ∉ T.ids\n", "",
     """open Norn.Sem.Law in
def mutantE : Exception ℕ Unit := ⟨0, 0, fun _ => true⟩
open Norn.Sem.Law in
theorem mutantRefuted : ¬ ∀ T : Tree ℕ Unit, T.WellFormed → ∃ A, T.Stable () A := by
  intro h
  obtain ⟨A, hA⟩ := h ⟨0, [mutantE]⟩ ⟨by decide, by decide, fun e' he' => ⟨0, by
    simp only [List.mem_singleton] at he'
    subst he'
    exact .root⟩⟩
  have key := hA mutantE (List.mem_singleton_self _)
  by_cases ha : A 0
  · exact (key.mp ha).2 mutantE (List.mem_singleton_self _) rfl ha
  · exact ha (key.mpr ⟨rfl, fun c hc _ => by
      rw [List.mem_singleton] at hc
      subst hc
      exact ha⟩)
"""),
    # A matching child no longer lifts its parent.
    ("fold-child-not-negated", "NornSem/Law.lean",
     "&& c.condition x && !lifts T x f c.id", "&& c.condition x && lifts T x f c.id",
     """open Norn.Sem.Law in
def mutantE : Exception ℕ Unit := ⟨1, 0, fun _ => true⟩
open Norn.Sem.Law in
def mutantTree : Tree ℕ Unit := ⟨0, [mutantE]⟩
open Norn.Sem.Law in
theorem mutantRefuted : ¬ ∀ T : Tree ℕ Unit, T.WellFormed → ∀ A, T.Stable () A →
    (T.LiftedBy A ↔ T.lifts () (T.nodes.length + 1) T.root = true) := by
  intro h
  have wf : mutantTree.WellFormed := ⟨by decide, by decide, by decide, fun e' he' => ⟨1, by
    rw [show mutantTree.nodes = [mutantE] from rfl, List.mem_singleton] at he'
    subst he'
    exact .step (List.mem_singleton_self _) .root⟩⟩
  have hst : mutantTree.Stable () (fun i => i = 1) := by
    intro e' he'
    rw [show mutantTree.nodes = [mutantE] from rfl, List.mem_singleton] at he'
    subst he'
    simp [mutantTree, mutantE]
  have := (h mutantTree wf _ hst).mp ⟨mutantE, List.mem_singleton_self _, rfl, rfl⟩
  exact absurd this (by decide)
"""),
    # Lifting no longer releases the law.
    ("binds-ignores-lift", "NornSem/Law.lean",
     "L.active x && !L.tree.lifts x", "L.active x || L.tree.lifts x",
     """open Norn.Sem in
def mutantLaw : Law ℕ Unit := ⟨0, fun _ => false, none, .always, fun _ => true, [⟨1, 0, fun _ => true⟩]⟩
open Norn.Sem in
theorem mutantRefuted : ¬ ∀ (L : Law ℕ Unit) (x : Unit), L.binds x = true → L.bare.binds x = true := by
  intro h
  exact absurd (h mutantLaw () (by decide)) (by decide)
"""),
    # Bounded search would admit.
    ("bounded-search-admits", "NornSem/Obligation.lean",
     "  | .exhaustiveFinite => true\n",
     "  | .exhaustiveFinite => true\n  | .boundedNoCounterexample => true\n",
     """open Norn.Sem.Obligation in
theorem mutantRefuted : ¬ ∀ e : Evidence, e.admits = true ↔ e = .proved ∨ e = .exhaustiveFinite := by
  intro h
  rcases (h .boundedNoCounterexample).mp rfl with h' | h' <;> cases h'
"""),
]
for name, path, old, new, refutation in mutants:
    tree = root / name / "src"
    shutil.copytree(source, tree)
    text = (tree / path).read_text(encoding="utf-8")
    if text.count(old) != 1:
        raise SystemExit(f"{name}: mutation site not found exactly once")
    mutated = text.replace(old, new) + "\n"
    start = mutated.count("\n") + 1
    (tree / path).write_text(mutated + refutation + "\n#print axioms mutantRefuted\n", encoding="utf-8")
    print(name, Path(path).with_suffix("").as_posix(), start)
PYMUTANTS
# check_refuted <log> <file> <start>: the refutation elaborated without sorry
# and no error points at or after it.
check_refuted() {
  python3 - "$@" <<'PYCHECK'
import re
import sys

log, path, start = open(sys.argv[1], encoding="utf-8").read(), sys.argv[2], int(sys.argv[3])
late = [int(m) for m in re.findall(re.escape(path) + r":(\d+):\d+: error", log) if int(m) >= start]
axioms = re.search(r"'mutantRefuted' (?:depends on axioms: \[([^\]]*)\]|does not depend on any axioms)", log)
allowed = {"propext", "Classical.choice", "Quot.sound"}
used = set() if axioms is None or axioms.group(1) is None else {a.strip() for a in axioms.group(1).split(",")}
if late or axioms is None or not used <= allowed:
    raise SystemExit(1)
PYCHECK
}
while read -r name module start; do
  src="$result/mutants/$name/src"
  out="$result/mutants/$name/out"
  mkdir -p "$(dirname "$out/$module.olean")"
  (cd "$lean_root" && LEAN_PATH="$result/baseline" lake env lean -R "$src" \
    "$src/$module.lean") > "$result/mutants/$name.log" 2>&1 || true
  if check_refuted "$result/mutants/$name.log" "$src/$module.lean" "$start"; then
    echo "mutant $name: refuted by a closed counterexample"
  else
    tail -n 30 "$result/mutants/$name.log" >&2
    echo "mutant $name: not refuted" >&2
    exit 1
  fi
done < "$result/mutants.list"
echo "semantics-v1: every seeded definition mutant is refuted"
