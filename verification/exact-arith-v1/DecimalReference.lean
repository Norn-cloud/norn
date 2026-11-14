import Mathlib

/-!
# VAL-EXACT-1 canonical decimal parts: written semantics

`IsCanonicalForm u s c s'` says that the decimal `c × 10^(-s')` is the
canonical form of `u × 10^(-s)`, as VAL-EXACT-1 requires of
`quantity_ledger::decimal::canonical_parts` ("remove only representational
zeros; retain the exact value"):

* the scale never grows (`s' ≤ s`);
* the value is retained exactly (`c × 10^(s - s') = u`);
* zero is canonically `(0, 0)`;
* no representational trailing zero remains (`0 < s'` implies `10 ∤ c`).

`pow10Spec e` is the exact multiplier `10^e`, or a refusal exactly when it does
not fit a signed 128-bit integer.

The statements mention neither machine integers nor the Rust loops.
`IsCanonicalForm.unique` shows these clauses determine `(c, s')` uniquely, so
they are a complete functional specification, not a loose property.
-/

namespace Norn.QuantityLedger

/-- VAL-EXACT-1: the exact decimal alignment multiplier `10^e`, refused exactly
    when it leaves the signed 128-bit range. -/
def pow10Spec (e : ℕ) : Option ℤ :=
  if (10 : ℤ) ^ e < 2 ^ 127 then some (10 ^ e) else none

/-- VAL-EXACT-1: `(c, s')` is the canonical form of the decimal `(u, s)`. -/
def IsCanonicalForm (u : ℤ) (s : ℕ) (c : ℤ) (s' : ℕ) : Prop :=
  s' ≤ s ∧ c * 10 ^ (s - s') = u ∧ (c = 0 → s' = 0) ∧ (0 < s' → ¬ (10 : ℤ) ∣ c)

private theorem unique_of_le {u : ℤ} {s : ℕ} {c₁ c₂ : ℤ} {s₁ s₂ : ℕ}
    (h₁ : IsCanonicalForm u s c₁ s₁) (h₂ : IsCanonicalForm u s c₂ s₂) (hle : s₁ ≤ s₂) :
    c₁ = c₂ ∧ s₁ = s₂ := by
  obtain ⟨hs₁, hv₁, -, -⟩ := h₁
  obtain ⟨hs₂, hv₂, -, hd₂⟩ := h₂
  have hsplit : s - s₁ = (s₂ - s₁) + (s - s₂) := by omega
  have hpos : (10 : ℤ) ^ (s - s₂) ≠ 0 := pow_ne_zero _ (by norm_num)
  have hc : c₁ * 10 ^ (s₂ - s₁) = c₂ := by
    apply mul_right_cancel₀ hpos
    rw [mul_assoc, ← pow_add, ← hsplit, hv₁, hv₂]
  rcases Nat.lt_or_ge s₁ s₂ with hlt | hge
  · exfalso
    apply hd₂ (by omega)
    rw [← hc]
    obtain ⟨k, hk⟩ : ∃ k, s₂ - s₁ = k + 1 := ⟨s₂ - s₁ - 1, by omega⟩
    rw [hk, pow_succ]
    exact dvd_mul_of_dvd_right (dvd_mul_left 10 (10 ^ k)) c₁
  · have heq : s₁ = s₂ := le_antisymm hle hge
    subst heq
    simp only [Nat.sub_self, pow_zero, mul_one] at hc
    exact ⟨hc, rfl⟩

/-- The canonical form of a decimal is unique. -/
theorem IsCanonicalForm.unique {u : ℤ} {s : ℕ} {c₁ c₂ : ℤ} {s₁ s₂ : ℕ}
    (h₁ : IsCanonicalForm u s c₁ s₁) (h₂ : IsCanonicalForm u s c₂ s₂) :
    c₁ = c₂ ∧ s₁ = s₂ := by
  rcases le_total s₁ s₂ with h | h
  · exact unique_of_le h₁ h₂ h
  · obtain ⟨hc, hs⟩ := unique_of_le h₂ h₁ h
    exact ⟨hc.symm, hs.symm⟩

end Norn.QuantityLedger
