import Mathlib

/-!
# FOLD-ARITHMETIC quantization: written semantics

The reference semantics for quantizing an exact quotient `n / d` to an integer
coefficient, transcribed from `verification/ordered-fold-v1/contract.md`
(FOLD-ARITHMETIC and FOLD-SCALED). It is stated over the rationals and never
mentions machine integers, remainders or the Rust implementation:

* division by zero refuses;
* `refuse` rejects a nonzero remainder, i.e. a non-integral quotient;
* `toward_zero` discards the fractional magnitude;
* `half_even` selects the nearest integer and the even one at an exact midpoint;
* `half_away_from_zero` selects the nearest integer and the larger magnitude at
  an exact midpoint;
* both nearest modes are symmetric for negative quotients;
* a result outside the signed 128-bit coefficient range refuses.
-/

namespace Norn.OrderedFold

/-- The declared rounding mode of a quantization site. -/
inductive Mode where
  | refuse
  | towardZero
  | halfEven
  | halfAwayFromZero
  deriving DecidableEq, Repr

/-- Quantize an exact rational to an integer under a declared mode. -/
def roundRat : Mode → ℚ → Option ℤ
  | .refuse, q => if q.den = 1 then some q.num else none
  | .towardZero, q => some (if 0 ≤ q then ⌊q⌋ else ⌈q⌉)
  | .halfEven, q =>
      if q - ⌊q⌋ < 1 / 2 then some ⌊q⌋
      else if 1 / 2 < q - ⌊q⌋ then some (⌊q⌋ + 1)
      else some (if Even ⌊q⌋ then ⌊q⌋ else ⌊q⌋ + 1)
  | .halfAwayFromZero, q =>
      some (if 0 ≤ q then ⌊q + 1 / 2⌋ else -⌊-q + 1 / 2⌋)

/-- Signed 128-bit coefficient range. -/
def fitsI128 (z : ℤ) : Bool := decide (-2 ^ 127 ≤ z ∧ z < 2 ^ 127)

/-- FOLD-ARITHMETIC quotient: `none` is a refusal. -/
def quantize (mode : Mode) (n d : ℤ) : Option ℤ :=
  if d = 0 then none
  else (roundRat mode ((n : ℚ) / d)).filter fitsI128

end Norn.OrderedFold
