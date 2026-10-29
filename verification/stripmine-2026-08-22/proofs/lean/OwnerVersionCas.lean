import Std

namespace Norn.Verification.OwnerVersionCas

/-!
Reference semantics for one owner-version compare-and-swap commit. Production
InMemory/Turso tests exercise the corresponding envelope, but this theorem is
not an extraction or refinement proof of those implementations.
-/

structure State where
  ownerVersion : Nat
  effectCount : Nat
  eventCount : Nat
  receiptHash : Option (Nat × Nat)
  deriving DecidableEq

def commit (state : State) (expectedVersion inputHash effectHash : Nat) : Option State :=
  if expectedVersion = state.ownerVersion ∧ state.receiptHash.isNone then
    some {
      ownerVersion := state.ownerVersion + 1
      effectCount := state.effectCount + 1
      eventCount := state.eventCount + 1
      receiptHash := some (inputHash, effectHash)
    }
  else
    none

def replay (state : State) (inputHash effectHash : Nat) : Option State :=
  if state.receiptHash = some (inputHash, effectHash) then some state else none

theorem stale_commit_rejects (state : State) (expectedVersion inputHash effectHash : Nat)
    (stale : expectedVersion ≠ state.ownerVersion) :
    commit state expectedVersion inputHash effectHash = none := by
  simp [commit, stale]

theorem accepted_commit_increments_once (state next : State)
    (expectedVersion inputHash effectHash : Nat)
    (accepted : commit state expectedVersion inputHash effectHash = some next) :
    next.ownerVersion = state.ownerVersion + 1 ∧
      next.effectCount = state.effectCount + 1 ∧
      next.eventCount = state.eventCount + 1 ∧
    next.receiptHash = some (inputHash, effectHash) := by
  unfold commit at accepted
  split at accepted
  · injection accepted with equality
    subst next
    simp
  · contradiction

theorem accepted_commit_keeps_effect_event_atomic (state next : State)
    (expectedVersion inputHash effectHash : Nat)
    (balanced : state.effectCount = state.eventCount)
    (accepted : commit state expectedVersion inputHash effectHash = some next) :
    next.effectCount = next.eventCount := by
  obtain ⟨_, effectStep, eventStep, _⟩ :=
    accepted_commit_increments_once state next expectedVersion inputHash effectHash accepted
  omega

theorem exact_replay_is_state_preserving (state : State) (inputHash effectHash : Nat)
    (same : state.receiptHash = some (inputHash, effectHash)) :
    replay state inputHash effectHash = some state := by
  simp [replay, same]

theorem stored_receipt_blocks_second_commit (state : State)
    (expectedVersion inputHash effectHash storedInput storedEffect : Nat)
    (stored : state.receiptHash = some (storedInput, storedEffect)) :
    commit state expectedVersion inputHash effectHash = none := by
  simp [commit, stored]

theorem conflicting_input_replay_rejects (state : State)
    (storedInput storedEffect inputHash effectHash : Nat)
    (stored : state.receiptHash = some (storedInput, storedEffect))
    (different : storedInput ≠ inputHash) :
    replay state inputHash effectHash = none := by
  simp [replay, stored, different]

theorem conflicting_effect_replay_rejects (state : State)
    (storedInput storedEffect effectHash : Nat)
    (stored : state.receiptHash = some (storedInput, storedEffect))
    (different : storedEffect ≠ effectHash) :
    replay state storedInput effectHash = none := by
  simp [replay, stored, different]

end Norn.Verification.OwnerVersionCas
