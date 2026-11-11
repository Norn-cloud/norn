import Std

/-!
CONS-LINEAR-1 meta-theorem for a rational conserved functional.
The faithful-step premise binds the weighted delta to the measure's change.
This does not prove declaration lowering, Rust correspondence, initial-state
validity or non-vacuity. No application-specific statement or axioms are used.
-/
namespace Norn.Verification.LinearConservation

inductive Reach {State : Type} (step : State → State → Prop) : State → State → Prop
  | initial (s) : Reach step s s
  | advance {s t u} : Reach step s t → step t u → Reach step s u

-- The source adapter supplies measure = wᵀs and change = wᵀΔ.
-- This engine theorem does not establish that adapter's correspondence.
theorem per_writer_balance_implies_invariant
    {State : Type} (step : State → State → Prop)
    (measure : State → Rat) (change : State → State → Rat)
    (faithful : ∀ {s t}, step s t → measure t = measure s + change s t)
    (balanced : ∀ {s t}, step s t → change s t = 0)
    {initial final : State} (reachable : Reach step initial final) :
    measure final = measure initial := by
  induction reachable with
  | initial => rfl
  | advance path edge ih =>
      calc
        measure _ = measure _ + change _ _ := faithful edge
        _ = measure _ := by rw [balanced edge, Rat.add_zero]
        _ = measure _ := ih

end Norn.Verification.LinearConservation
