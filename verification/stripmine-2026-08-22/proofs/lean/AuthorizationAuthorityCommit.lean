import Std

namespace Norn.Verification.AuthorizationAuthorityCommit

/-!
Reference composition for the authoritative ActionIR authorization/store boundary.

`freshCommitAllowed` models the facts that the Rust store checks from one
candidate snapshot before publishing a new business effect and its authorization
receipt. `apply` deliberately treats an exact replay as inert and every semantic
collision or failed authorization as a rejection.

This proves the small mathematical model below. It is not a refinement proof for
the Rust implementation, Turso's snapshot transaction, transport identity, or
the authority-provisioning boundary.
-/

structure Authority where
  installed : Bool
  replacementReceiptCurrent : Bool
  policyActive : Bool
  grantActive : Bool
  policyVersion : Nat
  authorizationEpoch : Nat
  resourceVersion : Nat
  logicalTime : Nat
  notBefore : Nat
  hasExpiry : Bool
  expiresAt : Nat
  deriving DecidableEq

structure Proposal where
  exactIdentity : Bool
  exactScope : Bool
  exactAction : Bool
  exactResource : Bool
  exactCapability : Bool
  exactGrant : Bool
  observedPolicyVersion : Nat
  observedAuthorizationEpoch : Nat
  observedResourceVersion : Nat
  transitionDigest : Nat
  deriving DecidableEq

structure StoreState where
  authority : Authority
  hasCommittedReceipt : Bool
  committedDigest : Nat
  commitCount : Nat
  deriving DecidableEq

def withinValidityWindow (authority : Authority) : Bool :=
  decide (authority.notBefore ≤ authority.logicalTime) &&
    (!authority.hasExpiry || decide (authority.logicalTime < authority.expiresAt))

def freshCommitAllowed (state : StoreState) (proposal : Proposal) : Bool :=
  state.authority.installed &&
    (state.authority.replacementReceiptCurrent &&
      (state.authority.policyActive &&
        (state.authority.grantActive &&
          (proposal.exactIdentity &&
            (proposal.exactScope &&
              (proposal.exactAction &&
                (proposal.exactResource &&
                  (proposal.exactCapability &&
                    (proposal.exactGrant &&
                      (proposal.observedPolicyVersion == state.authority.policyVersion &&
                        (proposal.observedAuthorizationEpoch == state.authority.authorizationEpoch &&
                          (proposal.observedResourceVersion == state.authority.resourceVersion &&
                            withinValidityWindow state.authority))))))))))))

def apply (state : StoreState) (proposal : Proposal) : StoreState :=
  if state.hasCommittedReceipt then
    state
  else if freshCommitAllowed state proposal then
    { state with
      hasCommittedReceipt := true
      committedDigest := proposal.transitionDigest
      commitCount := state.commitCount + 1 }
  else
    state

theorem accepted_fresh_commit_uses_current_authority
    (state : StoreState) (proposal : Proposal)
    (fresh : state.hasCommittedReceipt = false)
    (committed : (apply state proposal).commitCount = state.commitCount + 1) :
    state.authority.installed = true ∧
    state.authority.replacementReceiptCurrent = true ∧
    state.authority.policyActive = true ∧
    state.authority.grantActive = true ∧
    proposal.exactIdentity = true ∧
    proposal.exactScope = true ∧
    proposal.exactAction = true ∧
    proposal.exactResource = true ∧
    proposal.exactCapability = true ∧
    proposal.exactGrant = true ∧
    proposal.observedPolicyVersion = state.authority.policyVersion ∧
    proposal.observedAuthorizationEpoch = state.authority.authorizationEpoch ∧
    proposal.observedResourceVersion = state.authority.resourceVersion ∧
    withinValidityWindow state.authority = true := by
  have allowed : freshCommitAllowed state proposal = true := by
    rw [apply, fresh] at committed
    cases authorized : freshCommitAllowed state proposal with
    | false => simp [authorized] at committed
    | true => rfl
  simp_all [freshCommitAllowed]

theorem missing_authority_receipt_denies
    (state : StoreState) (proposal : Proposal)
    (missing : state.authority.replacementReceiptCurrent = false) :
    apply state proposal = state := by
  simp [apply, freshCommitAllowed, missing]

theorem stale_authorization_epoch_denies
    (state : StoreState) (proposal : Proposal)
    (stale : proposal.observedAuthorizationEpoch ≠ state.authority.authorizationEpoch) :
    apply state proposal = state := by
  simp [apply, freshCommitAllowed, stale]

theorem revoked_grant_denies
    (state : StoreState) (proposal : Proposal)
    (revoked : state.authority.grantActive = false) :
    apply state proposal = state := by
  simp [apply, freshCommitAllowed, revoked]

theorem expired_grant_denies
    (state : StoreState) (proposal : Proposal)
    (expiring : state.authority.hasExpiry = true)
    (expired : state.authority.expiresAt ≤ state.authority.logicalTime) :
    apply state proposal = state := by
  have notFresh : withinValidityWindow state.authority = false := by
    simp [withinValidityWindow, expiring, Nat.not_lt.mpr expired]
  simp [apply, freshCommitAllowed, notFresh]

theorem replay_or_collision_is_inert
    (state : StoreState) (proposal : Proposal)
    (alreadyCommitted : state.hasCommittedReceipt = true) :
    apply state proposal = state := by
  simp [apply, alreadyCommitted]

theorem successful_fresh_commit_is_atomic
    (state : StoreState) (proposal : Proposal)
    (fresh : state.hasCommittedReceipt = false)
    (allowed : freshCommitAllowed state proposal = true) :
    (apply state proposal).hasCommittedReceipt = true ∧
    (apply state proposal).committedDigest = proposal.transitionDigest ∧
    (apply state proposal).commitCount = state.commitCount + 1 ∧
    (apply state proposal).authority = state.authority := by
  simp [apply, fresh, allowed]

end Norn.Verification.AuthorizationAuthorityCommit
