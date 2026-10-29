import Std

namespace Norn.Verification.ClosedActionEffect

/-!
Reference semantics for the first closed custom ActionIR operation. It models
only `upsert_input_object`; production correspondence is exercised by tests but
is not proved by this file.
-/

structure Program where
  objectKind : String
  entityArtifactId : String
  entityArtifactVersion : String
  deriving DecidableEq

structure Input where
  objectId : Nat
  payloadHash : Nat
  deriving DecidableEq

structure Effect where
  objectId : Nat
  objectKind : String
  entityArtifactId : String
  entityArtifactVersion : String
  payloadHash : Nat
  deriving DecidableEq

structure State where
  ownerVersion : Nat
  effects : List Effect
  deriving DecidableEq

structure Request where
  expectedOwnerVersion : Option Nat
  input : Input
  deriving DecidableEq

def deriveEffect (program : Program) (input : Input) : Effect := {
  objectId := input.objectId
  objectKind := program.objectKind
  entityArtifactId := program.entityArtifactId
  entityArtifactVersion := program.entityArtifactVersion
  payloadHash := input.payloadHash
}

def execute (program : Program) (state : State) (request : Request) : Option State :=
  if request.expectedOwnerVersion = some state.ownerVersion then
    some {
      ownerVersion := state.ownerVersion + 1
      effects := state.effects ++ [deriveEffect program request.input]
    }
  else
    none

theorem missing_owner_precondition_rejects
    (program : Program) (state : State) (input : Input) :
    execute program state { expectedOwnerVersion := none, input := input } = none := by
  simp [execute]

theorem stale_owner_precondition_rejects
    (program : Program) (state : State) (request : Request)
    (stale : request.expectedOwnerVersion ≠ some state.ownerVersion) :
    execute program state request = none := by
  simp [execute, stale]

theorem accepted_execution_has_one_derived_effect
    (program : Program) (state next : State) (request : Request)
    (accepted : execute program state request = some next) :
    next.ownerVersion = state.ownerVersion + 1 ∧
      next.effects = state.effects ++ [deriveEffect program request.input] := by
  unfold execute at accepted
  split at accepted
  · injection accepted with equality
    subst next
    simp
  · contradiction

theorem derived_effect_cannot_change_declared_pins
    (program : Program) (input : Input) :
    (deriveEffect program input).objectKind = program.objectKind ∧
      (deriveEffect program input).entityArtifactId = program.entityArtifactId ∧
      (deriveEffect program input).entityArtifactVersion = program.entityArtifactVersion := by
  simp [deriveEffect]

end Norn.Verification.ClosedActionEffect
