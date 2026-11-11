import DecimalProof
import LedgerReference

/-!
# Production ledger scaled-integer arithmetic meets VAL-EXACT-1

`Decimal` is the fresh Aeneas translation of the production file
`crates/norn-checker/src/quantity_ledger/decimal.rs`. Ledger lowering
(`quantity_ledger::expression`) calls these functions and only attaches an
error code, path and message to a refusal. Each theorem covers every input.
-/

namespace Norn.QuantityLedger
open Aeneas Aeneas.Std Result
open norn_checker_decimal

/-- The written refusal a production refusal stands for. -/
def ofRust : decimal.ScaledIntegerRefusal → LedgerRefusal
  | .ScaleLoss => .scaleLoss
  | .AlignmentMultiplierOverflow => .alignmentMultiplierOverflow
  | .AlignmentOverflow => .alignmentOverflow
  | .AdditionOverflow => .additionOverflow
  | .SubtractionOverflow => .subtractionOverflow
  | .MultiplicationScaleOverflow => .multiplicationScaleOverflow
  | .MultiplicationScaleAboveMaximum s => .multiplicationScaleAboveMaximum s.val
  | .MultiplicationOverflow => .multiplicationOverflow

/-- A production rescale result against the written one. -/
def projRescale : core.result.Result Std.I128 decimal.ScaledIntegerRefusal → Except LedgerRefusal ℤ
  | .Ok v => .ok v.val
  | .Err e => .error (ofRust e)

/-- A production amount result against the written unnormalized one: the
    stored amount is the canonical form of the written result, and a refusal
    is the written refusal. -/
def Outcome :
    core.result.Result decimal.ScaledInteger decimal.ScaledIntegerRefusal →
      Except LedgerRefusal (ℤ × ℕ) → Prop
  | .Ok c, .ok (u, s) => IsCanonicalForm u s c.unscaled.val c.scale.val
  | .Err e, .error r => ofRust e = r
  | _, _ => False

/-- An accepted production amount is the exact written value. -/
theorem Outcome.value_eq {c : decimal.ScaledInteger} {u : ℤ} {s : ℕ}
    (h : Outcome (.Ok c) (.ok (u, s))) : ledgerValue c.unscaled.val c.scale.val = ledgerValue u s :=
  IsCanonicalForm.value_eq h

private theorem canonical_spec (v : decimal.ScaledInteger) :
    ∃ c, decimal.canonical v = ok c ∧
      IsCanonicalForm v.unscaled.val v.scale.val c.unscaled.val c.scale.val := by
  obtain ⟨c, s', hrun, hcan⟩ := canonical_parts_correct v.unscaled v.scale
  exact ⟨⟨c, s'⟩, by simp [decimal.canonical, hrun], hcan⟩

private theorem ii (z : ℤ) : InI128 z ↔ I128.min ≤ z ∧ z ≤ I128.max := by
  simp [InI128, I128.min, I128.max, I128.numBits]; omega

/-- VAL-EXACT-1 correspondence for `rescale`, over every amount and `u8` target scale. -/
theorem rescale_correct (v : decimal.ScaledInteger) (t : U8) :
    ∃ r, decimal.rescale v t = ok r ∧
      projRescale r = rescaleSpec v.unscaled.val v.scale.val t.val := by
  have hsub := U8.checked_sub_bv_spec t v.scale
  cases hcs : t.checked_sub v.scale with
  | none =>
      rw [hcs] at hsub
      simp at hsub
      refine ⟨.Err .ScaleLoss, by simp [decimal.rescale, hcs, lift], ?_⟩
      simp [projRescale, ofRust, rescaleSpec, hsub]
  | some g =>
      rw [hcs] at hsub
      obtain ⟨hle, hg, -⟩ := hsub
      try simp only [U8.max, U8.numBits] at hle hg ⊢
      have hnot : ¬ t.val < v.scale.val := by omega
      obtain ⟨p, hp, hpv⟩ := checked_pow10_correct g
      cases p with
      | none =>
          refine ⟨.Err .AlignmentMultiplierOverflow, by simp [decimal.rescale, hcs, hp, lift], ?_⟩
          simp only [Option.map_none] at hpv
          simp [projRescale, ofRust, rescaleSpec, hnot, ← hg, ← hpv]
      | some m =>
          simp only [Option.map_some] at hpv
          have hmul := I128.checked_mul_bv_spec v.unscaled m
          cases hm : core.num.checked_mul_IScalar v.unscaled m with
          | none =>
              rw [hm] at hmul
              refine ⟨.Err .AlignmentOverflow,
                by simp [decimal.rescale, hcs, hp, I128.checked_mul, hm, lift], ?_⟩
              have hout : ¬ InI128 (v.unscaled.val * m.val) := by
                rw [ii]; simpa using hmul
              simp [projRescale, ofRust, rescaleSpec, hnot, ← hg, ← hpv, hout]
          | some z =>
              rw [hm] at hmul
              obtain ⟨-, -, hzv, -⟩ := hmul
              refine ⟨.Ok z, by simp [decimal.rescale, hcs, hp, I128.checked_mul, hm, lift], ?_⟩
              have hin : InI128 (v.unscaled.val * m.val) := by
                rw [← hzv]; have := z.hBounds; simp at this; simp [InI128]; omega
              simpa [projRescale, rescaleSpec, hnot, ← hg, ← hpv, hzv, InI128] using hin

set_option hygiene false in
/-- The `Result` `?` of the extraction, in the Aeneas library. -/
local macro "try_simp" "[" extra:Lean.Parser.Tactic.simpLemma,* "]" : tactic => `(tactic|
  simp [core.result.Result.Insts.CoreOpsTry.branch,
    core.result.Result.Insts.CoreOpsTryTraitFromResidualResultInfallible.from_residual,
    core.convert.FromSame, lift, $extra,*])

private def projAlign :
    core.result.Result (Std.I128 × Std.I128 × Std.U8) decimal.ScaledIntegerRefusal →
      Except LedgerRefusal (ℤ × ℤ × ℕ)
  | .Ok (a, b, t) => .ok (a.val, b.val, t.val)
  | .Err e => .error (ofRust e)

private theorem align_correct (a b : decimal.ScaledInteger) :
    ∃ r, decimal.align a b = ok r ∧
      projAlign r = alignSpec a.unscaled.val a.scale.val b.unscaled.val b.scale.val := by
  obtain ⟨t, ht, htv⟩ : ∃ t : U8,
      (if a.scale >= b.scale then ok a.scale else ok b.scale : Result U8) = ok t ∧
      t.val = max a.scale.val b.scale.val := by
    split_ifs with h
    · exact ⟨_, rfl, by simp at h; omega⟩
    · exact ⟨_, rfl, by simp at h; omega⟩
  obtain ⟨r1, h1, p1⟩ := rescale_correct a t
  obtain ⟨r2, h2, p2⟩ := rescale_correct b t
  rw [htv] at p1 p2
  cases r1 with
  | Err e =>
      refine ⟨.Err e, by unfold decimal.align; rw [ht]; try_simp [h1], ?_⟩
      simp only [projRescale] at p1
      simp [projAlign, alignSpec, ← p1]
  | Ok x =>
      cases r2 with
      | Err e =>
          refine ⟨.Err e, by unfold decimal.align; rw [ht]; try_simp [h1, h2], ?_⟩
          simp only [projRescale] at p1 p2
          simp [projAlign, alignSpec, ← p1, ← p2]
      | Ok y =>
          refine ⟨.Ok (x, y, t), by unfold decimal.align; rw [ht]; try_simp [h1, h2], ?_⟩
          simp only [projRescale] at p1 p2
          simp [projAlign, alignSpec, ← p1, ← p2, htv]

/-- VAL-EXACT-1 correspondence for `checked_add`, over every pair of amounts. -/
theorem checked_add_correct (a b : decimal.ScaledInteger) :
    ∃ r, decimal.checked_add a b = ok r ∧
      Outcome r (addSpec a.unscaled.val a.scale.val b.unscaled.val b.scale.val) := by
  obtain ⟨r, hr, pr⟩ := align_correct a b
  cases r with
  | Err e =>
      refine ⟨.Err e, by try_simp [decimal.checked_add, hr], ?_⟩
      simp [addSpec, ← pr, projAlign, Outcome]
  | Ok xyt =>
      obtain ⟨x, y, t⟩ := xyt
      have hadd := I128.checked_add_bv_spec x y
      cases ha : core.num.checked_add_IScalar x y with
      | none =>
          rw [ha] at hadd
          refine ⟨.Err .AdditionOverflow,
            by try_simp [decimal.checked_add, hr, I128.checked_add, ha], ?_⟩
          have hout : ¬ InI128 (x.val + y.val) := by rw [ii]; simpa using hadd
          simp [addSpec, ← pr, projAlign, Outcome, ofRust, hout]
      | some z =>
          rw [ha] at hadd
          obtain ⟨-, -, hzv, -⟩ := hadd
          obtain ⟨c, hc, hcan⟩ := canonical_spec ⟨z, t⟩
          refine ⟨.Ok c, by try_simp [decimal.checked_add, hr, I128.checked_add, ha, hc], ?_⟩
          have hin : InI128 (x.val + y.val) := by
            rw [← hzv]; have := z.hBounds; simp at this; simp [InI128]; omega
          simp only [addSpec, ← pr, projAlign, if_pos hin, Outcome]
          rw [← hzv]; exact hcan

/-- VAL-EXACT-1 correspondence for `checked_sub`, over every pair of amounts. -/
theorem checked_sub_correct (a b : decimal.ScaledInteger) :
    ∃ r, decimal.checked_sub a b = ok r ∧
      Outcome r (subSpec a.unscaled.val a.scale.val b.unscaled.val b.scale.val) := by
  obtain ⟨r, hr, pr⟩ := align_correct a b
  cases r with
  | Err e =>
      refine ⟨.Err e, by try_simp [decimal.checked_sub, hr], ?_⟩
      simp [subSpec, ← pr, projAlign, Outcome]
  | Ok xyt =>
      obtain ⟨x, y, t⟩ := xyt
      have hsub := I128.checked_sub_bv_spec x y
      cases hs : core.num.checked_sub_IScalar x y with
      | none =>
          rw [hs] at hsub
          refine ⟨.Err .SubtractionOverflow,
            by try_simp [decimal.checked_sub, hr, I128.checked_sub, hs], ?_⟩
          have hout : ¬ InI128 (x.val - y.val) := by rw [ii]; simpa using hsub
          simp [subSpec, ← pr, projAlign, Outcome, ofRust, hout]
      | some z =>
          rw [hs] at hsub
          obtain ⟨-, -, hzv, -⟩ := hsub
          obtain ⟨c, hc, hcan⟩ := canonical_spec ⟨z, t⟩
          refine ⟨.Ok c, by try_simp [decimal.checked_sub, hr, I128.checked_sub, hs, hc], ?_⟩
          have hin : InI128 (x.val - y.val) := by
            rw [← hzv]; have := z.hBounds; simp at this; simp [InI128]; omega
          simp only [subSpec, ← pr, projAlign, if_pos hin, Outcome]
          rw [← hzv]; exact hcan

/-- VAL-EXACT-1 correspondence for `checked_mul`, over every pair of amounts and
    every `u8` maximum scale. -/
theorem checked_mul_correct (a b : decimal.ScaledInteger) (m : U8) :
    ∃ r, decimal.checked_mul a b m = ok r ∧
      Outcome r (mulSpec a.unscaled.val a.scale.val b.unscaled.val b.scale.val m.val) := by
  have hsum := U8.checked_add_bv_spec a.scale b.scale
  cases hsa : a.scale.checked_add b.scale with
  | none =>
      rw [hsa] at hsum
      simp [U8.max, U8.numBits, UScalarTy.numBits] at hsum
      have h256 : 256 ≤ a.scale.val + b.scale.val := by omega
      refine ⟨.Err .MultiplicationScaleOverflow, by simp [decimal.checked_mul, hsa, lift], ?_⟩
      simp [mulSpec, Outcome, ofRust, h256]
  | some s =>
      rw [hsa] at hsum
      obtain ⟨hlt, hsv, -⟩ := hsum
      try simp only [U8.max, U8.numBits, UScalarTy.numBits] at hlt hsv ⊢
      have h256 : ¬ 256 ≤ a.scale.val + b.scale.val := by omega
      by_cases hmax : s.val ≤ m.val
      swap
      · refine ⟨.Err (.MultiplicationScaleAboveMaximum s),
          by simp [decimal.checked_mul, hsa, hmax, lift], ?_⟩
        have hm : m.val < a.scale.val + b.scale.val := by omega
        simp [mulSpec, Outcome, ofRust, h256, hm, hsv]
      · have hm : ¬ m.val < a.scale.val + b.scale.val := by omega
        have hmul := I128.checked_mul_bv_spec a.unscaled b.unscaled
        cases hp : core.num.checked_mul_IScalar a.unscaled b.unscaled with
        | none =>
            rw [hp] at hmul
            refine ⟨.Err .MultiplicationOverflow,
              by simp [decimal.checked_mul, hsa, hmax, I128.checked_mul, hp, lift], ?_⟩
            have hout : ¬ InI128 (a.unscaled.val * b.unscaled.val) := by
              rw [ii]; simpa using hmul
            simp [mulSpec, Outcome, ofRust, h256, hm, hout]
        | some z =>
            rw [hp] at hmul
            obtain ⟨-, -, hzv, -⟩ := hmul
            obtain ⟨c, hc, hcan⟩ := canonical_spec ⟨z, s⟩
            refine ⟨.Ok c,
              by simp [decimal.checked_mul, hsa, hmax, I128.checked_mul, hp, lift, hc], ?_⟩
            have hin : InI128 (a.unscaled.val * b.unscaled.val) := by
              rw [← hzv]; have := z.hBounds; simp at this; simp [InI128]; omega
            simp only [mulSpec, if_neg h256, if_neg hm, if_pos hin, Outcome]
            rw [← hzv, ← hsv]; exact hcan

end Norn.QuantityLedger
#print axioms Norn.QuantityLedger.rescale_correct
#print axioms Norn.QuantityLedger.checked_add_correct
#print axioms Norn.QuantityLedger.checked_sub_correct
#print axioms Norn.QuantityLedger.checked_mul_correct
