//! PRINCIPAL-INPUT-1 (#1091): this host's resolver of a decision's declared
//! principal inputs.
//!
//! The inputs come from the request's verified principal
//! ([`crate::policy::Caller`]: the token mapped through the declared role
//! table) and the kernel PDP, never from the request. The catalog is the one
//! the scope's own `AccessIR` policies declare, checked against the types
//! the action policy infers. `holdsRole` and each role of a role profile are
//! PDP decisions under the capability
//! [`HOLDS_ROLE_ACTION`](norn_kernel::authz::HOLDS_ROLE_ACTION), which every
//! verified member of the cell holds in the cell's scope. Every object of a
//! cell lives in the cell's scope, so that is the scope of a decision's
//! subject. The one scoped attribute this host verifies is `id`, the
//! caller's subject id; any other declared attribute is unavailable.
//! A run's recorded starter acting at a flow step
//! ([`crate::caller::CallerSlot::is_acting`]) carries the roles of an earlier
//! request, so nothing resolves for it and the step fails closed. An approved
//! call is not such a stand-in: it is released only on its requester's own
//! live request, so its inputs resolve from that request's verified caller.

use std::collections::{BTreeMap, BTreeSet};

use norn_ir::access::{AccessIr, PolicyRoot, PrincipalAttributeCatalog};
use norn_ir::{ArtifactEnvelope, ArtifactKind, RuleRoot};
use norn_kernel::authz::{
    self, ActorKind, AttrValue, CapabilityGrant, Delegation, HoldsRolePolicy, PrincipalContext,
    PrincipalInput, ScopeKey,
};
use norn_kernel::{ActionPrincipalInputs, PrincipalInputsRequest, UnresolvedPrincipalInput};
use norn_projections::CedarPrincipalAttribute;

use crate::actions::{ActionPolicy, PRINCIPAL_ID_ATTRIBUTE};
use crate::policy::{Caller, GrantTable};
use crate::seed::SeedError;

/// The resolver of one definitions revision.
#[derive(Debug)]
pub struct PrincipalResolver {
    catalog: PrincipalAttributeCatalog,
    holds: HoldsRolePolicy,
    key: ScopeKey,
    /// Digest of the catalog's declared entries.
    catalog_version: String,
}

fn decode<T: serde::de::DeserializeOwned>(artifact: &ArtifactEnvelope) -> Option<T> {
    serde_json::to_value(artifact.payload.to_json_value())
        .and_then(serde_json::from_value)
        .ok()
}

fn request_scope(scope: &BTreeMap<String, String>) -> Option<ScopeKey> {
    Some(ScopeKey {
        org: scope.get("orgId")?.clone(),
        tenant: scope.get("tenantId").cloned(),
        workspace: scope.get("workspaceId").cloned(),
        environment: scope.get("environmentId").cloned(),
        business_unit: scope.get("businessUnitId").cloned(),
    })
}

impl PrincipalResolver {
    /// Compile the resolver of the definitions `artifacts` for the cell
    /// scope `key`. `inferred` is what the action policy infers for the
    /// principal attributes its rules read.
    ///
    /// # Errors
    /// The declared catalog is inconsistent, or a `holdsRole` rule does not
    /// compile; the cell then serves no action.
    pub fn compile(
        artifacts: &[ArtifactEnvelope],
        inferred: &BTreeMap<String, CedarPrincipalAttribute>,
        key: &ScopeKey,
    ) -> Result<Self, SeedError> {
        let policies: Vec<AccessIr> = artifacts
            .iter()
            .filter(|artifact| artifact.kind() == ArtifactKind::Policy)
            .filter_map(|artifact| match decode::<PolicyRoot>(artifact)? {
                PolicyRoot::Access(access) => Some(access),
                PolicyRoot::Obligations(_) | PolicyRoot::Approval(_) => None,
            })
            .collect();
        let catalog = authz::principal_attribute_catalog(&policies, inferred)
            .map_err(|error| SeedError(format!("principal catalog: {error}")))?;
        let dimensions = authz::scope_dimensions(key);
        let keys: BTreeSet<_> = artifacts
            .iter()
            .filter(|artifact| artifact.kind() == ArtifactKind::Rule)
            .filter_map(decode::<RuleRoot>)
            .flat_map(|rule| authz::holds_role_keys(&catalog, &rule.principal_inputs, &dimensions))
            .collect();
        let holds = HoldsRolePolicy::compile(&keys)
            .map_err(|error| SeedError(format!("principal roles: {error}")))?;
        let entries: Vec<_> = catalog.entries().collect();
        let catalog_version =
            norn_ir::canonical::domain_hash_hex(b"norn.principal-catalog.v1\0", &entries)
                .map_err(|error| SeedError(format!("principal catalog: {error}")))?;
        Ok(Self {
            catalog,
            holds,
            key: key.clone(),
            catalog_version,
        })
    }

    /// Resolve `request`'s declared inputs for `caller`, the request's
    /// verified principal, under the grant table version `grants`.
    ///
    /// # Errors
    /// The request is for another scope, or an input cannot be resolved.
    pub fn resolve(
        &self,
        caller: &Caller,
        grants: &str,
        request: &PrincipalInputsRequest<'_>,
    ) -> Result<ActionPrincipalInputs, UnresolvedPrincipalInput> {
        let actor_kind = match caller.actor_kind {
            "user" => Some(ActorKind::User),
            "service" => Some(ActorKind::Service),
            _ => None,
        };
        let (Some(actor_kind), true) = (
            actor_kind,
            caller.subject_id == request.actor
                && request_scope(request.scope).as_ref() == Some(&self.key),
        ) else {
            return Err(unresolved(request));
        };
        let mut attributes = caller.attributes.clone();
        attributes.insert(
            PRINCIPAL_ID_ATTRIBUTE.to_owned(),
            AttrValue::String(caller.subject_id.clone()),
        );
        let principal = PrincipalInput {
            subject_id: caller.subject_id.clone(),
            actor_kind,
            agent_session_id: None,
            roles: caller.roles.clone(),
            attributes,
        };
        let pdp = self
            .holds
            .pdp(&[CapabilityGrant {
                subject_id: caller.subject_id.clone(),
                actor_kind,
                agent_session_id: None,
                scope: Some(self.key.clone()),
                capabilities: vec![authz::HOLDS_ROLE_ACTION.to_owned()],
                binding: None,
            }])
            .map_err(|_uncompiled| unresolved(request))?;
        let context = PrincipalContext {
            principal: &principal,
            request_scope: &self.key,
            subject_scope: &self.key,
            delegation: &Delegation::Direct,
        };
        let values = authz::resolve_principal_inputs(&self.catalog, &pdp, &context, request.inputs)
            .map_err(|unavailable| UnresolvedPrincipalInput {
                input: unavailable.input,
            })?;
        Ok(ActionPrincipalInputs {
            principal: caller.subject_id.clone(),
            policy_version: format!(
                "roles={};grants={grants};catalog={}",
                caller.role_table, self.catalog_version
            ),
            subject: request.subject.clone(),
            values,
        })
    }
}

fn unresolved(request: &PrincipalInputsRequest<'_>) -> UnresolvedPrincipalInput {
    UnresolvedPrincipalInput {
        input: request
            .inputs
            .first()
            .map_or_default(|input| input.name.clone()),
    }
}

/// Resolve `request` for the request's verified member `caller` with the
/// cell's installed action policy; without either, nothing resolves.
///
/// # Errors
/// As [`PrincipalResolver::resolve`].
pub fn resolve(
    caller: Option<&Caller>,
    policy: Option<&ActionPolicy>,
    table: &GrantTable,
    request: &PrincipalInputsRequest<'_>,
) -> Result<ActionPrincipalInputs, UnresolvedPrincipalInput> {
    let (Some(caller), Some(policy)) = (caller, policy) else {
        return Err(unresolved(request));
    };
    policy
        .principal_resolver()
        .resolve(caller, table.version, request)
}

#[cfg(test)]
#[path = "principal_inputs_tests.rs"]
mod tests;
