/-
Production compare-and-swap is the written `firstConflict` (#718 P8, lane K-A).

`norn_kernel_commit_versions.first_conflict` is Aeneas's translation of
`crates/norn-kernel/src/commit_versions.rs::first_conflict`, extracted from the
production file by `scripts/verify-kernel-commit-aeneas.sh`. The theorem holds
for every host lookup (`CurrentVersion` implementation), every key type and
every pair of read and write slices. The host lookup itself, and which keys
`check_versions` passes, are not proved here.
-/
import Aeneas
import CommitVersions.Funs
import Commit.Reference

open Aeneas Aeneas.Std Result
open norn_kernel_commit_versions

namespace Norn.Kernel.Commit

/-- `pure` in the extracted model is `ok`. -/
@[simp] theorem pure_eq_ok {α : Type} (x : α) : (pure x : Result α) = ok x := rfl

/-- A Rust `Result` as the written semantics' outcome. -/
def toExceptR {α E : Type} : core.result.Result α E → Except E α
  | .Ok a => .ok a
  | .Err e => .error e

/-- One host lookup, seen through the written semantics. -/
def currentRun {K E C : Type} (inst : CurrentVersion C K E) (s : C) (key : K) :
    Result (Except E Nat × C) := do
  let (r, s1) ← inst.current s key
  ok ((toExceptR r).map (fun v => v.val), s1)

/-- The production outcome, seen through the written semantics. -/
def projectCas {K E C : Type}
    (x : Result (core.result.Result (Option (K × Std.U64 × Std.U64)) E × C)) :
    Result (Except E (Option (K × Nat × Nat)) × C) := do
  let (r, s1) ← x
  ok ((toExceptR r).map (Option.map fun (k, e, c) => (k, e.val, c.val)), s1)

/-- Expectations with versions as naturals. -/
def expectations {K : Type} (xs : List (K × Std.U64)) : List (K × Nat) :=
  xs.map fun (k, v) => (k, v.val)

/-- The production loop from any index is the written scan of the rest of the slice. -/
theorem loop_correspondence {K E C : Type} (inst : CurrentVersion C K E)
    (xs : Slice (K × Std.U64)) :
    ∀ (n : Nat) (index : Std.Usize) (versions : C), xs.val.length = index.val + n →
      projectCas (first_conflict_in_loop inst index versions xs) =
        firstConflict (currentRun inst) (expectations (xs.val.drop index.val)) versions := by
  intro n
  induction n with
  | zero =>
    intro index versions h
    have hnone : xs[index]? = none := by
      rw [Slice.getElem?_Usize_eq]; exact List.getElem?_eq_none (by omega)
    have hdrop : xs.val.drop index.val = [] := List.drop_eq_nil_of_le (by omega)
    unfold first_conflict_in_loop
    rw [loop.eq_1]
    dsimp only
    rw [first_conflict_in_loop.body]
    simp only [core.slice.Slice.get, core.slice.index.Usize.get, hnone, bind_tc_ok, hdrop,
      expectations, List.map_nil]
    rfl
  | succ n ih =>
    intro index versions h
    have hlt : index.val < xs.val.length := by omega
    have hdrop := List.drop_eq_getElem_cons hlt
    have hget : xs[index]? = some (xs.val[index.val]) := by
      rw [Slice.getElem?_Usize_eq]; exact List.getElem?_eq_getElem hlt
    generalize xs.val[index.val] = p at hdrop hget
    obtain ⟨key, expected⟩ := p
    rw [hdrop]
    generalize hrest : List.drop (index.val + 1) xs.val = rest
    unfold first_conflict_in_loop
    rw [loop.eq_1]
    dsimp only
    rw [first_conflict_in_loop.body]
    simp only [core.slice.Slice.get, core.slice.index.Usize.get, hget, bind_tc_ok,
      expectations, List.map_cons]
    rw [firstConflict]
    have hmax := xs.property
    rcases hc : inst.current versions key with ⟨r, v1⟩ | e | _
    · rcases r with c | e
      · by_cases heq : c = expected
        · subst heq
          have hadd := Usize.checked_add_bv_spec index 1#usize
          rcases hca : index.checked_add 1#usize with _ | next
          · rw [hca] at hadd; simp at hadd; omega
          · rw [hca] at hadd
            obtain ⟨-, hnext, -⟩ := hadd
            have hn : next.val = index.val + 1 := by simpa using hnext
            have := ih next v1 (by omega)
            rw [hn, hrest] at this
            simp [hc, currentRun, toExceptR, core.result.Result.Insts.CoreOpsTry.branch,
              Except.map]
            exact this
        · have hv : ¬ (c.val = expected.val) := fun hv => heq ((UScalar.eq_equiv c expected).mpr hv)
          simp [hc, hv, currentRun, toExceptR, projectCas, core.result.Result.Insts.CoreOpsTry.branch, Except.map]
      · simp [hc, currentRun, toExceptR, projectCas, core.result.Result.Insts.CoreOpsTry.branch,
          core.result.Result.Insts.CoreOpsTryTraitFromResidualResultInfallible.from_residual,
          core.convert.FromSame.from, Except.map]
    · simp [hc, currentRun, projectCas]
    · simp [hc, currentRun, projectCas]

/-- One scan from the start of a slice is the written semantics. -/
theorem first_conflict_in_correspondence {K E C : Type} (inst : CurrentVersion C K E)
    (xs : Slice (K × Std.U64)) (versions : C) :
    projectCas (first_conflict_in_loop inst 0#usize versions xs) =
      firstConflict (currentRun inst) (expectations xs.val) versions := by
  simpa using loop_correspondence inst xs xs.val.length 0#usize versions (by simp)

/-- Scanning `xs ++ ys` is scanning `xs`, then `ys` only if `xs` had no
conflict and no host error. -/
theorem firstConflict_append {K E C : Type} (current : C → K → Result (Except E Nat × C)) :
    ∀ (xs ys : List (K × Nat)) (s : C),
      firstConflict current (xs ++ ys) s = (do
        let (r, s1) ← firstConflict current xs s
        match r with
        | .ok none => firstConflict current ys s1
        | r => ok (r, s1))
  | [], ys, s => by simp [firstConflict]
  | (key, expected) :: xs, ys, s => by
    simp only [List.cons_append, firstConflict]
    rcases h : current s key with ⟨r, s1⟩ | e | _
    · rcases r with e | found
      · simp
      · by_cases hf : found = expected
        · simp [hf, firstConflict_append current xs ys s1]
        · simp [hf]
    · simp
    · simp

/-- **K-A unit 2.** For every host lookup and every commit, production
compare-and-swap reads the current version of each read expectation, then
each write expectation, in order, and returns the first that differs (key,
expected, current), the first host error, or no conflict; nothing after the
first conflict or error is read. -/
theorem first_conflict_correspondence {K E C : Type} (copy : core.marker.Copy K)
    (inst : CurrentVersion C K E) (versions : C) (reads writes : Slice (K × Std.U64)) :
    projectCas (first_conflict copy inst versions reads writes) =
      firstConflict (currentRun inst) (expectations reads.val ++ expectations writes.val)
        versions := by
  rw [firstConflict_append, ← first_conflict_in_correspondence inst reads versions]
  unfold first_conflict first_conflict_in
  rcases h : first_conflict_in_loop inst 0#usize versions reads with ⟨r, s1⟩ | e | _
  · rcases r with (_ | conflict) | e
    · simp [projectCas, toExceptR, core.result.Result.Insts.CoreOpsTry.branch, Except.map]
      exact first_conflict_in_correspondence inst writes s1
    · simp [projectCas, toExceptR, core.result.Result.Insts.CoreOpsTry.branch, Except.map]
    · simp [projectCas, toExceptR, core.result.Result.Insts.CoreOpsTry.branch, Except.map,
        core.result.Result.Insts.CoreOpsTryTraitFromResidualResultInfallible.from_residual,
        core.convert.FromSame.from]
  · simp [projectCas]
  · simp [projectCas]

#print axioms first_conflict_correspondence

end Norn.Kernel.Commit
