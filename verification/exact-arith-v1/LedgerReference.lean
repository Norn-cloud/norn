import DecimalReference

/-!
# VAL-EXACT-1 ledger scaled-integer arithmetic: written semantics

A ledger amount is the exact decimal `u × 10^(-s)` with a signed 128-bit
coefficient. Ledger lowering (`quantity_ledger::expression`) adds, subtracts
and multiplies amounts exactly; it never rounds, and it refuses instead of
overflowing. These definitions state, for every input, which result or
refusal each operation returns:

* `rescaleSpec u s t`: the coefficient of `u × 10^(-s)` at scale `t`. It
  refuses a lower target scale (that would discard digits), an alignment
  multiplier `10^(t - s)` outside `i128`, and an aligned coefficient outside
  `i128`, checked in that order.
* `addSpec` / `subSpec`: align both operands to the larger scale (the left
  operand's refusal first), then add or subtract exactly. They refuse a result
  outside `i128`.
* `mulSpec`: the product scale is `sa + sb`. It refuses a scale beyond `u8`,
  then a scale above the caller's maximum, then a product outside `i128`.

A returned amount is then put in canonical form (`IsCanonicalForm`). The
`*_exact` theorems show that every accepted result is the exact rational sum,
difference or product. The statements mention neither machine integers nor
Rust control flow.
-/

namespace Norn.QuantityLedger

/-- The exact value of the decimal `u × 10^(-s)`. -/
def ledgerValue (u : ℤ) (s : ℕ) : ℚ := (u : ℚ) / 10 ^ s

/-- `z` is a signed 128-bit integer. -/
abbrev InI128 (z : ℤ) : Prop := -2 ^ 127 ≤ z ∧ z < 2 ^ 127

/-- Why exact ledger arithmetic refuses. -/
inductive LedgerRefusal
  | scaleLoss
  | alignmentMultiplierOverflow
  | alignmentOverflow
  | additionOverflow
  | subtractionOverflow
  | multiplicationScaleOverflow
  | multiplicationScaleAboveMaximum (scale : ℕ)
  | multiplicationOverflow
  deriving DecidableEq

/-- The coefficient of `u × 10^(-s)` at scale `t`, or the refusal. -/
def rescaleSpec (u : ℤ) (s t : ℕ) : Except LedgerRefusal ℤ :=
  if t < s then .error .scaleLoss
  else match pow10Spec (t - s) with
    | none => .error .alignmentMultiplierOverflow
    | some m => if InI128 (u * m) then .ok (u * m) else .error .alignmentOverflow

/-- Both coefficients at the larger scale, and that scale. -/
def alignSpec (ua : ℤ) (sa : ℕ) (ub : ℤ) (sb : ℕ) : Except LedgerRefusal (ℤ × ℤ × ℕ) :=
  match rescaleSpec ua sa (max sa sb), rescaleSpec ub sb (max sa sb) with
  | .error r, _ => .error r
  | .ok _, .error r => .error r
  | .ok a, .ok b => .ok (a, b, max sa sb)

/-- The exact sum as an unnormalized `(coefficient, scale)`, or the refusal. -/
def addSpec (ua : ℤ) (sa : ℕ) (ub : ℤ) (sb : ℕ) : Except LedgerRefusal (ℤ × ℕ) :=
  match alignSpec ua sa ub sb with
  | .error r => .error r
  | .ok (a, b, t) => if InI128 (a + b) then .ok (a + b, t) else .error .additionOverflow

/-- The exact difference as an unnormalized `(coefficient, scale)`, or the refusal. -/
def subSpec (ua : ℤ) (sa : ℕ) (ub : ℤ) (sb : ℕ) : Except LedgerRefusal (ℤ × ℕ) :=
  match alignSpec ua sa ub sb with
  | .error r => .error r
  | .ok (a, b, t) => if InI128 (a - b) then .ok (a - b, t) else .error .subtractionOverflow

/-- The exact product as an unnormalized `(coefficient, scale)`, with a scale of
    at most `m`, or the refusal. -/
def mulSpec (ua : ℤ) (sa : ℕ) (ub : ℤ) (sb : ℕ) (m : ℕ) : Except LedgerRefusal (ℤ × ℕ) :=
  if 256 ≤ sa + sb then .error .multiplicationScaleOverflow
  else if m < sa + sb then .error (.multiplicationScaleAboveMaximum (sa + sb))
  else if InI128 (ua * ub) then .ok (ua * ub, sa + sb) else .error .multiplicationOverflow

/-- The canonical form denotes the same value. -/
theorem IsCanonicalForm.value_eq {u : ℤ} {s : ℕ} {c : ℤ} {s' : ℕ}
    (h : IsCanonicalForm u s c s') : ledgerValue c s' = ledgerValue u s := by
  obtain ⟨hle, hv, -, -⟩ := h
  rw [ledgerValue, ledgerValue, ← hv]; push_cast
  rw [pow_sub₀ (10 : ℚ) (by norm_num) hle]
  field_simp

/-- Rescaling never changes the value. -/
theorem rescaleSpec_exact {u : ℤ} {s t : ℕ} {a : ℤ} (h : rescaleSpec u s t = .ok a) :
    ledgerValue a t = ledgerValue u s := by
  unfold rescaleSpec at h
  split_ifs at h with hts
  rcases hp : pow10Spec (t - s) with _ | m
  · simp [hp] at h
  · simp only [hp] at h
    split_ifs at h
    cases h
    have hm : m = 10 ^ (t - s) := by
      unfold pow10Spec at hp; split_ifs at hp; exact (Option.some.inj hp).symm
    rw [hm, ledgerValue, ledgerValue]; push_cast
    rw [pow_sub₀ (10 : ℚ) (by norm_num) (by omega)]
    field_simp

private theorem alignSpec_exact {ua ub a b : ℤ} {sa sb t : ℕ}
    (h : alignSpec ua sa ub sb = .ok (a, b, t)) :
    ledgerValue a t = ledgerValue ua sa ∧ ledgerValue b t = ledgerValue ub sb := by
  unfold alignSpec at h
  split at h
  · cases h
  · cases h
  · rename_i a' b' ha hb
    cases h
    exact ⟨rescaleSpec_exact ha, rescaleSpec_exact hb⟩

/-- An accepted sum is the exact rational sum. -/
theorem addSpec_exact {ua ub c : ℤ} {sa sb t : ℕ} (h : addSpec ua sa ub sb = .ok (c, t)) :
    ledgerValue c t = ledgerValue ua sa + ledgerValue ub sb := by
  unfold addSpec at h
  split at h
  · cases h
  · rename_i a b t' ha
    split_ifs at h
    cases h
    obtain ⟨h1, h2⟩ := alignSpec_exact ha
    rw [← h1, ← h2, ledgerValue, ledgerValue, ledgerValue]; push_cast; ring

/-- An accepted difference is the exact rational difference. -/
theorem subSpec_exact {ua ub c : ℤ} {sa sb t : ℕ} (h : subSpec ua sa ub sb = .ok (c, t)) :
    ledgerValue c t = ledgerValue ua sa - ledgerValue ub sb := by
  unfold subSpec at h
  split at h
  · cases h
  · rename_i a b t' ha
    split_ifs at h
    cases h
    obtain ⟨h1, h2⟩ := alignSpec_exact ha
    rw [← h1, ← h2, ledgerValue, ledgerValue, ledgerValue]; push_cast; ring

/-- An accepted product is the exact rational product. -/
theorem mulSpec_exact {ua ub c : ℤ} {sa sb m t : ℕ} (h : mulSpec ua sa ub sb m = .ok (c, t)) :
    ledgerValue c t = ledgerValue ua sa * ledgerValue ub sb := by
  unfold mulSpec at h
  split_ifs at h
  cases h
  rw [ledgerValue, ledgerValue, ledgerValue, pow_add]; push_cast
  field_simp

end Norn.QuantityLedger
