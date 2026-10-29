//! EntityIR v2 vocabulary (ruling 2026-09-09 §5, extended by 2026-09-25 §2).
//!
//! An `entity` artifact carries one EntityIR v2 definition: an
//! [`object::ObjectTypeDef`] ([`crate::EntityRootV2`], T1-IR-1b #2507), an
//! [`interface::InterfaceDef`], an [`extension::ObjectTypeExtension`], or a
//! catalog of [`property::PropertyTypeDef`]s (#2844); the v1 map-backed form
//! ([`crate::EntityRootV1`]) keeps its wire bytes and hashes, and T1-ONT-4
//! (#2517) lowers it into these types. Validation of these types lives in
//! `norn-checker`.
//!
//! - [`property`]: shared property types (T1-ONT-1a, #2511).
//! - [`object`]: object types, properties, identity (single/composite keys
//!   and identity rules), defaults, and computed properties (T1-ONT-1b,
//!   #2512); single inheritance and implemented interfaces (T1-ONT-1c,
//!   #2513).
//! - [`interface`], [`status`], [`extension`]: interfaces, resource status,
//!   and tenant/package extensions under an extension namespace (T1-ONT-1c,
//!   #2513).
//! - [`link`], [`invariant`]: links with both-end cardinality, inverse roles,
//!   and link properties; required links on interfaces; extension links; and
//!   declared invariants (T1-ONT-1d, #2514).
//! - [`lifecycle`]: declared lifecycles, the allowed transitions of an
//!   `enum` status property (T3-LIFE-1, #2672).

use schemars::{Schema, schema_for};

pub mod extension;
pub mod interface;
pub mod invariant;
pub mod lifecycle;
pub mod link;
pub mod naming_series;
pub mod object;
pub mod property;
pub mod status;

/// Emit the JSON Schema of one shared property type definition
/// ([`property::PropertyTypeDef`]).
#[must_use]
pub fn property_type_def_schema() -> Schema {
    schema_for!(property::PropertyTypeDef)
}

/// Emit the JSON Schema of one object type definition
/// ([`object::ObjectTypeDef`]).
#[must_use]
pub fn object_type_def_schema() -> Schema {
    schema_for!(object::ObjectTypeDef)
}

/// Emit the JSON Schema of one interface definition
/// ([`interface::InterfaceDef`]).
#[must_use]
pub fn interface_def_schema() -> Schema {
    schema_for!(interface::InterfaceDef)
}

/// Emit the JSON Schema of one object type extension
/// ([`extension::ObjectTypeExtension`]).
#[must_use]
pub fn object_type_extension_schema() -> Schema {
    schema_for!(extension::ObjectTypeExtension)
}
