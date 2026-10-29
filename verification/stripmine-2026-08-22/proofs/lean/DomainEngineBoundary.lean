import Std

namespace Norn.Verification.DomainEngineBoundary

/-!
Reference semantics for the hard-invariant ownership boundary.

An Entity artifact declares whether its authoritative state is owned by the
generic closed ActionIR executor or by one typed domain engine. A command may
write a domain-owned entity only when it enters through the matching domain
engine boundary. This file is an independent reference proof; it does not
establish correspondence to the Rust executor.
-/

inductive Domain where
  | inventory
  | accounting
  | compliance
  | settlement
  | tax
  deriving DecidableEq

inductive MutationAuthority where
  | genericAction
  | domainEngine (domain : Domain)
  deriving DecidableEq

inductive CommitSource where
  | genericAction
  | domainCommand (domain : Domain)
  deriving DecidableEq

def mayCommit : CommitSource → MutationAuthority → Bool
  | .genericAction, .genericAction => true
  | .domainCommand commandDomain, .domainEngine ownerDomain =>
      decide (commandDomain = ownerDomain)
  | _, _ => false

theorem generic_action_cannot_mutate_domain_owned_state
    (domain : Domain) :
    mayCommit .genericAction (.domainEngine domain) = false := by
  rfl

theorem domain_command_cannot_mutate_generic_action_state
    (domain : Domain) :
    mayCommit (.domainCommand domain) .genericAction = false := by
  rfl

theorem mismatched_domain_command_rejects
    (commandDomain ownerDomain : Domain)
    (mismatch : commandDomain ≠ ownerDomain) :
    mayCommit (.domainCommand commandDomain) (.domainEngine ownerDomain) = false := by
  simp [mayCommit, mismatch]

theorem accepted_domain_commit_has_exact_owner
    (commandDomain ownerDomain : Domain)
    (accepted :
      mayCommit (.domainCommand commandDomain) (.domainEngine ownerDomain) = true) :
    commandDomain = ownerDomain := by
  simp [mayCommit] at accepted
  exact accepted

theorem accepted_generic_commit_has_generic_owner
    (authority : MutationAuthority)
    (accepted : mayCommit .genericAction authority = true) :
    authority = .genericAction := by
  cases authority with
  | genericAction => rfl
  | domainEngine domain => simp [mayCommit] at accepted

end Norn.Verification.DomainEngineBoundary
