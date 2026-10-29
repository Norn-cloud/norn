//! Tenant and package extensions of object types (T1-ONT-1c, #2513; ruling
//! 2026-09-09 §5 "tenant extension namespace", narrowed by 2026-09-25 §27).
//!
//! Base types are read-only to the layers that install them. A layer adds to
//! an installed type through an [`ObjectTypeExtension`]: new properties
//! whose ids are `ns__name`, where `ns` is the layer's
//! [`ExtensionNamespace`] (the Salesforce managed-package model). Base
//! member ids never contain `__`, so an extension member can never shadow a
//! base member, today or after a base package upgrade, and two layers'
//! extensions never collide. An extension may also add links (T1-ONT-1d,
//! #2514), whose ids and inverse roles are `ns__name` too.
//!
//! Every rule (namespace grammar, member prefixes, shadowing, the tenant
//! layer's namespace) is a `norn-checker` diagnostic, not a decode error.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::link::Link;
use super::object::{ObjectTypeName, Property};
use crate::members::present_non_null;

/// Grammar of an [`ExtensionNamespace`] (published as the schema `pattern`;
/// `norn-checker` enforces it): one lower-case alphanumeric segment, without
/// `_`, so `ns__name` splits unambiguously at its first `__`.
pub const EXTENSION_NAMESPACE_PATTERN: &str = r"^[a-z][a-z0-9]*$";

/// Separator between an extension namespace and a member name
/// (`acme__loyalty_tier`).
pub const EXTENSION_MEMBER_SEPARATOR: &str = "__";

/// The namespace a tenant (or a package) adds members under (`acme`).
/// Allocated by the registry (T8-REG-4); unique per tenant.
#[derive(
    Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
#[serde(transparent)]
pub struct ExtensionNamespace(#[schemars(regex(pattern = EXTENSION_NAMESPACE_PATTERN))] pub String);

impl ExtensionNamespace {
    /// The namespace as a string slice.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Members one layer adds to an object type it does not own.
///
/// The members apply to the target and every subtype of it. An extension
/// cannot remove, rename, or change a member of the target, and cannot add a
/// key or an invariant: identity and invariants belong to the type's owner.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ObjectTypeExtension {
    /// The namespace every added member id is prefixed with.
    pub namespace: ExtensionNamespace,
    /// The extended object type.
    pub target: ObjectTypeName,
    #[serde(
        default,
        deserialize_with = "present_non_null",
        skip_serializing_if = "Option::is_none"
    )]
    #[schemars(with = "String")]
    pub description: Option<String>,
    /// Added properties; each id is `<namespace>__<name>`. Omitted when
    /// empty.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub properties: Vec<Property>,
    /// Added links; each id, and each inverse role, is
    /// `<namespace>__<name>`. Omitted when empty.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub links: Vec<Link>,
}
