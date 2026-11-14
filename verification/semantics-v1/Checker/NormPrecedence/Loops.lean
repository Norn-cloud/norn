import Aeneas

/-!
# Counting loops of the extracted decision core

Every loop in `norm_precedence/decision.rs` walks an index `i` up to a bound
`n`, stopping early with a fixed answer. These two lemmas give the answer of
such a loop from the behaviour of one step: `loop_all` for a loop that answers
`false` at the first index failing `P`, `loop_any` for one that answers `true`
at the first index satisfying `P`. `Decides r P` states that `r` succeeds with
a Boolean that is `true` exactly when `P` holds.
-/

namespace Norn.Checker.NormPrecedence
open Aeneas Aeneas.Std Result ControlFlow

/-- `r` returns, without panic or divergence, a Boolean that is `true`
    exactly when `P` holds. -/
def Decides (r : Result Bool) (P : Prop) : Prop :=
  ∃ b, r = ok b ∧ (b = true ↔ P)

@[simp] theorem Decides.ok_iff {b : Bool} {P : Prop} : Decides (ok b) P ↔ (b = true ↔ P) := by
  constructor
  · rintro ⟨b', h, hb⟩; cases h; exact hb
  · intro h; exact ⟨b, rfl, h⟩

theorem Decides.congr {r : Result Bool} {P Q : Prop} (h : Decides r P) (hpq : P ↔ Q) :
    Decides r Q := by
  obtain ⟨b, hr, hb⟩ := h; exact ⟨b, hr, hb.trans hpq⟩

/-- A short-circuit conjunction `if b then r₂ else false`. -/
theorem Decides.and {r₁ : Result Bool} {P₁ : Prop} {r₂ : Result Bool} {P₂ : Prop}
    (h₁ : Decides r₁ P₁) (h₂ : P₁ → Decides r₂ P₂) :
    Decides (do let b ← r₁; if b then r₂ else ok false) (P₁ ∧ P₂) := by
  obtain ⟨b, hr, hb⟩ := h₁
  subst hr
  cases b
  · exact ⟨false, by simp, by simp at hb ⊢; intro h; exact absurd h hb⟩
  · obtain ⟨c, hc, hcp⟩ := h₂ (hb.mp rfl)
    exact ⟨c, by simp [hc], by rw [hcp]; exact ⟨fun h => ⟨hb.mp rfl, h⟩, fun h => h.2⟩⟩

/-- The successor of an index below a `Usize` bound is a `Usize`. -/
theorem succ_ok (i : Usize) (n : Nat) (hn : n ≤ Usize.max) (hi : i.val < n) :
    ∃ j : Usize, i + 1#usize = ok j ∧ j.val = i.val + 1 := by
  have h := @UScalar.add_spec _ i 1#usize (by simp [UScalar.max_USize_eq]; omega)
  obtain ⟨j, hj, hjv⟩ := WP.spec_imp_exists h
  exact ⟨j, hj, by simpa using hjv⟩

/-- A loop that answers `false` at the first index below `n` failing `P`,
    and `true` once the index reaches `n`. -/
theorem loop_all {body : Usize → Result (ControlFlow Usize Bool)} {n : Nat} {P : Nat → Prop}
    (hstep : ∀ i : Usize, i.val < n → ∃ j : Usize, j.val = i.val + 1 ∧
      ((P i.val ∧ body i = ok (cont j)) ∨ (¬ P i.val ∧ body i = ok (done false))))
    (hstop : ∀ i : Usize, n ≤ i.val → body i = ok (done true)) :
    ∀ i : Usize, Decides (loop body i) (∀ k, i.val ≤ k → k < n → P k) := by
  suffices h : ∀ m (i : Usize), n - i.val = m →
      Decides (loop body i) (∀ k, i.val ≤ k → k < n → P k) from fun i => h _ i rfl
  intro m
  induction m with
  | zero =>
    intro i hm
    rw [loop.eq_1, hstop i (by omega)]
    exact ⟨true, rfl, by simp; intro k hk hkn; omega⟩
  | succ m ih =>
    intro i hm
    obtain ⟨j, hj, hcase | hcase⟩ := hstep i (by omega)
    · rw [loop.eq_1, hcase.2]
      refine (ih j (by omega)).congr ?_
      constructor
      · intro h k hk hkn
        rcases Nat.eq_or_lt_of_le hk with h' | h'
        · subst h'; exact hcase.1
        · exact h k (by omega) hkn
      · intro h k hk hkn; exact h k (by omega) hkn
    · rw [loop.eq_1, hcase.2]
      exact ⟨false, rfl, by simp; exact ⟨i.val, le_refl _, by omega, hcase.1⟩⟩

/-- A loop that answers `true` at the first index below `n` satisfying `P`,
    and `false` once the index reaches `n`. -/
theorem loop_any {body : Usize → Result (ControlFlow Usize Bool)} {n : Nat} {P : Nat → Prop}
    (hstep : ∀ i : Usize, i.val < n → ∃ j : Usize, j.val = i.val + 1 ∧
      ((P i.val ∧ body i = ok (done true)) ∨ (¬ P i.val ∧ body i = ok (cont j))))
    (hstop : ∀ i : Usize, n ≤ i.val → body i = ok (done false)) :
    ∀ i : Usize, Decides (loop body i) (∃ k, i.val ≤ k ∧ k < n ∧ P k) := by
  suffices h : ∀ m (i : Usize), n - i.val = m →
      Decides (loop body i) (∃ k, i.val ≤ k ∧ k < n ∧ P k) from fun i => h _ i rfl
  intro m
  induction m with
  | zero =>
    intro i hm
    rw [loop.eq_1, hstop i (by omega)]
    exact ⟨false, rfl, by simp; intro k hk hkn; omega⟩
  | succ m ih =>
    intro i hm
    obtain ⟨j, hj, hcase | hcase⟩ := hstep i (by omega)
    · rw [loop.eq_1, hcase.2]
      exact ⟨true, rfl, by simp; exact ⟨i.val, le_refl _, by omega, hcase.1⟩⟩
    · rw [loop.eq_1, hcase.2]
      refine (ih j (by omega)).congr ?_
      constructor
      · rintro ⟨k, hk, hkn, hp⟩; exact ⟨k, by omega, hkn, hp⟩
      · rintro ⟨k, hk, hkn, hp⟩
        rcases Nat.eq_or_lt_of_le hk with h' | h'
        · subst h'; exact absurd hp hcase.1
        · exact ⟨k, by omega, hkn, hp⟩

end Norn.Checker.NormPrecedence
