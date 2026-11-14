/-!
# Finite write-back lenses: written semantics

The admitted lens of `crates/norn-checker/src/mapping/lens.rs` is a pair of
tables over a declared, finite list of source states: a Get table (the view of
each row) and a put table (for each row and view, the destination row). A
source state is looked up at the first row that holds it.

This file states that semantics and the lens laws, and proves the lens
composition meta-theorem (#718). It never imports the extraction.
-/

namespace Norn.Checker.Certificates.Lens

variable {S V W : Type}

/-- `o` is the first row of `l`, from `start` on, that holds `s`. -/
def FirstFrom (l : List S) (s : S) (start : Nat) : Option Nat → Prop
  | some r => start ≤ r ∧ l[r]? = some s ∧ ∀ j, start ≤ j → j < r → l[j]? ≠ some s
  | none => ∀ j, start ≤ j → l[j]? ≠ some s

/-- The declared view of `s`: the Get entry of the first row holding it. -/
def TableGet (sources : List S) (gets : List Bool) (s : S) (out : Option Bool) : Prop :=
  ∃ o, FirstFrom sources s 0 o ∧ out = o.bind (fun r => gets[r]?)

/-- The declared put-back of view `v` into `s`: the source at the destination
    row named by the first row holding `s`. -/
def TablePut (sources : List S) (puts : List (Nat × Nat)) (s : S) (v : Bool)
    (out : Option S) : Prop :=
  ∃ o, FirstFrom sources s 0 o ∧
    out = (o.bind fun r => puts[r]?).bind (fun p => sources[if v then p.2 else p.1]?)

/-- A total lens. -/
structure Lens (S V : Type) where
  get : S → V
  put : S → V → S

/-- GetPut, PutGet and PutPut. -/
def Lens.WellBehaved (l : Lens S V) : Prop :=
  (∀ s, l.put s (l.get s) = s) ∧
  (∀ s v, l.get (l.put s v) = v) ∧
  (∀ s v w, l.put (l.put s v) w = l.put s w)

/-- Sequential composition: view through `l`, then through `m`. -/
def Lens.comp (l : Lens S V) (m : Lens V W) : Lens S W where
  get s := m.get (l.get s)
  put s w := l.put s (m.put (l.get s) w)

/-- Lens composition (#718 meta-theorem 7): composing well-behaved lenses
    yields a well-behaved lens. -/
theorem Lens.comp_wellBehaved {l : Lens S V} {m : Lens V W}
    (hl : l.WellBehaved) (hm : m.WellBehaved) : (l.comp m).WellBehaved := by
  obtain ⟨lgp, lpg, lpp⟩ := hl
  obtain ⟨mgp, mpg, mpp⟩ := hm
  refine ⟨fun s => ?_, fun s w => ?_, fun s v w => ?_⟩
  · simp only [Lens.comp, mgp, lgp]
  · simp only [Lens.comp, lpg, mpg]
  · simp only [Lens.comp, lpg, lpp, mpp]

/-- The lens laws on a finite domain, for partial Get and put: every domain
    state has a view, puts back to itself, and every put-back stays in the
    domain, reads back its view, and is overwritten by a later put. -/
def LawsOn (dom : List S) (get : S → Option Bool) (put : S → Bool → Option S) : Prop :=
  ∀ s ∈ dom, ∃ g, get s = some g ∧ put s g = some s ∧
    ∀ v, ∃ u, put s v = some u ∧ u ∈ dom ∧ get u = some v ∧
      ∀ w, ∃ t, put u w = some t ∧ put s w = some t

namespace LawsOn

variable {dom : List S} {get : S → Option Bool} {put : S → Bool → Option S}

/-- The total lens on the domain that an admitted table denotes. -/
noncomputable def toLens (h : LawsOn dom get put) : Lens {s // s ∈ dom} Bool where
  get s := Classical.choose (h s.1 s.2)
  put s v := ⟨Classical.choose ((Classical.choose_spec (h s.1 s.2)).2.2 v),
    (Classical.choose_spec ((Classical.choose_spec (h s.1 s.2)).2.2 v)).2.1⟩

theorem toLens_get (h : LawsOn dom get put) (s : {s // s ∈ dom}) :
    get s.1 = some (h.toLens.get s) :=
  (Classical.choose_spec (h s.1 s.2)).1

theorem toLens_put (h : LawsOn dom get put) (s : {s // s ∈ dom}) (v : Bool) :
    put s.1 v = some (h.toLens.put s v).1 :=
  (Classical.choose_spec ((Classical.choose_spec (h s.1 s.2)).2.2 v)).1

/-- An admitted finite lens is a well-behaved total lens on its domain, so it
    composes with any other well-behaved lens. -/
theorem toLens_wellBehaved (h : LawsOn dom get put) : h.toLens.WellBehaved := by
  refine ⟨fun s => ?_, fun s v => ?_, fun s v w => ?_⟩
  · obtain ⟨g, hg, hgs, -⟩ := h s.1 s.2
    have hg' := toLens_get h s
    rw [hg] at hg'
    cases hg'
    apply Subtype.ext
    rw [toLens_put h s] at hgs
    exact Option.some.inj hgs
  · obtain ⟨_, _, _, hv⟩ := h s.1 s.2
    obtain ⟨u, hu, -, hgu, -⟩ := hv v
    rw [toLens_put h s v] at hu
    cases hu
    have := toLens_get h (h.toLens.put s v)
    rw [hgu] at this
    exact (Option.some.inj this).symm
  · obtain ⟨_, _, _, hv⟩ := h s.1 s.2
    obtain ⟨u, hu, -, -, hw⟩ := hv v
    obtain ⟨t, hut, hst⟩ := hw w
    rw [toLens_put h s v] at hu
    cases hu
    apply Subtype.ext
    rw [toLens_put h _ w] at hut
    rw [toLens_put h s w] at hst
    rw [Option.some.inj hut, Option.some.inj hst]

end LawsOn

end Norn.Checker.Certificates.Lens

#print axioms Norn.Checker.Certificates.Lens.Lens.comp_wellBehaved
#print axioms Norn.Checker.Certificates.Lens.LawsOn.toLens_wellBehaved
