import Std

namespace Norn.Verification.QuantityUnitDimension

/-!
Small mathematical reference for exact unit/dimension binding. It has no
axioms, `sorry`, or `admit`. It does not prove correspondence to Rust,
ontology-artifact admission, SHA-256 collision resistance, or Turso durability.
-/

structure UnitBinding where
  unitTerm : Nat
  quantityKindTerm : Nat
  deriving DecidableEq

def maySubstitute (sealed proposed : UnitBinding) : Bool :=
  sealed == proposed

def mayBalanceTogether (left right : UnitBinding) : Bool :=
  left.unitTerm == right.unitTerm && left.quantityKindTerm == right.quantityKindTerm

theorem different_dimensions_cannot_balance
    {left right : UnitBinding}
    (different : left.quantityKindTerm ≠ right.quantityKindTerm) :
    mayBalanceTogether left right = false := by
  simp [mayBalanceTogether, different]

theorem different_units_cannot_balance_without_explicit_identity
    {left right : UnitBinding}
    (different : left.unitTerm ≠ right.unitTerm) :
    mayBalanceTogether left right = false := by
  simp [mayBalanceTogether, different]

theorem admitted_substitution_is_exact
    {sealed proposed : UnitBinding}
    (accepted : maySubstitute sealed proposed = true) :
    proposed = sealed := by
  have exact : sealed = proposed := by
    simpa [maySubstitute] using accepted
  exact exact.symm

theorem changing_dimension_rejects_substitution
    {sealed proposed : UnitBinding}
    (changed : sealed.quantityKindTerm ≠ proposed.quantityKindTerm) :
    maySubstitute sealed proposed = false := by
  cases accepted : maySubstitute sealed proposed with
  | false => rfl
  | true =>
      have exact : proposed = sealed := admitted_substitution_is_exact accepted
      exact (changed (congrArg UnitBinding.quantityKindTerm exact.symm)).elim

end Norn.Verification.QuantityUnitDimension
