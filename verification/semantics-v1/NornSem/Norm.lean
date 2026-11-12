import Mathlib.Data.Finset.Card
import Mathlib.Logic.Relation

/-!
# `Norn.Sem.Norm`: declared norm precedence

The written semantics of norm precedence (D1, LAW-RETAIN-1, OVERLAY-MONOTONE-1),
which lane C-C of #718 proves the checker's decision core against.

A `Catalog` is a finite set of grounded norms over a finite set of contexts.
Each norm has a grade, a constitution layer (a smaller number is a higher
layer), a protection flag and an activity mask: whether it applies, unexcepted,
in each context. Priorities are only the declared edges `(winner, loser)` and
their transitive closure `Prec`. Authority levels, enactment order and
specificity never supply one.

* `Admissible` is what admission requires: edges join norms, never point up a
  layer or into a protected norm, the closure is acyclic, and opposing norms
  that meet in a context share a layer and are ordered by the closure.
* `Stable C c S` says `S` is the set of survivors in context `c`: exactly the
  active norms no survivor defeats. On an admissible catalog it is unique
  (`stable_unique`) and has no two opposing members (`stable_no_conflict`).
* Permission is only ever a surviving permission (`Permits`). An uncovered
  context, where nothing survives, permits nothing (`uncovered_not_permits`).
-/

namespace Norn.Sem.Norm

/-- A normative grade. -/
inductive Modality where
  | obligation
  | permission
  | prohibition
  deriving DecidableEq, Repr

/-- A prohibition opposes an obligation or a permission; nothing else opposes. -/
def Modality.opposed (a b : Modality) : Prop :=
  ¬ (a = .prohibition ↔ b = .prohibition)

instance (a b : Modality) : Decidable (a.opposed b) := by
  unfold Modality.opposed; infer_instance

theorem Modality.opposed_irrefl (a : Modality) : ¬ a.opposed a := by
  simp [opposed]

theorem Modality.opposed_symm {a b : Modality} : a.opposed b → b.opposed a := by
  unfold opposed; intro h h'; exact h h'.symm

/-- A finite grounded catalog. Norms are `0, …, size - 1`; contexts are
    `0, …, contexts - 1`. -/
structure Catalog where
  size : Nat
  contexts : Nat
  modality : Nat → Modality
  layer : Nat → Nat
  isProtected : Nat → Bool
  active : Nat → Nat → Bool
  edges : List (Nat × Nat)

variable (C : Catalog)

/-- A declared priority edge. -/
def Edge (h l : Nat) : Prop := (h, l) ∈ C.edges

/-- Declared priority: the transitive closure of the declared edges. -/
def Prec : Nat → Nat → Prop := Relation.TransGen (Edge C)

/-- `w` defeats `l`: a declared priority between opposing norms. -/
def Defeats (w l : Nat) : Prop := Prec C w l ∧ (C.modality w).opposed (C.modality l)

/-- Two norms both apply, unexcepted, in some context. -/
def CoActive (i j : Nat) : Prop := ∃ c < C.contexts, C.active i c ∧ C.active j c

/-- What admission requires of a declared order. -/
structure Admissible : Prop where
  edges_in : ∀ h l, Edge C h l → h < C.size ∧ l < C.size
  edges_down : ∀ h l, Edge C h l → C.layer h ≤ C.layer l
  edges_unprotected : ∀ h l, Edge C h l → C.isProtected l = false
  acyclic : ∀ i, ¬ Prec C i i
  clashes : ∀ i j, i < C.size → j < C.size → (C.modality i).opposed (C.modality j) →
    CoActive C i j → C.layer i = C.layer j ∧ (Prec C i j ∨ Prec C j i)

/-- `S` is the set of survivors in context `c`: the active norms that no
    survivor defeats. -/
def Stable (c : Nat) (S : Nat → Prop) : Prop :=
  ∀ i, S i ↔ i < C.size ∧ C.active i c ∧ ¬ ∃ j, S j ∧ Defeats C j i

/-- The only way a context permits: a permission survives. -/
def Permits (S : Nat → Prop) : Prop := ∃ i, S i ∧ C.modality i = .permission

/-- Nothing survives: the context is uncovered. -/
def Uncovered (S : Nat → Prop) : Prop := ∀ i, ¬ S i

variable {C}

theorem uncovered_not_permits {S : Nat → Prop} (h : Uncovered S) : ¬ Permits C S := by
  rintro ⟨i, hi, -⟩; exact h i hi

theorem Prec.in_range (hA : Admissible C) {i j : Nat} (h : Prec C i j) :
    i < C.size ∧ j < C.size := by
  induction h with
  | single he => exact hA.edges_in _ _ he
  | tail _ he ih => exact ⟨ih.1, (hA.edges_in _ _ he).2⟩

theorem Prec.trans' {i j k : Nat} (h₁ : Prec C i j) (h₂ : Prec C j k) : Prec C i k :=
  Relation.TransGen.trans h₁ h₂

/-- Declared priority never points up a layer. -/
theorem Prec.layer_le (hA : Admissible C) {i j : Nat} (h : Prec C i j) :
    C.layer i ≤ C.layer j := by
  induction h with
  | single he => exact hA.edges_down _ _ he
  | tail _ he ih => exact le_trans ih (hA.edges_down _ _ he)

/-- Declared priority never reaches a protected norm. -/
theorem Prec.unprotected (hA : Admissible C) {i j : Nat} (h : Prec C i j) :
    C.isProtected j = false := by
  cases h with
  | single he => exact hA.edges_unprotected _ _ he
  | tail _ he => exact hA.edges_unprotected _ _ he

open Classical in
/-- The number of norms with a declared priority over `i`. -/
noncomputable def predecessors (C : Catalog) (i : Nat) : Nat :=
  ((Finset.range C.size).filter (fun k => Prec C k i)).card

theorem predecessors_lt (hA : Admissible C) {j i : Nat} (h : Prec C j i) :
    predecessors C j < predecessors C i := by
  classical
  unfold predecessors
  apply Finset.card_lt_card
  rw [Finset.ssubset_iff_of_subset]
  · refine ⟨j, ?_, ?_⟩
    · simp [(Prec.in_range hA h).1, h]
    · simp [hA.acyclic j]
  · intro k hk
    simp only [Finset.mem_filter, Finset.mem_range] at hk ⊢
    exact ⟨hk.1, hk.2.trans' h⟩

/-- On an admissible catalog, declared priority is well-founded. -/
theorem prec_wf (hA : Admissible C) : WellFounded (fun j i => Prec C j i) :=
  Subrelation.wf (fun h => predecessors_lt hA h) (measure (predecessors C)).wf

/-- On an admissible catalog the survivors of a context are unique. -/
theorem stable_unique (hA : Admissible C) {c : Nat} {S S' : Nat → Prop}
    (hS : Stable C c S) (hS' : Stable C c S') : ∀ i, S i ↔ S' i := by
  intro i
  induction i using (prec_wf hA).induction with
  | _ i ih =>
    rw [hS i, hS' i]
    have : (∃ j, S j ∧ Defeats C j i) ↔ (∃ j, S' j ∧ Defeats C j i) := by
      constructor
      · rintro ⟨j, hj, hd⟩; exact ⟨j, (ih j hd.1).mp hj, hd⟩
      · rintro ⟨j, hj, hd⟩; exact ⟨j, (ih j hd.1).mpr hj, hd⟩
    rw [this]

/-- On an admissible catalog no two survivors oppose: every clash is resolved. -/
theorem stable_no_conflict (hA : Admissible C) {c : Nat} (hc : c < C.contexts)
    {S : Nat → Prop} (hS : Stable C c S) {i j : Nat} (hi : S i) (hj : S j) :
    ¬ (C.modality i).opposed (C.modality j) := by
  intro hop
  obtain ⟨hin, hia, hiu⟩ := (hS i).mp hi
  obtain ⟨hjn, hja, hju⟩ := (hS j).mp hj
  obtain ⟨-, hij | hji⟩ := hA.clashes i j hin hjn hop ⟨c, hc, hia, hja⟩
  · exact hju ⟨i, hi, hij, hop⟩
  · exact hiu ⟨j, hj, hji, Modality.opposed_symm hop⟩

/-- A survivor is active and no survivor defeats it. -/
theorem Stable.survivor {c : Nat} {S : Nat → Prop} (hS : Stable C c S) {i : Nat} (hi : S i) :
    i < C.size ∧ C.active i c := by
  obtain ⟨h1, h2, -⟩ := (hS i).mp hi; exact ⟨h1, h2⟩

end Norn.Sem.Norm
