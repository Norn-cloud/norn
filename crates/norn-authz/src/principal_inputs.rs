//! PRINCIPAL-INPUT-1 (#1091): the principal attribute catalog shared with
//! decisions, and the trusted resolver of a decision's declared principal
//! inputs.
//!
//! One catalog ([`principal_attribute_catalog`]): the entries `AccessIR`
//! policies declare (`principalCatalog`), checked against the types
//! [`crate::infer_principal_attributes`] infers from every
//! `principalAttribute` use. A declared scoped attribute whose type disagrees
//! with its inferred use refuses, so `AccessIR` and decisions never read one
//! name at two types.
//!
//! One decider ([`HoldsRolePolicy`]): `holdsRole { role, at }` is a boolean
//! the PDP evaluates under its AND rule. Each `(role, dimensions)` pair a
//! host serves compiles to one `AccessIR` rule on the synthetic resource
//! [`PRINCIPAL_ROLE_RESOURCE`]: the role condition, the structural scope
//! conjunct over those dimensions, and the trusted context that binds the
//! rule to exactly that pair. A decision never sees a role list or a scope
//! key.
//!
//! One resolver ([`resolve_principal_inputs`]): every declared input from
//! the principal the host verified for the request. It fails closed: an
//! input that cannot be resolved is [`PrincipalInputUnavailable`], never a
//! default, and the error names the input and carries no value.

use std::collections::{BTreeMap, BTreeSet};

use norn_ir::access::{
    AccessCondition, AccessIr, AccessLiteral, AccessRule, AccessScope, CompareOp, ContextCondition,
    PrincipalAttributeCatalog, PrincipalAttributeSource, PrincipalAttributeType,
    PrincipalCatalogError, RoleCondition, ScopeDimension,
};
use norn_ir::rule::{HoldsRoleAt, PrincipalInputDeclaration, PrincipalInputKind};
use norn_ir::{ObjectTypeName, RuntimeValue};
use norn_projections::CedarPrincipalAttribute;

use crate::admit::ValidatedPolicySet;
use crate::base::AuthzSchema;
use crate::diag::{AuthzCode, AuthzDiagnostic, AuthzError};
use crate::lower::{compile, is_context_name, is_reserved_principal_attribute};
use crate::pdp::{DenyReason, Pdp, PdpDecision};
use crate::request::{
    AccessRequest, AttrValue, CapabilityGrant, Delegation, PrincipalInput, ResourceInput, ScopeKey,
};

/// The host action a `holdsRole` input is decided under; also the capability
/// a caller's grant must carry.
pub const HOLDS_ROLE_ACTION: &str = "norn.holds_role";
/// The synthetic resource of [`HOLDS_ROLE_ACTION`], identified by the role.
pub const PRINCIPAL_ROLE_RESOURCE: &str = "norn.PrincipalRole";
/// The id of the compiled `holdsRole` policy.
pub const HOLDS_ROLE_POLICY_ID: &str = "norn.principal.holds_role";

const ROLE_CONTEXT: &str = "role";
const HELD_AT_CONTEXT: &str = "heldAt";

/// The Cedar fragment of [`HOLDS_ROLE_ACTION`], composed with the base schema
/// and the projected `Norn::Actor`.
const HOLDS_ROLE_SCHEMA: &str = "namespace norn {
  entity PrincipalRole = {
    scope: Norn::Scope
  };
}
namespace Norn {
  action \"norn.holds_role\" appliesTo {
    principal: [Actor],
    resource: [norn::PrincipalRole],
    context: {
      scope: Scope,
      role: String,
      heldAt: String
    }
  };
}
";

const fn cedar_type(value_type: PrincipalAttributeType) -> CedarPrincipalAttribute {
    match value_type {
        PrincipalAttributeType::Bool => CedarPrincipalAttribute::Bool,
        PrincipalAttributeType::Long => CedarPrincipalAttribute::Long,
        PrincipalAttributeType::String => CedarPrincipalAttribute::String,
        PrincipalAttributeType::StringSet => CedarPrincipalAttribute::StringSet,
    }
}

/// Build the one principal attribute catalog of `documents`. `inferred` is
/// [`crate::infer_principal_attributes`] over the same documents.
///
/// # Errors
///
/// [`AuthzCode::AccessNameInvalid`] for a malformed entry or a scoped
/// attribute whose name is not an identifier,
/// [`AuthzCode::AccessPrincipalReserved`] for a reserved principal attribute,
/// and [`AuthzCode::AccessPrincipalConflict`] for two different declarations of
/// one name or a scoped attribute declared at another type than `AccessIR`
/// uses it at.
pub fn principal_attribute_catalog(
    documents: &[AccessIr],
    inferred: &BTreeMap<String, CedarPrincipalAttribute>,
) -> Result<PrincipalAttributeCatalog, AuthzError> {
    let mut catalog = PrincipalAttributeCatalog::default();
    let mut diagnostics = Vec::new();
    for document in documents {
        for entry in &document.principal_catalog {
            let name = entry.name.as_str();
            let refusal = match catalog.declare(entry) {
                Ok(()) if entry.source != PrincipalAttributeSource::Attribute => None,
                Ok(()) if !is_context_name(name) => Some((
                    AuthzCode::AccessNameInvalid,
                    format!(
                        "principal attribute '{name}' is not an identifier ([A-Za-z_][A-Za-z0-9_]*)"
                    ),
                )),
                Ok(()) if is_reserved_principal_attribute(name) => Some((
                    AuthzCode::AccessPrincipalReserved,
                    format!("principal attribute '{name}' is reserved"),
                )),
                Ok(()) => inferred
                    .get(name)
                    .filter(|used| **used != cedar_type(entry.value_type))
                    .map(|used| {
                        (
                            AuthzCode::AccessPrincipalConflict,
                            format!(
                                "principal attribute '{name}' is declared {:?} but AccessIR uses it as {used:?}",
                                entry.value_type
                            ),
                        )
                    }),
                Err(PrincipalCatalogError::Conflict { .. }) => Some((
                    AuthzCode::AccessPrincipalConflict,
                    format!("principal catalog entry '{name}' is declared twice differently"),
                )),
                Err(
                    PrincipalCatalogError::InvalidEntry { .. }
                    | PrincipalCatalogError::Unknown { .. },
                ) => Some((
                    AuthzCode::AccessNameInvalid,
                    format!("principal catalog entry '{name}' is invalid"),
                )),
            };
            if let Some((code, message)) = refusal {
                diagnostics
                    .push(AuthzDiagnostic::new(code, message).for_policy(&document.policy_id));
            }
        }
    }
    if diagnostics.is_empty() {
        Ok(catalog)
    } else {
        Err(AuthzError::new(diagnostics))
    }
}

/// The principal attribute catalog of a definition set, as admission checks
/// it before install: the Cedar schema is projected from the set's own
/// model and actions, [`crate::infer_principal_attributes`] reads what the
/// set's access rules use each principal attribute at, and
/// [`principal_attribute_catalog`] holds the declared entries to that.
///
/// # Errors
///
/// As [`crate::infer_principal_attributes`] and
/// [`principal_attribute_catalog`].
pub fn admit_principal_catalog(
    documents: &[AccessIr],
    object_types: &[norn_ir::ObjectTypeDef],
    interfaces: &[norn_ir::InterfaceDef],
    actions: &[norn_ir::ActionRoot],
) -> Result<PrincipalAttributeCatalog, AuthzError> {
    let projection = norn_projections::project_cedar_schema(object_types, interfaces, actions);
    let inferred = crate::lower::infer_principal_attributes(documents, &projection)?;
    principal_attribute_catalog(documents, &inferred)
}

/// One `(role, dimensions)` pair a `holdsRole` decision is compiled for. The
/// dimensions are distinct and ordered.
pub type HoldsRoleKey = (String, Vec<ScopeDimension>);

fn canonical(dimensions: &[ScopeDimension]) -> Vec<ScopeDimension> {
    dimensions
        .iter()
        .copied()
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}

/// The dimensions `scope` carries.
#[must_use]
pub fn scope_dimensions(scope: &ScopeKey) -> Vec<ScopeDimension> {
    [
        (Some(scope.org.as_str()), ScopeDimension::Org),
        (scope.tenant.as_deref(), ScopeDimension::Tenant),
        (scope.workspace.as_deref(), ScopeDimension::Workspace),
        (scope.environment.as_deref(), ScopeDimension::Environment),
        (scope.business_unit.as_deref(), ScopeDimension::BusinessUnit),
    ]
    .into_iter()
    .filter_map(|(value, dimension)| value.map(|_| dimension))
    .collect()
}

fn held_at(dimensions: &[ScopeDimension]) -> String {
    dimensions
        .iter()
        .map(|dimension| match dimension {
            ScopeDimension::Org => "org",
            ScopeDimension::Tenant => "tenant",
            ScopeDimension::Workspace => "workspace",
            ScopeDimension::Environment => "environment",
            ScopeDimension::BusinessUnit => "businessUnit",
        })
        .collect::<Vec<_>>()
        .join(",")
}

/// The pairs `inputs` need decided for a subject and a request that both
/// live in a scope with `dimensions`: a `holdsRole` at the resource and each
/// role of a role profile use those dimensions; a `holdsRole` at listed
/// dimensions uses its own.
#[must_use]
pub fn holds_role_keys(
    catalog: &PrincipalAttributeCatalog,
    inputs: &[PrincipalInputDeclaration],
    dimensions: &[ScopeDimension],
) -> BTreeSet<HoldsRoleKey> {
    let mut keys = BTreeSet::new();
    for input in inputs {
        match (&input.kind, catalog.resolve(&input.kind)) {
            (PrincipalInputKind::HoldsRole(holds), Ok(_)) => {
                let at = match &holds.at {
                    HoldsRoleAt::Subject(_) => canonical(dimensions),
                    HoldsRoleAt::Scope(listed) => canonical(listed),
                };
                keys.insert((holds.role.clone(), at));
            }
            (PrincipalInputKind::RoleProfile(_), Ok(entry)) => {
                if let PrincipalAttributeSource::RoleProfile { roles } = &entry.source {
                    for role in roles {
                        keys.insert((role.clone(), canonical(dimensions)));
                    }
                }
            }
            (PrincipalInputKind::Attribute(_), Ok(_)) | (_, Err(_)) => {}
        }
    }
    keys
}

/// The compiled `holdsRole` rules of one definitions revision.
#[derive(Debug, Clone)]
pub struct HoldsRolePolicy {
    rules: Option<ValidatedPolicySet>,
    keys: BTreeSet<HoldsRoleKey>,
}

fn holds_role_schema() -> Result<AuthzSchema, AuthzError> {
    static PARSED: std::sync::OnceLock<Result<AuthzSchema, (AuthzCode, String)>> =
        std::sync::OnceLock::new();
    PARSED
        .get_or_init(|| {
            let projection = norn_projections::project_cedar_schema(&[], &[], &[]);
            AuthzSchema::from_projected_cedarschema_fragments(&[
                projection.schema.as_str(),
                HOLDS_ROLE_SCHEMA,
            ])
            .map_err(|error| (AuthzCode::SchemaInvalid, error.to_string()))
        })
        .clone()
        .map_err(|(code, message)| AuthzError::one(code, message))
}

impl HoldsRolePolicy {
    /// Compile one rule per key. No key compiles to no rule: nothing is held.
    ///
    /// # Errors
    ///
    /// The schema or a rule does not compile (an empty role, a dimension
    /// list without `org`).
    pub fn compile(keys: &BTreeSet<HoldsRoleKey>) -> Result<Self, AuthzError> {
        let keys: BTreeSet<HoldsRoleKey> = keys
            .iter()
            .map(|(role, dimensions)| (role.clone(), canonical(dimensions)))
            .collect();
        if keys.is_empty() {
            return Ok(Self { rules: None, keys });
        }
        let schema = holds_role_schema()?;
        let rules = keys
            .iter()
            .enumerate()
            .map(|(index, (role, dimensions))| AccessRule {
                rule_id: format!("holds_{index}"),
                actions: vec![HOLDS_ROLE_ACTION.to_owned()],
                resource_type: ObjectTypeName(PRINCIPAL_ROLE_RESOURCE.to_owned()),
                scope: AccessScope {
                    dimensions: dimensions.clone(),
                },
                all_of: vec![
                    AccessCondition::Role(RoleCondition {
                        any_of: vec![role.clone()],
                    }),
                    context_equals(ROLE_CONTEXT, role),
                    context_equals(HELD_AT_CONTEXT, &held_at(dimensions)),
                ],
            })
            .collect();
        let policy = AccessIr::with_rules(HOLDS_ROLE_POLICY_ID, rules);
        Ok(Self {
            rules: Some(compile(&schema, &policy)?),
            keys,
        })
    }

    /// The PDP deciding for the callers `grants` name. A grant must carry
    /// the capability [`HOLDS_ROLE_ACTION`] in the request scope.
    ///
    /// # Errors
    ///
    /// A grant does not compile.
    pub fn pdp(&self, grants: &[CapabilityGrant]) -> Result<HoldsRolePdp, AuthzError> {
        let pdp = if let Some(rules) = &self.rules {
            Some(Pdp::new(rules.clone(), grants)?)
        } else {
            crate::grants::CompiledGrants::compile(grants)?;
            None
        };
        Ok(HoldsRolePdp {
            pdp,
            keys: self.keys.clone(),
        })
    }
}

fn context_equals(name: &str, value: &str) -> AccessCondition {
    AccessCondition::Context(ContextCondition {
        name: name.to_owned(),
        op: CompareOp::Eq,
        value: AccessLiteral::String(value.to_owned()),
    })
}

/// [`HoldsRolePolicy`] with one request's capability grants.
#[derive(Debug, Clone)]
pub struct HoldsRolePdp {
    pdp: Option<Pdp>,
    keys: BTreeSet<HoldsRoleKey>,
}

/// The acting principal a host verified for one request, and where the
/// decision's subject lives.
#[derive(Debug, Clone, Copy)]
pub struct PrincipalContext<'a> {
    /// The acting principal with the roles and attributes the trusted mapper
    /// resolved in the request scope. For an agent acting on someone's
    /// behalf this is the narrower delegated principal, never its delegator.
    pub principal: &'a PrincipalInput,
    /// The request scope.
    pub request_scope: &'a ScopeKey,
    /// The scope of the decision's subject (`at: resource`).
    pub subject_scope: &'a ScopeKey,
    /// The delegation slot of the PDP's AND rule.
    pub delegation: &'a Delegation,
}

/// A declared principal input could not be resolved. There is no default.
/// Carries the input's name only, never a value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrincipalInputUnavailable {
    /// The declared input's name.
    pub input: String,
}

impl std::fmt::Display for PrincipalInputUnavailable {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "principal input `{}` could not be resolved", self.input)
    }
}

impl std::error::Error for PrincipalInputUnavailable {}

impl HoldsRolePdp {
    /// Whether the PDP finds the principal holding `role` at `dimensions` of
    /// `resource_scope`. `None` when the pair was not compiled or the PDP
    /// could not evaluate its access conjunct.
    fn holds(
        &self,
        context: &PrincipalContext<'_>,
        role: &str,
        dimensions: &[ScopeDimension],
        resource_scope: &ScopeKey,
    ) -> Option<bool> {
        let dimensions = canonical(dimensions);
        let pdp = self.pdp.as_ref()?;
        if !self.keys.contains(&(role.to_owned(), dimensions.clone())) {
            return None;
        }
        let request = AccessRequest {
            // Membership reads the principal's roles alone: its scoped
            // attributes are not part of the `holdsRole` schema.
            principal: PrincipalInput {
                attributes: BTreeMap::new(),
                ..context.principal.clone()
            },
            scope: Some(context.request_scope.clone()),
            capability: HOLDS_ROLE_ACTION.to_owned(),
            action_id: HOLDS_ROLE_ACTION.to_owned(),
            resource: ResourceInput {
                object_type: PRINCIPAL_ROLE_RESOURCE.to_owned(),
                id: role.to_owned(),
                scope: resource_scope.clone(),
                attributes: BTreeMap::new(),
            },
            context: BTreeMap::from([
                (ROLE_CONTEXT.to_owned(), AttrValue::String(role.to_owned())),
                (
                    HELD_AT_CONTEXT.to_owned(),
                    AttrValue::String(held_at(&dimensions)),
                ),
            ]),
            binding: crate::Binding::Unbound(crate::UnboundProfile::HostedRead),
            delegation: context.delegation.clone(),
        };
        match pdp.decide(&request) {
            PdpDecision::Allow => Some(true),
            PdpDecision::Deny(DenyReason::Access) => Some(false),
            PdpDecision::Deny(
                DenyReason::InvalidRequest
                | DenyReason::AbsentScope
                | DenyReason::Capability
                | DenyReason::Delegation
                | DenyReason::AccessError,
            ) => None,
        }
    }
}

impl HoldsRolePdp {
    /// The roles of `roles` the PDP finds the principal holding in the
    /// request scope. `None` when any of them cannot be decided.
    fn held_roles(
        &self,
        context: &PrincipalContext<'_>,
        roles: &[String],
        dimensions: &[ScopeDimension],
    ) -> Option<BTreeSet<String>> {
        let mut held = BTreeSet::new();
        for role in roles {
            if self.holds(context, role, dimensions, context.request_scope)? {
                held.insert(role.clone());
            }
        }
        Some(held)
    }
}

/// The value of one scoped attribute at its declared type, if the trusted
/// mapper supplied one of that type.
fn attribute_value(value: &AttrValue, value_type: PrincipalAttributeType) -> Option<RuntimeValue> {
    match (value_type, value) {
        (PrincipalAttributeType::Bool, AttrValue::Bool(flag)) => Some(RuntimeValue::Bool(*flag)),
        (PrincipalAttributeType::Long, AttrValue::Long(number)) => {
            Some(RuntimeValue::Number(norn_ir::Decimal::from(*number)))
        }
        (PrincipalAttributeType::String, AttrValue::String(text)) => {
            Some(RuntimeValue::String(text.clone()))
        }
        (PrincipalAttributeType::StringSet, AttrValue::Set(items)) => items
            .iter()
            .map(|item| match item {
                AttrValue::String(text) => Some(text.clone()),
                AttrValue::Bool(_)
                | AttrValue::Long(_)
                | AttrValue::Entity(_)
                | AttrValue::Set(_)
                | AttrValue::Record(_) => None,
            })
            .collect::<Option<BTreeSet<String>>>()
            .map(string_set),
        (
            PrincipalAttributeType::Bool
            | PrincipalAttributeType::Long
            | PrincipalAttributeType::String
            | PrincipalAttributeType::StringSet,
            _,
        ) => None,
    }
}

fn string_set(items: BTreeSet<String>) -> RuntimeValue {
    RuntimeValue::Array(items.into_iter().map(RuntimeValue::String).collect())
}

/// Resolve every declared input of one decision for the verified principal
/// in `context`, by input name.
///
/// - `holdsRole`: the PDP's decision at the subject's scope (`"resource"`)
///   or at the listed dimensions of the request scope.
/// - `roleProfile`: the profile's roles the PDP finds the principal holding
///   in the request scope, ordered.
/// - `attribute`: the trusted mapper's value, at the catalog's type.
///
/// # Errors
///
/// [`PrincipalInputUnavailable`] for the first input the catalog does not
/// define, the PDP cannot evaluate, or the mapper did not supply at the
/// declared type.
pub fn resolve_principal_inputs(
    catalog: &PrincipalAttributeCatalog,
    pdp: &HoldsRolePdp,
    context: &PrincipalContext<'_>,
    inputs: &[PrincipalInputDeclaration],
) -> Result<BTreeMap<String, RuntimeValue>, PrincipalInputUnavailable> {
    let request_dimensions = scope_dimensions(context.request_scope);
    let mut resolved = BTreeMap::new();
    for input in inputs {
        let unavailable = || PrincipalInputUnavailable {
            input: input.name.clone(),
        };
        if resolved.contains_key(&input.name) {
            return Err(unavailable());
        }
        let entry = catalog
            .resolve(&input.kind)
            .map_err(|_unknown| unavailable())?;
        let value = match (&input.kind, &entry.source) {
            (PrincipalInputKind::HoldsRole(holds), PrincipalAttributeSource::Role) => {
                let held = match &holds.at {
                    HoldsRoleAt::Subject(_) => pdp.holds(
                        context,
                        &holds.role,
                        &scope_dimensions(context.subject_scope),
                        context.subject_scope,
                    ),
                    HoldsRoleAt::Scope(dimensions) => {
                        pdp.holds(context, &holds.role, dimensions, context.request_scope)
                    }
                };
                held.map(RuntimeValue::Bool)
            }
            (
                PrincipalInputKind::RoleProfile(_),
                PrincipalAttributeSource::RoleProfile { roles },
            ) => pdp
                .held_roles(context, roles, &request_dimensions)
                .map(string_set),
            (PrincipalInputKind::Attribute(_), PrincipalAttributeSource::Attribute) => context
                .principal
                .attributes
                .get(&entry.name)
                .and_then(|value| attribute_value(value, entry.value_type)),
            (
                PrincipalInputKind::HoldsRole(_)
                | PrincipalInputKind::RoleProfile(_)
                | PrincipalInputKind::Attribute(_),
                _,
            ) => None,
        };
        resolved.insert(input.name.clone(), value.ok_or_else(unavailable)?);
    }
    Ok(resolved)
}
