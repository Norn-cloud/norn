import Checker.NormPrecedence.Admits

/-!
# An admitted order meets the written semantics

If the extracted `admits` returns `true`, the catalog its inputs denote is
`Admissible` in `Norn.Sem.Norm`, and the certificate it checked is exactly the
declared-priority closure `Prec`.
-/

namespace Norn.Checker.NormPrecedence
open Aeneas Aeneas.Std Result norn_checker_norm_decision Norn.Sem.Norm

variable {norms : Norms} {edges : Edges} {reach : Reach} {contexts : Usize}

theorem edge_iff {h l : Nat} :
    Edge (catalog norms edges contexts) h l ↔ ∃ e ∈ edges.val, e.1.val = h ∧ e.2.val = l := by
  simp [Edge, catalog, List.mem_map, Prod.ext_iff]

theorem R_bounds {i j : Nat} (h : R reach i j = true) :
    ∃ hi : i < reach.length, j < (reach.val[i]).length := by
  unfold R at h
  by_cases hi : i < reach.length
  · refine ⟨hi, ?_⟩
    rw [List.getElem?_eq_getElem hi] at h
    by_contra hj
    rw [Option.bind_some, List.getElem?_eq_none (Nat.le_of_not_lt hj)] at h
    simp at h
  · rw [List.getElem?_eq_none (Nat.le_of_not_lt hi)] at h; simp at h

theorem R_lt (hC : Checks norms edges reach contexts) {i j : Nat} (h : R reach i j = true) :
    i < reach.length ∧ j < reach.length := by
  obtain ⟨hi, hj⟩ := R_bounds h
  refine ⟨hi, ?_⟩
  rw [hC.shaped.rows _ hi, ← hC.shaped.reach_len] at hj; exact hj

theorem edge_lt (hC : Checks norms edges reach contexts) {e : Usize × Usize} (he : e ∈ edges.val) :
    e.1.val < reach.length ∧ e.2.val < reach.length := by
  rw [hC.shaped.reach_len]; exact hC.shaped.edges_in e he

/-- Every declared priority is certified. -/
theorem prec_R (hC : Checks norms edges reach contexts) {i j : Nat}
    (h : Prec (catalog norms edges contexts) i j) : R reach i j = true := by
  induction h with
  | single he =>
    obtain ⟨e, hmem, rfl, rfl⟩ := edge_iff.mp he
    exact hC.contains e hmem
  | @tail k m _ he ih =>
    obtain ⟨e, hmem, rfl, rfl⟩ := edge_iff.mp he
    have hkm := hC.contains e hmem
    exact hC.transitive _ _ _ (R_lt hC ih).1 (edge_lt hC hmem).1 (edge_lt hC hmem).2 ih hkm

open Classical in
/-- The norms certified as reachable from `i`. -/
noncomputable def succs (reach : Reach) (i : Nat) : Finset Nat :=
  (Finset.range reach.length).filter (fun m => R reach i m = true)

/-- Every certified pair is a declared priority. -/
theorem R_prec (hC : Checks norms edges reach contexts) :
    ∀ n i j, (succs reach i).card = n → R reach i j = true →
      Prec (catalog norms edges contexts) i j := by
  intro n
  induction n using Nat.strong_induction_on with
  | _ n ih =>
    intro i j hn hij
    obtain ⟨hi, hj⟩ := R_lt hC hij
    obtain ⟨e, hmem, he1, he2⟩ := hC.justified i j hi hj hij
    have hedge : Edge (catalog norms edges contexts) i e.2.val := edge_iff.mpr ⟨e, hmem, he1, rfl⟩
    rcases he2 with he2 | hkj
    · rw [← he2]; exact Relation.TransGen.single hedge
    · have hik : R reach i e.2.val = true := by rw [← he1]; exact hC.contains e hmem
      have hk := (edge_lt hC hmem).2
      have hlt : (succs reach e.2.val).card < (succs reach i).card := by
        apply Finset.card_lt_card
        rw [Finset.ssubset_iff_of_subset]
        · refine ⟨e.2.val, ?_, ?_⟩
          · simp [succs, hk, hik]
          · simp [succs, hC.irreflexive _ hk]
        · intro m hm
          simp only [succs, Finset.mem_filter, Finset.mem_range] at hm ⊢
          exact ⟨hm.1, hC.transitive _ _ _ hi hk hm.1 hik hm.2⟩
      exact Relation.TransGen.head hedge (ih _ (hn ▸ hlt) _ _ rfl hkj)

/-- The checked certificate is exactly the declared-priority closure. -/
theorem R_iff_prec (hC : Checks norms edges reach contexts) (i j : Nat) :
    R reach i j = true ↔ Prec (catalog norms edges contexts) i j :=
  ⟨R_prec hC _ i j rfl, prec_R hC⟩

theorem opposed_ne {C : Catalog} {i j : Nat} (h : (C.modality i).opposed (C.modality j)) : i ≠ j := by
  rintro rfl; exact Modality.opposed_irrefl _ h

/-- Every check `admits` makes yields the written admissibility. -/
theorem checks_admissible (hC : Checks norms edges reach contexts) :
    Admissible (catalog norms edges contexts) where
  edges_in h l he := by
    obtain ⟨e, hmem, rfl, rfl⟩ := edge_iff.mp he; exact hC.shaped.edges_in e hmem
  edges_down h l he := by
    obtain ⟨e, hmem, rfl, rfl⟩ := edge_iff.mp he; exact (hC.layers e hmem).1
  edges_unprotected h l he := by
    obtain ⟨e, hmem, rfl, rfl⟩ := edge_iff.mp he; exact (hC.layers e hmem).2
  acyclic i h := by
    have hr := prec_R hC h
    rw [hC.irreflexive i (R_lt hC hr).1] at hr; exact Bool.false_ne_true hr
  clashes i j hi hj hop hco := by
    have hne := opposed_ne hop
    rcases Nat.lt_or_gt_of_ne hne with hlt | hlt
    · obtain ⟨hl, hr⟩ := hC.clashes i j hlt hj hop hco
      exact ⟨hl, hr.imp (R_prec hC _ i j rfl) (R_prec hC _ j i rfl)⟩
    · obtain ⟨c, hc, h1, h2⟩ := hco
      obtain ⟨hl, hr⟩ := hC.clashes j i hlt hi (Modality.opposed_symm hop) ⟨c, hc, h2, h1⟩
      exact ⟨hl.symm, hr.symm.imp (R_prec hC _ i j rfl) (R_prec hC _ j i rfl)⟩

/-- **Decision soundness (U3).** For all inputs, the extracted production
    `admits` returns without panic or divergence. When it returns `true`, the
    declared order the inputs denote is admissible, and the certificate it was
    given is exactly the declared-priority closure. -/
theorem admits_sound (norms : Norms) (edges : Edges) (reach : Reach) (contexts : Usize) :
    ∃ b, decision.admits norms edges reach contexts = ok b ∧
      (b = true → Admissible (catalog norms edges contexts) ∧
        ∀ i j, R reach i j = true ↔ Prec (catalog norms edges contexts) i j) := by
  obtain ⟨b, hrun, hb⟩ := admits_decides norms edges reach contexts
  exact ⟨b, hrun, fun h => ⟨checks_admissible (hb.mp h), R_iff_prec (hb.mp h)⟩⟩

end Norn.Checker.NormPrecedence
