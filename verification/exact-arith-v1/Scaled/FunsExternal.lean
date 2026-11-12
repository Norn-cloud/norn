import CoreModels
import Scaled.Types

/-!
# Trusted models of the `rust_decimal` functions the scaled core calls

Written from `rust_decimal` 1.43's documented behaviour, over the
`Decimal` model of `Scaled/TypesExternal.lean`. Part of
`TRUST-CHARON-AENEAS-AND-CORE-MODELS`; no axiom is introduced.

One deliberate difference: see `try_from_i128_with_scale` for the error variant
on a coefficient that is too negative.
-/

open Aeneas Aeneas.Std Result

/-- Remove trailing zeros from `m · 10^(-s)`: the representation `normalize`
    returns, with zero as `(0, 0)`. -/
def rust_decimal.decimal.stripZeros (m : ℤ) : ℕ → ℤ × ℕ
  | 0 => (m, 0)
  | s + 1 => if m % 10 = 0 then rust_decimal.decimal.stripZeros (m / 10) s else (m, s + 1)

theorem rust_decimal.decimal.stripZeros_bound (m : ℤ) (s : ℕ)
    (h : -2 ^ 96 < m ∧ m < 2 ^ 96) :
    (-2 ^ 96 < (rust_decimal.decimal.stripZeros m s).1 ∧
      (rust_decimal.decimal.stripZeros m s).1 < 2 ^ 96) ∧
      (rust_decimal.decimal.stripZeros m s).2 ≤ s := by
  induction s generalizing m with
  | zero => exact ⟨h, le_refl _⟩
  | succ s ih =>
      unfold rust_decimal.decimal.stripZeros
      split_ifs
      · have := ih (m / 10) (by omega)
        exact ⟨this.1, by omega⟩
      · exact ⟨h, le_refl _⟩

/-- `Decimal::scale`. -/
def rust_decimal.decimal.Decimal.scale (d : rust_decimal.decimal.Decimal) : Result Std.U32 :=
  ok ⟨BitVec.ofNat 32 d.places⟩

/-- `Decimal::mantissa`: the signed 96-bit mantissa as `i128`. -/
def rust_decimal.decimal.Decimal.mantissa (d : rust_decimal.decimal.Decimal) :
    Result Std.I128 :=
  ok (I128.ofIntCore d.coeff (by
    have := d.coeff_bound
    simp only [IScalarTy.numBits]
    constructor <;> omega))

/-- `Decimal::normalize`: strip trailing zeros; zero becomes `0` at scale 0. -/
def rust_decimal.decimal.Decimal.normalize (d : rust_decimal.decimal.Decimal) :
    Result rust_decimal.decimal.Decimal :=
  have h := rust_decimal.decimal.stripZeros_bound d.coeff d.places d.coeff_bound
  ok ⟨(rust_decimal.decimal.stripZeros d.coeff d.places).1,
    (rust_decimal.decimal.stripZeros d.coeff d.places).2, h.1, le_trans h.2 d.places_bound⟩

/-- `Decimal::try_from_i128_with_scale`: `Ok` exactly when the scale is at most
    28 and the coefficient fits 96 bits.

    The error variant is not modelled faithfully. For a coefficient at or below
    `-2^96` this model returns `ExceedsMaximumPossibleValue`, where
    `rust_decimal` 1.43 returns `LessThanMinimumPossibleValue`. The scaled core
    cannot observe the difference: its only caller, `to_decimal`, discards the
    error with `.ok()`. A caller that inspects the variant must not rely on this
    model. -/
def rust_decimal.decimal.Decimal.try_from_i128_with_scale (c : Std.I128) (s : Std.U32) :
    Result (core.result.Result rust_decimal.decimal.Decimal
      zen_expression.rust_decimal.error.Error) :=
  ok (if h : s.val ≤ 28 ∧ -2 ^ 96 < c.val ∧ c.val < 2 ^ 96 then
    core.result.Result.Ok ⟨c.val, s.val, h.2, h.1⟩
  else if s.val ≤ 28 then
    core.result.Result.Err zen_expression.rust_decimal.error.Error.ExceedsMaximumPossibleValue
  else
    core.result.Result.Err (zen_expression.rust_decimal.error.Error.ScaleExceedsMaximumPrecision s))
