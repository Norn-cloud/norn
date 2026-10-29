//! The capability conjunct of the AND rule, evaluated by Cedar (T5-AZ-3,
//! #2578).
//!
//! Each [`CapabilityGrant`] lowers to one Cedar `permit` over a fixed,
//! Norn-owned capability schema ([`CAPABILITY_SCHEMA`]). The policy compares
//! every field exactly, so the semantics are those of `norn-engine`'s
//! hand-rolled `authz_decision` (parity: `crates/norn-api/tests/`): subject,
//! actor kind, agent session, all five scope dimensions (`None` matches only
//! `None`), optional commit binding, and capability membership. Grants are
//! data; they never reach the `AccessIR` policy set.

use cedar_policy::{
    Authorizer, Context, Decision, Entities, Entity, EntityId, EntityTypeName, EntityUid, Policy,
    PolicyId, PolicySet, Request, RestrictedExpression, Schema, ValidationMode, Validator,
};
use std::collections::{HashMap, HashSet};
use std::str::FromStr;

use crate::diag::{AuthzCode, AuthzDiagnostic, AuthzError};
use crate::lower::string_literal;
use crate::request::{CapabilityGrant, CommitBinding, ScopeKey, is_exact_value};

/// The capability schema (Cedar schema syntax). Binding numbers are carried
/// as decimal strings so every `u64` compares exactly.
pub const CAPABILITY_SCHEMA: &str = "namespace NornCap {
  type Scope = {
    org: String,
    tenant?: String,
    workspace?: String,
    environment?: String,
    businessUnit?: String
  };
  type Binding = {
    action: String,
    resourceKind: String,
    resourceId: String,
    artifactVersion?: String,
    policyId: String,
    policyVersion: String,
    resourceVersion: String,
    epoch: String,
    evaluatedAt: String
  };
  entity Subject = {
    kind: String,
    session?: String
  };
  entity Request;
  action \"use\" appliesTo {
    principal: [Subject],
    resource: [Request],
    context: {
      scope: Scope,
      capability: String,
      binding?: Binding
    }
  };
}
";

const SUBJECT_TYPE: &str = "NornCap::Subject";
const REQUEST_TYPE: &str = "NornCap::Request";
const ACTION_TYPE: &str = "NornCap::Action";
const ACTION_ID: &str = "use";
const REQUEST_ID: &str = "request";

/// A validated, compiled grant set.
#[derive(Debug, Clone)]
pub(crate) struct CompiledGrants {
    schema: Schema,
    policies: PolicySet,
}

/// The capability stage's view of one request.
pub(crate) struct CapabilityQuery<'a> {
    pub(crate) subject_id: &'a str,
    pub(crate) actor_kind: &'a str,
    pub(crate) agent_session_id: Option<&'a str>,
    pub(crate) scope: &'a ScopeKey,
    pub(crate) capability: &'a str,
    pub(crate) binding: Option<&'a CommitBinding>,
}

impl CompiledGrants {
    /// Validate and compile `grants` (rules of `norn-engine`'s
    /// `validate_capability_grants`).
    pub(crate) fn compile(grants: &[CapabilityGrant]) -> Result<Self, AuthzError> {
        let (schema, warnings) =
            Schema::from_cedarschema_str(CAPABILITY_SCHEMA).map_err(|error| {
                AuthzError::one(
                    AuthzCode::SchemaParse,
                    format!("capability schema: {error}"),
                )
            })?;
        if warnings.count() != 0 {
            return Err(AuthzError::one(
                AuthzCode::SchemaInvalid,
                "capability schema has warnings",
            ));
        }

        let mut diagnostics = Vec::new();
        let mut policies = PolicySet::new();
        for (index, grant) in grants.iter().enumerate() {
            let id = format!("grant{index}");
            let mut grant_diagnostics = validate_grant(grant);
            if grant_diagnostics.is_empty() {
                // A grant without a scope never matches; it compiles to nothing.
                let Some(text) = lower_grant(grant) else {
                    continue;
                };
                match Policy::parse(Some(PolicyId::new(&id)), &text) {
                    Ok(policy) => {
                        if let Err(error) = policies.add(policy) {
                            grant_diagnostics.push(AuthzDiagnostic::new(
                                AuthzCode::PolicyParse,
                                error.to_string(),
                            ));
                        }
                    }
                    Err(error) => grant_diagnostics.push(AuthzDiagnostic::new(
                        AuthzCode::PolicyParse,
                        error.to_string(),
                    )),
                }
            }
            diagnostics.extend(
                grant_diagnostics
                    .into_iter()
                    .map(|diagnostic| diagnostic.for_policy(id.as_str())),
            );
        }
        if diagnostics.is_empty() {
            let result = Validator::new(schema.clone()).validate(&policies, ValidationMode::Strict);
            // Only type errors matter here: grant strings are data, so
            // confusable-string lints are not a reason to drop a grant.
            for error in result.validation_errors() {
                diagnostics.push(
                    AuthzDiagnostic::new(AuthzCode::PolicyInvalid, error.to_string())
                        .for_policy(error.policy_id().to_string()),
                );
            }
        }
        if diagnostics.is_empty() {
            Ok(Self { schema, policies })
        } else {
            Err(AuthzError::new(diagnostics))
        }
    }

    /// Whether some grant matches. Any construction or evaluation error is
    /// `false` (fail closed).
    pub(crate) fn allows(&self, query: &CapabilityQuery<'_>) -> bool {
        self.evaluate(query).unwrap_or(false)
    }

    fn evaluate(&self, query: &CapabilityQuery<'_>) -> Option<bool> {
        let subject = uid(SUBJECT_TYPE, query.subject_id)?;
        let mut attrs = HashMap::from([(
            "kind".to_owned(),
            RestrictedExpression::new_string(query.actor_kind.to_owned()),
        )]);
        if let Some(session) = query.agent_session_id {
            attrs.insert(
                "session".to_owned(),
                RestrictedExpression::new_string(session.to_owned()),
            );
        }
        let subject_entity = Entity::new(subject.clone(), attrs, HashSet::new()).ok()?;
        let request_uid = uid(REQUEST_TYPE, REQUEST_ID)?;
        let entities = Entities::from_entities(
            [
                subject_entity,
                Entity::new_no_attrs(request_uid.clone(), HashSet::new()),
            ],
            Some(&self.schema),
        )
        .ok()?;

        let mut pairs = vec![
            ("scope".to_owned(), scope_record(query.scope)?),
            (
                "capability".to_owned(),
                RestrictedExpression::new_string(query.capability.to_owned()),
            ),
        ];
        if let Some(binding) = query.binding {
            pairs.push(("binding".to_owned(), binding_record(binding)?));
        }
        let context = Context::from_pairs(pairs).ok()?;
        let action = uid(ACTION_TYPE, ACTION_ID)?;
        let request =
            Request::new(subject, action, request_uid, context, Some(&self.schema)).ok()?;
        let response = Authorizer::new().is_authorized(&request, &self.policies, &entities);
        Some(
            response.decision() == Decision::Allow
                && response.diagnostics().errors().next().is_none(),
        )
    }
}

pub(crate) fn uid(entity_type: &str, id: &str) -> Option<EntityUid> {
    let type_name = EntityTypeName::from_str(entity_type).ok()?;
    Some(EntityUid::from_type_name_and_id(
        type_name,
        EntityId::new(id),
    ))
}

/// A `Norn::Scope`-shaped record of the present dimensions.
pub(crate) fn scope_record(scope: &ScopeKey) -> Option<RestrictedExpression> {
    RestrictedExpression::new_record(scope.dimensions().into_iter().map(|(name, value)| {
        (
            name.to_owned(),
            RestrictedExpression::new_string(value.to_owned()),
        )
    }))
    .ok()
}

fn binding_record(binding: &CommitBinding) -> Option<RestrictedExpression> {
    let mut fields: Vec<(String, RestrictedExpression)> = binding
        .required()
        .into_iter()
        .map(|(name, value)| {
            (
                name.to_owned(),
                RestrictedExpression::new_string(value.to_owned()),
            )
        })
        .collect();
    if let Some(version) = &binding.artifact_version {
        fields.push((
            "artifactVersion".to_owned(),
            RestrictedExpression::new_string(version.clone()),
        ));
    }
    RestrictedExpression::new_record(fields).ok()
}

fn validate_grant(grant: &CapabilityGrant) -> Vec<AuthzDiagnostic> {
    let mut diagnostics = Vec::new();
    let mut invalid = |message: &str| {
        diagnostics.push(AuthzDiagnostic::new(AuthzCode::GrantInvalid, message));
    };
    if !is_exact_value(&grant.subject_id) {
        invalid("subject_id must be one exact, non-wildcard value");
    }
    if grant
        .agent_session_id
        .as_deref()
        .is_some_and(|session| !is_exact_value(session))
    {
        invalid("agent_session_id must be omitted or one exact, non-wildcard value");
    }
    if let Some(scope) = &grant.scope
        && !scope.is_exact()
    {
        invalid("scope dimensions must be omitted or one exact, non-wildcard value");
    }
    if grant.capabilities.is_empty() {
        invalid("a grant must declare at least one capability");
    }
    if grant
        .capabilities
        .iter()
        .any(|capability| capability.trim().is_empty())
    {
        invalid("a grant contains an empty capability");
    }
    diagnostics
}

/// One `permit` per grant; `None` for a scope-less grant (never matches).
fn lower_grant(grant: &CapabilityGrant) -> Option<String> {
    let scope = grant.scope.as_ref()?;
    let subject = uid(SUBJECT_TYPE, &grant.subject_id)?;
    let action = uid(ACTION_TYPE, ACTION_ID)?;
    let mut conjuncts = vec![format!(
        "principal.kind == {}",
        string_literal(grant.actor_kind.as_str())
    )];
    conjuncts.push(optional_equals(
        "principal",
        "session",
        grant.agent_session_id.as_deref(),
    ));
    conjuncts.push(format!(
        "context.scope.org == {}",
        string_literal(&scope.org)
    ));
    for (name, value) in scope.optional_dimensions() {
        conjuncts.push(optional_equals("context.scope", name, value));
    }
    let capabilities: Vec<String> = grant
        .capabilities
        .iter()
        .map(|capability| string_literal(capability.as_str()))
        .collect();
    conjuncts.push(format!(
        "[{}].contains(context.capability)",
        capabilities.join(", ")
    ));
    match &grant.binding {
        None => conjuncts.push("!(context has binding)".to_owned()),
        Some(binding) => {
            conjuncts.push("context has binding".to_owned());
            for (name, value) in binding.required() {
                conjuncts.push(format!(
                    "context.binding.{name} == {}",
                    string_literal(value)
                ));
            }
            conjuncts.push(optional_equals(
                "context.binding",
                "artifactVersion",
                binding.artifact_version.as_deref(),
            ));
        }
    }
    Some(format!(
        "permit (\n  principal == {subject},\n  action == {action},\n  resource\n)\nwhen {{\n  {}\n}};\n",
        conjuncts.join(" &&\n  ")
    ))
}

/// Exact optional member: present and equal, or absent on both sides.
fn optional_equals(record: &str, name: &str, value: Option<&str>) -> String {
    match value {
        Some(value) => format!(
            "({record} has {name} && {record}.{name} == {})",
            string_literal(value)
        ),
        None => format!("!({record} has {name})"),
    }
}
