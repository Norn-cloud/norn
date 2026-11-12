import RoundingReference

/-!
# Truncated division and remainder decide FOLD-ARITHMETIC rounding

`viaRemainder` restates, over unbounded integers, the decision the production
`ScaledArithmetic::quotient` makes from Rust's truncated quotient and remainder.
`roundRat_eq_viaRemainder` proves it equals the written rational semantics.
This file never mentions the extracted code.
-/

namespace Norn.OrderedFold

/-- The remainder-based decision, in the shape of the production code. -/
def viaRemainder (mode : Mode) (n d : ℤ) : Option ℤ :=
  if n.tmod d = 0 then some (n.tdiv d)
  else
    match mode with
    | .refuse => none
    | .towardZero => some (n.tdiv d)
    | .halfEven =>
        if d.natAbs - (n.tmod d).natAbs < (n.tmod d).natAbs ∨
            ((n.tmod d).natAbs = d.natAbs - (n.tmod d).natAbs ∧ (n.tdiv d).tmod 2 ≠ 0) then
          some (n.tdiv d + if decide (n < 0) = decide (d < 0) then 1 else -1)
        else some (n.tdiv d)
    | .halfAwayFromZero =>
        if d.natAbs - (n.tmod d).natAbs < (n.tmod d).natAbs ∨
            (n.tmod d).natAbs = d.natAbs - (n.tmod d).natAbs then
          some (n.tdiv d + if decide (n < 0) = decide (d < 0) then 1 else -1)
        else some (n.tdiv d)

private theorem no_int_strictly_between (k : ℤ) (lo : ℤ) (h1 : ((lo : ℤ) : ℚ) < k)
    (h2 : (k : ℚ) < lo + 1) : False := by
  have a : lo < k := by exact_mod_cast h1
  have b : k < lo + 1 := by exact_mod_cast h2
  omega

private theorem den_ne_one_of_frac (t : ℤ) (e : ℚ) (lo : ℤ) (_h1 : (lo : ℚ) < e)
    (_h2 : e < lo + 1) (h3 : ¬ ∃ z : ℤ, (z : ℚ) = e) : (↑t + e : ℚ).den ≠ 1 := by
  intro hden
  have hq := Rat.num_div_den ((t : ℚ) + e)
  rw [hden] at hq
  simp only [Nat.cast_one, div_one] at hq
  exact h3 ⟨(↑t + e : ℚ).num - t, by push_cast; linarith⟩

/-- Rounding of `t + e` for a nonzero fractional part `e` with the sign of the
    quotient. -/
theorem roundRat_frac (mode : Mode) (t : ℤ) (e : ℚ) (h1 : -1 < e) (h2 : e < 1)
    (hne : e ≠ 0) (hsign : 0 ≤ e * (t + e)) :
    roundRat mode (t + e) =
      match mode with
      | .refuse => none
      | .towardZero => some t
      | .halfEven =>
          if 1 / 2 < |e| ∨ (|e| = 1 / 2 ∧ ¬ Even t) then
            some (t + if 0 < e then 1 else -1)
          else some t
      | .halfAwayFromZero =>
          if 1 / 2 ≤ |e| then some (t + if 0 < e then 1 else -1) else some t := by
  rcases lt_or_gt_of_ne hne with hneg | hpos
  · -- negative fractional part: the quotient is negative
    have hle : (t : ℚ) + e ≤ 0 := by nlinarith
    have hlt : (t : ℚ) + e < 0 := by
      rcases lt_or_eq_of_le hle with h | h
      · exact h
      · exact (no_int_strictly_between t 0 (by push_cast; linarith) (by push_cast; linarith)).elim
    have hfloor : ⌊(t : ℚ) + e⌋ = t - 1 :=
      Int.floor_eq_iff.mpr ⟨by push_cast; linarith, by push_cast; linarith⟩
    have hceil : ⌈(t : ℚ) + e⌉ = t :=
      Int.ceil_eq_iff.mpr ⟨by linarith, by linarith⟩
    have habs : |e| = -e := abs_of_neg hneg
    have hnotint : ¬ ∃ z : ℤ, (z : ℚ) = e := by
      rintro ⟨z, hz⟩
      exact no_int_strictly_between z (-1) (by push_cast; linarith) (by push_cast; linarith)
    cases mode with
    | refuse =>
        simp only [roundRat]
        rw [if_neg (den_ne_one_of_frac t e (-1) (by push_cast; linarith)
          (by push_cast; linarith) hnotint)]
    | towardZero =>
        simp only [roundRat]
        rw [if_neg (not_le.mpr hlt), hceil]
    | halfEven =>
        simp only [roundRat, hfloor, habs]
        have hfrac : (t : ℚ) + e - ((t - 1 : ℤ) : ℚ) = e + 1 := by push_cast; ring
        rw [hfrac, if_neg (not_lt.mpr hneg.le)]
        rcases lt_trichotomy e (-1 / 2) with h | h | h
        · rw [if_pos (by linarith), if_pos (Or.inl (by linarith))]
          congr 1
        · subst h
          rw [if_neg (by norm_num), if_neg (by norm_num)]
          by_cases he : Even t
          · have : ¬ Even (t - 1) := Int.even_sub_one.not.mpr (not_not.mpr he)
            rw [if_neg this, if_neg (by rintro (h | ⟨_, h⟩) <;> [norm_num at h; exact h he])]
            ring_nf
          · have : Even (t - 1) := Int.even_sub_one.mpr he
            rw [if_pos this, if_pos (Or.inr ⟨by norm_num, he⟩)]
            congr 1
        · rw [if_neg (by linarith), if_pos (by linarith),
            if_neg (by rintro (h' | ⟨h', _⟩) <;> linarith)]
          congr 1; ring
    | halfAwayFromZero =>
        simp only [roundRat, habs]
        rw [if_neg (not_le.mpr hlt)]
        by_cases h : 1 / 2 ≤ -e
        · have : ⌊-((t : ℚ) + e) + 1 / 2⌋ = -t + 1 :=
            Int.floor_eq_iff.mpr ⟨by push_cast; linarith, by push_cast; linarith⟩
          rw [this, if_pos h, if_neg (not_lt.mpr hneg.le)]
          congr 1; ring
        · have : ⌊-((t : ℚ) + e) + 1 / 2⌋ = -t :=
            Int.floor_eq_iff.mpr ⟨by push_cast; linarith, by push_cast; linarith⟩
          rw [this, if_neg h]
          congr 1; ring
  · -- positive fractional part: the quotient is positive
    have hge : 0 ≤ (t : ℚ) + e := (mul_nonneg_iff_of_pos_left hpos).mp hsign
    have hfloor : ⌊(t : ℚ) + e⌋ = t :=
      Int.floor_eq_iff.mpr ⟨by linarith, by linarith⟩
    have habs : |e| = e := abs_of_pos hpos
    have hnotint : ¬ ∃ z : ℤ, (z : ℚ) = e := by
      rintro ⟨z, hz⟩
      exact no_int_strictly_between z 0 (by push_cast; linarith) (by push_cast; linarith)
    cases mode with
    | refuse =>
        simp only [roundRat]
        rw [if_neg (den_ne_one_of_frac t e 0 (by push_cast; linarith)
          (by push_cast; linarith) hnotint)]
    | towardZero =>
        simp only [roundRat]
        rw [if_pos hge, hfloor]
    | halfEven =>
        simp only [roundRat, hfloor, habs]
        have hfrac : (t : ℚ) + e - (t : ℚ) = e := by ring
        rw [hfrac, if_pos hpos]
        rcases lt_trichotomy e (1 / 2) with h | h | h
        · rw [if_pos h, if_neg (by rintro (h' | ⟨h', _⟩) <;> linarith)]
        · subst h
          rw [if_neg (by norm_num), if_neg (by norm_num)]
          by_cases he : Even t
          · rw [if_pos he, if_neg (by rintro (h | ⟨_, h⟩) <;> [norm_num at h; exact h he])]
          · rw [if_neg he, if_pos (Or.inr ⟨rfl, he⟩)]
        · rw [if_neg (by linarith), if_pos h, if_pos (Or.inl h)]
    | halfAwayFromZero =>
        simp only [roundRat, habs]
        rw [if_pos hge, if_pos hpos]
        by_cases h : 1 / 2 ≤ e
        · have : ⌊(t : ℚ) + e + 1 / 2⌋ = t + 1 :=
            Int.floor_eq_iff.mpr ⟨by push_cast; linarith, by push_cast; linarith⟩
          rw [this, if_pos h]
        · have : ⌊(t : ℚ) + e + 1 / 2⌋ = t :=
            Int.floor_eq_iff.mpr ⟨by linarith, by linarith⟩
          rw [this, if_neg h]

/-- An integral quotient is returned unchanged by every mode. -/
theorem roundRat_int (mode : Mode) (t : ℤ) : roundRat mode (t : ℚ) = some t := by
  cases mode with
  | refuse => simp [roundRat]
  | towardZero =>
      simp only [roundRat]
      split_ifs <;> simp
  | halfEven =>
      simp only [roundRat, Int.floor_intCast, sub_self]
      rw [if_pos (by norm_num)]
  | halfAwayFromZero =>
      simp only [roundRat]
      split_ifs with h
      · congr 1
        exact Int.floor_eq_iff.mpr ⟨by linarith, by linarith⟩
      · have : ⌊-(t : ℚ) + 1 / 2⌋ = -t :=
          Int.floor_eq_iff.mpr ⟨by push_cast; linarith, by push_cast; linarith⟩
        rw [this, neg_neg]

/-- The production decision equals the written semantics for every nonzero
    divisor. -/
theorem roundRat_eq_viaRemainder (mode : Mode) (n d : ℤ) (hd : d ≠ 0) :
    roundRat mode ((n : ℚ) / d) = viaRemainder mode n d := by
  have hdq : (d : ℚ) ≠ 0 := by exact_mod_cast hd
  have hdef : n.tmod d = n - d * n.tdiv d := Int.tmod_def n d
  have hnat : (n.tmod d).natAbs < d.natAbs := by
    rw [Int.natAbs_tmod]; exact Nat.mod_lt _ (Int.natAbs_pos.mpr hd)
  have hrn : 0 ≤ n.tmod d * n := by
    rcases le_or_gt 0 n with hn | hn
    · exact mul_nonneg (Int.tmod_nonneg d hn) hn
    · have h0 := Int.tmod_nonneg d (by omega : 0 ≤ -n)
      rw [Int.neg_tmod] at h0
      exact mul_nonneg_of_nonpos_of_nonpos (by omega) hn.le
  have hzero : n = 0 → n.tmod d = 0 := by intro hn; rw [hn]; simp
  unfold viaRemainder
  generalize hr : n.tmod d = r at *
  generalize ht : n.tdiv d = t at *
  have hq : (n : ℚ) / d = t + (r : ℚ) / d := by
    have hn : (n : ℚ) = r + d * t := by exact_mod_cast (by rw [hdef]; ring : n = r + d * t)
    rw [hn]; field_simp; ring
  generalize he : (r : ℚ) / d = e at hq
  have hdpos : (0 : ℚ) < |(d : ℚ)| := abs_pos.mpr hdq
  have habsr : |(r : ℚ)| = (r.natAbs : ℚ) := by rw [Nat.cast_natAbs, Int.cast_abs]
  have habsd : |(d : ℚ)| = (d.natAbs : ℚ) := by rw [Nat.cast_natAbs, Int.cast_abs]
  have habse : |e| = (r.natAbs : ℚ) / (d.natAbs : ℚ) := by
    rw [← he, abs_div, habsr, habsd]
  have hDpos : (0 : ℚ) < (d.natAbs : ℚ) := by rw [← habsd]; exact hdpos
  have hlt1 : |e| < 1 := by
    rw [habse, div_lt_one hDpos]; exact_mod_cast hnat
  have hsign : 0 ≤ e * (t + e) := by
    rw [← hq, ← he, div_mul_div_comm]
    apply div_nonneg
    · exact_mod_cast hrn
    · exact mul_self_nonneg _
  rw [hq]
  by_cases hr0 : r = 0
  · rw [if_pos hr0, ← he, hr0]
    simp only [Int.cast_zero, zero_div, add_zero]
    exact roundRat_int mode t
  rw [if_neg hr0]
  have hne : e ≠ 0 := by rw [← he]; exact div_ne_zero (by exact_mod_cast hr0) hdq
  rw [roundRat_frac mode t e (abs_lt.mp hlt1).1 (abs_lt.mp hlt1).2 hne hsign]
  have hgt : (1 / 2 < |e|) ↔ d.natAbs - r.natAbs < r.natAbs := by
    rw [habse, lt_div_iff₀ hDpos]
    constructor
    · intro h
      have h' : (d.natAbs : ℚ) < 2 * r.natAbs := by linarith
      have : d.natAbs < 2 * r.natAbs := by exact_mod_cast h'
      omega
    · intro h
      have : d.natAbs < 2 * r.natAbs := by omega
      have h' : (d.natAbs : ℚ) < 2 * r.natAbs := by exact_mod_cast this
      linarith
  have heq : (|e| = 1 / 2) ↔ r.natAbs = d.natAbs - r.natAbs := by
    rw [habse, div_eq_iff hDpos.ne']
    constructor
    · intro h
      have h' : (2 * r.natAbs : ℚ) = d.natAbs := by linarith
      have : 2 * r.natAbs = d.natAbs := by exact_mod_cast h'
      omega
    · intro h
      have : 2 * r.natAbs = d.natAbs := by omega
      have h' : (2 * r.natAbs : ℚ) = d.natAbs := by exact_mod_cast this
      linarith
  have hge : (1 / 2 ≤ |e|) ↔
      (d.natAbs - r.natAbs < r.natAbs ∨ r.natAbs = d.natAbs - r.natAbs) := by
    rw [← hgt, ← heq]
    constructor
    · intro h
      rcases h.lt_or_eq with h | h
      exacts [Or.inl h, Or.inr h.symm]
    · rintro (h | h)
      exacts [h.le, h.ge]
  have hpar : (¬ Even t) ↔ t.tmod 2 ≠ 0 := by
    rw [Ne, ← Int.natAbs_eq_zero, Int.natAbs_tmod, ← Int.natAbs_even, Nat.even_iff]
    simp
  have hn : n ≠ 0 := fun hn => hr0 (hzero hn)
  have hrn' : 0 < r * n := lt_of_le_of_ne hrn (Ne.symm (mul_ne_zero hr0 hn))
  have hdir : (0 < e) ↔ (decide (n < 0) = decide (d < 0)) := by
    rw [← he]
    rcases lt_or_gt_of_ne hn with hn' | hn' <;> rcases lt_or_gt_of_ne hd with hd' | hd'
    · have hr' : r < 0 := by nlinarith
      exact ⟨fun _ => by simp [hn', hd'],
        fun _ => div_pos_of_neg_of_neg (by exact_mod_cast hr') (by exact_mod_cast hd')⟩
    · have hr' : r < 0 := by nlinarith
      have hneg : (r : ℚ) / d < 0 :=
        div_neg_of_neg_of_pos (by exact_mod_cast hr') (by exact_mod_cast hd')
      exact ⟨fun h => absurd h (not_lt.mpr hneg.le),
        fun h => by simp [hn', not_lt.mpr hd'.le] at h⟩
    · have hr' : 0 < r := by nlinarith
      have hneg : (r : ℚ) / d < 0 :=
        div_neg_of_pos_of_neg (by exact_mod_cast hr') (by exact_mod_cast hd')
      exact ⟨fun h => absurd h (not_lt.mpr hneg.le),
        fun h => by simp [hd', not_lt.mpr hn'.le] at h⟩
    · have hr' : 0 < r := by nlinarith
      exact ⟨fun _ => by simp [not_lt.mpr hn'.le, not_lt.mpr hd'.le],
        fun _ => div_pos (by exact_mod_cast hr') (by exact_mod_cast hd')⟩
  cases mode with
  | refuse => rfl
  | towardZero => rfl
  | halfEven =>
      have hiff : (1 / 2 < |e| ∨ (|e| = 1 / 2 ∧ ¬ Even t)) ↔
          (d.natAbs - r.natAbs < r.natAbs ∨
            (r.natAbs = d.natAbs - r.natAbs ∧ t.tmod 2 ≠ 0)) := by
        rw [hgt, heq, hpar]
      show (if 1 / 2 < |e| ∨ (|e| = 1 / 2 ∧ ¬ Even t) then
            some (t + if 0 < e then 1 else -1) else some t) =
          (if d.natAbs - r.natAbs < r.natAbs ∨
              (r.natAbs = d.natAbs - r.natAbs ∧ t.tmod 2 ≠ 0) then
            some (t + if decide (n < 0) = decide (d < 0) then 1 else -1) else some t)
      by_cases hc : d.natAbs - r.natAbs < r.natAbs ∨
          (r.natAbs = d.natAbs - r.natAbs ∧ t.tmod 2 ≠ 0)
      · rw [if_pos (hiff.mpr hc), if_pos hc]
        by_cases hp : 0 < e
        · rw [if_pos hp, if_pos (hdir.mp hp)]
        · rw [if_neg hp, if_neg (fun h => hp (hdir.mpr h))]
      · rw [if_neg (fun h => hc (hiff.mp h)), if_neg hc]
  | halfAwayFromZero =>
      show (if 1 / 2 ≤ |e| then some (t + if 0 < e then 1 else -1) else some t) =
          (if d.natAbs - r.natAbs < r.natAbs ∨ r.natAbs = d.natAbs - r.natAbs then
            some (t + if decide (n < 0) = decide (d < 0) then 1 else -1) else some t)
      by_cases hc : d.natAbs - r.natAbs < r.natAbs ∨ r.natAbs = d.natAbs - r.natAbs
      · rw [if_pos (hge.mpr hc), if_pos hc]
        by_cases hp : 0 < e
        · rw [if_pos hp, if_pos (hdir.mp hp)]
        · rw [if_neg hp, if_neg (fun h => hp (hdir.mpr h))]
      · rw [if_neg (fun h => hc (hge.mp h)), if_neg hc]

end Norn.OrderedFold
