import NornSem.Norm

/-!
# Meta-theorems of declared norm precedence (U4, #718)

Over any admissible catalog:

* **Downstream can't defeat upstream.** Declared priority never points up a
  layer and never reaches a protected norm (`downstream_cannot_defeat_upstream`).
  Every defeat that happens in a context is between norms of the same layer
  (`defeat_same_layer`).
* **Strict-layer preservation.** An active protected norm always survives
  (`protected_survives`). A surviving permission never coexists with an active
  protected prohibition or an active prohibition of a higher layer
  (`permission_respects_invariants`): permitted actions never violate an
  invariant.
* **Overlays preserve upstream survivors.** Restricting the catalog to the
  layers at or above `L` (`Catalog.upTo`) does not change which of those norms
  survive (`overlay_preserves_upstream`): adding lower-layer norms and their
  edges never removes or adds an upstream survivor.
-/

namespace Norn.Sem.Norm

variable {C : Catalog}

/-- **Downstream can't defeat upstream.** A declared priority points to the
    same or a lower layer, never into a protected norm. -/
theorem downstream_cannot_defeat_upstream (hA : Admissible C) {w l : Nat} (h : Prec C w l) :
    C.layer w ≤ C.layer l ∧ C.isProtected l = false :=
  ⟨Prec.layer_le hA h, Prec.unprotected hA h⟩

/-- Every defeat between two norms active in the same context stays inside one
    layer: no defeat crosses layers in either direction. -/
theorem defeat_same_layer (hA : Admissible C) {c w l : Nat} (hc : c < C.contexts)
    (hw : C.active w c = true) (hl : C.active l c = true) (hd : Defeats C w l) :
    C.layer w = C.layer l :=
  (hA.clashes w l (Prec.in_range hA hd.1).1 (Prec.in_range hA hd.1).2 hd.2 ⟨c, hc, hw, hl⟩).1

/-- An active protected norm survives: nothing has a declared priority over it. -/
theorem protected_survives (hA : Admissible C) {c : Nat} {S : Nat → Prop} (hS : Stable C c S)
    {p : Nat} (hp : p < C.size) (ha : C.active p c = true) (hprot : C.isProtected p = true) :
    S p := by
  refine (hS p).mpr ⟨hp, ha, ?_⟩
  rintro ⟨j, -, hd⟩
  rw [Prec.unprotected hA hd.1] at hprot
  exact Bool.false_ne_true hprot

/-- **Strict-layer preservation.** If a permission survives in a context, no
    protected prohibition and no prohibition of a strictly higher layer is
    active there. Permission is only ever a surviving permission (`Permits`),
    so a permitted action never violates an invariant. -/
theorem permission_respects_invariants (hA : Admissible C) {c : Nat} (hc : c < C.contexts)
    {S : Nat → Prop} (hS : Stable C c S) {q : Nat} (hq : S q)
    (hperm : C.modality q = .permission) {p : Nat} (hp : p < C.size)
    (ha : C.active p c = true) (hproh : C.modality p = .prohibition) :
    C.isProtected p = false ∧ C.layer q ≤ C.layer p := by
  have hop : (C.modality p).opposed (C.modality q) := by
    simp [Modality.opposed, hperm, hproh]
  obtain ⟨hqn, hqa, -⟩ := (hS q).mp hq
  refine ⟨?_, ?_⟩
  · cases hprot : C.isProtected p
    · rfl
    · exact absurd hop (stable_no_conflict hA hc hS (protected_survives hA hS hp ha hprot) hq)
  · exact le_of_eq (hA.clashes p q hp hqn hop ⟨c, hc, ha, hqa⟩).1.symm

/-- The catalog cut to the layers at or above `L`: norms below are inactive
    and their edges are dropped. -/
def Catalog.upTo (C : Catalog) (L : Nat) : Catalog :=
  { C with
    active := fun i c => C.active i c && decide (C.layer i ≤ L)
    edges := C.edges.filter (fun e => decide (C.layer e.1 ≤ L) && decide (C.layer e.2 ≤ L)) }

/-- Below an upstream norm, every declared path stays upstream. -/
theorem prec_upTo_iff (hA : Admissible C) {L j i : Nat} (hi : C.layer i ≤ L) :
    Prec (C.upTo L) j i ↔ Prec C j i := by
  constructor
  · intro h
    clear hi
    induction h with
    | single he =>
      exact Relation.TransGen.single (List.mem_of_mem_filter he)
    | tail _ he ih =>
      exact Relation.TransGen.tail ih (List.mem_of_mem_filter he)
  · intro h
    induction h using Relation.TransGen.head_induction_on with
    | single he =>
      refine Relation.TransGen.single (List.mem_filter.mpr ⟨he, ?_⟩)
      simp [le_trans (hA.edges_down _ _ he) hi, hi]
    | head he hbi ih =>
      refine Relation.TransGen.head (List.mem_filter.mpr ⟨he, ?_⟩) ih
      have hb := le_trans (Prec.layer_le hA hbi) hi
      simp [le_trans (hA.edges_down _ _ he) hb, hb]

/-- **Overlays preserve upstream survivors.** On an admissible catalog, the
    survivors among the norms at or above layer `L` are exactly the survivors
    of the catalog cut to those layers. -/
theorem overlay_preserves_upstream (hA : Admissible C) {L c : Nat} {S S' : Nat → Prop}
    (hS : Stable C c S) (hS' : Stable (C.upTo L) c S') :
    ∀ i, C.layer i ≤ L → (S i ↔ S' i) := by
  intro i
  induction i using (prec_wf hA).induction with
  | _ i ih =>
    intro hi
    rw [hS i, hS' i]
    have hact : (C.upTo L).active i c = C.active i c := by simp [Catalog.upTo, hi]
    have hdef : ∀ j, Defeats (C.upTo L) j i ↔ Defeats C j i := fun j => by
      unfold Defeats; rw [prec_upTo_iff hA hi]; rfl
    have hex : (∃ j, S j ∧ Defeats C j i) ↔ (∃ j, S' j ∧ Defeats (C.upTo L) j i) := by
      constructor
      · rintro ⟨j, hj, hd⟩
        exact ⟨j, (ih j hd.1 (le_trans (Prec.layer_le hA hd.1) hi)).mp hj, (hdef j).mpr hd⟩
      · rintro ⟨j, hj, hd⟩
        have hd' := (hdef j).mp hd
        exact ⟨j, (ih j hd'.1 (le_trans (Prec.layer_le hA hd'.1) hi)).mpr hj, hd'⟩
    rw [hact, hex]
    rfl

end Norn.Sem.Norm
