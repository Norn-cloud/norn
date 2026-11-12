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
//! [`Operation::ReadDefinitions`], granted to any principal the table grants
//! (every granted role holds Population). Definition writes (admit, install,
//! retire) are [`Operation::WriteDefinitions`], granted only to the table's
//! declared writer roles. Creating a cell is [`Operation::ProvisionCell`],
//! granted only to the table's declared provisioner roles: a scope with no
//! cell has no definitions, so this one authority is the host's
//! ([`Provisioners`] decides it before a cell exists, [`ReadPolicy`] after).

use std::sync::{Arc, Mutex, RwLock};

use norn_api::authz::{AccessRequest, Authorizer, DenyAll, Operation, ReadCapability, ReadRequest};
use norn_api::refusal::Refusal;
use norn_api::scope::CellScope;
use norn_ir::access::{AccessIr, AccessRule, AccessScope, ScopeDimension};
use norn_ir::{IrVersion01, ObjectTypeDef};
use norn_kernel::authz::{self, ActorKind, AuthzSchema, CapabilityGrant, Pdp, ScopeKey};
use norn_kernel::{
    ActionAuthzRequest, ActionDenial, ActionReadAuthzRequest, ActionReadTypeAuthzRequest,
};

use crate::actions::ActionPolicy;

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
    /// Norn roles that may write retained definitions (admit, install,
    /// retire); a writer must also hold a read grant.
    pub definition_writers: &'static [&'static str],
    /// Norn roles that may provision the cell of their own organization's
    /// scope; a provisioner must also hold a read grant.
    pub provisioners: &'static [&'static str],
}

impl GrantTable {
    /// Whether `roles` include a declared provisioner that also holds a
    /// read grant.
    #[must_use]
    pub fn provisions(&self, roles: &[String]) -> bool {
        !self.capabilities(roles).is_empty()
            && self
                .provisioners
                .iter()
                .any(|provisioner| roles.iter().any(|held| held == provisioner))
    }

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

    /// Whether `roles` include a declared definition writer that also holds
    /// a read grant.
    #[must_use]
    pub fn writes_definitions(&self, roles: &[String]) -> bool {
        !self.capabilities(roles).is_empty()
            && self
                .definition_writers
                .iter()
                .any(|writer| roles.iter().any(|held| held == writer))
    }
}

/// The demo cells' table over `studio-roles/1` Norn roles: a viewer sees which
/// members exist, a member also their values, an admin or owner also sources.
/// Admins and owners write retained definitions (dashboards, agents); `/2`
/// added that grant to `/1`'s read mapping, unchanged. `/3` adds the
/// provisioner grant: an owner alone creates the cell of its organization's
/// scope. Reads and definition writes are as in `/2`.
pub const DEMO_GRANTS: GrantTable = GrantTable {
    version: "norn-cell-do-grants/3",
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
    definition_writers: &["admin", "owner"],
    provisioners: &["owner"],
};

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

/// The cell's action policy for its active definitions, replaced when they
/// change; `None` serves no action.
pub type ActionSlot = Arc<RwLock<Option<Arc<ActionPolicy>>>>;

/// The compiled policy for one cell.
pub struct ReadPolicy {
    /// The compiled policy with no grant; each request adds its caller's.
    base: Pdp,
    key: ScopeKey,
    table: &'static GrantTable,
    caller: Arc<CallerSlot>,
    actions: ActionSlot,
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
            actions: Arc::default(),
        })
    }

    /// The slot this cell's action policy is installed into; empty until the
    /// host installs one, and then no action is served.
    #[must_use]
    pub fn action_slot(&self) -> ActionSlot {
        Arc::clone(&self.actions)
    }

    /// The installed action policy, if any.
    fn action_policy(&self) -> Option<Arc<ActionPolicy>> {
        self.actions.read().ok().and_then(|slot| slot.clone())
    }

    /// The request's verified principal, when it is `actor` and the declared
    /// table grants it something in this cell.
    fn member(&self, actor: &str) -> Option<Caller> {
        self.caller
            .current()
            .filter(|caller| caller.subject_id == actor)
            .filter(|caller| !self.table.capabilities(&caller.roles).is_empty())
    }

    /// An object read inside an action: the caller's Values grant on the
    /// object's type and id, decided by the read PDP.
    fn action_reads(&self, actor: &str, object_type: &str, id: &str) -> bool {
        let Some(pdp) = self.pdp_for(actor) else {
            return false;
        };
        let mut request =
            collection_access_key(&self.key, actor, ReadCapability::Values, object_type);
        id.clone_into(&mut request.resource.id);
        authz::authorize(&pdp, &request).is_ok()
    }

    /// Read the request's verified principal from `slot` instead of this
    /// policy's own: a cell whose resolver existed before its policy.
    #[must_use]
    pub fn with_caller(mut self, slot: Arc<CallerSlot>) -> Self {
        self.caller = slot;
        self
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

/// Whether the request's verified principal is `actor` and holds a declared
/// provisioner role.
fn provisioner(table: &GrantTable, caller: &CallerSlot, actor: &str) -> bool {
    caller
        .current()
        .filter(|caller| caller.subject_id == actor)
        .is_some_and(|caller| table.provisions(&caller.roles))
}

/// The server policy of a scope with no cell: the declared provisioners may
/// create it, and nothing else is granted to anyone.
pub struct Provisioners {
    table: &'static GrantTable,
    caller: Arc<CallerSlot>,
}

impl Provisioners {
    /// The provisioner grant of `table`, read against the principal the
    /// resolver records in `caller`.
    #[must_use]
    pub const fn new(table: &'static GrantTable, caller: Arc<CallerSlot>) -> Self {
        Self { table, caller }
    }
}

impl Authorizer for Provisioners {
    fn authorize(&self, request: &AccessRequest<'_>) -> Result<(), Refusal> {
        let granted = matches!(request.operation, Operation::ProvisionCell)
            && request
                .actor
                .is_some_and(|actor| provisioner(self.table, &self.caller, actor));
        if granted {
            Ok(())
        } else {
            DenyAll.authorize(request)
        }
    }
}

impl Authorizer for ReadPolicy {
    fn authorize(&self, request: &AccessRequest<'_>) -> Result<(), Refusal> {
        let granted = match request.operation {
            Operation::ReadDefinitions => request
                .actor
                .is_some_and(|actor| self.pdp_for(actor).is_some()),
            Operation::WriteDefinitions => request.actor.is_some_and(|actor| {
                self.caller
                    .current()
                    .filter(|caller| caller.subject_id == actor)
                    .is_some_and(|caller| self.table.writes_definitions(&caller.roles))
            }),
            Operation::ProvisionCell => request
                .actor
                .is_some_and(|actor| provisioner(self.table, &self.caller, actor)),
            // The gate: a verified member of the cell's organization, and an
            // action the scope's policy governs. The kernel callback below
            // decides the call itself, per target.
            Operation::CallAction(ref action_id) => request.actor.is_some_and(|actor| {
                self.member(actor).is_some()
                    && self
                        .action_policy()
                        .is_some_and(|policy| policy.callable(action_id))
            }),
        };
        if granted {
            Ok(())
        } else {
            DenyAll.authorize(request)
        }
    }

    fn action(&self, request: &ActionAuthzRequest<'_>) -> Result<(), ActionDenial> {
        let denied = || ActionDenial::Forbidden {
            reason: norn_api::refusal::codes::AUTHZ_DENIED.into(),
        };
        let (Some(caller), Some(policy)) = (self.member(request.actor), self.action_policy())
        else {
            return Err(denied());
        };
        policy.decide(&caller, request)
    }

    fn action_read(&self, request: &ActionReadAuthzRequest<'_>) -> Result<(), ActionDenial> {
        if self.action_reads(
            request.actor,
            request.target.object_type.as_str(),
            &request.target.key,
        ) {
            Ok(())
        } else {
            Err(ActionDenial::Invisible)
        }
    }

    fn action_read_type(
        &self,
        request: &ActionReadTypeAuthzRequest<'_>,
    ) -> Result<(), ActionDenial> {
        let object_type = request.object_type.as_str();
        if self.action_reads(request.actor, object_type, object_type) {
            Ok(())
        } else {
            Err(ActionDenial::Invisible)
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
    collection_access_key(&scope_key(scope), subject, capability, object_type)
}

fn collection_access_key(
    key: &ScopeKey,
    subject: &str,
    capability: ReadCapability,
    object_type: &str,
) -> norn_kernel::authz::AccessRequest {
    let key = key.clone();
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

/// The cell's object read mapping (HOST-READ-2/3): the collection request, or
/// for one row the same request naming that row's id and type. Nothing in a
/// request chooses the principal, scope or capability.
pub struct CellObjectAccess;

impl norn_api::object_routes::ObjectAccess for CellObjectAccess {
    fn access(
        &self,
        scope: &CellScope,
        actor: &str,
        capability: ReadCapability,
        object_type: &str,
        row: Option<&norn_cell::ObjectRecord>,
    ) -> Result<norn_kernel::authz::AccessRequest, Refusal> {
        let mut request = collection_access(scope, actor, capability, object_type);
        if let Some(row) = row {
            row.object_type
                .clone_into(&mut request.resource.object_type);
            row.id.clone_into(&mut request.resource.id);
        }
        Ok(request)
    }
}
