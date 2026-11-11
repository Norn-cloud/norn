import ExactRational.Funs
import RationalReference

/-!
# Production `Rational` meets EXACT-RATIONAL-1

`ExactRational` is the fresh Aeneas translation of the production file
`crates/norn-checker/src/exact_rational.rs`, with the core models of
`CoreModels.lean`.
-/

namespace Norn.ExactRational
open Aeneas Aeneas.Std Result
open norn_checker_rational

/-- A stored rational's components. -/
def components (r : exact_rational.Rational) : ℤ × ℤ := (r.numerator.val, r.denominator.val)

/-- The exact value of a stored rational. -/
def value (r : exact_rational.Rational) : ℚ := (r.numerator.val : ℚ) / r.denominator.val

private theorem ieq (a b : I128) : a = b ↔ a.val = b.val := IScalar.eq_equiv a b

private theorem neg_some (x : I128) (h : x.val ≠ -2 ^ 127) :
    ∃ y, core.num.I128.checked_neg x = ok (some y) ∧ y.val = -x.val := by
  have hb := x.hBounds
  simp at hb
  unfold core.num.I128.checked_neg
  split_ifs with h1
  · exact ⟨_, rfl, I128.ofInt_val_eq _⟩
  · exfalso; simp at h1; omega

private theorem neg_none (x : I128) (h : x.val = -2 ^ 127) :
    core.num.I128.checked_neg x = ok none := by
  unfold core.num.I128.checked_neg
  rw [dif_neg]
  simp [h]

private theorem abs_some (x : I128) (h : x.val ≠ -2 ^ 127) :
    ∃ y, core.num.I128.checked_abs x = ok (some y) ∧ y.val = |x.val| := by
  have hb := x.hBounds
  simp at hb
  unfold core.num.I128.checked_abs
  split_ifs with h1
  · exact ⟨_, rfl, I128.ofInt_val_eq _⟩
  · exfalso; simp [abs_lt, le_abs] at h1
    rcases abs_cases x.val with ⟨h2, _⟩ | ⟨h2, _⟩ <;> omega

private theorem abs_none (x : I128) (h : x.val = -2 ^ 127) :
    core.num.I128.checked_abs x = ok none := by
  unfold core.num.I128.checked_abs
  rw [dif_neg]
  simp [h]

/-- Euclid's loop computes the gcd of two non-negative operands. -/
private theorem gcd_loop (k : ℕ) : ∀ (a b : I128), b.val.toNat = k → 0 ≤ a.val → 0 ≤ b.val →
    ∃ g : I128, exact_rational.Rational.normalize_loop0 a b = ok (some g) ∧
      g.val = (Int.gcd a.val b.val : ℤ) := by
  induction k using Nat.strong_induction_on with
  | _ k ih =>
    intro a b hk ha hb
    unfold exact_rational.Rational.normalize_loop0
    rw [loop.eq_1]
    dsimp only
    unfold exact_rational.Rational.normalize_loop0.body
    by_cases hb0 : b.val = 0
    · have hz : b = 0#i128 := (ieq b 0#i128).mpr (by simpa using hb0)
      refine ⟨a, by simp [hz], ?_⟩
      rw [hb0, Int.gcd_zero_right, Int.natAbs_of_nonneg ha]
    · have hne : ¬ b = 0#i128 := fun h => hb0 (by simpa using (ieq b 0#i128).mp h)
      have hrem := I128.checked_rem_bv_spec a b
      cases hr : core.num.checked_rem_IScalar a b with
      | none => rw [hr] at hrem; exact absurd hrem hb0
      | some r =>
        rw [hr] at hrem
        obtain ⟨-, hrv, -⟩ := hrem
        have hemod : r.val = a.val % b.val := by rw [hrv, Int.tmod_eq_emod_of_nonneg ha]
        have hr0 : 0 ≤ r.val := by rw [hemod]; exact Int.emod_nonneg _ hb0
        have hrlt : r.val < b.val := by
          rw [hemod]; exact Int.emod_lt_of_pos _ (lt_of_le_of_ne hb (Ne.symm hb0))
        obtain ⟨g, hg, hgv⟩ := ih r.val.toNat (by omega) b r rfl hb hr0
        refine ⟨g, ?_, ?_⟩
        · simp [hne, I128.checked_rem, hr, lift]
          exact hg
        · rw [hgv, hemod, Int.gcd_comm, Int.gcd_emod]

private theorem loop1_eq (a b : I128) :
    exact_rational.Rational.normalize_loop1 a b = exact_rational.Rational.normalize_loop0 a b :=
  rfl

/-- Dividing by the gcd gives Mathlib's canonical numerator and denominator. -/
private theorem reduce (n d : ℤ) (hd : 0 < d) :
    ((n : ℚ) / d).num = n / (Int.gcd n d : ℤ) ∧
      (((n : ℚ) / d).den : ℤ) = d / (Int.gcd n d : ℤ) := by
  have hg : 0 < Int.gcd n d := Int.gcd_pos_of_ne_zero_right n (ne_of_gt hd)
  have hg0 : (Int.gcd n d : ℤ) ≠ 0 := by exact_mod_cast hg.ne'
  obtain ⟨a, ha⟩ := Int.gcd_dvd_left n d
  obtain ⟨b, hb⟩ := Int.gcd_dvd_right n d
  have hna : n / (Int.gcd n d : ℤ) = a := (Int.eq_ediv_of_mul_eq_right hg0 ha.symm).symm
  have hdb : d / (Int.gcd n d : ℤ) = b := (Int.eq_ediv_of_mul_eq_right hg0 hb.symm).symm
  have hcop := Int.gcd_div_gcd_div_gcd hg
  rw [hna, hdb] at hcop
  have hgpos : (0 : ℤ) < Int.gcd n d := by exact_mod_cast hg
  have hbpos : 0 < b := by
    rcases lt_trichotomy b 0 with h | h | h
    · nlinarith
    · rw [h, mul_zero] at hb; omega
    · exact h
  have hcross : n * b = a * d := by
    have h1 := ha
    have h2 := hb
    set g : ℤ := (Int.gcd n d : ℤ)
    rw [h1, h2]; ring
  have hq : (n : ℚ) / d = (a : ℚ) / b := by
    rw [div_eq_div_iff (by exact_mod_cast hd.ne') (by exact_mod_cast hbpos.ne')]
    exact_mod_cast hcross
  rw [hna, hdb, hq]
  exact ⟨Rat.num_div_eq_of_coprime hbpos hcop, Rat.den_div_eq_of_coprime hbpos hcop⟩

private theorem tf_some (x : I128) (h : fitsI64 x.val = true) :
    ∃ y, I64.Insts.CoreConvertTryFromI128TryFromIntError.try_from x =
      ok (core.result.Result.Ok y) ∧ y.val = x.val := by
  unfold I64.Insts.CoreConvertTryFromI128TryFromIntError.try_from
  split_ifs with h1
  · exact ⟨_, rfl, I64.ofInt_val_eq _⟩
  · exfalso; simp [fitsI64] at h h1; omega

private theorem tf_none (x : I128) (h : fitsI64 x.val = false) :
    I64.Insts.CoreConvertTryFromI128TryFromIntError.try_from x =
      ok (core.result.Result.Err ()) := by
  unfold I64.Insts.CoreConvertTryFromI128TryFromIntError.try_from
  split_ifs with h1
  · exfalso; simp [fitsI64] at h h1; omega
  · rfl

/-- The divisions by the gcd succeed, and storing their results is `represent`. -/
private theorem tail (n1 d1 g : I128) (hd : 0 < d1.val) (hgv : g.val = (Int.gcd n1.val d1.val : ℤ)) :
    ∃ q1 q2 : I128, core.num.checked_div_IScalar n1 g = some q1 ∧
      core.num.checked_div_IScalar d1 g = some q2 ∧
      represent ((n1.val : ℚ) / d1.val) =
        (if fitsI64 q1.val && fitsI64 q2.val then some (q1.val, q2.val) else none) := by
  have hgpos : 0 < g.val := by
    rw [hgv]; exact_mod_cast Int.gcd_pos_of_ne_zero_right _ (ne_of_gt hd)
  have h1 := I128.checked_div_bv_spec n1 g
  have h2 := I128.checked_div_bv_spec d1 g
  cases hq1 : core.num.checked_div_IScalar n1 g with
  | none => rw [hq1] at h1; omega
  | some q1 =>
  cases hq2 : core.num.checked_div_IScalar d1 g with
  | none => rw [hq2] at h2; omega
  | some q2 =>
  rw [hq1] at h1
  rw [hq2] at h2
  obtain ⟨-, -, hq1v, -⟩ := h1
  obtain ⟨-, -, hq2v, -⟩ := h2
  refine ⟨q1, q2, rfl, rfl, ?_⟩
  obtain ⟨hnum, hden⟩ := reduce n1.val d1.val hd
  have e1 : q1.val = n1.val / (Int.gcd n1.val d1.val : ℤ) := by
    rw [hq1v, hgv, Int.tdiv_eq_ediv_of_dvd (Int.gcd_dvd_left _ _)]
  have e2 : q2.val = d1.val / (Int.gcd n1.val d1.val : ℤ) := by
    rw [hq2v, hgv, Int.tdiv_eq_ediv_of_dvd (Int.gcd_dvd_right _ _)]
  simp only [represent, hnum, hden, ← e1, ← e2]

set_option hygiene false in
/-- Evaluate the extracted `normalize` from the per-operation facts in scope. -/
local macro "eval_normalize" "[" extra:Lean.Parser.Tactic.simpLemma,* "]" : tactic => `(tactic|
  simp [exact_rational.Rational.normalize, hdne, I128.checked_div, hq1, hq2,
    core.result.Result.ok, lift, core.option.Option.Insts.CoreOpsTry_traitTry.branch,
    core.option.Option.Insts.CoreOpsTry_traitFromResidualOptionInfallible.from_residual,
    $extra,*])

set_option hygiene false in
/-- Close the four fit/no-fit cases of the two stored components. -/
local macro "finish_normalize" "[" extra:Lean.Parser.Tactic.simpLemma,* "]" : tactic => `(tactic|
  (cases hf1 : fitsI64 q1.val <;> cases hf2 : fitsI64 q2.val
   · exact ⟨none, by eval_normalize [tf_none q1 hf1, $extra,*], by simp⟩
   · exact ⟨none, by eval_normalize [tf_none q1 hf1, $extra,*], by simp⟩
   · obtain ⟨y1, hy1, -⟩ := tf_some q1 hf1
     exact ⟨none, by eval_normalize [hy1, tf_none q2 hf2, $extra,*], by simp⟩
   · obtain ⟨y1, hy1, hy1v⟩ := tf_some q1 hf1
     obtain ⟨y2, hy2, hy2v⟩ := tf_some q2 hf2
     exact ⟨some ⟨y1, y2⟩, by eval_normalize [hy1, hy2, $extra,*],
       by simp [components, hy1v, hy2v]⟩))

/-- EXACT-RATIONAL-1 correspondence for `normalize`, over every pair of `i128`. -/
theorem normalize_correct (n d : I128) :
    ∃ o, exact_rational.Rational.normalize n d = ok o ∧
      o.map components = normalizeSpec n.val d.val := by
  have hnb := n.hBounds
  have hdb := d.hBounds
  simp at hnb hdb
  by_cases hd0 : d.val = 0
  · have hz : d = 0#i128 := (ieq d 0#i128).mpr (by simpa using hd0)
    exact ⟨none, by simp [exact_rational.Rational.normalize, hz], by simp [normalizeSpec, hd0]⟩
  have hdne : ¬ d = 0#i128 := fun h => hd0 (by simpa using (ieq d 0#i128).mp h)
  by_cases hneg : d.val < 0
  · have hlt : d < 0#i128 := hneg
    by_cases hnmin : n.val = -2 ^ 127
    · refine ⟨none, ?_, by simp [normalizeSpec, hnmin]⟩
      simp [exact_rational.Rational.normalize, hdne, hlt, neg_none n hnmin,
        core.option.Option.Insts.CoreOpsTry_traitTry.branch,
        core.option.Option.Insts.CoreOpsTry_traitFromResidualOptionInfallible.from_residual]
    obtain ⟨n1, hn1, hn1v⟩ := neg_some n hnmin
    by_cases hdmin : d.val = -2 ^ 127
    · refine ⟨none, ?_, by simp [normalizeSpec, hdmin]⟩
      simp [exact_rational.Rational.normalize, hdne, hlt, hn1, neg_none d hdmin,
        core.option.Option.Insts.CoreOpsTry_traitTry.branch,
        core.option.Option.Insts.CoreOpsTry_traitFromResidualOptionInfallible.from_residual]
    obtain ⟨d1, hd1, hd1v⟩ := neg_some d hdmin
    have hn1min : n1.val ≠ -2 ^ 127 := by rw [hn1v]; omega
    obtain ⟨a, ha, hav⟩ := abs_some n1 hn1min
    obtain ⟨g, hg, hgv⟩ := gcd_loop _ a d1 rfl (by rw [hav]; exact abs_nonneg _) (by omega)
    have hgv' : g.val = (Int.gcd n1.val d1.val : ℤ) := by
      rw [hgv, hav]; simp [Int.gcd, Int.natAbs_abs]
    obtain ⟨q1, q2, hq1, hq2, hrep⟩ := tail n1 d1 g (by omega) hgv'
    have hspec : normalizeSpec n.val d.val = represent ((n1.val : ℚ) / d1.val) := by
      rw [normalizeSpec, if_neg (by omega), hn1v, hd1v]
      push_cast; rw [neg_div_neg_eq]
    rw [hspec, hrep]
    finish_normalize [hlt, hn1, hd1, ha, hg]
  · have hlt : ¬ d < 0#i128 := hneg
    by_cases hnmin : n.val = -2 ^ 127
    · refine ⟨none, ?_, by simp [normalizeSpec, hnmin]⟩
      simp [exact_rational.Rational.normalize, hdne, hlt, abs_none n hnmin,
        core.option.Option.Insts.CoreOpsTry_traitTry.branch,
        core.option.Option.Insts.CoreOpsTry_traitFromResidualOptionInfallible.from_residual]
    obtain ⟨a, ha, hav⟩ := abs_some n hnmin
    obtain ⟨g, hg, hgv⟩ := gcd_loop _ a d rfl (by rw [hav]; exact abs_nonneg _) (by omega)
    have hg1 : exact_rational.Rational.normalize_loop1 a d = ok (some g) := (loop1_eq a d).trans hg
    have hgv' : g.val = (Int.gcd n.val d.val : ℤ) := by
      rw [hgv, hav]; simp [Int.gcd, Int.natAbs_abs]
    obtain ⟨q1, q2, hq1, hq2, hrep⟩ := tail n d g (by omega) hgv'
    have hspec : normalizeSpec n.val d.val = represent ((n.val : ℚ) / d.val) := by
      rw [normalizeSpec, if_neg (by omega)]
    rw [hspec, hrep]
    finish_normalize [hlt, ha, hg1]

/-- `normalize` of operands that avoid its intermediate refusals stores `x / y`. -/
private theorem normalize_of (x y : I128) (hy : y.val ≠ 0) (hx : x.val ≠ -2 ^ 127)
    (hy' : y.val ≠ -2 ^ 127) (q : ℚ) (hq : (x.val : ℚ) / y.val = q) :
    ∃ o, exact_rational.Rational.normalize x y = ok o ∧ o.map components = represent q := by
  obtain ⟨o, ho, hspec⟩ := normalize_correct x y
  refine ⟨o, ho, ?_⟩
  rw [hspec, normalizeSpec, if_neg (by omega), hq]

private theorem from_val (x : I64) : (core.convert.num.FromI128I64.from x).val = x.val := by
  simp

private theorem i64_bounds (x : I64) : -2 ^ 63 ≤ x.val ∧ x.val < 2 ^ 63 := by
  have h := x.hBounds; simp at h; omega

/-- The product of a stored numerator-sized and a positive denominator-sized
    component never overflows `i128` and stays clear of `i128::MIN / 2`. -/
private theorem mul_some (x y : I64) (hy : 0 < y.val) :
    ∃ z, core.num.checked_mul_IScalar (core.convert.num.FromI128I64.from x)
        (core.convert.num.FromI128I64.from y) = some z ∧ z.val = x.val * y.val ∧
      -(2 ^ 126 - 2 ^ 63) ≤ z.val ∧ z.val ≤ 2 ^ 126 - 2 ^ 63 := by
  have hx := i64_bounds x
  have hyb := i64_bounds y
  have hb : -(2 ^ 126 - 2 ^ 63) ≤ x.val * y.val ∧ x.val * y.val ≤ 2 ^ 126 - 2 ^ 63 := by
    constructor <;> nlinarith
  have h := I128.checked_mul_bv_spec (core.convert.num.FromI128I64.from x)
    (core.convert.num.FromI128I64.from y)
  rw [from_val, from_val] at h
  cases hz : core.num.checked_mul_IScalar (core.convert.num.FromI128I64.from x)
      (core.convert.num.FromI128I64.from y) with
  | none => rw [hz] at h; simp [I128.min, I128.max, I128.numBits] at h; omega
  | some z =>
    rw [hz] at h
    obtain ⟨-, -, hzv, -⟩ := h
    exact ⟨z, rfl, hzv, by rw [hzv]; exact hb.1, by rw [hzv]; exact hb.2⟩

/-- A product of two positive denominator-sized components. -/
private theorem mul_pos_some (x y : I64) (hx : 0 < x.val) (hy : 0 < y.val) :
    ∃ z, core.num.checked_mul_IScalar (core.convert.num.FromI128I64.from x)
        (core.convert.num.FromI128I64.from y) = some z ∧ z.val = x.val * y.val ∧
      0 < z.val ∧ z.val ≤ 2 ^ 126 - 2 ^ 63 := by
  obtain ⟨z, hz, hzv, -, hb⟩ := mul_some x y hy
  exact ⟨z, hz, hzv, by rw [hzv]; positivity, hb⟩

private theorem add_some (a b : I128) (ha : -(2 ^ 126 - 2 ^ 63) ≤ a.val ∧ a.val ≤ 2 ^ 126 - 2 ^ 63)
    (hb : -(2 ^ 126 - 2 ^ 63) ≤ b.val ∧ b.val ≤ 2 ^ 126 - 2 ^ 63) :
    ∃ z, core.num.checked_add_IScalar a b = some z ∧ z.val = a.val + b.val := by
  have h := I128.checked_add_bv_spec a b
  cases hz : core.num.checked_add_IScalar a b with
  | none => rw [hz] at h; simp [I128.min, I128.max, I128.numBits] at h; omega
  | some z => rw [hz] at h; exact ⟨z, rfl, h.2.2.1⟩

private theorem sub_some (a b : I128) (ha : -(2 ^ 126 - 2 ^ 63) ≤ a.val ∧ a.val ≤ 2 ^ 126 - 2 ^ 63)
    (hb : -(2 ^ 126 - 2 ^ 63) ≤ b.val ∧ b.val ≤ 2 ^ 126 - 2 ^ 63) :
    ∃ z, core.num.checked_sub_IScalar a b = some z ∧ z.val = a.val - b.val := by
  have h := I128.checked_sub_bv_spec a b
  cases hz : core.num.checked_sub_IScalar a b with
  | none => rw [hz] at h; simp [I128.min, I128.max, I128.numBits] at h; omega
  | some z => rw [hz] at h; exact ⟨z, rfl, h.2.2.1⟩

set_option hygiene false in
/-- Evaluate an extracted operation from the per-operation facts in scope. -/
local macro "eval_op" "[" extra:Lean.Parser.Tactic.simpLemma,* "]" : tactic => `(tactic|
  simp [I128.checked_mul, I128.checked_add, I128.checked_sub, lift,
    core.option.Option.Insts.CoreOpsTry_traitTry.branch,
    core.option.Option.Insts.CoreOpsTry_traitFromResidualOptionInfallible.from_residual,
    $extra,*])

/-- `new` stores `n / d` and refuses a zero denominator. -/
theorem new_correct (n d : I64) :
    ∃ o, exact_rational.Rational.new n d = ok o ∧
      o.map components = (if d.val = 0 then none else represent ((n.val : ℚ) / d.val)) := by
  have hn := i64_bounds n
  have hd := i64_bounds d
  obtain ⟨o, ho, hspec⟩ := normalize_correct (core.convert.num.FromI128I64.from n)
    (core.convert.num.FromI128I64.from d)
  refine ⟨o, by simp [exact_rational.Rational.new, lift]; exact ho, ?_⟩
  rw [hspec, normalizeSpec, from_val, from_val]
  by_cases h0 : d.val = 0
  · simp [h0]
  · rw [if_neg (by omega), if_neg h0]

/-- Exact addition of stored rationals with positive denominators. -/
theorem checked_add_correct (r s : exact_rational.Rational)
    (hr : 0 < r.denominator.val) (hs : 0 < s.denominator.val) :
    ∃ o, exact_rational.Rational.checked_add r s = ok o ∧
      o.map components = represent (value r + value s) := by
  obtain ⟨a, ha, hav, hab⟩ := mul_some r.numerator s.denominator hs
  obtain ⟨b, hb, hbv, hbb⟩ := mul_some s.numerator r.denominator hr
  obtain ⟨c, hc, hcv⟩ := add_some a b hab hbb
  obtain ⟨e, he, hev, hepos, heb⟩ := mul_pos_some r.denominator s.denominator hr hs
  obtain ⟨o, ho, hrep⟩ := normalize_of c e (by omega) (by omega) (by omega)
    (value r + value s) (by
      rw [hcv, hav, hbv, hev, value, value]
      rw [div_add_div _ _ (by exact_mod_cast hr.ne') (by exact_mod_cast hs.ne')]
      push_cast; ring)
  exact ⟨o, by eval_op [exact_rational.Rational.checked_add, exact_rational.Rational.add,
    ha, hb, hc, he]; exact ho, hrep⟩

/-- Exact subtraction of stored rationals with positive denominators. -/
theorem checked_sub_correct (r s : exact_rational.Rational)
    (hr : 0 < r.denominator.val) (hs : 0 < s.denominator.val) :
    ∃ o, exact_rational.Rational.checked_sub r s = ok o ∧
      o.map components = represent (value r - value s) := by
  obtain ⟨a, ha, hav, hab⟩ := mul_some r.numerator s.denominator hs
  obtain ⟨b, hb, hbv, hbb⟩ := mul_some s.numerator r.denominator hr
  obtain ⟨c, hc, hcv⟩ := sub_some a b hab hbb
  obtain ⟨e, he, hev, hepos, heb⟩ := mul_pos_some r.denominator s.denominator hr hs
  obtain ⟨o, ho, hrep⟩ := normalize_of c e (by omega) (by omega) (by omega)
    (value r - value s) (by
      rw [hcv, hav, hbv, hev, value, value]
      rw [div_sub_div _ _ (by exact_mod_cast hr.ne') (by exact_mod_cast hs.ne')]
      push_cast; ring)
  exact ⟨o, by eval_op [exact_rational.Rational.checked_sub, ha, hb, hc, he]; exact ho, hrep⟩

/-- Exact multiplication of stored rationals with positive denominators. -/
theorem checked_mul_correct (r s : exact_rational.Rational)
    (hr : 0 < r.denominator.val) (hs : 0 < s.denominator.val) :
    ∃ o, exact_rational.Rational.checked_mul r s = ok o ∧
      o.map components = represent (value r * value s) := by
  have hnr := i64_bounds r.numerator
  have hns := i64_bounds s.numerator
  have hdr := i64_bounds r.denominator
  have hds := i64_bounds s.denominator
  have h := I128.checked_mul_bv_spec (core.convert.num.FromI128I64.from r.numerator)
    (core.convert.num.FromI128I64.from s.numerator)
  rw [from_val, from_val] at h
  cases ha : core.num.checked_mul_IScalar (core.convert.num.FromI128I64.from r.numerator)
      (core.convert.num.FromI128I64.from s.numerator) with
  | none =>
      rw [ha] at h
      have hlo : -2 ^ 126 ≤ r.numerator.val * s.numerator.val := by nlinarith
      have hhi : r.numerator.val * s.numerator.val ≤ 2 ^ 126 := by nlinarith
      simp [I128.min, I128.max, I128.numBits] at h
      omega
  | some a =>
  rw [ha] at h
  obtain ⟨-, -, hav, -⟩ := h
  have hab : -2 ^ 126 ≤ a.val ∧ a.val ≤ 2 ^ 126 := by rw [hav]; constructor <;> nlinarith
  obtain ⟨e, he, hev, hepos, heb⟩ := mul_pos_some r.denominator s.denominator hr hs
  obtain ⟨o, ho, hrep⟩ := normalize_of a e (by omega) (by omega) (by omega)
    (value r * value s) (by
      rw [hav, hev, value, value, div_mul_div_comm]; push_cast; ring)
  refine ⟨o, ?_, hrep⟩
  eval_op [exact_rational.Rational.checked_mul, exact_rational.Rational.mul, ha, he]
  exact ho

/-- Exact division of stored rationals with positive denominators; a zero
    divisor refuses. -/
theorem checked_div_correct (r s : exact_rational.Rational)
    (hr : 0 < r.denominator.val) (hs : 0 < s.denominator.val) :
    ∃ o, exact_rational.Rational.checked_div r s = ok o ∧
      o.map components = (if value s = 0 then none else represent (value r / value s)) := by
  obtain ⟨a, ha, hav, hab⟩ := mul_some r.numerator s.denominator hs
  obtain ⟨b, hb, hbv, hbb⟩ := mul_some s.numerator r.denominator hr
  have hb' : core.num.checked_mul_IScalar (core.convert.num.FromI128I64.from r.denominator)
      (core.convert.num.FromI128I64.from s.numerator) = some b ∧ b.val = r.denominator.val * s.numerator.val := by
    have h := I128.checked_mul_bv_spec (core.convert.num.FromI128I64.from r.denominator)
      (core.convert.num.FromI128I64.from s.numerator)
    have h' := I128.checked_mul_bv_spec (core.convert.num.FromI128I64.from s.numerator)
      (core.convert.num.FromI128I64.from r.denominator)
    rw [hb] at h'
    rw [from_val, from_val] at h h'
    cases hz : core.num.checked_mul_IScalar (core.convert.num.FromI128I64.from r.denominator)
        (core.convert.num.FromI128I64.from s.numerator) with
    | none => rw [hz] at h; exfalso; apply h; rw [mul_comm]; exact ⟨h'.1, h'.2.1⟩
    | some z =>
      rw [hz] at h
      refine ⟨?_, by rw [hbv, mul_comm]⟩
      congr 1
      apply (ieq z b).mpr
      rw [h.2.2.1, hbv, mul_comm]
  have hsv : value s = 0 ↔ s.numerator.val = 0 := by
    rw [value, div_eq_zero_iff]; simp [hs.ne']
  have hbz : b.val = 0 ↔ s.numerator.val = 0 := by
    rw [hbv]; constructor
    · intro h; rcases mul_eq_zero.mp h with h | h
      · exact h
      · omega
    · intro h; rw [h, zero_mul]
  obtain ⟨o, ho, hspec⟩ := normalize_correct a b
  refine ⟨o, by eval_op [exact_rational.Rational.checked_div, ha, hb'.1]; exact ho, ?_⟩
  rw [hspec, normalizeSpec]
  by_cases h0 : s.numerator.val = 0
  · rw [if_pos (Or.inl (hbz.mpr h0)), if_pos (hsv.mpr h0)]
  · rw [if_neg (by have := hbz.not.mpr h0; omega), if_neg (fun h => h0 (hsv.mp h))]
    congr 1
    rw [hav, hb'.2, value, value, div_div_div_eq]; push_cast; ring

/-- Exact negation of a stored rational with a positive denominator. -/
theorem checked_neg_correct (r : exact_rational.Rational) (hr : 0 < r.denominator.val) :
    ∃ o, exact_rational.Rational.checked_neg r = ok o ∧
      o.map components = represent (-value r) := by
  have hn := i64_bounds r.numerator
  have hd := i64_bounds r.denominator
  obtain ⟨m, hm, hmv⟩ := neg_some (core.convert.num.FromI128I64.from r.numerator)
    (by rw [from_val]; omega)
  obtain ⟨o, ho, hrep⟩ := normalize_of m (core.convert.num.FromI128I64.from r.denominator)
    (by rw [from_val]; omega) (by rw [hmv, from_val]; omega) (by rw [from_val]; omega)
    (-value r) (by rw [hmv, from_val, from_val, value]; push_cast; ring)
  exact ⟨o, by eval_op [exact_rational.Rational.checked_neg, hm]; exact ho, hrep⟩

/-- Exact comparison of stored rationals with positive denominators. -/
theorem cmp_correct (r s : exact_rational.Rational)
    (hr : 0 < r.denominator.val) (hs : 0 < s.denominator.val) :
    exact_rational.Rational.cmp r s = ok (some (compare (value r) (value s))) := by
  obtain ⟨a, ha, hav, -⟩ := mul_some r.numerator s.denominator hs
  obtain ⟨b, hb, hbv, -⟩ := mul_some s.numerator r.denominator hr
  have key : compare a.val b.val = compare (value r) (value s) := by
    have hr' : (0 : ℚ) < r.denominator.val := by exact_mod_cast hr
    have hs' : (0 : ℚ) < s.denominator.val := by exact_mod_cast hs
    have hlt : value r < value s ↔ a.val < b.val := by
      rw [value, value, div_lt_div_iff₀ hr' hs', hav, hbv]; norm_cast
    have heq : value r = value s ↔ a.val = b.val := by
      rw [value, value, div_eq_div_iff hr'.ne' hs'.ne', hav, hbv]; norm_cast
    rcases lt_trichotomy a.val b.val with h | h | h
    · rw [compare_lt_iff_lt.mpr h, compare_lt_iff_lt.mpr (hlt.mpr h)]
    · rw [compare_eq_iff_eq.mpr h, compare_eq_iff_eq.mpr (heq.mpr h)]
    · have h2 : value s < value r := by
        have := (show value s < value r ↔ b.val < a.val by
          rw [value, value, div_lt_div_iff₀ hs' hr', hav, hbv]; norm_cast)
        exact this.mpr h
      rw [compare_gt_iff_gt.mpr h, compare_gt_iff_gt.mpr h2]
  simp [exact_rational.Rational.cmp, I128.checked_mul, ha, hb, lift,
    core.option.Option.Insts.CoreOpsTry_traitTry.branch, key]

end Norn.ExactRational
#print axioms Norn.ExactRational.normalize_correct
#print axioms Norn.ExactRational.new_correct
#print axioms Norn.ExactRational.checked_add_correct
#print axioms Norn.ExactRational.checked_sub_correct
#print axioms Norn.ExactRational.checked_mul_correct
#print axioms Norn.ExactRational.checked_div_correct
#print axioms Norn.ExactRational.checked_neg_correct
#print axioms Norn.ExactRational.cmp_correct
