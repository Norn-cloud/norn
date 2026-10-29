//! T2 policy admission: parse and strictly validate compiled policies against
//! the composed schema (T5-AZ-1, #2576).
//!
//! Only policies compiled from `AccessIR` are admitted: admission is
//! crate-private, so hand-written Cedar cannot bypass the mandatory scope
//! conjunct.

use cedar_policy::{Policy, PolicyId, PolicySet, ValidationMode, Validator};

use crate::base::AuthzSchema;
use crate::diag::{AuthzCode, AuthzDiagnostic, AuthzError};

/// One compiled Cedar policy: its id and source text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CompiledPolicy {
    pub(crate) id: String,
    /// Source `AccessIR` policy identity. Kept separately because Cedar ids
    /// combine this with the rule id and the source ids need not be splittable.
    pub(crate) source_policy_id: String,
    /// Source `AccessIR` rule identity.
    pub(crate) source_rule_id: String,
    pub(crate) text: String,
    /// Validation-only policy pinning the three `scope` types to
    /// `Norn::Scope`; never admitted.
    pub(crate) scope_probe: String,
    /// Whether this generated policy reads the PDP-owned relationship signal.
    pub(crate) uses_entitlements: bool,
}

/// A policy set that parsed and passed strict Cedar validation against an
/// [`AuthzSchema`] with no errors and no warnings.
///
/// This is the input the kernel PDP (T5-AZ-3, #2578) evaluates; the schema it
/// was validated against travels with it so request validation uses the same
/// schema. Policies keep compile order.
#[derive(Debug, Clone)]
pub struct ValidatedPolicySet {
    policies: Vec<CompiledPolicy>,
    set: PolicySet,
    schema: AuthzSchema,
    uses_entitlements: bool,
}

impl ValidatedPolicySet {
    /// Number of policies.
    #[must_use]
    pub fn len(&self) -> usize {
        self.policies.len()
    }

    /// Whether the set is empty. An empty set denies every request.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.policies.is_empty()
    }

    /// Policy ids in compile order.
    pub fn policy_ids(&self) -> impl Iterator<Item = &str> {
        self.policies.iter().map(|policy| policy.id.as_str())
    }

    /// Source `AccessIR` identities for one exact Cedar policy id.
    pub(crate) fn source_ids(&self, cedar_policy_id: &str) -> Option<(&str, &str)> {
        self.policies
            .iter()
            .find(|policy| policy.id.as_str() == cedar_policy_id)
            .map(|policy| {
                (
                    policy.source_policy_id.as_str(),
                    policy.source_rule_id.as_str(),
                )
            })
    }

    /// The whole set as Cedar source text, in compile order, one blank line
    /// between policies. Deterministic; used for goldens and receipts.
    #[must_use]
    pub fn to_cedar(&self) -> String {
        let texts: Vec<&str> = self
            .policies
            .iter()
            .map(|policy| policy.text.as_str())
            .collect();
        texts.join("\n")
    }

    pub(crate) fn cedar_policies(&self) -> &PolicySet {
        &self.set
    }

    pub(crate) fn schema(&self) -> &AuthzSchema {
        &self.schema
    }

    pub(crate) const fn uses_entitlements(&self) -> bool {
        self.uses_entitlements
    }
}

/// Parse every compiled policy, then validate the set in strict mode. Every
/// finding is collected; any finding rejects the whole set.
pub(crate) fn admit(
    schema: &AuthzSchema,
    policies: Vec<CompiledPolicy>,
) -> Result<ValidatedPolicySet, AuthzError> {
    let mut diagnostics = Vec::new();
    let mut set = PolicySet::new();
    for policy in &policies {
        match Policy::parse(Some(PolicyId::new(&policy.id)), &policy.text) {
            Ok(parsed) => {
                if let Err(error) = set.add(parsed) {
                    diagnostics.push(
                        AuthzDiagnostic::new(AuthzCode::AccessRuleDuplicate, error.to_string())
                            .for_policy(&policy.id),
                    );
                }
            }
            Err(error) => diagnostics.push(
                AuthzDiagnostic::new(AuthzCode::PolicyParse, error.to_string())
                    .for_policy(&policy.id),
            ),
        }
    }
    if !diagnostics.is_empty() {
        return Err(AuthzError::new(diagnostics));
    }

    let result = Validator::new(schema.cedar().clone()).validate(&set, ValidationMode::Strict);
    for error in result.validation_errors() {
        diagnostics.push(
            AuthzDiagnostic::new(AuthzCode::PolicyInvalid, error.to_string())
                .for_policy(error.policy_id().to_string()),
        );
    }
    for warning in result.validation_warnings() {
        diagnostics.push(
            AuthzDiagnostic::new(AuthzCode::PolicyLint, warning.to_string())
                .for_policy(warning.policy_id().to_string()),
        );
    }
    if !diagnostics.is_empty() {
        sort_diagnostics(&mut diagnostics);
        return Err(AuthzError::new(diagnostics));
    }

    // The scope conjunct is an exact record comparison; it only means
    // "same org/tenant/..." when all three sides are `Norn::Scope`. A
    // projection that types `scope` otherwise (say `String`) is rejected here.
    let mut probes = PolicySet::new();
    for policy in &policies {
        match Policy::parse(Some(PolicyId::new(&policy.id)), &policy.scope_probe) {
            Ok(parsed) => {
                if let Err(error) = probes.add(parsed) {
                    diagnostics.push(
                        AuthzDiagnostic::new(AuthzCode::AccessRuleDuplicate, error.to_string())
                            .for_policy(&policy.id),
                    );
                }
            }
            Err(error) => diagnostics.push(
                AuthzDiagnostic::new(AuthzCode::PolicyParse, error.to_string())
                    .for_policy(&policy.id),
            ),
        }
    }
    let probe_result =
        Validator::new(schema.cedar().clone()).validate(&probes, ValidationMode::Strict);
    for error in probe_result.validation_errors() {
        diagnostics.push(
            AuthzDiagnostic::new(AuthzCode::SchemaScopeType, error.to_string())
                .for_policy(error.policy_id().to_string()),
        );
    }
    if !diagnostics.is_empty() {
        sort_diagnostics(&mut diagnostics);
        return Err(AuthzError::new(diagnostics));
    }

    let uses_entitlements = policies.iter().any(|policy| policy.uses_entitlements);
    Ok(ValidatedPolicySet {
        policies,
        set,
        schema: schema.clone(),
        uses_entitlements,
    })
}

/// The validator reports per policy in no promised order; sort so a rejection
/// is deterministic.
fn sort_diagnostics(diagnostics: &mut [AuthzDiagnostic]) {
    diagnostics.sort_by(|left, right| {
        (left.policy_id.as_deref(), left.code, left.message.as_str()).cmp(&(
            right.policy_id.as_deref(),
            right.code,
            right.message.as_str(),
        ))
    });
}
