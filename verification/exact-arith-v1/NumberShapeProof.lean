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

private theorem get_eval (s : Slice Std.U8) (c : Usize) :
    core.slice.Slice.get (core.slice.index.SliceIndexUsizeSlice Std.U8) s c = ok (s.val[c.val]?) := by
  simp [core.slice.Slice.get]

private theorem get_some (s : Slice Std.U8) (c : Usize) (h : c.val < s.length) :
    ∃ x : Std.U8, core.slice.Slice.get (core.slice.index.SliceIndexUsizeSlice Std.U8) s c =
      ok (some x) ∧ (bytesOf s).drop c.val = x.val :: (bytesOf s).drop (c.val + 1) := by
  refine ⟨s.val[c.val], by rw [get_eval, List.getElem?_eq_getElem h], ?_⟩
  have h' : c.val < (bytesOf s).length := by rw [bytesOf_length]; exact h
  rw [List.drop_eq_getElem_cons h']
  simp [bytesOf]

private theorem get_none (s : Slice Std.U8) (c : Usize) (h : s.length ≤ c.val) :
    core.slice.Slice.get (core.slice.index.SliceIndexUsizeSlice Std.U8) s c = ok none := by
  rw [get_eval, List.getElem?_eq_none h]

private theorem succ_eval (s : Slice Std.U8) (c : Usize) (h : c.val < s.length) :
    ∃ d : Usize, Usize.checked_add c 1#usize = some d ∧ d.val = c.val + 1 := by
  have hb := Usize.checked_add_bv_spec c 1#usize
  have hm : s.length ≤ Usize.max := s.length_ineq
  cases hz : Usize.checked_add c 1#usize with
  | none => rw [hz] at hb; simp at hb; omega
  | some z => rw [hz] at hb; exact ⟨z, rfl, by simpa using hb.2.1⟩

private theorem digit_eval (x : Std.U8) : core.num.U8.is_ascii_digit x = ok (isDigit x.val) := by
  simp [core.num.U8.is_ascii_digit, isDigit]

private theorem u8_ne {x : Std.U8} {n : Std.U8} (h : x = n → False) : x.val ≠ n.val :=
  fun hv => h ((UScalar.eq_equiv x n).mpr hv)

set_option hygiene false in
/-- The `Option` `?` of the extraction, as modelled in `CoreModels.lean`. -/
local macro "try_simp" "[" extra:Lean.Parser.Tactic.simpLemma,* "]" : tactic => `(tactic|
  simp [core.option.Option.Insts.CoreOpsTry_traitTry.branch,
    core.option.Option.Insts.CoreOpsTry_traitFromResidualOptionInfallible.from_residual, $extra,*])

/-- `skip_digits` stops at the first non-digit at or after the cursor. -/
private theorem skip_spec (s : Slice Std.U8) : ∀ (k : ℕ) (c : Usize), s.length - c.val = k →
    c.val ≤ s.length →
    ∃ d : Usize, shape.skip_digits s c = ok (some d) ∧ d.val ≤ s.length ∧
      (bytesOf s).drop d.val = ((bytesOf s).drop c.val).dropWhile isDigit ∧
      d.val = c.val + (((bytesOf s).drop c.val).takeWhile isDigit).length := by
  intro k
  induction k with
  | zero =>
    intro c hk hc
    refine ⟨c, ?_, hc, ?_, ?_⟩
    · unfold shape.skip_digits shape.skip_digits_loop
      rw [loop.eq_1]
      simp [shape.skip_digits_loop.body, get_none s c (by omega)]
    · rw [drop_nil s c.val (by omega)]; rfl
    · rw [drop_nil s c.val (by omega)]; rfl
  | succ k ih =>
    intro c hk hc
    have hlt : c.val < s.length := by omega
    obtain ⟨x, hx, hdrop⟩ := get_some s c hlt
    by_cases hd : isDigit x.val = true
    · obtain ⟨c', hc', hc'v⟩ := succ_eval s c hlt
      obtain ⟨d, hrun, hdl, hdd, hdv⟩ := ih c' (by omega) (by omega)
      refine ⟨d, ?_, hdl, ?_, ?_⟩
      · unfold shape.skip_digits shape.skip_digits_loop
        rw [loop.eq_1]
        simp only [shape.skip_digits_loop.body, hx, bind_tc_ok, digit_eval, hd, if_true, lift, hc']
        exact hrun
      · rw [hdd, hdrop, List.dropWhile_cons, if_pos hd, hc'v]
      · rw [hdv, hdrop, List.takeWhile_cons, if_pos hd, hc'v]; simp; omega
    · have hd' : isDigit x.val = false := by simpa using hd
      refine ⟨c, ?_, hc, ?_, ?_⟩
      · unfold shape.skip_digits shape.skip_digits_loop
        rw [loop.eq_1]
        simp [shape.skip_digits_loop.body, hx, digit_eval, hd']
      · rw [hdrop, List.dropWhile_cons, if_neg (by simpa using hd')]
      · rw [hdrop, List.takeWhile_cons, if_neg (by simpa using hd')]; simp

private theorem skip_eval (s : Slice Std.U8) (c : Usize) (hc : c.val ≤ s.length) :
    ∃ d : Usize, shape.skip_digits s c = ok (some d) ∧ d.val ≤ s.length ∧
      (bytesOf s).drop d.val = ((bytesOf s).drop c.val).dropWhile isDigit ∧
      d.val = c.val + (((bytesOf s).drop c.val).takeWhile isDigit).length :=
  skip_spec s _ c rfl hc

private theorem sign_eval (s : Slice Std.U8) (c : Usize) (hc : c.val ≤ s.length) :
    ∃ d : Usize, shape.after_sign s c = ok (some d) ∧ d.val ≤ s.length ∧
      (bytesOf s).drop d.val = afterSign ((bytesOf s).drop c.val) := by
  by_cases hlt : c.val < s.length
  · obtain ⟨x, hx, hdrop⟩ := get_some s c hlt
    generalize hxv : x.val = v at hdrop
    obtain ⟨c', hc', hc'v⟩ := succ_eval s c hlt
    unfold shape.after_sign
    rw [hx]
    simp only [bind_tc_ok]
    split
    next =>
      have hm : v = 45 := by rw [← hxv]; decide
      refine ⟨c', by rw [hc'], by omega, ?_⟩
      rw [hdrop, hc'v]; simp [afterSign, hm]
    next hne =>
      have hm : v ≠ 45 := by rw [← hxv]; intro h; exact u8_ne hne (by rw [h]; decide)
      refine ⟨c, rfl, hc, ?_⟩
      rw [hdrop]; simp [afterSign, hm]
  · refine ⟨c, ?_, hc, ?_⟩
    · unfold shape.after_sign; rw [get_none s c (by omega)]; rfl
    · rw [drop_nil s c.val (by omega)]; rfl

private theorem exp_sign_eval (s : Slice Std.U8) (c : Usize) (hc : c.val ≤ s.length) :
    ∃ d : Usize, shape.after_exponent_sign s c = ok (some d) ∧ d.val ≤ s.length ∧
      (bytesOf s).drop d.val = afterExpSign ((bytesOf s).drop c.val) := by
  by_cases hlt : c.val < s.length
  · obtain ⟨x, hx, hdrop⟩ := get_some s c hlt
    generalize hxv : x.val = v at hdrop
    obtain ⟨c', hc', hc'v⟩ := succ_eval s c hlt
    unfold shape.after_exponent_sign
    rw [hx]
    simp only [bind_tc_ok]
    split
    next =>
      have hm : v = 43 := by rw [← hxv]; decide
      refine ⟨c', by rw [hc'], by omega, ?_⟩
      rw [hdrop, hc'v]; simp [afterExpSign, hm]
    next =>
      have hm : v = 45 := by rw [← hxv]; decide
      refine ⟨c', by rw [hc'], by omega, ?_⟩
      rw [hdrop, hc'v]; simp [afterExpSign, hm]
    next h43 h45 =>
      have hm : ¬ (v = 43 ∨ v = 45) := by
        rw [← hxv]
        rintro (h | h)
        · exact u8_ne h43 (by rw [h]; decide)
        · exact u8_ne h45 (by rw [h]; decide)
      refine ⟨c, rfl, hc, ?_⟩
      rw [hdrop]; simp only [afterExpSign, if_neg hm]
  · refine ⟨c, ?_, hc, ?_⟩
    · unfold shape.after_exponent_sign; rw [get_none s c (by omega)]; rfl
    · rw [drop_nil s c.val (by omega)]; rfl

private theorem integer_eval (s : Slice Std.U8) (c : Usize) (hc : c.val ≤ s.length) :
    ∃ r : Option Usize, shape.after_integer s c = ok r ∧
      r.map (fun d => (bytesOf s).drop d.val) = afterInt ((bytesOf s).drop c.val) ∧
      ∀ d, r = some d → d.val ≤ s.length := by
  by_cases hlt : c.val < s.length
  · obtain ⟨x, hx, hdrop⟩ := get_some s c hlt
    generalize hxv : x.val = v at hdrop
    obtain ⟨c', hc', hc'v⟩ := succ_eval s c hlt
    unfold shape.after_integer
    rw [hx]
    simp only [bind_tc_ok]
    split
    next =>
      have h0 : v = 48 := by rw [← hxv]; decide
      refine ⟨some c', by rw [hc'], ?_, ?_⟩
      · rw [hdrop]; simp [afterInt, h0, hc'v]
      · intro d hd; cases hd; omega
    next hne =>
      have h0 : v ≠ 48 := by rw [← hxv]; intro h; exact u8_ne hne (by rw [h]; decide)
      by_cases hd : isDigit v = true
      · obtain ⟨d, hrun, hdl, hdd, -⟩ := skip_eval s c' (by omega)
        refine ⟨some d, ?_, ?_, ?_⟩
        · try_simp [digit_eval, hxv, hd, lift, hc', hrun]
        · rw [hdrop]; simp [afterInt, h0, hd, hdd, hc'v]
        · intro e he; cases he; exact hdl
      · have hd' : isDigit v = false := by simpa using hd
        refine ⟨none, by simp [digit_eval, hxv, hd'], ?_, ?_⟩
        · rw [hdrop]; simp [afterInt, h0, hd']
        · intro d hd; cases hd
  · refine ⟨none, ?_, ?_, ?_⟩
    · unfold shape.after_integer; rw [get_none s c (by omega)]; rfl
    · rw [drop_nil s c.val (by omega)]; rfl
    · intro d hd; cases hd

private theorem fraction_eval (s : Slice Std.U8) (c : Usize) (hc : c.val ≤ s.length) :
    ∃ r : Option (Usize × Bool), shape.after_fraction s c = ok r ∧
      r.map (fun p => ((bytesOf s).drop p.1.val, p.2)) = afterFrac ((bytesOf s).drop c.val) ∧
      ∀ p, r = some p → p.1.val ≤ s.length := by
  by_cases hlt : c.val < s.length
  · obtain ⟨x, hx, hdrop⟩ := get_some s c hlt
    generalize hxv : x.val = v at hdrop
    obtain ⟨c', hc', hc'v⟩ := succ_eval s c hlt
    unfold shape.after_fraction
    rw [hx]
    simp only [bind_tc_ok]
    split
    next =>
      have h46 : v = 46 := by rw [← hxv]; decide
      obtain ⟨d, hrun, hdl, hdd, hdv⟩ := skip_eval s c' (by omega)
      by_cases hempty : d = c'
      · refine ⟨none, by try_simp [lift, hc', hrun, hempty], ?_, ?_⟩
        · have hz : (((bytesOf s).drop (c.val + 1)).takeWhile isDigit).length = 0 := by
            subst hempty; rw [hc'v] at hdv; omega
          rw [hdrop]; simp [afterFrac, hz, h46]
        · intro p hp; cases hp
      · refine ⟨some (d, true), by try_simp [lift, hc', hrun, hempty], ?_, ?_⟩
        · have hz : (((bytesOf s).drop (c.val + 1)).takeWhile isDigit).length ≠ 0 := by
            intro hz; apply hempty; rw [UScalar.eq_equiv]; rw [hc'v] at hdv ⊢; omega
          rw [hdrop]; simp [afterFrac, hz, hdd, hc'v, h46]
        · intro p hp; cases hp; exact hdl
    next hne =>
      have h46 : v ≠ 46 := by rw [← hxv]; intro h; exact u8_ne hne (by rw [h]; decide)
      refine ⟨some (c, false), rfl, ?_, ?_⟩
      · simp [afterFrac, h46, hdrop]
      · intro p hp; cases hp; exact hc
  · refine ⟨some (c, false), ?_, ?_, ?_⟩
    · unfold shape.after_fraction; rw [get_none s c (by omega)]; rfl
    · simp [afterFrac, drop_nil s c.val (by omega)]
    · intro p hp; cases hp; exact hc

private theorem exponent_some (s : Slice Std.U8) (c : Usize) (hlt : c.val < s.length)
    (v : ℕ) (hdrop : (bytesOf s).drop c.val = v :: (bytesOf s).drop (c.val + 1))
    (he : v = 101 ∨ v = 69) :
    ∃ c' st d : Usize, Usize.checked_add c 1#usize = some c' ∧
      shape.after_exponent_sign s c' = ok (some st) ∧ shape.skip_digits s st = ok (some d) ∧
      d.val ≤ s.length ∧
      afterExp ((bytesOf s).drop c.val) =
        (if d = st then none else some ((bytesOf s).drop d.val, true)) := by
  obtain ⟨c', hc', hc'v⟩ := succ_eval s c hlt
  obtain ⟨st, hst, hstl, hstd⟩ := exp_sign_eval s c' (by omega)
  obtain ⟨d, hrun, hdl, hdd, hdv⟩ := skip_eval s st hstl
  refine ⟨c', st, d, hc', hst, hrun, hdl, ?_⟩
  have hspec : afterExp ((bytesOf s).drop c.val) =
      (if ((afterExpSign ((bytesOf s).drop (c.val + 1))).takeWhile isDigit).length = 0
        then none
        else some ((afterExpSign ((bytesOf s).drop (c.val + 1))).dropWhile isDigit, true)) := by
    rw [hdrop]; simp [afterExp, he]
  rw [← hc'v, ← hstd, ← hdd] at hspec
  rw [hspec]
  by_cases hempty : d = st
  · have hz : (((bytesOf s).drop st.val).takeWhile isDigit).length = 0 := by
      subst hempty; omega
    rw [if_pos hz, if_pos hempty]
  · have hz : (((bytesOf s).drop st.val).takeWhile isDigit).length ≠ 0 := by
      intro hz; apply hempty; rw [UScalar.eq_equiv]; omega
    rw [if_neg hz, if_neg hempty]

private theorem exponent_eval (s : Slice Std.U8) (c : Usize) (hc : c.val ≤ s.length) :
    ∃ r : Option (Usize × Bool), shape.after_exponent s c = ok r ∧
      r.map (fun p => ((bytesOf s).drop p.1.val, p.2)) = afterExp ((bytesOf s).drop c.val) ∧
      ∀ p, r = some p → p.1.val ≤ s.length := by
  by_cases hlt : c.val < s.length
  · obtain ⟨x, hx, hdrop⟩ := get_some s c hlt
    generalize hxv : x.val = v at hdrop
    unfold shape.after_exponent
    rw [hx]
    simp only [bind_tc_ok]
    split
    next =>
      obtain ⟨c', st, d, hc', hst, hrun, hdl, hspec⟩ :=
        exponent_some s c hlt v hdrop (Or.inl (by rw [← hxv]; decide))
      by_cases hempty : d = st
      · exact ⟨none, by try_simp [lift, hc', hst, hrun, hempty], by rw [hspec, if_pos hempty]; rfl,
          fun p hp => by cases hp⟩
      · exact ⟨some (d, true), by try_simp [lift, hc', hst, hrun, hempty],
          by rw [hspec, if_neg hempty]; rfl, fun p hp => by cases hp; exact hdl⟩
    next =>
      obtain ⟨c', st, d, hc', hst, hrun, hdl, hspec⟩ :=
        exponent_some s c hlt v hdrop (Or.inr (by rw [← hxv]; decide))
      by_cases hempty : d = st
      · exact ⟨none, by try_simp [lift, hc', hst, hrun, hempty], by rw [hspec, if_pos hempty]; rfl,
          fun p hp => by cases hp⟩
      · exact ⟨some (d, true), by try_simp [lift, hc', hst, hrun, hempty],
          by rw [hspec, if_neg hempty]; rfl, fun p hp => by cases hp; exact hdl⟩
    next h101 h69 =>
      have he : ¬ (v = 101 ∨ v = 69) := by
        rw [← hxv]
        rintro (h | h)
        · exact u8_ne h101 (by rw [h]; decide)
        · exact u8_ne h69 (by rw [h]; decide)
      refine ⟨some (c, false), rfl, ?_, ?_⟩
      · simp only [Option.map_some, hdrop, afterExp, if_neg he]
      · intro p hp; cases hp; exact hc
  · refine ⟨some (c, false), ?_, ?_, ?_⟩
    · unfold shape.after_exponent; rw [get_none s c (by omega)]; rfl
    · simp [afterExp, drop_nil s c.val (by omega)]
    · intro p hp; cases hp; exact hc

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
