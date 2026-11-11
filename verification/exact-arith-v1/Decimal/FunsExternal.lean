import Aeneas
import Decimal.Types

/-!
# Trusted models of the Rust core functions `checked_pow10` calls

Aeneas has no model of `Option`'s `?` operator, so it emits these two `core`
functions as axioms in `FunsExternal_Template.lean`. They are defined here
instead, as Rust's documented semantics. These definitions are a stated trust
assumption of the correspondence proof; no axiom is introduced.
-/

open Aeneas Aeneas.Std Result

/-- `<Option<T> as Try>::branch`: `Some(v)` continues with `v`; `None` breaks
    with the `None` residual. -/
def core.option.Option.Insts.CoreOpsTry_traitTry.branch {T : Type} (x : Option T) :
    Result (core.ops.control_flow.ControlFlow (Option core.convert.Infallible) T) :=
  ok (match x with
    | some v => core.ops.control_flow.ControlFlow.Continue v
    | none => core.ops.control_flow.ControlFlow.Break none)

/-- `<Option<T> as FromResidual<Option<Infallible>>>::from_residual`: `None`. -/
def core.option.Option.Insts.CoreOpsTry_traitFromResidualOptionInfallible.from_residual
    (T : Type) (_residual : Option core.convert.Infallible) : Result (Option T) :=
  ok none
