import NormDecision.Funs
import Checker.NormPrecedence.Loops
import NornSem.Norm

/-!
# The extracted admission decision decides its written conditions

Every function on the admission path of `norm_precedence/decision.rs`, as
extracted by Aeneas, returns (without panic or divergence) a Boolean that is
`true` exactly when its written condition holds. `admits_decides` composes them.
-/

set_option linter.unusedSimpArgs false
set_option linter.unnecessarySeqFocus false

namespace Norn.Checker.NormPrecedence
open Aeneas Aeneas.Std Result ControlFlow norn_checker_norm_decision

abbrev Norms := Slice decision.Norm
abbrev Edges := Slice (Usize × Usize)
abbrev Reach := Slice (alloc.vec.Vec Bool)

/-- The closure certificate's entry `(i, j)`; absent entries read `false`. -/
def R (reach : Reach) (i j : Nat) : Bool :=
  ((reach.val[i]?).bind (fun r => r.val[j]?)).getD false

/-- The written grade of an extracted grade. -/
def toModality : decision.Modality → Norn.Sem.Norm.Modality
  | .Obligation => .obligation
  | .Permission => .permission
  | .Prohibition => .prohibition

/-- Grade, layer, protection and activity of norm `i`, read from the slice. -/
def modOf (norms : Norms) (i : Nat) : Norn.Sem.Norm.Modality :=
  ((norms.val[i]?).map (fun n => toModality n.modality)).getD .obligation
def layerOf (norms : Norms) (i : Nat) : Nat := ((norms.val[i]?).map (fun n => n.layer.val)).getD 0
def protOf (norms : Norms) (i : Nat) : Bool := ((norms.val[i]?).map (fun n => n.«protected»)).getD false
def actOf (norms : Norms) (i c : Nat) : Bool :=
  ((norms.val[i]?).bind (fun n => n.active.val[c]?)).getD false

/-- The written catalog the extracted inputs denote. -/
def catalog (norms : Norms) (edges : Edges) (contexts : Usize) : Norn.Sem.Norm.Catalog where
  size := norms.length
  contexts := contexts.val
  modality := modOf norms
  layer := layerOf norms
  isProtected := protOf norms
  active := actOf norms
  edges := edges.val.map (fun e => (e.1.val, e.2.val))

/-! ## Reading slices -/

theorem sidx {α : Type} (s : Slice α) (i : Usize) (h : i.val < s.length) :
    Slice.index_usize s i = ok (s.val[i.val]'h) := by
  obtain ⟨x, hx, rfl⟩ := WP.spec_imp_exists (Slice.index_usize_spec s i h); exact hx

theorem vidx {α : Type} (v : alloc.vec.Vec α) (i : Usize) (h : i.val < v.length) :
    alloc.vec.Vec.index (core.slice.index.SliceIndexUsizeSlice α) v i = ok (v.val[i.val]'h) := by
  rw [alloc.vec.Vec.index_slice_index]
  obtain ⟨x, hx, rfl⟩ := WP.spec_imp_exists (alloc.vec.Vec.index_usize_spec v i h); exact hx

theorem vidxu {α : Type} (v : alloc.vec.Vec α) (i : Usize) (h : i.val < v.length) :
    alloc.vec.Vec.index_usize v i = ok (v.val[i.val]'h) := by
  obtain ⟨x, hx, rfl⟩ := WP.spec_imp_exists (alloc.vec.Vec.index_usize_spec v i h); exact hx

theorem lt_len {α : Type} (i : Usize) (s : Slice α) : i < Slice.len s ↔ i.val < s.length := by
  simp [UScalar.lt_equiv]

theorem usize_bne (a b : Usize) : (a != b) = !decide (a.val = b.val) := by
  by_cases h : a.val = b.val
  · have : a = b := (UScalar.eq_equiv a b).mpr h
    subst this; simp
  · have : a ≠ b := fun e => h (by rw [e])
    simp [h, this]

theorem scalar_le {ty : UScalarTy} (a b : UScalar ty) : a ≤ b ↔ a.val ≤ b.val := Iff.rfl
theorem scalar_lt {ty : UScalarTy} (a b : UScalar ty) : a < b ↔ a.val < b.val := Iff.rfl

theorem usize_eq (a b : Usize) : a = b ↔ a.val = b.val := UScalar.eq_equiv a b

theorem getD_true (o : Option Bool) : o.getD false = true ↔ o = some true := by
  cases o <;> simp

theorem R_eq (reach : Reach) {i j : Nat} (hi : i < reach.length) (hj : j < (reach.val[i]).length) :
    R reach i j = (reach.val[i]).val[j] := by
  simp [R, List.getElem?_eq_getElem hi, List.getElem?_eq_getElem hj]

/-! ## Grades -/

theorem prohibits_eq (m : decision.Modality) :
    decision.prohibits m = ok (decide (toModality m = .prohibition)) := by
  cases m <;> rfl

theorem obliges_eq (m : decision.Modality) :
    decision.obliges m = ok (decide (toModality m = .obligation)) := by
  cases m <;> rfl

theorem opposed_eq (a b : decision.Norm) :
    decision.opposed a b = ok (decide ((toModality a.modality).opposed (toModality b.modality))) := by
  simp only [decision.opposed, prohibits_eq, bind_tc_ok]
  cases a.modality <;> cases b.modality <;> simp [toModality, Norn.Sem.Norm.Modality.opposed]

theorem active_in_eq (n : decision.Norm) (c : Usize) (h : c.val < n.active.length) :
    decision.active_in n c = ok (n.active.val[c.val]) := by
  simp [decision.active_in, vidx _ c h]

/-! ## Shape -/

/-- The certificate is square, every mask covers every context and every
    edge joins two norms. -/
structure Shaped (norms : Norms) (edges : Edges) (reach : Reach) (contexts : Usize) : Prop where
  reach_len : reach.length = norms.length
  rows : ∀ k (h : k < reach.length), (reach.val[k]).length = norms.length
  masks : ∀ k (h : k < norms.length), (norms.val[k]).active.length = contexts.val
  edges_in : ∀ e ∈ edges.val, e.1.val < norms.length ∧ e.2.val < norms.length

theorem rows_shaped_decides (norms : Norms) (reach : Reach) (contexts : Usize)
    (hlen : reach.length = norms.length) :
    Decides (decision.rows_shaped norms reach contexts)
      (∀ k, k < norms.length →
        (reach.val[k]?).map (fun r => r.length) = some norms.length ∧
        (norms.val[k]?).map (fun n => n.active.length) = some contexts.val) := by
  have key := loop_all (body := decision.rows_shaped_loop.body norms reach contexts)
    (n := norms.length) (P := fun k =>
        (reach.val[k]?).map (fun r => r.length) = some norms.length ∧
        (norms.val[k]?).map (fun n => n.active.length) = some contexts.val) ?step ?stop 0#usize
  · exact key.congr (by simp)
  case step =>
    intro i hi
    obtain ⟨j, hj, hjv⟩ := succ_ok i norms.length norms.property hi
    refine ⟨j, hjv, ?_⟩
    have hr : i.val < reach.length := by omega
    simp only [decision.rows_shaped_loop.body, (lt_len i norms).mpr hi, if_true,
      sidx reach i hr, sidx norms i hi, bind_tc_ok, hj, usize_bne, alloc.vec.Vec.len_val,
      Slice.len_val, List.getElem?_eq_getElem hr, List.getElem?_eq_getElem hi, Option.map_some,
      Option.some.injEq]
    by_cases h1 : (reach.val[i.val]).length = norms.length <;>
      by_cases h2 : (norms.val[i.val]).active.length = contexts.val <;> simp [h1, h2]
  case stop =>
    intro i hi
    have : ¬ i < Slice.len norms := by rw [lt_len]; omega
    simp [decision.rows_shaped_loop.body, this]

theorem edges_in_range_decides (edges : Edges) (n : Usize) :
    Decides (decision.edges_in_range edges n)
      (∀ k, k < edges.length → ∀ e ∈ edges.val[k]?, e.1.val < n.val ∧ e.2.val < n.val) := by
  have key := loop_all (body := decision.edges_in_range_loop.body edges n)
    (n := edges.length)
    (P := fun k => ∀ e ∈ edges.val[k]?, e.1.val < n.val ∧ e.2.val < n.val) ?step ?stop 0#usize
  · exact key.congr (by simp)
  case step =>
    intro i hi
    obtain ⟨j, hj, hjv⟩ := succ_ok i edges.length edges.property hi
    refine ⟨j, hjv, ?_⟩
    simp only [decision.edges_in_range_loop.body, (lt_len i edges).mpr hi, if_true,
      sidx edges i hi, bind_tc_ok, hj, List.getElem?_eq_getElem hi, Option.mem_def,
      Option.some.injEq, forall_eq']
    generalize edges.val[i.val] = p
    obtain ⟨a, b⟩ := p
    by_cases h1 : n.val ≤ a.val <;> by_cases h2 : n.val ≤ b.val <;>
      simp [h1, h2, ge_iff_le, scalar_le] <;> omega
  case stop =>
    intro i hi
    have : ¬ i < Slice.len edges := by rw [lt_len]; omega
    simp [decision.edges_in_range_loop.body, this]

theorem mem_of_getElem? {α : Type} {l : List α} {x : α} :
    (∃ k, k < l.length ∧ x ∈ l[k]?) ↔ x ∈ l := by
  rw [List.mem_iff_getElem?]
  constructor
  · rintro ⟨k, -, h⟩; exact ⟨k, h⟩
  · rintro ⟨k, h⟩
    exact ⟨k, (List.getElem?_eq_some_iff.mp h).1, h⟩

theorem from_zero {n : Nat} {P : Nat → Prop} :
    (∀ k, (0#usize).val ≤ k → k < n → P k) ↔ ∀ k, k < n → P k := by
  simp

theorem forall_getElem? {α : Type} {l : List α} {P : α → Prop} :
    (∀ k, k < l.length → ∀ x ∈ l[k]?, P x) ↔ ∀ x ∈ l, P x := by
  constructor
  · intro h x hx
    obtain ⟨k, hk⟩ := List.mem_iff_getElem?.mp hx
    exact h k (List.getElem?_eq_some_iff.mp hk).1 x hk
  · intro h k _ x hx
    obtain ⟨_, rfl⟩ := List.getElem?_eq_some_iff.mp hx
    exact h _ (List.getElem_mem _)

theorem well_shaped_decides (norms : Norms) (edges : Edges) (reach : Reach) (contexts : Usize) :
    Decides (decision.well_shaped norms edges reach contexts) (Shaped norms edges reach contexts) := by
  unfold decision.well_shaped
  by_cases hlen : reach.length = norms.length
  · have heq : Slice.len reach = Slice.len norms := by
      rw [usize_eq]; simp [hlen]
    simp only [heq, if_true]
    have h := (rows_shaped_decides norms reach contexts hlen).and
      (fun _ => edges_in_range_decides edges (Slice.len norms))
    refine h.congr ?_
    rw [forall_getElem?]
    constructor
    · rintro ⟨hrows, hedges⟩
      refine ⟨hlen, ?_, ?_, ?_⟩
      · intro k hk
        have := (hrows k (by omega)).1
        simpa [List.getElem?_eq_getElem hk] using this
      · intro k hk
        have := (hrows k hk).2
        simpa [List.getElem?_eq_getElem hk] using this
      · intro e he; simpa using hedges e he
    · rintro ⟨-, hrows, hmasks, hedges⟩
      refine ⟨fun k hk => ⟨?_, ?_⟩, fun e he => by simpa using hedges e he⟩
      · have hk' : k < reach.length := by omega
        simp [List.getElem?_eq_getElem hk', hrows k hk']
      · simp [List.getElem?_eq_getElem hk, hmasks k hk]
  · have hne : ¬ Slice.len reach = Slice.len norms := by
      rw [usize_eq]; simpa using hlen
    simp only [hne, if_false]
    exact ⟨false, rfl, by simp; intro h; exact hlen h.reach_len⟩

/-! ## Layers and protection -/

theorem edges_respect_layers_decides (norms : Norms) (edges : Edges) (reach : Reach) (contexts : Usize)
    (hS : Shaped norms edges reach contexts) :
    Decides (decision.edges_respect_layers norms edges)
      (∀ e ∈ edges.val, layerOf norms e.1.val ≤ layerOf norms e.2.val ∧
        protOf norms e.2.val = false) := by
  have key := loop_all (body := decision.edges_respect_layers_loop.body norms edges)
    (n := edges.length) (P := fun k => ∀ e ∈ edges.val[k]?,
      layerOf norms e.1.val ≤ layerOf norms e.2.val ∧ protOf norms e.2.val = false) ?step ?stop 0#usize
  · exact (key.congr from_zero).congr forall_getElem?
  case step =>
    intro i hi
    obtain ⟨j, hj, hjv⟩ := succ_ok i edges.length edges.property hi
    refine ⟨j, hjv, ?_⟩
    have hin := hS.edges_in _ (List.getElem_mem hi)
    simp only [decision.edges_respect_layers_loop.body, (lt_len i edges).mpr hi, if_true,
      sidx edges i hi, bind_tc_ok, hj, List.getElem?_eq_getElem hi, Option.mem_def,
      Option.some.injEq, forall_eq']
    generalize edges.val[i.val] = p at hin
    obtain ⟨a, b⟩ := p
    obtain ⟨ha, hb⟩ := hin
    dsimp only at ha hb
    change a.val < norms.val.length at ha
    change b.val < norms.val.length at hb
    by_cases h1 : (norms.val[b.val]).layer.val < (norms.val[a.val]).layer.val <;>
      cases h2 : (norms.val[b.val]).«protected» <;>
      simp [sidx norms a ha, sidx norms b hb, layerOf, protOf, List.getElem?_eq_getElem ha,
        List.getElem?_eq_getElem hb, h1, h2, scalar_lt] <;> omega
  case stop =>
    intro i hi
    have : ¬ i < Slice.len edges := by rw [lt_len]; omega
    simp [decision.edges_respect_layers_loop.body, this]

/-! ## The closure certificate -/

theorem reach_contains_edges_decides (norms : Norms) (edges : Edges) (reach : Reach) (contexts : Usize)
    (hS : Shaped norms edges reach contexts) :
    Decides (decision.reach_contains_edges edges reach)
      (∀ e ∈ edges.val, R reach e.1.val e.2.val = true) := by
  have key := loop_all (body := decision.reach_contains_edges_loop.body edges reach)
    (n := edges.length) (P := fun k => ∀ e ∈ edges.val[k]?, R reach e.1.val e.2.val = true)
    ?step ?stop 0#usize
  · exact (key.congr from_zero).congr forall_getElem?
  case step =>
    intro i hi
    obtain ⟨j, hj, hjv⟩ := succ_ok i edges.length edges.property hi
    refine ⟨j, hjv, ?_⟩
    have hin := hS.edges_in _ (List.getElem_mem hi)
    simp only [decision.reach_contains_edges_loop.body, (lt_len i edges).mpr hi, if_true,
      sidx edges i hi, bind_tc_ok, hj, List.getElem?_eq_getElem hi, Option.mem_def,
      Option.some.injEq, forall_eq']
    generalize edges.val[i.val] = p at hin
    obtain ⟨a, b⟩ := p
    obtain ⟨ha, hb⟩ := hin
    dsimp only at ha hb
    have ha' : a.val < reach.length := by rw [hS.reach_len]; exact ha
    have hb' : b.val < (reach.val[a.val]).length := by rw [hS.rows _ ha']; exact hb
    cases hr : (reach.val[a.val]).val[b.val] <;>
      simp [sidx reach a ha', vidxu _ b hb', R_eq reach ha' hb', hr]
  case stop =>
    intro i hi
    have : ¬ i < Slice.len edges := by rw [lt_len]; omega
    simp [decision.reach_contains_edges_loop.body, this]

/-- A row of the certificate, read inside the square. -/
theorem row_bound (norms : Norms) (edges : Edges) (reach : Reach) (contexts : Usize)
    (hS : Shaped norms edges reach contexts) {i j : Nat} (hi : i < reach.length) (hj : j < reach.length) :
    j < (reach.val[i]).length := by
  rw [hS.rows _ hi, ← hS.reach_len]; exact hj

theorem row_within_decides (norms : Norms) (edges : Edges) (reach : Reach) (contexts : Usize)
    (hS : Shaped norms edges reach contexts) (a b : Usize) (ha : a.val < reach.length)
    (hb : b.val < reach.length) :
    Decides (decision.row_within reach a b)
      (∀ k, k < reach.length → R reach a.val k = true → R reach b.val k = true) := by
  have key := loop_all (body := decision.row_within_loop.body reach a b)
    (n := reach.length) (P := fun k => R reach a.val k = true → R reach b.val k = true)
    ?step ?stop 0#usize
  · exact key.congr (by simp)
  case step =>
    intro i hi
    obtain ⟨j, hj, hjv⟩ := succ_ok i reach.length reach.property hi
    refine ⟨j, hjv, ?_⟩
    have h1 := row_bound norms edges reach contexts hS ha hi
    have h2 := row_bound norms edges reach contexts hS hb hi
    simp only [decision.row_within_loop.body, (lt_len i reach).mpr hi, if_true,
      sidx reach a ha, sidx reach b hb, vidx _ i h1, vidx _ i h2, bind_tc_ok, hj,
      R_eq reach ha h1, R_eq reach hb h2]
    cases (reach.val[a.val]).val[i.val] <;> cases (reach.val[b.val]).val[i.val] <;> simp
  case stop =>
    intro i hi
    have : ¬ i < Slice.len reach := by rw [lt_len]; omega
    simp [decision.row_within_loop.body, this]

theorem row_transitive_decides (norms : Norms) (edges : Edges) (reach : Reach) (contexts : Usize)
    (hS : Shaped norms edges reach contexts) (a : Usize) (ha : a.val < reach.length) :
    Decides (decision.row_transitive reach a)
      (∀ k, k < reach.length → R reach a.val k = true →
        ∀ m, m < reach.length → R reach k m = true → R reach a.val m = true) := by
  have key := loop_all (body := decision.row_transitive_loop.body reach a)
    (n := reach.length) (P := fun k => R reach a.val k = true →
        ∀ m, m < reach.length → R reach k m = true → R reach a.val m = true)
    ?step ?stop 0#usize
  · exact key.congr (by simp)
  case step =>
    intro i hi
    obtain ⟨j, hj, hjv⟩ := succ_ok i reach.length reach.property hi
    refine ⟨j, hjv, ?_⟩
    have h1 := row_bound norms edges reach contexts hS ha hi
    simp only [decision.row_transitive_loop.body, (lt_len i reach).mpr hi, if_true,
      sidx reach a ha, vidx _ i h1, bind_tc_ok, hj, R_eq reach ha h1]
    obtain ⟨b, hrun, hb⟩ := row_within_decides norms edges reach contexts hS i a hi ha
    cases hk : (reach.val[a.val]).val[i.val]
    · simp
    · simp only [hrun, bind_tc_ok, if_true]
      cases b
      · refine Or.inr ⟨?_, by simp⟩
        intro h; apply (show ¬ (false = true) by simp); apply hb.mpr
        intro m hm hr; exact h trivial m hm hr
      · refine Or.inl ⟨?_, by simp⟩
        intro _ m hm hr; exact (hb.mp rfl) m hm hr
  case stop =>
    intro i hi
    have : ¬ i < Slice.len reach := by rw [lt_len]; omega
    simp [decision.row_transitive_loop.body, this]

theorem reach_transitive_decides (norms : Norms) (edges : Edges) (reach : Reach) (contexts : Usize)
    (hS : Shaped norms edges reach contexts) :
    Decides (decision.reach_transitive reach)
      (∀ i k m, i < reach.length → k < reach.length → m < reach.length →
        R reach i k = true → R reach k m = true → R reach i m = true) := by
  have key := loop_all (body := decision.reach_transitive_loop.body reach)
    (n := reach.length) (P := fun i => ∀ k, k < reach.length → R reach i k = true →
        ∀ m, m < reach.length → R reach k m = true → R reach i m = true)
    ?step ?stop 0#usize
  · refine key.congr ⟨fun h i k m hi hk hm h1 h2 => h i (Nat.zero_le _) hi k hk h1 m hm h2,
      fun h i _ hi k hk h1 m hm h2 => h i k m hi hk hm h1 h2⟩
  case step =>
    intro i hi
    obtain ⟨j, hj, hjv⟩ := succ_ok i reach.length reach.property hi
    refine ⟨j, hjv, ?_⟩
    obtain ⟨b, hrun, hb⟩ := row_transitive_decides norms edges reach contexts hS i hi
    simp only [decision.reach_transitive_loop.body, (lt_len i reach).mpr hi, if_true, hrun,
      bind_tc_ok, hj]
    cases b
    · exact Or.inr ⟨fun h => absurd (hb.mpr h) (by simp), by simp⟩
    · exact Or.inl ⟨hb.mp rfl, by simp⟩
  case stop =>
    intro i hi
    have : ¬ i < Slice.len reach := by rw [lt_len]; omega
    simp [decision.reach_transitive_loop.body, this]

theorem justified_decides (norms : Norms) (edges : Edges) (reach : Reach) (contexts : Usize)
    (hS : Shaped norms edges reach contexts) (a b : Usize) (hb : b.val < reach.length) :
    Decides (decision.justified edges reach a b)
      (∃ e ∈ edges.val, e.1.val = a.val ∧ (e.2.val = b.val ∨ R reach e.2.val b.val = true)) := by
  have key := loop_any (body := decision.justified_loop.body edges reach a b)
    (n := edges.length) (P := fun k => ∃ e ∈ edges.val[k]?,
      e.1.val = a.val ∧ (e.2.val = b.val ∨ R reach e.2.val b.val = true)) ?step ?stop 0#usize
  · refine key.congr ?_
    constructor
    · rintro ⟨k, -, hk, e, he, hp⟩
      exact ⟨e, List.mem_of_getElem? he, hp⟩
    · rintro ⟨e, he, hp⟩
      obtain ⟨k, hk⟩ := List.mem_iff_getElem?.mp he
      exact ⟨k, Nat.zero_le _, (List.getElem?_eq_some_iff.mp hk).1, e, hk, hp⟩
  case step =>
    intro i hi
    obtain ⟨j, hj, hjv⟩ := succ_ok i edges.length edges.property hi
    refine ⟨j, hjv, ?_⟩
    have hin := hS.edges_in _ (List.getElem_mem hi)
    simp only [decision.justified_loop.body, (lt_len i edges).mpr hi, if_true,
      sidx edges i hi, bind_tc_ok, hj, List.getElem?_eq_getElem hi, Option.mem_def,
      Option.some.injEq, exists_eq_left']
    generalize edges.val[i.val] = p at hin
    obtain ⟨h, l⟩ := p
    obtain ⟨-, hl⟩ := hin
    dsimp only at hl
    have hl' : l.val < reach.length := by rw [hS.reach_len]; exact hl
    have hlb := row_bound norms edges reach contexts hS hl' hb
    by_cases e1 : h.val = a.val <;> by_cases e2 : l.val = b.val <;>
      cases hr : (reach.val[l.val]).val[b.val] <;>
      simp [sidx reach l hl', vidxu _ b hlb, R_eq reach hl' hlb, hr, e1, e2, usize_eq]
  case stop =>
    intro i hi
    have : ¬ i < Slice.len edges := by rw [lt_len]; omega
    simp [decision.justified_loop.body, this]

/-- Every certified pair is justified by a first declared edge. -/
def Justified (edges : Edges) (reach : Reach) (i j : Nat) : Prop :=
  ∃ e ∈ edges.val, e.1.val = i ∧ (e.2.val = j ∨ R reach e.2.val j = true)

theorem row_justified_decides (norms : Norms) (edges : Edges) (reach : Reach) (contexts : Usize)
    (hS : Shaped norms edges reach contexts) (a : Usize) (ha : a.val < reach.length) :
    Decides (decision.row_justified edges reach a)
      (∀ k, k < reach.length → R reach a.val k = true → Justified edges reach a.val k) := by
  have key := loop_all (body := decision.row_justified_loop.body edges reach a)
    (n := reach.length) (P := fun k => R reach a.val k = true → Justified edges reach a.val k)
    ?step ?stop 0#usize
  · exact key.congr (by simp)
  case step =>
    intro i hi
    obtain ⟨j, hj, hjv⟩ := succ_ok i reach.length reach.property hi
    refine ⟨j, hjv, ?_⟩
    have h1 := row_bound norms edges reach contexts hS ha hi
    simp only [decision.row_justified_loop.body, (lt_len i reach).mpr hi, if_true,
      sidx reach a ha, vidx _ i h1, bind_tc_ok, hj, R_eq reach ha h1]
    obtain ⟨b, hrun, hb⟩ := justified_decides norms edges reach contexts hS a i hi
    cases (reach.val[a.val]).val[i.val]
    · simp
    · simp only [hrun, bind_tc_ok, if_true]
      cases b
      · exact Or.inr ⟨fun h => absurd (hb.mpr (h trivial)) (by simp), by simp⟩
      · exact Or.inl ⟨fun _ => hb.mp rfl, by simp⟩
  case stop =>
    intro i hi
    have : ¬ i < Slice.len reach := by rw [lt_len]; omega
    simp [decision.row_justified_loop.body, this]

theorem reach_justified_decides (norms : Norms) (edges : Edges) (reach : Reach) (contexts : Usize)
    (hS : Shaped norms edges reach contexts) :
    Decides (decision.reach_justified edges reach)
      (∀ i j, i < reach.length → j < reach.length → R reach i j = true → Justified edges reach i j) := by
  have key := loop_all (body := decision.reach_justified_loop.body edges reach)
    (n := reach.length) (P := fun i => ∀ k, k < reach.length → R reach i k = true →
      Justified edges reach i k) ?step ?stop 0#usize
  · refine key.congr ⟨fun h i j hi hj hr => h i (Nat.zero_le _) hi j hj hr,
      fun h i _ hi j hj hr => h i j hi hj hr⟩
  case step =>
    intro i hi
    obtain ⟨j, hj, hjv⟩ := succ_ok i reach.length reach.property hi
    refine ⟨j, hjv, ?_⟩
    obtain ⟨b, hrun, hb⟩ := row_justified_decides norms edges reach contexts hS i hi
    simp only [decision.reach_justified_loop.body, (lt_len i reach).mpr hi, if_true, hrun,
      bind_tc_ok, hj]
    cases b
    · exact Or.inr ⟨fun h => absurd (hb.mpr h) (by simp), by simp⟩
    · exact Or.inl ⟨hb.mp rfl, by simp⟩
  case stop =>
    intro i hi
    have : ¬ i < Slice.len reach := by rw [lt_len]; omega
    simp [decision.reach_justified_loop.body, this]

theorem irreflexive_decides (norms : Norms) (edges : Edges) (reach : Reach) (contexts : Usize)
    (hS : Shaped norms edges reach contexts) :
    Decides (decision.irreflexive reach) (∀ k, k < reach.length → R reach k k = false) := by
  have key := loop_all (body := decision.irreflexive_loop.body reach) (n := reach.length)
    (P := fun k => R reach k k = false) ?step ?stop 0#usize
  · exact key.congr (by simp)
  case step =>
    intro i hi
    obtain ⟨j, hj, hjv⟩ := succ_ok i reach.length reach.property hi
    refine ⟨j, hjv, ?_⟩
    have hb := row_bound norms edges reach contexts hS hi hi
    simp only [decision.irreflexive_loop.body, (lt_len i reach).mpr hi, if_true,
      sidx reach i hi, vidx _ i hb, bind_tc_ok, hj, R_eq reach hi hb]
    cases (reach.val[i.val]).val[i.val] <;> simp
  case stop =>
    intro i hi
    have : ¬ i < Slice.len reach := by rw [lt_len]; omega
    simp [decision.irreflexive_loop.body, this]

/-! ## Clashes -/

theorem co_active_decides (left right : decision.Norm) (contexts : Usize)
    (hl : left.active.length = contexts.val) (hr : right.active.length = contexts.val) :
    Decides (decision.co_active left right contexts)
      (∃ c, c < contexts.val ∧ left.active.val[c]? = some true ∧ right.active.val[c]? = some true) := by
  have key := loop_any (body := decision.co_active_loop.body left right contexts)
    (n := contexts.val)
    (P := fun c => left.active.val[c]? = some true ∧ right.active.val[c]? = some true)
    ?step ?stop 0#usize
  · exact key.congr ⟨fun ⟨c, _, h⟩ => ⟨c, h⟩, fun ⟨c, h⟩ => ⟨c, Nat.zero_le _, h⟩⟩
  case step =>
    intro i hi
    obtain ⟨j, hj, hjv⟩ := succ_ok i contexts.val (by scalar_tac) hi
    refine ⟨j, hjv, ?_⟩
    have h1 : i.val < left.active.length := by rw [hl]; exact hi
    have h2 : i.val < right.active.length := by rw [hr]; exact hi
    have hlt : i < contexts := hi
    cases e1 : left.active.val[i.val] <;> cases e2 : right.active.val[i.val] <;>
      simp [decision.co_active_loop.body, hlt, active_in_eq _ _ h1, active_in_eq _ _ h2, hj,
        List.getElem?_eq_getElem h1, List.getElem?_eq_getElem h2, e1, e2]
  case stop =>
    intro i hi
    have : ¬ i < contexts := by change ¬ i.val < contexts.val; omega
    simp [decision.co_active_loop.body, this]

/-- The two norms both apply, unexcepted, in some context. -/
def CoActiveOf (norms : Norms) (contexts : Usize) (i j : Nat) : Prop :=
  ∃ c, c < contexts.val ∧ actOf norms i c = true ∧ actOf norms j c = true

/-- An opposing pair that meets in some context shares a layer and is ordered
    by the certificate. -/
def ClashResolved (norms : Norms) (reach : Reach) (contexts : Usize) (i j : Nat) : Prop :=
  (modOf norms i).opposed (modOf norms j) → CoActiveOf norms contexts i j →
    layerOf norms i = layerOf norms j ∧ (R reach i j = true ∨ R reach j i = true)

theorem clash_resolved_decides (norms : Norms) (edges : Edges) (reach : Reach) (contexts : Usize)
    (hS : Shaped norms edges reach contexts) (i j : Usize) (hi : i.val < norms.length)
    (hj : j.val < norms.length) :
    Decides (decision.clash_resolved norms reach contexts i j)
      (ClashResolved norms reach contexts i.val j.val) := by
  have hri : i.val < reach.length := by rw [hS.reach_len]; exact hi
  have hrj : j.val < reach.length := by rw [hS.reach_len]; exact hj
  have hij := row_bound norms edges reach contexts hS hri hrj
  have hji := row_bound norms edges reach contexts hS hrj hri
  obtain ⟨b, hrun, hb⟩ := co_active_decides norms.val[i.val] norms.val[j.val] contexts
    (hS.masks _ hi) (hS.masks _ hj)
  have hco : (∃ c, c < contexts.val ∧ (norms.val[i.val]).active.val[c]? = some true ∧
      (norms.val[j.val]).active.val[c]? = some true) ↔ CoActiveOf norms contexts i.val j.val := by
    simp only [CoActiveOf, actOf, List.getElem?_eq_getElem hi, List.getElem?_eq_getElem hj,
      Option.bind_some, getD_true]
  rw [hco] at hb
  have hmod : ∀ k (hk : k < norms.length), modOf norms k = toModality (norms.val[k]).modality := by
    intro k hk; simp [modOf, List.getElem?_eq_getElem hk]
  have hlay : ∀ k (hk : k < norms.length), layerOf norms k = (norms.val[k]).layer.val := by
    intro k hk; simp [layerOf, List.getElem?_eq_getElem hk]
  unfold ClashResolved
  rw [hmod _ hi, hmod _ hj, hlay _ hi, hlay _ hj, R_eq reach hri hij, R_eq reach hrj hji]
  simp only [decision.clash_resolved, sidx norms i hi, sidx norms j hj, opposed_eq, bind_tc_ok,
    hrun, sidx reach i hri, sidx reach j hrj, vidx _ j hij, vidx _ i hji, decide_eq_true_eq]
  by_cases hop : (toModality (norms.val[i.val]).modality).opposed (toModality (norms.val[j.val]).modality)
  · cases b
    · have : ¬ CoActiveOf norms contexts i.val j.val := fun h => by simpa using hb.mpr h
      simp [hop, this]
    · have hc := hb.mp rfl
      by_cases hl : (norms.val[i.val]).layer = (norms.val[j.val]).layer
      · have hl' : (norms.val[i.val]).layer.val = (norms.val[j.val]).layer.val := by rw [hl]
        cases e1 : (reach.val[i.val]).val[j.val] <;> cases e2 : (reach.val[j.val]).val[i.val] <;>
          simp [hop, hc, hl, hl', e1, e2]
      · have hl' : ¬ (norms.val[i.val]).layer.val = (norms.val[j.val]).layer.val := fun h =>
          hl ((UScalar.eq_equiv _ _).mpr h)
        simp [hop, hc, hl, hl']
  · simp [hop]

theorem clashes_resolved_from_decides (norms : Norms) (edges : Edges) (reach : Reach) (contexts : Usize)
    (hS : Shaped norms edges reach contexts) (i : Usize) (hi : i.val < norms.length) :
    Decides (decision.clashes_resolved_from norms reach contexts i)
      (∀ j, i.val + 1 ≤ j → j < norms.length → ClashResolved norms reach contexts i.val j) := by
  obtain ⟨j0, hj0, hj0v⟩ := succ_ok i norms.length norms.property hi
  have key := loop_all (body := decision.clashes_resolved_from_loop.body norms reach contexts i)
    (n := norms.length) (P := fun j => ClashResolved norms reach contexts i.val j) ?step ?stop j0
  · unfold decision.clashes_resolved_from
    simp only [hj0, bind_tc_ok]
    exact key.congr (by rw [hj0v])
  case step =>
    intro k hk
    obtain ⟨j, hj, hjv⟩ := succ_ok k norms.length norms.property hk
    refine ⟨j, hjv, ?_⟩
    obtain ⟨b, hrun, hb⟩ := clash_resolved_decides norms edges reach contexts hS i k hi hk
    simp only [decision.clashes_resolved_from_loop.body, (lt_len k norms).mpr hk, if_true, hrun,
      bind_tc_ok, hj]
    cases b
    · exact Or.inr ⟨fun h => absurd (hb.mpr h) (by simp), by simp⟩
    · exact Or.inl ⟨hb.mp rfl, by simp⟩
  case stop =>
    intro k hk
    have : ¬ k < Slice.len norms := by rw [lt_len]; omega
    simp [decision.clashes_resolved_from_loop.body, this]

theorem clashes_resolved_decides (norms : Norms) (edges : Edges) (reach : Reach) (contexts : Usize)
    (hS : Shaped norms edges reach contexts) :
    Decides (decision.clashes_resolved norms reach contexts)
      (∀ i j, i < j → j < norms.length → ClashResolved norms reach contexts i j) := by
  have key := loop_all (body := decision.clashes_resolved_loop.body norms reach contexts)
    (n := norms.length)
    (P := fun i => ∀ j, i + 1 ≤ j → j < norms.length → ClashResolved norms reach contexts i j)
    ?step ?stop 0#usize
  · refine key.congr ⟨fun h i j hij hj => h i (Nat.zero_le _) (by omega) j hij hj,
      fun h i _ _ j hij hj => h i j hij hj⟩
  case step =>
    intro i hi
    obtain ⟨j, hj, hjv⟩ := succ_ok i norms.length norms.property hi
    refine ⟨j, hjv, ?_⟩
    obtain ⟨b, hrun, hb⟩ := clashes_resolved_from_decides norms edges reach contexts hS i hi
    simp only [decision.clashes_resolved_loop.body, (lt_len i norms).mpr hi, if_true, hrun,
      bind_tc_ok, hj]
    cases b
    · exact Or.inr ⟨fun h => absurd (hb.mpr h) (by simp), by simp⟩
    · exact Or.inl ⟨hb.mp rfl, by simp⟩
  case stop =>
    intro i hi
    have : ¬ i < Slice.len norms := by rw [lt_len]; omega
    simp [decision.clashes_resolved_loop.body, this]

/-! ## The whole decision -/

/-- Everything `admits` checks, as written conditions. -/
structure Checks (norms : Norms) (edges : Edges) (reach : Reach) (contexts : Usize) : Prop where
  shaped : Shaped norms edges reach contexts
  layers : ∀ e ∈ edges.val, layerOf norms e.1.val ≤ layerOf norms e.2.val ∧
    protOf norms e.2.val = false
  contains : ∀ e ∈ edges.val, R reach e.1.val e.2.val = true
  transitive : ∀ i k m, i < reach.length → k < reach.length → m < reach.length →
    R reach i k = true → R reach k m = true → R reach i m = true
  justified : ∀ i j, i < reach.length → j < reach.length → R reach i j = true →
    Justified edges reach i j
  irreflexive : ∀ k, k < reach.length → R reach k k = false
  clashes : ∀ i j, i < j → j < norms.length → ClashResolved norms reach contexts i j

/-- The extracted `admits` returns, for every input, `true` exactly when every
    written check holds. -/
theorem admits_decides (norms : Norms) (edges : Edges) (reach : Reach) (contexts : Usize) :
    Decides (decision.admits norms edges reach contexts) (Checks norms edges reach contexts) := by
  unfold decision.admits
  refine (Decides.and (well_shaped_decides norms edges reach contexts) fun hS =>
    Decides.and (edges_respect_layers_decides norms edges reach contexts hS) fun _ =>
    Decides.and (reach_contains_edges_decides norms edges reach contexts hS) fun _ =>
    Decides.and (reach_transitive_decides norms edges reach contexts hS) fun _ =>
    Decides.and (reach_justified_decides norms edges reach contexts hS) fun _ =>
    Decides.and (irreflexive_decides norms edges reach contexts hS) fun _ =>
    clashes_resolved_decides norms edges reach contexts hS).congr ?_
  exact ⟨fun ⟨a, b, c, d, e, f, g⟩ => ⟨a, b, c, d, e, f, g⟩,
    fun ⟨a, b, c, d, e, f, g⟩ => ⟨a, b, c, d, e, f, g⟩⟩

end Norn.Checker.NormPrecedence
