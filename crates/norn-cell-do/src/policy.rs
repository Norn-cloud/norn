//! HOST-READ-1 server policy for the demo cells, decided by the kernel PDP.
//!
//! Data reads (Population, Values, Sources) are decided by the kernel's Cedar
//! PDP under its AND rule. The policy is an `AccessIR` read rule per object
//! type, compiled against the Cedar schema projected from the cell's own
//! model. A request's capability grant comes from the declared, versioned
//! [`GrantTable`] applied to the roles of the principal verified for that
//! request ([`CallerSlot`]); no client claim becomes a grant, and an
//! undeclared role grants nothing. Each capability has its own grant name, and
//! the policy checks that the trusted request mapping asked for the capability
//! being decided. Definition reads remain the host operation
//! [`Operation::ReadDefinitions`], granted to any principal the table grants.

use std::sync::{Arc, Mutex};

use norn_api::authz::{AccessRequest, Authorizer, DenyAll, Operation, ReadCapability, ReadRequest};
use norn_api::refusal::Refusal;
use norn_api::scope::CellScope;
use norn_ir::access::{AccessIr, AccessRule, AccessScope, ScopeDimension};
use norn_ir::{IrVersion01, ObjectTypeDef};
use norn_kernel::authz::{self, ActorKind, AuthzSchema, CapabilityGrant, Pdp, ScopeKey};

use crate::seed::SeedError;

/// The id of the compiled `AccessIR` read policy.
pub const READ_POLICY_ID: &str = "demo.host.read";

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

/// Declared mapping from Norn roles to read capabilities; a change of
/// mapping is a new version.
#[derive(Debug)]
pub struct GrantTable {
    /// Identity of this table.
    pub version: &'static str,
    /// Norn role and the capabilities it grants.
    pub entries: &'static [(&'static str, &'static [ReadCapability])],
}

impl GrantTable {
    /// The capabilities `roles` hold together; undeclared roles add nothing.
    #[must_use]
    pub fn capabilities(&self, roles: &[String]) -> Vec<ReadCapability> {
        let mut granted: Vec<ReadCapability> = Vec::new();
        for (_, capabilities) in self
            .entries
            .iter()
            .filter(|(role, _)| roles.iter().any(|held| held == role))
        {
            for capability in *capabilities {
                if !granted.contains(capability) {
                    granted.push(*capability);
                }
            }
        }
        granted
    }
}

/// The demo cells' table over `studio-roles/1` Norn roles: a viewer sees which
/// members exist, a member also their values, an admin or owner also sources.
pub const DEMO_GRANTS: GrantTable = GrantTable {
    version: "norn-cell-do-grants/1",
    entries: &[
        ("viewer", &[ReadCapability::Population]),
        (
            "member",
            &[ReadCapability::Population, ReadCapability::Values],
        ),
        (
            "admin",
            &[
                ReadCapability::Population,
                ReadCapability::Values,
                ReadCapability::Sources,
            ],
        ),
        (
            "owner",
            &[
                ReadCapability::Population,
                ReadCapability::Values,
                ReadCapability::Sources,
            ],
        ),
    ],
};

/// RUN-ROUTES-1: the declared roles that may start flow runs; a change is a
/// new version.
pub const RUN_STARTERS: (&str, &[&str]) = ("norn-cell-do-run-grants/1", &["admin", "owner"]);

/// The principal a resolver verified for the current request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Caller {
    /// Norn subject id; the request's actor.
    pub subject_id: String,
    /// Norn roles from the declared role table.
    pub roles: Vec<String>,
    /// `user` or `service`.
    pub actor_kind: &'static str,
    /// The declared role table the roles came from.
    pub role_table: &'static str,
}

/// Where the resolver records the current request's verified principal for
/// the policy. Every request records first (possibly nobody), so a caller
/// never outlives its request; the cell serves one request at a time.
#[derive(Debug, Default)]
pub struct CallerSlot(Mutex<Option<Caller>>);

impl CallerSlot {
    /// Record the request's verified principal, or nobody.
    pub fn record(&self, caller: Option<Caller>) {
        if let Ok(mut slot) = self.0.lock() {
            *slot = caller;
        }
    }

    /// The current request's verified principal, if any.
    #[must_use]
    pub fn current(&self) -> Option<Caller> {
        self.0.lock().ok().and_then(|slot| slot.clone())
    }
}

/// The compiled policy for one cell.
pub struct ReadPolicy {
    /// The compiled policy with no grant; each request adds its caller's.
    base: Pdp,
    key: ScopeKey,
    table: &'static GrantTable,
    caller: Arc<CallerSlot>,
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
        table: &'static GrantTable,
    ) -> Result<Self, SeedError> {
        let types: Vec<ObjectTypeDef> = object_types.into_iter().cloned().collect();
        let projection = norn_projections::project_cedar_schema(&types, &[], &[]);
        let host = norn_projections::project_host_access_schema(&types);
        let schema =
            AuthzSchema::from_projected_cedarschema_fragments(&[&projection.schema, &host])
                .map_err(|e| SeedError(format!("read schema: {e}")))?;
        let policy = AccessIr {
            access_version: IrVersion01::V0_1,
            policy_id: READ_POLICY_ID.into(),
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
        let compiled =
            authz::compile(&schema, &policy).map_err(|e| SeedError(format!("read policy: {e}")))?;
        let base = Pdp::new(compiled, &[]).map_err(|e| SeedError(format!("read policy: {e}")))?;
        Ok(Self {
            base,
            key: scope_key(scope),
            table,
            caller: Arc::default(),
        })
    }

    /// The slot this policy reads the request's verified principal from; the
    /// cell's [`crate::principal::PrincipalResolver`] records into it.
    #[must_use]
    pub fn caller(&self) -> Arc<CallerSlot> {
        Arc::clone(&self.caller)
    }

    /// The PDP for this request's verified caller, holding exactly the grant
    /// the table gives its roles.
    fn pdp_for(&self, subject: &str) -> Option<Pdp> {
        let caller = self
            .caller
            .current()
            .filter(|caller| caller.subject_id == subject)?;
        let capabilities = self.table.capabilities(&caller.roles);
        if capabilities.is_empty() {
            return None;
        }
        Pdp::new(
            self.base.policy_set().clone(),
            &[CapabilityGrant {
                subject_id: caller.subject_id,
                actor_kind: ActorKind::User,
                agent_session_id: None,
                scope: Some(self.key.clone()),
                capabilities: capabilities
                    .into_iter()
                    .map(|c| capability_name(c).to_owned())
                    .collect(),
                binding: None,
            }],
        )
        .ok()
    }
}

impl Authorizer for ReadPolicy {
    fn authorize(&self, request: &AccessRequest<'_>) -> Result<(), Refusal> {
        let caller = || {
            self.caller
                .current()
                .filter(|caller| Some(caller.subject_id.as_str()) == request.actor)
        };
        let granted = match &request.operation {
            Operation::ReadDefinitions => request
                .actor
                .is_some_and(|actor| self.pdp_for(actor).is_some()),
            Operation::StartRun(_) => caller().is_some_and(|caller| {
                caller
                    .roles
                    .iter()
                    .any(|role| RUN_STARTERS.1.contains(&role.as_str()))
            }),
            Operation::CallAction(_) => false,
        };
        if granted {
            Ok(())
        } else {
            DenyAll.authorize(request)
        }
    }

    fn read(&self, request: &ReadRequest<'_>) -> Result<(), Refusal> {
        let asked = request.access.capability == capability_name(request.capability)
            && request.access.action_id == READ_ACTION;
        let allowed = asked
            && self
                .pdp_for(&request.access.principal.subject_id)
                .is_some_and(|pdp| authz::authorize(&pdp, request.access).is_ok());
        if allowed {
            Ok(())
        } else {
            DenyAll.read(request)
        }
    }
}

/// The PDP request the cell's mappings build for `capability` on the
/// collection `object_type` (resource id and type are the type).
#[must_use]
pub fn collection_access(
    scope: &CellScope,
    subject: &str,
    capability: ReadCapability,
    object_type: &str,
) -> norn_kernel::authz::AccessRequest {
    let key = scope_key(scope);
    norn_kernel::authz::AccessRequest {
        principal: norn_kernel::authz::PrincipalInput {
            subject_id: subject.into(),
            actor_kind: ActorKind::User,
            agent_session_id: None,
            roles: vec![],
            attributes: std::collections::BTreeMap::new(),
        },
        scope: Some(key.clone()),
        capability: capability_name(capability).into(),
        action_id: READ_ACTION.into(),
        resource: norn_kernel::authz::ResourceInput {
            object_type: object_type.into(),
            id: object_type.into(),
            scope: key,
            attributes: std::collections::BTreeMap::new(),
        },
        context: std::collections::BTreeMap::new(),
        binding: None,
        delegation: norn_kernel::authz::Delegation::Direct,
    }
}
