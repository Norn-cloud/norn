//! Stable, typed admission diagnostics.
//!
//! Every rejection carries an [`AuthzCode`] whose string form is part of the
//! T2 contract: callers branch on the code, never on the message. The error
//! types are hand-written because `thiserror` is not on this crate's
//! dependency allowlist (SKELETON.md); they are plain data plus `Display`.

use std::fmt;

/// Stable machine-readable code of an authorization admission finding.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[non_exhaustive]
pub enum AuthzCode {
    /// A schema fragment failed to parse.
    SchemaParse,
    /// The fragments parsed but do not compose into one schema, or a fragment
    /// produced a warning (fail closed).
    SchemaInvalid,
    /// A principal, resource, or action-context `scope` is not typed
    /// `Norn::Scope` in the composed schema, so the structural scope
    /// conjunct would not compare scopes.
    SchemaScopeType,
    /// A compiled policy failed to parse. This is a compiler defect; the
    /// policy is still rejected.
    PolicyParse,
    /// A policy failed schema validation (strict mode).
    PolicyInvalid,
    /// A policy passed type checking but the validator flagged it (for
    /// example an impossible policy or a confusable string); rejected.
    PolicyLint,
    /// A policy or rule id is empty.
    AccessIdEmpty,
    /// Two rules of one policy share a rule id.
    AccessRuleDuplicate,
    /// A generic entitlement template does not use the fixed closed grammar.
    AccessGenericRuleInvalid,
    /// A rule permits no action.
    AccessActionsEmpty,
    /// The structural scope conjunct does not list `org`.
    AccessScopeOrgRequired,
    /// A role condition names no role.
    AccessRolesEmpty,
    /// A type, link role, or context name does not fit its grammar.
    AccessNameInvalid,
    /// A context condition names the reserved `scope` context value.
    AccessContextReserved,
    /// A resource condition was compiled without the projection metadata
    /// needed to type-check it and reject unsupported values.
    AccessProjectionRequired,
    /// An `AccessIR` comparison refers to a decimal property, whose string
    /// projection does not preserve numeric comparison semantics.
    AccessDecimalComparison,
    /// A trusted principal attribute uses a reserved actor member name.
    AccessPrincipalReserved,
    /// An attribute is inferred with conflicting types or cardinalities.
    AccessPrincipalConflict,
    /// The Cedar projection omits or disagrees with inferred principal fields.
    AccessPrincipalProjectionRequired,
    /// A principal attribute comparison uses an unsupported operator.
    AccessPrincipalOperator,
    /// `in` is used with a literal resource comparison.
    AccessResourceOperator,
    /// A projected `rel` cannot be represented by the supported record
    /// binding semantics.
    AccessRelUnsupported,
    /// A marking condition names no markings.
    AccessMarkingsEmpty,
    /// A marking condition names an empty, wildcard, or non-canonical marking id.
    AccessMarkingIdInvalid,
    /// A marking condition is attached to a resource other than `norn.Marking`.
    AccessMarkingResource,
    /// A marking condition is attached to a non-marking system action.
    AccessMarkingAction,
    /// A capability grant is malformed (empty or wildcard identity, scope,
    /// or capability).
    GrantInvalid,
}

impl AuthzCode {
    /// The stable code string.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::SchemaParse => "authz.schema.parse",
            Self::SchemaInvalid => "authz.schema.invalid",
            Self::SchemaScopeType => "authz.schema.scope_type",
            Self::PolicyParse => "authz.policy.parse",
            Self::PolicyInvalid => "authz.policy.invalid",
            Self::PolicyLint => "authz.policy.lint",
            Self::AccessIdEmpty => "authz.access.id_empty",
            Self::AccessRuleDuplicate => "authz.access.rule_duplicate",
            Self::AccessGenericRuleInvalid => "authz.access.generic_rule_invalid",
            Self::AccessActionsEmpty => "authz.access.actions_empty",
            Self::AccessScopeOrgRequired => "authz.access.scope_org_required",
            Self::AccessRolesEmpty => "authz.access.roles_empty",
            Self::AccessNameInvalid => "authz.access.name_invalid",
            Self::AccessContextReserved => "authz.access.context_reserved",
            Self::AccessProjectionRequired => "authz.access.projection_required",
            Self::AccessDecimalComparison => "authz.access.decimal_comparison",
            Self::AccessPrincipalReserved => "authz.access.principal_reserved",
            Self::AccessPrincipalConflict => "authz.access.principal_attribute_conflict",
            Self::AccessPrincipalProjectionRequired => "authz.access.principal_projection_required",
            Self::AccessPrincipalOperator => "authz.access.principal_operator_invalid",
            Self::AccessResourceOperator => "authz.access.resource_operator_invalid",
            Self::AccessRelUnsupported => "authz.access.rel_unsupported",
            Self::AccessMarkingsEmpty => "authz.access.markings_empty",
            Self::AccessMarkingIdInvalid => "authz.access.marking_id_invalid",
            Self::AccessMarkingResource => "authz.access.marking_resource_invalid",
            Self::AccessMarkingAction => "authz.access.marking_action_invalid",
            Self::GrantInvalid => "authz.grant.invalid",
        }
    }
}

impl fmt::Display for AuthzCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// One admission finding.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthzDiagnostic {
    /// Stable code.
    pub code: AuthzCode,
    /// Human-readable detail; not part of the contract.
    pub message: String,
    /// The compiled Cedar policy id (`<policyId>/<ruleId>`) the finding
    /// belongs to, when it belongs to one.
    pub policy_id: Option<String>,
}

impl AuthzDiagnostic {
    pub(crate) fn new(code: AuthzCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            policy_id: None,
        }
    }

    pub(crate) fn for_policy(mut self, policy_id: impl Into<String>) -> Self {
        self.policy_id = Some(policy_id.into());
        self
    }
}

impl fmt::Display for AuthzDiagnostic {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.policy_id {
            Some(policy_id) => write!(f, "{} [{policy_id}]: {}", self.code, self.message),
            None => write!(f, "{}: {}", self.code, self.message),
        }
    }
}

/// A rejected schema, policy, or `AccessIR` document: one or more findings,
/// in a deterministic order (input order).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthzError {
    diagnostics: Vec<AuthzDiagnostic>,
}

impl AuthzError {
    pub(crate) fn new(diagnostics: Vec<AuthzDiagnostic>) -> Self {
        Self { diagnostics }
    }

    pub(crate) fn one(code: AuthzCode, message: impl Into<String>) -> Self {
        Self::new(vec![AuthzDiagnostic::new(code, message)])
    }

    /// Every finding.
    #[must_use]
    pub fn diagnostics(&self) -> &[AuthzDiagnostic] {
        &self.diagnostics
    }

    /// Whether any finding has `code`.
    #[must_use]
    pub fn has(&self, code: AuthzCode) -> bool {
        self.diagnostics.iter().any(|d| d.code == code)
    }
}

impl fmt::Display for AuthzError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut first = true;
        for diagnostic in &self.diagnostics {
            if !first {
                f.write_str("; ")?;
            }
            first = false;
            write!(f, "{diagnostic}")?;
        }
        Ok(())
    }
}

impl std::error::Error for AuthzError {}
