import RoundingReference

/-!
# FOLD-SCALED / VAL-ROUND / VAL-EXACT-1: written semantics of the scaled core

The scaled-arithmetic core of `vendor/zen/core/expression/src/scaled.rs`, as
`verification/ordered-fold-v1/contract.md` states it (FOLD-ARITHMETIC,
FOLD-SCALED, FOLD-RESCALE, VAL-ROUND, VAL-EXACT-1), over ℚ and ℤ. A decimal
`(c, s)` denotes `c / 10^s`; a result is stored as `rust_decimal` stores it
(at most 28 places, a 96-bit coefficient) or refused.

FOLD-SCALED fixes the arithmetic as well as the value: coefficients and
intermediate products are checked signed 128-bit integers, and "an
intermediate overflow refuses even if algebraic cancellation could fit the
final result". The specs below therefore state those intermediate refusals.
-/

namespace Norn.OrderedFold

/-- The value of the decimal `(c, s)`. -/
def decValue (c : ℤ) (s : ℕ) : ℚ := (c : ℚ) / 10 ^ s

/-- A result `rust_decimal` can store, or a refusal. -/
def store (c : ℤ) (s : ℕ) : Option (ℤ × ℕ) :=
  if s ≤ 28 ∧ -2 ^ 96 < c ∧ c < 2 ^ 96 then some (c, s) else none

/-- FOLD-SCALED: `v`'s exact integer coefficient at scale `s`; refused above
    scale 18, when `v` is not exact at `s`, or when it leaves `i128`. -/
def coefficientSpec (s : ℕ) (v : ℚ) : Option ℤ :=
  if s ≤ 18 ∧ (v * 10 ^ s).den = 1 ∧ fitsI128 (v * 10 ^ s).num = true then
    some (v * 10 ^ s).num
  else none

/-- FOLD-RESCALE / VAL-ROUND: `v` quantized to scale `s` by the declared mode. -/
def rescaleSpec (mode : Mode) (s : ℕ) (v : ℚ) : Option (ℤ × ℕ) :=
  if s ≤ 18 then (roundRat mode (v * 10 ^ s)).bind (fun c => store c s) else none

/-- The arithmetic opcode a scaled operation names. -/
inductive Op where
  | add
  | subtract
  | multiply
  | divide
  | other
  deriving DecidableEq

/-- VAL-EXACT-1: the scale of an exact result, which never exceeds 18. -/
def exactResultScaleSpec (op : Op) (a b : ℕ) : Option ℕ :=
  match op with
  | .add | .subtract => if max a b ≤ 18 then some (max a b) else none
  | .multiply => if a + b ≤ 18 then some (a + b) else none
  | .divide | .other => none

/-- VAL-EXACT-1: exact addition, subtraction or multiplication; never rounded.
    Multiplication multiplies the stored coefficients; addition and
    subtraction align both operands at the result scale. -/
def exactBinarySpec (op : Op) (ac : ℤ) (as : ℕ) (bc : ℤ) (bs : ℕ) : Option (ℤ × ℕ) :=
  match exactResultScaleSpec op as bs with
  | none => none
  | some s =>
    match op with
    | .multiply => if fitsI128 (ac * bc) = true then store (ac * bc) s else none
    | .add =>
        match coefficientSpec s (decValue ac as), coefficientSpec s (decValue bc bs) with
        | some x, some y => if fitsI128 (x + y) = true then store (x + y) s else none
        | _, _ => none
    | .subtract =>
        match coefficientSpec s (decValue ac as), coefficientSpec s (decValue bc bs) with
        | some x, some y => if fitsI128 (x - y) = true then store (x - y) s else none
        | _, _ => none
    | .divide | .other => none

/-- VAL-EXACT-1: exact comparison after scale alignment. -/
def exactCompareSpec (ac : ℤ) (as : ℕ) (bc : ℤ) (bs : ℕ) : Option Ordering :=
  match exactResultScaleSpec .add as bs with
  | none => none
  | some s =>
    match coefficientSpec s (decValue ac as), coefficientSpec s (decValue bc bs) with
    | some x, some y => some (compare x y)
    | _, _ => none

/-- VAL-ROUND: `a / b` divided directly into the rounding site `(mode, s)`.
    The operands are aligned by multiplying one coefficient by a power of ten,
    a checked `i128` product. -/
def divideSpec (mode : Mode) (s : ℕ) (ac : ℤ) (as : ℕ) (bc : ℤ) (bs : ℕ) : Option (ℤ × ℕ) :=
  if 18 < max as bs ∨ 18 < s then none
  else
    let e : ℤ := (s : ℤ) + bs - as
    let n : ℤ := if 0 ≤ e then ac * 10 ^ e.toNat else ac
    let d : ℤ := if 0 ≤ e then bc else bc * 10 ^ (-e).toNat
    if fitsI128 n = true ∧ fitsI128 d = true then (quantize mode n d).bind (fun c => store c s)
    else none

/-- FOLD-SCALED: one operation on the operands' coefficients at the declared
    scale `s`. Multiplication divides the product by `10^s`, division
    multiplies the numerator by `10^s` before dividing, each quotient by the
    declared rounding. -/
def binarySpec (mode : Mode) (s : ℕ) (op : Op) (ac : ℤ) (as : ℕ) (bc : ℤ) (bs : ℕ) :
    Option (ℤ × ℕ) :=
  match coefficientSpec s (decValue ac as), coefficientSpec s (decValue bc bs) with
  | some x, some y =>
    match op with
    | .add => if fitsI128 (x + y) = true then store (x + y) s else none
    | .subtract => if fitsI128 (x - y) = true then store (x - y) s else none
    | .multiply =>
        if fitsI128 (x * y) = true then (quantize mode (x * y) (10 ^ s)).bind (fun c => store c s)
        else none
    | .divide =>
        if fitsI128 (x * 10 ^ s) = true then
          (quantize mode (x * 10 ^ s) y).bind (fun c => store c s)
        else none
    | .other => none
  | _, _ => none

end Norn.OrderedFold
