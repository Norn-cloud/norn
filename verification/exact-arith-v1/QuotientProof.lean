import ScaledQuotient.Funs
import RoundingMath

/-!
# Production `rounded_quotient` meets FOLD-ARITHMETIC

`ScaledQuotient` is the fresh Aeneas translation of `rounded_quotient` in the
production file `vendor/zen/core/expression/src/scaled.rs`, with the two core
models of `ScaledQuotient/FunsExternal.lean`. For every rounding mode and every
pair of `i128` operands the function returns (never panics or diverges) and
its result is exactly the written semantics `quantize`: refusal is `Err`, and an
`Ok` coefficient is the declared rounding of the exact quotient.
-/

namespace Norn.OrderedFold
open Aeneas Aeneas.Std Result
open zen_expression

/-- The production rounding enum, as the written semantics' mode. -/
def modeOf : scaled.Rounding → Mode
  | .Refuse => .refuse
  | .TowardZero => .towardZero
  | .HalfEven => .halfEven
  | .HalfAwayFromZero => .halfAwayFromZero

/-- A production result as a written-semantics result: `Err` is refusal. -/
def project {E : Type} : core.result.Result Std.I128 E → Option ℤ
  | .Ok v => some v.val
  | .Err _ => none

private theorem unsigned_abs_val (x : Std.I128) :
    ∃ m, core.num.I128.unsigned_abs x = ok m ∧ m.val = x.val.natAbs := by
  refine ⟨_, rfl, ?_⟩
  have hb := x.hBounds
  simp only [UScalar.val]
  apply Nat.mod_eq_of_lt
  simp at hb
  omega

private theorem fits_val (x : Std.I128) : fitsI128 x.val = true := by
  have hb := x.hBounds
  simp at hb
  simp [fitsI128]
  omega

private theorem tdiv_small (n d : ℤ) (hn : n.natAbs ≤ 2 ^ 127) (hd : 2 ≤ d.natAbs) :
    -2 ^ 126 ≤ n.tdiv d ∧ n.tdiv d ≤ 2 ^ 126 := by
  have h : (n.tdiv d).natAbs = n.natAbs / d.natAbs := Int.natAbs_tdiv n d
  have h2 : n.natAbs / d.natAbs ≤ 2 ^ 126 := by
    calc n.natAbs / d.natAbs ≤ n.natAbs / 2 := Nat.div_le_div_left hd (by norm_num)
      _ ≤ 2 ^ 126 := by omega
  rcases Int.natAbs_eq (n.tdiv d) with he | he <;> omega

private theorem lt_zero_iff (x : Std.I128) : (x < 0#i128) ↔ x.val < 0 := Iff.rfl

private theorem direction_iff (n d : Std.I128) :
    ((n < 0#i128) = (d < 0#i128)) ↔ (decide (n.val < 0) = decide (d.val < 0)) := by
  rw [lt_zero_iff, lt_zero_iff]
  by_cases hn : n.val < 0 <;> by_cases hd : d.val < 0 <;> simp [hn, hd]

private theorem eq_even_away :
    scaled.Rounding.Insts.CoreCmpPartialEqRounding.eq .HalfEven .HalfAwayFromZero = ok false := by
  simp [scaled.Rounding.Insts.CoreCmpPartialEqRounding.eq, scaled.Rounding.read_discriminant]

private theorem eq_away_away :
    scaled.Rounding.Insts.CoreCmpPartialEqRounding.eq .HalfAwayFromZero .HalfAwayFromZero =
      ok true := by
  first | rfl | decide | simp [scaled.Rounding.Insts.CoreCmpPartialEqRounding.eq]

private theorem ugt_iff (a b : Std.U128) : (a > b) ↔ b.val < a.val := Iff.rfl

private theorem ueq_iff (a b : Std.U128) : (a = b) ↔ a.val = b.val := UScalar.eq_equiv a b

private theorem ieq_iff (a b : Std.I128) : (a = b) ↔ a.val = b.val := IScalar.eq_equiv a b

set_option hygiene false in
/-- Evaluate the extracted program from the per-operation facts in scope. -/
local macro "eval_prog" "[" extra:Lean.Parser.Tactic.simpLemma,* "]" : tactic => `(tactic|
  simp [scaled.rounded_quotient, I128.checked_div, I128.checked_rem, I128.checked_add, hq, hr,
    lift, core.option.Option.ok_or, core.result.Result.Insts.CoreOpsTry.branch,
    core.result.Result.Insts.CoreOpsTryTraitFromResidualResultInfallible.from_residual,
    hrne, hm, hi, hc, hp, eq_even_away, eq_away_away, ugt_iff, ueq_iff, hmv, hcv', hpz, hpv2,
    $extra,*])

/-- FOLD-ARITHMETIC correspondence for the production rounding core. -/
theorem rounded_quotient_correct (rounding : scaled.Rounding) (n d : Std.I128) :
    ∃ r, scaled.rounded_quotient rounding n d = ok r ∧
      project r = quantize (modeOf rounding) n.val d.val := by
  have hnb := n.hBounds
  have hdb := d.hBounds
  simp at hnb hdb
  have hdiv := I128.checked_div_bv_spec n d
  have hrem := I128.checked_rem_bv_spec n d
  cases hq : core.num.checked_div_IScalar n d with
  | none =>
      rw [hq] at hdiv
      refine ⟨.Err .DivisorOrOverflow, ?_, ?_⟩
      · simp [scaled.rounded_quotient, I128.checked_div, hq, lift, core.option.Option.ok_or,
          core.result.Result.Insts.CoreOpsTry.branch,
          core.result.Result.Insts.CoreOpsTryTraitFromResidualResultInfallible.from_residual]
      · simp only [project, quantize]
        rcases hdiv with h0 | ⟨hmin, hm1⟩
        · simp [h0]
        · have hd0 : d.val ≠ 0 := by rw [hm1]; decide
          rw [if_neg hd0, hmin, hm1]
          have : ((I128.min : ℤ) : ℚ) / ((-1 : ℤ) : ℚ) = ((2 ^ 127 : ℤ) : ℚ) := by
            simp [I128.min, I128.numBits]
          rw [this, roundRat_int]
          simp [fitsI128, Option.filter]
  | some t =>
      rw [hq] at hdiv
      obtain ⟨hd0, -, ht, -⟩ := hdiv
      cases hr : core.num.checked_rem_IScalar n d with
      | none => rw [hr] at hrem; exact absurd hrem hd0
      | some r =>
      rw [hr] at hrem
      obtain ⟨-, hrv, -⟩ := hrem
      have hmath := roundRat_eq_viaRemainder (modeOf rounding) n.val d.val hd0
      simp only [quantize, if_neg hd0, hmath]
      unfold viaRemainder
      rw [← hrv, ← ht]
      -- Facts about every operation the program performs.
      obtain ⟨m, hm, hmv⟩ := unsigned_abs_val r
      obtain ⟨i, hi, hiv⟩ := unsigned_abs_val d
      have hlt : r.val.natAbs < d.val.natAbs := by
        rw [hrv, Int.natAbs_tmod]; exact Nat.mod_lt _ (Int.natAbs_pos.mpr hd0)
      have hle : m.val ≤ i.val := by rw [hmv, hiv]; exact hlt.le
      obtain ⟨c, hc, hcv, -⟩ := WP.spec_imp_exists (U128.sub_spec (x := i) (y := m) hle)
      have hcv' : c.val = d.val.natAbs - r.val.natAbs := by rw [hcv, hiv, hmv]
      obtain ⟨p, hp, hpv, -⟩ := WP.spec_imp_exists (I128.rem_bv_spec t (y := 2#i128) (by simp))
      have hpv2 : p.val = t.val.tmod 2 := by simpa using hpv
      have hpz : (p != 0#i128) = decide (t.val.tmod 2 ≠ 0) := by
        rw [← hpv2]
        by_cases h : p.val = 0
        · have : p = 0#i128 := (ieq_iff p 0#i128).mpr (by simpa using h)
          simp [this]
        · have : ¬ p = 0#i128 := fun e => h (by simpa using (ieq_iff p 0#i128).mp e)
          simp [this, h]
      by_cases hr0 : r.val = 0
      · have hrne : r = 0#i128 := (ieq_iff r 0#i128).mpr (by simpa using hr0)
        refine ⟨.Ok t, ?_, ?_⟩
        · eval_prog []
        · simp [project, hr0, fits_val, Option.filter]
      have hrne : ¬ r = 0#i128 := fun h => hr0 (by simpa using (ieq_iff r 0#i128).mp h)
      -- A rounded step never overflows: a nonzero remainder needs |d| ≥ 2.
      have hd2 : 2 ≤ d.val.natAbs := by
        have : 0 < r.val.natAbs := Int.natAbs_pos.mpr hr0
        omega
      have hsmall := tdiv_small n.val d.val (by omega) hd2
      rw [← ht] at hsmall
      have hup : ∃ z, core.num.checked_add_IScalar t 1#i128 = some z ∧ z.val = t.val + 1 := by
        have h := I128.checked_add_bv_spec t 1#i128
        cases hz : core.num.checked_add_IScalar t 1#i128 with
        | none => rw [hz] at h; simp [I128.min, I128.max, I128.numBits] at h; omega
        | some z => rw [hz] at h; exact ⟨z, rfl, by simpa using h.2.2.1⟩
      have hdown : ∃ z, core.num.checked_add_IScalar t (-1)#i128 = some z ∧ z.val = t.val + -1 := by
        have h := I128.checked_add_bv_spec t (-1)#i128
        cases hz : core.num.checked_add_IScalar t (-1)#i128 with
        | none => rw [hz] at h; simp [I128.min, I128.max, I128.numBits] at h; omega
        | some z => rw [hz] at h; exact ⟨z, rfl, by simp at h; omega⟩
      obtain ⟨zu, hzu, hzuv⟩ := hup
      obtain ⟨zd, hzd, hzdv⟩ := hdown
      have hfu := fits_val zu
      have hfd := fits_val zd
      rw [hzuv] at hfu
      rw [hzdv] at hfd
      have hdir := direction_iff n d
      have hB2 : (r.val.natAbs = d.val.natAbs - r.val.natAbs) ↔
          (d.val.natAbs - r.val.natAbs = r.val.natAbs) := eq_comm
      have hdirT : ((n < 0#i128) = (d < 0#i128)) = (decide (n.val < 0) = decide (d.val < 0)) :=
        propext hdir
      cases rounding with
      | Refuse =>
          refine ⟨.Err .Inexact, ?_, ?_⟩
          · eval_prog []
          · simp [project, modeOf, hr0]
      | TowardZero =>
          refine ⟨.Ok t, ?_, ?_⟩
          · eval_prog []
          · simp [project, modeOf, hr0, fits_val, Option.filter]
      | HalfEven =>
          by_cases hA : d.val.natAbs - r.val.natAbs < r.val.natAbs <;>
          by_cases hB : d.val.natAbs - r.val.natAbs = r.val.natAbs <;>
          by_cases hP : t.val.tmod 2 ≠ 0 <;>
          by_cases hD : (n.val < 0 ↔ d.val < 0) <;>
          first
            | (exfalso; omega)
            | (refine ⟨.Ok t, ?_, ?_⟩
               · eval_prog [hA, hB2, hB, hP, hD]
               · simp [project, modeOf, hr0, hA, hB2, hB, hP, fits_val, Option.filter])
            | (refine ⟨.Ok zu, ?_, ?_⟩
               · eval_prog [hA, hB2, hB, hP, hD, hzu]
               · simp [project, modeOf, hr0, hA, hB2, hB, hP, hD, hzuv, hfu, Option.filter])
            | (refine ⟨.Ok zd, ?_, ?_⟩
               · eval_prog [hA, hB2, hB, hP, hD, hzd]
               · simp [project, modeOf, hr0, hA, hB2, hB, hP, hD, hzdv, hfd, Option.filter])
      | HalfAwayFromZero =>
          by_cases hA : d.val.natAbs - r.val.natAbs < r.val.natAbs <;>
          by_cases hB : d.val.natAbs - r.val.natAbs = r.val.natAbs <;>
          by_cases hD : (n.val < 0 ↔ d.val < 0) <;>
          first
            | (exfalso; omega)
            | (refine ⟨.Ok t, ?_, ?_⟩
               · eval_prog [hA, hB2, hB, hD]
               · simp [project, modeOf, hr0, hA, hB2, hB, fits_val, Option.filter])
            | (refine ⟨.Ok zu, ?_, ?_⟩
               · eval_prog [hA, hB2, hB, hD, hzu]
               · simp [project, modeOf, hr0, hA, hB2, hB, hD, hzuv, hfu, Option.filter])
            | (refine ⟨.Ok zd, ?_, ?_⟩
               · eval_prog [hA, hB2, hB, hD, hzd]
               · simp [project, modeOf, hr0, hA, hB2, hB, hD, hzdv, hfd, Option.filter])

end Norn.OrderedFold
#print axioms Norn.OrderedFold.rounded_quotient_correct
