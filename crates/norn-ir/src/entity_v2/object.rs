//! Object types, their properties, identity, defaults, and computed
//! properties (T1-ONT-1b, #2512; ruling 2026-09-09 §5 as extended by
//! 2026-09-25 §2), with single inheritance, implemented interfaces, and
//! resource status (T1-ONT-1c, #2513).
//!
//! An [`ObjectTypeDef`] declares [`Property`]s and its `identity`: zero or
//! more [`KeySpec`]s, each an ordered list of property ids (one id for a
//! single key, several for a composite key) with an optional declared
//! [`IdentityRule`] (per-component [`Normalization`] and a reference to an
//! entity-resolution policy, resolved by T8-ID-1). A property may declare a
//! literal [`DefaultValue`] or be `computed` from a ZEN Expression, which makes
//! it read-only.
//!
//! Rules that shape the wire:
//!
//! - **Keys are the object's own business identity.** The identity of a record
//!   in an external system (`ERPNext.Employee/EMP182`) is provenance carried
//!   per source system on ingested facts (ruling 2026-09-25 §2, T8-ID-1), not a
//!   key property: several source identities may resolve to one object, and
//!   flattening them into one key is forbidden.
//! - **Key order is significant.** A key value is the ordered tuple of its
//!   components; `[company, number]` and `[number, company]` encode different
//!   tuples (and enforce the same uniqueness, so declaring both is redundant).
//! - **Defaults are literals.** A default is a JSON literal of the property's
//!   type, never an expression: a clock- or context-dependent initial value is
//!   an action input. Integer and decimal defaults are exact decimal strings
//!   (`"0.00"`), never JSON numbers (OPEN-DECISIONS D37).
//! - **Omitted vs `null`** follows #2819: optional members may be omitted, and
//!   an explicit `null` is rejected, including `"default": null` (an absent
//!   default already means "no value").
//!
//! - **The name's namespace is the domain namespace.** `retail.Sku` is in the
//!   `retail` domain; there is no separate `domain` member to disagree with
//!   it. A tenant's types are named in its extension namespace
//!   (`acme.WholesaleCustomer`).
//! - **Inheritance only narrows.** A subtype inherits every property, link,
//!   key, invariant, and implemented interface of its single supertype;
//!   redeclaring an inherited property or link id may only narrow it.
//! - **Links** ([`super::link`]) and **invariants** ([`super::invariant`])
//!   arrived with T1-ONT-1d (#2514); declared **lifecycles**
//!   ([`super::lifecycle`]) with T3-LIFE-1 (#2672).
//!
//! Every consistency rule (names, key components, admissible key kinds,
//! nullability, normalization applicability, default type checks) is a
//! `norn-checker` diagnostic, not a decode error.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::interface::InterfaceName;
use super::invariant::Invariant;
use super::lifecycle::Lifecycle;
use super::link::{Link, LinkId};
use super::naming_series::NamingSeriesIR;
use super::property::{PROPERTY_TYPE_NAME_PATTERN, PropertyConstraints, PropertyType, is_false};
use super::status::ResourceStatus;
use crate::members::{WholeNumber, present_non_null};
use crate::posting::AccountingLegDeclaration;
use crate::{JsonValue, MarkingId, ZenExpression};

/// Grammar of an [`ObjectTypeName`] (published as the schema `pattern`;
/// `norn-checker` enforces it).
pub const OBJECT_TYPE_NAME_PATTERN: &str =
    r"^[a-z][a-z0-9_]*(\.[a-z][a-z0-9_]*)*\.[A-Z][A-Za-z0-9]*$";

/// Grammar of a [`PropertyId`] and a [`KeyName`] (published as the schema
/// `pattern`; `norn-checker` enforces it). `__` is reserved as the extension
/// namespace separator ([`super::extension::EXTENSION_MEMBER_SEPARATOR`]):
/// only members a tenant or extension adds contain it.
pub const MEMBER_ID_PATTERN: &str = r"^[a-z][a-z0-9_]*$";

/// Qualified name of an object type: dotted lower-case namespace segments and
/// a PascalCase type name (`retail.Sku`, `hr.Employee`), the form v1
/// `objectKind` already uses. The namespace segments are the type's domain
/// namespace ([`Self::domain`]).
#[derive(
    Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
#[serde(transparent)]
pub struct ObjectTypeName(#[schemars(regex(pattern = OBJECT_TYPE_NAME_PATTERN))] pub String);

impl ObjectTypeName {
    /// The name as a string slice.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// The type's domain namespace: every segment before the last `.`
    /// (`retail` for `retail.Sku`, `acme.retail` for `acme.retail.LoyaltySku`).
    /// `None` when the name has no `.` (an invalid name).
    #[must_use]
    pub fn domain(&self) -> Option<&str> {
        self.0.rsplit_once('.').map(|(domain, _)| domain)
    }

    /// The PascalCase local name after the domain namespace (`Sku`).
    #[must_use]
    pub fn local_name(&self) -> &str {
        self.0
            .rsplit_once('.')
            .map_or(self.0.as_str(), |(_, local)| local)
    }
}

/// Id of a property within its object type (`sku_code`), unique there.
#[derive(
    Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
#[serde(transparent)]
pub struct PropertyId(#[schemars(regex(pattern = MEMBER_ID_PATTERN))] pub String);

impl PropertyId {
    /// The id as a string slice.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Name of a [`KeySpec`] within its object type (`primary`, `by_email`),
/// unique there.
#[derive(
    Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
#[serde(transparent)]
pub struct KeyName(#[schemars(regex(pattern = MEMBER_ID_PATTERN))] pub String);

impl KeyName {
    /// The name as a string slice.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// An object type: its resource status, supertype, interfaces, properties,
/// links, identity, invariants, and declared lifecycles.
///
/// The name's namespace is the type's domain namespace
/// ([`ObjectTypeName::domain`]); there is no separate `domain` member.
/// Properties, links, and the inverse roles of links that target the type
/// share one member name space.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ObjectTypeDef {
    /// Append-only: existing instances cannot be changed or deleted. Inherited.
    #[serde(default, skip_serializing_if = "is_false")]
    pub immutable: bool,
    /// Unique name, across object types and interfaces, within the artifact
    /// set and against every installed base type.
    pub name: ObjectTypeName,
    #[serde(
        default,
        deserialize_with = "present_non_null",
        skip_serializing_if = "Option::is_none"
    )]
    #[schemars(with = "String")]
    pub description: Option<String>,
    /// The definition's resource status. Omitted means `active` (the
    /// canonical form omits it).
    #[serde(default, skip_serializing_if = "ResourceStatus::is_active")]
    pub status: ResourceStatus<ObjectTypeName>,
    /// The single supertype. The subtype inherits every property, key, and
    /// implemented interface, and may only narrow inherited properties.
    #[serde(
        default,
        deserialize_with = "present_non_null",
        skip_serializing_if = "Option::is_none"
    )]
    #[schemars(with = "ObjectTypeName")]
    pub extends: Option<ObjectTypeName>,
    /// Interfaces the type implements, in addition to those its supertypes
    /// implement. Omitted when empty.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub implements: Vec<InterfaceName>,
    /// Declared properties, in presentation order: new properties, and
    /// narrowing overrides of inherited ones (same id). Omitted when empty.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub properties: Vec<Property>,
    /// Declared links, in presentation order: new links, and narrowing
    /// overrides of inherited ones (same id). Omitted when empty.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub links: Vec<Link>,
    /// Declared owned child collections. Each collection is backed by a
    /// declared many/from-one link and keeps membership and order as part of
    /// the parent's transactional write boundary. Omitted when empty.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub owned_collections: Vec<OwnedChildCollection>,
    /// Declared keys. Each is unique among the type's live objects within one
    /// tenant scope and world, enforced at commit. Omitted when empty: every
    /// object always has its kernel-assigned object id, and a type need not
    /// declare a business key.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub identity: Vec<KeySpec>,
    /// Declared invariants, in addition to every supertype's. Omitted when
    /// empty.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub invariants: Vec<Invariant>,
    /// Declared lifecycles: the allowed transitions of `enum` properties, at
    /// most one per property. New lifecycles, and narrowing redeclarations of
    /// inherited ones (same property). Omitted when empty.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub lifecycles: Vec<Lifecycle>,
    /// Classification markings inherited by the type and its properties.
    /// Omitted when empty.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub markings: Vec<MarkingId>,
    /// Marks this type's facts as GL legs for runtime balanced-posting checks.
    /// Inherited by subtypes. Omitted for object types that are not GL legs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub accounting_leg: Option<AccountingLegDeclaration>,
}

/// A declared, parent-owned ordered collection of child objects.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct OwnedChildCollection {
    /// The parent-to-child link that owns the collection.
    pub id: LinkId,
    /// Exact child object type accepted by the owning link.
    pub child_type: ObjectTypeName,
    /// Stable child property equal to the child fact key.
    pub row_id_property: PropertyId,
    /// Stored exact-integer property holding dense 1-based row order.
    pub ordering_key: PropertyId,
    /// Whether parent deletion cascades through these rows.
    pub cascade_on_parent_delete: bool,
    /// Maximum live rows (bounded by the platform limit of 100).
    pub max_rows: WholeNumber,
}

/// One property of an object type.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Property {
    /// System-maintained: declared rules may write it, actions may not.
    #[serde(default, skip_serializing_if = "is_false")]
    pub protected: bool,
    /// Unique id within the object type.
    pub id: PropertyId,
    #[serde(
        default,
        deserialize_with = "present_non_null",
        skip_serializing_if = "Option::is_none"
    )]
    #[schemars(with = "String")]
    pub description: Option<String>,
    /// The value kind: inline, or a shared property type through `ref`.
    #[serde(rename = "type")]
    pub property_type: PropertyType,
    /// Constraints on this property's values, in addition to those of a
    /// `ref`'d shared type. Omitted when empty.
    #[serde(default, skip_serializing_if = "PropertyConstraints::is_empty")]
    pub constraints: PropertyConstraints,
    /// The value a new object takes when its creator supplies none.
    #[serde(
        default,
        deserialize_with = "present_non_null",
        skip_serializing_if = "Option::is_none"
    )]
    #[schemars(with = "DefaultValue")]
    pub default: Option<DefaultValue>,
    /// Classification markings declared on this property. Markings from
    /// property types, interfaces, supertypes, and computed inputs are unioned
    /// by the checker and cannot be removed by narrowing. Omitted when empty.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub markings: Vec<MarkingId>,
    /// A ZEN Expression over the object's other properties. A computed
    /// property is derived and read-only: no action writes it, and it has no
    /// default. It reads the object through `self` (`self.quantity`); the
    /// checker parses it (T1-ONT-2) and types it (T1-EXPR-5).
    #[serde(
        default,
        deserialize_with = "present_non_null",
        skip_serializing_if = "Option::is_none"
    )]
    #[schemars(with = "ZenExpression")]
    pub computed: Option<ZenExpression>,
}

impl Property {
    /// Whether the property is read-only (derived from `computed`).
    #[must_use]
    pub const fn is_read_only(&self) -> bool {
        self.computed.is_some()
    }
}

/// A literal default value of a property's type (any JSON except `null`).
///
/// The literal's shape per kind (checked by `norn-checker`): a string for
/// `text`, `enum`, `date`, and `timestamp`; an exact decimal string for
/// `integer` and `decimal`; a boolean for `boolean`; any JSON for `json-ext`.
#[derive(Debug, Clone, PartialEq, Serialize, JsonSchema)]
#[serde(transparent)]
#[schemars(extend("not" = { "type": "null" }))]
pub struct DefaultValue(pub JsonValue);

impl<'de> Deserialize<'de> for DefaultValue {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        // Through `JsonValue`, so numbers decode identically under both
        // `serde_json` number profiles.
        match JsonValue::deserialize(deserializer)? {
            JsonValue::Null => Err(serde::de::Error::custom(
                "a default must not be null; omit it instead",
            )),
            value @ (JsonValue::Bool(_)
            | JsonValue::Number(_)
            | JsonValue::String(_)
            | JsonValue::Array(_)
            | JsonValue::Object(_)) => Ok(Self(value)),
        }
    }
}

/// One declared key: an ordered list of property ids.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct KeySpec {
    /// Unique name within the object type.
    pub name: KeyName,
    /// The key's components, in tuple order: one id for a single key, several
    /// for a composite key. Each names a stored, non-nullable property of an
    /// admissible kind.
    pub properties: Vec<PropertyId>,
    /// Generates one stored text component of this key inside the atomic
    /// create commit. Omitted for caller-supplied keys.
    #[serde(
        default,
        deserialize_with = "present_non_null",
        skip_serializing_if = "Option::is_none"
    )]
    #[schemars(with = "NamingSeriesIR")]
    pub naming_series: Option<NamingSeriesIR>,
    /// How key values are normalized and matched. Omitted: components compare
    /// exactly as stored.
    #[serde(
        default,
        deserialize_with = "present_non_null",
        skip_serializing_if = "Option::is_none"
    )]
    #[schemars(with = "IdentityRule")]
    pub rule: Option<IdentityRule>,
}

/// A declared identity rule: normalization applied to key components before
/// comparison, and the entity-resolution policy that matches incoming records
/// to existing objects (T8-ID-1 resolves the reference and runs the policy).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct IdentityRule {
    /// Per-component normalization, applied in step order. Omitted when
    /// empty.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub normalize: Vec<ComponentNormalization>,
    /// Name of the entity-resolution policy artifact (T8-ID-1). Omitted:
    /// records match only on equal normalized key values.
    #[serde(
        default,
        deserialize_with = "present_non_null",
        skip_serializing_if = "Option::is_none"
    )]
    #[schemars(with = "ResolutionPolicyName")]
    pub match_policy: Option<ResolutionPolicyName>,
}

/// Dotted, lower-case name of an entity-resolution policy artifact
/// (`party.email_exact`); same grammar as a property type name
/// ([`super::property::PROPERTY_TYPE_NAME_PATTERN`]).
#[derive(
    Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
#[serde(transparent)]
pub struct ResolutionPolicyName(
    #[schemars(regex(pattern = PROPERTY_TYPE_NAME_PATTERN))] pub String,
);

impl ResolutionPolicyName {
    /// The name as a string slice.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// The normalization steps for one key component.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ComponentNormalization {
    /// A component of the enclosing key; its kind must be `text`.
    pub property: PropertyId,
    /// Steps, applied in order; non-empty.
    pub steps: Vec<Normalization>,
}

/// A deterministic text normalization step.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "kebab-case")]
pub enum Normalization {
    /// Remove leading and trailing Unicode whitespace.
    Trim,
    /// Replace each run of Unicode whitespace with one space.
    CollapseWhitespace,
    /// Unicode default lower-casing.
    Lowercase,
    /// Unicode default upper-casing.
    Uppercase,
    /// Unicode Normalization Form C.
    Nfc,
    /// Unicode Normalization Form KC (compatibility composition).
    Nfkc,
}
