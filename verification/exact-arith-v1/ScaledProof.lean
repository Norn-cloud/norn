import QuotientProof
import ScaledReference

/-!
# Production scaled core meets FOLD-SCALED / VAL-ROUND / VAL-EXACT-1

`Scaled` is the fresh Aeneas translation of the scaled-arithmetic core of
`vendor/zen/core/expression/src/scaled.rs`, over the `rust_decimal` and core
models. Each theorem covers every input of the extracted function.
-/

namespace Norn.OrderedFold
open Aeneas Aeneas.Std Result
open zen_expression

/-- A decimal result as its stored `(coefficient, places)`; `Err` is refusal. -/
def projDec {E : Type} : core.result.Result rust_decimal.decimal.Decimal E → Option (ℤ × ℕ)
  | .Ok d => some (d.coeff, d.places)
  | .Err _ => none

/-- An integer result; `Err` is refusal. -/
def projInt {E : Type} : core.result.Result Std.I128 E → Option ℤ
  | .Ok v => some v.val
  | .Err _ => none

/-- The value a modelled decimal denotes. -/
def decOf (d : rust_decimal.decimal.Decimal) : ℚ := decValue d.coeff d.places

private theorem scale_eval (d : rust_decimal.decimal.Decimal) :
    ∃ s, rust_decimal.decimal.Decimal.scale d = ok s ∧ s.val = d.places := by
  refine ⟨_, rfl, ?_⟩
  have := d.places_bound
  show (BitVec.ofNat 32 d.places).toNat = d.places
  rw [BitVec.toNat_ofNat]
  exact Nat.mod_eq_of_lt (by omega)

private theorem mantissa_eval (d : rust_decimal.decimal.Decimal) :
    ∃ m, rust_decimal.decimal.Decimal.mantissa d = ok m ∧ m.val = d.coeff :=
  ⟨_, rfl, I128.ofInt_val_eq _⟩

/-- `stripZeros` returns the canonical form: same value, no trailing zero. -/
private theorem strip_spec (m : ℤ) (s : ℕ) :
    (rust_decimal.decimal.stripZeros m s).2 ≤ s ∧
      (rust_decimal.decimal.stripZeros m s).1 * 10 ^ (s - (rust_decimal.decimal.stripZeros m s).2) = m ∧
      (0 < (rust_decimal.decimal.stripZeros m s).2 →
        ¬ (10 : ℤ) ∣ (rust_decimal.decimal.stripZeros m s).1) := by
  induction s generalizing m with
  | zero => simp [rust_decimal.decimal.stripZeros]
  | succ s ih =>
      unfold rust_decimal.decimal.stripZeros
      split_ifs with h
      · obtain ⟨h1, h2, h3⟩ := ih (m / 10)
        refine ⟨by omega, ?_, h3⟩
        have hs : s + 1 - (rust_decimal.decimal.stripZeros (m / 10) s).2 =
            (s - (rust_decimal.decimal.stripZeros (m / 10) s).2) + 1 := by omega
        rw [hs, pow_succ, ← mul_assoc, h2]
        exact Int.ediv_mul_cancel (Int.dvd_of_emod_eq_zero h)
      · dsimp only
        exact ⟨le_refl _, by simp, fun _ hd => h (Int.emod_eq_zero_of_dvd hd)⟩

private theorem normalize_eval (d : rust_decimal.decimal.Decimal) :
    ∃ d', rust_decimal.decimal.Decimal.normalize d = ok d' ∧
      d'.places ≤ d.places ∧ d'.coeff * 10 ^ (d.places - d'.places) = d.coeff ∧
      (0 < d'.places → ¬ (10 : ℤ) ∣ d'.coeff) := by
  refine ⟨_, rfl, strip_spec d.coeff d.places⟩

/-- A canonical decimal is exact at scale `t` exactly when `t` covers its places. -/
private theorem exact_at (c : ℤ) (s t : ℕ) (hcan : 0 < s → ¬ (10 : ℤ) ∣ c) :
    ((decValue c s * 10 ^ t).den = 1 ↔ s ≤ t) ∧
      (s ≤ t → decValue c s * 10 ^ t = ((c * 10 ^ (t - s) : ℤ) : ℚ)) := by
  have hval : s ≤ t → decValue c s * 10 ^ t = ((c * 10 ^ (t - s) : ℤ) : ℚ) := by
    intro h
    rw [decValue]; push_cast
    rw [pow_sub₀ (10 : ℚ) (by norm_num) h]
    ring
  refine ⟨⟨fun hden => ?_, fun h => by rw [hval h]; exact Rat.den_intCast _⟩, hval⟩
  by_contra hle
  have hlt : t < s := lt_of_not_ge hle
  have hq := Rat.num_div_den (decValue c s * 10 ^ t)
  rw [hden] at hq
  simp only [Nat.cast_one, div_one] at hq
  set z := (decValue c s * 10 ^ t).num
  have hz : (c : ℚ) = z * 10 ^ (s - t) := by
    have e : decValue c s * 10 ^ t * 10 ^ (s - t) = c := by
      rw [decValue, mul_assoc, ← pow_add, Nat.add_sub_cancel' hlt.le]
      field_simp
    rw [← e, ← hq]
  have hzc : c = z * 10 ^ (s - t) := by exact_mod_cast hz
  apply hcan (by omega)
  rw [hzc]
  obtain ⟨k, hk⟩ : ∃ k, s - t = k + 1 := ⟨s - t - 1, by omega⟩
  rw [hk, pow_succ]
  exact dvd_mul_of_dvd_right (dvd_mul_left 10 _) z

/-- Normalizing preserves a decimal's value. -/
private theorem value_preserved (c c' : ℤ) (s s' : ℕ) (hle : s' ≤ s)
    (h : c' * 10 ^ (s - s') = c) : decValue c' s' = decValue c s := by
  rw [decValue, decValue, ← h]; push_cast
  rw [pow_sub₀ (10 : ℚ) (by norm_num) hle]
  field_simp

private theorem pow_eval (n : U32) (h : n.val ≤ 38) :
    ∃ p, core.num.I128.pow 10#i128 n = ok p ∧ p.val = 10 ^ n.val := by
  have hb : (10 : ℤ) ^ n.val ≤ 10 ^ 38 := pow_le_pow_right₀ (by norm_num) h
  have hp : (0 : ℤ) < 10 ^ n.val := by positivity
  unfold core.num.I128.pow
  rw [dif_pos (by simp; constructor <;> omega)]
  exact ⟨_, rfl, by rw [I128.ofInt_val_eq]; simp⟩

private theorem ieq (a b : I128) : a = b ↔ a.val = b.val := IScalar.eq_equiv a b

private theorem fits_iff (z : ℤ) : fitsI128 z = true ↔ I128.min ≤ z ∧ z ≤ I128.max := by
  simp [fitsI128, I128.min, I128.max, I128.numBits]; omega

/-- `to_decimal` stores exactly what `rust_decimal` can hold. -/
theorem to_decimal_correct (c : I128) (s : U32) :
    ∃ r, scaled.to_decimal c s = ok r ∧ projDec r = store c.val s.val := by
  by_cases h : s.val ≤ 28 ∧ -2 ^ 96 < c.val ∧ c.val < 2 ^ 96
  · refine ⟨.Ok ⟨c.val, s.val, h.2, h.1⟩, ?_, ?_⟩
    · simp only [scaled.to_decimal, rust_decimal.decimal.Decimal.try_from_i128_with_scale,
        dif_pos h]
      simp [core.result.Result.ok, core.option.Option.ok_or]
    · simp only [projDec, store]; rw [if_pos h]
  · refine ⟨.Err .DecimalRange, ?_, ?_⟩
    · simp only [scaled.to_decimal, rust_decimal.decimal.Decimal.try_from_i128_with_scale,
        dif_neg h]
      split_ifs <;> simp [core.result.Result.ok, core.option.Option.ok_or]
    · simp only [projDec, store]; rw [if_neg h]

/-- FOLD-SCALED correspondence for `coefficient`, over every `u32` scale and decimal. -/
theorem coefficient_correct (t : U32) (v : rust_decimal.decimal.Decimal) :
    ∃ r, scaled.coefficient t v = ok r ∧ projInt r = coefficientSpec t.val (decOf v) := by
  obtain ⟨d', hn, hle, hval, hcan⟩ := normalize_eval v
  have hv : decOf v = decValue d'.coeff d'.places :=
    (value_preserved v.coeff d'.coeff v.places d'.places hle hval).symm
  have hdb := d'.coeff_bound
  have hpb := d'.places_bound
  by_cases ht : t.val ≤ 18
  · have hgt : ¬ t > 18#u32 := by
      intro h; change (18#u32).val < t.val at h; simp at h; omega
    obtain ⟨s', hs, hsv⟩ := scale_eval d'
    have hsub := U32.checked_sub_bv_spec t s'
    cases hk : U32.checked_sub t s' with
    | none =>
        rw [hk] at hsub
        refine ⟨.Err .OutsideDeclaredScale, ?_, ?_⟩
        · simp [scaled.coefficient, hn, hgt, hs, hk, lift, core.option.Option.ok_or,
            core.result.Result.Insts.CoreOpsTry.branch,
            core.result.Result.Insts.CoreOpsTryTraitFromResidualResultInfallible.from_residual]
        · have hnot : ¬ d'.places ≤ t.val := by omega
          simp only [projInt, coefficientSpec, hv, (exact_at _ _ _ hcan).1, hnot,
            and_false, false_and, if_false]
    | some k =>
        rw [hk] at hsub
        obtain ⟨hle', hkv, -⟩ := hsub
        rw [hsv] at hle' hkv
        obtain ⟨p, hp, hpv⟩ := pow_eval k (by omega)
        obtain ⟨m, hm, hmv⟩ := mantissa_eval d'
        have hmul := I128.checked_mul_bv_spec m p
        have hexact := (exact_at d'.coeff d'.places t.val hcan).2 hle'
        cases hz : core.num.checked_mul_IScalar m p with
        | none =>
            rw [hz] at hmul
            refine ⟨.Err .ScaledCoefficientOverflow, ?_, ?_⟩
            · simp [scaled.coefficient, hn, hgt, hs, hk, hm, hp, I128.checked_mul, hz, lift,
                core.option.Option.ok_or, core.result.Result.Insts.CoreOpsTry.branch]
            · rw [hmv, hpv, hkv] at hmul
              have hf : fitsI128 (d'.coeff * 10 ^ (t.val - d'.places)) = false := by
                cases h : fitsI128 (d'.coeff * 10 ^ (t.val - d'.places))
                · rfl
                · exact absurd ((fits_iff _).mp h) hmul
              rw [projInt, coefficientSpec, hv, hexact, Rat.den_intCast, Rat.num_intCast, hf]
              simp
        | some z =>
            rw [hz] at hmul
            obtain ⟨hlo, hhi, hzv, -⟩ := hmul
            rw [hmv, hpv, hkv] at hlo hhi hzv
            refine ⟨.Ok z, ?_, ?_⟩
            · simp [scaled.coefficient, hn, hgt, hs, hk, hm, hp, I128.checked_mul, hz, lift,
                core.option.Option.ok_or, core.result.Result.Insts.CoreOpsTry.branch]
            · have hf : fitsI128 (d'.coeff * 10 ^ (t.val - d'.places)) = true :=
                (fits_iff _).mpr ⟨hlo, hhi⟩
              rw [projInt, coefficientSpec, hv, hexact, Rat.den_intCast, Rat.num_intCast, hf,
                if_pos ⟨ht, rfl, rfl⟩, hzv]
  · have hgt : t > 18#u32 := by change (18#u32).val < t.val; simp; omega
    refine ⟨.Err .OutsideDeclaredScale, ?_, by simp [projInt, coefficientSpec, ht]⟩
    simp [scaled.coefficient, hn, hgt]

/-- The opcode an extracted `ArithmeticOp` names. -/
def opOf : scaled.ArithmeticOp → Op
  | .Add => .add
  | .Subtract => .subtract
  | .Multiply => .multiply
  | .Divide => .divide
  | .Other => .other

/-- VAL-EXACT-1 correspondence for `exact_result_scale`, over every pair of `u32`. -/
theorem exact_result_scale_correct (op : scaled.ArithmeticOp) (a b : U32) :
    ∃ r, scaled.exact_result_scale op a b = ok r ∧
      (match r with | .Ok v => some v.val | .Err _ => none) =
        exactResultScaleSpec (opOf op) a.val b.val := by
  cases op with
  | Add | Subtract =>
      by_cases hab : b.val ≤ a.val
      · have hge : a ≥ b := hab
        have hm : max a.val b.val = a.val := max_eq_left hab
        by_cases h18 : a.val ≤ 18
        · exact ⟨.Ok a, by simp [scaled.exact_result_scale, hge, h18],
            by simp [exactResultScaleSpec, opOf, hm, h18]⟩
        · exact ⟨.Err .ExactScaleExceeded,
            by simp [scaled.exact_result_scale, hge, lt_of_not_ge h18],
            by simp [exactResultScaleSpec, opOf, hm, h18]⟩
      · have hge : ¬ a ≥ b := hab
        have hm : max a.val b.val = b.val := max_eq_right (by omega)
        by_cases h18 : b.val ≤ 18
        · exact ⟨.Ok b, by simp [scaled.exact_result_scale, hge, h18],
            by simp [exactResultScaleSpec, opOf, hm, h18]⟩
        · exact ⟨.Err .ExactScaleExceeded,
            by simp [scaled.exact_result_scale, hge, lt_of_not_ge h18],
            by simp [exactResultScaleSpec, opOf, hm, h18]⟩
  | Multiply =>
      have hadd := U32.checked_add_bv_spec a b
      cases hz : U32.checked_add a b with
      | none =>
          rw [hz] at hadd
          refine ⟨.Err .ExactScaleOverflow, ?_, ?_⟩
          · simp [scaled.exact_result_scale, hz, lift, core.option.Option.ok_or,
              core.result.Result.Insts.CoreOpsTry.branch,
              core.result.Result.Insts.CoreOpsTryTraitFromResidualResultInfallible.from_residual]
          · simp [U32.max, U32.numBits] at hadd
            simp [exactResultScaleSpec, opOf]; omega
      | some z =>
          rw [hz] at hadd
          obtain ⟨-, hzv, -⟩ := hadd
          by_cases h18 : z.val ≤ 18
          · exact ⟨.Ok z, by simp [scaled.exact_result_scale, hz, lift, core.option.Option.ok_or,
              core.result.Result.Insts.CoreOpsTry.branch, h18],
              by simp [exactResultScaleSpec, opOf]; omega⟩
          · exact ⟨.Err .ExactScaleExceeded, by simp [scaled.exact_result_scale, hz, lift,
              core.option.Option.ok_or, core.result.Result.Insts.CoreOpsTry.branch,
              lt_of_not_ge h18], by simp [exactResultScaleSpec, opOf]; omega⟩
  | Divide => exact ⟨.Err .NeedsRoundingSite, rfl, rfl⟩
  | Other => exact ⟨.Err .NeedsRoundingSite, rfl, rfl⟩

/-- A value outside `i128` is outside `rust_decimal`'s range too. -/
private theorem store_of_not_fits (c : ℤ) (s : ℕ) (h : fitsI128 c = false) : store c s = none := by
  simp [fitsI128] at h
  simp only [store]
  rw [if_neg]
  omega

private theorem not_fits_of (z : ℤ) (h : ¬ (I128.min ≤ z ∧ z ≤ I128.max)) : fitsI128 z = false := by
  cases hf : fitsI128 z
  · rfl
  · exact absurd ((fits_iff z).mp hf) h

/-- Rescaling by `10^t` a canonical decimal with more places divides it. -/
private theorem shift_down (c : ℤ) (s t : ℕ) (h : t < s) :
    decValue c s * 10 ^ t = (c : ℚ) / ((10 ^ (s - t) : ℤ) : ℚ) := by
  rw [decValue]; push_cast
  rw [pow_sub₀ (10 : ℚ) (by norm_num) h.le]
  field_simp

/-- FOLD-RESCALE / VAL-ROUND correspondence for `rescale`, over every site and decimal. -/
theorem rescale_correct (profile : scaled.ScaledArithmetic) (v : rust_decimal.decimal.Decimal) :
    ∃ r, scaled.rescale profile v = ok r ∧
      projDec r = rescaleSpec (modeOf profile.rounding) profile.scale.val (decOf v) := by
  by_cases ht : profile.scale.val ≤ 18
  swap
  · have hgt : profile.scale > 18#u32 := by change (18#u32).val < _; simp; omega
    exact ⟨.Err .OutsideDeclaredScale, by simp [scaled.rescale, hgt],
      by simp [projDec, rescaleSpec, ht]⟩
  have hgt : ¬ profile.scale > 18#u32 := by
    intro h; change (18#u32).val < _ at h; simp at h; omega
  obtain ⟨d', hn, hle, hval, hcan⟩ := normalize_eval v
  have hv : decOf v = decValue d'.coeff d'.places :=
    (value_preserved v.coeff d'.coeff v.places d'.places hle hval).symm
  have hpb := d'.places_bound
  obtain ⟨sv, hs, hsv⟩ := scale_eval d'
  obtain ⟨m, hm, hmv⟩ := mantissa_eval d'
  by_cases hge : d'.places ≤ profile.scale.val
  · have hge' : sv.val ≤ profile.scale.val := by omega
    obtain ⟨k, hk, hkv, -⟩ := WP.spec_imp_exists (U32.sub_spec (x := profile.scale) (y := sv)
      (by omega))
    rw [hsv] at hkv
    obtain ⟨p, hp, hpv⟩ := pow_eval k (by omega)
    have hexact := (exact_at d'.coeff d'.places profile.scale.val hcan).2 hge
    have hspec : rescaleSpec (modeOf profile.rounding) profile.scale.val (decOf v) =
        store (d'.coeff * 10 ^ (profile.scale.val - d'.places)) profile.scale.val := by
      rw [rescaleSpec, if_pos ht, hv, hexact, roundRat_int]; rfl
    rw [hspec]
    have hmul := I128.checked_mul_bv_spec m p
    cases hz : core.num.checked_mul_IScalar m p with
    | none =>
        rw [hz, hmv, hpv, hkv] at hmul
        refine ⟨.Err .ScaledCoefficientOverflow, ?_, ?_⟩
        · simp [scaled.rescale, hgt, hn, hs, hge', hm, hk, hp, I128.checked_mul, hz, lift,
            core.option.Option.ok_or, core.result.Result.Insts.CoreOpsTry.branch,
            core.result.Result.Insts.CoreOpsTryTraitFromResidualResultInfallible.from_residual]
        · rw [store_of_not_fits _ _ (not_fits_of _ hmul)]; rfl
    | some z =>
        rw [hz] at hmul
        obtain ⟨-, -, hzv, -⟩ := hmul
        rw [hmv, hpv, hkv] at hzv
        obtain ⟨r, hr, hrv⟩ := to_decimal_correct z profile.scale
        refine ⟨r, ?_, by rw [hrv, hzv]⟩
        simp [scaled.rescale, hgt, hn, hs, hge', hm, hk, hp, I128.checked_mul, hz, lift,
          core.option.Option.ok_or, core.result.Result.Insts.CoreOpsTry.branch]
        exact hr
  · have hge' : ¬ sv.val ≤ profile.scale.val := by omega
    obtain ⟨k, hk, hkv, -⟩ := WP.spec_imp_exists (U32.sub_spec (x := sv) (y := profile.scale)
      (by omega))
    rw [hsv] at hkv
    obtain ⟨p, hp, hpv⟩ := pow_eval k (by omega)
    obtain ⟨q, hq, hqv⟩ := rounded_quotient_correct profile.rounding m p
    have hd0 : p.val ≠ 0 := by rw [hpv]; positivity
    have hshift := shift_down d'.coeff d'.places profile.scale.val (by omega)
    have hqspec : quantize (modeOf profile.rounding) m.val p.val =
        (roundRat (modeOf profile.rounding) (decOf v * 10 ^ profile.scale.val)).filter fitsI128 := by
      rw [quantize, if_neg hd0, hv, hshift, hmv, hpv, hkv]
    rw [hqspec] at hqv
    cases q with
    | Ok z =>
        obtain ⟨r, hr, hrv⟩ := to_decimal_correct z profile.scale
        refine ⟨r, ?_, ?_⟩
        · simp [scaled.rescale, hgt, hn, hs, hge', hm, hk, hp,
            scaled.ScaledArithmetic.quotient, hq, core.result.Result.Insts.CoreOpsTry.branch]
          exact hr
        · rw [hrv, rescaleSpec, if_pos ht]
          simp only [project] at hqv
          cases hround : roundRat (modeOf profile.rounding) (decOf v * 10 ^ profile.scale.val) with
          | none => rw [hround] at hqv; simp at hqv
          | some c0 =>
              rw [hround] at hqv
              simp [Option.filter] at hqv
              obtain ⟨-, hc⟩ := hqv
              simp [← hc]
    | Err e =>
        refine ⟨.Err (.Quotient e), ?_, ?_⟩
        · simp [scaled.rescale, hgt, hn, hs, hge', hm, hk, hp,
            scaled.ScaledArithmetic.quotient, hq, core.result.Result.Insts.CoreOpsTry.branch,
            core.result.Result.Insts.CoreOpsTryTraitFromResidualResultInfallible.from_residual,
            scaled.ScaledRefusal.Insts.CoreConvertFromQuotientRefusal.from]
        · rw [rescaleSpec, if_pos ht]
          simp only [project] at hqv
          cases hround : roundRat (modeOf profile.rounding) (decOf v * 10 ^ profile.scale.val) with
          | none => rfl
          | some c0 =>
              rw [hround] at hqv
              simp only [Option.filter] at hqv
              split_ifs at hqv with hf
              have hf' : fitsI128 c0 = false := by simpa using hf
              simp [projDec, store_of_not_fits _ _ hf']

set_option hygiene false in
/-- Evaluate an extracted scaled function from the facts in scope. -/
local macro "evalS" "[" extra:Lean.Parser.Tactic.simpLemma,* "]" : tactic => `(tactic|
  simp [lift, core.option.Option.ok_or, core.result.Result.Insts.CoreOpsTry.branch,
    core.result.Result.Insts.CoreOpsTryTraitFromResidualResultInfallible.from_residual,
    scaled.ScaledRefusal.Insts.CoreConvertFromQuotientRefusal.from,
    scaled.ScaledArithmetic.quotient, I128.checked_add, I128.checked_sub, I128.checked_mul,
    $extra,*])

/-- The scale-alignment step every exact operation starts with. -/
private theorem scales_eval (op : scaled.ArithmeticOp) (a b : rust_decimal.decimal.Decimal) :
    ∃ sa sb r, rust_decimal.decimal.Decimal.scale a = ok sa ∧
      rust_decimal.decimal.Decimal.scale b = ok sb ∧
      scaled.exact_result_scale op sa sb = ok r ∧
      (match r with | .Ok v => some v.val | .Err _ => none) =
        exactResultScaleSpec (opOf op) a.places b.places := by
  obtain ⟨sa, hsa, hsav⟩ := scale_eval a
  obtain ⟨sb, hsb, hsbv⟩ := scale_eval b
  obtain ⟨r, hr, hrv⟩ := exact_result_scale_correct op sa sb
  rw [hsav, hsbv] at hrv
  exact ⟨sa, sb, r, hsa, hsb, hr, hrv⟩

/-- VAL-EXACT-1 correspondence for `exact_compare`, over every pair of decimals. -/
theorem exact_compare_correct (a b : rust_decimal.decimal.Decimal) :
    ∃ r, scaled.exact_compare a b = ok r ∧
      (match r with | .Ok o => some o | .Err _ => none) =
        exactCompareSpec a.coeff a.places b.coeff b.places := by
  obtain ⟨sa, sb, r0, hsa, hsb, hr0, hr0v⟩ := scales_eval .Add a b
  cases r0 with
  | Err e =>
      refine ⟨.Err e, by evalS [scaled.exact_compare, hsa, hsb, hr0], ?_⟩
      simp only [opOf] at hr0v
      simp only [exactCompareSpec]; rw [← hr0v]
  | Ok s =>
      simp only at hr0v
      have hspec : exactCompareSpec a.coeff a.places b.coeff b.places =
          match coefficientSpec s.val (decOf a), coefficientSpec s.val (decOf b) with
          | some x, some y => some (compare x y)
          | _, _ => none := by
        simp only [exactCompareSpec, opOf] at hr0v ⊢; rw [← hr0v]; rfl
      obtain ⟨ra, hra, hrav⟩ := coefficient_correct s a
      obtain ⟨rb, hrb, hrbv⟩ := coefficient_correct s b
      rw [hspec, ← hrav, ← hrbv]
      cases ra with
      | Err e => exact ⟨.Err e, by evalS [scaled.exact_compare, hsa, hsb, hr0, hra], rfl⟩
      | Ok x =>
        cases rb with
        | Err e => exact ⟨.Err e, by evalS [scaled.exact_compare, hsa, hsb, hr0, hra, hrb], rfl⟩
        | Ok y =>
            exact ⟨.Ok (compare x.val y.val),
              by evalS [scaled.exact_compare, hsa, hsb, hr0, hra, hrb], rfl⟩

private theorem checked_eval_add (x y : I128) :
    ∃ o, core.num.checked_add_IScalar x y = o ∧
      (match o with | some z => some z.val | none => none) =
        (if fitsI128 (x.val + y.val) = true then some (x.val + y.val) else none) := by
  have h := I128.checked_add_bv_spec x y
  cases hz : core.num.checked_add_IScalar x y with
  | none => rw [hz] at h; exact ⟨none, rfl, by rw [not_fits_of _ h]; rfl⟩
  | some z =>
      rw [hz] at h
      exact ⟨some z, rfl, by simp [(fits_iff _).mpr ⟨h.1, h.2.1⟩, h.2.2.1]⟩

private theorem checked_eval_sub (x y : I128) :
    ∃ o, core.num.checked_sub_IScalar x y = o ∧
      (match o with | some z => some z.val | none => none) =
        (if fitsI128 (x.val - y.val) = true then some (x.val - y.val) else none) := by
  have h := I128.checked_sub_bv_spec x y
  cases hz : core.num.checked_sub_IScalar x y with
  | none => rw [hz] at h; exact ⟨none, rfl, by rw [not_fits_of _ h]; rfl⟩
  | some z =>
      rw [hz] at h
      exact ⟨some z, rfl, by simp [(fits_iff _).mpr ⟨h.1, h.2.1⟩, h.2.2.1]⟩

private theorem checked_eval_mul (x y : I128) :
    ∃ o, core.num.checked_mul_IScalar x y = o ∧
      (match o with | some z => some z.val | none => none) =
        (if fitsI128 (x.val * y.val) = true then some (x.val * y.val) else none) := by
  have h := I128.checked_mul_bv_spec x y
  cases hz : core.num.checked_mul_IScalar x y with
  | none => rw [hz] at h; exact ⟨none, rfl, by rw [not_fits_of _ h]; rfl⟩
  | some z =>
      rw [hz] at h
      exact ⟨some z, rfl, by simp [(fits_iff _).mpr ⟨h.1, h.2.1⟩, h.2.2.1]⟩

/-- Storing a checked result: refusal when the checked operation refused. -/
private theorem store_checked (o : Option I128) (s : U32) (c : Option ℤ) (err : scaled.ScaledRefusal)
    (ho : (match o with | some z => some z.val | none => none) = c) :
    ∃ r, (match o with
        | some z => scaled.to_decimal z s
        | none => ok (core.result.Result.Err err)) = ok r ∧
      projDec r = c.bind (fun v => store v s.val) := by
  cases o with
  | none => subst ho; exact ⟨_, rfl, rfl⟩
  | some z =>
      subst ho
      obtain ⟨r, hr, hrv⟩ := to_decimal_correct z s
      exact ⟨r, hr, hrv⟩

private theorem bind_if (b : Bool) (v : ℤ) (s : ℕ) :
    (if b = true then some v else none).bind (fun v => store v s) =
      (if b = true then store v s else none) := by
  cases b <;> rfl

private theorem coeff_le18 (s : ℕ) (q : ℚ) (x : ℤ) (h : some x = coefficientSpec s q) : s ≤ 18 := by
  by_contra hs
  simp [coefficientSpec, hs] at h

set_option hygiene false in
/-- Close one checked-arithmetic leaf: the program is the checked result stored. -/
local macro "checked_leaf" sc:term:max "[" extra:Lean.Parser.Tactic.simpLemma,* "]" : tactic => `(tactic|
  (cases o with
   | none => exact ⟨.Err _, by (evalS [$extra,*, ho]) <;> rfl,
       by (try simp only [projInt]); rw [← bind_if, ← hov]; rfl⟩
   | some z =>
       obtain ⟨r, hr, hrv⟩ := to_decimal_correct z $sc
       refine ⟨r, by evalS [$extra,*, ho]; exact hr, ?_⟩
       (try simp only [projInt]); rw [hrv, ← bind_if, ← hov]; rfl))

/-- VAL-EXACT-1 correspondence for `exact_binary`, over every opcode and pair of decimals. -/
theorem exact_binary_correct (op : scaled.ArithmeticOp) (a b : rust_decimal.decimal.Decimal) :
    ∃ r, scaled.exact_binary op a b = ok r ∧
      projDec r = exactBinarySpec (opOf op) a.coeff a.places b.coeff b.places := by
  obtain ⟨sa, sb, r0, hsa, hsb, hr0, hr0v⟩ := scales_eval op a b
  cases r0 with
  | Err e =>
      refine ⟨.Err e, by evalS [scaled.exact_binary, hsa, hsb, hr0], ?_⟩
      simp only at hr0v
      simp only [exactBinarySpec]; rw [← hr0v]; rfl
  | Ok s =>
      simp only [opOf] at hr0v
      have hspec : ∀ o : Op, exactResultScaleSpec o a.places b.places = some s.val →
          exactBinarySpec o a.coeff a.places b.coeff b.places =
            match o with
            | .multiply => if fitsI128 (a.coeff * b.coeff) = true then
                store (a.coeff * b.coeff) s.val else none
            | .add =>
                match coefficientSpec s.val (decOf a), coefficientSpec s.val (decOf b) with
                | some x, some y => if fitsI128 (x + y) = true then store (x + y) s.val else none
                | _, _ => none
            | .subtract =>
                match coefficientSpec s.val (decOf a), coefficientSpec s.val (decOf b) with
                | some x, some y => if fitsI128 (x - y) = true then store (x - y) s.val else none
                | _, _ => none
            | .divide | .other => none := by
        intro o ho; simp only [exactBinarySpec, ho]; rfl
      cases op with
      | Divide => simp [exactResultScaleSpec] at hr0v
      | Other => simp [exactResultScaleSpec] at hr0v
      | Multiply =>
          simp only [opOf] at hr0v ⊢
          rw [hspec _ hr0v.symm]
          obtain ⟨ma, hma, hmav⟩ := mantissa_eval a
          obtain ⟨mb, hmb, hmbv⟩ := mantissa_eval b
          obtain ⟨o, ho, hov⟩ := checked_eval_mul ma mb
          rw [hmav, hmbv] at hov
          checked_leaf s [scaled.exact_binary, hsa, hsb, hr0, hma, hmb]
      | Add =>
          simp only [opOf] at hr0v ⊢
          rw [hspec _ hr0v.symm]
          obtain ⟨ra, hra, hrav⟩ := coefficient_correct s a
          obtain ⟨rb, hrb, hrbv⟩ := coefficient_correct s b
          rw [← hrav, ← hrbv]
          cases ra with
          | Err e => exact ⟨.Err e, by evalS [scaled.exact_binary, hsa, hsb, hr0, hra], rfl⟩
          | Ok x =>
            cases rb with
            | Err e => exact ⟨.Err e, by evalS [scaled.exact_binary, hsa, hsb, hr0, hra, hrb], rfl⟩
            | Ok y =>
                obtain ⟨o, ho, hov⟩ := checked_eval_add x y
                checked_leaf s [scaled.exact_binary, hsa, hsb, hr0, hra, hrb]
      | Subtract =>
          simp only [opOf] at hr0v ⊢
          rw [hspec _ hr0v.symm]
          obtain ⟨ra, hra, hrav⟩ := coefficient_correct s a
          obtain ⟨rb, hrb, hrbv⟩ := coefficient_correct s b
          rw [← hrav, ← hrbv]
          cases ra with
          | Err e => exact ⟨.Err e, by evalS [scaled.exact_binary, hsa, hsb, hr0, hra], rfl⟩
          | Ok x =>
            cases rb with
            | Err e => exact ⟨.Err e, by evalS [scaled.exact_binary, hsa, hsb, hr0, hra, hrb], rfl⟩
            | Ok y =>
                obtain ⟨o, ho, hov⟩ := checked_eval_sub x y
                checked_leaf s [scaled.exact_binary, hsa, hsb, hr0, hra, hrb]

private theorem fit_of_some (v : ℤ) (z : I128) (h : some z.val = (if fitsI128 v = true then some v else none)) :
    fitsI128 v = true ∧ z.val = v := by
  cases hf : fitsI128 v <;> simp [hf] at h ⊢; exact h

private theorem nofit_of_none (v : ℤ) (h : (none : Option ℤ) = (if fitsI128 v = true then some v else none)) :
    fitsI128 v = false := by
  cases hf : fitsI128 v <;> simp [hf] at h ⊢

/-- FOLD-SCALED correspondence for `binary`, over every site, opcode and pair of decimals. -/
theorem binary_correct (profile : scaled.ScaledArithmetic) (op : scaled.ArithmeticOp)
    (a b : rust_decimal.decimal.Decimal) :
    ∃ r, scaled.binary profile op a b = ok r ∧
      projDec r = binarySpec (modeOf profile.rounding) profile.scale.val (opOf op)
        a.coeff a.places b.coeff b.places := by
  obtain ⟨ra, hra, hrav⟩ := coefficient_correct profile.scale a
  obtain ⟨rb, hrb, hrbv⟩ := coefficient_correct profile.scale b
  simp only [decOf] at hrav hrbv
  simp only [binarySpec]
  rw [← hrav, ← hrbv]
  cases ra with
  | Err e => exact ⟨.Err e, by evalS [scaled.binary, hra], rfl⟩
  | Ok x =>
    cases rb with
    | Err e => exact ⟨.Err e, by evalS [scaled.binary, hra, hrb], rfl⟩
    | Ok y =>
      have h18 := coeff_le18 _ _ _ hrav
      obtain ⟨f, hf, hfv⟩ := pow_eval profile.scale (by omega)
      simp only [projInt]
      cases op with
      | Add =>
          simp only [opOf]
          obtain ⟨o, ho, hov⟩ := checked_eval_add x y
          checked_leaf profile.scale [scaled.binary, hra, hrb, hf]
      | Subtract =>
          simp only [opOf]
          obtain ⟨o, ho, hov⟩ := checked_eval_sub x y
          checked_leaf profile.scale [scaled.binary, hra, hrb, hf]
      | Other => exact ⟨.Err _, by (evalS [scaled.binary, hra, hrb, hf] ; rfl), rfl⟩
      | Multiply =>
          simp only [opOf]
          obtain ⟨o, ho, hov⟩ := checked_eval_mul x y
          cases o with
          | none =>
              refine ⟨.Err _, by (evalS [scaled.binary, hra, hrb, hf, ho] ; rfl), ?_⟩
              rw [nofit_of_none _ hov]; rfl
          | some p =>
              obtain ⟨hfit, hpv⟩ := fit_of_some _ _ hov
              obtain ⟨q, hq, hqv⟩ := rounded_quotient_correct profile.rounding p f
              rw [if_pos hfit, ← hpv, ← hfv, ← hqv]
              cases q with
              | Ok c =>
                  obtain ⟨r, hr, hrv⟩ := to_decimal_correct c profile.scale
                  exact ⟨r, by evalS [scaled.binary, hra, hrb, hf, ho, hq]; exact hr, hrv⟩
              | Err e =>
                  exact ⟨.Err _, by (evalS [scaled.binary, hra, hrb, hf, ho, hq] ; rfl), rfl⟩
      | Divide =>
          simp only [opOf]
          obtain ⟨o, ho, hov⟩ := checked_eval_mul x f
          rw [hfv] at hov
          cases o with
          | none =>
              refine ⟨.Err _, by (evalS [scaled.binary, hra, hrb, hf, ho] ; rfl), ?_⟩
              rw [nofit_of_none _ hov]; rfl
          | some n =>
              obtain ⟨hfit, hnv⟩ := fit_of_some _ _ hov
              obtain ⟨q, hq, hqv⟩ := rounded_quotient_correct profile.rounding n y
              rw [if_pos hfit, ← hnv, ← hqv]
              cases q with
              | Ok c =>
                  obtain ⟨r, hr, hrv⟩ := to_decimal_correct c profile.scale
                  exact ⟨r, by evalS [scaled.binary, hra, hrb, hf, ho, hq]; exact hr, hrv⟩
              | Err e =>
                  exact ⟨.Err _, by (evalS [scaled.binary, hra, hrb, hf, ho, hq] ; rfl), rfl⟩

private theorem from_u32_eval (x : U32) :
    ∃ y, I64.Insts.CoreConvertFromU32.from x = ok y ∧ y.val = x.val := by
  have hb : x.val < 2 ^ 32 := UScalar.hBounds x
  unfold I64.Insts.CoreConvertFromU32.from
  rw [dif_pos (by norm_num [IScalarTy.numBits]; omega)]
  exact ⟨_, rfl, I64.ofInt_val_eq _⟩

private theorem uabs_eval (x : I64) (h : x.val.natAbs ≤ 36) :
    ∃ u k, core.num.I64.unsigned_abs x = ok u ∧
      U32.Insts.CoreConvertTryFromU64TryFromIntError.try_from u = ok (core.result.Result.Ok k) ∧
      k.val = x.val.natAbs := by
  have hu : (⟨BitVec.ofNat 64 x.val.natAbs⟩ : U64).val = x.val.natAbs := by
    show (BitVec.ofNat 64 x.val.natAbs).toNat = _
    rw [BitVec.toNat_ofNat]; exact Nat.mod_eq_of_lt (by omega)
  refine ⟨⟨BitVec.ofNat 64 x.val.natAbs⟩, ⟨BitVec.ofNat 32 x.val.natAbs⟩, rfl, ?_, ?_⟩
  · simp only [U32.Insts.CoreConvertTryFromU64TryFromIntError.try_from, hu]
    rw [if_pos (by omega)]
  · show (BitVec.ofNat 32 x.val.natAbs).toNat = _
    rw [BitVec.toNat_ofNat]; exact Nat.mod_eq_of_lt (by omega)

private theorem scale_spec_le (pa pb s : ℕ) (h : some s = exactResultScaleSpec .add pa pb) :
    pa ≤ 18 ∧ pb ≤ 18 := by
  simp only [exactResultScaleSpec] at h
  split_ifs at h with h1
  · omega

private theorem mant_fits (d : rust_decimal.decimal.Decimal) : fitsI128 d.coeff = true := by
  have := d.coeff_bound
  simp [fitsI128]; omega

/-- VAL-ROUND correspondence for `divide`, over every site and pair of decimals. -/
theorem divide_correct (profile : scaled.ScaledArithmetic) (a b : rust_decimal.decimal.Decimal) :
    ∃ r, scaled.divide profile a b = ok r ∧
      projDec r = divideSpec (modeOf profile.rounding) profile.scale.val
        a.coeff a.places b.coeff b.places := by
  obtain ⟨sa, hsa, hsav⟩ := scale_eval a
  obtain ⟨sb, hsb, hsbv⟩ := scale_eval b
  obtain ⟨r0, hr0, hr0v⟩ := exact_result_scale_correct .Add sa sb
  rw [hsav, hsbv] at hr0v
  simp only [opOf] at hr0v
  cases r0 with
  | Err e =>
      refine ⟨.Err e, by evalS [scaled.divide, hsa, hsb, hr0], ?_⟩
      simp only at hr0v
      have : 18 < max a.places b.places := by
        by_contra h
        simp [exactResultScaleSpec] at hr0v; omega
      simp only [divideSpec]; rw [if_pos (Or.inl this)]; rfl
  | Ok sc =>
    simp only at hr0v
    obtain ⟨ha18, hb18⟩ := scale_spec_le _ _ _ hr0v
    by_cases ht : profile.scale.val ≤ 18
    swap
    · have hgt : profile.scale > 18#u32 := by change (18#u32).val < _; simp; omega
      refine ⟨.Err .OutsideDeclaredScale, by evalS [scaled.divide, hsa, hsb, hr0, hgt], ?_⟩
      simp only [divideSpec]; rw [if_pos (Or.inr (by omega))]; rfl
    have hgt : ¬ profile.scale > 18#u32 := by
      intro h; change (18#u32).val < _ at h; simp at h; omega
    obtain ⟨i2, hi2, hi2v⟩ := from_u32_eval profile.scale
    obtain ⟨i3, hi3, hi3v⟩ := from_u32_eval sb
    obtain ⟨i5, hi5, hi5v⟩ := from_u32_eval sa
    obtain ⟨i4, hi4, hi4v⟩ := WP.spec_imp_exists (I64.add_spec (x := i2) (y := i3)
      (by simp [I64.min, I64.numBits]; omega) (by simp [I64.max, I64.numBits]; omega))
    obtain ⟨ex, hex, hexv⟩ := WP.spec_imp_exists (I64.sub_spec (x := i4) (y := i5)
      (by simp [I64.min, I64.numBits]; omega) (by simp [I64.max, I64.numBits]; omega))
    rw [hi4v, hi2v, hi3v, hi5v, hsbv, hsav] at hexv
    have hexb : ex.val.natAbs ≤ 36 := by
      rw [← Int.ofNat_le, Int.natCast_natAbs, abs_le]; constructor <;> omega
    obtain ⟨u, k, hu, htf, hkv⟩ := uabs_eval ex hexb
    obtain ⟨f, hf, hfv⟩ := pow_eval k (by omega)
    obtain ⟨ma, hma, hmav⟩ := mantissa_eval a
    obtain ⟨mb, hmb, hmbv⟩ := mantissa_eval b
    have hcond : ¬ (18 < max a.places b.places ∨ 18 < profile.scale.val) := by omega
    simp only [divideSpec]
    rw [if_neg hcond]
    simp only [← hexv]
    by_cases he : 0 ≤ ex.val
    · have hkn : (10 : ℤ) ^ ex.val.toNat = f.val := by
        have h1 : ((ex.val.toNat : ℕ) : ℤ) = ex.val := Int.toNat_of_nonneg he
        have h2 : ((ex.val.natAbs : ℕ) : ℤ) = ex.val := Int.natAbs_of_nonneg he
        have h3 : ex.val.toNat = ex.val.natAbs := by exact_mod_cast h1.trans h2.symm
        rw [hfv, hkv, h3]
      simp only [he, if_true, hkn, mant_fits b, and_true]
      obtain ⟨o, ho, hov⟩ := checked_eval_mul ma f
      rw [hmav] at hov
      cases o with
      | none =>
          refine ⟨.Err _, by (evalS [scaled.divide, hsa, hsb, hr0, hgt, hi2, hi3, hi4, hi5, hex,
            hu, htf, core.result.Result.ok, hf, he, hma, ho] ; rfl), ?_⟩
          rw [nofit_of_none _ hov]; rfl
      | some nn =>
          obtain ⟨hfit, hnv⟩ := fit_of_some _ _ hov
          obtain ⟨q, hq, hqv⟩ := rounded_quotient_correct profile.rounding nn mb
          rw [if_pos hfit, ← hnv, ← hmbv, ← hqv]
          cases q with
          | Ok c =>
              obtain ⟨r, hr, hrv⟩ := to_decimal_correct c profile.scale
              exact ⟨r, by evalS [scaled.divide, hsa, hsb, hr0, hgt, hi2, hi3, hi4, hi5, hex, hu, htf, core.result.Result.ok,
                hf, he, hma, ho, hmb, hq]; exact hr, hrv⟩
          | Err e =>
              exact ⟨.Err _, by (evalS [scaled.divide, hsa, hsb, hr0, hgt, hi2, hi3, hi4, hi5,
                hex, hu, htf, core.result.Result.ok, hf, he, hma, ho, hmb, hq] ; rfl), rfl⟩
    · have hkn : (10 : ℤ) ^ (-ex.val).toNat = f.val := by
        have h1 : (((-ex.val).toNat : ℕ) : ℤ) = -ex.val := Int.toNat_of_nonneg (by omega)
        have h2 : ((ex.val.natAbs : ℕ) : ℤ) = -ex.val := Int.ofNat_natAbs_of_nonpos (by omega)
        have h3 : (-ex.val).toNat = ex.val.natAbs := by exact_mod_cast h1.trans h2.symm
        rw [hfv, hkv, h3]
      simp only [he, if_false, hkn, mant_fits a, true_and]
      obtain ⟨o, ho, hov⟩ := checked_eval_mul mb f
      rw [hmbv] at hov
      cases o with
      | none =>
          refine ⟨.Err _, by (evalS [scaled.divide, hsa, hsb, hr0, hgt, hi2, hi3, hi4, hi5, hex,
            hu, htf, core.result.Result.ok, hf, he, hma, hmb, ho] ; rfl), ?_⟩
          rw [nofit_of_none _ hov]; rfl
      | some dd =>
          obtain ⟨hfit, hdv⟩ := fit_of_some _ _ hov
          obtain ⟨q, hq, hqv⟩ := rounded_quotient_correct profile.rounding ma dd
          rw [if_pos hfit, ← hdv, ← hmav, ← hqv]
          cases q with
          | Ok c =>
              obtain ⟨r, hr, hrv⟩ := to_decimal_correct c profile.scale
              exact ⟨r, by evalS [scaled.divide, hsa, hsb, hr0, hgt, hi2, hi3, hi4, hi5, hex, hu, htf, core.result.Result.ok,
                hf, he, hma, hmb, ho, hq]; exact hr, hrv⟩
          | Err e =>
              exact ⟨.Err _, by (evalS [scaled.divide, hsa, hsb, hr0, hgt, hi2, hi3, hi4, hi5,
                hex, hu, htf, core.result.Result.ok, hf, he, hma, hmb, ho, hq] ; rfl), rfl⟩

/-- VAL-ROUND correspondence for `rounding_site`: an explicit mode overrides an
    inherited site, absent modes never round, and the scale is at most 18. -/
theorem rounding_site_correct (s : U32) (mode : Option scaled.Rounding)
    (inherited : Option scaled.ScaledArithmetic) (exact : Bool) :
    scaled.rounding_site s mode inherited exact = ok
      (match mode, inherited with
       | some m, _ => if s.val ≤ 18 then .Ok (some ⟨s, m⟩) else .Err .InvalidSiteScale
       | none, some site =>
           if s.val ≤ 18 then .Ok (some ⟨s, site.rounding⟩) else .Err .InvalidSiteScale
       | none, none => if exact then .Err .RoundingUndeclared else .Ok none) := by
  have hgt : (s > 18#u32) ↔ ¬ s.val ≤ 18 := by
    constructor
    · intro h; change (18#u32).val < _ at h; simp at h; omega
    · intro h; change (18#u32).val < _; simp; omega
  cases mode <;> cases inherited <;> cases exact <;>
    simp [scaled.rounding_site, hgt] <;> split_ifs <;> first | rfl | (exfalso; omega)

end Norn.OrderedFold
#print axioms Norn.OrderedFold.coefficient_correct
#print axioms Norn.OrderedFold.to_decimal_correct
#print axioms Norn.OrderedFold.exact_result_scale_correct
#print axioms Norn.OrderedFold.exact_compare_correct
#print axioms Norn.OrderedFold.exact_binary_correct
#print axioms Norn.OrderedFold.rescale_correct
#print axioms Norn.OrderedFold.divide_correct
#print axioms Norn.OrderedFold.binary_correct
#print axioms Norn.OrderedFold.rounding_site_correct
