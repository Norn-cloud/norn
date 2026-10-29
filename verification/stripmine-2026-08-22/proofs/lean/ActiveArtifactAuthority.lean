import Std

namespace Norn.Verification.ActiveArtifactAuthority

/-!
Reference proof for the immutable-envelope / mutable-authority-pointer split.
Versions are abstract naturals and persistence is one atomic state assignment.
This proves the named state-machine properties only; it is not a refinement proof
of the Rust, Turso, or legacy Postgres implementations.
-/

structure State where
  envelopes : List Nat
  activeVersion : Option Nat
  deriving DecidableEq

def executable (state : State) (version : Nat) : Bool :=
  state.activeVersion == some version && state.envelopes.contains version

def replace (state : State) (expected next : Nat) : State :=
  if state.activeVersion == some expected then
    { envelopes := next :: state.envelopes, activeVersion := some next }
  else
    state

theorem accepted_replacement_selects_next
    (state : State) (expected next : Nat)
    (current : state.activeVersion = some expected) :
    executable (replace state expected next) next = true := by
  simp [replace, executable, current]

theorem accepted_replacement_rejects_prior_exact_version
    (state : State) (expected next : Nat)
    (current : state.activeVersion = some expected)
    (distinct : expected ≠ next) :
    executable (replace state expected next) expected = false := by
  have next_ne : next ≠ expected := Ne.symm distinct
  simp [replace, executable, current, distinct, next_ne]

theorem accepted_replacement_preserves_prior_envelope
    (state : State) (expected next : Nat)
    (current : state.activeVersion = some expected)
    (present : expected ∈ state.envelopes) :
    expected ∈ (replace state expected next).envelopes := by
  simp [replace, current, present]

theorem stale_competing_replacement_is_noop
    (state : State) (staleExpected next : Nat)
    (stale : state.activeVersion ≠ some staleExpected) :
    replace state staleExpected next = state := by
  simp [replace, stale]

end Norn.Verification.ActiveArtifactAuthority
