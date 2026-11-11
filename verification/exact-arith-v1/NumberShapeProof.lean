import NumberShape.Funs
import NumberShapeReference

/-!
# Production JSON number grammar check meets RFC 8259

`NumberShape` is the fresh Aeneas translation of the production file
`crates/norn-canonical/src/number/shape.rs`, with the core model of
`u8::is_ascii_digit` from `CoreModels.lean`. `json_number_shape_correct`
covers every byte slice. The binary64 value of an accepted literal comes from
`serde_json`'s float parser, which is trusted and not covered here.
-/

namespace Norn.Codec
open Aeneas Aeneas.Std Result
open norn_canonical_shape

/-- The bytes of a slice, as naturals. -/
def bytesOf (s : Slice Std.U8) : List ℕ := s.val.map (·.val)

private theorem bytesOf_length (s : Slice Std.U8) : (bytesOf s).length = s.length := by
  simp [bytesOf]

private theorem drop_nil (s : Slice Std.U8) (c : ℕ) (h : s.length ≤ c) :
    (bytesOf s).drop c = [] := by
  apply List.drop_eq_nil_of_le; rw [bytesOf_length]; exact h

private theorem drop_nil_iff (s : Slice Std.U8) (c : ℕ) (h : c ≤ s.length) :
    (bytesOf s).drop c = [] ↔ c = s.length := by
  rw [List.drop_eq_nil_iff, bytesOf_length]; omega

private theorem index_eval (s : Slice Std.U8) (c : Usize) (h : c.val < s.length) :
    ∃ x : Std.U8, Slice.index_usize s c = ok x ∧
      (bytesOf s).drop c.val = x.val :: (bytesOf s).drop (c.val + 1) := by
  obtain ⟨x, hx, hxv⟩ := WP.spec_imp_exists (Slice.index_usize_spec s c h)
  refine ⟨x, hx, ?_⟩
  have h' : c.val < (bytesOf s).length := by rw [bytesOf_length]; exact h
  rw [List.drop_eq_getElem_cons h', hxv]
  simp [bytesOf]

private theorem succ_eval (s : Slice Std.U8) (c : Usize) (h : c.val < s.length) :
    ∃ d : Usize, c + 1#usize = ok d ∧ d.val = c.val + 1 := by
  obtain ⟨d, hd, hdv⟩ := WP.spec_imp_exists
    (UScalar.add_spec (x := c) (y := 1#usize) (by scalar_tac))
  exact ⟨d, hd, by simpa using hdv⟩

private theorem digit_eval (x : Std.U8) : core.num.U8.is_ascii_digit x = ok (isDigit x.val) := by
  simp [core.num.U8.is_ascii_digit, isDigit]

private theorem u8_eq (x : Std.U8) (n : Std.U8) : x = n ↔ x.val = n.val := UScalar.eq_equiv x n

private theorem lt_len (s : Slice Std.U8) (c : Usize) : c < Slice.len s ↔ c.val < s.length := by
  simp [UScalar.lt_equiv]

/-- `skip_digits` stops at the first non-digit at or after the cursor. -/
private theorem skip_spec (s : Slice Std.U8) : ∀ (k : ℕ) (c : Usize), s.length - c.val = k →
    c.val ≤ s.length →
    ∃ d : Usize, shape.skip_digits s c = ok d ∧ d.val ≤ s.length ∧
      (bytesOf s).drop d.val = ((bytesOf s).drop c.val).dropWhile isDigit ∧
      d.val = c.val + (((bytesOf s).drop c.val).takeWhile isDigit).length := by
  intro k
  induction k with
  | zero =>
    intro c hk hc
    have hcl : c.val = s.length := by omega
    have hnot : ¬ c < Slice.len s := by rw [lt_len]; omega
    refine ⟨c, ?_, hc, ?_, ?_⟩
    · unfold shape.skip_digits shape.skip_digits_loop
      rw [loop.eq_1]
      simp [shape.skip_digits_loop.body, hnot]
    · rw [drop_nil s c.val (by omega)]; rfl
    · rw [drop_nil s c.val (by omega)]; rfl
  | succ k ih =>
    intro c hk hc
    have hlt : c.val < s.length := by omega
    obtain ⟨x, hx, hdrop⟩ := index_eval s c hlt
    by_cases hd : isDigit x.val = true
    · obtain ⟨c', hc', hc'v⟩ := succ_eval s c hlt
      obtain ⟨d, hrun, hdl, hdd, hdv⟩ := ih c' (by omega) (by omega)
      refine ⟨d, ?_, hdl, ?_, ?_⟩
      · unfold shape.skip_digits shape.skip_digits_loop
        rw [loop.eq_1]
        simp only [shape.skip_digits_loop.body, (lt_len s c).mpr hlt, if_true, hx, bind_tc_ok,
          digit_eval, hd, hc']
        exact hrun
      · rw [hdd, hdrop, List.dropWhile_cons, if_pos hd, hc'v]
      · rw [hdv, hdrop, List.takeWhile_cons, if_pos hd, hc'v]; simp; omega
    · have hd' : isDigit x.val = false := by simpa using hd
      refine ⟨c, ?_, hc, ?_, ?_⟩
      · unfold shape.skip_digits shape.skip_digits_loop
        rw [loop.eq_1]
        simp [shape.skip_digits_loop.body, (lt_len s c).mpr hlt, hx, digit_eval, hd']
      · rw [hdrop, List.dropWhile_cons, if_neg (by simpa using hd')]
      · rw [hdrop, List.takeWhile_cons, if_neg (by simpa using hd')]; simp

private theorem skip_eval (s : Slice Std.U8) (c : Usize) (hc : c.val ≤ s.length) :
    ∃ d : Usize, shape.skip_digits s c = ok d ∧ d.val ≤ s.length ∧
      (bytesOf s).drop d.val = ((bytesOf s).drop c.val).dropWhile isDigit ∧
      d.val = c.val + (((bytesOf s).drop c.val).takeWhile isDigit).length :=
  skip_spec s _ c rfl hc

private theorem sign_eval (s : Slice Std.U8) (c : Usize) (hc : c.val ≤ s.length) :
    ∃ d : Usize, shape.after_sign s c = ok d ∧ d.val ≤ s.length ∧
      (bytesOf s).drop d.val = afterSign ((bytesOf s).drop c.val) := by
  by_cases hlt : c.val < s.length
  · obtain ⟨x, hx, hdrop⟩ := index_eval s c hlt
    by_cases hm : x.val = 45
    · obtain ⟨d, hd, hdv⟩ := succ_eval s c hlt
      have hx45 : x = 45#u8 := by rw [u8_eq]; simpa using hm
      refine ⟨d, ?_, by omega, ?_⟩
      · simp [shape.after_sign, (lt_len s c).mpr hlt, hx, hx45, hd]
      · rw [hdrop, hdv]; simp [afterSign, hm]
    · have hx45 : ¬ x = 45#u8 := by rw [u8_eq]; simpa using hm
      refine ⟨c, ?_, hc, ?_⟩
      · simp [shape.after_sign, (lt_len s c).mpr hlt, hx, hx45]
      · rw [hdrop]; simp [afterSign, hm]
  · have hnot : ¬ c < Slice.len s := by rw [lt_len]; exact hlt
    refine ⟨c, by simp [shape.after_sign, hnot], hc, ?_⟩
    rw [drop_nil s c.val (by omega)]; rfl

private theorem exp_sign_eval (s : Slice Std.U8) (c : Usize) (hc : c.val ≤ s.length) :
    ∃ d : Usize, shape.after_exponent_sign s c = ok d ∧ d.val ≤ s.length ∧
      (bytesOf s).drop d.val = afterExpSign ((bytesOf s).drop c.val) := by
  by_cases hlt : c.val < s.length
  · obtain ⟨x, hx, hdrop⟩ := index_eval s c hlt
    by_cases hm : x.val = 43 ∨ x.val = 45
    · obtain ⟨d, hd, hdv⟩ := succ_eval s c hlt
      refine ⟨d, ?_, by omega, ?_⟩
      · rcases hm with hm | hm
        · have h43 : x = 43#u8 := by rw [u8_eq]; simpa using hm
          simp [shape.after_exponent_sign, (lt_len s c).mpr hlt, hx, h43, hd]
        · have h43 : ¬ x = 43#u8 := by rw [u8_eq]; simp; omega
          have h45 : x = 45#u8 := by rw [u8_eq]; simpa using hm
          simp [shape.after_exponent_sign, (lt_len s c).mpr hlt, hx, h45, hd]
      · rw [hdrop, hdv]; simp [afterExpSign, hm]
    · have h43 : ¬ x = 43#u8 := by rw [u8_eq]; simp; omega
      have h45 : ¬ x = 45#u8 := by rw [u8_eq]; simp; omega
      refine ⟨c, ?_, hc, ?_⟩
      · simp [shape.after_exponent_sign, (lt_len s c).mpr hlt, hx, h43, h45]
      · rw [hdrop]; simp [afterExpSign, hm]
  · have hnot : ¬ c < Slice.len s := by rw [lt_len]; exact hlt
    refine ⟨c, by simp [shape.after_exponent_sign, hnot], hc, ?_⟩
    rw [drop_nil s c.val (by omega)]; rfl

private theorem integer_eval (s : Slice Std.U8) (c : Usize) (hc : c.val ≤ s.length) :
    ∃ r : Option Usize, shape.after_integer s c = ok r ∧
      r.map (fun d => (bytesOf s).drop d.val) = afterInt ((bytesOf s).drop c.val) ∧
      ∀ d, r = some d → d.val ≤ s.length := by
  by_cases hlt : c.val < s.length
  · have hge : ¬ c ≥ Slice.len s := by
      intro h; have := (lt_len s c).mpr hlt; exact absurd this (not_lt.mpr h)
    obtain ⟨x, hx, hdrop⟩ := index_eval s c hlt
    obtain ⟨c', hc', hc'v⟩ := succ_eval s c hlt
    by_cases h0 : x.val = 48
    · have hx48 : x = 48#u8 := by rw [u8_eq]; simpa using h0
      refine ⟨some c', by simp [shape.after_integer, hge, hx, hx48, hc'], ?_, ?_⟩
      · rw [hdrop]; simp [afterInt, h0, hc'v]
      · intro d hd; cases hd; omega
    · have hx48 : ¬ x = 48#u8 := by rw [u8_eq]; simpa using h0
      by_cases hd : isDigit x.val = true
      · obtain ⟨d, hrun, hdl, hdd, -⟩ := skip_eval s c' (by omega)
        refine ⟨some d, ?_, ?_, ?_⟩
        · simp [shape.after_integer, hge, hx, hx48, digit_eval, hd, hc', hrun]
        · rw [hdrop]; simp [afterInt, h0, hd, hdd, hc'v]
        · intro e he; cases he; exact hdl
      · have hd' : isDigit x.val = false := by simpa using hd
        refine ⟨none, by simp [shape.after_integer, hge, hx, hx48, digit_eval, hd'], ?_, ?_⟩
        · rw [hdrop]; simp [afterInt, h0, hd']
        · intro d hd; cases hd
  · have hge : c ≥ Slice.len s := by
      have : ¬ c < Slice.len s := by rw [lt_len]; exact hlt
      exact not_lt.mp this
    refine ⟨none, by simp [shape.after_integer, hge], ?_, ?_⟩
    · rw [drop_nil s c.val (by omega)]; rfl
    · intro d hd; cases hd

private theorem fraction_eval (s : Slice Std.U8) (c : Usize) (hc : c.val ≤ s.length) :
    ∃ r : Option (Usize × Bool), shape.after_fraction s c = ok r ∧
      r.map (fun p => ((bytesOf s).drop p.1.val, p.2)) = afterFrac ((bytesOf s).drop c.val) ∧
      ∀ p, r = some p → p.1.val ≤ s.length := by
  by_cases hlt : c.val < s.length
  · obtain ⟨x, hx, hdrop⟩ := index_eval s c hlt
    by_cases h46 : x.val = 46
    · have hx46 : x = 46#u8 := by rw [u8_eq]; simpa using h46
      obtain ⟨c', hc', hc'v⟩ := succ_eval s c hlt
      obtain ⟨d, hrun, hdl, hdd, hdv⟩ := skip_eval s c' (by omega)
      by_cases hempty : d = c'
      · refine ⟨none, by simp [shape.after_fraction, (lt_len s c).mpr hlt, hx, h46, hc', hrun,
          hempty], ?_, ?_⟩
        · have hz : (((bytesOf s).drop (c.val + 1)).takeWhile isDigit).length = 0 := by
            subst hempty; rw [hc'v] at hdv; omega
          rw [hdrop]; simp [afterFrac, hz, h46]
        · intro p hp; cases hp
      · refine ⟨some (d, true), by simp [shape.after_fraction, (lt_len s c).mpr hlt, hx, h46,
          hc', hrun, hempty], ?_, ?_⟩
        · have hz : (((bytesOf s).drop (c.val + 1)).takeWhile isDigit).length ≠ 0 := by
            intro hz; apply hempty; rw [UScalar.eq_equiv]; rw [hc'v] at hdv ⊢; omega
          rw [hdrop]; simp [afterFrac, hz, hdd, hc'v, h46]
        · intro p hp; cases hp; exact hdl
    · have hx46 : ¬ x = 46#u8 := by rw [u8_eq]; simpa using h46
      refine ⟨some (c, false), by simp [shape.after_fraction, (lt_len s c).mpr hlt, hx, hx46],
        ?_, ?_⟩
      · simp [afterFrac, h46, hdrop]
      · intro p hp; cases hp; exact hc
  · have hnot : ¬ c < Slice.len s := by rw [lt_len]; exact hlt
    refine ⟨some (c, false), by simp [shape.after_fraction, hnot], ?_, ?_⟩
    · simp [afterFrac, drop_nil s c.val (by omega)]
    · intro p hp; cases hp; exact hc

private theorem exponent_eval (s : Slice Std.U8) (c : Usize) (hc : c.val ≤ s.length) :
    ∃ r : Option (Usize × Bool), shape.after_exponent s c = ok r ∧
      r.map (fun p => ((bytesOf s).drop p.1.val, p.2)) = afterExp ((bytesOf s).drop c.val) ∧
      ∀ p, r = some p → p.1.val ≤ s.length := by
  by_cases hlt : c.val < s.length
  · obtain ⟨x, hx, hdrop⟩ := index_eval s c hlt
    by_cases he : x.val = 101 ∨ x.val = 69
    · obtain ⟨c', hc', hc'v⟩ := succ_eval s c hlt
      obtain ⟨st, hst, hstl, hstd⟩ := exp_sign_eval s c' (by omega)
      obtain ⟨d, hrun, hdl, hdd, hdv⟩ := skip_eval s st hstl
      have hcode : shape.after_exponent s c =
          (if d = st then ok none else ok (some (d, true))) := by
        rcases he with he | he
        · have h101 : x = 101#u8 := by rw [u8_eq]; simpa using he
          simp [shape.after_exponent, (lt_len s c).mpr hlt, hx, h101, hc', hst, hrun]
        · have h101 : ¬ x = 101#u8 := by rw [u8_eq]; simp; omega
          have h69 : x = 69#u8 := by rw [u8_eq]; simpa using he
          simp [shape.after_exponent, (lt_len s c).mpr hlt, hx, h69, hc', hst, hrun]
      have hspec : afterExp ((bytesOf s).drop c.val) =
          (if ((afterExpSign ((bytesOf s).drop (c.val + 1))).takeWhile isDigit).length = 0
            then none
            else some ((afterExpSign ((bytesOf s).drop (c.val + 1))).dropWhile isDigit, true)) := by
        rw [hdrop]; simp [afterExp, he]
      rw [← hc'v, ← hstd] at hspec
      by_cases hempty : d = st
      · refine ⟨none, by rw [hcode, if_pos hempty], ?_, ?_⟩
        · have hz : (((bytesOf s).drop st.val).takeWhile isDigit).length = 0 := by
            subst hempty; omega
          rw [hspec, if_pos hz]; rfl
        · intro p hp; cases hp
      · refine ⟨some (d, true), by rw [hcode, if_neg hempty], ?_, ?_⟩
        · have hz : (((bytesOf s).drop st.val).takeWhile isDigit).length ≠ 0 := by
            intro hz; apply hempty; rw [UScalar.eq_equiv]; omega
          rw [hspec, if_neg hz, ← hdd]; rfl
        · intro p hp; cases hp; exact hdl
    · have h101 : ¬ x = 101#u8 := by rw [u8_eq]; simp; omega
      have h69 : ¬ x = 69#u8 := by rw [u8_eq]; simp; omega
      refine ⟨some (c, false), by simp [shape.after_exponent, (lt_len s c).mpr hlt, hx, h101, h69],
        ?_, ?_⟩
      · simp [afterExp, he, hdrop]
      · intro p hp; cases hp; exact hc
  · have hnot : ¬ c < Slice.len s := by rw [lt_len]; exact hlt
    refine ⟨some (c, false), by simp [shape.after_exponent, hnot], ?_, ?_⟩
    · simp [afterExp, drop_nil s c.val (by omega)]
    · intro p hp; cases hp; exact hc

set_option hygiene false in
/-- The `Option` `?` of the extraction, as modelled in `CoreModels.lean`. -/
local macro "try_simp" "[" extra:Lean.Parser.Tactic.simpLemma,* "]" : tactic => `(tactic|
  simp [core.option.Option.Insts.CoreOpsTry_traitTry.branch,
    core.option.Option.Insts.CoreOpsTry_traitFromResidualOptionInfallible.from_residual, $extra,*])

/-- RFC 8259 correspondence: for every byte slice the extracted production
    `json_number_shape` returns (without panic or divergence) exactly what the
    reference decision procedure `shapeOf` returns. -/
theorem json_number_shape_correct (s : Slice Std.U8) :
    shape.json_number_shape s = ok (shapeOf (bytesOf s)) := by
  obtain ⟨c1, h1, hl1, hd1⟩ := sign_eval s 0#usize (by simp)
  rw [show (0#usize).val = 0 by simp, List.drop_zero] at hd1
  obtain ⟨r2, h2, hm2, hl2⟩ := integer_eval s c1 hl1
  unfold shape.json_number_shape shapeOf
  rw [← hd1]
  cases r2 with
  | none =>
    simp only [Option.map_none] at hm2
    rw [← hm2]; try_simp [h1, h2]
  | some c2 =>
    simp only [Option.map_some] at hm2
    rw [← hm2]
    dsimp only
    obtain ⟨r3, h3, hm3, hl3⟩ := fraction_eval s c2 (hl2 c2 rfl)
    cases r3 with
    | none =>
      simp only [Option.map_none] at hm3
      rw [← hm3]; try_simp [h1, h2, h3]
    | some p3 =>
      obtain ⟨c3, f⟩ := p3
      simp only [Option.map_some] at hm3
      rw [← hm3]
      dsimp only
      obtain ⟨r4, h4, hm4, hl4⟩ := exponent_eval s c3 (hl3 (c3, f) rfl)
      cases r4 with
      | none =>
        simp only [Option.map_none] at hm4
        rw [← hm4]; try_simp [h1, h2, h3, h4]
      | some p4 =>
        obtain ⟨c4, e⟩ := p4
        simp only [Option.map_some] at hm4
        rw [← hm4]
        dsimp only
        have hend := drop_nil_iff s c4.val (hl4 (c4, e) rfl)
        have hlen : c4 = Slice.len s ↔ c4.val = s.length := by
          rw [UScalar.eq_equiv]; simp
        by_cases hc4 : c4.val = s.length
        · try_simp [h1, h2, h3, h4, hlen.mpr hc4, hend.mpr hc4]
          cases f <;> cases e <;> simp <;> simp [bytesOf]
        · have hne : ¬ c4 = Slice.len s := fun h => hc4 (hlen.mp h)
          have hne' : ¬ (bytesOf s).drop c4.val = [] := fun h => hc4 (hend.mp h)
          try_simp [h1, h2, h3, h4, hne, hne']

/-- The production check accepts exactly the RFC 8259 numbers, with the
    integer flag: `Some b` iff the bytes are a JSON number whose flag is `b`. -/
theorem json_number_shape_some_iff (s : Slice Std.U8) (b : Bool) :
    shape.json_number_shape s = ok (some b) ↔ JsonNumberShape (bytesOf s) b := by
  rw [json_number_shape_correct, ← shapeOf_iff]
  simp

/-- The production check refuses exactly the byte strings that are not JSON numbers. -/
theorem json_number_shape_none_iff (s : Slice Std.U8) :
    shape.json_number_shape s = ok none ↔ ∀ b, ¬ JsonNumberShape (bytesOf s) b := by
  rw [json_number_shape_correct, ← shapeOf_none_iff]
  simp

end Norn.Codec
#print axioms Norn.Codec.json_number_shape_correct
#print axioms Norn.Codec.json_number_shape_some_iff
#print axioms Norn.Codec.json_number_shape_none_iff
