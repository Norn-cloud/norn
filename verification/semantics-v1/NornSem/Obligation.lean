import NornSem.Law

/-!
# Obligations, evidence and checker soundness

An obligation family is an IR fragment, the semantic property each obligation
states, and the checker's decision for it. The checker's decision is one of the
evidence labels of #718. Only `proved` and `exhaustiveFinite` admit:
bounded-no-counterexample, witnessed, refuted and unknown never do.

A family is **sound** when, for every input, an admitting decision implies that
the property holds in the IR semantics. That is P6's statement, and each
family's proof instantiates `Family.Sound`. `admitAll_sound` composes the
families: a challenge whose obligations all admit has every property.

The first instance is `exceptionTrees`, for the exception-tree obligation of a
law. Admission must refuse every tree the runtime could not evaluate
unambiguously (#1190). Its property `Tree.Unambiguous` says that at every point
the tree has exactly one stable labelling, and that the bounded fold decides
lifting under it. `exceptionTrees_sound` reduces the family's soundness to the
checker deciding `WellFormed`. U1 discharges that for `law_exceptions::check_tree`.
-/

namespace Norn.Sem.Law.Tree

variable {Id σ : Type} [DecidableEq Id]

/-- The runtime can evaluate the tree unambiguously: at every point it has
    exactly one stable labelling, and the fold with fuel `|nodes| + 1` decides
    lifting under it. -/
def Unambiguous (T : Tree Id σ) : Prop :=
  ∀ x : σ, (∃ A, T.Stable x A) ∧
    (∀ A B, T.Stable x A → T.Stable x B → ∀ e ∈ T.nodes, (A e.id ↔ B e.id)) ∧
    (∀ A, T.Stable x A → (T.LiftedBy A ↔ T.lifts x (T.nodes.length + 1) T.root = true))

/-- A well-formed tree is unambiguous. -/
theorem WellFormed.unambiguous {T : Tree Id σ} (wf : T.WellFormed) : T.Unambiguous :=
  fun x => ⟨stable_exists wf x, fun _ _ hA hB => stable_unique wf hA hB,
    fun _ hA => lifted_iff wf hA⟩

end Norn.Sem.Law.Tree

namespace Norn.Sem.Obligation

/-- The evidence vocabulary of #718. -/
inductive Evidence where
  | proved
  | exhaustiveFinite
  | boundedNoCounterexample
  | witnessed
  | refuted
  | unknown
  deriving DecidableEq, Repr

/-- Only proof and exhaustive finite checking admit. -/
def Evidence.admits : Evidence → Bool
  | .proved => true
  | .exhaustiveFinite => true
  | _ => false

theorem Evidence.admits_iff (e : Evidence) :
    e.admits = true ↔ e = .proved ∨ e = .exhaustiveFinite := by
  cases e <;> simp [Evidence.admits]

/-- An obligation family: its IR fragment, the property each obligation states,
    and the checker's decision. -/
structure Family where
  Input : Type
  Holds : Input → Prop
  decide : Input → Evidence

/-- Soundness: an admitting decision implies the property. -/
def Family.Sound (F : Family) : Prop := ∀ i, (F.decide i).admits = true → F.Holds i

/-- One generated obligation: a family and its input. -/
structure Instance where
  family : Family
  input : family.Input

/-- A challenge admits when every one of its obligations admits. -/
def admitAll (os : List Instance) : Bool :=
  os.all fun o => (o.family.decide o.input).admits

/-- Soundness composes over a challenge. -/
theorem admitAll_sound (os : List Instance) (hs : ∀ o ∈ os, o.family.Sound)
    (h : admitAll os = true) : ∀ o ∈ os, o.family.Holds o.input := by
  intro o ho
  have hall := List.all_eq_true.mp h o ho
  exact hs o ho o.input hall

section ExceptionTrees

variable {Id σ : Type} [DecidableEq Id]

/-- The exception-tree family of a checker deciding trees with `check`. -/
def exceptionTrees (Id σ : Type) [DecidableEq Id] (check : Law.Tree Id σ → Bool) : Family where
  Input := Law.Tree Id σ
  Holds := Law.Tree.Unambiguous
  decide T := if check T then .proved else .refuted

/-- A checker that admits only well-formed trees is sound for exception trees. -/
theorem exceptionTrees_sound (check : Law.Tree Id σ → Bool)
    (hcheck : ∀ T, check T = true → T.WellFormed) : (exceptionTrees Id σ check).Sound := by
  intro T h
  by_cases hc : check T = true
  · exact (hcheck T hc).unambiguous
  · simp [exceptionTrees, hc, Evidence.admits] at h

end ExceptionTrees

end Norn.Sem.Obligation
