import Mathlib.Data.List.Range
import Mathlib.Order.Interval.Finset.Defs
import Mathlib.Tactic.Linarith

/-!
# Inductive safety, frames and finite exhaustiveness: written semantics

The induction obligations of `crates/norn-checker/src/obligations/induction.rs`
generate, per law, an initialization goal and one preservation goal per writer
in the closure. A preservation goal is discharged by `transition_frame` when
the writer is a transition that changes only one stored property that the law
never reads. The finite obligations of `obligations/finite.rs` lower a
one-create writer to a closed finite relation whose inputs range over integer
intervals.

This file states, independently of the extraction:
* inductive safety (#718 meta-theorem 1): initialization plus preservation by
  every writer make the law hold on every reachable state;
* the frame lemma: a law that depends only on its reads is preserved by a step
  that changes nothing it reads;
* finite exhaustiveness: checking every point of the enumerated interval
  product checks every in-bounds valuation, so an exhaustive-finite verdict
  over the generated relation covers the whole declared finite domain.
-/

namespace Norn.Checker.Obligations

variable {S : Type}

/-- States reachable from `init` by `step`. -/
inductive Reachable (init : S → Prop) (step : S → S → Prop) : S → Prop
  | base {s} : init s → Reachable init step s
  | next {s t} : Reachable init step s → step s t → Reachable init step t

/-- Inductive safety: an invariant that holds initially and is preserved by
    every step holds on every reachable state. -/
theorem inductive_safety {init : S → Prop} {step : S → S → Prop} {I : S → Prop}
    (hinit : ∀ s, init s → I s) (hpres : ∀ s t, I s → step s t → I t) :
    ∀ s, Reachable init step s → I s := by
  intro s hs
  induction hs with
  | base h => exact hinit _ h
  | next _ hst ih => exact hpres _ _ ih hst

/-- Inductive safety over a writer closure: a step is a step of some writer,
    and every writer's preservation goal is discharged. -/
theorem inductive_safety_writers {W : Type} {init : S → Prop} (writers : List W)
    (rel : W → S → S → Prop) {I : S → Prop} (hinit : ∀ s, init s → I s)
    (hpres : ∀ w ∈ writers, ∀ s t, I s → rel w s t → I t) :
    ∀ s, Reachable init (fun s t => ∃ w ∈ writers, rel w s t) s → I s :=
  inductive_safety hinit (fun s t hs ⟨w, hw, hst⟩ => hpres w hw s t hs hst)

variable {F V : Type}

/-- The law's truth depends only on the fields in `reads`. -/
def DependsOnly (law : (F → V) → Prop) (reads : List F) : Prop :=
  ∀ σ τ : F → V, (∀ f ∈ reads, σ f = τ f) → (law σ ↔ law τ)

/-- The step from `pre` to `post` changes no field other than `written`. -/
def Frames (written : F) (pre post : F → V) : Prop :=
  ∀ f, f ≠ written → post f = pre f

/-- The frame lemma: a step that writes only a field the law never reads
    preserves the law. -/
theorem frame_preserves {law : (F → V) → Prop} {reads : List F} {written : F}
    (hdep : DependsOnly law reads) (hw : written ∉ reads) {pre post : F → V}
    (hframe : Frames written pre post) (h : law pre) : law post :=
  (hdep pre post (fun f hf => (hframe f (fun e => hw (e ▸ hf))).symm)).mp h

/-! ## Finite exhaustiveness -/

/-- The points of an integer interval, in order. -/
def interval (b : Int × Int) : List Int :=
  List.map (fun k : Nat => b.1 + (k : Int)) (List.range (b.2 - b.1 + 1).toNat)

/-- Every valuation of a product of integer intervals, in order. -/
def enumerate : List (Int × Int) → List (List Int)
  | [] => [[]]
  | b :: bs => (interval b).flatMap (fun x => (enumerate bs).map (x :: ·))

/-- A valuation within the declared bounds. -/
def InBounds : List (Int × Int) → List Int → Prop
  | [], [] => True
  | b :: bs, x :: xs => b.1 ≤ x ∧ x ≤ b.2 ∧ InBounds bs xs
  | _, _ => False

theorem mem_interval {b : Int × Int} {x : Int} (h1 : b.1 ≤ x) (h2 : x ≤ b.2) :
    x ∈ interval b := by
  unfold interval
  refine List.mem_map.mpr ⟨(x - b.1).toNat, List.mem_range.mpr ?_, ?_⟩
  · omega
  · omega

/-- The enumeration is complete: every in-bounds valuation is enumerated. -/
theorem mem_enumerate : ∀ (bs : List (Int × Int)) (xs : List Int),
    InBounds bs xs → xs ∈ enumerate bs
  | [], [], _ => by simp [enumerate]
  | b :: bs, x :: xs, ⟨h1, h2, h⟩ => by
    unfold enumerate
    exact List.mem_flatMap.mpr ⟨x, mem_interval h1 h2,
      List.mem_map.mpr ⟨xs, mem_enumerate bs xs h, rfl⟩⟩
  | [], _ :: _, h => absurd h (by simp [InBounds])
  | _ :: _, [], h => absurd h (by simp [InBounds])

/-- Finite exhaustiveness: a property checked at every enumerated valuation
    holds at every valuation in the declared bounds. -/
theorem exhaustive_finite {bs : List (Int × Int)} {P : List Int → Prop}
    (h : ∀ xs ∈ enumerate bs, P xs) : ∀ xs, InBounds bs xs → P xs :=
  fun xs hxs => h xs (mem_enumerate bs xs hxs)

end Norn.Checker.Obligations

#print axioms Norn.Checker.Obligations.inductive_safety
#print axioms Norn.Checker.Obligations.inductive_safety_writers
#print axioms Norn.Checker.Obligations.frame_preserves
#print axioms Norn.Checker.Obligations.exhaustive_finite
