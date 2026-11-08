//! HOST-READ-1 server policy for the demo cells, decided by the kernel PDP.
//!
//! Data reads (Population, Values, Sources) are decided by the kernel's Cedar
//! PDP under its AND rule. The policy is an `AccessIR` read rule per object
//! type, compiled against the Cedar schema projected from the cell's own
//! model, and the capability grants are per fixture principal. Each capability
//! has its own grant name, and the policy checks that the trusted request
//! mapping asked for the capability being decided. Definition reads remain
//! the host operation [`Operation::ReadDefinitions`].

use norn_api::authz::{AccessRequest, Authorizer, DenyAll, Operation, ReadCapability, ReadRequest};
use norn_api::refusal::Refusal;
use norn_api::scope::CellScope;
use norn_ir::access::{AccessIr, AccessRule, AccessScope, ScopeDimension};
use norn_ir::{IrVersion01, ObjectTypeDef};
use norn_kernel::authz::{self, ActorKind, AuthzSchema, CapabilityGrant, Pdp, ScopeKey};

use crate::seed::SeedError;

/// The `AccessIR` action every data read is decided under.
pub const READ_ACTION: &str = "norn.read";

/// The grant name of one read capability; none implies another.
#[must_use]
pub const fn capability_name(capability: ReadCapability) -> &'static str {
    match capability {
        ReadCapability::Population => "norn.read.population",
        ReadCapability::Values => "norn.read.values",
        ReadCapability::Sources => "norn.read.sources",
    }
}

/// One principal's grants in one cell.
#[derive(Debug, Clone, Copy)]
pub struct Grant {
    /// Principal subject id, as [`crate::principal::PrincipalResolver`] returns it.
    pub subject: &'static str,
    /// The read capabilities granted.
    pub capabilities: &'static [ReadCapability],
    /// Whether the principal may read the admitted definitions.
    pub definitions: bool,
}

/// The demo's grants: the viewer sees everything; the guest sees which
/// members exist but no values or sources.
pub const DEMO_GRANTS: &[Grant] = &[
    Grant {
        subject: "viewer@demo",
        capabilities: &[
            ReadCapability::Population,
            ReadCapability::Values,
            ReadCapability::Sources,
        ],
        definitions: true,
    },
    Grant {
        subject: "guest@demo",
        capabilities: &[ReadCapability::Population],
        definitions: true,
    },
];

/// The compiled policy for one cell.
pub struct ReadPolicy {
    pdp: Pdp,
    grants: &'static [Grant],
}

/// The physical scope as the PDP's scope key.
#[must_use]
pub fn scope_key(scope: &CellScope) -> ScopeKey {
    ScopeKey {
        org: scope.org_id.clone(),
        environment: Some(scope.environment_id.clone()),
        tenant: None,
        workspace: None,
        business_unit: None,
    }
}

impl ReadPolicy {
    /// Compile the read rules for `object_types` and the scoped grants.
    ///
    /// # Errors
    /// The projected schema or the policy does not compile.
    pub fn compile<'a>(
        object_types: impl IntoIterator<Item = &'a ObjectTypeDef>,
        scope: &CellScope,
        grants: &'static [Grant],
    ) -> Result<Self, SeedError> {
        let types: Vec<ObjectTypeDef> = object_types.into_iter().cloned().collect();
        let projection = norn_projections::project_cedar_schema(&types, &[], &[]);
        let host = norn_projections::project_host_access_schema(&types);
        let schema =
            AuthzSchema::from_projected_cedarschema_fragments(&[&projection.schema, &host])
                .map_err(|e| SeedError(format!("read schema: {e}")))?;
        let policy = AccessIr {
            access_version: IrVersion01::V0_1,
            policy_id: "demo.host.read".into(),
            rules: types
                .iter()
                .map(|object_type| AccessRule {
                    rule_id: format!("read_{}", object_type.name.as_str()),
                    actions: vec![READ_ACTION.into()],
                    resource_type: object_type.name.clone(),
                    scope: AccessScope {
                        dimensions: vec![ScopeDimension::Org, ScopeDimension::Environment],
                    },
                    all_of: vec![],
                })
                .collect(),
            generic_rules: vec![],
            extensions: None,
        };
        let key = scope_key(scope);
        let capability_grants: Vec<CapabilityGrant> = grants
            .iter()
            .filter(|grant| !grant.capabilities.is_empty())
            .map(|grant| CapabilityGrant {
                subject_id: grant.subject.into(),
                actor_kind: ActorKind::User,
                agent_session_id: None,
                scope: Some(key.clone()),
                capabilities: grant
                    .capabilities
                    .iter()
                    .map(|c| capability_name(*c).to_owned())
                    .collect(),
                binding: None,
            })
            .collect();
        let compiled =
            authz::compile(&schema, &policy).map_err(|e| SeedError(format!("read policy: {e}")))?;
        let pdp = Pdp::new(compiled, &capability_grants)
            .map_err(|e| SeedError(format!("read grants: {e}")))?;
        Ok(Self { pdp, grants })
    }
}

impl Authorizer for ReadPolicy {
    fn authorize(&self, request: &AccessRequest<'_>) -> Result<(), Refusal> {
        let granted = matches!(request.operation, Operation::ReadDefinitions)
            && request.actor.is_some_and(|actor| {
                self.grants
                    .iter()
                    .any(|grant| grant.definitions && grant.subject == actor)
            });
        if granted {
            Ok(())
        } else {
            DenyAll.authorize(request)
        }
    }

    fn read(&self, request: &ReadRequest<'_>) -> Result<(), Refusal> {
        let asked = request.access.capability == capability_name(request.capability)
            && request.access.action_id == READ_ACTION;
        if asked && authz::authorize(&self.pdp, request.access).is_ok() {
            Ok(())
        } else {
            DenyAll.read(request)
        }
    }
}
