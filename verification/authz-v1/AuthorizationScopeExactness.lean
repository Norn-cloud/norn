import Std

namespace Norn.Verification.AuthorizationScopeExactness

/-!
Reference semantics for exact optional authorization dimensions.

`none` means the dimension is absent and never means wildcard. The production
Rust validator separately rejects empty and `*` strings before this equality
predicate is relevant. This file is not Rust refinement or authenticated-token
evidence.
-/

structure Scope where
  org : Nat
  tenant : Option Nat
  workspace : Option Nat
  environment : Option Nat
  businessUnit : Option Nat
  deriving DecidableEq

structure Subject where
  principal : Nat
  actorKind : Nat
  agentSession : Option Nat
  deriving DecidableEq

def exactScope (grant request : Scope) : Bool := grant == request
def exactSubject (grant request : Subject) : Bool := grant == request

theorem omitted_tenant_is_not_wildcard
    (org tenant : Nat) :
    exactScope
      { org := org, tenant := none, workspace := none,
        environment := none, businessUnit := none }
      { org := org, tenant := some tenant, workspace := none,
        environment := none, businessUnit := none } = false := by
  simp [exactScope]

theorem omitted_workspace_is_not_wildcard
    (org workspace : Nat) :
    exactScope
      { org := org, tenant := none, workspace := none,
        environment := none, businessUnit := none }
      { org := org, tenant := none, workspace := some workspace,
        environment := none, businessUnit := none } = false := by
  simp [exactScope]

theorem omitted_environment_is_not_wildcard
    (org environment : Nat) :
    exactScope
      { org := org, tenant := none, workspace := none,
        environment := none, businessUnit := none }
      { org := org, tenant := none, workspace := none,
        environment := some environment, businessUnit := none } = false := by
  simp [exactScope]

theorem omitted_business_unit_is_not_wildcard
    (org businessUnit : Nat) :
    exactScope
      { org := org, tenant := none, workspace := none,
        environment := none, businessUnit := none }
      { org := org, tenant := none, workspace := none,
        environment := none, businessUnit := some businessUnit } = false := by
  simp [exactScope]

theorem omitted_agent_session_is_not_wildcard
    (principal actorKind session : Nat) :
    exactSubject
      { principal := principal, actorKind := actorKind, agentSession := none }
      { principal := principal, actorKind := actorKind, agentSession := some session } = false := by
  simp [exactSubject]

theorem exact_scope_allow_implies_every_dimension_equal
    (grant request : Scope)
    (allowed : exactScope grant request = true) :
    grant.org = request.org ∧
    grant.tenant = request.tenant ∧
    grant.workspace = request.workspace ∧
    grant.environment = request.environment ∧
    grant.businessUnit = request.businessUnit := by
  simp [exactScope] at allowed
  subst request
  simp

end Norn.Verification.AuthorizationScopeExactness
