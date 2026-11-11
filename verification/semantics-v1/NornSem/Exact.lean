import Mathlib

/-!
# Exact runtime numbers (VAL-EXACT-1)

A runtime number is an exact decimal: a mantissa and a scale, denoting
`mantissa / 10^scale`. Numbers are never rounded implicitly; an operation whose
exact result has no runtime decimal refuses.

* `Dec.val` is the denotation, in ℚ.
* `Dec.InRange` is the `rust_decimal` range of `RuntimeValue::Number`: a
  mantissa of at most 96 bits and a scale of at most 28.
* `Dec.Canonical` removes representational zeros: zero is `(0, 0)` and a
  positive scale never leaves a trailing zero.
* `IsExact q` says some in-range decimal denotes `q`.

The theorems show that the canonical form is a function of the value
(`Dec.canonical_unique`), always exists (`Dec.exists_canonical`) and never
leaves the range (`Dec.canonical_in_range`). So comparing canonical forms decides
numeric equality, and canonical encodings identify values.

This module mentions no machine integer and no Rust code. No module bridges it
to exact-arith-v1's `IsCanonicalForm` or `RoundingReference` yet: the two
statements of canonical form are separate.
-/

namespace Norn.Sem.Exact

/-- Largest scale (fractional digits) of a runtime decimal. -/
def maxScale : ℕ := 28

/-- Exclusive bound on a runtime decimal's mantissa magnitude (96 bits). -/
def mantissaBound : ℤ := 2 ^ 96

/-- A runtime decimal as written: mantissa and scale. -/
structure Dec where
  mantissa : ℤ
  scale : ℕ
  deriving DecidableEq, Repr

namespace Dec

/-- The exact value `mantissa / 10^scale`. -/
def val (d : Dec) : ℚ := (d.mantissa : ℚ) / (10 : ℚ) ^ d.scale

/-- The range of `RuntimeValue::Number`: 96-bit mantissa, scale at most 28. -/
def InRange (d : Dec) : Prop := |d.mantissa| < mantissaBound ∧ d.scale ≤ maxScale

/-- No representational zero remains: zero is `(0, 0)`, and a positive scale
    leaves no trailing zero. -/
def Canonical (d : Dec) : Prop :=
  (d.mantissa = 0 → d.scale = 0) ∧ (0 < d.scale → ¬ (10 : ℤ) ∣ d.mantissa)

private theorem cross {m₁ m₂ : ℤ} {s₁ s₂ : ℕ}
    (h : (m₁ : ℚ) / (10 : ℚ) ^ s₁ = (m₂ : ℚ) / (10 : ℚ) ^ s₂) :
    m₁ * 10 ^ s₂ = m₂ * 10 ^ s₁ := by
  rw [div_eq_div_iff (pow_ne_zero _ (by norm_num)) (pow_ne_zero _ (by norm_num))] at h
  exact_mod_cast h

private theorem unique_of_le {m₁ m₂ : ℤ} {s₁ s₂ : ℕ}
    (c₂ : 0 < s₂ → ¬ (10 : ℤ) ∣ m₂)
    (hx : m₁ * 10 ^ s₂ = m₂ * 10 ^ s₁) (hle : s₁ ≤ s₂) : m₁ = m₂ ∧ s₁ = s₂ := by
  obtain ⟨k, rfl⟩ : ∃ k, s₂ = s₁ + k := ⟨s₂ - s₁, by omega⟩
  rw [pow_add] at hx
  have hpos : (10 : ℤ) ^ s₁ ≠ 0 := pow_ne_zero _ (by norm_num)
  have hm : m₂ = m₁ * 10 ^ k := by
    apply mul_right_cancel₀ hpos
    rw [← hx]
    ring
  cases k with
  | zero =>
    simp only [pow_zero, mul_one] at hm
    exact ⟨hm.symm, rfl⟩
  | succ j =>
    exfalso
    apply c₂ (by omega)
    rw [hm, pow_succ]
    exact Dvd.dvd.mul_left (dvd_mul_left 10 _) _

/-- The canonical form is a function of the value: two canonical decimals with
    the same value are the same decimal. -/
theorem canonical_unique {d₁ d₂ : Dec} (c₁ : d₁.Canonical) (c₂ : d₂.Canonical)
    (h : d₁.val = d₂.val) : d₁ = d₂ := by
  obtain ⟨m₁, s₁⟩ := d₁
  obtain ⟨m₂, s₂⟩ := d₂
  have hx : m₁ * 10 ^ s₂ = m₂ * 10 ^ s₁ := cross h
  rcases le_total s₁ s₂ with hle | hle
  · obtain ⟨hm, hs⟩ := unique_of_le (And.right c₂) hx hle
    subst hm
    subst hs
    rfl
  · obtain ⟨hm, hs⟩ := unique_of_le (And.right c₁) hx.symm hle
    subst hm
    subst hs
    rfl

/-- Canonical decimals are equal exactly when their values are. -/
theorem canonical_eq_iff {d₁ d₂ : Dec} (c₁ : d₁.Canonical) (c₂ : d₂.Canonical) :
    d₁ = d₂ ↔ d₁.val = d₂.val :=
  ⟨fun h => h ▸ rfl, canonical_unique c₁ c₂⟩

/-- Every decimal has a canonical form with the same value, a mantissa no
    larger in magnitude and a scale no larger. -/
theorem exists_canonical (s : ℕ) : ∀ m : ℤ, ∃ c : Dec,
    c.Canonical ∧ c.val = (Dec.mk m s).val ∧ |c.mantissa| ≤ |m| ∧ c.scale ≤ s := by
  induction s with
  | zero =>
    intro m
    exact ⟨⟨m, 0⟩, ⟨fun _ => rfl, fun h => absurd h (lt_irrefl 0)⟩, rfl, le_rfl, le_rfl⟩
  | succ t ih =>
    intro m
    by_cases hm : m = 0
    · subst hm
      refine ⟨⟨0, 0⟩, ⟨fun _ => rfl, fun h => absurd h (lt_irrefl 0)⟩, ?_, le_rfl,
        Nat.zero_le _⟩
      simp [Dec.val]
    · by_cases hd : (10 : ℤ) ∣ m
      · obtain ⟨k, rfl⟩ := hd
        obtain ⟨c, hc, hv, hb, hsc⟩ := ih k
        refine ⟨c, hc, ?_, ?_, by omega⟩
        · rw [hv]
          simp only [Dec.val]
          rw [div_eq_div_iff (pow_ne_zero _ (by norm_num)) (pow_ne_zero _ (by norm_num))]
          push_cast
          ring
        · calc |c.mantissa| ≤ |k| := hb
            _ ≤ |10 * k| := by
              rw [abs_mul]
              have : (0 : ℤ) ≤ |k| := abs_nonneg k
              have h10 : |(10 : ℤ)| = 10 := by norm_num
              rw [h10]
              omega
      · exact ⟨⟨m, t + 1⟩, ⟨fun h => absurd h hm, fun _ h => hd h⟩, rfl, le_rfl, le_rfl⟩

/-- Canonicalization never leaves the runtime range. -/
theorem canonical_in_range (d : Dec) (h : d.InRange) :
    ∃ c : Dec, c.Canonical ∧ c.val = d.val ∧ c.InRange := by
  obtain ⟨c, hc, hv, hb, hs⟩ := exists_canonical d.scale d.mantissa
  exact ⟨c, hc, hv, lt_of_le_of_lt hb h.1, le_trans hs h.2⟩

end Dec

/-- A rational is an exact runtime number when some in-range decimal denotes it. -/
def IsExact (q : ℚ) : Prop := ∃ d : Dec, d.InRange ∧ d.val = q

/-- Every exact runtime number has exactly one canonical in-range decimal. -/
theorem IsExact.existsUnique_canonical {q : ℚ} (h : IsExact q) :
    ∃! c : Dec, c.Canonical ∧ c.val = q ∧ c.InRange := by
  obtain ⟨d, hr, rfl⟩ := h
  obtain ⟨c, hc, hv, hcr⟩ := Dec.canonical_in_range d hr
  exact ⟨c, ⟨hc, hv, hcr⟩, fun c' ⟨hc', hv', _⟩ => Dec.canonical_unique hc' hc (hv'.trans hv.symm)⟩

open Classical in
/-- VAL-EXACT-1: an operation's outcome is its exact rational result, or a
    refusal (`none`) when no runtime decimal denotes it. It is never rounded. -/
noncomputable def exactly (q : ℚ) : Option ℚ := if IsExact q then some q else none

/-- An admitted outcome is the exact result itself. -/
theorem exactly_eq_some {q r : ℚ} : exactly q = some r ↔ IsExact q ∧ r = q := by
  unfold exactly
  split_ifs with h
  · simp [h, eq_comm]
  · simp [h]

end Norn.Sem.Exact
