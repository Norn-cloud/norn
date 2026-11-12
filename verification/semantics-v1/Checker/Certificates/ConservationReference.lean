import Mathlib.Algebra.BigOperators.Group.Finset.Basic
import Mathlib.Algebra.BigOperators.Ring.Finset
import Mathlib.Algebra.BigOperators.Group.Finset.Sigma
import Mathlib.Data.Rat.Defs
import Mathlib.Tactic.Ring

/-!
# Linear conservation certificates: written semantics

`crates/norn-checker/src/obligations/conservation.rs` (CONS-LINEAR-1) checks a
declared conserved functional `w` against every writer's affine delta. The
`n` coordinates are partitioned into scope/unit classes (`part`); a writer's
delta is a matrix `rows i c` (coordinate `i`, parameter column `c`, the last
column being the affine constant). The certificate is accepted only when, in
every class and every column, the weighted coefficients sum to exactly zero.

This file states that condition (`Balanced`) and proves the linear
conservation meta-theorem (#718 meta-theorem 4): an accepted writer preserves
the weighted class sum of every state for every parameter valuation
(`step_conserves`), and so does every finite run of accepted writers
(`run_conserves`). It never imports the extraction.
-/

namespace Norn.Checker.Certificates.Conservation
open Finset

/-- The weighted sum of `f` over the coordinates of class `l`. -/
def classSum (n : Nat) (part : Nat → Nat) (w : Nat → ℚ) (l : Nat) (f : Nat → ℚ) : ℚ :=
  ∑ i ∈ range n, if part i = l then w i * f i else 0

/-- The certificate condition: every class's weighted column sums are zero. -/
def Balanced (n columns : Nat) (part : Nat → Nat) (w : Nat → ℚ) (rows : Nat → Nat → ℚ) : Prop :=
  ∀ l c, c < columns → classSum n part w l (fun i => rows i c) = 0

/-- One writer step: coordinate `i` changes by `∑ c, rows i c * x c`, for a
    valuation `x` of the parameter columns (the constant column included). -/
def step (columns : Nat) (rows : Nat → Nat → ℚ) (x : Nat → ℚ) (σ : Nat → ℚ) : Nat → ℚ :=
  fun i => σ i + ∑ c ∈ range columns, rows i c * x c

/-- Linear conservation: an accepted writer preserves every class's weighted
    sum, for every state and every parameter valuation. -/
theorem step_conserves {n columns : Nat} {part : Nat → Nat} {w : Nat → ℚ}
    {rows : Nat → Nat → ℚ} (hb : Balanced n columns part w rows) (x σ : Nat → ℚ) (l : Nat) :
    classSum n part w l (step columns rows x σ) = classSum n part w l σ := by
  have key : (∑ i ∈ range n, if part i = l then w i * ∑ c ∈ range columns, rows i c * x c else 0)
      = 0 := by
    have : ∀ i ∈ range n, (if part i = l then w i * ∑ c ∈ range columns, rows i c * x c else 0)
        = ∑ c ∈ range columns, x c * (if part i = l then w i * rows i c else 0) := by
      intro i _
      split
      · rw [mul_sum]; apply sum_congr rfl; intro c _; ring
      · simp
    rw [sum_congr rfl this, Finset.sum_comm]
    apply sum_eq_zero
    intro c hc
    rw [← mul_sum]
    have := hb l c (mem_range.mp hc)
    unfold classSum at this
    rw [this, mul_zero]
  unfold classSum step
  rw [← add_zero (∑ i ∈ range n, if part i = l then w i * σ i else 0), ← key, ← sum_add_distrib]
  apply sum_congr rfl
  intro i _
  split <;> ring

/-- A writer of the run: its delta matrix and column count. -/
structure Writer where
  columns : Nat
  rows : Nat → Nat → ℚ

/-- Apply a finite run of writer steps, each with its own valuation. -/
def run : List (Writer × (Nat → ℚ)) → (Nat → ℚ) → (Nat → ℚ)
  | [], σ => σ
  | (wr, x) :: rest, σ => run rest (step wr.columns wr.rows x σ)

/-- Every finite run of accepted writers preserves every class's weighted sum. -/
theorem run_conserves {n : Nat} {part : Nat → Nat} {w : Nat → ℚ} :
    ∀ (steps : List (Writer × (Nat → ℚ))),
      (∀ s ∈ steps, Balanced n s.1.columns part w s.1.rows) →
      ∀ σ l, classSum n part w l (run steps σ) = classSum n part w l σ
  | [], _, _, _ => rfl
  | (wr, x) :: rest, h, σ, l => by
    simp only [run]
    rw [run_conserves rest (fun s hs => h s (List.mem_cons_of_mem _ hs)),
      step_conserves (h (wr, x) List.mem_cons_self) x σ l]

end Norn.Checker.Certificates.Conservation

#print axioms Norn.Checker.Certificates.Conservation.step_conserves
#print axioms Norn.Checker.Certificates.Conservation.run_conserves
