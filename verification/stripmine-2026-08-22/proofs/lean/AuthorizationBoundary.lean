import Std

namespace Norn.Verification.AuthorizationBoundary

/-!
Reference semantics for the pure commit-time predicate in `norn-engine/authz.rs`.

An allow decision requires well-formed, exact evidence for subject, actor kind,
session, full scope, action, resource, capability, policy identity/version and
authorization epoch. It additionally requires a current allow result, an active
grant, and validity at the injected commit logical time. `hasExpiry = false`
models the Rust grant's `None` expiry.

This proves properties of the mathematical reference predicate. It is not a
Rust refinement theorem, a policy-evaluator proof, an authenticated-identity
proof, a snapshot/store atomicity proof, or evidence that production mutations
already invoke this checker.
-/

structure Request where
  evidenceWellFormed : Bool
  requestSubject : Nat
  grantSubject : Nat
  policySubject : Nat
  requestActorKind : Nat
  grantActorKind : Nat
  policyActorKind : Nat
  requestSession : Nat
  grantSession : Nat
  policySession : Nat
  requestScope : Nat
  grantScope : Nat
  resourceScope : Nat
  policyScope : Nat
  requestAction : Nat
  grantAction : Nat
  policyAction : Nat
  requestResource : Nat
  grantResource : Nat
  authoritativeResource : Nat
  policyResource : Nat
  expectedResourceVersion : Nat
  currentResourceVersion : Nat
  policyResourceVersion : Nat
  requiredCapability : Nat
  grantedCapability : Nat
  policyCapability : Nat
  expectedPolicyId : Nat
  grantPolicyId : Nat
  currentPolicyId : Nat
  policyEvidenceId : Nat
  expectedPolicyVersion : Nat
  grantPolicyVersion : Nat
  currentPolicyVersion : Nat
  policyEvidenceVersion : Nat
  observedAuthorizationEpoch : Nat
  grantAuthorizationEpoch : Nat
  currentAuthorizationEpoch : Nat
  policyAuthorizationEpoch : Nat
  policyAllows : Bool
  currentPolicyActive : Bool
  grantActive : Bool
  notBefore : Nat
  commitTime : Nat
  policyEvaluatedAt : Nat
  hasExpiry : Bool
  expiresAt : Nat
  deriving DecidableEq

def identityBound (request : Request) : Bool :=
  request.requestSubject == request.grantSubject &&
    (request.requestSubject == request.policySubject &&
      (request.requestActorKind == request.grantActorKind &&
        (request.requestActorKind == request.policyActorKind &&
          (request.requestSession == request.grantSession &&
            request.requestSession == request.policySession))))

def scopeBound (request : Request) : Bool :=
  request.requestScope == request.grantScope &&
    (request.requestScope == request.resourceScope &&
      request.requestScope == request.policyScope)

def operationBound (request : Request) : Bool :=
  request.requestAction == request.grantAction &&
    (request.requestAction == request.policyAction &&
      (request.requestResource == request.grantResource &&
        (request.requestResource == request.authoritativeResource &&
          (request.requestResource == request.policyResource &&
            (request.requiredCapability == request.grantedCapability &&
              request.requiredCapability == request.policyCapability)))))

def policyBound (request : Request) : Bool :=
  request.expectedPolicyId == request.grantPolicyId &&
    (request.expectedPolicyId == request.currentPolicyId &&
      (request.expectedPolicyId == request.policyEvidenceId &&
        (request.expectedPolicyVersion == request.grantPolicyVersion &&
          (request.expectedPolicyVersion == request.currentPolicyVersion &&
            (request.expectedPolicyVersion == request.policyEvidenceVersion &&
              (request.observedAuthorizationEpoch == request.grantAuthorizationEpoch &&
                (request.observedAuthorizationEpoch == request.currentAuthorizationEpoch &&
                  request.observedAuthorizationEpoch == request.policyAuthorizationEpoch)))))))

def resourceVersionBound (request : Request) : Bool :=
  request.expectedResourceVersion == request.currentResourceVersion &&
    request.expectedResourceVersion == request.policyResourceVersion

def temporallyFresh (request : Request) : Bool :=
  request.policyEvaluatedAt == request.commitTime &&
    (decide (request.notBefore ≤ request.commitTime) &&
      (!request.hasExpiry || decide (request.commitTime < request.expiresAt)))

def allowed (request : Request) : Bool :=
  request.evidenceWellFormed &&
    (identityBound request &&
      (scopeBound request &&
        (operationBound request &&
          (policyBound request &&
            (resourceVersionBound request &&
              (request.currentPolicyActive &&
                (request.policyAllows &&
                  (request.grantActive && temporallyFresh request))))))))

theorem allow_parts (request : Request)
    (authorized : allowed request = true) :
    request.evidenceWellFormed = true ∧
    identityBound request = true ∧
    scopeBound request = true ∧
    operationBound request = true ∧
    policyBound request = true ∧
    resourceVersionBound request = true ∧
    request.currentPolicyActive = true ∧
    request.policyAllows = true ∧
    request.grantActive = true ∧
    temporallyFresh request = true := by
  simpa [allowed] using authorized

theorem allow_binds_identity (request : Request)
    (authorized : allowed request = true) :
    request.requestSubject = request.grantSubject ∧
    request.requestSubject = request.policySubject ∧
    request.requestActorKind = request.grantActorKind ∧
    request.requestActorKind = request.policyActorKind ∧
    request.requestSession = request.grantSession ∧
    request.requestSession = request.policySession := by
  have bounded := (allow_parts request authorized).2.1
  simpa [identityBound] using bounded

theorem allow_is_scope_closed (request : Request)
    (authorized : allowed request = true) :
    request.requestScope = request.grantScope ∧
    request.requestScope = request.resourceScope ∧
    request.requestScope = request.policyScope := by
  have bounded := (allow_parts request authorized).2.2.1
  simpa [scopeBound] using bounded

theorem allow_binds_action_resource_capability (request : Request)
    (authorized : allowed request = true) :
    request.requestAction = request.grantAction ∧
    request.requestAction = request.policyAction ∧
    request.requestResource = request.grantResource ∧
    request.requestResource = request.authoritativeResource ∧
    request.requestResource = request.policyResource ∧
    request.requiredCapability = request.grantedCapability ∧
    request.requiredCapability = request.policyCapability := by
  have bounded := (allow_parts request authorized).2.2.2.1
  simpa [operationBound] using bounded

theorem allow_uses_current_policy_and_authorization_epoch (request : Request)
    (authorized : allowed request = true) :
    request.expectedPolicyId = request.grantPolicyId ∧
    request.expectedPolicyId = request.currentPolicyId ∧
    request.expectedPolicyId = request.policyEvidenceId ∧
    request.expectedPolicyVersion = request.grantPolicyVersion ∧
    request.expectedPolicyVersion = request.currentPolicyVersion ∧
    request.expectedPolicyVersion = request.policyEvidenceVersion ∧
    request.observedAuthorizationEpoch = request.grantAuthorizationEpoch ∧
    request.observedAuthorizationEpoch = request.currentAuthorizationEpoch ∧
    request.observedAuthorizationEpoch = request.policyAuthorizationEpoch := by
  have bounded := (allow_parts request authorized).2.2.2.2.1
  simpa [policyBound] using bounded

theorem allow_requires_current_policy_allow_and_active_grant (request : Request)
    (authorized : allowed request = true) :
    request.evidenceWellFormed = true ∧
    request.currentPolicyActive = true ∧
    request.policyAllows = true ∧
    request.grantActive = true := by
  have parts := allow_parts request authorized
  exact
    ⟨parts.1,
      parts.2.2.2.2.2.2.1,
      parts.2.2.2.2.2.2.2.1,
      parts.2.2.2.2.2.2.2.2.1⟩

theorem allow_uses_current_resource_version (request : Request)
    (authorized : allowed request = true) :
    request.expectedResourceVersion = request.currentResourceVersion ∧
    request.expectedResourceVersion = request.policyResourceVersion := by
  have bounded := (allow_parts request authorized).2.2.2.2.2.1
  simpa [resourceVersionBound] using bounded

theorem allow_is_temporally_fresh (request : Request)
    (authorized : allowed request = true) :
    request.policyEvaluatedAt = request.commitTime ∧
    request.notBefore ≤ request.commitTime ∧
    (request.hasExpiry = false ∨ request.commitTime < request.expiresAt) := by
  have fresh := (allow_parts request authorized).2.2.2.2.2.2.2.2.2
  simpa [temporallyFresh] using fresh

theorem revoked_grant_denies (request : Request)
    (revoked : request.grantActive = false) :
    allowed request = false := by
  simp [allowed, revoked]

theorem stale_policy_version_denies (request : Request)
    (stale : request.expectedPolicyVersion ≠ request.currentPolicyVersion) :
    allowed request = false := by
  simp [allowed, policyBound, stale]

theorem stale_authorization_epoch_denies (request : Request)
    (stale : request.observedAuthorizationEpoch ≠ request.currentAuthorizationEpoch) :
    allowed request = false := by
  simp [allowed, policyBound, stale]

theorem stale_resource_version_denies (request : Request)
    (stale : request.expectedResourceVersion ≠ request.currentResourceVersion) :
    allowed request = false := by
  simp [allowed, resourceVersionBound, stale]

theorem stale_policy_evaluation_time_denies (request : Request)
    (stale : request.policyEvaluatedAt ≠ request.commitTime) :
    allowed request = false := by
  simp [allowed, temporallyFresh, stale]

theorem inactive_policy_denies (request : Request)
    (inactive : request.currentPolicyActive = false) :
    allowed request = false := by
  simp [allowed, inactive]

theorem expired_grant_denies (request : Request)
    (expiring : request.hasExpiry = true)
    (expired : request.expiresAt ≤ request.commitTime) :
    allowed request = false := by
  simp [allowed, temporallyFresh, expiring, Nat.not_lt.mpr expired]

end Norn.Verification.AuthorizationBoundary
