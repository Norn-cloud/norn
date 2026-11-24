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
//! declared writer roles. Starting a flow run is [`Operation::StartRun`],
//! granted only to the declared starter roles of [`RUN_STARTERS`] that also
//! hold the Values grant (a starter can read the run it starts). Deciding a
//! decision is [`Operation::Decide`], granted to a caller holding one of the
//! decision's declared roles. Creating a cell is [`Operation::ProvisionCell`],
//! granted only to the table's declared provisioner roles: a scope with no
//! cell has no definitions, so this one authority is the host's
//! ([`Provisioners`] decides it before a cell exists, [`ReadPolicy`] after).
//! A refusal's full detail (law, subject, values) is disclosed only under the
//! declared [`REFUSAL_DISCLOSERS`] policy, decided by the same PDP; a scope
//! with no cell has no governed action, so [`Provisioners`] discloses nothing.

use std::sync::{Arc, Mutex, RwLock};

use norn_api::authz::{
    AccessRequest, Authorizer, DenyAll, Operation, ReadCapability, ReadRequest,
    RefusalDetailRequest,
};
use norn_api::refusal::Refusal;
use norn_api::scope::CellScope;
use norn_ir::access::{
    AccessCondition, AccessIr, AccessRule, AccessScope, DISCLOSE_REFUSAL_DETAIL_ACTION,
    REFUSAL_DETAIL_RESOURCE, RoleCondition, ScopeDimension,
};
use norn_ir::{IrVersion01, ObjectTypeDef, ObjectTypeName};
use norn_kernel::authz::{
    self, ActorKind, AttrValue, AuthzSchema, CapabilityGrant, Delegation, Pdp, PrincipalInput,
    ResourceInput, ScopeKey,
};
use norn_kernel::{
    ActionAuthzRequest, ActionDenial, ActionReadAuthzRequest, ActionReadTypeAuthzRequest,
};

use crate::actions::{ActionPolicy, collection_access_key};

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

/// Declared roles that may start a run of an installed flow (RUN-ROUTES-1).
/// A run holds no grants: its steps act under the starter's own authority, so
/// this decides only who may start one. A change is a new version.
#[derive(Debug)]
pub struct RunStarters {
    /// Identity of this table.
    pub version: &'static str,
    /// Norn roles that may start runs.
    pub roles: &'static [&'static str],
}

/// The demo cells' run starters over `studio-roles/1`: members, admins and
/// owners; a viewer, who cannot read a run's values, starts none.
pub const RUN_STARTERS: RunStarters = RunStarters {
    version: "norn-cell-do-run-grants/1",
    roles: &["member", "admin", "owner"],
};

impl RunStarters {
    /// Whether `roles` include a declared starter that also holds the Values
    /// grant in `table`.
    #[must_use]
    pub fn starts(&self, table: &GrantTable, roles: &[String]) -> bool {
        table.capabilities(roles).contains(&ReadCapability::Values)
            && self
                .roles
                .iter()
                .any(|starter| roles.iter().any(|held| held == starter))
    }
}

/// Declared roles whose callers see the full refusal detail of a governed
/// action they call (DIAG-DISCLOSE-1, #1353): the law, its subject and its
/// values, which Studio's refusal card reads ("72 vs 70", "Use 72"). Any
/// other caller gets the redacted refusal. A change is a new version.
#[derive(Debug)]
pub struct RefusalDisclosers {
    /// Identity of this table; also the compiled policy's id.
    pub version: &'static str,
    /// Norn roles granted `norn.disclose_refusal_detail`.
    pub roles: &'static [&'static str],
}

/// The demo cells' disclosers over `studio-roles/1`: admins and owners, the
/// roles that author the cell's definitions and so already read its laws.
pub const REFUSAL_DISCLOSERS: RefusalDisclosers = RefusalDisclosers {
    version: "norn-cell-do-refusal-detail/1",
    roles: &["admin", "owner"],
};

impl RefusalDisclosers {
    /// This table as an `AccessIR` policy: one rule granting
    /// `norn.disclose_refusal_detail` on `norn.RefusalDetail` to the declared
    /// roles in the cell's org and environment.
    #[must_use]
    pub fn access_policy(&self) -> AccessIr {
        AccessIr {
            access_version: IrVersion01::V0_1,
            policy_id: self.version.into(),
            rules: vec![AccessRule {
                rule_id: "refusal_detail".into(),
                actions: vec![DISCLOSE_REFUSAL_DETAIL_ACTION.into()],
                resource_type: ObjectTypeName(REFUSAL_DETAIL_RESOURCE.to_owned()),
                scope: AccessScope {
                    dimensions: vec![ScopeDimension::Org, ScopeDimension::Environment],
                },
                all_of: vec![AccessCondition::Role(RoleCondition {
                    any_of: self.roles.iter().map(|role| (*role).to_owned()).collect(),
                })],
            }],
            generic_rules: vec![],
            extensions: None,
        }
    }

    /// The compiled policy with no grant; each decision adds its caller's.
    fn compile(&self) -> Result<Pdp, SeedError> {
        let projection = norn_projections::project_cedar_schema(&[], &[], &[]);
        let host = norn_projections::project_host_access_schema(&[]);
        let schema =
            AuthzSchema::from_projected_cedarschema_fragments(&[&projection.schema, &host])
                .map_err(|e| SeedError(format!("refusal detail schema: {e}")))?;
        let compiled = authz::compile(&schema, &self.access_policy())
            .map_err(|e| SeedError(format!("refusal detail policy: {e}")))?;
        Pdp::new(compiled, &[]).map_err(|e| SeedError(format!("refusal detail policy: {e}")))
    }
}

/// Declared connector grants over `studio-roles/1` (#1036): who may see a
/// workspace's connectors, who may bind a tool, and who may run a binding of
/// each `runRole`. Studio asks through `POST /v1/access:decide` and holds no
/// role logic; a change is a new version.
#[derive(Debug)]
pub struct ConnectorGrants {
    /// Identity of this table.
    pub version: &'static str,
    /// Roles that may see the workspace's connectors and bindings.
    pub read: &'static [&'static str],
    /// Roles that may add, edit and remove tool bindings.
    pub bind: &'static [&'static str],
    /// For each binding `runRole`, the roles that may run it.
    pub run: &'static [(&'static str, &'static [&'static str])],
}

/// The demo cells' connector grants: every member reads, admins and owners
/// bind, and a binding runs for its `runRole` and the roles above it.
pub const CONNECTOR_GRANTS: ConnectorGrants = ConnectorGrants {
    version: "norn-cell-do-connector-grants/1",
    read: &["viewer", "member", "admin", "owner"],
    bind: &["admin", "owner"],
    run: &[
        ("member", &["member", "admin", "owner"]),
        ("admin", &["admin", "owner"]),
    ],
};

/// The host's Cedar fragment for connector checks: Studio's workspace and
/// tool-binding resources and the three connector actions, alongside the
/// projected `Norn` base (actor, scope, roles).
const CONNECTOR_SCHEMA: &str = "namespace studio {\n  entity Workspace = {\n    scope: Norn::Scope\n  };\n  entity ToolBinding = {\n    scope: Norn::Scope\n  };\n}\nnamespace Norn {\n  action \"connector.read\" appliesTo {\n    principal: [Actor],\n    resource: [studio::Workspace],\n    context: {\n      scope: Scope\n    }\n  };\n  action \"connector.bind\" appliesTo {\n    principal: [Actor],\n    resource: [studio::Workspace],\n    context: {\n      scope: Scope\n    }\n  };\n  action \"connector.run\" appliesTo {\n    principal: [Actor],\n    resource: [studio::ToolBinding],\n    context: {\n      scope: Scope,\n      runRole: String\n    }\n  };\n}\n";

/// The `AccessIR` resource type of a workspace's connector checks.
pub const WORKSPACE_RESOURCE: &str = "studio.Workspace";
/// The `AccessIR` resource type of a tool binding's run check.
pub const TOOL_BINDING_RESOURCE: &str = "studio.ToolBinding";

impl ConnectorGrants {
    /// This table as an `AccessIR` policy: one rule per action (and per
    /// binding `runRole`), each a role condition in the cell's org and
    /// environment; a run rule also requires the binding's `runRole`.
    #[must_use]
    pub fn access_policy(&self) -> AccessIr {
        let granted_to = |granted: &[&str]| {
            AccessCondition::Role(RoleCondition {
                any_of: granted.iter().map(|role| (*role).to_owned()).collect(),
            })
        };
        let scope = || AccessScope {
            dimensions: vec![ScopeDimension::Org, ScopeDimension::Environment],
        };
        let mut rules = vec![
            AccessRule {
                rule_id: "connector_read".into(),
                actions: vec!["connector.read".into()],
                resource_type: ObjectTypeName(WORKSPACE_RESOURCE.to_owned()),
                scope: scope(),
                all_of: vec![granted_to(self.read)],
            },
            AccessRule {
                rule_id: "connector_bind".into(),
                actions: vec!["connector.bind".into()],
                resource_type: ObjectTypeName(WORKSPACE_RESOURCE.to_owned()),
                scope: scope(),
                all_of: vec![granted_to(self.bind)],
            },
        ];
        for (run_role, granted) in self.run {
            rules.push(AccessRule {
                rule_id: format!("run_{run_role}"),
                actions: vec!["connector.run".into()],
                resource_type: ObjectTypeName(TOOL_BINDING_RESOURCE.to_owned()),
                scope: scope(),
                all_of: vec![
                    granted_to(granted),
                    AccessCondition::Context(norn_ir::access::ContextCondition {
                        name: "runRole".into(),
                        op: norn_ir::access::CompareOp::Eq,
                        value: norn_ir::access::AccessLiteral::String((*run_role).to_owned()),
                    }),
                ],
            });
        }
        AccessIr {
            access_version: IrVersion01::V0_1,
            policy_id: self.version.into(),
            rules,
            generic_rules: vec![],
            extensions: None,
        }
    }

    /// The compiled policy for the cell of `scope`, with no grant.
    ///
    /// # Errors
    /// The schema or the policy does not compile.
    pub fn compile(&'static self, scope: &CellScope) -> Result<ConnectorPolicy, SeedError> {
        let projection = norn_projections::project_cedar_schema(&[], &[], &[]);
        let schema = AuthzSchema::from_projected_cedarschema_fragments(&[
            &projection.schema,
            CONNECTOR_SCHEMA,
        ])
        .map_err(|e| SeedError(format!("connector schema: {e}")))?;
        let compiled = authz::compile(&schema, &self.access_policy())
            .map_err(|e| SeedError(format!("connector policy: {e}")))?;
        Ok(ConnectorPolicy {
            base: Pdp::new(compiled, &[])
                .map_err(|e| SeedError(format!("connector policy: {e}")))?,
            key: scope_key(scope),
            grants: self,
        })
    }
}

/// [`CONNECTOR_GRANTS`] compiled for one cell: Studio's connector checks
/// (`POST /v1/access:decide`), each decided by the kernel PDP for the
/// request's verified principal and its own declared roles.
pub struct ConnectorPolicy {
    base: Pdp,
    key: ScopeKey,
    grants: &'static ConnectorGrants,
}

impl std::fmt::Debug for ConnectorPolicy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ConnectorPolicy")
            .field("version", &self.grants.version)
            .finish_non_exhaustive()
    }
}

impl ConnectorPolicy {
    /// The rule that permits `caller` to take `action` on the resource
    /// `(kind, id)`, or `None` (deny). Fails closed before the PDP for an
    /// action or resource type the policy does not declare, a workspace other
    /// than this cell's organization, and a run whose binding names no
    /// declared `runRole` (`run_role`, read by the host from the binding's
    /// own definition). Otherwise the PDP decides, with only `action` as the
    /// capability and only the caller's own roles.
    #[must_use]
    pub fn decide(
        &self,
        caller: &Caller,
        action: &str,
        (kind, id): (&str, &str),
        run_role: Option<&str>,
    ) -> Option<String> {
        let (rule, context) = match (action, kind) {
            ("connector.read" | "connector.bind", WORKSPACE_RESOURCE) if id == self.key.org => (
                if action == "connector.read" {
                    "connector_read".to_owned()
                } else {
                    "connector_bind".to_owned()
                },
                std::collections::BTreeMap::new(),
            ),
            ("connector.run", TOOL_BINDING_RESOURCE) => {
                let run_role = run_role
                    .filter(|role| self.grants.run.iter().any(|(declared, _)| declared == role))?;
                (
                    format!("run_{run_role}"),
                    std::collections::BTreeMap::from([(
                        "runRole".to_owned(),
                        AttrValue::String(run_role.to_owned()),
                    )]),
                )
            }
            _ => return None,
        };
        let actor_kind = match caller.actor_kind {
            "user" => ActorKind::User,
            "service" => ActorKind::Service,
            _ => return None,
        };
        let pdp = Pdp::new(
            self.base.policy_set().clone(),
            &[CapabilityGrant {
                subject_id: caller.subject_id.clone(),
                actor_kind,
                agent_session_id: None,
                scope: Some(self.key.clone()),
                capabilities: vec![action.to_owned()],
                binding: None,
            }],
        )
        .ok()?;
        let request = authz::AccessRequest {
            principal: PrincipalInput {
                subject_id: caller.subject_id.clone(),
                actor_kind,
                agent_session_id: None,
                roles: caller.roles.clone(),
                attributes: std::collections::BTreeMap::new(),
            },
            scope: Some(self.key.clone()),
            capability: action.to_owned(),
            action_id: action.to_owned(),
            resource: ResourceInput {
                object_type: kind.to_owned(),
                id: id.to_owned(),
                scope: self.key.clone(),
                attributes: std::collections::BTreeMap::new(),
            },
            context,
            binding: None,
            delegation: Delegation::Direct,
        };
        matches!(pdp.decide(&request), norn_kernel::authz::PdpDecision::Allow).then_some(rule)
    }
}

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
    /// AUTH-INDEPENDENT-PERSON (#618): the accountable person the verified
    /// token names, if it names one.
    pub accountable_person: Option<String>,
}

/// Where the resolver records the current request's verified principal for
/// the policy. Every request records first (possibly nobody), so a caller
/// never outlives its request. A request runs on one thread from resolution
/// to answer, and the slot keeps each thread's caller apart, so requests a
/// host serves concurrently (the dev host's workers, DEV-CONCURRENT-1) never
/// see each other's caller.
#[derive(Debug, Default)]
pub struct CallerSlot(Mutex<Vec<(Option<std::thread::ThreadId>, Caller)>>);

impl CallerSlot {
    /// Record the request's verified principal, or nobody, for the calling
    /// thread's request.
    pub fn record(&self, caller: Option<Caller>) {
        let me = this_thread();
        if let Ok(mut slot) = self.0.lock() {
            slot.retain(|(thread, _)| *thread != me);
            if let Some(caller) = caller {
                slot.push((me, caller));
            }
        }
    }

    /// The calling thread's request's verified principal, if any.
    #[must_use]
    pub fn current(&self) -> Option<Caller> {
        let me = this_thread();
        self.0.lock().ok().and_then(|slot| {
            slot.iter()
                .find(|(thread, _)| *thread == me)
                .map(|(_, caller)| caller.clone())
        })
    }
}

/// The calling thread, on a host with threads; the Durable Object cell has
/// one thread and serves one request at a time.
fn this_thread() -> Option<std::thread::ThreadId> {
    (!cfg!(target_arch = "wasm32")).then(|| std::thread::current().id())
}

/// The compiled read rules with no grant, one per object type of the cell's
/// model. They are recompiled when an install changes the model
/// ([`Authorizer::model_changed`]), so a type added after the cell opened (a
/// flow's Run type) is decided like every other; until then it is denied.
///
/// Each read decision asks for the PDP of its caller's grant. Building one
/// clones the compiled rules and compiles the grant (it parses the
/// capability schema), a few milliseconds; a list or the catalog's counts
/// decide twice per row, so the PDP of each grant is built once per rules
/// revision and kept in `granted` (#1385). A PDP is a pure value of the
/// rules and the grant, so a kept one decides exactly as a fresh one would.
struct ReadRules {
    rules: RwLock<Pdp>,
    /// The PDP of each grant over the current `rules`, emptied under the
    /// rules' write lock whenever they are replaced.
    granted: Mutex<GrantedPdps>,
}

/// One grant: its subject and the capability names the table gives its roles.
type GrantKey = (String, Vec<&'static str>);

/// The kept PDPs by grant; at most [`GRANTED_PDPS`] are kept at once.
type GrantedPdps = std::collections::HashMap<GrantKey, Arc<Pdp>>;

/// Grants whose PDP is kept at once. A cell has a handful of active callers;
/// past this many the kept PDPs are dropped and rebuilt on demand.
const GRANTED_PDPS: usize = 256;

impl ReadRules {
    fn compile<'a>(
        object_types: impl IntoIterator<Item = &'a ObjectTypeDef>,
    ) -> Result<Pdp, SeedError> {
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
        Pdp::new(compiled, &[]).map_err(|e| SeedError(format!("read policy: {e}")))
    }

    /// Recompile the rules over the model's current `object_types`. On a
    /// refusal the previous rules stay in force, so a new type stays denied.
    ///
    /// # Errors
    /// The projected schema or the policy does not compile.
    fn recompile<'a>(
        &self,
        object_types: impl IntoIterator<Item = &'a ObjectTypeDef>,
    ) -> Result<(), SeedError> {
        let compiled = Self::compile(object_types)?;
        let mut rules = self
            .rules
            .write()
            .map_err(|_poisoned| SeedError("read rules: poisoned".to_owned()))?;
        *rules = compiled;
        // Emptied while the write lock is held: a reader holds the read lock
        // from reading the rules until it keeps their PDP, so no PDP of the
        // replaced rules is kept past this point.
        self.granted
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clear();
        Ok(())
    }

    fn new(rules: Pdp) -> Self {
        Self {
            rules: RwLock::new(rules),
            granted: Mutex::default(),
        }
    }

    /// The PDP of `subject`'s grant of `capabilities` in `key`, over the
    /// current rules: the kept one, or built and kept. `None` (deny) when the
    /// rules are poisoned or the grant does not compile.
    fn granted(
        &self,
        key: &ScopeKey,
        subject: &str,
        capabilities: &[ReadCapability],
    ) -> Option<Arc<Pdp>> {
        let rules = self.rules.read().ok()?;
        let grant: GrantKey = (
            subject.to_owned(),
            capabilities.iter().copied().map(capability_name).collect(),
        );
        let mut granted = self
            .granted
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(pdp) = granted.get(&grant) {
            return Some(Arc::clone(pdp));
        }
        let pdp = Arc::new(
            Pdp::new(
                rules.policy_set().clone(),
                &[CapabilityGrant {
                    subject_id: grant.0.clone(),
                    actor_kind: ActorKind::User,
                    agent_session_id: None,
                    scope: Some(key.clone()),
                    capabilities: grant.1.iter().map(|&name| name.to_owned()).collect(),
                    binding: None,
                }],
            )
            .ok()?,
        );
        if granted.len() >= GRANTED_PDPS {
            granted.clear();
        }
        granted.insert(grant, Arc::clone(&pdp));
        Some(pdp)
    }
}

/// The cell's action policy for its active definitions, replaced when they
/// change; `None` serves no action.
pub type ActionSlot = Arc<RwLock<Option<Arc<ActionPolicy>>>>;

/// The cell's reveal policy for its active definitions (PRIVACY-2), replaced
/// when they change; `None` reveals nothing.
pub type RevealSlot = Arc<RwLock<Option<Arc<crate::reveal::RevealPolicy>>>>;

/// The compiled policy for one cell.
pub struct ReadPolicy {
    /// The compiled rules with no grant; each request adds its caller's.
    base: Arc<ReadRules>,
    key: ScopeKey,
    table: &'static GrantTable,
    caller: Arc<CallerSlot>,
    actions: ActionSlot,
    reveal: RevealSlot,
    /// [`REFUSAL_DISCLOSERS`] compiled, with no grant.
    disclosure: Pdp,
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
        Ok(Self {
            base: Arc::new(ReadRules::new(ReadRules::compile(object_types)?)),
            key: scope_key(scope),
            table,
            caller: Arc::default(),
            actions: Arc::default(),
            reveal: Arc::default(),
            disclosure: REFUSAL_DISCLOSERS.compile()?,
        })
    }

    /// The slot this cell's action policy is installed into; empty until the
    /// host installs one, and then no action is served.
    #[must_use]
    pub fn action_slot(&self) -> ActionSlot {
        Arc::clone(&self.actions)
    }

    /// The slot this cell's reveal policy is installed into; empty until the
    /// host installs one, and then nothing is revealable.
    #[must_use]
    pub fn reveal_slot(&self) -> RevealSlot {
        Arc::clone(&self.reveal)
    }

    /// The read PDP as `GET /v1/access?objectType=` asks it about one
    /// declared role at a time ([`RoleReads`]); it follows the rules'
    /// recompiles, so a type added later is decided like every other.
    #[must_use]
    pub fn role_reads(&self) -> RoleReads {
        RoleReads {
            base: Arc::clone(&self.base),
            key: self.key.clone(),
            table: self.table,
        }
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

    /// Whether the request's verified principal may read objects of
    /// `object_type` at all. The kernel's own call records (who called which
    /// action, with which input and idempotency key) are read by the table's
    /// definition writers (admin, owner) only: a member or viewer never sees
    /// another caller's inputs or keys. Every other type follows the grants.
    fn reads_type(&self, subject: &str, object_type: &str) -> bool {
        let kernel_record = object_type == norn_kernel::ACTION_CALL_TYPE
            || object_type == norn_kernel::OWNED_ROWS_TYPE;
        !kernel_record
            || self
                .caller
                .current()
                .filter(|caller| caller.subject_id == subject)
                .is_some_and(|caller| self.table.writes_definitions(&caller.roles))
    }

    /// An object read inside an action: the caller's Values grant on the
    /// object's type and id, decided by the read PDP.
    fn action_reads(&self, actor: &str, object_type: &str, id: &str) -> bool {
        let Some(pdp) = self
            .pdp_for(actor)
            .filter(|_| self.reads_type(actor, object_type))
        else {
            return false;
        };
        let mut request =
            collection_access_key(&self.key, actor, ReadCapability::Values, object_type);
        id.clone_into(&mut request.resource.id);
        authz::authorize(&pdp, &request).is_ok()
    }

    /// The trusted disclosure request for `request`: the verified caller, its
    /// declared roles and a `norn.disclose_refusal_detail` grant in this
    /// cell's scope, asking for the diagnostic of an action the cell's policy
    /// governs. `None` (deny) for any other caller or action.
    fn disclosure_request(
        &self,
        request: &RefusalDetailRequest<'_>,
    ) -> Option<(Pdp, authz::AccessRequest)> {
        let caller = self.member(request.actor)?;
        self.action_policy()
            .filter(|policy| policy.callable(request.action_id))?;
        let actor_kind = match caller.actor_kind {
            "user" => ActorKind::User,
            "service" => ActorKind::Service,
            _ => return None,
        };
        let pdp = Pdp::new(
            self.disclosure.policy_set().clone(),
            &[CapabilityGrant {
                subject_id: caller.subject_id.clone(),
                actor_kind,
                agent_session_id: None,
                scope: Some(self.key.clone()),
                capabilities: vec![DISCLOSE_REFUSAL_DETAIL_ACTION.to_owned()],
                binding: None,
            }],
        )
        .ok()?;
        let access = authz::AccessRequest {
            principal: PrincipalInput {
                subject_id: caller.subject_id,
                actor_kind,
                agent_session_id: None,
                roles: caller.roles,
                attributes: std::collections::BTreeMap::new(),
            },
            scope: Some(self.key.clone()),
            capability: DISCLOSE_REFUSAL_DETAIL_ACTION.to_owned(),
            action_id: DISCLOSE_REFUSAL_DETAIL_ACTION.to_owned(),
            resource: ResourceInput {
                object_type: REFUSAL_DETAIL_RESOURCE.to_owned(),
                id: request.action_id.to_owned(),
                scope: self.key.clone(),
                attributes: std::collections::BTreeMap::new(),
            },
            context: std::collections::BTreeMap::from([(
                "governedAction".to_owned(),
                AttrValue::String(request.action_id.into()),
            )]),
            binding: None,
            delegation: Delegation::Direct,
        };
        Some((pdp, access))
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
    fn pdp_for(&self, subject: &str) -> Option<Arc<Pdp>> {
        let caller = self
            .caller
            .current()
            .filter(|caller| caller.subject_id == subject)?;
        let capabilities = self.table.capabilities(&caller.roles);
        if capabilities.is_empty() {
            return None;
        }
        self.base
            .granted(&self.key, &caller.subject_id, &capabilities)
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
            // CONN-AUTHZ-1: the roles that write definitions (admins and
            // owners) manage connection credentials, every connection of the
            // cell, so they also revoke a grant siblings share
            // (GRANT-REVOKE-1).
            Operation::WriteDefinitions
            | Operation::SyncSource(_)
            | Operation::ManageConnection(_)
            | Operation::RevokeConnectionGrant(_) => request.actor.is_some_and(|actor| {
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
            Operation::StartRun(_) => request.actor.is_some_and(|actor| {
                self.caller
                    .current()
                    .filter(|caller| caller.subject_id == actor)
                    .is_some_and(|caller| RUN_STARTERS.starts(self.table, &caller.roles))
            }),
            // A decider holds one of the decision's declared roles, verified
            // for this request, and some grant of the table.
            Operation::Decide { ref roles, .. } => request.actor.is_some_and(|actor| {
                self.caller
                    .current()
                    .filter(|caller| caller.subject_id == actor)
                    .is_some_and(|caller| {
                        !self.table.capabilities(&caller.roles).is_empty()
                            && caller.roles.iter().any(|held| roles.contains(held))
                    })
            }),
        };
        if granted {
            Ok(())
        } else {
            DenyAll.authorize(request)
        }
    }

    fn model_changed(&self, object_types: &[ObjectTypeDef]) {
        // A failed recompile keeps the previous rules: the new types stay denied.
        let _kept = self.base.recompile(object_types);
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

    fn verified_principal(&self, actor: &str) -> Option<norn_cell::flow_run::RunStarter> {
        let caller = self.caller.current();
        caller
            .filter(|caller| caller.subject_id == actor)
            .map(crate::runs::principal_of)
    }

    fn admits_model(&self, object_types: &[ObjectTypeDef]) -> Result<(), Refusal> {
        ReadRules::compile(object_types)
            .map(|_| ())
            .map_err(|error| {
                Refusal::new(
                    http::StatusCode::CONFLICT,
                    "defs.migration_refused",
                    "the candidate definitions cannot carry the cell's live data",
                    format!(
                        "the cell's read rules do not compile for the candidate model: {}",
                        error.0
                    ),
                )
            })
    }

    fn reveal(&self, actor: &str, marking_id: &str) -> bool {
        let Some(caller) = self.member(actor) else {
            return false;
        };
        self.reveal
            .read()
            .ok()
            .and_then(|slot| slot.clone())
            .is_some_and(|policy| policy.reveals(&caller, marking_id))
    }

    fn disclose_refusal_detail(&self, request: &RefusalDetailRequest<'_>) -> bool {
        // The host's scope is the request's: a refusal of another cell is
        // never this policy's to disclose.
        scope_key(request.scope) == self.key
            && self
                .disclosure_request(request)
                .is_some_and(|(pdp, access)| {
                    norn_api::refusal_disclosure::pdp_discloses(&pdp, request, &access)
                })
    }

    fn authorize_automation_grant(
        &self,
        request: &norn_api::authz::AutomationGrantRequest<'_>,
    ) -> Result<norn_api::authz::VerifiedAutomationGrant, Refusal> {
        if scope_key(request.scope) != self.key {
            return Err(norn_api::authz::read_denied());
        }
        let parent_grant_ids = self
            .member(request.actor)
            .zip(self.action_policy())
            .and_then(|(caller, policy)| {
                policy.authorize_automation_grant(
                    &caller,
                    &request.action.action_id,
                    request.matched_types,
                )
            })
            .ok_or_else(norn_api::authz::read_denied)?;
        Ok(norn_api::authz::VerifiedAutomationGrant { parent_grant_ids })
    }

    fn read(&self, request: &ReadRequest<'_>) -> Result<(), Refusal> {
        let asked = request.access.capability == capability_name(request.capability)
            && request.access.action_id == READ_ACTION
            && self.reads_type(
                &request.access.principal.subject_id,
                &request.access.resource.object_type,
            );
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

/// The subject id a declared role is decided under when no caller holds it:
/// a role is asked about, never a person, so no grant of a real subject is
/// built or kept for it.
#[must_use]
pub fn role_subject(role: &str) -> String {
    format!("norn:role/{role}")
}

/// The cell's read PDP, asked what one declared role alone may read
/// (`GET /v1/access?objectType=`). Each answer is the PDP's own decision for
/// a principal holding exactly the grant the table gives that role, as a
/// caller holding only that role would be decided; nothing here grants.
pub struct RoleReads {
    base: Arc<ReadRules>,
    key: ScopeKey,
    table: &'static GrantTable,
}

impl std::fmt::Debug for RoleReads {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RoleReads")
            .field("table", &self.table.version)
            .finish_non_exhaustive()
    }
}

impl RoleReads {
    /// The roles the declared table names, in its order.
    #[must_use]
    pub fn roles(&self) -> Vec<&'static str> {
        self.table.entries.iter().map(|(role, _)| *role).collect()
    }

    /// The table's identity.
    #[must_use]
    pub fn table(&self) -> &'static str {
        self.table.version
    }

    /// Whether `role` alone may read `capability` of `object_type`: the PDP's
    /// decision, as for any caller holding only that role. The kernel's own
    /// call records are read by definition writers only, as for a caller.
    #[must_use]
    pub fn permits(&self, role: &str, capability: ReadCapability, object_type: &str) -> bool {
        let roles = [role.to_owned()];
        let kernel_record = object_type == norn_kernel::ACTION_CALL_TYPE
            || object_type == norn_kernel::OWNED_ROWS_TYPE;
        if kernel_record && !self.table.writes_definitions(&roles) {
            return false;
        }
        let capabilities = self.table.capabilities(&roles);
        if capabilities.is_empty() {
            return false;
        }
        let subject = role_subject(role);
        self.base
            .granted(&self.key, &subject, &capabilities)
            .is_some_and(|pdp| {
                authz::authorize(
                    &pdp,
                    &collection_access_key(&self.key, &subject, capability, object_type),
                )
                .is_ok()
            })
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

#[cfg(test)]
#[allow(clippy::expect_used, reason = "caller slot scenarios")]
mod caller_slot_tests {
    use super::{Caller, CallerSlot};

    fn caller(subject: &str) -> Caller {
        Caller {
            subject_id: subject.to_owned(),
            roles: Vec::new(),
            actor_kind: "user",
            role_table: "test",
            accountable_person: Some(subject.to_owned()),
        }
    }

    /// DEV-CONCURRENT-1: two requests in flight on two threads each see
    /// their own caller, and ending one never ends the other's.
    #[test]
    fn concurrent_requests_never_see_each_others_caller() {
        let slot = CallerSlot::default();
        let (arrived, go) = (std::sync::Barrier::new(2), std::sync::Barrier::new(2));
        std::thread::scope(|scope| {
            for subject in ["alice", "bob"] {
                let (slot, arrived, go) = (&slot, &arrived, &go);
                scope.spawn(move || {
                    slot.record(Some(caller(subject)));
                    arrived.wait();
                    // Both are recorded now; each still reads its own.
                    assert_eq!(
                        slot.current().map(|c| c.subject_id),
                        Some(subject.to_owned())
                    );
                    go.wait();
                    slot.record(None);
                    assert_eq!(slot.current(), None);
                });
            }
        });
        assert_eq!(slot.current(), None, "this thread recorded nobody");
    }
}
