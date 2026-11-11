import Replay.Funs
import BindingReference

/-!
# Production `decide_replay` and `same_call` meet K6

`Replay` is the fresh Aeneas translation of the production file
`crates/norn-kernel/src/replay_decision.rs`. The theorems hold for every
`RecordedCall` implementation and every stored record: an exact retry under the
same key replays the record that was committed (and only that record), a key
that holds a different call, or an unreadable record, is a conflict, and an
empty key is fresh.
-/

namespace Norn.Kernel.Binding
open Aeneas Aeneas.Std Result
open norn_kernel_replay

/-- The written decision as the production decision type. -/
def toDecision {R : Type} : Replay R → ReplayDecision R
  | .fresh => .Fresh
  | .replay recorded => .Replay recorded
  | .conflict => .Conflict

/-- The production call identity as the written one. -/
def callOf {A O I B : Type} (c : CallIdentity A O I B) : Call A O I B :=
  ⟨c.action, c.object, c.input, c.actor, c.rule_binding⟩

/-- For every `RecordedCall` implementation whose `same_call` answers `same`,
    production `decide_replay` is exactly the written K6 decision. -/
theorem decide_replay_correspondence {R : Type} (inst : RecordedCall R)
    (same : R → R → Bool) (hsame : ∀ a b, inst.same_call a b = ok (same a b))
    (stored : Option (Option R)) (call : R) :
    decide_replay inst stored call = ok (toDecision (replay same stored call)) := by
  rcases stored with _ | _ | recorded
  · rfl
  · rfl
  · simp only [decide_replay, hsame, bind_tc_ok, replay]
    cases same recorded call <;> rfl

/-- `same_call` under equality instances that decide equality is exactly
    equality of the five identity components. -/
theorem same_call_correct {A O I B : Type} [DecidableEq A] [DecidableEq O] [DecidableEq I]
    [DecidableEq B] (iA : core.cmp.PartialEq A A) (iO : core.cmp.PartialEq O O)
    (iI : core.cmp.PartialEq I I) (iB : core.cmp.PartialEq B B)
    (hA : ∀ a b, iA.eq a b = ok (decide (a = b))) (hO : ∀ a b, iO.eq a b = ok (decide (a = b)))
    (hI : ∀ a b, iI.eq a b = ok (decide (a = b))) (hB : ∀ a b, iB.eq a b = ok (decide (a = b)))
    (recorded call : CallIdentity A O I B) :
    same_call iA iO iI iB recorded call = ok (decide (callOf recorded = callOf call)) := by
  obtain ⟨a₁, o₁, i₁, x₁, b₁⟩ := recorded
  obtain ⟨a₂, o₂, i₂, x₂, b₂⟩ := call
  simp only [same_call, hA, hO, hI, hB, bind_tc_ok, callOf, Call.mk.injEq]
  by_cases h₁ : a₁ = a₂ <;> by_cases h₂ : o₁ = o₂ <;> by_cases h₃ : i₁ = i₂ <;>
    by_cases h₄ : x₁ = x₂ <;> by_cases h₅ : b₁ = b₂ <;> simp [h₁, h₂, h₃, h₄, h₅]

/-- K6 for the production record: `ActionCallRecord`'s `RecordedCall`
    implementation is `same_call` over its call identity `ident` (glue in
    `action_commit`). Then a repeated key replays exactly when the recorded
    identity is the call's, and the replayed outcome is the recorded one. -/
theorem replay_decision_k6 {R A O I B : Type} [DecidableEq A] [DecidableEq O]
    [DecidableEq I] [DecidableEq B] (iA : core.cmp.PartialEq A A)
    (iO : core.cmp.PartialEq O O) (iI : core.cmp.PartialEq I I) (iB : core.cmp.PartialEq B B)
    (hA : ∀ a b, iA.eq a b = ok (decide (a = b))) (hO : ∀ a b, iO.eq a b = ok (decide (a = b)))
    (hI : ∀ a b, iI.eq a b = ok (decide (a = b))) (hB : ∀ a b, iB.eq a b = ok (decide (a = b)))
    (ident : R → CallIdentity A O I B) (inst : RecordedCall R)
    (hinst : ∀ a b, inst.same_call a b = same_call iA iO iI iB (ident a) (ident b))
    (stored : Option (Option R)) (call : R) :
    decide_replay inst stored call =
      ok (toDecision (replay (fun a b => decide (callOf (ident a) = callOf (ident b)))
        stored call)) :=
  decide_replay_correspondence inst _ (fun a b => by
    rw [hinst]; exact same_call_correct iA iO iI iB hA hO hI hB (ident a) (ident b)) stored call

/-- An empty key is fresh: the call runs. -/
theorem replay_fresh_iff {R : Type} (inst : RecordedCall R) (same : R → R → Bool)
    (hsame : ∀ a b, inst.same_call a b = ok (same a b))
    (stored : Option (Option R)) (call : R) :
    decide_replay inst stored call = ok .Fresh ↔ stored = none := by
  rw [decide_replay_correspondence inst same hsame]
  rcases stored with _ | _ | recorded
  · simp [replay, toDecision]
  · simp [replay, toDecision]
  · cases h : same recorded call <;> simp [replay, toDecision, h]

/-- A replay returns the record that was committed under the key, and only
    for the same call: it never returns the new call, and it commits nothing
    (the decision carries no write; the caller returns it). -/
theorem replay_returns_recorded {R : Type} (inst : RecordedCall R) (same : R → R → Bool)
    (hsame : ∀ a b, inst.same_call a b = ok (same a b))
    (stored : Option (Option R)) (call outcome : R) :
    decide_replay inst stored call = ok (.Replay outcome) ↔
      stored = some (some outcome) ∧ same outcome call = true := by
  rw [decide_replay_correspondence inst same hsame]
  rcases stored with _ | _ | recorded
  · simp [replay, toDecision]
  · simp [replay, toDecision]
  · cases h : same recorded call <;> simp [replay, toDecision, h] <;> grind

/-- K6, exact retry and different payload, for the production identity: a key
    holding a record replays it exactly when every identity component (action,
    object, input, actor, writer binding) equals the call's, and is a conflict
    otherwise. -/
theorem replay_same_iff {R A O I B : Type} [DecidableEq A] [DecidableEq O]
    [DecidableEq I] [DecidableEq B] (iA : core.cmp.PartialEq A A)
    (iO : core.cmp.PartialEq O O) (iI : core.cmp.PartialEq I I) (iB : core.cmp.PartialEq B B)
    (hA : ∀ a b, iA.eq a b = ok (decide (a = b))) (hO : ∀ a b, iO.eq a b = ok (decide (a = b)))
    (hI : ∀ a b, iI.eq a b = ok (decide (a = b))) (hB : ∀ a b, iB.eq a b = ok (decide (a = b)))
    (ident : R → CallIdentity A O I B) (inst : RecordedCall R)
    (hinst : ∀ a b, inst.same_call a b = same_call iA iO iI iB (ident a) (ident b))
    (recorded call : R) :
    decide_replay inst (some (some recorded)) call =
      ok (if callOf (ident recorded) = callOf (ident call) then .Replay recorded else .Conflict) := by
  rw [replay_decision_k6 iA iO iI iB hA hO hI hB ident inst hinst]
  by_cases h : callOf (ident recorded) = callOf (ident call) <;> simp [replay, toDecision, h]

/-! ## Concrete vectors

A closed check the kernel evaluates on the production code, so a seeded bug is
refuted (`decide` finds the proposition false), not merely left unproved. -/

/-- Decided equality on `Nat`, as the derived `PartialEq` of the production
    identity fields. -/
def natEq : core.cmp.PartialEq Nat Nat := { eq := fun a b => ok (decide (a = b)) }

/-- The production `RecordedCall` shape: `same_call` over the call identity. -/
def identityCall : RecordedCall (CallIdentity Nat Nat Nat Nat) :=
  { same_call := same_call natEq natEq natEq natEq }

/-- Whether a production decision is the expected written one. -/
def decisionIs (r : Result (ReplayDecision (CallIdentity Nat Nat Nat Nat)))
    (e : Replay (Call Nat Nat Nat Nat)) : Bool :=
  match r, e with
  | ok .Fresh, .fresh => true
  | ok (.Replay x), .replay y => decide (callOf x = y)
  | ok .Conflict, .conflict => true
  | _, _ => false

/-- The call under test. -/
def call₀ : CallIdentity Nat Nat Nat Nat := ⟨1, 2, 3, 4, 5⟩

/-- What the key holds, and what the written semantics decides. -/
def replayVectors :
    List (Option (Option (CallIdentity Nat Nat Nat Nat)) × Replay (Call Nat Nat Nat Nat)) :=
  [ (none, .fresh),
    (some none, .conflict),
    (some (some ⟨1, 2, 3, 4, 5⟩), .replay ⟨1, 2, 3, 4, 5⟩),
    (some (some ⟨9, 2, 3, 4, 5⟩), .conflict),
    (some (some ⟨1, 9, 3, 4, 5⟩), .conflict),
    (some (some ⟨1, 2, 9, 4, 5⟩), .conflict),
    (some (some ⟨1, 2, 3, 9, 5⟩), .conflict),
    (some (some ⟨1, 2, 3, 4, 9⟩), .conflict) ]

theorem replay_vectors :
    replayVectors.all (fun v => decisionIs (decide_replay identityCall v.1 call₀) v.2) = true := by
  decide

end Norn.Kernel.Binding

#print axioms Norn.Kernel.Binding.decide_replay_correspondence
#print axioms Norn.Kernel.Binding.same_call_correct
#print axioms Norn.Kernel.Binding.replay_decision_k6
#print axioms Norn.Kernel.Binding.replay_fresh_iff
#print axioms Norn.Kernel.Binding.replay_returns_recorded
#print axioms Norn.Kernel.Binding.replay_same_iff
#print axioms Norn.Kernel.Binding.replay_vectors
