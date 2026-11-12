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

use std::collections::{BTreeMap, BTreeSet};

use norn_api::scope::CellScope;
use norn_cell::Model;
use norn_ir::access::{AccessIr, AccessRule, PolicyRoot};
use norn_ir::{ActionRoot, ArtifactEnvelope, ArtifactKind, IrVersion01};
use norn_kernel::authz::{
    self, AccessRequest, ActorKind, AuthzSchema, CapabilityGrant, Delegation, Pdp,
    PdpActionAuthorizer, PrincipalInput, ResourceInput, ScopeKey,
};
use norn_kernel::{ActionAuthorizer, ActionAuthzRequest, ActionDenial};

use crate::policy::{Caller, scope_key};
use crate::seed::SeedError;

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
}

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
                | ArtifactKind::Labels => {}
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
        let mut rules: Vec<AccessRule> = Vec::new();
        for (policy_id, governed) in &governs {
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
                    rule_id: format!("{policy_id}.{}", rule.rule_id),
                    actions,
                    ..rule.clone()
                });
            }
        }
        let callable: BTreeSet<String> = rules
            .iter()
            .flat_map(|rule| rule.actions.iter().cloned())
            .collect();
        if callable.is_empty() {
            return Ok(None);
        }
        let types: Vec<_> = model.object_types().cloned().collect();
        let interfaces: Vec<_> = model.interfaces().cloned().collect();
        let roots: Vec<ActionRoot> = actions.into_iter().map(|(_, action)| action).collect();
        let projection = norn_projections::project_cedar_schema(&types, &interfaces, &roots);
        let host = norn_projections::project_host_access_schema(&types);
        let schema =
            AuthzSchema::from_projected_cedarschema_fragments(&[&projection.schema, &host])
                .map_err(|error| SeedError(format!("action schema: {error}")))?;
        let policy = AccessIr {
            access_version: IrVersion01::V0_1,
            policy_id: ACTION_POLICY_ID.into(),
            rules,
            generic_rules: vec![],
            extensions: None,
        };
        let compiled = authz::compile(&schema, &policy)
            .map_err(|error| SeedError(format!("action policy: {error}")))?;
        let base = Pdp::new(compiled, &[])
            .map_err(|error| SeedError(format!("action policy: {error}")))?;
        Ok(Some(Self {
            base,
            callable,
            key: scope_key(scope),
        }))
    }

    /// Whether `action_id` names a callable action.
    #[must_use]
    pub fn callable(&self, action_id: &str) -> bool {
        self.callable.contains(action_id)
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
        let actor_kind = if caller.actor_kind == "service" {
            ActorKind::Service
        } else {
            ActorKind::User
        };
        let grant = CapabilityGrant {
            subject_id: caller.subject_id.clone(),
            actor_kind,
            agent_session_id: None,
            scope: Some(self.key.clone()),
            capabilities: self.callable.iter().cloned().collect(),
            binding: None,
        };
        let pdp = Pdp::new(self.base.policy_set().clone(), &[grant]).map_err(|_| denied())?;
        let key = self.key.clone();
        let roles = caller.roles.clone();
        PdpActionAuthorizer::new(pdp, move |call: &ActionAuthzRequest<'_>| {
            Ok(AccessRequest {
                principal: PrincipalInput {
                    subject_id: call.actor.to_owned(),
                    actor_kind,
                    agent_session_id: None,
                    roles: roles.clone(),
                    attributes: BTreeMap::new(),
                },
                scope: Some(key.clone()),
                capability: call.action.action_id.clone(),
                action_id: call.action.action_id.clone(),
                resource: ResourceInput {
                    object_type: call.target.object_type.as_str().to_owned(),
                    id: call.target.key.clone(),
                    scope: key.clone(),
                    attributes: BTreeMap::new(),
                },
                context: BTreeMap::new(),
                binding: None,
                delegation: Delegation::Direct,
            })
        })
        .authorize(request)
    }
}
