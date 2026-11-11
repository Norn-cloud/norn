import Aeneas

/-!
# Trusted models of Rust core functions that Aeneas leaves external

Aeneas has no model of these `core` functions, so it emits each as an axiom in
`FunsExternal_Template.lean`. They are defined here instead, once, as Rust's
documented semantics, and every extracted module's `FunsExternal` imports them.
These definitions are the stated trust assumption
`TRUST-CHARON-AENEAS-AND-CORE-MODELS`; no axiom is introduced.
-/

open Aeneas Aeneas.Std Result

/-- `i128::unsigned_abs`: the magnitude as `u128`; total, never panics. -/
def core.num.I128.unsigned_abs (x : Std.I128) : Result Std.U128 :=
  ok ⟨BitVec.ofNat 128 x.val.natAbs⟩

/-- `i128::checked_neg`: `None` exactly when `-x` leaves the `i128` range. -/
def core.num.I128.checked_neg (x : Std.I128) : Result (Option Std.I128) :=
  ok (if h : -2 ^ (IScalarTy.I128.numBits - 1) ≤ -x.val ∧
      -x.val < 2 ^ (IScalarTy.I128.numBits - 1) then
    some (I128.ofIntCore (-x.val) h) else none)

/-- `i128::checked_abs`: `None` exactly when `|x|` leaves the `i128` range. -/
def core.num.I128.checked_abs (x : Std.I128) : Result (Option Std.I128) :=
  ok (if h : -2 ^ (IScalarTy.I128.numBits - 1) ≤ |x.val| ∧
      |x.val| < 2 ^ (IScalarTy.I128.numBits - 1) then
    some (I128.ofIntCore |x.val| h) else none)

/-- `<i64 as TryFrom<i128>>::try_from`: `Ok` exactly when the value fits. -/
def I64.Insts.CoreConvertTryFromI128TryFromIntError.try_from (x : Std.I128) :
    Result (core.result.Result Std.I64 core.num.error.TryFromIntError) :=
  ok (if h : -2 ^ (IScalarTy.I64.numBits - 1) ≤ x.val ∧
      x.val < 2 ^ (IScalarTy.I64.numBits - 1) then
    core.result.Result.Ok (I64.ofIntCore x.val h) else core.result.Result.Err ())

/-- `Result::ok`: `Ok(v)` becomes `Some(v)`, `Err(_)` becomes `None`. -/
def core.result.Result.ok {T : Type} {E : Type} (x : core.result.Result T E) :
    Result (Option T) :=
  Aeneas.Std.Result.ok (match x with
    | core.result.Result.Ok v => some v
    | core.result.Result.Err _ => none)

/-- `Option::ok_or`: `Some(v)` becomes `Ok(v)`, `None` becomes `Err(err)`. -/
def core.option.Option.ok_or {T : Type} {E : Type} (x : Option T) (err : E) :
    Result (core.result.Result T E) :=
  ok (match x with
    | some v => core.result.Result.Ok v
    | none => core.result.Result.Err err)

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
