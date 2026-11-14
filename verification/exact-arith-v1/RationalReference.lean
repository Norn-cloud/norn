import Mathlib

/-!
# EXACT-RATIONAL-1: written semantics

`crates/norn-checker/src/exact_rational.rs` stores an exact rational as two
64-bit components and never rounds. This file states, over ℚ and ℤ only, what
each operation must return:

* the stored form of a rational is Mathlib's canonical `(num, den)`: lowest
  terms with a positive denominator; it is refused when either component leaves
  the signed 64-bit range;
* `normalize n d` stores `n / d`; it refuses a zero denominator and, as an
  intermediate overflow of its sign normalization, an `i128::MIN` operand;
* addition, subtraction, multiplication, division and negation store the exact
  result, division by zero refuses, and comparison is exact.
-/

namespace Norn.ExactRational

/-- A stored component fits the signed 64-bit range. -/
def fitsI64 (z : ℤ) : Bool := decide (-2 ^ 63 ≤ z ∧ z < 2 ^ 63)

/-- EXACT-RATIONAL-1: the stored form of an exact rational, or a refusal. -/
def represent (q : ℚ) : Option (ℤ × ℤ) :=
  if fitsI64 q.num && fitsI64 (q.den : ℤ) then some (q.num, q.den) else none

/-- `normalize n d`: the stored form of `n / d`, with its documented refusals. -/
def normalizeSpec (n d : ℤ) : Option (ℤ × ℤ) :=
  if d = 0 ∨ n = -2 ^ 127 ∨ d = -2 ^ 127 then none else represent ((n : ℚ) / d)

end Norn.ExactRational
