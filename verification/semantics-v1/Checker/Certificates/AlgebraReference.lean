import Mathlib.Algebra.Group.Defs
import Mathlib.Tactic.Abel
import Mathlib.Tactic.Ring
import Mathlib.Tactic.Linarith

/-!
# DBSP operator algebra: written semantics

`crates/norn-checker/src/metric/compute/certificate.rs` checks a dependency-ordered
composition of operators over signed multisets (Z-sets) and marks each node
`ProvedAlgebraic` when its incremental (delta) form is algebraically exact.
This file states, independently of the extraction:

* the operators and the checker's per-node verdict (`verdictAt`);
* full evaluation (`evalAt`) and incremental maintenance (`deltaAt`) in any
  additive commutative monoid, under an interpretation of the closed laws, the
  bilinear join and the join coefficients;
* the IVM correctness meta-theorem (`incremental_sound`): at every proved node,
  evaluating on updated inputs equals the old value plus the maintained delta;
* that a refuted join coefficient is a genuine counterexample (`join_refuted_witness`).
-/

namespace Norn.Checker.Certificates.Algebra

/-- The closed homomorphism laws. -/
inductive Law where
  | select | project | groupSum | monoidSum
  deriving DecidableEq

/-- One dependency-ordered operator. -/
inductive Op where
  | input
  | hom (dep : Nat) (law : Option Law)
  | join (left right : Nat) (c0 c1 c2 : Int)
  | unknown

/-- The checker's verdict for one node. -/
inductive Verdict where
  | proved
  | refuted (term : Nat)
  | overBudget
  | undeclaredFold
  | monoidUnderDeletions
  | unsupported
  | invalidDependency
  deriving DecidableEq

/-- The verdict for a homomorphism whose input is proved. -/
def homVerdict (law : Option Law) (signed : Bool) : Verdict :=
  match law with
  | none => .undeclaredFold
  | some .monoidSum => if signed then .monoidUnderDeletions else .proved
  | some _ => .proved

/-- The verdict for a join whose inputs are proved: the first coefficient that is not 1. -/
def joinVerdict (c0 c1 c2 : Int) : Verdict :=
  if c0 ≠ 1 then .refuted 0 else if c1 ≠ 1 then .refuted 1 else if c2 ≠ 1 then .refuted 2
  else .proved

/-- The verdict at node `i`: a node is proved only if every node it reads is
    strictly earlier and itself proved. -/
def verdictAt (ops : List Op) (signed : Bool) (i : Nat) : Verdict :=
  match ops[i]? with
  | none => .unsupported
  | some .input => .proved
  | some (.hom j law) =>
    if _hj : j < i then
      if verdictAt ops signed j = .proved then homVerdict law signed else .invalidDependency
    else .invalidDependency
  | some (.join l r c0 c1 c2) =>
    if _hl : l < i then
      if verdictAt ops signed l = .proved then
        if _hr : r < i then
          if verdictAt ops signed r = .proved then joinVerdict c0 c1 c2 else .invalidDependency
        else .invalidDependency
      else .invalidDependency
    else .invalidDependency
  | some .unknown => .unsupported
termination_by i

/-- The whole composition's verdicts: more than 256 operators are all over budget. -/
def verdicts (ops : List Op) (signed : Bool) : List Verdict :=
  if ops.length > 256 then List.replicate ops.length .overBudget
  else (List.range ops.length).map (verdictAt ops signed)

variable {V : Type} [AddCommMonoid V]

/-- An interpretation: the closed laws, the bilinear join, and the action of a
    join coefficient on a value. -/
structure Interp (V : Type) where
  law : Law → V → V
  join : V → V → V
  coef : Int → V → V

/-- Full evaluation of node `i` on inputs `inp` (input node `i` reads `inp i`). -/
def evalAt (I : Interp V) (ops : List Op) (inp : Nat → V) (i : Nat) : V :=
  match ops[i]? with
  | some .input => inp i
  | some (.hom j (some law)) => if _hj : j < i then I.law law (evalAt I ops inp j) else 0
  | some (.join l r _ _ _) =>
    if _hl : l < i then if _hr : r < i then I.join (evalAt I ops inp l) (evalAt I ops inp r)
      else 0 else 0
  | _ => 0
termination_by i

/-- Incremental maintenance of node `i`: the change of each input node is
    `dinp`; a homomorphism maps its input's change; a join combines the old
    snapshots `evalAt … inp` with the changes, weighted by its coefficients. -/
def deltaAt (I : Interp V) (ops : List Op) (inp dinp : Nat → V) (i : Nat) : V :=
  match ops[i]? with
  | some .input => dinp i
  | some (.hom j (some law)) => if _hj : j < i then I.law law (deltaAt I ops inp dinp j) else 0
  | some (.join l r c0 c1 c2) =>
    if _hl : l < i then if _hr : r < i then
      I.coef c0 (I.join (deltaAt I ops inp dinp l) (evalAt I ops inp r)) +
      I.coef c1 (I.join (evalAt I ops inp l) (deltaAt I ops inp dinp r)) +
      I.coef c2 (I.join (deltaAt I ops inp dinp l) (deltaAt I ops inp dinp r))
      else 0 else 0
  | _ => 0
termination_by i

/-- The laws the interpretation must satisfy: every law the checker may prove
    is additive (the monoid sum only when changes are insert-only), the join
    is additive in each argument, and coefficient 1 is the identity. -/
structure Interp.Lawful (I : Interp V) (signed : Bool) : Prop where
  law_add : ∀ l, (signed = true → l ≠ .monoidSum) → ∀ a b, I.law l (a + b) = I.law l a + I.law l b
  join_add_left : ∀ a b c, I.join (a + b) c = I.join a c + I.join b c
  join_add_right : ∀ a b c, I.join a (b + c) = I.join a b + I.join a c
  coef_one : ∀ v, I.coef 1 v = v

theorem homVerdict_proved {law : Option Law} {signed : Bool} (h : homVerdict law signed = .proved) :
    ∃ l, law = some l ∧ (signed = true → l ≠ .monoidSum) := by
  cases law with
  | none => simp [homVerdict] at h
  | some l =>
    refine ⟨l, rfl, fun hs hl => ?_⟩
    subst hs hl
    simp [homVerdict] at h

theorem joinVerdict_proved {c0 c1 c2 : Int} (h : joinVerdict c0 c1 c2 = .proved) :
    c0 = 1 ∧ c1 = 1 ∧ c2 = 1 := by
  unfold joinVerdict at h
  by_cases h0 : c0 = 1 <;> by_cases h1 : c1 = 1 <;> by_cases h2 : c2 = 1 <;> simp_all

/-- IVM correctness (#718 meta-theorem 5): at every node the checker proves,
    evaluating on updated inputs equals the old value plus the maintained delta,
    for every input and every change. -/
theorem incremental_sound (I : Interp V) {ops : List Op} {signed : Bool} (hI : I.Lawful signed)
    (inp dinp : Nat → V) :
    ∀ i, verdictAt ops signed i = .proved →
      evalAt I ops (fun k => inp k + dinp k) i = evalAt I ops inp i + deltaAt I ops inp dinp i := by
  intro i
  induction i using Nat.strong_induction_on with
  | _ i ih =>
    intro hv
    rw [verdictAt] at hv
    rw [evalAt, evalAt, deltaAt]
    cases hop : ops[i]? with
    | none => simp [hop] at hv
    | some op =>
      rw [hop] at hv
      cases op with
      | input => rfl
      | unknown => simp at hv
      | hom j law =>
        simp only at hv ⊢
        by_cases hj : j < i
        · rw [dif_pos hj] at hv
          by_cases hpj : verdictAt ops signed j = .proved
          · rw [if_pos hpj] at hv
            obtain ⟨l, rfl, hl⟩ := homVerdict_proved hv
            simp only [dif_pos hj]
            rw [ih j hj hpj, hI.law_add l hl]
          · rw [if_neg hpj] at hv; cases hv
        · rw [dif_neg hj] at hv; cases hv
      | join l r c0 c1 c2 =>
        simp only at hv ⊢
        by_cases hl : l < i
        · rw [dif_pos hl] at hv
          by_cases hpl : verdictAt ops signed l = .proved
          · rw [if_pos hpl] at hv
            by_cases hr : r < i
            · rw [dif_pos hr] at hv
              by_cases hpr : verdictAt ops signed r = .proved
              · rw [if_pos hpr] at hv
                obtain ⟨rfl, rfl, rfl⟩ := joinVerdict_proved hv
                simp only [dif_pos hl, dif_pos hr, hI.coef_one]
                rw [ih l hl hpl, ih r hr hpr, hI.join_add_left, hI.join_add_right,
                  hI.join_add_right]
                abel
              · rw [if_neg hpr] at hv; cases hv
            · rw [dif_neg hr] at hv; cases hv
          · rw [if_neg hpl] at hv; cases hv
        · rw [dif_neg hl] at hv; cases hv

/-- A refuted join coefficient is a real error: over the integers, with the
    join as multiplication, some old values and changes make the declared delta
    differ from the true change. -/
theorem join_refuted_witness {c0 c1 c2 : Int} (h : ¬ (c0 = 1 ∧ c1 = 1 ∧ c2 = 1)) :
    ∃ L R dL dR : Int,
      (L + dL) * (R + dR) - L * R ≠ c0 * (dL * R) + c1 * (L * dR) + c2 * (dL * dR) := by
  by_cases h0 : c0 = 1
  · by_cases h1 : c1 = 1
    · have h2 : c2 ≠ 1 := fun h2 => h ⟨h0, h1, h2⟩
      refine ⟨0, 0, 1, 1, ?_⟩
      subst h0 h1
      intro e; apply h2; linarith
    · refine ⟨1, 0, 0, 1, ?_⟩
      intro e; apply h1; linarith
  · refine ⟨0, 1, 1, 0, ?_⟩
    intro e; apply h0; linarith

end Norn.Checker.Certificates.Algebra

#print axioms Norn.Checker.Certificates.Algebra.incremental_sound
#print axioms Norn.Checker.Certificates.Algebra.join_refuted_witness
