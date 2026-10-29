//! Norn-typed PDP inputs (T5-AZ-3, #2578). No `cedar-policy` type crosses the
//! crate boundary: shells and the kernel build these plain values and the PDP
//! lowers them to Cedar entities, context, and requests.

use std::collections::BTreeMap;

use norn_ir::ObjectTypeName;
use norn_ir::entitlement::Entitlement;

/// The closed set of actor kinds (ruling 2026-08-23 §6).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ActorKind {
    /// A human user.
    User,
    /// An AI agent acting for a principal.
    Agent,
    /// The platform itself.
    System,
    /// A deterministic service.
    Service,
}

impl ActorKind {
    /// The wire string.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::User => "user",
            Self::Agent => "agent",
            Self::System => "system",
            Self::Service => "service",
        }
    }

    /// Parse the wire string; `None` for anything outside the closed set.
    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "user" => Some(Self::User),
            "agent" => Some(Self::Agent),
            "system" => Some(Self::System),
            "service" => Some(Self::Service),
            _ => None,
        }
    }
}

/// The kernel's five-dimension scope key. Every value is exact: an absent
/// dimension matches only an absent dimension; empty and `*` are rejected.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ScopeKey {
    /// Organization; required.
    pub org: String,
    /// Tenant.
    pub tenant: Option<String>,
    /// Workspace.
    pub workspace: Option<String>,
    /// Environment.
    pub environment: Option<String>,
    /// Business unit.
    pub business_unit: Option<String>,
}

/// One entitlement row and the exact physical cell scope in which it was
/// observed. Entitlement metadata itself remains portable in `norn-ir`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EntitlementInput {
    /// Canonical entitlement row.
    pub entitlement: Entitlement,
    /// Exact five-dimension Norn scope supplied by the trusted snapshot.
    pub scope: ScopeKey,
}

/// The admitted business catalog against which Core generic rules expand.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EntitlementCatalog {
    /// Exact eligible action ids grouped by admitted business resource type.
    /// `ActionIR` entries already include each declared target and subtype;
    /// `norn.read` is added for every admitted resource type.
    pub resource_actions: BTreeMap<ObjectTypeName, Vec<String>>,
}

impl EntitlementCatalog {
    /// Build the entitlement catalog from a Cedar projection. Host actions
    /// in the reserved `norn.*` namespace (admin, observation,
    /// definition, delegation, and membership operations) are excluded;
    /// `norn.read` is included explicitly for every business type.
    #[must_use]
    pub fn from_projection(projection: &norn_projections::CedarSchemaProjection) -> Self {
        let resource_actions = projection
            .object_types
            .iter()
            .filter(|object_type| !object_type.as_str().starts_with("norn."))
            .map(|object_type| {
                let mut action_ids: std::collections::BTreeSet<_> = projection
                    .resource_actions
                    .get(object_type)
                    .into_iter()
                    .flatten()
                    .filter(|action_id| !action_id.starts_with("norn."))
                    .cloned()
                    .collect();
                action_ids.insert("norn.read".to_owned());
                (object_type.clone(), action_ids.into_iter().collect())
            })
            .collect();
        Self { resource_actions }
    }
}

impl ScopeKey {
    /// `(attribute, value)` for every present dimension, `org` first.
    pub(crate) fn dimensions(&self) -> Vec<(&'static str, &str)> {
        let mut out = vec![("org", self.org.as_str())];
        for (name, value) in self.optional_dimensions() {
            if let Some(value) = value {
                out.push((name, value));
            }
        }
        out
    }

    pub(crate) fn optional_dimensions(&self) -> [(&'static str, Option<&str>); 4] {
        [
            ("tenant", self.tenant.as_deref()),
            ("workspace", self.workspace.as_deref()),
            ("environment", self.environment.as_deref()),
            ("businessUnit", self.business_unit.as_deref()),
        ]
    }

    /// Whether every present value is one exact, non-wildcard value.
    pub(crate) fn is_exact(&self) -> bool {
        is_exact_value(&self.org)
            && self
                .optional_dimensions()
                .iter()
                .all(|(_, value)| value.is_none_or(is_exact_value))
    }
}

/// Commit-time binding of a capability grant to one action on one resource
/// under one policy version, authorization epoch, and logical time. Values
/// compare exactly (numbers are carried as their decimal text).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct CommitBinding {
    /// Action id; must equal [`AccessRequest::action_id`].
    pub action_id: String,
    /// Resource kind: the resource's object type; must equal
    /// [`ResourceInput::object_type`].
    pub resource_kind: String,
    /// Resource id; must equal [`ResourceInput::id`].
    pub resource_id: String,
    /// Artifact version of the resource, when it has one.
    pub artifact_version: Option<String>,
    /// Policy identity.
    pub policy_id: String,
    /// Policy version.
    pub policy_version: String,
    /// Resource version.
    pub resource_version: String,
    /// Authorization epoch.
    pub authorization_epoch: String,
    /// Logical time the policy was evaluated at.
    pub evaluated_at_logical_time: String,
}

impl CommitBinding {
    /// `(attribute, value)` for every required member.
    pub(crate) fn required(&self) -> [(&'static str, &str); 8] {
        [
            ("action", self.action_id.as_str()),
            ("resourceKind", self.resource_kind.as_str()),
            ("resourceId", self.resource_id.as_str()),
            ("policyId", self.policy_id.as_str()),
            ("policyVersion", self.policy_version.as_str()),
            ("resourceVersion", self.resource_version.as_str()),
            ("epoch", self.authorization_epoch.as_str()),
            ("evaluatedAt", self.evaluated_at_logical_time.as_str()),
        ]
    }
}

/// One scoped capability grant: the capability conjunct of the AND rule.
/// Semantics match `norn-engine`'s `authz_decision` exactly (subject, actor
/// kind, agent session, all five scope dimensions, optional commit binding,
/// capability membership; `None` is never a wildcard).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CapabilityGrant {
    /// Exact subject id.
    pub subject_id: String,
    /// Exact actor kind.
    pub actor_kind: ActorKind,
    /// Exact agent session; `None` matches only a session-less request.
    pub agent_session_id: Option<String>,
    /// Exact scope; a grant without a scope never matches.
    pub scope: Option<ScopeKey>,
    /// Granted capabilities; at least one.
    pub capabilities: Vec<String>,
    /// Commit binding; `None` matches only an unbound request.
    pub binding: Option<CommitBinding>,
}

/// A reference to an entity in a Cedar attribute value.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum EntityRef {
    /// A principal (`Norn::Actor`).
    Actor(String),
    /// An object of an `EntityIR` v2 object type (`a.b.C`).
    Object {
        /// The object type name.
        object_type: String,
        /// The object id.
        id: String,
    },
}

/// A typed attribute or context value. A link role with cardinality one is
/// an [`AttrValue::Entity`]; with cardinality many, an [`AttrValue::Set`] of
/// entities (the caller knows the `EntityIR` cardinality; the schema checks
/// it).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AttrValue {
    /// Boolean.
    Bool(bool),
    /// 64-bit integer.
    Long(i64),
    /// String.
    String(String),
    /// Entity reference.
    Entity(EntityRef),
    /// Set.
    Set(Vec<AttrValue>),
    /// Record.
    Record(BTreeMap<String, AttrValue>),
}

/// The acting principal. It is materialized for the request scope: its
/// `scope` attribute is the request scope, so its scope binding comes from
/// the capability conjunct (a grant must match the request scope exactly).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrincipalInput {
    /// Exact subject id.
    pub subject_id: String,
    /// Actor kind.
    pub actor_kind: ActorKind,
    /// Agent session, for agents acting in a session.
    pub agent_session_id: Option<String>,
    /// Role ids the principal holds in the request scope.
    pub roles: Vec<String>,
    /// Further attributes (principal-side link roles); `scope` is reserved.
    pub attributes: BTreeMap<String, AttrValue>,
}

/// The resource acted on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResourceInput {
    /// `EntityIR` v2 object type (`a.b.C`).
    pub object_type: String,
    /// Object id.
    pub id: String,
    /// The scope the object lives in.
    pub scope: ScopeKey,
    /// Further attributes (link roles, scalars); `scope` is reserved.
    pub attributes: BTreeMap<String, AttrValue>,
}

/// The delegation slot of the AND rule. Until Biscuit lands (ruling
/// 2026-08-27 §7, sequenced last) only a direct principal passes; any
/// presented token denies.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Delegation {
    /// The principal acts directly (no delegated token).
    Direct,
    /// A Biscuit token was presented. Denied until Biscuit verification exists.
    Biscuit(String),
}

/// One authorization request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccessRequest {
    /// The principal.
    pub principal: PrincipalInput,
    /// The request scope; `None` denies (absence is never a wildcard).
    pub scope: Option<ScopeKey>,
    /// The capability the action requires.
    pub capability: String,
    /// The `ActionIR` id.
    pub action_id: String,
    /// The resource.
    pub resource: ResourceInput,
    /// Typed context values (computed outside Cedar); `scope` is reserved.
    pub context: BTreeMap<String, AttrValue>,
    /// Commit binding, on the commit path.
    pub binding: Option<CommitBinding>,
    /// Delegation slot.
    pub delegation: Delegation,
}

/// One exact value: not empty after trimming and not the `*` wildcard (the
/// rule `norn-engine`'s validators apply).
pub(crate) fn is_exact_value(value: &str) -> bool {
    let trimmed = value.trim();
    !trimmed.is_empty() && trimmed != "*"
}
