//! HOST-ACTION-2 (#1252): governed action calls decided by the scope's own
//! policy, through the kernel PDP.
//!
//! The policy is data in the cell's active definitions: the `policy`
//! artifacts (the `AccessIR` form) that the cell's actions name in their
//! `access` ref. [`ActionPolicy::compile`] lowers them to Cedar against the
//! schema projected from the cell's model and actions. Nothing here names a
//! pack, tenant or action.
//!
//! - An action is callable only when its `access` ref names a policy in the
//!   set that is in the `AccessIR` form. An action without one is uncallable.
//! - Each referenced policy grants only the actions that name it: a rule's
//!   action list is narrowed to those actions, so a policy cannot grant an
//!   action that is governed by another one.
//! - A set whose referenced policies do not compile yields no policy, and
//!   then the cell serves no action. It never falls back to allowing one.
//! - The PDP's capability conjunct grants the verified caller the call
//!   capability for exactly the callable actions, in this cell's scope; the
//!   tenant's policy (its role, scope and other conditions) is the deciding
//!   conjunct. The caller's Norn roles are the principal's roles there.
//! - ACTION-RESOURCE-1: the rules compile against the projected schema, so a
//!   `resource` condition decides. The resource's attributes are the values
//!   the kernel supplies for the check (the committed target, or the proposed
//!   one for a create), each projected scalar property only; the principal's
//!   one trusted attribute is `id`, the verified caller's subject id. A
//!   `principalAttribute` any other name never holds (default deny).

use std::collections::{BTreeMap, BTreeSet};

use norn_api::scope::CellScope;
use norn_cell::Model;
use norn_ir::access::{AccessCondition, AccessIr, AccessRule, PolicyRoot};
use norn_ir::{ActionRoot, ArtifactEnvelope, ArtifactKind, IrVersion01, RuntimeValue};
use norn_kernel::authz::{
    self, AccessRequest, ActorKind, AttrValue, AuthzSchema, Binding, CapabilityGrant, Pdp,
    PdpActionAuthorizer, PdpDecision, ResourceInput, ScopeKey, UnboundProfile, VerifiedPrincipal,
};
use norn_kernel::{ActionAuthorizer, ActionAuthzRequest, ActionDenial};
use norn_projections::{CedarAttribute, CedarPrincipalAttribute};

use crate::policy::{Caller, scope_key};
use crate::seed::SeedError;

/// The one trusted principal attribute: the verified caller's subject id.
pub const PRINCIPAL_ID_ATTRIBUTE: &str = "id";

/// The id of the compiled action policy: every tenant rule is prefixed with
/// the id of the policy artifact it came from.
pub const ACTION_POLICY_ID: &str = "tenant.actions";

/// The compiled action policy of one definitions revision.
pub struct ActionPolicy {
    /// The tenant's rules, compiled, with no grant; each call adds its caller's.
    base: Pdp,
    /// The actions whose `access` ref names a compiled policy.
    callable: BTreeSet<String>,
    key: ScopeKey,
    /// The projected scalar attributes of each resource type
    /// (ACTION-RESOURCE-1): the target values the PDP sees.
    resource_attributes: BTreeMap<String, BTreeMap<String, CedarAttribute>>,
    /// Whether the rules read the principal's `id`.
    principal_id: bool,
    /// PRINCIPAL-INPUT-1 (#1091): this revision's principal input resolver.
    principal: crate::principal_inputs::PrincipalResolver,
    /// ADMIT-CACHE-1 (#1533): each caller's PDP (the base rules plus that
    /// caller's call grant), compiled once per revision instead of per call.
    /// The grant is a function of the caller's subject and kind and of this
    /// revision alone; at most [`CALLER_PDPS`] are kept.
    callers: std::sync::Mutex<BTreeMap<(String, &'static str), std::sync::Arc<Pdp>>>,
    /// Each callable action's target type and title, for
    /// `GET /v1/access?objectType=` ([`ActionPolicy::role_decisions`]).
    targets: BTreeMap<String, (String, String)>,
    /// For each callable action, the role lists of every rule that grants it
    /// under a condition other than a role (a resource value, a relation, a
    /// context value, the caller's identity): a role every list names (or a
    /// rule with no role condition) may call it depending on the record or
    /// the person.
    conditional: BTreeMap<String, Vec<Vec<Vec<String>>>>,
}

/// What one declared role alone may do with one action
/// ([`ActionPolicy::role_decisions`]).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RoleAction {
    /// The action identity.
    pub action_id: String,
    /// Its title, or its identity when it declares none.
    pub title: String,
    /// `permit`: a rule grants the role whatever the record; `conditional`:
    /// only a rule that names the role and also reads the record or the
    /// caller could grant it, decided at each call; `deny`: no rule grants it.
    pub decision: &'static str,
}

/// The most callers whose PDP one revision keeps; past it the cache is
/// emptied and refills, so a flood of callers never grows it without bound.
const CALLER_PDPS: usize = 1024;

impl std::fmt::Debug for ActionPolicy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ActionPolicy")
            .field("callable", &self.callable)
            .finish_non_exhaustive()
    }
}

/// The payload of `artifact`, decoded as `T`.
fn payload<T: serde::de::DeserializeOwned>(artifact: &ArtifactEnvelope) -> Result<T, SeedError> {
    serde_json::to_value(artifact.payload.to_json_value())
        .and_then(serde_json::from_value)
        .map_err(|error| SeedError(format!("{}: {error}", artifact.artifact_id)))
}

/// The object type an action's effect targets; `None` without an effect
/// program (nothing a caller can run).
fn target_kind(action: &ActionRoot) -> Option<&str> {
    Some(action.effect_program.as_ref()?.target().object_kind)
}

/// For each action, the role lists of every rule that grants it under a
/// condition other than a role ([`ActionPolicy::role_decisions`]).
fn conditional_rules(rules: &[AccessRule]) -> BTreeMap<String, Vec<Vec<Vec<String>>>> {
    let mut conditional: BTreeMap<String, Vec<Vec<Vec<String>>>> = BTreeMap::new();
    for rule in rules {
        if rule
            .all_of
            .iter()
            .all(|condition| matches!(condition, AccessCondition::Role(_)))
        {
            continue;
        }
        let named: Vec<Vec<String>> = rule
            .all_of
            .iter()
            .filter_map(|condition| match condition {
                AccessCondition::Role(role) => Some(role.any_of.clone()),
                AccessCondition::Rel(_)
                | AccessCondition::Context(_)
                | AccessCondition::Resource(_)
                | AccessCondition::Marking(_)
                | AccessCondition::Entitlement(_) => None,
            })
            .collect();
        for action in &rule.actions {
            conditional
                .entry(action.clone())
                .or_default()
                .push(named.clone());
        }
    }
    conditional
}

/// Each callable action's target type and title.
fn callable_targets<'a>(
    actions: impl Iterator<Item = &'a ActionRoot>,
    callable: &BTreeSet<String>,
) -> BTreeMap<String, (String, String)> {
    actions
        .filter(|action| callable.contains(&action.action_id))
        .filter_map(|action| {
            let kind = target_kind(action)?;
            let title = action
                .title
                .clone()
                .unwrap_or_else(|| action.action_id.clone());
            Some((action.action_id.clone(), (kind.to_owned(), title)))
        })
        .collect()
}

/// The rules of each governing policy, each narrowed to the actions that
/// name that policy; a rule left with no action is dropped.
fn narrowed(
    governs: &BTreeMap<&str, BTreeSet<&str>>,
    policies: &BTreeMap<&str, (&str, AccessIr)>,
) -> Result<Vec<AccessRule>, SeedError> {
    let mut rules: Vec<AccessRule> = Vec::new();
    for (policy_id, governed) in governs {
        let Some((_, access)) = policies.get(policy_id) else {
            continue;
        };
        if !access.generic_rules.is_empty() || access.extensions.is_some() {
            return Err(SeedError(format!(
                "{policy_id}: generic rules and extensions are not served on this host"
            )));
        }
        for rule in &access.rules {
            let actions: Vec<String> = rule
                .actions
                .iter()
                .filter(|action| governed.contains(action.as_str()))
                .cloned()
                .collect();
            if actions.is_empty() {
                continue;
            }
            rules.push(AccessRule {
                // Length-prefixed, so no two (policy, rule) pairs share an id.
                rule_id: format!("{}:{policy_id}.{}", policy_id.len(), rule.rule_id),
                actions,
                ..rule.clone()
            });
        }
    }
    Ok(rules)
}

/// The tenant's rules compiled against the projected schema
/// (ACTION-RESOURCE-1).
struct Compiled {
    base: Pdp,
    resource_attributes: BTreeMap<String, BTreeMap<String, CedarAttribute>>,
    principal_id: bool,
    /// The principal attributes the rules read, as inferred.
    principal_attributes: BTreeMap<String, CedarPrincipalAttribute>,
}

fn compiled(
    types: &[norn_ir::ObjectTypeDef],
    interfaces: &[norn_ir::InterfaceDef],
    roots: &[ActionRoot],
    rules: Vec<AccessRule>,
) -> Result<Compiled, SeedError> {
    let policy = AccessIr {
        access_version: IrVersion01::V0_1,
        policy_id: ACTION_POLICY_ID.into(),
        rules,
        generic_rules: vec![],
        principal_catalog: vec![],
        extensions: None,
    };
    // ACTION-RESOURCE-1: the projection types each resource condition;
    // the principal attributes the rules read are inferred from it.
    let plain = norn_projections::project_cedar_schema(types, interfaces, roots);
    let principal = authz::infer_principal_attributes(std::slice::from_ref(&policy), &plain)
        .map_err(|error| SeedError(format!("action policy: {error}")))?;
    if let Some((name, _)) = principal.iter().find(|(name, kind)| {
        name.as_str() != PRINCIPAL_ID_ATTRIBUTE || !matches!(kind, CedarPrincipalAttribute::String)
    }) {
        return Err(SeedError(format!(
            "action policy: principal attribute {name:?} is not one this host verifies; only `{PRINCIPAL_ID_ATTRIBUTE}` (the caller's subject id, a string) is"
        )));
    }
    let projection = norn_projections::project_cedar_schema_with_principal_attributes(
        types, interfaces, roots, &principal,
    );
    let host = norn_projections::project_host_access_schema(types);
    let schema = AuthzSchema::from_projected_cedarschema_fragments(&[&projection.schema, &host])
        .map_err(|error| SeedError(format!("action schema: {error}")))?;
    let compiled = authz::compile_all_projected(&schema, &[policy], &projection)
        .map_err(|error| SeedError(format!("action policy: {error}")))?;
    let base =
        Pdp::new(compiled, &[]).map_err(|error| SeedError(format!("action policy: {error}")))?;
    let resource_attributes = projection
        .attributes
        .into_iter()
        .map(|(object_type, attributes)| {
            let scalar = attributes
                .into_iter()
                .filter(|(_, kind)| {
                    matches!(
                        kind,
                        CedarAttribute::Bool | CedarAttribute::Long | CedarAttribute::String
                    )
                })
                .collect();
            (object_type, scalar)
        })
        .collect();
    Ok(Compiled {
        base,
        resource_attributes,
        principal_id: principal.contains_key(PRINCIPAL_ID_ATTRIBUTE),
        principal_attributes: principal,
    })
}

impl ActionPolicy {
    /// Compile the action policy of the definitions `artifacts` installed over
    /// `model`, for `scope`. `Ok(None)` when no action is callable.
    ///
    /// # Errors
    /// An action or policy does not decode, or the referenced policies do not
    /// compile against the projected schema; the cell then serves no action.
    pub fn compile(
        model: &Model,
        artifacts: &[ArtifactEnvelope],
        scope: &CellScope,
    ) -> Result<Option<Self>, SeedError> {
        let mut actions: Vec<(&ArtifactEnvelope, ActionRoot)> = Vec::new();
        // Policy artifacts in the access form, by id, with their version.
        let mut policies: BTreeMap<&str, (&str, AccessIr)> = BTreeMap::new();
        for artifact in artifacts {
            match artifact.kind() {
                ArtifactKind::Action => actions.push((artifact, payload(artifact)?)),
                ArtifactKind::Policy => {
                    if let PolicyRoot::Access(access) = payload(artifact)? {
                        policies.insert(
                            artifact.artifact_id.as_str(),
                            (artifact.version.as_str(), access),
                        );
                    }
                }
                ArtifactKind::Entity
                | ArtifactKind::Relation
                | ArtifactKind::Rule
                | ArtifactKind::Connector
                | ArtifactKind::Widget
                | ArtifactKind::Customization
                | ArtifactKind::Setting
                | ArtifactKind::BusinessPack
                | ArtifactKind::Derived
                | ArtifactKind::SourceSchema
                | ArtifactKind::Mapping
                | ArtifactKind::Law
                | ArtifactKind::DefinedConcept
                | ArtifactKind::Labels
                | ArtifactKind::Model => {}
            }
        }
        // Which policy governs each action: the one its `access` ref names, at
        // the version the ref pins or, failing that, the version its required
        // dependency on that policy pins (as the checker resolves it).
        let mut governs: BTreeMap<&str, BTreeSet<&str>> = BTreeMap::new();
        for (envelope, action) in &actions {
            let Some(reference) = &action.access else {
                continue;
            };
            let Some((version, _)) = policies.get(reference.artifact_id.as_str()) else {
                continue;
            };
            let pinned = reference.version.as_deref().or_else(|| {
                envelope
                    .dependencies
                    .iter()
                    .find(|dependency| {
                        dependency.artifact_id == reference.artifact_id && dependency.required
                    })
                    .and_then(|dependency| dependency.version.as_deref())
            });
            if pinned.is_some_and(|pinned| pinned != *version) {
                continue;
            }
            governs
                .entry(reference.artifact_id.as_str())
                .or_default()
                .insert(action.action_id.as_str());
        }
        let rules = narrowed(&governs, &policies)?;
        let callable: BTreeSet<String> = rules
            .iter()
            .flat_map(|rule| rule.actions.iter().cloned())
            .collect();
        let conditional = conditional_rules(&rules);
        let targets = callable_targets(actions.iter().map(|(_, action)| action), &callable);
        if callable.is_empty() {
            return Ok(None);
        }
        let types: Vec<_> = model.object_types().cloned().collect();
        let interfaces: Vec<_> = model.interfaces().cloned().collect();
        let roots: Vec<ActionRoot> = actions.into_iter().map(|(_, action)| action).collect();
        let Compiled {
            base,
            resource_attributes,
            principal_id,
            principal_attributes,
        } = compiled(&types, &interfaces, &roots, rules)?;
        let key = scope_key(scope);
        let principal = crate::principal_inputs::PrincipalResolver::compile(
            artifacts,
            &principal_attributes,
            &key,
        )?;
        Ok(Some(Self {
            base,
            callable,
            key,
            resource_attributes,
            principal_id,
            principal,
            callers: std::sync::Mutex::new(BTreeMap::new()),
            targets,
            conditional,
        }))
    }

    /// ACTION-RESOURCE-1: the target's projected scalar values as the PDP's
    /// resource attributes. A value of another shape (a decimal, a link, an
    /// integer outside `i64`, null) is left out, so a condition on it never
    /// holds.
    fn resource_values(
        &self,
        object_type: &str,
        values: Option<&norn_kernel::cell_host::JsonObject>,
    ) -> BTreeMap<String, AttrValue> {
        let (Some(projected), Some(values)) = (self.resource_attributes.get(object_type), values)
        else {
            return BTreeMap::new();
        };
        projected
            .iter()
            .filter_map(|(name, kind)| {
                let value = values.get(name)?;
                let attribute = match (kind, value) {
                    (CedarAttribute::String, RuntimeValue::String(text)) => {
                        AttrValue::String(text.clone())
                    }
                    (CedarAttribute::Bool, RuntimeValue::Bool(flag)) => AttrValue::Bool(*flag),
                    (CedarAttribute::Long, RuntimeValue::Number(number)) => {
                        AttrValue::Long(number.to_string().parse().ok()?)
                    }
                    _ => return None,
                };
                Some((name.clone(), attribute))
            })
            .collect()
    }

    /// What `role` alone may do with each callable action on `object_type`,
    /// ordered by action identity. The PDP decides each for a principal
    /// holding only that role, with no record and no identity: a rule that
    /// grants on the role alone permits; one that also reads the record or the
    /// caller cannot hold without them, so that action answers `conditional`
    /// and is decided at each call. No record value is read here.
    #[must_use]
    pub fn role_decisions(&self, role: &str, object_type: &str) -> Vec<RoleAction> {
        let subject = crate::policy::role_subject(role);
        let pdp = Pdp::new(
            self.base.policy_set().clone(),
            &[CapabilityGrant {
                subject_id: subject.clone(),
                actor_kind: ActorKind::User,
                agent_session_id: None,
                scope: Some(self.key.clone()),
                capabilities: self.callable.iter().cloned().collect(),
                binding: None,
            }],
        );
        self.targets
            .iter()
            .filter(|(_, (kind, _))| kind == object_type)
            .map(|(action_id, (_, title))| {
                let permitted = pdp.as_ref().is_ok_and(|pdp| {
                    authz::authorize(
                        pdp,
                        &AccessRequest::new(
                            &VerifiedPrincipal::new(
                                subject.clone(),
                                ActorKind::User,
                                None,
                                vec![role.to_owned()],
                                BTreeMap::new(),
                            ),
                            Some(self.key.clone()),
                            action_id,
                            action_id,
                            ResourceInput {
                                object_type: object_type.to_owned(),
                                id: String::new(),
                                scope: self.key.clone(),
                                attributes: BTreeMap::new(),
                            },
                            BTreeMap::new(),
                            Binding::Unbound(UnboundProfile::HostedAction),
                        ),
                    )
                    .is_ok()
                });
                let decision = if permitted {
                    "permit"
                } else if self.conditional.get(action_id).is_some_and(|rules| {
                    rules.iter().any(|lists| {
                        lists
                            .iter()
                            .all(|list| list.iter().any(|named| named == role))
                    })
                }) {
                    "conditional"
                } else {
                    "deny"
                };
                RoleAction {
                    action_id: action_id.clone(),
                    title: title.clone(),
                    decision,
                }
            })
            .collect()
    }

    /// PRINCIPAL-INPUT-1: this revision's principal input resolver.
    #[must_use]
    pub const fn principal_resolver(&self) -> &crate::principal_inputs::PrincipalResolver {
        &self.principal
    }

    /// Whether `action_id` names a callable action.
    #[must_use]
    pub fn callable(&self, action_id: &str) -> bool {
        self.callable.contains(action_id)
    }

    /// Core 05 `AUTH-ATTENUATE-1`: verify through this revision's scoped Cedar
    /// PDP that the verified principal `caller` holds an explicit grant to
    /// invoke `action_id` on every concrete type in `matched_types` (the
    /// watched `objectType` and every subtype extending it).
    ///
    /// Returns the sorted, deduplicated, non-empty Cedar policy rule ids whose
    /// `Allow` decisions authorized the grant, or `None` on any deny, empty
    /// determining policy set, uncallable action, empty roles, empty
    /// `matched_types`, or unsupported actor kind. Never falls back to
    /// inspecting role conjuncts of conditional rules: rules requiring record
    /// or context attributes fail closed here.
    ///
    /// U2 dispatch obligation: dispatch still must require the current active
    /// automation revision and reauthorize `caller`'s live grant against the
    /// live target row under the dispatch hold.
    #[must_use]
    pub fn authorize_automation_grant(
        &self,
        caller: &Caller,
        action_id: &str,
        matched_types: &[String],
    ) -> Option<Vec<String>> {
        if !self.callable(action_id) || caller.roles.is_empty() || matched_types.is_empty() {
            return None;
        }
        let actor_kind = match caller.actor_kind {
            "user" => ActorKind::User,
            "service" => ActorKind::Service,
            _ => return None,
        };
        let pdp = self
            .caller_pdp(&caller.subject_id, caller.actor_kind, actor_kind)
            .ok()?;
        let principal_attributes = if self.principal_id {
            BTreeMap::from([(
                PRINCIPAL_ID_ATTRIBUTE.to_owned(),
                AttrValue::String(caller.subject_id.clone()),
            )])
        } else {
            BTreeMap::new()
        };
        let mut parent_grant_ids = BTreeSet::new();
        for resource_type in matched_types {
            let principal = VerifiedPrincipal::new(
                caller.subject_id.clone(),
                actor_kind,
                None,
                caller.roles.clone(),
                principal_attributes.clone(),
            );
            let explanation = pdp.explain(&AccessRequest::new(
                &principal,
                Some(self.key.clone()),
                action_id,
                action_id,
                ResourceInput {
                    object_type: resource_type.clone(),
                    id: String::new(),
                    scope: self.key.clone(),
                    attributes: BTreeMap::new(),
                },
                BTreeMap::new(),
                Binding::Unbound(UnboundProfile::HostedAction),
            ));
            if !matches!(explanation.decision, PdpDecision::Allow)
                || explanation.determining_policies.is_empty()
            {
                return None;
            }
            for policy in explanation.determining_policies {
                parent_grant_ids.insert(policy.cedar_policy_id);
            }
        }
        (!parent_grant_ids.is_empty()).then(|| parent_grant_ids.into_iter().collect())
    }

    /// The PDP deciding `subject`'s calls: the base rules and the call
    /// capability for exactly the callable actions, compiled on its first
    /// call under this revision (ADMIT-CACHE-1).
    fn caller_pdp(
        &self,
        subject: &str,
        kind: &'static str,
        actor_kind: ActorKind,
    ) -> Result<std::sync::Arc<Pdp>, ActionDenial> {
        let denied = || ActionDenial::Forbidden {
            reason: norn_api::refusal::codes::AUTHZ_DENIED.into(),
        };
        let key = (subject.to_owned(), kind);
        let mut callers = self.callers.lock().map_err(|_poisoned| denied())?;
        if let Some(pdp) = callers.get(&key) {
            return Ok(std::sync::Arc::clone(pdp));
        }
        let grant = CapabilityGrant {
            subject_id: subject.to_owned(),
            actor_kind,
            agent_session_id: None,
            scope: Some(self.key.clone()),
            capabilities: self.callable.iter().cloned().collect(),
            binding: None,
        };
        let pdp = std::sync::Arc::new(
            Pdp::new(self.base.policy_set().clone(), &[grant]).map_err(|_unbuildable| denied())?,
        );
        if callers.len() >= CALLER_PDPS {
            callers.clear();
        }
        callers.insert(key, std::sync::Arc::clone(&pdp));
        Ok(pdp)
    }

    /// Decide the kernel's action request for `caller`, the request's
    /// verified principal: the call capability for exactly the callable
    /// actions, and the tenant's rules deciding.
    ///
    /// # Errors
    /// The PDP denies, or the request names an action that is not callable.
    pub fn decide(
        &self,
        caller: &Caller,
        request: &ActionAuthzRequest<'_>,
    ) -> Result<(), ActionDenial> {
        let denied = || ActionDenial::Forbidden {
            reason: norn_api::refusal::codes::AUTHZ_DENIED.into(),
        };
        if caller.subject_id != request.actor || !self.callable(&request.action.action_id) {
            return Err(denied());
        }
        let actor_kind = match caller.actor_kind {
            "user" => ActorKind::User,
            "service" => ActorKind::Service,
            _ => return Err(denied()),
        };
        let pdp = self.caller_pdp(&caller.subject_id, caller.actor_kind, actor_kind)?;
        let key = self.key.clone();
        let roles = caller.roles.clone();
        let principal_attributes = if self.principal_id {
            BTreeMap::from([(
                PRINCIPAL_ID_ATTRIBUTE.to_owned(),
                AttrValue::String(caller.subject_id.clone()),
            )])
        } else {
            BTreeMap::new()
        };
        PdpActionAuthorizer::new(pdp, move |call: &ActionAuthzRequest<'_>| {
            let principal = VerifiedPrincipal::new(
                call.actor,
                actor_kind,
                None,
                roles.clone(),
                principal_attributes.clone(),
            );
            Ok(AccessRequest::new(
                &principal,
                Some(key.clone()),
                &call.action.action_id,
                &call.action.action_id,
                ResourceInput {
                    object_type: call.target.object_type.as_str().to_owned(),
                    id: call.target.key.clone(),
                    scope: key.clone(),
                    attributes: self
                        .resource_values(call.target.object_type.as_str(), call.target_values),
                },
                BTreeMap::new(),
                Binding::Unbound(UnboundProfile::HostedAction),
            ))
        })
        .authorize(request)
    }
}

pub(crate) fn collection_access_key(
    key: &ScopeKey,
    subject: &str,
    capability: norn_api::authz::ReadCapability,
    object_type: &str,
) -> AccessRequest {
    let key = key.clone();
    let who = VerifiedPrincipal::new(subject, ActorKind::User, None, vec![], BTreeMap::new());
    AccessRequest::new(
        &who,
        Some(key.clone()),
        crate::policy::capability_name(capability),
        crate::policy::READ_ACTION,
        ResourceInput {
            object_type: object_type.into(),
            id: object_type.into(),
            scope: key,
            attributes: BTreeMap::new(),
        },
        BTreeMap::new(),
        Binding::Unbound(UnboundProfile::HostedRead),
    )
}
