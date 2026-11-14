import Decimal.Funs
import DecimalReference

/-!
# Production `canonical_parts` meets VAL-EXACT-1

`Decimal` is the fresh Aeneas translation of the production file
`crates/norn-checker/src/quantity_ledger/decimal.rs`, with the two core models
of `Decimal/FunsExternal.lean`. The theorems cover every `i128` coefficient and
every `u8` scale or exponent: neither function panics or diverges, and each
returns exactly its written VAL-EXACT-1 semantics.
-/

namespace Norn.QuantityLedger
open Aeneas Aeneas.Std Result
open norn_checker_decimal

private theorem i128_eq_zero_iff (i : I128) : i = 0#i128 ↔ i.val = 0 := by
  exact IScalar.eq_equiv i 0#i128

private theorem body_done (u : I128) (s : U8) (h : ¬ 0 < s.val ∨ u.val.tmod 10 ≠ 0) :
    decimal.canonical_parts_loop.body u s = ok (ControlFlow.done (u, s)) := by
  unfold decimal.canonical_parts_loop.body
  by_cases hs : 0 < s.val
  · have hgt : s > 0#u8 := hs
    have hr : u.val.tmod 10 ≠ 0 := h.resolve_left (not_not.mpr hs)
    obtain ⟨i, hi, hiv, -⟩ := WP.spec_imp_exists (I128.rem_bv_spec u (y := 10#i128) (by simp))
    have hi0 : ¬ i = 0#i128 := by
      rw [i128_eq_zero_iff, hiv]; simpa using hr
    simp [hgt, hi, hi0]
  · have hgt : ¬ s > 0#u8 := hs
    simp only [hgt, if_false]

private theorem body_cont (u : I128) (s : U8) (hs : 0 < s.val) (hr : u.val.tmod 10 = 0) :
    ∃ q s₁, decimal.canonical_parts_loop.body u s = ok (ControlFlow.cont (q, s₁)) ∧
      q.val = u.val.tdiv 10 ∧ s₁.val = s.val - 1 := by
  unfold decimal.canonical_parts_loop.body
  have hgt : s > 0#u8 := hs
  obtain ⟨i, hi, hiv, -⟩ := WP.spec_imp_exists (I128.rem_bv_spec u (y := 10#i128) (by simp))
  have hi0 : i = 0#i128 := by
    rw [i128_eq_zero_iff, hiv]; simpa using hr
  obtain ⟨q, hq, hqv⟩ := WP.spec_imp_exists
    (I128.div_spec (x := u) (y := 10#i128) (by simp) (by simp))
  have hsub := U8.checked_sub_bv_spec s 1#u8
  cases hcs : s.checked_sub 1#u8 with
  | none =>
      rw [hcs] at hsub
      simp at hsub
      omega
  | some s₁ =>
      rw [hcs] at hsub
      obtain ⟨-, hs₁, -⟩ := hsub
      refine ⟨q, s₁, ?_, by simpa using hqv, by simpa using hs₁⟩
      simp [hgt, hi, hi0, hq, lift]

/-- Loop invariant, by strong induction on the scale: from a nonzero
    coefficient the loop returns a canonical form of its input. -/
private theorem loop_spec (n : ℕ) : ∀ (u : I128) (s : U8), s.val = n → u.val ≠ 0 →
    ∃ c s', decimal.canonical_parts_loop u s = ok (c, s') ∧
      s'.val ≤ s.val ∧ c.val * 10 ^ (s.val - s'.val) = u.val ∧
      (0 < s'.val → ¬ (10 : ℤ) ∣ c.val) ∧ c.val ≠ 0 := by
  induction n using Nat.strong_induction_on with
  | _ n ih =>
    intro u s hn hu
    unfold decimal.canonical_parts_loop
    rw [loop.eq_1]
    dsimp only
    by_cases hstop : ¬ 0 < s.val ∨ u.val.tmod 10 ≠ 0
    · rw [body_done u s hstop]
      refine ⟨u, s, rfl, le_refl _, by simp, ?_, hu⟩
      intro hs hd
      rcases hstop with h | h
      · exact h hs
      · exact h (Int.tmod_eq_zero_of_dvd hd)
    · have hs : 0 < s.val := by tauto
      have hr : u.val.tmod 10 = 0 := by tauto
      obtain ⟨q, s₁, hbody, hq, hs₁⟩ := body_cont u s hs hr
      rw [hbody]
      dsimp only
      have hmul : q.val * 10 = u.val := by
        rw [hq]; exact Int.tdiv_mul_cancel (Int.dvd_of_tmod_eq_zero hr)
      have hq0 : q.val ≠ 0 := by
        intro h; apply hu; rw [← hmul, h]; simp
      obtain ⟨c, s', hloop, hle, hval, hcan, hc0⟩ :=
        ih s₁.val (by omega) q s₁ rfl hq0
      refine ⟨c, s', hloop, by omega, ?_, hcan, hc0⟩
      have hexp : s.val - s'.val = (s₁.val - s'.val) + 1 := by omega
      rw [hexp, pow_succ, ← mul_assoc, hval, hmul]

/-- VAL-EXACT-1 correspondence: for every `i128` coefficient and `u8` scale the
    extracted production `canonical_parts` returns (without panic or
    divergence) the canonical form of its input. -/
theorem canonical_parts_correct (u : I128) (s : U8) :
    ∃ c s', decimal.canonical_parts u s = ok (c, s') ∧
      IsCanonicalForm u.val s.val c.val s'.val := by
  unfold decimal.canonical_parts
  by_cases hu : u = 0#i128
  · rw [if_pos hu]
    have hv : u.val = 0 := (i128_eq_zero_iff u).mp hu
    refine ⟨0#i128, 0#u8, rfl, Nat.zero_le _, by simp [hv], fun _ => rfl, ?_⟩
    intro h; simp at h
  · rw [if_neg hu]
    have hv : u.val ≠ 0 := fun h => hu ((i128_eq_zero_iff u).mpr h)
    obtain ⟨c, s', hloop, hle, hval, hcan, hc0⟩ := loop_spec s.val u s rfl hv
    exact ⟨c, s', hloop, hle, hval, fun h => absurd h hc0, hcan⟩

/-- The production result is the unique canonical form: any canonical form of
    the input is exactly what `canonical_parts` returns. -/
theorem canonical_parts_unique (u : I128) (s : U8) (c : ℤ) (s' : ℕ)
    (h : IsCanonicalForm u.val s.val c s') :
    ∃ c₀ s₀, decimal.canonical_parts u s = ok (c₀, s₀) ∧ c₀.val = c ∧ s₀.val = s' := by
  obtain ⟨c₀, s₀, hrun, hcan⟩ := canonical_parts_correct u s
  obtain ⟨hc, hs⟩ := IsCanonicalForm.unique hcan h
  exact ⟨c₀, s₀, hrun, hc, hs⟩

private theorem pow10_loop (k : ℕ) : ∀ (a : ℕ) (v : I128) (rem : U8), rem.val = k →
    v.val = 10 ^ a →
    ∃ r, decimal.checked_pow10_loop v rem = ok r ∧
      r.map (fun x => x.val) = pow10Spec (a + k) := by
  induction k with
  | zero =>
      intro a v rem hk hv
      have hsub := U8.checked_sub_bv_spec rem 1#u8
      cases hcs : rem.checked_sub 1#u8 with
      | some r' => rw [hcs] at hsub; simp at hsub; omega
      | none =>
          refine ⟨some v, ?_, ?_⟩
          · unfold decimal.checked_pow10_loop
            rw [loop.eq_1]
            simp [decimal.checked_pow10_loop.body, hcs, lift]
          · have hb := v.hBounds
            simp at hb
            simp [pow10Spec, hv]
            omega
  | succ k ih =>
      intro a v rem hk hv
      have hsub := U8.checked_sub_bv_spec rem 1#u8
      cases hcs : rem.checked_sub 1#u8 with
      | none => rw [hcs] at hsub; simp at hsub; omega
      | some r' =>
          rw [hcs] at hsub
          obtain ⟨-, hr', -⟩ := hsub
          have hmul := I128.checked_mul_bv_spec v 10#i128
          cases hm : core.num.checked_mul_IScalar v 10#i128 with
          | none =>
              rw [hm] at hmul
              refine ⟨none, ?_, ?_⟩
              · unfold decimal.checked_pow10_loop
                rw [loop.eq_1]
                simp [decimal.checked_pow10_loop.body, hcs, I128.checked_mul, hm, lift,
                  core.option.Option.Insts.CoreOpsTry_traitTry.branch,
                  core.option.Option.Insts.CoreOpsTry_traitFromResidualOptionInfallible.from_residual]
              · simp [I128.min, I128.max, I128.numBits, hv] at hmul
                have hle : (10 : ℤ) ^ a * 10 ≤ 10 ^ (a + (k + 1)) := by
                  rw [← pow_succ]; exact pow_le_pow_right₀ (by norm_num) (by omega)
                have hpos : (0 : ℤ) ≤ 10 ^ a := by positivity
                simp only [pow10Spec, Option.map_none]
                rw [if_neg (by omega)]
          | some v' =>
              rw [hm] at hmul
              obtain ⟨-, -, hv', -⟩ := hmul
              obtain ⟨r, hrun, hr⟩ :=
                ih (a + 1) v' r' (by simp at hr'; omega) (by rw [hv', hv, pow_succ]; rfl)
              refine ⟨r, ?_, ?_⟩
              · unfold decimal.checked_pow10_loop
                rw [loop.eq_1]
                simp [decimal.checked_pow10_loop.body, hcs, I128.checked_mul, hm, lift,
                  core.option.Option.Insts.CoreOpsTry_traitTry.branch,
                  core.option.Option.Insts.CoreOpsTry_traitFromResidualOptionInfallible.from_residual]
                exact hrun
              · rw [hr]
                congr 1
                omega

/-- VAL-EXACT-1 correspondence: for every `u8` exponent the extracted production
    `checked_pow10` returns (without panic or divergence) exactly `10^e`, or
    `None` exactly when `10^e` exceeds the signed 128-bit range. -/
theorem checked_pow10_correct (e : U8) :
    ∃ r, decimal.checked_pow10 e = ok r ∧ r.map (fun x => x.val) = pow10Spec e.val := by
  obtain ⟨r, hrun, hr⟩ := pow10_loop e.val 0 1#i128 e rfl (by simp)
  exact ⟨r, hrun, by simpa using hr⟩

end Norn.QuantityLedger
#print axioms Norn.QuantityLedger.canonical_parts_correct
#print axioms Norn.QuantityLedger.canonical_parts_unique
#print axioms Norn.QuantityLedger.checked_pow10_correct
