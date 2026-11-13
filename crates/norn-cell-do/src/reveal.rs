//! PRIVACY-2: who may reveal a marked value, decided by the kernel PDP from
//! the cell's own definitions.
//!
//! The policy is data: the `norn.reveal` rules on `norn.Marking` in the
//! active definitions' `policy` artifacts (the `AccessIR` form), each scoped
//! to markings with a `marking` condition. Every other rule of those policies
//! is left to the action policy. The PDP's capability conjunct grants the
//! verified caller `norn.reveal` in this cell's scope; the tenant's rules
//! (role, marking, scope) decide. With no reveal rule, nothing is revealable.
//! Weakening the rules is refused at install, like weakening a mask
//! (`norn_cell` store preflight).

use std::collections::BTreeMap;

use norn_api::scope::CellScope;
use norn_cell::Model;
use norn_ir::access::{AccessIr, AccessRule, PolicyRoot};
use norn_ir::{ArtifactEnvelope, ArtifactKind, IrVersion01};
use norn_kernel::authz::{
    self, AccessRequest, ActorKind, AuthzSchema, CapabilityGrant, Delegation, Pdp, PdpDecision,
    PrincipalInput, ResourceInput, ScopeKey,
};

use crate::policy::{Caller, scope_key};
use crate::seed::SeedError;

/// The `AccessIR` action and capability of a reveal.
pub const REVEAL: &str = "norn.reveal";

/// The synthetic resource type a reveal rule governs.
pub const MARKING_TYPE: &str = "norn.Marking";

/// The id of the compiled reveal policy.
pub const REVEAL_POLICY_ID: &str = "tenant.reveal";

/// The compiled reveal rules of one definitions revision.
pub struct RevealPolicy {
    base: Pdp,
    key: ScopeKey,
}

impl std::fmt::Debug for RevealPolicy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RevealPolicy").finish_non_exhaustive()
    }
}

/// The reveal rules of every `AccessIR` policy in `artifacts`, each narrowed
/// to `norn.reveal` and prefixed with its policy id.
///
/// # Errors
/// A policy does not decode.
pub fn reveal_rules(artifacts: &[ArtifactEnvelope]) -> Result<Vec<AccessRule>, SeedError> {
    let mut rules = Vec::new();
    for artifact in artifacts
        .iter()
        .filter(|artifact| artifact.kind() == ArtifactKind::Policy)
    {
        let root: PolicyRoot = serde_json::to_value(artifact.payload.to_json_value())
            .and_then(serde_json::from_value)
            .map_err(|error| SeedError(format!("{}: {error}", artifact.artifact_id)))?;
        let PolicyRoot::Access(access) = root else {
            continue;
        };
        for rule in access.rules.iter().filter(|rule| {
            rule.resource_type.as_str() == MARKING_TYPE
                && rule.actions.iter().any(|action| action == REVEAL)
        }) {
            let policy_id = artifact.artifact_id.as_str();
            rules.push(AccessRule {
                rule_id: format!("{}:{policy_id}.{}", policy_id.len(), rule.rule_id),
                actions: vec![REVEAL.to_owned()],
                ..rule.clone()
            });
        }
    }
    Ok(rules)
}

impl RevealPolicy {
    /// Compile the reveal rules of `artifacts` installed over `model`, for
    /// `scope`. `Ok(None)` when they declare none.
    ///
    /// # Errors
    /// A policy does not decode, or the rules do not compile.
    pub fn compile(
        model: &Model,
        artifacts: &[ArtifactEnvelope],
        scope: &CellScope,
    ) -> Result<Option<Self>, SeedError> {
        let rules = reveal_rules(artifacts)?;
        if rules.is_empty() {
            return Ok(None);
        }
        let types: Vec<_> = model.object_types().cloned().collect();
        let interfaces: Vec<_> = model.interfaces().cloned().collect();
        let projection = norn_projections::project_cedar_schema(&types, &interfaces, &[]);
        let host = norn_projections::project_host_access_schema(&types);
        let schema =
            AuthzSchema::from_projected_cedarschema_fragments(&[&projection.schema, &host])
                .map_err(|error| SeedError(format!("reveal schema: {error}")))?;
        let policy = AccessIr {
            access_version: IrVersion01::V0_1,
            policy_id: REVEAL_POLICY_ID.into(),
            rules,
            generic_rules: vec![],
            extensions: None,
        };
        let compiled = authz::compile(&schema, &policy)
            .map_err(|error| SeedError(format!("reveal policy: {error}")))?;
        let base = Pdp::new(compiled, &[])
            .map_err(|error| SeedError(format!("reveal policy: {error}")))?;
        Ok(Some(Self {
            base,
            key: scope_key(scope),
        }))
    }

    /// Whether the verified `caller` may reveal `marking`.
    #[must_use]
    pub fn reveals(&self, caller: &Caller, marking: &str) -> bool {
        let actor_kind = match caller.actor_kind {
            "user" => ActorKind::User,
            "service" => ActorKind::Service,
            _ => return false,
        };
        let grant = CapabilityGrant {
            subject_id: caller.subject_id.clone(),
            actor_kind,
            agent_session_id: None,
            scope: Some(self.key.clone()),
            capabilities: vec![REVEAL.to_owned()],
            binding: None,
        };
        let Ok(pdp) = Pdp::new(self.base.policy_set().clone(), &[grant]) else {
            return false;
        };
        let request = AccessRequest {
            principal: PrincipalInput {
                subject_id: caller.subject_id.clone(),
                actor_kind,
                agent_session_id: None,
                roles: caller.roles.clone(),
                attributes: BTreeMap::new(),
            },
            scope: Some(self.key.clone()),
            capability: REVEAL.to_owned(),
            action_id: REVEAL.to_owned(),
            resource: ResourceInput {
                object_type: MARKING_TYPE.to_owned(),
                id: marking.to_owned(),
                scope: self.key.clone(),
                attributes: BTreeMap::new(),
            },
            context: BTreeMap::new(),
            binding: None,
            delegation: Delegation::Direct,
        };
        pdp.decide_marking(&request, marking, REVEAL) == PdpDecision::Allow
    }
}
