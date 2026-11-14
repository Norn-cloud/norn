import LensCore.Funs
import Checker.Certificates.Support
import Checker.Certificates.LensReference

/-!
# Production lens lookups and law check meet the written lens semantics

`LensCore` is the fresh Aeneas translation of the production file
`crates/norn-checker/src/mapping/lens/decision.rs`, generic over the source
state type `S` and its `PartialEq` instance. The only assumption on that
instance is `LawfulEq`: `==` is structural equality, as the derived
`PartialEq` of `BTreeMap<String, bool>` is (registered trust boundary).

* `get_correct` / `put_correct`: for every input the production lookups
  return (never panic or diverge) exactly the table semantics of
  `LensReference`, with `Ok` precisely when the written semantics defines a value.
* `check_laws_sound`: if the production check admits the tables, the lens laws
  hold on the whole declared domain (`LawsOn`), so the admitted lens is a
  well-behaved total lens there (`LawsOn.toLens_wellBehaved`) and composes
  (`Lens.comp_wellBehaved`).
-/

namespace Norn.Checker.Certificates.Lens
open Aeneas Aeneas.Std Result
open norn_checker_lens
open Norn.Checker.Certificates

variable {S : Type}

/-- The instance's `eq` is structural equality and never fails. -/
def LawfulEq (inst : core.cmp.PartialEq S S) : Prop :=
  ∀ a b, ∃ r, inst.eq a b = ok r ∧ (r = true ↔ a = b)

/-- The `Ok` payload of a production result, if any. -/
def okPart {T E : Type} : Result (core.result.Result T E) → Option T
  | ok (.Ok v) => some v
  | _ => none

/-- A put-table row as a pair of row numbers. -/
def pairOf (p : Std.Array Usize 2#usize) : Nat × Nat := (p.val[0]!.val, p.val[1]!.val)

variable {inst : core.cmp.PartialEq S S}

theorem same_ok (hEq : LawfulEq inst) (a b : S) :
    ∃ r, same inst a b = ok r ∧ (r = true ↔ a = b) := by
  obtain ⟨r, hr, hiff⟩ := hEq a b
  exact ⟨r, by simp [same, hr], hiff⟩

open Classical in
theorem same_eq (hEq : LawfulEq inst) (a b : S) : same inst a b = ok (decide (a = b)) := by
  obtain ⟨r, hr, hiff⟩ := same_ok hEq a b
  rw [hr]
  cases r
  · simp only [Bool.false_eq_true, false_iff] at hiff; simp [hiff]
  · simp only [true_iff] at hiff; simp [hiff]

theorem same_true (hEq : LawfulEq inst) {a b : S} (h : same inst a b = ok true) : a = b := by
  obtain ⟨r, hr, hiff⟩ := same_ok hEq a b
  rw [h] at hr
  cases hr
  exact hiff.mp rfl

/-! ## Lookups: total correctness against the table semantics -/

theorem position_correct (hEq : LawfulEq inst) (sources : Slice S) (s : S) :
    ∃ o, position inst sources s = ok o ∧
      (∀ r, o = some r → r.val < sources.length) ∧
      FirstFrom sources.val s 0 (o.map (·.val)) := by
  classical
  let I : Usize → Prop := fun row =>
    row.val ≤ sources.length ∧ ∀ j, j < row.val → sources.val[j]? ≠ some s
  let Q : Option Usize → Prop := fun o =>
    (∀ r, o = some r → r.val < sources.length) ∧ FirstFrom sources.val s 0 (o.map (·.val))
  have H := loop_total (position_loop.body inst sources s)
    (fun row => sources.length - row.val) I Q ?_ 0#usize ⟨by simp, by simp⟩
  · exact H
  intro row ⟨hle, hnot⟩
  unfold position_loop.body
  dsimp only
  split
  · rename_i hlt'
    have hlt : row.val < sources.length := by simpa using hlt'
    rw [index_of_lt sources row hlt, bind_tc_ok, same_eq hEq, bind_tc_ok]
    split
    · rename_i heq
      have heq : sources.val[row.val] = s := of_decide_eq_true heq
      refine ⟨_, rfl, ?_⟩
      show (∀ r : Usize, some row = some r → r.val < sources.length) ∧
        FirstFrom sources.val s 0 (some row.val)
      refine ⟨fun r hr => by cases hr; exact hlt, Nat.zero_le _, ?_, fun j _ hj => hnot j hj⟩
      rw [List.getElem?_eq_getElem hlt, heq]
    · rename_i hne
      have hne : ¬ sources.val[row.val] = s := fun e => hne (decide_eq_true e)
      have hmax : row.val < Usize.max := Nat.lt_of_lt_of_le hlt sources.property
      obtain ⟨j, hj, hjv⟩ := usize_succ_ok row hmax
      rw [hj, bind_tc_ok]
      refine ⟨_, rfl, ⟨by omega, fun k hk => ?_⟩, by omega⟩
      by_cases hkr : k < row.val
      · exact hnot k hkr
      · have hk' : k = row.val := by omega
        subst hk'
        rw [List.getElem?_eq_getElem hlt]
        intro hsome
        exact hne (Option.some.inj hsome)
  · rename_i hge
    have hge : ¬ row.val < sources.length := by simpa using hge
    refine ⟨_, rfl, ?_⟩
    show _ ∧ FirstFrom sources.val s 0 none
    constructor
    · intro r hr; cases hr
    intro j _
    by_cases hj : j < row.val
    · exact hnot j hj
    · rw [List.getElem?_eq_none (Nat.le_trans (Nat.le_of_not_lt hge) (Nat.le_of_not_lt hj))]
      simp

/-- `get` is the table semantics, for every input: it returns `Ok v` exactly
    when the written semantics gives `some v`, and otherwise `Err`. -/
theorem get_correct (hEq : LawfulEq inst) (sources : Slice S) (gets : Slice Bool) (s : S) :
    ∃ res, norn_checker_lens.get inst sources gets s = ok res ∧
      TableGet sources.val gets.val s (okPart (ok res)) := by
  obtain ⟨o, ho, hlen, hfirst⟩ := position_correct hEq sources s
  unfold norn_checker_lens.get
  rw [ho, bind_tc_ok]
  cases o with
  | none => exact ⟨_, rfl, none, hfirst, rfl⟩
  | some r =>
    dsimp only
    split
    · rename_i hr'
      have hr : r.val < gets.length := by simpa using hr'
      rw [index_of_lt gets r hr, bind_tc_ok]
      refine ⟨_, rfl, some r.val, hfirst, ?_⟩
      simp [okPart, List.getElem?_eq_getElem hr]
    · rename_i hr'
      have hr : ¬ r.val < gets.length := by simpa using hr'
      refine ⟨_, rfl, some r.val, hfirst, ?_⟩
      simp [okPart, List.getElem?_eq_none (by omega : gets.length ≤ r.val)]

/-- `put` is the table semantics, for every input. -/
theorem put_correct (hEq : LawfulEq inst) (sources : Slice S)
    (puts : Slice (Std.Array Usize 2#usize)) (s : S) (v : Bool) :
    ∃ res, put inst sources puts s v = ok res ∧
      TablePut sources.val (puts.val.map pairOf) s v (okPart (ok res)) := by
  obtain ⟨o, ho, hlen, hfirst⟩ := position_correct hEq sources s
  unfold put
  rw [ho, bind_tc_ok]
  cases o with
  | none => exact ⟨_, rfl, none, hfirst, rfl⟩
  | some r =>
    dsimp only
    split
    · rename_i hr'
      have hr : ¬ r.val < puts.length := by simpa using hr'
      refine ⟨_, rfl, some r.val, hfirst, ?_⟩
      simp [okPart, List.getElem?_eq_none (by omega : puts.length ≤ r.val)]
    · rename_i hr'
      have hr : r.val < puts.length := by simpa using hr'
      have hlen2 : (puts.val[r.val]'hr).val.length = 2 := (puts.val[r.val]'hr).property
      have hspec : (some r.val).bind (fun r => (puts.val.map pairOf)[r]?) =
          some (pairOf (puts.val[r.val]'hr)) := by
        simp [List.getElem?_eq_getElem hr]
      cases v
      all_goals
        simp only [Bool.false_eq_true, if_true, if_false]
        rw [index_of_lt puts r hr, bind_tc_ok, array_index_of_lt _ _ (by simp [hlen2]),
          bind_tc_ok]
        try dsimp only
        split
        · rename_i hds
          simp only [UScalar.lt_equiv, Slice.len_val] at hds
          rw [index_of_lt sources _ hds, bind_tc_ok]
          refine ⟨_, rfl, some r.val, hfirst, ?_⟩
          rw [hspec]
          simp [okPart, pairOf, getElem!_pos]
        · rename_i hds
          simp only [UScalar.lt_equiv, Slice.len_val] at hds
          refine ⟨_, rfl, some r.val, hfirst, ?_⟩
          rw [hspec]
          simp [okPart, pairOf, getElem!_pos]
          exact Nat.le_of_not_lt hds

/-- Every put-back the production returns is a declared source state. -/
theorem put_mem {sources : Slice S} {puts : Slice (Std.Array Usize 2#usize)} {s : S} {v : Bool}
    {u : S} (h : put inst sources puts s v = ok (.Ok u)) : u ∈ sources.val := by
  unfold put at h
  peel_ o at h
  cases o with
  | none => simp at h
  | some r =>
    dsimp only at h
    split at h
    · simp at h
    · peel_ d at h
      split at h
      · peel t ht at h
        obtain ⟨hi, hx⟩ := index_ok ht
        simp at h
        subst h
        rw [← hx]
        exact List.getElem_mem hi
      · simp at h

/-! ## The law check: soundness -/

/-- The production lookups as partial functions. -/
def prodGet (inst : core.cmp.PartialEq S S) (sources : Slice S) (gets : Slice Bool) (s : S) :
    Option Bool :=
  okPart (norn_checker_lens.get inst sources gets s)

def prodPut (inst : core.cmp.PartialEq S S) (sources : Slice S)
    (puts : Slice (Std.Array Usize 2#usize)) (s : S) (v : Bool) : Option S :=
  okPart (put inst sources puts s v)

/-- PutGet at view `view` and PutPut after it, at the source in row `row`. -/
theorem check_view_sound (hEq : LawfulEq inst) {sources : Slice S} {gets : Slice Bool}
    {puts : Slice (Std.Array Usize 2#usize)} {row : Usize} {view : Bool}
    (h : check_view inst sources gets puts row view = ok (.Ok ())) :
    ∃ s, sources.val[row.val]? = some s ∧
      ∃ u, put inst sources puts s view = ok (.Ok u) ∧ norn_checker_lens.get inst sources gets u = ok (.Ok view) ∧
        ∀ w, ∃ t, put inst sources puts u w = ok (.Ok t) ∧
          put inst sources puts s w = ok (.Ok t) := by
  unfold check_view at h
  peel s hs at h
  obtain ⟨hi, hs⟩ := index_ok hs
  refine ⟨s, by rw [List.getElem?_eq_getElem hi, hs], ?_⟩
  peel r hr at h
  peel cf hcf at h
  rcases branch_ok hcf with ⟨u, rfl, rfl⟩ | ⟨e, rfl, rfl⟩
  swap
  · obtain ⟨_, he⟩ := from_residual_err h; cases he
  refine ⟨u, hr, ?_⟩
  peel r1 hr1 at h
  peel cf1 hcf1 at h
  rcases branch_ok hcf1 with ⟨g, rfl, rfl⟩ | ⟨e, rfl, rfl⟩
  swap
  · obtain ⟨_, he⟩ := from_residual_err h; cases he
  dsimp only at h
  cases hgv : (g != view)
  swap
  · rw [hgv, if_pos rfl] at h; cases h
  rw [hgv, if_neg Bool.false_ne_true] at h
  have hg : g = view := by simpa using hgv
  subst hg
  refine ⟨hr1, ?_⟩
  peel r2 hr2 at h
  peel cf2 hcf2 at h
  rcases branch_ok hcf2 with ⟨t0, rfl, rfl⟩ | ⟨e, rfl, rfl⟩
  swap
  · obtain ⟨_, he⟩ := from_residual_err h; cases he
  dsimp only at h
  peel r3 hr3 at h
  peel cf3 hcf3 at h
  rcases branch_ok hcf3 with ⟨t0', rfl, rfl⟩ | ⟨e, rfl, rfl⟩
  swap
  · obtain ⟨_, he⟩ := from_residual_err h; cases he
  dsimp only at h
  peel b hb at h
  cases b
  · rw [if_neg Bool.false_ne_true] at h; cases h
  rw [if_pos rfl] at h
  have heq0 : t0 = t0' := same_true hEq hb
  subst heq0
  peel r4 hr4 at h
  peel cf4 hcf4 at h
  rcases branch_ok hcf4 with ⟨t1, rfl, rfl⟩ | ⟨e, rfl, rfl⟩
  swap
  · obtain ⟨_, he⟩ := from_residual_err h; cases he
  dsimp only at h
  peel r5 hr5 at h
  peel cf5 hcf5 at h
  rcases branch_ok hcf5 with ⟨t1', rfl, rfl⟩ | ⟨e, rfl, rfl⟩
  swap
  · obtain ⟨_, he⟩ := from_residual_err h; cases he
  dsimp only at h
  peel b1 hb1 at h
  cases b1
  · rw [if_neg Bool.false_ne_true] at h; cases h
  have heq1 : t1 = t1' := same_true hEq hb1
  subst heq1
  intro w
  cases w
  · exact ⟨t0, hr2, hr3⟩
  · exact ⟨t1, hr4, hr5⟩

/-- All three laws at the source in row `row`. -/
def RowLaws (inst : core.cmp.PartialEq S S) (sources : Slice S) (gets : Slice Bool)
    (puts : Slice (Std.Array Usize 2#usize)) (j : Nat) : Prop :=
  ∃ s, sources.val[j]? = some s ∧ ∃ g, norn_checker_lens.get inst sources gets s = ok (.Ok g) ∧
    put inst sources puts s g = ok (.Ok s) ∧
    ∀ v, ∃ u, put inst sources puts s v = ok (.Ok u) ∧ norn_checker_lens.get inst sources gets u = ok (.Ok v) ∧
      ∀ w, ∃ t, put inst sources puts u w = ok (.Ok t) ∧ put inst sources puts s w = ok (.Ok t)

theorem check_laws_rows (hEq : LawfulEq inst) {sources : Slice S} {gets : Slice Bool}
    {puts : Slice (Std.Array Usize 2#usize)}
    (h : check_laws inst sources gets puts = ok (.Ok ())) :
    ∀ j, j < sources.length → RowLaws inst sources gets puts j := by
  let I : Usize → Prop := fun row => ∀ j, j < row.val → RowLaws inst sources gets puts j
  let Q : core.result.Result Unit Refusal → Prop := fun res =>
    res = .Ok () → ∀ j, j < sources.length → RowLaws inst sources gets puts j
  refine loop_ok (check_laws_loop.body inst sources gets puts)
    (fun row => sources.length - row.val) I Q ?_ ?_ 0#usize (.Ok ())
    (fun j hj => by simp at hj) h rfl
  · intro row row' hinv hb
    unfold check_laws_loop.body at hb
    dsimp only at hb
    split at hb
    swap
    · simp at hb
    rename_i hlt
    have hlt : row.val < sources.length := by simpa using hlt
    peel s hs at hb
    obtain ⟨hi, hs⟩ := index_ok hs
    peel r hr at hb
    peel cf hcf at hb
    rcases branch_ok hcf with ⟨g, rfl, rfl⟩ | ⟨e, rfl, rfl⟩
    swap
    · peel_ _x at hb; cases hb
    dsimp only at hb
    peel r1 hr1 at hb
    peel cf1 hcf1 at hb
    rcases branch_ok hcf1 with ⟨u, rfl, rfl⟩ | ⟨e, rfl, rfl⟩
    swap
    · peel_ _x at hb; cases hb
    dsimp only at hb
    peel b hbs at hb
    cases b
    · rw [if_neg Bool.false_ne_true] at hb; cases hb
    rw [if_pos rfl] at hb
    have hus : u = s := same_true hEq hbs
    subst hus
    peel r2 hr2 at hb
    peel cf2 hcf2 at hb
    rcases branch_ok hcf2 with ⟨_, rfl, rfl⟩ | ⟨e, rfl, rfl⟩
    swap
    · peel_ _x at hb; cases hb
    dsimp only at hb
    peel r3 hr3 at hb
    peel cf3 hcf3 at hb
    rcases branch_ok hcf3 with ⟨_, rfl, rfl⟩ | ⟨e, rfl, rfl⟩
    swap
    · peel_ _x at hb; cases hb
    dsimp only at hb
    peel row1 hrow1 at hb
    have hrow1v := usize_succ hrow1
    simp at hb
    subst hb
    refine ⟨fun j hj => ?_, by omega⟩
    by_cases hjr : j < row.val
    · exact hinv j hjr
    have hj' : j = row.val := by omega
    subst hj'
    obtain ⟨s0, hs0, u0, hu0, hgu0, hw0⟩ := check_view_sound hEq hr2
    obtain ⟨s1, hs1, u1, hu1, hgu1, hw1⟩ := check_view_sound hEq hr3
    have hsrc : sources.val[row.val]? = some u := by rw [List.getElem?_eq_getElem hi, hs]
    rw [hsrc] at hs0 hs1
    cases hs0
    cases hs1
    refine ⟨u, hsrc, g, hr, hr1, fun v => ?_⟩
    cases v
    · exact ⟨u0, hu0, hgu0, hw0⟩
    · exact ⟨u1, hu1, hgu1, hw1⟩
  · intro row res hinv hb hres
    subst hres
    unfold check_laws_loop.body at hb
    dsimp only at hb
    split at hb
    · exfalso
      peel_ s at hb
      peel_ r at hb
      peel cf hcf at hb
      rcases branch_ok hcf with ⟨g, rfl, rfl⟩ | ⟨e, rfl, rfl⟩
      swap
      · peel x hx at hb
        obtain ⟨_, he⟩ := from_residual_err hx
        simp at hb; subst hb; cases he
      dsimp only at hb
      peel_ r1 at hb
      peel cf1 hcf1 at hb
      rcases branch_ok hcf1 with ⟨u, rfl, rfl⟩ | ⟨e, rfl, rfl⟩
      swap
      · peel x hx at hb
        obtain ⟨_, he⟩ := from_residual_err hx
        simp at hb; subst hb; cases he
      dsimp only at hb
      peel_ b at hb
      cases b
      · rw [if_neg Bool.false_ne_true] at hb; cases hb
      rw [if_pos rfl] at hb
      peel_ r2 at hb
      peel cf2 hcf2 at hb
      rcases branch_ok hcf2 with ⟨_, rfl, rfl⟩ | ⟨e, rfl, rfl⟩
      swap
      · peel x hx at hb
        obtain ⟨_, he⟩ := from_residual_err hx
        simp at hb; subst hb; cases he
      dsimp only at hb
      peel_ r3 at hb
      peel cf3 hcf3 at hb
      rcases branch_ok hcf3 with ⟨_, rfl, rfl⟩ | ⟨e, rfl, rfl⟩
      swap
      · peel x hx at hb
        obtain ⟨_, he⟩ := from_residual_err hx
        simp at hb; subst hb; cases he
      dsimp only at hb
      peel_ _x at hb
      simp at hb
    · rename_i hge
      have hge : sources.length ≤ row.val := by simpa using hge
      intro j hj
      exact hinv j (by omega)

/-- Soundness of the production law check: if it admits the tables, the lens
    laws hold on the declared finite domain for the production lookups. -/
theorem check_laws_sound (hEq : LawfulEq inst) {sources : Slice S} {gets : Slice Bool}
    {puts : Slice (Std.Array Usize 2#usize)}
    (h : check_laws inst sources gets puts = ok (.Ok ())) :
    LawsOn sources.val (prodGet inst sources gets) (prodPut inst sources puts) := by
  intro s hs
  obtain ⟨j, hj, hjs⟩ := List.getElem_of_mem hs
  obtain ⟨s', hs', g, hg, hgs, hv⟩ := check_laws_rows hEq h j hj
  rw [List.getElem?_eq_getElem hj, hjs] at hs'
  cases hs'
  refine ⟨g, by simp [prodGet, okPart, hg], by simp [prodPut, okPart, hgs], fun v => ?_⟩
  obtain ⟨u, hu, hgu, hw⟩ := hv v
  refine ⟨u, by simp [prodPut, okPart, hu], put_mem hu, by simp [prodGet, okPart, hgu],
    fun w => ?_⟩
  obtain ⟨t, hut, hst⟩ := hw w
  exact ⟨t, by simp [prodPut, okPart, hut], by simp [prodPut, okPart, hst]⟩

/-- The admitted lens is a well-behaved total lens on its declared domain. -/
theorem check_laws_wellBehaved (hEq : LawfulEq inst) {sources : Slice S} {gets : Slice Bool}
    {puts : Slice (Std.Array Usize 2#usize)}
    (h : check_laws inst sources gets puts = ok (.Ok ())) :
    (check_laws_sound hEq h).toLens.WellBehaved :=
  LawsOn.toLens_wellBehaved _

end Norn.Checker.Certificates.Lens

#print axioms Norn.Checker.Certificates.Lens.get_correct
#print axioms Norn.Checker.Certificates.Lens.put_correct
#print axioms Norn.Checker.Certificates.Lens.check_laws_sound
#print axioms Norn.Checker.Certificates.Lens.check_laws_wellBehaved
