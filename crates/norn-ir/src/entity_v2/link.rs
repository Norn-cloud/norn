//! Links (T1-ONT-1d, #2514; ruling 2026-09-09 §5 "links with cardinality
//! declared on both ends, inverses, and optional link properties", 2026-09-25
//! §2 "`Relation` (with `Role`s; link properties as today)").
//!
//! A [`Link`] is a directional relation declared on its source object type
//! (Gel/LinkML style): a [`LinkId`] that names the forward role (`customer`
//! on `commerce.SalesOrder`), a target object type or interface, a
//! [`LinkCardinality`] for both ends, an optional `inverse` role name that
//! the target traverses back by (`orders` on `crm.Customer`), and optional
//! link properties (relationship attributes, reusing [`Property`]).
//!
//! One relation is declared once. Its two roles are the link id (a member of
//! the source type) and the inverse (a member of the target type); both are
//! directional names that AccessIR `rel(...)` and link traversal resolve on
//! the type they start from. The inverse is derived, never declared a second
//! time on the target, so the two ends cannot disagree.
//!
//! Every rule (role grammar, resolvable targets, member collisions, narrowing
//! overrides, interface conformance, namespaces) is a `norn-checker`
//! diagnostic, not a decode error.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::object::{MEMBER_ID_PATTERN, ObjectTypeName, Property};
use super::property::is_false;
use crate::members::present_non_null;

/// A role name of a link: the link id on the source type, or the inverse
/// on the target type. Same grammar as a property id
/// ([`MEMBER_ID_PATTERN`]); properties, links, and inverse roles share one
/// member name space per object type.
#[derive(
    Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
#[serde(transparent)]
pub struct LinkId(#[schemars(regex(pattern = MEMBER_ID_PATTERN))] pub String);

impl LinkId {
    /// The id as a string slice.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// How many objects one end of a link may relate to (Gel `single`/`multi`).
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "kebab-case")]
pub enum Multiplicity {
    /// At most one.
    One,
    /// Any number.
    Many,
}

/// The cardinality of a link, declared on both ends.
///
/// `to` and `from` together give the relation's shape: `one`/`one` is 1:1,
/// `one`/`many` is N:1 from the source (1:N from the target), and
/// `many`/`many` is N:M. `required` is Gel's `required`: every source object
/// links to at least one target.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LinkCardinality {
    /// How many targets one source object links to.
    pub to: Multiplicity,
    /// How many source objects link to one target: the multiplicity of the
    /// inverse role.
    pub from: Multiplicity,
    /// Whether every source object links to at least one target. Defaults to
    /// `false`; the canonical form omits `false`.
    #[serde(default, skip_serializing_if = "is_false")]
    pub required: bool,
}

/// One link of an object type (or of an extension of one).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Link {
    /// The forward role: unique among the source type's members
    /// (properties, links, and inverse roles).
    pub id: LinkId,
    #[serde(
        default,
        deserialize_with = "present_non_null",
        skip_serializing_if = "Option::is_none"
    )]
    #[schemars(with = "String")]
    pub description: Option<String>,
    /// The object type or interface linked to (they share one name space).
    /// A link to an interface admits every type that implements it.
    pub target: ObjectTypeName,
    /// Multiplicity of both ends, and whether the link is required.
    pub cardinality: LinkCardinality,
    /// Whether this forward role must be acyclic in the final cell graph.
    /// Omitted means false. Subtype overrides must preserve true.
    #[serde(default, skip_serializing_if = "is_false")]
    pub acyclic: bool,
    /// The inverse role: the member of the target (and of every subtype or
    /// implementor of it) that traverses back to the source, with the
    /// multiplicity `cardinality.from`. Omitted: the relation is traversed
    /// only forward by name.
    #[serde(
        default,
        deserialize_with = "present_non_null",
        skip_serializing_if = "Option::is_none"
    )]
    #[schemars(with = "LinkId")]
    pub inverse: Option<LinkId>,
    /// Link properties: attributes of each link instance (a relationship
    /// object), with the shape of object type properties. Omitted when
    /// empty.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub properties: Vec<Property>,
}

/// One required link of an interface: every implementing type has a link
/// with this id whose target and cardinality narrow the requirement's.
///
/// A requirement names no inverse and no link properties: an interface is a
/// read contract on the implementing type, and cannot add members to the
/// target.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct InterfaceLink {
    /// Unique id among the interface's requirements; never contains `__`.
    pub id: LinkId,
    #[serde(
        default,
        deserialize_with = "present_non_null",
        skip_serializing_if = "Option::is_none"
    )]
    #[schemars(with = "String")]
    pub description: Option<String>,
    /// The widest target an implementing link may have: an object type (or
    /// a subtype of it) or an interface (or a type implementing it).
    pub target: ObjectTypeName,
    /// The widest cardinality an implementing link may have.
    pub cardinality: LinkCardinality,
}
