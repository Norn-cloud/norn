//! Value types and closed property-kind enums for effective entity contracts.

use norn_ir::{DefaultValue, Lifecycle, PropertyId, PropertyType, PropertyTypeName};

/// One effective lifecycle of an object type: the machine the checker
/// enforces and the diagram renders (ruling 2026-09-25 §2, "this one list").
#[derive(Debug, Clone, PartialEq)]
pub struct EffectiveLifecycle {
    /// The lifecycle property's effective enum values (the states), in
    /// presentation order.
    pub states: Vec<String>,
    /// The effective machine as a lifecycle: inherited guards first, states
    /// in presentation order.
    pub lifecycle: Lifecycle,
}

/// A feature of an object type whose instance semantics are not implemented,
/// so writes of the type fail closed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Unsupported {
    /// What the type uses, for a message (`links`, `property kind 'decimal'
    /// on 'price'`).
    pub feature: String,
    /// Dotted path of the member in the artifact.
    pub path: &'static str,
}

/// What a property's type resolves to.
#[derive(Debug, Clone, PartialEq)]
pub enum EffectiveKind {
    /// A local, non-`ref` type.
    Base(PropertyType),
    /// A registry value type, whose definition is not available (T8-STD-1).
    Registry(PropertyTypeName),
    /// A `ref` that does not resolve.
    Unresolved,
}

/// Who may write a property.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WriteAuthority {
    /// FA-PROTECT-1: only rule outputs or the ACTOR-STAMP-1 create stamp write it.
    RulesOnly,
    /// Callers and rules may write it.
    CallersAndRules,
}

/// Whether a property may be mutated after object creation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PostCreateMutability {
    /// Updates after creation are permitted.
    Mutable,
    /// ORIGIN-DATAFLOW-1: writes after creation are forbidden.
    Immutable,
}

/// Effective-property dependencies read by a computed property's expression.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ComputedInputs {
    /// The expression reads or passes through the whole object root, so
    /// runtime metadata must union every property input.
    AllObjectProperties,
    /// Direct effective-property dependencies read by the computed expression.
    Named(Vec<PropertyId>),
}

/// Closed storage/derivation kind of an [`super::EffectiveProperty`].
///
/// Eliminates impossible states such as a computed property carrying a stored
/// default or a stored property carrying computed input dependencies.
#[derive(Debug, Clone, PartialEq)]
pub enum PropertyKind {
    /// Stored property persisted in cell facts.
    Stored {
        /// The effective literal default (IR encoding), if declared.
        default: Option<DefaultValue>,
    },
    /// Derived, read-only property evaluated from a declarative expression.
    Computed {
        /// Effective-property dependencies read by the computed expression.
        inputs: ComputedInputs,
    },
}

/// Why an instance value is rejected.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InstanceError {
    /// The property uses a feature whose instance semantics are not
    /// implemented (see [`super::EffectiveObjectType::unsupported`]).
    Unsupported(String),
    /// The value is not of the property's type; holds the type's label.
    TypeMismatch(&'static str),
    /// The value is of the type but violates a constraint; holds why.
    ConstraintViolation(String),
}
