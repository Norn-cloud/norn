//! Checks over `EntityIR` v2 interface definitions (T1-ONT-1c, #2513): names,
//! required property ids and types, status, and the tenant rule; and required
//! links (T1-ONT-1d, #2514). Conformance
//! of implementing object types is an object-type check
//! ([`crate::object_types::codes`]); both run in
//! [`crate::check_entity_model`].

use std::collections::BTreeSet;

use norn_ir::identifier_limits::{MAX_LINK_NAME_BYTES, MAX_OBJECT_TYPE_NAME_BYTES};
use norn_ir::{InterfaceDef, InterfaceProperty, ObjectTypeName, PropertyTypeDef, PropertyTypeName};

use crate::Diagnostic;
use crate::object_types::MEMBER_ID_GRAMMAR;
use crate::property_types::{
    self, TypeResolution, check_constraints, check_type_members, error, matches_grammar,
    resolve_type, span_at,
};

/// Diagnostic codes emitted for interface definitions by
/// [`crate::check_entity_model`].
pub mod codes {
    /// An interface name is not `namespace.PascalName`.
    pub const INVALID_NAME: &str = "interface.validation.invalid_name";
    /// Two definitions in the model (interfaces or object types) share a name.
    pub const DUPLICATE_NAME: &str = "interface.validation.duplicate_name";
    /// A local interface has the name of an installed base interface or
    /// object type; base definitions are read-only.
    pub const SHADOWS_BASE: &str = "interface.validation.shadows_base";
    /// A required property id is not `[a-z][a-z0-9_]*`, or contains the
    /// reserved extension separator `__`.
    pub const INVALID_PROPERTY_ID: &str = "interface.validation.invalid_property_id";
    /// Two required properties of one interface share an id.
    pub const DUPLICATE_PROPERTY_ID: &str = "interface.validation.duplicate_property_id";
    /// A `deprecated` status's `since` is not a calendar date.
    pub const INVALID_STATUS_DATE: &str = "interface.validation.invalid_status_date";
    /// A `deprecated` status's `replacedBy` names no interface, or the
    /// deprecated interface itself.
    pub const UNKNOWN_REPLACEMENT: &str = "interface.validation.unknown_replacement";
    /// A tenant layer declares an interface; interfaces are registry
    /// definitions.
    pub const TENANT_INTERFACE: &str = "interface.validation.tenant_interface";
    /// A required link id is not `[a-z][a-z0-9_]*`, or contains the reserved
    /// extension separator `__`.
    pub const INVALID_LINK_ID: &str = "interface.validation.invalid_link_id";
    /// A required link id is declared twice, or is also a required property
    /// id.
    pub const DUPLICATE_LINK_ID: &str = "interface.validation.duplicate_link_id";
    /// A required link's `target` names no object type or interface.
    pub const UNKNOWN_LINK_TARGET: &str = "interface.validation.unknown_link_target";
}

/// Required property ids, types, and constraints of one local interface at
/// `base` (its JSON pointer).
pub(crate) fn check_requirements(
    base: &[&str],
    def: &InterfaceDef,
    catalog: &[PropertyTypeDef],
    registry: &BTreeSet<PropertyTypeName>,
    out: &mut Vec<Diagnostic>,
) {
    let mut ids = BTreeSet::new();
    for (index, requirement) in def.properties.iter().enumerate() {
        let position = index.to_string();
        let pointer = [base, &["properties", position.as_str()]].concat();
        check_requirement(&pointer, requirement, catalog, registry, out);
        if !ids.insert(&requirement.id) {
            out.push(error(
                codes::DUPLICATE_PROPERTY_ID,
                format!(
                    "required property id {:?} is declared twice",
                    requirement.id.as_str()
                ),
                span_at(&pointer, &["id"]),
            ));
        }
    }
}

fn check_requirement(
    pointer: &[&str],
    requirement: &InterfaceProperty,
    catalog: &[PropertyTypeDef],
    registry: &BTreeSet<PropertyTypeName>,
    out: &mut Vec<Diagnostic>,
) {
    let id = requirement.id.as_str();
    if !matches_grammar(&MEMBER_ID_GRAMMAR, id) || id.contains(norn_ir::EXTENSION_MEMBER_SEPARATOR)
    {
        out.push(error(
            codes::INVALID_PROPERTY_ID,
            format!(
                "required property id {id:?} must match [a-z][a-z0-9_]* without the reserved extension separator \"__\""
            ),
            span_at(pointer, &["id"]),
        ));
    }
    check_type_members(pointer, &requirement.property_type, out);
    property_types::check_record_refs(pointer, &requirement.property_type, catalog, registry, out);
    let resolved = resolve_type(&requirement.property_type, catalog, registry);
    if let (norn_ir::PropertyType::Ref { name }, TypeResolution::Unresolved { direct: true }) =
        (&requirement.property_type, &resolved.resolution)
    {
        out.push(error(
            property_types::codes::UNKNOWN_REF,
            format!(
                "ref {:?} names no property type in the catalog or registry",
                name.as_str()
            ),
            span_at(pointer, &["type", "name"]),
        ));
    }
    let base = match resolved.resolution {
        TypeResolution::Base(base) => Some(base),
        TypeResolution::Registry | TypeResolution::Unresolved { .. } => None,
    };
    check_constraints(pointer, &requirement.constraints, base, out);
}

/// Required link ids and targets of one local interface at `base` (its JSON
/// pointer). `known` tells whether a name is an object type or an interface.
pub(crate) fn check_link_requirements(
    base: &[&str],
    def: &InterfaceDef,
    known: impl Fn(&ObjectTypeName) -> bool,
) -> Vec<Diagnostic> {
    let mut out = Vec::new();
    let mut ids: BTreeSet<&str> = def
        .properties
        .iter()
        .map(|property| property.id.as_str())
        .collect();
    for (index, requirement) in def.links.iter().enumerate() {
        let position = index.to_string();
        let pointer = [base, &["links", position.as_str()]].concat();
        let id = requirement.id.as_str();
        if !matches_grammar(&MEMBER_ID_GRAMMAR, id)
            || id.contains(norn_ir::EXTENSION_MEMBER_SEPARATOR)
        {
            out.push(error(
                codes::INVALID_LINK_ID,
                format!(
                    "required link id {id:?} must match [a-z][a-z0-9_]* without the reserved extension separator \"__\""
                ),
                span_at(&pointer, &["id"]),
            ));
        }
        if id.len() > MAX_LINK_NAME_BYTES {
            out.push(error(
                crate::codes::IDENTIFIER_TOO_LONG,
                format!(
                    "required link id is {} UTF-8 bytes; the admission limit is {MAX_LINK_NAME_BYTES}. Rename it before installing updated definitions",
                    id.len()
                ),
                span_at(&pointer, &["id"]),
            ));
        }
        if !ids.insert(id) {
            out.push(error(
                codes::DUPLICATE_LINK_ID,
                format!("required link id {id:?} is already a required link or property id"),
                span_at(&pointer, &["id"]),
            ));
        }
        if !known(&requirement.target) {
            out.push(error(
                codes::UNKNOWN_LINK_TARGET,
                format!(
                    "required link {id:?} targets {:?}, which names no object type or interface",
                    requirement.target.as_str()
                ),
                span_at(&pointer, &["target"]),
            ));
        }
        if requirement.target.as_str().len() > MAX_OBJECT_TYPE_NAME_BYTES {
            out.push(error(
                crate::codes::IDENTIFIER_TOO_LONG,
                format!(
                    "required link target is {} UTF-8 bytes; the admission limit is {MAX_OBJECT_TYPE_NAME_BYTES}. Rename it before installing updated definitions",
                    requirement.target.as_str().len()
                ),
                span_at(&pointer, &["target"]),
            ));
        }
    }
    out
}
