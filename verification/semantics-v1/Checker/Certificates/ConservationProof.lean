import ConservationCore.Funs
import Checker.Certificates.Support
import Checker.Certificates.ConservationReference

/-!
# Production conservation check meets the written certificate condition

`ConservationCore` is the fresh Aeneas translation of the production file
`crates/norn-checker/src/obligations/conservation/decision.rs`, generic over the
number type `N` and its `ExactArithmetic` instance. The theorem assumes only
that the instance is exact where it succeeds (`Lawful`): a returned sum or
product denotes the exact rational sum or product, zero denotes 0, and
`exact_is_zero` answers true only for 0. The production instance is
`exact_rational::Rational` (its exactness is lane C-B's unit, registered as a
trusted assumption until it lands).

`check_writer_sound`: if the production check accepts one writer, the written
certificate condition `Balanced` holds, so the writer conserves every class's
weighted sum for every state and parameter valuation (`step_conserves`), and
so does every run of accepted writers (`run_conserves`).
-/

namespace Norn.Checker.Certificates.Conservation
open Aeneas Aeneas.Std Result
open Norn.Checker.Certificates
open norn_checker_conservation

variable {N : Type}

/-- The rational value of entry `i` of a list (0 when absent). -/
def entry (val : N → ℚ) (l : List N) (i : Nat) : ℚ := (l[i]?.map val).getD 0

/-- The rational value of cell `(r, c)` of a matrix (0 when absent). -/
def cell (val : N → ℚ) (m : List (alloc.vec.Vec N)) (r c : Nat) : ℚ :=
  (m[r]?.map (fun row => entry val row.val c)).getD 0

/-- The instance is exact wherever it succeeds. -/
structure Lawful (inst : ExactArithmetic N) (val : N → ℚ) : Prop where
  zero : ∀ z, inst.exact_zero = ok z → val z = 0
  add : ∀ a b c, inst.exact_add a b = ok (some c) → val c = val a + val b
  mul : ∀ a b c, inst.exact_mul a b = ok (some c) → val c = val a * val b
  is_zero : ∀ a, inst.exact_is_zero a = ok true → val a = 0

variable {inst : ExactArithmetic N} {val : N → ℚ}

theorem push_ok {α : Type} {v v' : alloc.vec.Vec α} {x : α}
    (h : alloc.vec.Vec.push v x = ok v') : v'.val = v.val ++ [x] := by
  unfold alloc.vec.Vec.push at h
  dsimp only at h
  split at h
  · cases h; simp
  · cases h

theorem entry_of_lt (l : List N) (i : Nat) (h : i < l.length) : entry val l i = val l[i] := by
  simp [entry, List.getElem?_eq_getElem h]

theorem labels_in_range_true {p : Slice Usize} (h : labels_in_range p = ok true) :
    ∀ i (hi : i < p.length), (p.val[i]).val < p.length := by
  have H := loop_ok (labels_in_range_loop.body p) (fun i => p.length - i.val)
    (fun i => ∀ k (hk : k < p.length), k < i.val → (p.val[k]).val < p.length)
    (fun b => b = true → ∀ k (hk : k < p.length), (p.val[k]).val < p.length) ?_ ?_
    0#usize true (fun k _ hk => absurd hk (Nat.not_lt_zero _)) h rfl
  · exact H
  · intro i y hI hb
    unfold labels_in_range_loop.body at hb
    try dsimp only at hb
    split at hb
    · rename_i hlt
      simp only [UScalar.lt_equiv, Slice.len_val] at hlt
      peel x hx at hb
      obtain ⟨_hxi, hxv⟩ := index_ok hx
      split at hb
      · cases hb
      · rename_i hge
        simp only [ge_iff_le, UScalar.le_equiv, Slice.len_val, not_le] at hge
        peel i4 hi4 at hb
        simp only [ok.injEq, ControlFlow.cont.injEq] at hb
        subst hb
        have hv := usize_succ hi4
        refine ⟨fun k hk hki => ?_, by omega⟩
        by_cases hkl : k < i.val
        · exact hI k hk hkl
        · have : k = i.val := by omega
          subst this; rw [hxv]; exact hge
    · cases hb
  · intro i r hI hb hr k hk
    subst hr
    unfold labels_in_range_loop.body at hb
    try dsimp only at hb
    split at hb
    · peel_ x at hb
      split at hb
      · cases hb
      · peel_ i4 at hb; cases hb
    · rename_i hge
      simp only [UScalar.lt_equiv, Slice.len_val, not_lt] at hge
      exact hI k hk (by omega)

theorem rows_have_width_true {rows : Slice (alloc.vec.Vec N)} {columns : Usize}
    (h : rows_have_width rows columns = ok true) :
    ∀ i (hi : i < rows.length), (rows.val[i]).val.length = columns.val := by
  have H := loop_ok (rows_have_width_loop.body rows columns) (fun i => rows.length - i.val)
    (fun i => ∀ k (hk : k < rows.length), k < i.val → (rows.val[k]).val.length = columns.val)
    (fun b => b = true → ∀ k (hk : k < rows.length), (rows.val[k]).val.length = columns.val)
    ?_ ?_ 0#usize true (fun k _ hk => absurd hk (Nat.not_lt_zero _)) h rfl
  · exact H
  · intro i y hI hb
    unfold rows_have_width_loop.body at hb
    try dsimp only at hb
    split at hb
    · rename_i hlt
      simp only [UScalar.lt_equiv, Slice.len_val] at hlt
      peel x hx at hb
      obtain ⟨_hxi, hxv⟩ := index_ok hx
      split at hb
      · cases hb
      · rename_i hne
        have hne : x.val.length = columns.val := by
          by_contra hc
          exact hne (by simp [bne_iff_ne, ne_eq, alloc.vec.Vec.len, hc])
        peel i3 hi3 at hb
        simp only [ok.injEq, ControlFlow.cont.injEq] at hb
        subst hb
        have hv := usize_succ hi3
        refine ⟨fun k hk hki => ?_, by omega⟩
        by_cases hkl : k < i.val
        · exact hI k hk hkl
        · have : k = i.val := by omega
          subst this; rw [hxv]; exact hne
    · cases hb
  · intro i r hI hb hr k hk
    subst hr
    unfold rows_have_width_loop.body at hb
    try dsimp only at hb
    split at hb
    · peel_ x at hb
      split at hb
      · cases hb
      · peel_ i3 at hb; cases hb
    · rename_i hge
      simp only [UScalar.lt_equiv, Slice.len_val, not_lt] at hge
      exact hI k hk (by omega)

theorem row_is_zero_true (hL : Lawful inst val) {row : Slice N}
    (h : row_is_zero inst row = ok true) : ∀ c, entry val row.val c = 0 := by
  have H := loop_ok (row_is_zero_loop.body inst row) (fun c => row.length - c.val)
    (fun c => ∀ k (hk : k < row.length), k < c.val → val (row.val[k]) = 0)
    (fun b => b = true → ∀ k (hk : k < row.length), val (row.val[k]) = 0)
    ?_ ?_ 0#usize true (fun k _ hk => absurd hk (Nat.not_lt_zero _)) h rfl
  · intro c
    by_cases hc : c < row.length
    · rw [entry_of_lt _ _ hc]; exact H c hc
    · simp [entry, List.getElem?_eq_none (Nat.le_of_not_lt hc)]
  · intro i y hI hb
    unfold row_is_zero_loop.body at hb
    try dsimp only at hb
    split at hb
    · rename_i hlt
      simp only [UScalar.lt_equiv, Slice.len_val] at hlt
      peel x hx at hb
      obtain ⟨_hxi, hxv⟩ := index_ok hx
      peel b hbz at hb
      cases b
      · rw [if_neg Bool.false_ne_true] at hb; cases hb
      rw [if_pos rfl] at hb
      peel c1 hc1 at hb
      simp only [ok.injEq, ControlFlow.cont.injEq] at hb
      subst hb
      have hv := usize_succ hc1
      refine ⟨fun k hk hki => ?_, by omega⟩
      by_cases hkl : k < i.val
      · exact hI k hk hkl
      · have : k = i.val := by omega
        subst this; rw [hxv]; exact hL.is_zero x hbz
    · cases hb
  · intro i r hI hb hr k hk
    subst hr
    unfold row_is_zero_loop.body at hb
    try dsimp only at hb
    split at hb
    · peel_ x at hb
      peel_ b at hb
      cases b
      · rw [if_neg Bool.false_ne_true] at hb; cases hb
      · rw [if_pos rfl] at hb; peel_ c1 at hb; cases hb
    · rename_i hge
      simp only [UScalar.lt_equiv, Slice.len_val, not_lt] at hge
      exact hI k hk (by omega)

theorem all_zero_true (hL : Lawful inst val) {totals : Slice (alloc.vec.Vec N)}
    (h : all_zero inst totals = ok true) : ∀ r c, cell val totals.val r c = 0 := by
  have H := loop_ok (all_zero_loop.body inst totals) (fun r => totals.length - r.val)
    (fun r => ∀ k (hk : k < totals.length), k < r.val → ∀ c, entry val (totals.val[k]).val c = 0)
    (fun b => b = true → ∀ k (hk : k < totals.length), ∀ c, entry val (totals.val[k]).val c = 0)
    ?_ ?_ 0#usize true (fun k _ hk => absurd hk (Nat.not_lt_zero _)) h rfl
  · intro r c
    by_cases hr : r < totals.length
    · simp only [cell, List.getElem?_eq_getElem hr, Option.map_some, Option.getD_some]
      exact H r hr c
    · simp [cell, List.getElem?_eq_none (Nat.le_of_not_lt hr)]
  · intro i y hI hb
    unfold all_zero_loop.body at hb
    try dsimp only at hb
    split at hb
    · rename_i hlt
      simp only [UScalar.lt_equiv, Slice.len_val] at hlt
      peel x hx at hb
      obtain ⟨_hxi, hxv⟩ := index_ok hx
      peel b hbz at hb
      cases b
      · rw [if_neg Bool.false_ne_true] at hb; cases hb
      rw [if_pos rfl] at hb
      peel r1 hr1 at hb
      simp only [ok.injEq, ControlFlow.cont.injEq] at hb
      subst hb
      have hv := usize_succ hr1
      refine ⟨fun k hk hki => ?_, by omega⟩
      by_cases hkl : k < i.val
      · exact hI k hk hkl
      · have : k = i.val := by omega
        subst this; rw [hxv]
        exact row_is_zero_true hL hbz
    · cases hb
  · intro i r hI hb hr k hk
    subst hr
    unfold all_zero_loop.body at hb
    try dsimp only at hb
    split at hb
    · peel_ x at hb
      peel_ b at hb
      cases b
      · rw [if_neg Bool.false_ne_true] at hb; cases hb
      · rw [if_pos rfl] at hb; peel_ r1 at hb; cases hb
    · rename_i hge
      simp only [UScalar.lt_equiv, Slice.len_val, not_lt] at hge
      exact hI k hk (by omega)

theorem zero_row_ok (hL : Lawful inst val) {columns : Usize} {row : alloc.vec.Vec N}
    (h : zero_row inst columns = ok row) :
    row.val.length = columns.val ∧ ∀ c, entry val row.val c = 0 := by
  have H := loop_ok (fun x => zero_row_loop.body inst columns x.1 x.2)
    (fun x => columns.val - x.2.val)
    (fun x => x.1.val.length = x.2.val ∧ x.2.val ≤ columns.val ∧ ∀ c, entry val x.1.val c = 0)
    (fun r => r.val.length = columns.val ∧ ∀ c, entry val r.val c = 0) ?_ ?_
    (alloc.vec.Vec.new N, 0#usize) row ⟨by simp, by simp, fun c => by simp [entry]⟩ h
  · exact H
  · rintro ⟨r0, c0⟩ ⟨r1, c1⟩ ⟨hlen, hle, hz⟩ hb
    unfold zero_row_loop.body at hb
    dsimp only at hb hlen hle hz ⊢
    split at hb
    · rename_i hlt
      simp only [UScalar.lt_equiv] at hlt
      peel t ht at hb
      peel r2 hr2 at hb
      peel c2 hc2 at hb
      simp only [ok.injEq, ControlFlow.cont.injEq, Prod.mk.injEq] at hb
      obtain ⟨rfl, rfl⟩ := hb
      have hv := usize_succ hc2
      have hp := push_ok hr2
      refine ⟨⟨by simp [hp, hlen, hv], by omega, fun c => ?_⟩, by omega⟩
      rw [hp]
      by_cases hc : c < r0.val.length
      · simp only [entry, List.getElem?_append_left hc]; exact hz c
      · by_cases hce : c = r0.val.length
        · subst hce; simp [entry, hL.zero t ht]
        · have hle' : (r0.val ++ [t]).length ≤ c := by simp; omega
          simp [entry, List.getElem?_eq_none hle']
    · cases hb
  · rintro ⟨r0, c0⟩ r ⟨hlen, hle, hz⟩ hb
    unfold zero_row_loop.body at hb
    dsimp only at hb hlen hle hz ⊢
    split at hb
    · peel_ t at hb; peel_ r2 at hb; peel_ c2 at hb; cases hb
    · rename_i hge
      simp only [UScalar.lt_equiv, not_lt] at hge
      simp only [ok.injEq, ControlFlow.done.injEq] at hb
      subst hb
      exact ⟨by omega, hz⟩

theorem zero_totals_ok (hL : Lawful inst val) {classes columns : Usize}
    {totals : alloc.vec.Vec (alloc.vec.Vec N)}
    (h : zero_totals inst classes columns = ok totals) :
    totals.val.length = classes.val ∧
      (∀ r (hr : r < totals.val.length), (totals.val[r]).val.length = columns.val) ∧
      ∀ r c, cell val totals.val r c = 0 := by
  have H := loop_ok (fun x => zero_totals_loop.body inst classes columns x.1 x.2)
    (fun x => classes.val - x.2.val)
    (fun x => x.1.val.length = x.2.val ∧ x.2.val ≤ classes.val ∧
      (∀ r (hr : r < x.1.val.length), (x.1.val[r]).val.length = columns.val) ∧
      ∀ r c, cell val x.1.val r c = 0)
    (fun t => t.val.length = classes.val ∧
      (∀ r (hr : r < t.val.length), (t.val[r]).val.length = columns.val) ∧
      ∀ r c, cell val t.val r c = 0) ?_ ?_
    (alloc.vec.Vec.new _, 0#usize) totals
    ⟨by simp, by simp, fun r hr => by simp at hr, fun r c => by simp [cell]⟩ h
  · exact H
  · rintro ⟨t0, r0⟩ ⟨t1, r1⟩ ⟨hlen, hle, hw, hz⟩ hb
    unfold zero_totals_loop.body at hb
    dsimp only at hb hlen hle hw hz ⊢
    split at hb
    · rename_i hlt
      simp only [UScalar.lt_equiv] at hlt
      peel v hv at hb
      peel t2 ht2 at hb
      peel r2 hr2 at hb
      simp only [ok.injEq, ControlFlow.cont.injEq, Prod.mk.injEq] at hb
      obtain ⟨rfl, rfl⟩ := hb
      have hrv := usize_succ hr2
      have hp := push_ok ht2
      obtain ⟨hvlen, hvz⟩ := zero_row_ok hL hv
      refine ⟨⟨by simp [hp, hlen, hrv], by omega, fun r hr => ?_, fun r c => ?_⟩, by omega⟩
      · simp only [hp, List.length_append, List.length_singleton] at hr
        by_cases hrl : r < t0.val.length
        · simp only [hp, List.getElem_append_left hrl]; exact hw r hrl
        · have : r = t0.val.length := by omega
          subst this; simp [hp, hvlen]
      · rw [hp]
        by_cases hrl : r < t0.val.length
        · simp only [cell, List.getElem?_append_left hrl]; exact hz r c
        · by_cases hre : r = t0.val.length
          · subst hre; simp [cell, hvz c]
          · have hle' : (t0.val ++ [v]).length ≤ r := by simp; omega
            simp [cell, List.getElem?_eq_none hle']
    · cases hb
  · rintro ⟨t0, r0⟩ t ⟨hlen, hle, hw, hz⟩ hb
    unfold zero_totals_loop.body at hb
    dsimp only at hb hlen hle hw hz ⊢
    split at hb
    · peel_ v at hb; peel_ t2 at hb; peel_ r2 at hb; cases hb
    · rename_i hge
      simp only [UScalar.lt_equiv, not_lt] at hge
      simp only [ok.injEq, ControlFlow.done.injEq] at hb
      subst hb
      exact ⟨by omega, hw, hz⟩

/-- One row update: `total[c] += row[c] * weight` for every column of `row`. -/
theorem add_weighted_row_ok (hL : Lawful inst val) {total row : Slice N} {weight : N}
    {res : core.result.Result Unit (Refusal N)} {total' : Slice N}
    (h : add_weighted_row inst total row weight = ok (res, total')) (hres : res = .Ok ()) :
    total'.length = total.length ∧ row.length ≤ total.length ∧
      ∀ c, entry val total'.val c =
        entry val total.val c + (if c < row.length then entry val row.val c * val weight else 0) := by
  have H := loop_ok (fun x => add_weighted_row_loop.body inst row weight x.1 x.2)
    (fun x => row.length - x.2.val)
    (fun x => x.1.length = total.length ∧ x.2.val ≤ total.length ∧
      ∀ c, entry val x.1.val c =
        entry val total.val c + (if c < x.2.val ∧ c < row.length then
          entry val row.val c * val weight else 0))
    (fun p => p.1 = .Ok () → p.2.length = total.length ∧ row.length ≤ total.length ∧
      ∀ c, entry val p.2.val c =
        entry val total.val c + (if c < row.length then entry val row.val c * val weight else 0))
    ?_ ?_ (total, 0#usize) (res, total') ⟨rfl, by simp, fun c => by simp⟩ h hres
  · exact H
  · rintro ⟨t0, c0⟩ ⟨t1, c1⟩ ⟨hlen, hle, he⟩ hb
    unfold add_weighted_row_loop.body at hb
    dsimp only at hb hlen hle he ⊢
    split at hb
    · rename_i hlt
      simp only [UScalar.lt_equiv, Slice.len_val] at hlt
      peel x hx at hb
      obtain ⟨_hxi, hxv⟩ := index_ok hx
      peel o ho at hb
      cases o with
      | none => cases hb
      | some prod =>
        try dsimp only at hb
        peel y hy at hb
        obtain ⟨hyl, hyv⟩ := index_ok hy
        peel o1 ho1 at hb
        cases o1 with
        | none => cases hb
        | some sum =>
          try dsimp only at hb
          peel s hs at hb
          peel c2 hc2 at hb
          simp only [ok.injEq, ControlFlow.cont.injEq, Prod.mk.injEq] at hb
          obtain ⟨rfl, rfl⟩ := hb
          have hcv := usize_succ hc2
          have hsv : s = t0.set c0 sum := by
            unfold Slice.update at hs
            split at hs
            · cases hs
            · cases hs; rfl
          subst hsv
          have hprod := hL.mul _ _ _ ho
          have hsum := hL.add _ _ _ ho1
          refine ⟨⟨by simp [Slice.set, hlen], by omega, fun c => ?_⟩, by omega⟩
          by_cases hcc : c = c0.val
          · subst hcc
            have hlt0 : c0.val < t0.val.length := hyl
            have hs' : val sum = val (t0.val[c0.val]) + val (row.val[c0.val]) * val weight := by
              rw [hsum, hprod, hyv, hxv]
            have he0 := he c0.val
            rw [if_neg (by omega), add_zero, entry_of_lt _ _ hlt0] at he0
            show entry val (t0.val.set c0.val sum) c0.val = _
            rw [entry, List.getElem?_set_self hlt0, Option.map_some, Option.getD_some, hs', he0,
              if_pos ⟨by omega, hlt⟩, entry_of_lt _ _ hlt]
          · have := he c
            show entry val (t0.val.set c0.val sum) c = _
            simp only [entry] at this ⊢
            rw [List.getElem?_set_ne (Ne.symm hcc), this]
            congr 1
            by_cases hc1 : c < c0.val
            · rw [if_pos ⟨hc1, by omega⟩, if_pos ⟨by omega, by omega⟩]
            · rw [if_neg (by omega), if_neg (by omega)]
    · cases hb
  · rintro ⟨t0, c0⟩ ⟨r, t⟩ ⟨hlen, hle, he⟩ hb hr
    dsimp only at hr ⊢
    subst hr
    unfold add_weighted_row_loop.body at hb
    dsimp only at hb hlen hle he
    split at hb
    · peel_ x at hb
      peel_ o at hb
      cases o with
      | none => cases hb
      | some prod =>
        try dsimp only at hb
        peel_ y at hb
        peel_ o1 at hb
        cases o1 with
        | none => cases hb
        | some sum => dsimp only at hb; peel_ s at hb; peel_ c2 at hb; cases hb
    · rename_i hge
      simp only [UScalar.lt_equiv, Slice.len_val, not_lt] at hge
      simp only [ok.injEq, ControlFlow.done.injEq, Prod.mk.injEq] at hb
      obtain ⟨-, rfl⟩ := hb
      refine ⟨hlen, by omega, fun c => ?_⟩
      rw [he c]
      congr 1
      by_cases hc : c < row.length
      · rw [if_pos ⟨by omega, hc⟩, if_pos hc]
      · rw [if_neg (by omega), if_neg hc]

/-- The running class column sums after the first `i` coordinates. -/
def prefixSum (part : Nat → Nat) (w : Nat → ℚ) (rows : Nat → Nat → ℚ) (i r c : Nat) : ℚ :=
  ∑ j ∈ Finset.range i, if part j = r then w j * rows j c else 0

theorem vec_index_mut_ok {α : Type} {v : alloc.vec.Vec α} {i : Usize}
    {p : α × (α → alloc.vec.Vec α)}
    (h : alloc.vec.Vec.index_mut (core.slice.index.SliceIndexUsizeSlice α) v i = ok p) :
    ∃ hi : i.val < v.val.length, p = (v.val[i.val], alloc.vec.Vec.set v i) := by
  rw [alloc.vec.Vec.index_mut_slice_index] at h
  unfold alloc.vec.Vec.index_mut_usize at h
  split at h
  · rename_i x hx
    simp only [ok.injEq] at h
    subst h
    unfold alloc.vec.Vec.index_usize at hx
    split at hx
    · cases hx
    · rename_i y hy
      have hyx : y = x := by injection hx
      subst hyx
      have hy' : v.val[i.val]? = some y := hy
      obtain ⟨hi, hyv⟩ := List.getElem?_eq_some_iff.mp hy'
      exact ⟨hi, by rw [hyv]⟩
  · cases h
  · cases h

theorem weighted_totals_ok (hL : Lawful inst val) {partition : Slice Usize} {weights : Slice N}
    {rows : Slice (alloc.vec.Vec N)} {columns : Usize}
    {res : core.result.Result (alloc.vec.Vec (alloc.vec.Vec N)) (Refusal N)}
    (hlab : ∀ i (hi : i < partition.length), (partition.val[i]).val < partition.length)
    (_hwl : weights.length = partition.length) (hrl : rows.length = partition.length)
    (hwidth : ∀ i (hi : i < rows.length), (rows.val[i]).val.length = columns.val)
    (h : weighted_totals inst partition weights rows columns = ok res)
    {totals : alloc.vec.Vec (alloc.vec.Vec N)} (hres : res = .Ok totals) :
    ∀ r c, r < partition.length → c < columns.val →
      cell val totals.val r c =
        prefixSum (fun j => (partition.val.getD j 0#usize).val) (fun j => entry val weights.val j)
          (fun j c => cell val rows.val j c) partition.length r c := by
  let part := fun j => (partition.val.getD j 0#usize).val
  let w := fun j => entry val weights.val j
  let rq := fun j c => cell val rows.val j c
  let n := partition.length
  unfold weighted_totals at h
  peel t0 ht0 at h
  obtain ⟨ht0len, ht0w, ht0z⟩ := zero_totals_ok hL ht0
  have H := loop_ok
    (fun x => weighted_totals_loop.body inst partition weights rows x.1 x.2)
    (fun x => n - x.2.val)
    (fun x => x.1.val.length = n ∧ x.2.val ≤ n ∧
      (∀ r (hr : r < x.1.val.length), (x.1.val[r]).val.length = columns.val) ∧
      ∀ r c, r < n → c < columns.val → cell val x.1.val r c = prefixSum part w rq x.2.val r c)
    (fun res => ∀ totals, res = .Ok totals →
      ∀ r c, r < n → c < columns.val → cell val totals.val r c = prefixSum part w rq n r c)
    ?_ ?_ (t0, 0#usize) res
    ⟨by simp [ht0len, n], by simp, ht0w, fun r c _ _ => by simp [ht0z, prefixSum]⟩ h
  · exact H totals hres
  · rintro ⟨tt, i⟩ ⟨tt1, i1⟩ ⟨hlen, hle, hw, hsum⟩ hb
    unfold weighted_totals_loop.body at hb
    dsimp only at hb hlen hle hw hsum ⊢
    split at hb
    · rename_i hlt
      simp only [UScalar.lt_equiv, Slice.len_val] at hlt
      peel l hl at hb
      obtain ⟨_hli, hlv⟩ := index_ok hl
      have hln : l.val < n := by rw [← hlv]; exact hlab i.val hlt
      peel p hp at hb
      obtain ⟨hli, rfl⟩ := vec_index_mut_ok hp
      try dsimp only at hb
      peel q hq at hb
      simp only [lift, ok.injEq] at hq
      subst hq
      try dsimp only at hb
      peel v1 hv1 at hb
      obtain ⟨hv1l, hv1v⟩ := index_ok hv1
      peel wt hwt at hb
      obtain ⟨hwtl, hwtv⟩ := index_ok hwt
      peel ar har at hb
      obtain ⟨r, s2⟩ := ar
      try dsimp only at hb
      peel cf hcf at hb
      rcases branch_ok hcf with ⟨u, rfl, rfl⟩ | ⟨e, rfl, rfl⟩
      swap
      · peel_ r1 at hb; cases hb
      try dsimp only at hb
      peel i3 hi3 at hb
      simp only [ok.injEq, ControlFlow.cont.injEq, Prod.mk.injEq] at hb
      obtain ⟨rfl, rfl⟩ := hb
      have hiv := usize_succ hi3
      obtain ⟨hs2len, hrowle, hs2⟩ := add_weighted_row_ok hL har rfl
      have hrowlen : (alloc.vec.Vec.deref v1).length = columns.val := by
        simp only [alloc.vec.Vec.deref, Slice.length]
        rw [← hv1v]; exact hwidth i.val (by omega)
      refine ⟨⟨by simp [alloc.vec.Vec.set, hlen], by omega, fun r hr => ?_, fun r c hr hc => ?_⟩,
        by omega⟩
      · simp only [alloc.vec.Vec.set, List.length_set] at hr ⊢
        by_cases hrl : r = l.val
        · subst hrl
          simp only [List.getElem_set_self]
          simp only [Slice.length] at hs2len
          rw [hs2len]
          exact hw _ hli
        · rw [List.getElem_set_ne (Ne.symm hrl)]; exact hw r hr
      · have hpart : part i.val = l.val := by
          simp only [part, List.getD_eq_getElem?_getD, List.getElem?_eq_getElem hlt,
            Option.getD_some, hlv]
        unfold prefixSum
        rw [hiv, Finset.sum_range_succ]
        have hprev := hsum r c hr hc
        unfold prefixSum at hprev
        by_cases hrl : r = l.val
        · subst hrl
          simp only [cell, alloc.vec.Vec.set,
            List.getElem?_set_self (by omega : l.val < tt.val.length), Option.map_some,
            Option.getD_some]
          rw [hs2 c]
          simp only [cell, List.getElem?_eq_getElem (by omega : l.val < tt.val.length),
            Option.map_some, Option.getD_some] at hprev
          have hcr : c < (alloc.vec.Vec.deref v1).length := by rw [hrowlen]; exact hc
          rw [if_pos hcr]
          dsimp only [alloc.vec.Vec.deref]
          rw [hprev, if_pos hpart]
          congr 1
          simp only [w, rq, cell, entry, List.getElem?_eq_getElem hwtl,
            List.getElem?_eq_getElem hv1l, Option.map_some, Option.getD_some, hwtv, hv1v]
          ring
        · simp only [cell, alloc.vec.Vec.set,
            List.getElem?_set_ne (Ne.symm hrl)]
          rw [if_neg (by rw [hpart]; exact Ne.symm hrl), add_zero]
          exact hprev
    · cases hb
  · rintro ⟨tt, i⟩ res' ⟨hlen, hle, hw, hsum⟩ hb totals' hres'
    subst hres'
    unfold weighted_totals_loop.body at hb
    dsimp only at hb hlen hle hw hsum
    split at hb
    · peel_ l at hb
      peel_ p at hb
      obtain ⟨p1, p2⟩ := p
      try dsimp only at hb
      peel_ q at hb
      obtain ⟨q1, q2⟩ := q
      try dsimp only at hb
      peel_ v1 at hb
      peel_ wt at hb
      peel_ ar at hb
      obtain ⟨r, s2⟩ := ar
      try dsimp only at hb
      peel cf hcf at hb
      rcases branch_ok hcf with ⟨u, rfl, rfl⟩ | ⟨e, rfl, rfl⟩
      · dsimp only at hb; peel_ i3 at hb; cases hb
      · dsimp only at hb
        peel r1 hr1 at hb
        obtain ⟨e', he'⟩ := from_residual_err hr1
        simp only [ok.injEq, ControlFlow.done.injEq] at hb
        subst hb
        cases he'
    · rename_i hge
      simp only [UScalar.lt_equiv, Slice.len_val, not_lt] at hge
      simp only [ok.injEq, ControlFlow.done.injEq, core.result.Result.Ok.injEq] at hb
      subst hb
      intro r c hr hc
      rw [hsum r c hr hc, Nat.le_antisymm hle hge]

/-- Soundness of the production conservation check for one writer: if it
    accepts, the written certificate condition holds for the rational values of
    the inputs, so the writer conserves every class's weighted sum. -/
theorem check_writer_sound (hL : Lawful inst val) {partition : Slice Usize} {weights : Slice N}
    {rows : Slice (alloc.vec.Vec N)} {columns : Usize}
    (h : check_writer inst partition weights rows columns = ok (.Ok ())) :
    Balanced partition.length columns.val (fun i => (partition.val.getD i 0#usize).val)
      (fun i => entry val weights.val i) (fun i c => cell val rows.val i c) := by
  unfold check_writer at h
  split at h
  · cases h
  rename_i hc0
  dsimp only at h
  split at h
  · cases h
  rename_i hwl
  split at h
  · cases h
  rename_i hrl
  have hwl : weights.length = partition.length := by
    simpa [bne_iff_ne, ne_eq, UScalar.eq_equiv, Slice.len_val] using hwl
  have hrl : rows.length = partition.length := by
    simpa [bne_iff_ne, ne_eq, UScalar.eq_equiv, Slice.len_val] using hrl
  peel b hb at h
  cases b
  · rw [if_neg Bool.false_ne_true] at h; cases h
  rw [if_pos rfl] at h
  peel b1 hb1 at h
  cases b1
  · rw [if_neg Bool.false_ne_true] at h; cases h
  rw [if_pos rfl] at h
  peel r hr at h
  peel cf hcf at h
  rcases branch_ok hcf with ⟨totals, rfl, rfl⟩ | ⟨e, rfl, rfl⟩
  swap
  · obtain ⟨_, he⟩ := from_residual_err h; cases he
  dsimp only at h
  peel b2 hb2 at h
  cases b2
  · rw [if_neg Bool.false_ne_true] at h; cases h
  have hlab := labels_in_range_true hb
  have hwidth := rows_have_width_true hb1
  have hz := all_zero_true hL hb2
  have ht := weighted_totals_ok hL hlab hwl hrl hwidth hr rfl
  intro l c hc
  by_cases hl : l < partition.length
  · have := ht l c hl hc
    have hz' : cell val totals.val l c = 0 := hz l c
    rw [hz'] at this
    unfold classSum
    unfold prefixSum at this
    exact this.symm
  · unfold classSum
    apply Finset.sum_eq_zero
    intro i hi
    rw [if_neg]
    intro he
    apply hl
    rw [← he]
    have hi := Finset.mem_range.mp hi
    simp only [List.getD_eq_getElem?_getD, List.getElem?_eq_getElem hi, Option.getD_some]
    exact hlab i hi

/-- End to end: an accepted writer conserves every class's weighted sum of the
    rational values, for every state and every parameter valuation. -/
theorem check_writer_conserves (hL : Lawful inst val) {partition : Slice Usize}
    {weights : Slice N} {rows : Slice (alloc.vec.Vec N)} {columns : Usize}
    (h : check_writer inst partition weights rows columns = ok (.Ok ())) (x σ : Nat → ℚ)
    (l : Nat) :
    classSum partition.length (fun i => (partition.val.getD i 0#usize).val)
        (fun i => entry val weights.val i)
        l (step columns.val (fun i c => cell val rows.val i c) x σ) =
      classSum partition.length (fun i => (partition.val.getD i 0#usize).val)
        (fun i => entry val weights.val i) l σ :=
  step_conserves (check_writer_sound hL h) x σ l

end Norn.Checker.Certificates.Conservation

#print axioms Norn.Checker.Certificates.Conservation.check_writer_sound
#print axioms Norn.Checker.Certificates.Conservation.check_writer_conserves
