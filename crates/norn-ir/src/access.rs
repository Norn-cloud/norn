//! Typed Access root: the payload of a `policy` artifact (T1-IR-1d, #2509).
//!
//! The rulings' "Access" primitive (2026-08-27 §2/§7, 2026-09-25 §2) is carried
//! on the wire by the existing `policy` kind; the kind string is unchanged. An
//! action's [`crate::AccessRef`] names a policy artifact.
//!
//! Three forms, selected by their members:
//!
//! - [`AccessIr`] (`accessVersion` present, #2879): the restricted access
//!   predicate (`allOf`, mandatory structural `scope` conjunct, directional
//!   `rel`, roles), compiled to Cedar by `norn-authz` at admission, never from
//!   arbitrary expressions.
//! - [`ObligationPolicyRoot`] (`policyVersion`, `obligations`, or
//!   `scopeRequired` present): versioned, scope-bearing obligations.
//! - [`ApprovalPolicyRoot`]: the pre-versioned approval policy.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::members::{IrVersion01, one_of_forms, present_non_null};
use crate::rule::JsonObject;

/// Explicit authority to disclose one complete action-refusal diagnostic.
pub const DISCLOSE_REFUSAL_DETAIL_ACTION: &str = "norn.disclose_refusal_detail";
/// Host resource whose identity is the admitted action id.
pub const REFUSAL_DETAIL_RESOURCE: &str = "norn.RefusalDetail";

/// Typed payload of a `policy` (Access) artifact.
///
/// The variants are distinguished by their members, not by a tag: the checker
/// selects the form exactly as the validator does (`accessVersion` present →
/// access form; else any of `policyVersion`, `obligations`, `scopeRequired`
/// present → obligations form).
///
/// The published schema is a `oneOf`: the forms are disjoint because each of
/// the access and obligations forms requires a version member
/// (`accessVersion`, `policyVersion`) that no other closed form admits.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(untagged)]
#[schemars(transform = one_of_forms)]
pub enum PolicyRoot {
    Obligations(ObligationPolicyRoot),
    Approval(ApprovalPolicyRoot),
    /// Restricted `AccessIR` (#2879), selected by `accessVersion`; compiled to
    /// Cedar by `norn-authz` at admission.
    Access(AccessIr),
}

/// Versioned policy: scope-bearing obligations with evidence and review roles.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ObligationPolicyRoot {
    pub policy_version: IrVersion01,
    pub policy_id: String,
    /// Scope dimensions the caller must supply (fail closed).
    pub scope_required: Vec<String>,
    pub obligations: Vec<PolicyObligation>,
    /// What the policy applies to. An approval-form member; in the obligations
    /// form it is optional and, when present, a non-empty string (checker rule).
    #[serde(
        default,
        deserialize_with = "present_non_null",
        skip_serializing_if = "Option::is_none"
    )]
    #[schemars(with = "String")]
    pub applies_to: Option<String>,
    /// When review is required. An approval-form member; in the obligations
    /// form it is optional and, when present, a non-empty string (checker rule).
    #[serde(
        default,
        deserialize_with = "present_non_null",
        skip_serializing_if = "Option::is_none"
    )]
    #[schemars(with = "String")]
    pub review_required_when: Option<String>,
    /// Roles that may approve. An approval-form member; in the obligations form
    /// it is optional and, when present, a non-empty list of non-empty strings
    /// (checker rule).
    #[serde(
        default,
        deserialize_with = "present_non_null",
        skip_serializing_if = "Option::is_none"
    )]
    #[schemars(with = "Vec<String>")]
    pub approval_roles: Option<Vec<String>>,
    /// Reserved extension slots keyed by `<name>/<semver>`; `null` is rejected.
    #[serde(
        default,
        deserialize_with = "present_non_null",
        skip_serializing_if = "Option::is_none"
    )]
    #[schemars(with = "JsonObject")]
    pub extensions: Option<JsonObject>,
}

/// One obligation of an [`ObligationPolicyRoot`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct PolicyObligation {
    pub obligation_id: String,
    pub applies_to: String,
    pub evidence_required: Vec<String>,
    pub review_roles: Vec<String>,
    /// Members the validator does not check, kept verbatim.
    #[serde(flatten)]
    pub additional: JsonObject,
}

/// Pre-versioned approval policy: who must approve what, and when.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ApprovalPolicyRoot {
    pub policy_id: String,
    pub applies_to: String,
    pub review_required_when: String,
    pub approval_roles: Vec<String>,
    /// Reserved extension slots keyed by `<name>/<semver>`; `null` is rejected.
    #[serde(
        default,
        deserialize_with = "present_non_null",
        skip_serializing_if = "Option::is_none"
    )]
    #[schemars(with = "JsonObject")]
    pub extensions: Option<JsonObject>,
}

/// Restricted `AccessIR` v1 (T5-AZ-2, #2577; ruling 2026-08-27 §2/§7, retained by
/// 2026-09-25): the typed access predicate that `norn-authz` compiles to Cedar.
///
/// The grammar is closed. A rule is a conjunction (`allOf`) of typed
/// conditions under a **mandatory structural `scope` conjunct**; there is no
/// expression node, so an arbitrary ZEN Expression (or Cedar text) cannot be
/// carried here. Expressions may only compute typed context values outside
/// Cedar; a rule compares such a value through [`AccessCondition::Context`].
///
/// The [`PolicyRoot::Access`] form of a `policy` artifact (#2879).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AccessIr {
    pub access_version: IrVersion01,
    /// Identity of the policy artifact; prefixes every compiled Cedar policy id.
    pub policy_id: String,
    /// Permit rules. Authorization is default-deny: no rule, no permit.
    pub rules: Vec<AccessRule>,
    /// Closed rules expanded over the admitted entitlement target/action
    /// catalog. These are templates, never one rule per entitlement row.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub generic_rules: Vec<GenericAccessRule>,
    /// Declared entries of the principal attribute catalog (#1091): the
    /// roles, role profiles and scoped attributes a decision may read as
    /// typed principal inputs. A scoped attribute's type must agree with
    /// every `principalAttribute` use `norn-authz` infers for it.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub principal_catalog: Vec<PrincipalCatalogEntry>,
    /// Reserved extension slots keyed by `<name>/<semver>`; `null` is rejected.
    #[serde(
        default,
        deserialize_with = "present_non_null",
        skip_serializing_if = "Option::is_none"
    )]
    #[schemars(with = "JsonObject")]
    pub extensions: Option<JsonObject>,
}

impl AccessIr {
    /// A policy of `rules` alone: no generic rules, catalog entries or
    /// extensions.
    #[must_use]
    pub fn with_rules(policy_id: impl Into<String>, rules: Vec<AccessRule>) -> Self {
        Self {
            access_version: IrVersion01::V0_1,
            policy_id: policy_id.into(),
            rules,
            generic_rules: Vec::new(),
            principal_catalog: Vec::new(),
            extensions: None,
        }
    }
}

/// One declared entry of the principal attribute catalog (#1091): a name, a
/// type and a source.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PrincipalCatalogEntry {
    /// The role id, role profile name or scoped attribute name.
    pub name: String,
    /// The type a decision reads the entry at.
    #[serde(rename = "type")]
    pub value_type: PrincipalAttributeType,
    /// Where the trusted resolver takes the value from.
    pub source: PrincipalAttributeSource,
}

/// The closed set of principal attribute types.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "camelCase")]
pub enum PrincipalAttributeType {
    Bool,
    Long,
    String,
    /// `set<string>`.
    StringSet,
}

/// Where a catalog entry's value comes from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum PrincipalAttributeSource {
    /// A role, named by the entry: a boolean the PDP evaluates.
    Role,
    /// A named role bundle: the set of its roles the principal holds.
    RoleProfile {
        /// The bundled role ids; at least one.
        roles: Vec<String>,
    },
    /// A scoped attribute supplied by the trusted resolver.
    Attribute,
}

/// Why a catalog entry or a principal input does not resolve.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PrincipalCatalogError {
    /// The entry's name is not well formed (a scoped attribute is an
    /// identifier, `[A-Za-z_][A-Za-z0-9_]*`), or its type contradicts its
    /// source (a role is `bool`, a role profile `stringSet`).
    InvalidEntry {
        /// The entry.
        name: String,
    },
    /// Two declarations of one name differ.
    Conflict {
        /// The entry.
        name: String,
    },
    /// No entry of the source the input reads has this name.
    Unknown {
        /// The name the input reads.
        name: String,
    },
}

/// The principal attribute catalog shared by `AccessIR` and decisions
/// (#1091): every declared entry, by name. One implementation resolves a
/// decision's [`crate::rule::PrincipalInputKind`] to its type for the
/// checker, `norn-authz` and the host resolver.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PrincipalAttributeCatalog {
    entries: std::collections::BTreeMap<String, PrincipalCatalogEntry>,
}

impl PrincipalAttributeCatalog {
    /// Add one declaration. Declaring an identical entry twice is accepted.
    ///
    /// # Errors
    /// An invalid entry, or one that differs from an earlier declaration.
    pub fn declare(&mut self, entry: &PrincipalCatalogEntry) -> Result<(), PrincipalCatalogError> {
        // A scoped attribute is read as `principal.<name>` by Cedar and by
        // decisions, so it is an identifier; a role or role profile name is a
        // role id, which may also carry `.` and `-` after its first character.
        let attribute = entry.source == PrincipalAttributeSource::Attribute;
        let named = entry
            .name
            .chars()
            .enumerate()
            .all(|(index, character)| match character {
                'A'..='Z' | 'a'..='z' | '_' => true,
                '0'..='9' => index > 0,
                '.' | '-' => index > 0 && !attribute,
                _ => false,
            });
        let typed = match &entry.source {
            PrincipalAttributeSource::Role => entry.value_type == PrincipalAttributeType::Bool,
            PrincipalAttributeSource::RoleProfile { roles } => {
                entry.value_type == PrincipalAttributeType::StringSet
                    && !roles.is_empty()
                    && roles.iter().all(|role| !role.trim().is_empty())
            }
            PrincipalAttributeSource::Attribute => true,
        };
        if entry.name.is_empty() || !named || !typed {
            return Err(PrincipalCatalogError::InvalidEntry {
                name: entry.name.clone(),
            });
        }
        match self.entries.get(&entry.name) {
            Some(declared) if declared != entry => Err(PrincipalCatalogError::Conflict {
                name: entry.name.clone(),
            }),
            Some(_) => Ok(()),
            None => {
                self.entries.insert(entry.name.clone(), entry.clone());
                Ok(())
            }
        }
    }

    /// The declared entry named `name`.
    #[must_use]
    pub fn get(&self, name: &str) -> Option<&PrincipalCatalogEntry> {
        self.entries.get(name)
    }

    /// Every declared entry, ordered by name.
    pub fn entries(&self) -> impl Iterator<Item = &PrincipalCatalogEntry> {
        self.entries.values()
    }

    /// The entry a decision input reads: a `holdsRole` reads a role, a
    /// `roleProfile` a role profile and an `attribute` a scoped attribute.
    ///
    /// # Errors
    /// [`PrincipalCatalogError::Unknown`] when no entry of that source has
    /// the name.
    pub fn resolve(
        &self,
        kind: &crate::rule::PrincipalInputKind,
    ) -> Result<&PrincipalCatalogEntry, PrincipalCatalogError> {
        use crate::rule::PrincipalInputKind;
        let name = kind.catalog_name();
        self.entries
            .get(name)
            .filter(|entry| {
                matches!(
                    (kind, &entry.source),
                    (
                        PrincipalInputKind::HoldsRole(_),
                        PrincipalAttributeSource::Role
                    ) | (
                        PrincipalInputKind::RoleProfile(_),
                        PrincipalAttributeSource::RoleProfile { .. }
                    ) | (
                        PrincipalInputKind::Attribute(_),
                        PrincipalAttributeSource::Attribute
                    )
                )
            })
            .ok_or_else(|| PrincipalCatalogError::Unknown {
                name: name.to_owned(),
            })
    }
}

/// A fixed, typed rule template compiled once for each admitted target type and
/// eligible action. It cannot name arbitrary Cedar source or imported rows.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct GenericAccessRule {
    /// Unique within the policy.
    pub rule_id: String,
    /// The closed target catalog selector.
    pub target: GenericTargetSelector,
    /// The closed action catalog selector.
    pub actions: GenericActionSelector,
    /// The mandatory structural scope conjunct.
    pub scope: AccessScope,
    /// Further typed conjuncts; entitlement templates require at least one.
    pub all_of: Vec<AccessCondition>,
}

/// Closed catalog selectors for generic AccessIR templates.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "camelCase", deny_unknown_fields)]
pub enum GenericTargetSelector {
    /// Every object type in the explicit admitted business catalog.
    AllAdmittedObjectTypes,
}

/// Closed catalog selectors for generic AccessIR templates.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "camelCase", deny_unknown_fields)]
pub enum GenericActionSelector {
    /// Every action in the explicit entitlement-eligible action catalog.
    AllAdmittedActions,
}

/// One permit rule: `principal` may perform any of `actions` on a `resource`
/// of `resourceType` when the scope conjunct and every `allOf` condition hold.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AccessRule {
    /// Unique within the policy.
    pub rule_id: String,
    /// `ActionIR` ids this rule permits; at least one.
    pub actions: Vec<String>,
    /// The `EntityIR` v2 object type of the resource.
    pub resource_type: crate::entity_v2::object::ObjectTypeName,
    /// The mandatory structural scope conjunct. Required on the wire: a rule
    /// without it fails to decode.
    pub scope: AccessScope,
    /// Further conjuncts; empty means the scope conjunct alone.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub all_of: Vec<AccessCondition>,
}

/// The structural scope conjunct. The principal's scope, the resource's scope,
/// and the request scope must be exactly equal (an absent dimension matches
/// only an absent dimension, never as a wildcard), and every listed dimension
/// must be present in the request scope. `org` must be listed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AccessScope {
    pub dimensions: Vec<ScopeDimension>,
}

/// A structural scope dimension (the kernel's five-dimension scope key).
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "camelCase")]
pub enum ScopeDimension {
    Org,
    Tenant,
    Workspace,
    Environment,
    BusinessUnit,
}

/// One typed conjunct of [`AccessRule::all_of`]. Externally tagged and closed:
/// an unknown node (for example `{"expression": "..."}`) fails to decode.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum AccessCondition {
    /// The principal holds at least one of the roles.
    Role(RoleCondition),
    /// Directional relation: the other endpoint is reached from `from` by the
    /// link role `role` (a link id or inverse role on the starting type).
    Rel(RelCondition),
    /// A typed context value, computed outside Cedar, compared to a literal.
    Context(ContextCondition),
    /// A projected scalar property of the resource, compared to a literal or
    /// to a trusted principal attribute. This condition requires schema
    /// projection metadata so admission can reject unsupported property types
    /// (including decimals and links) before lowering.
    Resource(ResourceCondition),
    /// The synthetic marking resource is one of the listed markings. This is
    /// reserved for the `norn.reveal` and `norn.declassify` actions on
    /// `norn.Marking`; admission rejects it on every other resource or action.
    Marking(MarkingCondition),
    /// Matching trusted entitlement kind.
    Entitlement(EntitlementMatchKind),
}

/// `resource.id` is a member of at least one of `anyOf` for a synthetic
/// `norn.Marking` resource.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct MarkingCondition {
    /// Marking ids this rule may reveal or declassify.
    pub any_of: Vec<crate::MarkingId>,
}

/// The relationship condition selected by a fixed entitlement policy rule.
/// Link ids and target references belong to entitlement data, not this rule.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "camelCase", deny_unknown_fields)]
pub enum EntitlementMatchKind {
    /// No extra resource relationship is required.
    None,
    /// The resource's configured owner link must identify the principal.
    OwnerOnly,
    /// The resource's configured link must match the exact linked object.
    LinkedRecord,
}

/// `principal` is a member of at least one of `anyOf`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RoleCondition {
    pub any_of: Vec<String>,
}

/// Which endpoint a [`RelCondition`] starts from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum RelEndpoint {
    /// `principal in resource.<role>`.
    Resource,
    /// `resource in principal.<role>`.
    Principal,
}

/// Directional `rel(from, role)`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RelCondition {
    pub from: RelEndpoint,
    pub role: crate::entity_v2::link::LinkId,
}

/// Comparison operator of a [`ContextCondition`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum CompareOp {
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
}

/// Comparison operator of a [`ResourceCondition`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum ResourceCompareOp {
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
    /// The resource scalar is a member of the named principal attribute set.
    In,
}

/// A typed literal: boolean, 64-bit integer, or string. No expression form.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(untagged)]
pub enum AccessLiteral {
    Bool(bool),
    Long(#[serde(deserialize_with = "literal_integer")] i64),
    String(String),
}

/// `context.<name> <op> <value>` over a typed context value.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ContextCondition {
    pub name: String,
    pub op: CompareOp,
    pub value: AccessLiteral,
}

/// `resource.<name> <op> <operand>` over a projected resource scalar.
/// Literal operands preserve the existing AccessIR wire form. The closed
/// principal attribute operand is resolved against the Cedar schema inferred
/// from all resource conditions in the policy set.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ResourceCondition {
    pub name: String,
    pub op: ResourceCompareOp,
    pub value: ResourceOperand,
}

/// A resource comparison's literal or trusted principal attribute reference.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(untagged)]
pub enum ResourceOperand {
    /// The existing boolean, exact signed 64-bit integer, or string literal.
    Literal(AccessLiteral),
    /// A principal attribute name supplied by the trusted resolver.
    PrincipalAttribute(PrincipalAttributeOperand),
}

/// The name of a trusted principal attribute, represented on the wire as
/// `{ "principalAttribute": "..." }`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PrincipalAttributeOperand {
    pub principal_attribute: String,
}

// Artifact payloads expose numbers through JsonValue's f64 representation.
// Accept exactly integral values as Long while rejecting fractions and overflow.
fn literal_integer<'de, D: serde::Deserializer<'de>>(deserializer: D) -> Result<i64, D::Error> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Number {
        Signed(i64),
        Value(crate::JsonValue),
    }
    match Number::deserialize(deserializer)? {
        Number::Signed(value) => Ok(value),
        Number::Value(crate::JsonValue::Number(value)) => value
            .is_finite()
            .then(|| value.to_string())
            .and_then(|text| text.parse().ok())
            .ok_or_else(|| {
                serde::de::Error::custom(
                    "AccessIR Long literal must be an exact signed 64-bit integer",
                )
            }),
        Number::Value(_) => Err(serde::de::Error::custom(
            "AccessIR Long literal must be a number",
        )),
    }
}
