import Std

/-!
Reference rule specialisation-conjunction/1. A specialisation may restrict steps
by conjunction; it cannot add projected writers. This proves universal safety,
not progress, non-vacuity, declaration lowering or Rust correspondence.
-/
namespace Norn.Verification.Specialisation

inductive Reach {State : Type} (initial : State → Prop)
    (step : State → State → Prop) : State → Prop
  | start {s} : initial s → Reach initial step s
  | advance {s t} : Reach initial step s → step s t → Reach initial step t

-- Projection names exactly the base-owned relations. The no-new-writer premise
-- is explicit, rather than inferred from the extension's declared dependencies.
theorem projected_safety
    {Base Extension : Type} (project : Extension → Base)
    (baseInitial : Base → Prop) (baseStep : Base → Base → Prop)
    (extensionInitial : Extension → Prop) (extensionStep : Extension → Extension → Prop)
    (invariant : Base → Prop)
    (initialSubset : ∀ s, extensionInitial s → baseInitial (project s))
    (noNewWriter : ∀ s t, extensionStep s t → baseStep (project s) (project t))
    (baseInit : ∀ s, baseInitial s → invariant s)
    (basePreserves : ∀ s t, invariant s → baseStep s t → invariant t)
    {s : Extension} (reachable : Reach extensionInitial extensionStep s) :
    invariant (project s) := by
  induction reachable with
  | start h => exact baseInit _ (initialSubset _ h)
  | advance _ edge ih => exact basePreserves _ _ ih (noNewWriter _ _ edge)

-- Adding only conjuncts preserves safety; repeating this construction gives
-- composition of restrictions. There is deliberately no liveness conclusion.
theorem conjunction_preserves_safety
    {State : Type} (initial : State → Prop) (step : State → State → Prop)
    (extraInitial : State → Prop) (extraStep : State → State → Prop)
    (invariant : State → Prop)
    (baseInit : ∀ s, initial s → invariant s)
    (basePreserves : ∀ s t, invariant s → step s t → invariant t)
    {s : State}
    (reachable : Reach (fun s => initial s ∧ extraInitial s)
      (fun s t => step s t ∧ extraStep s t) s) : invariant s := by
  exact projected_safety id initial step _ _ invariant
    (fun _ h => h.1) (fun _ _ h => h.1) baseInit basePreserves reachable

end Norn.Verification.Specialisation
