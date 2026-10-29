//! The Cedar PDP and the AND rule (T5-AZ-3, #2578; ruling 2026-08-27 §7).
//!
//! A request is allowed only when **all** of these hold:
//!
//! 1. the request is well formed and carries an explicit scope;
//! 2. **capability**: some scoped capability grant matches exactly
//!    ([`crate::CapabilityGrant`], evaluated by Cedar over the grant set);
//! 3. **delegation (Biscuit slot)**: the principal acts directly; any
//!    presented token denies until Biscuit verification lands;
//! 4. **access**: the admitted `AccessIR` policy set permits the request.
//!
//! Every error (malformed input, schema mismatch, evaluation error) denies.
//! Cedar itself is default-deny with forbid-overrides-permit. The kernel calls
//! this PDP on every request (`norn_kernel::authz`); the gateway is never the
//! trust boundary.

use std::collections::{BTreeMap, HashMap, HashSet};

use crate::entitlement::{EntitlementEvaluation, EntitlementMatches, evaluate_entitlements};
use cedar_policy::{
    Authorizer, Context, Decision, Entities, Entity, EntityUid, Request, Response,
    RestrictedExpression,
};

use crate::admit::ValidatedPolicySet;
use crate::diag::AuthzError;
use crate::grants::{CapabilityQuery, CompiledGrants, scope_record, uid};
use crate::lower::{ACTION_TYPE, ACTOR_TYPE, ROLE_TYPE, SCOPE_ATTRIBUTE, cedar_type_name};
use crate::lower::{DECLASSIFY_ACTION_ID, MARKING_RESOURCE_TYPE, REVEAL_ACTION_ID};
use crate::request::{
    AccessRequest, AttrValue, CapabilityGrant, Delegation, EntitlementInput, EntityRef,
    ResourceInput, ScopeKey, is_exact_value,
};

/// Why the PDP denied. The decision is the contract; the reason is for audit.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum DenyReason {
    /// Malformed request (empty or wildcard identity, reserved attribute).
    InvalidRequest,
    /// No request scope.
    AbsentScope,
    /// No capability grant matches.
    Capability,
    /// A delegated token was presented (unsupported until Biscuit lands).
    Delegation,
    /// The `AccessIR` policies do not permit the request.
    Access,
    /// The request could not be evaluated against the access schema (for
    /// example an entity that does not conform), or evaluation errored.
    AccessError,
}

impl DenyReason {
    /// Stable code.
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::InvalidRequest => "authz.deny.invalid_request",
            Self::AbsentScope => "authz.deny.absent_scope",
            Self::Capability => "authz.deny.capability",
            Self::Delegation => "authz.deny.delegation",
            Self::Access => "authz.deny.access",
            Self::AccessError => "authz.deny.access_error",
        }
    }
}

/// The PDP's decision.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PdpDecision {
    /// Every conjunct of the AND rule holds.
    Allow,
    /// At least one conjunct fails (the first failing one is named).
    Deny(DenyReason),
}

impl PdpDecision {
    /// Whether the request is allowed.
    #[must_use]
    pub const fn is_allow(self) -> bool {
        matches!(self, Self::Allow)
    }
}

/// Outcome of one conjunct in the PDP's ordered authorization path.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ConjunctOutcome {
    /// The conjunct passed.
    Allow,
    /// The conjunct denied the request.
    Deny,
    /// The conjunct could not be evaluated safely.
    Error,
    /// The production path stopped before evaluating this conjunct.
    NotEvaluated,
}

/// An `AccessIR` policy that Cedar reports as contributing to its decision.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeterminingPolicy {
    /// Exact Cedar policy id used by the admitted policy set.
    pub cedar_policy_id: String,
    /// Source `AccessIR` policy id, when the Cedar id maps to a compiled rule.
    pub policy_id: Option<String>,
    /// Source `AccessIR` rule id, when the Cedar id maps to a compiled rule.
    pub rule_id: Option<String>,
}

/// Decision and diagnostics from one evaluation of the production PDP path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PdpExplanation {
    /// The same fail-closed decision returned by [`Pdp::decide`].
    pub decision: PdpDecision,
    /// Request shape and explicit-scope admission.
    pub scope: ConjunctOutcome,
    /// Scoped capability-grant conjunct.
    pub capability: ConjunctOutcome,
    /// Direct/delegated-principal envelope.
    pub delegation: ConjunctOutcome,
    /// Admitted AccessIR/Cedar conjunct.
    pub access: ConjunctOutcome,
    /// Cedar policies that contributed to the access decision.
    pub determining_policies: Vec<DeterminingPolicy>,
    /// Cedar evaluation errors or request-lowering diagnostics.
    pub diagnostics: Vec<String>,
    /// Typed entitlement evidence and shadow-mode comparison.
    pub entitlement_evaluation: EntitlementEvaluation,
}

/// The policy decision point: admitted `AccessIR` policies plus the
/// compiled capability grants of one authorization epoch.
#[derive(Debug, Clone)]
pub struct Pdp {
    access: ValidatedPolicySet,
    grants: CompiledGrants,
    entitlements: Vec<EntitlementInput>,
}

impl Pdp {
    /// Build a PDP from an admitted policy set and the current grants.
    ///
    /// # Errors
    ///
    /// `authz.grant.invalid` for a malformed grant (empty or wildcard
    /// subject, scope dimension, session, or capability), or a schema or
    /// policy code if a grant cannot be compiled. The caller must deny while
    /// it has no PDP.
    pub fn new(access: ValidatedPolicySet, grants: &[CapabilityGrant]) -> Result<Self, AuthzError> {
        Ok(Self {
            access,
            grants: CompiledGrants::compile(grants)?,
            entitlements: Vec::new(),
        })
    }

    /// The admitted policy set, retained when refreshing capability grants.
    #[must_use]
    pub const fn policy_set(&self) -> &ValidatedPolicySet {
        &self.access
    }

    /// Attach a trusted immutable entitlement snapshot to this PDP. A later
    /// per-call snapshot passed to `decide_with_entitlements` or
    /// `explain_with_entitlements` replaces these rows for that evaluation.
    #[must_use]
    pub fn with_entitlements(mut self, entitlements: Vec<EntitlementInput>) -> Self {
        self.entitlements = entitlements;
        self
    }

    /// Decide one request under the AND rule. Never errors: anything that
    /// cannot be evaluated is a denial.
    #[must_use]
    pub fn decide(&self, request: &AccessRequest) -> PdpDecision {
        self.explain_internal(request, &self.entitlements, false)
            .decision
    }

    /// Decide with one trusted immutable entitlement snapshot.
    #[must_use]
    pub fn decide_with_entitlements(
        &self,
        request: &AccessRequest,
        entitlements: &[EntitlementInput],
    ) -> PdpDecision {
        self.explain_internal(request, entitlements, false).decision
    }

    /// Explain one request using the same ordered, fail-closed path as
    /// [`Pdp::decide`]. Later conjuncts remain [`ConjunctOutcome::NotEvaluated`]
    /// after an earlier conjunct denies, matching production short-circuiting.
    #[must_use]
    pub fn explain(&self, request: &AccessRequest) -> PdpExplanation {
        self.explain_with_entitlements(request, &self.entitlements)
    }

    /// Explain one request against a trusted immutable entitlement snapshot.
    /// The supplied snapshot replaces the optional builder snapshot.
    #[must_use]
    pub fn explain_with_entitlements(
        &self,
        request: &AccessRequest,
        entitlements: &[EntitlementInput],
    ) -> PdpExplanation {
        self.explain_internal(request, entitlements, true)
    }

    fn explain_internal(
        &self,
        request: &AccessRequest,
        entitlements: &[EntitlementInput],
        explain_shadow: bool,
    ) -> PdpExplanation {
        let mut explanation = PdpExplanation {
            decision: PdpDecision::Deny(DenyReason::InvalidRequest),
            scope: ConjunctOutcome::NotEvaluated,
            capability: ConjunctOutcome::NotEvaluated,
            delegation: ConjunctOutcome::NotEvaluated,
            access: ConjunctOutcome::NotEvaluated,
            determining_policies: Vec::new(),
            diagnostics: Vec::new(),
            entitlement_evaluation: EntitlementEvaluation::not_evaluated(),
        };

        if !is_well_formed(request) {
            explanation.scope = ConjunctOutcome::Error;
            explanation.decision = PdpDecision::Deny(DenyReason::InvalidRequest);
            return explanation;
        }
        explanation.scope = ConjunctOutcome::Allow;
        let Some(scope) = &request.scope else {
            explanation.scope = ConjunctOutcome::Deny;
            explanation.decision = PdpDecision::Deny(DenyReason::AbsentScope);
            return explanation;
        };
        let entitlement_matches =
            evaluate_entitlements(request, scope, entitlements, explain_shadow);
        explanation.entitlement_evaluation = entitlement_matches.evaluation.clone();
        let principal = &request.principal;
        let capability = CapabilityQuery {
            subject_id: &principal.subject_id,
            actor_kind: principal.actor_kind.as_str(),
            agent_session_id: principal.agent_session_id.as_deref(),
            scope,
            capability: &request.capability,
            binding: request.binding.as_ref(),
        };
        if !self.grants.allows(&capability) {
            explanation.capability = ConjunctOutcome::Deny;
            explanation.decision = PdpDecision::Deny(DenyReason::Capability);
            explanation.entitlement_evaluation.outcome = ConjunctOutcome::NotEvaluated;
            set_shadow_counterfactual(&mut explanation, false, &entitlement_matches);
            return explanation;
        }
        explanation.capability = ConjunctOutcome::Allow;
        if request.delegation != Delegation::Direct {
            explanation.delegation = ConjunctOutcome::Deny;
            explanation.decision = PdpDecision::Deny(DenyReason::Delegation);
            explanation.entitlement_evaluation.outcome = ConjunctOutcome::NotEvaluated;
            set_shadow_counterfactual(&mut explanation, false, &entitlement_matches);
            return explanation;
        }
        explanation.delegation = ConjunctOutcome::Allow;

        let access = self.access_evaluation(request, scope, &entitlement_matches.enforced_kinds);
        explanation.access = access.outcome;
        explanation.determining_policies = access.determining_policies;
        explanation.diagnostics = access.diagnostics;
        explanation.decision = match explanation.access {
            ConjunctOutcome::Allow => PdpDecision::Allow,
            ConjunctOutcome::Deny => PdpDecision::Deny(DenyReason::Access),
            ConjunctOutcome::Error | ConjunctOutcome::NotEvaluated => {
                PdpDecision::Deny(DenyReason::AccessError)
            }
        };
        if entitlement_matches.has_relevant_shadow {
            let counterfactual =
                self.access_evaluation(request, scope, &entitlement_matches.all_kinds);
            explanation.entitlement_evaluation.would_allow_if_enforced = Some(
                explanation.access != ConjunctOutcome::Error
                    && counterfactual.outcome == ConjunctOutcome::Allow,
            );
        }
        explanation
    }

    /// Explain whether `request`'s principal may use a marking action for one
    /// exact marking id. The ordinary PDP path evaluates the same scoped
    /// capability and `AccessIR` conjuncts against a synthetic `norn.Marking`
    /// resource, so marking checks retain the production fail-closed behavior.
    #[must_use]
    pub fn explain_marking(
        &self,
        request: &AccessRequest,
        marking_id: &str,
        action_id: &str,
    ) -> PdpExplanation {
        let mut marking_request = request.clone();
        let valid_action = matches!(action_id, REVEAL_ACTION_ID | DECLASSIFY_ACTION_ID);
        if !valid_action || !crate::lower::canonical_marking_id(marking_id) {
            // Reuse the PDP's stable malformed-request denial. This helper is
            // an allow-list boundary as well as a convenience constructor.
            marking_request.capability.clear();
            return self.explain(&marking_request);
        }

        action_id.clone_into(&mut marking_request.capability);
        action_id.clone_into(&mut marking_request.action_id);
        marking_request.resource = ResourceInput {
            object_type: MARKING_RESOURCE_TYPE.to_owned(),
            id: marking_id.to_owned(),
            scope: request.resource.scope.clone(),
            attributes: BTreeMap::from([(
                "id".to_owned(),
                AttrValue::String(marking_id.to_owned()),
            )]),
        };
        marking_request.context.clear();
        marking_request.binding = None;
        self.explain(&marking_request)
    }

    /// Decide one exact marking action using the same scoped PDP path as
    /// [`Self::explain_marking`].
    #[must_use]
    pub fn decide_marking(
        &self,
        request: &AccessRequest,
        marking_id: &str,
        action_id: &str,
    ) -> PdpDecision {
        self.explain_marking(request, marking_id, action_id)
            .decision
    }

    fn access_evaluation(
        &self,
        request: &AccessRequest,
        scope: &ScopeKey,
        entitlement_kinds: &std::collections::BTreeSet<&'static str>,
    ) -> AccessEvaluation {
        let response = match self.cedar_response(request, scope, entitlement_kinds) {
            Ok(response) => response,
            Err(diagnostic) => {
                return AccessEvaluation {
                    outcome: ConjunctOutcome::Error,
                    determining_policies: Vec::new(),
                    diagnostics: vec![diagnostic],
                };
            }
        };

        let mut diagnostics: Vec<_> = response
            .diagnostics()
            .errors()
            .map(ToString::to_string)
            .collect();
        diagnostics.sort();

        let mut determining_policies: Vec<_> = response
            .diagnostics()
            .reason()
            .map(|policy_id| {
                let cedar_policy_id: &str = policy_id.as_ref();
                let source_ids = self.access.source_ids(cedar_policy_id);
                DeterminingPolicy {
                    cedar_policy_id: cedar_policy_id.to_owned(),
                    policy_id: source_ids.map(|(policy_id, _)| policy_id.to_owned()),
                    rule_id: source_ids.map(|(_, rule_id)| rule_id.to_owned()),
                }
            })
            .collect();
        determining_policies
            .sort_by(|left, right| left.cedar_policy_id.cmp(&right.cedar_policy_id));

        let outcome = if diagnostics.is_empty() {
            match response.decision() {
                Decision::Allow => ConjunctOutcome::Allow,
                Decision::Deny => ConjunctOutcome::Deny,
            }
        } else {
            ConjunctOutcome::Error
        };

        AccessEvaluation {
            outcome,
            determining_policies,
            diagnostics,
        }
    }

    fn cedar_response(
        &self,
        request: &AccessRequest,
        scope: &ScopeKey,
        entitlement_kinds: &std::collections::BTreeSet<&'static str>,
    ) -> Result<Response, String> {
        let schema = self.access.schema().cedar();
        let principal_uid = uid(ACTOR_TYPE, &request.principal.subject_id)
            .ok_or_else(|| "principal could not be lowered to a Cedar entity id".to_owned())?;
        let mut role_entities = Vec::with_capacity(request.principal.roles.len());
        let mut parents = HashSet::new();
        let roles: std::collections::BTreeSet<&str> =
            request.principal.roles.iter().map(String::as_str).collect();
        for role in roles {
            let role_uid = uid(ROLE_TYPE, role)
                .ok_or_else(|| "role could not be lowered to a Cedar entity id".to_owned())?;
            role_entities.push(Entity::new_no_attrs(role_uid.clone(), HashSet::new()));
            parents.insert(role_uid);
        }
        let mut principal_input_attributes = request.principal.attributes.clone();
        // Verified membership keys are inputs to the typed entitlement
        // matcher, not Cedar entity attributes; keeping them out also
        // preserves old projected schemas without generic rules.
        principal_input_attributes.remove("entitlement_roles");
        principal_input_attributes.remove("entitlement_groups");
        if self.access.uses_entitlements() {
            principal_input_attributes.insert(
                "entitlement_kinds".to_owned(),
                AttrValue::Set(
                    entitlement_kinds
                        .iter()
                        .map(|kind| AttrValue::String((*kind).to_owned()))
                        .collect(),
                ),
            );
        }
        let principal_attributes = attributes(scope, &principal_input_attributes)
            .ok_or_else(|| "principal attributes could not be lowered to Cedar".to_owned())?;
        let principal = Entity::new(principal_uid.clone(), principal_attributes, parents)
            .map_err(|error| error.to_string())?;

        let mut type_diagnostics = Vec::new();
        let resource_type = cedar_type_name(&request.resource.object_type, &mut type_diagnostics)
            .ok_or_else(|| {
            type_diagnostics
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join("; ")
        })?;
        let resource_uid = uid(&resource_type, &request.resource.id)
            .ok_or_else(|| "resource could not be lowered to a Cedar entity id".to_owned())?;
        let resource_attributes = attributes(&request.resource.scope, &request.resource.attributes)
            .ok_or_else(|| "resource attributes could not be lowered to Cedar".to_owned())?;
        let resource = Entity::new(resource_uid.clone(), resource_attributes, HashSet::new())
            .map_err(|error| error.to_string())?;

        let mut entities = vec![principal, resource];
        entities.extend(role_entities);
        let entities =
            Entities::from_entities(entities, Some(schema)).map_err(|error| error.to_string())?;

        let scope_value = scope_record(scope)
            .ok_or_else(|| "request scope could not be lowered to Cedar".to_owned())?;
        let mut pairs = vec![(SCOPE_ATTRIBUTE.to_owned(), scope_value)];
        for (name, value) in &request.context {
            pairs.push((
                name.clone(),
                restricted(value).ok_or_else(|| {
                    format!("context value '{name}' could not be lowered to Cedar")
                })?,
            ));
        }
        let context = Context::from_pairs(pairs).map_err(|error| error.to_string())?;
        let action = uid(ACTION_TYPE, &request.action_id)
            .ok_or_else(|| "action could not be lowered to a Cedar entity id".to_owned())?;
        let cedar_request =
            Request::new(principal_uid, action, resource_uid, context, Some(schema))
                .map_err(|error| error.to_string())?;

        Ok(
            Authorizer::new().is_authorized(
                &cedar_request,
                self.access.cedar_policies(),
                &entities,
            ),
        )
    }
}

fn set_shadow_counterfactual(
    explanation: &mut PdpExplanation,
    outer_gates_allow: bool,
    entitlement_matches: &EntitlementMatches,
) {
    if entitlement_matches.has_relevant_shadow {
        explanation.entitlement_evaluation.would_allow_if_enforced = Some(outer_gates_allow);
    }
}

struct AccessEvaluation {
    outcome: ConjunctOutcome,
    determining_policies: Vec<DeterminingPolicy>,
    diagnostics: Vec<String>,
}

/// The checks `norn-engine` applies before its decision, the reserved `scope`
/// attribute, and target consistency: a commit binding must name the same
/// action and resource the access conjunct evaluates, so a grant bound to one
/// target can never authorize another.
fn is_well_formed(request: &AccessRequest) -> bool {
    let principal = &request.principal;
    !principal.subject_id.trim().is_empty()
        && principal
            .agent_session_id
            .as_deref()
            .is_none_or(is_exact_value)
        && !request.capability.trim().is_empty()
        && !request.action_id.trim().is_empty()
        && request.scope.as_ref().is_none_or(ScopeKey::is_exact)
        && request.resource.scope.is_exact()
        && !principal.attributes.contains_key(SCOPE_ATTRIBUTE)
        && !principal.attributes.contains_key("entitlement_kinds")
        && !request.resource.attributes.contains_key(SCOPE_ATTRIBUTE)
        && !request.context.contains_key(SCOPE_ATTRIBUTE)
        && request.binding.as_ref().is_none_or(|binding| {
            binding.action_id == request.action_id
                && binding.resource_kind == request.resource.object_type
                && binding.resource_id == request.resource.id
        })
}

fn attributes(
    scope: &ScopeKey,
    values: &BTreeMap<String, AttrValue>,
) -> Option<HashMap<String, RestrictedExpression>> {
    let mut out = HashMap::with_capacity(values.len());
    out.insert(SCOPE_ATTRIBUTE.to_owned(), scope_record(scope)?);
    for (name, value) in values {
        out.insert(name.clone(), restricted(value)?);
    }
    Some(out)
}

fn restricted(value: &AttrValue) -> Option<RestrictedExpression> {
    Some(match value {
        AttrValue::Bool(value) => RestrictedExpression::new_bool(*value),
        AttrValue::Long(value) => RestrictedExpression::new_long(*value),
        AttrValue::String(value) => RestrictedExpression::new_string(value.clone()),
        AttrValue::Entity(reference) => {
            RestrictedExpression::new_entity_uid(entity_ref(reference)?)
        }
        AttrValue::Set(items) => {
            RestrictedExpression::new_set(items.iter().map(restricted).collect::<Option<Vec<_>>>()?)
        }
        AttrValue::Record(fields) => RestrictedExpression::new_record(
            fields
                .iter()
                .map(|(name, value)| restricted(value).map(|value| (name.clone(), value)))
                .collect::<Option<Vec<_>>>()?,
        )
        .ok()?,
    })
}

fn entity_ref(reference: &EntityRef) -> Option<EntityUid> {
    match reference {
        EntityRef::Actor(id) => uid(ACTOR_TYPE, id),
        EntityRef::Object { object_type, id } => {
            let mut diagnostics = Vec::new();
            uid(&cedar_type_name(object_type, &mut diagnostics)?, id)
        }
    }
}
