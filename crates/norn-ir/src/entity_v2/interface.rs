//! Interfaces (T1-ONT-1c, #2513; ruling 2026-09-09 §5, 2026-09-25 §27).
//!
//! An [`InterfaceDef`] is a named contract of required properties that
//! several object types implement (`party.Party`, `commerce.Item`): the unit
//! Norn Core maps source systems to ("map interfaces, not systems"). An
//! implementing type declares, or inherits, a property with each required id
//! whose type narrows the requirement's; the requirement's constraints then
//! apply to that property too. Likewise each required link id names a link
//! of the type whose target and cardinality narrow the requirement's
//! (T1-ONT-1d, #2514).
//!
//! Interfaces are flat: an interface does not extend another. Every rule
//! (names, conformance, conflicting requirements) is a `norn-checker`
//! diagnostic, not a decode error.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::invariant::Invariant;
use super::link::InterfaceLink;
use super::object::{OBJECT_TYPE_NAME_PATTERN, PropertyId};
use super::property::{PropertyConstraints, PropertyType};
use super::status::ResourceStatus;
use crate::MarkingId;
use crate::members::present_non_null;

/// Qualified name of an interface: the object type name grammar
/// ([`OBJECT_TYPE_NAME_PATTERN`], `party.Party`). Interfaces and object types
/// share one name space.
#[derive(
    Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
#[serde(transparent)]
pub struct InterfaceName(#[schemars(regex(pattern = OBJECT_TYPE_NAME_PATTERN))] pub String);

impl InterfaceName {
    /// The name as a string slice.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// The interface's domain namespace: every segment before the last `.`.
    #[must_use]
    pub fn domain(&self) -> Option<&str> {
        self.0.rsplit_once('.').map(|(domain, _)| domain)
    }
}

/// An interface: the properties and links every implementing object type
/// has, and the invariants every such object satisfies.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct InterfaceDef {
    /// Unique name, across interfaces and object types.
    pub name: InterfaceName,
    #[serde(
        default,
        deserialize_with = "present_non_null",
        skip_serializing_if = "Option::is_none"
    )]
    #[schemars(with = "String")]
    pub description: Option<String>,
    /// The definition's resource status. Omitted means `active`.
    #[serde(default, skip_serializing_if = "ResourceStatus::is_active")]
    pub status: ResourceStatus<InterfaceName>,
    /// Required properties, in presentation order. Omitted when empty.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub properties: Vec<InterfaceProperty>,
    /// Required links, in presentation order; their ids are distinct from
    /// the required property ids. Omitted when empty.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub links: Vec<InterfaceLink>,
    /// Classification markings inherited by every implementing property.
    /// Omitted when empty.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub markings: Vec<MarkingId>,
    /// Invariants every implementing object type satisfies, in addition to
    /// its own and inherited invariants. Omitted when empty.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub invariants: Vec<Invariant>,
}

/// One required property of an interface: an id, a type an implementing
/// property must narrow, and constraints that apply to it.
///
/// A requirement has no default and is never `computed`: an interface is a
/// read contract, and an implementing type may satisfy it with a stored or a
/// computed property.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct InterfaceProperty {
    /// Unique id within the interface; never contains `__`.
    pub id: PropertyId,
    #[serde(
        default,
        deserialize_with = "present_non_null",
        skip_serializing_if = "Option::is_none"
    )]
    #[schemars(with = "String")]
    pub description: Option<String>,
    /// The widest type an implementing property may have.
    #[serde(rename = "type")]
    pub property_type: PropertyType,
    /// Classification markings required of every implementing property.
    /// Implementations may add markings but cannot remove these. Omitted when
    /// empty.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub markings: Vec<MarkingId>,
    /// Constraints every implementing property is held to. Omitted when
    /// empty.
    #[serde(default, skip_serializing_if = "PropertyConstraints::is_empty")]
    pub constraints: PropertyConstraints,
}
