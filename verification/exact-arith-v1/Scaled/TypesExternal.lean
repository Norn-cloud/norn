import Aeneas

/-!
# Trusted model of the `rust_decimal::Decimal` type

Charon leaves `rust_decimal::Decimal` (1.43) opaque. It is modelled as the exact
decimal `coeff · 10^(-places)` with a 96-bit coefficient and at most 28
decimal places, the range `rust_decimal` documents. The sign of a zero is not
modelled; no modelled function observes it. Part of
`TRUST-CHARON-AENEAS-AND-CORE-MODELS`; no axiom is introduced.
-/

/-- `rust_decimal::Decimal`: `coeff · 10^(-places)`. -/
structure rust_decimal.decimal.Decimal where
  coeff : Int
  places : Nat
  coeff_bound : -2 ^ 96 < coeff ∧ coeff < 2 ^ 96
  places_bound : places ≤ 28
