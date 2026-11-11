import Aeneas
import ScaledQuotient.Types

/-!
# Trusted models of the two Rust core functions the rounding quotient calls

Aeneas has no model of these two `core` functions, so it emits them as axioms
in `FunsExternal_Template.lean`. They are defined here instead, as Rust's
documented semantics. These definitions are a stated trust assumption of the
correspondence proof; no axiom is introduced.
-/

open Aeneas Aeneas.Std Result

/-- `i128::unsigned_abs`: the magnitude as `u128`; total, never panics. -/
def core.num.I128.unsigned_abs (x : Std.I128) : Result Std.U128 :=
  ok ⟨BitVec.ofNat 128 x.val.natAbs⟩

/-- `Option::ok_or`: `Some(v)` becomes `Ok(v)`, `None` becomes `Err(err)`. -/
def core.option.Option.ok_or {T : Type} {E : Type} (x : Option T) (err : E) :
    Result (core.result.Result T E) :=
  ok (match x with
    | some v => core.result.Result.Ok v
    | none => core.result.Result.Err err)
