import Checker.NormPrecedence.Resolution
import NornSem.Meta.Precedence

/-!
# Norm precedence: the production decision core is sound (U3, U4; #718 lane C-C)

`NormDecision` is the fresh Aeneas translation of the production file
`crates/norn-checker/src/norm_precedence/decision.rs`. The theorems quantify over
every input the core can receive:

* `admits_sound`: `admits` never panics or diverges, and when it returns `true`
  the declared order is `Admissible` in `Norn.Sem.Norm` and the closure
  certificate it checked is exactly the declared-priority closure `Prec`.
* `resolution_sound`: after an admission, `resolution_holds` never panics or
  diverges, and when it returns `true` the untrusted resolution of the context
  is the semantics: its survivors are the unique stable set (the undefeated
  active norms), no two survivors oppose, no conflict is recorded, its
  obligations are exactly the surviving obligations, every defeat cites a
  survivor and a declared defeat, and every active non-survivor is recorded as
  defeated. Survivors, defeats and conflicts never mean permission: a context
  permits only through a surviving permission (`Permits`), and an uncovered
  context permits nothing.
* `admitted_meta`: an admitted order satisfies the U4 meta-theorems
  (downstream can't defeat upstream, strict-layer preservation, overlays
  preserve upstream survivors).
-/

namespace Norn.Checker.NormPrecedence
open Aeneas Aeneas.Std Result norn_checker_norm_decision Norn.Sem.Norm

/-- The listed survivors, as a set of norm indices. -/
def survivorSet (survivors : Slice Usize) : Nat → Prop := Mem survivors

theorem defeatedBy_iff {norms : Norms} {edges : Edges} {reach : Reach} {contexts : Usize}
    (hC : Checks norms edges reach contexts) (survivors : Slice Usize) (i : Nat) :
    DefeatedBy norms reach survivors i ↔
      ∃ j, survivorSet survivors j ∧ Defeats (catalog norms edges contexts) j i := by
  constructor
  · rintro ⟨y, hy, hr, hop⟩
    exact ⟨y.val, ⟨y, hy, rfl⟩, (R_iff_prec hC _ _).mp hr, hop⟩
  · rintro ⟨j, ⟨y, hy, rfl⟩, hp, hop⟩
    exact ⟨y, hy, (R_iff_prec hC _ _).mpr hp, hop⟩

/-- **Resolution soundness (U3).** -/
theorem resolution_sound (norms : Norms) (edges : Edges) (reach : Reach) (contexts : Usize)
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
        (∀ i, i < C.size → C.active i c.val = true → S i ∨ ∃ d ∈ defeats.val, d.2.val = i)) := by
  obtain ⟨b₀, hrun₀, hb₀⟩ := admits_decides norms edges reach contexts
  rw [hadm] at hrun₀
  cases hrun₀
  have hC := hb₀.mp rfl
  have hA := checks_admissible hC
  obtain ⟨b, hrun, hb⟩ :=
    resolution_holds_decides norms edges reach contexts hC.shaped c survivors obligations conflicts defeats
  refine ⟨b, hrun, fun h => ?_⟩
  have hR := hb.mp h
  have hstable : Stable (catalog norms edges contexts) c.val (survivorSet survivors) := by
    intro i
    by_cases hi : i < norms.length
    · have hse : Mem survivors i ↔ _ := hR.stable i hi
      rw [show survivorSet survivors i = Mem survivors i from rfl, hse, defeatedBy_iff hC]
      simp only [catalog]
      exact ⟨fun ⟨ha, hd⟩ => ⟨hi, ha, hd⟩, fun ⟨_, ha, hd⟩ => ⟨ha, hd⟩⟩
    · constructor
      · rintro ⟨y, hy, rfl⟩; exact absurd (hR.survivors_in y hy) hi
      · rintro ⟨h', -⟩; exact absurd h' hi
  refine ⟨hR.no_conflicts, hstable, fun S' hS' i => stable_unique hA hS' hstable i, ?_, ?_, ?_, ?_⟩
  · intro i j hi hj
    obtain ⟨hin, -⟩ := hstable.survivor hi
    have hc : c.val < contexts.val := by
      have := hR.context i hin; rwa [hC.shaped.masks i hin] at this
    exact stable_no_conflict hA hc hstable hi hj
  · intro i
    constructor
    · rintro ⟨y, hy, rfl⟩; exact ⟨(hR.obligations_survive y hy).2, (hR.obligations_survive y hy).1⟩
    · rintro ⟨⟨y, hy, rfl⟩, hm⟩; exact hR.obligations_listed y hy hm
  · intro d hd
    obtain ⟨-, -, hw, ha, hr, hop⟩ := hR.cited d hd
    exact ⟨hw, ha, (R_iff_prec hC _ _).mp hr, hop⟩
  · intro i hi ha; exact hR.complete i hi ha

/-- **The U4 meta-theorems hold of every admitted order.** -/
theorem admitted_meta (norms : Norms) (edges : Edges) (reach : Reach) (contexts : Usize)
    (hadm : decision.admits norms edges reach contexts = ok true) :
    let C := catalog norms edges contexts
    (∀ w l, Prec C w l → C.layer w ≤ C.layer l ∧ C.isProtected l = false) ∧
    (∀ c S, c < C.contexts → Stable C c S → ∀ q, S q → C.modality q = .permission →
      ∀ p, p < C.size → C.active p c = true → C.modality p = .prohibition →
        C.isProtected p = false ∧ C.layer q ≤ C.layer p) ∧
    (∀ L c S S', Stable C c S → Stable (C.upTo L) c S' → ∀ i, C.layer i ≤ L → (S i ↔ S' i)) := by
  obtain ⟨b, hrun, hb⟩ := admits_sound norms edges reach contexts
  rw [hadm] at hrun
  cases hrun
  have hA := (hb rfl).1
  exact ⟨fun _ _ h => downstream_cannot_defeat_upstream hA h,
    fun _ _ hc hS _ hq hperm _ hp ha hproh =>
      permission_respects_invariants hA hc hS hq hperm hp ha hproh,
    fun _ _ _ _ hS hS' => overlay_preserves_upstream hA hS hS'⟩

end Norn.Checker.NormPrecedence
#print axioms Norn.Checker.NormPrecedence.admits_sound
#print axioms Norn.Checker.NormPrecedence.resolution_sound
#print axioms Norn.Checker.NormPrecedence.admitted_meta
#print axioms Norn.Sem.Norm.stable_unique
#print axioms Norn.Sem.Norm.stable_no_conflict
#print axioms Norn.Sem.Norm.uncovered_not_permits
#print axioms Norn.Sem.Norm.downstream_cannot_defeat_upstream
#print axioms Norn.Sem.Norm.defeat_same_layer
#print axioms Norn.Sem.Norm.protected_survives
#print axioms Norn.Sem.Norm.permission_respects_invariants
#print axioms Norn.Sem.Norm.overlay_preserves_upstream
