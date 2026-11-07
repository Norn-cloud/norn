//! The one admission rule for properties that select objects in a bounded
//! predicate read: #1105 aggregate filters and windows, and #1104's
//! property-selected slices. Keeping it in one place stops the two forms from
//! drifting.
//!
//! A predicate property is *indexed scalar*: a stored, non-computed, direct
//! property of the type (links and related records are never implicit reads)
//! whose kind is a comparable scalar. Hosts may index exactly these
//! properties to answer predicate reads without a type scan; a property
//! outside the rule cannot key one.

use norn_expr::{FieldSource, RecordType, Type};

use crate::entity_instance::EffectiveObjectType;

/// The comparable value class of a scalar expression type.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ScalarClass {
    /// Text or an enumeration (compared as strings).
    Text,
    /// A signed integer.
    Integer,
    /// An exact decimal.
    Decimal,
    /// A boolean.
    Bool,
    /// A calendar date.
    Date,
    /// An instant with an explicit offset.
    Timestamp,
}

/// Property kinds an equality term may compare.
pub(crate) const EQUALITY: &[ScalarClass] = &[
    ScalarClass::Text,
    ScalarClass::Integer,
    ScalarClass::Decimal,
    ScalarClass::Bool,
    ScalarClass::Date,
];

/// Property kinds a window may range over.
pub(crate) const WINDOW: &[ScalarClass] = &[ScalarClass::Date, ScalarClass::Timestamp];

/// The scalar class of an expression type; optional types classify by their
/// inner type.
#[expect(
    clippy::wildcard_enum_match_arm,
    reason = "The foreign non-exhaustive enum requires a catch-all; any other kind is not a comparable scalar"
)]
pub(crate) fn scalar_class(ty: &Type) -> Option<ScalarClass> {
    match ty {
        Type::Optional(inner) => scalar_class(inner),
        Type::Text { .. } | Type::Enum { .. } => Some(ScalarClass::Text),
        Type::Integer => Some(ScalarClass::Integer),
        Type::Decimal { .. } | Type::Number => Some(ScalarClass::Decimal),
        Type::Bool => Some(ScalarClass::Bool),
        Type::Date => Some(ScalarClass::Date),
        Type::Timestamp { .. } => Some(ScalarClass::Timestamp),
        _ => None,
    }
}

/// Why a property cannot select objects in a predicate read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PredicatePropertyError {
    /// Not a stored, direct property of the type.
    NotStored,
    /// A computed (derived, read-only) property.
    Computed,
    /// Not a scalar of an allowed kind.
    NotComparable,
}

impl PredicatePropertyError {
    /// A short human-readable reason.
    pub(crate) const fn reason(self) -> &'static str {
        match self {
            Self::NotStored => "is not a stored property of the type",
            Self::Computed => "is computed; predicates select by stored properties",
            Self::NotComparable => "has a kind the predicate cannot compare",
        }
    }
}

/// The indexed-scalar rule: `property` of the type described by `record` and
/// `effective` can select objects in a predicate read, with a class in
/// `allowed`. Returns the property's scalar class and its expression type.
pub(crate) fn indexed_scalar_property(
    record: &RecordType,
    effective: &EffectiveObjectType,
    property: &str,
    allowed: &[ScalarClass],
) -> Result<(ScalarClass, Type), PredicatePropertyError> {
    let field = record
        .fields
        .get(property)
        .filter(|field| matches!(field.source, FieldSource::Property { .. }))
        .ok_or(PredicatePropertyError::NotStored)?;
    let stored = effective
        .property(property)
        .ok_or(PredicatePropertyError::NotStored)?;
    if stored.computed {
        return Err(PredicatePropertyError::Computed);
    }
    let class = scalar_class(&field.ty)
        .filter(|class| allowed.contains(class))
        .ok_or(PredicatePropertyError::NotComparable)?;
    Ok((class, field.ty.clone()))
}
