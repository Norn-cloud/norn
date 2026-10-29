import Std

namespace Norn.Verification.QuantityBatchAtomicity

/-!
Small mathematical reference for the all-or-none batch boundary. It has no
axioms, `sorry`, or `admit`. It is not a refinement proof for Rust, checked
`i128`, the Norn posting evaluator, serialization, or Turso durability.
-/

structure State where
  ownerVersion : Nat
  sourceBalance : Int
  destinationBalance : Int
  deriving DecidableEq

structure Posting where
  sourceDelta : Int
  destinationDelta : Int
  deriving DecidableEq

def total (state : State) : Int :=
  state.sourceBalance + state.destinationBalance

/- An admitted posting changes both balance components. Admission requires its
two explicit deltas to conserve the mathematical total. -/
def applyPosting (state : State) (posting : Posting) : Option State :=
  if _conserves : posting.sourceDelta + posting.destinationDelta = 0 then
    some {
      ownerVersion := state.ownerVersion + 1
      sourceBalance := state.sourceBalance + posting.sourceDelta
      destinationBalance := state.destinationBalance + posting.destinationDelta
    }
  else
    none

def evaluateBatch : State → List Posting → Option State
  | state, [] => some state
  | state, delta :: rest =>
      match applyPosting state delta with
      | none => none
      | some next => evaluateBatch next rest

def publishBatch (state : State) (postings : List Posting) : State :=
  (evaluateBatch state postings).getD state

theorem accepted_posting_applies_deltas_and_conserves
    {state next : State} {posting : Posting}
    (accepted : applyPosting state posting = some next) :
    next.sourceBalance = state.sourceBalance + posting.sourceDelta ∧
      next.destinationBalance = state.destinationBalance + posting.destinationDelta ∧
      total next = total state := by
  unfold applyPosting at accepted
  split at accepted
  · simp only [Option.some.injEq] at accepted
    subst next
    constructor
    · rfl
    constructor
    · rfl
    · simp only [total]
      omega
  · simp at accepted

theorem rejected_batch_is_noop
    {state : State} {postings : List Posting}
    (rejected : evaluateBatch state postings = none) :
    publishBatch state postings = state := by
  simp [publishBatch, rejected]

theorem accepted_batch_matches_sequential_execution
    {state next : State} {postings : List Posting}
    (accepted : evaluateBatch state postings = some next) :
    publishBatch state postings = next := by
  simp [publishBatch, accepted]

theorem accepted_batch_conserves_total
    {state next : State} {postings : List Posting}
    (accepted : evaluateBatch state postings = some next) :
    total next = total state := by
  induction postings generalizing state next with
  | nil =>
      simp [evaluateBatch] at accepted
      subst next
      rfl
  | cons posting rest inductionHypothesis =>
      unfold evaluateBatch at accepted
      cases applied : applyPosting state posting with
      | none => simp [applied] at accepted
      | some intermediate =>
          rw [applied] at accepted
          have tailConserved := inductionHypothesis accepted
          have headConserved := (accepted_posting_applies_deltas_and_conserves applied).2.2
          omega

theorem accepted_batch_advances_exactly
    {state next : State} {postings : List Posting}
    (accepted : evaluateBatch state postings = some next) :
    next.ownerVersion = state.ownerVersion + postings.length := by
  induction postings generalizing state next with
  | nil =>
      simp [evaluateBatch] at accepted
      subst next
      simp
  | cons posting rest inductionHypothesis =>
      unfold evaluateBatch at accepted
      cases applied : applyPosting state posting with
      | none => simp [applied] at accepted
      | some intermediate =>
          rw [applied] at accepted
          have child := inductionHypothesis accepted
          unfold applyPosting at applied
          split at applied
          · simp only [Option.some.injEq] at applied
            subst intermediate
            simpa [Nat.add_assoc, Nat.add_comm, Nat.add_left_comm] using child
          · simp at applied

end Norn.Verification.QuantityBatchAtomicity
