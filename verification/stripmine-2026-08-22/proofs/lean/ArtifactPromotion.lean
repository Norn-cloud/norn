import Std

namespace Norn.Verification.ArtifactPromotion

/-!
Reference proof for the direct-authoring lifecycle guard. The production API
calls the corresponding Rust guard and has adversarial route tests, but this
file does not by itself establish Rust-to-Lean correspondence.
-/

inductive ArtifactKind where
  | entity
  | relation
  | action
  | rule
  | connector
  | widget
  | customization
  | setting
  | businessPack
  | policy
  | derived
  | sourceSchema
  | mapping
  deriving DecidableEq

inductive Lifecycle where
  | draft
  | proposed
  | approved
  | active
  | deprecated
  | superseded
  | rejected
  deriving DecidableEq

def authorityBearing : ArtifactKind → Bool
  | .entity
  | .relation
  | .action
  | .rule
  | .connector
  | .widget
  | .customization
  | .setting
  | .businessPack
  | .policy
  | .derived
  | .mapping => true
  | .sourceSchema => false

def grantsRuntimeAuthority : Lifecycle → Bool
  | .approved | .active => true
  | _ => false

def directWriteAllowed (kind : ArtifactKind) (lifecycle : Lifecycle) : Bool :=
  !(authorityBearing kind && grantsRuntimeAuthority lifecycle)

theorem allowed_direct_write_never_grants_authority
    (kind : ArtifactKind) (lifecycle : Lifecycle)
    (allowed : directWriteAllowed kind lifecycle = true) :
    authorityBearing kind = false ∨ grantsRuntimeAuthority lifecycle = false := by
  cases kind <;> cases lifecycle <;>
    simp_all [directWriteAllowed, authorityBearing, grantsRuntimeAuthority]

theorem active_authority_bearing_write_is_rejected
    (kind : ArtifactKind) (authority : authorityBearing kind = true) :
    directWriteAllowed kind .active = false := by
  simp [directWriteAllowed, grantsRuntimeAuthority, authority]

theorem approved_authority_bearing_write_is_rejected
    (kind : ArtifactKind) (authority : authorityBearing kind = true) :
    directWriteAllowed kind .approved = false := by
  simp [directWriteAllowed, grantsRuntimeAuthority, authority]

end Norn.Verification.ArtifactPromotion
