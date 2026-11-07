//! PROOF-TRANSITION-1: a closed one-create relation, independent of solver syntax.
//!
//! The initial store is empty. A single creation chooses finite inputs, checks
//! guards, then writes every scalar field at once. Occupied stores only stutter.
//! Expressions use Norn's common Boolean/additive grammar with scaled integral
//! literals and synthetic state/input symbols. Only checker-owned lowering
//! qualifies a relation; constructing this native view supplies no evidence.

use crate::{ObjectTypeName, PropertyId, PropertyType, ZenExpression};

/// Where the final stored value came from; never an implicit universal claim.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AssignmentOrigin {
    /// Caller input selected by the finite profile.
    Input,
    /// Last assigning derivation's zero-based declaration index.
    Derivation(usize),
    /// Stored default used because the profile omits the caller input.
    DefaultAbsentInput,
}

/// One derivation, retained in declaration order after earlier substitutions.
#[derive(Debug, Clone, PartialEq)]
pub struct Derivation {
    /// Source target property.
    pub property: PropertyId,
    /// Exact substituted, scaled expression.
    pub expression: ZenExpression,
    /// Whether the runtime requires the target input before this derivation.
    pub input_required: bool,
}

/// One stored numeric coefficient, interpreted by its effective declaration.
#[derive(Debug, Clone, PartialEq)]
pub struct ScalarState {
    /// Stable synthetic state symbol.
    pub symbol: String,
    /// Source property identity.
    pub property: PropertyId,
    /// Effective fixed-scale/unit decimal contract.
    pub property_type: PropertyType,
}

/// Inclusive finite coefficient domain for one writer input.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FiniteInput {
    /// Synthetic input symbol used by the normalized expressions.
    pub symbol: String,
    /// Declared input property, with the matching stored scale/unit contract.
    pub property: PropertyId,
    /// Least admitted input coefficient.
    pub lower: i64,
    /// Greatest admitted input coefficient.
    pub upper: i64,
}

/// Finite empty-to-singleton creation and its pointwise law interpretation.
/// Numeric variables are zero while absent; no stored object is implied.
#[derive(Debug, Clone, PartialEq)]
pub struct CreateRelation {
    /// Exact governing law artifact.
    pub law_artifact: String,
    /// Exact selected writer artifact.
    pub writer_artifact: String,
    /// Concrete created object type.
    pub object_type: ObjectTypeName,
    /// Singleton object identity fixed by the bounds profile.
    pub identity: String,
    /// Stored coefficient variables with scale/unit provenance.
    pub state: Vec<ScalarState>,
    /// Finite nondeterministic input choices.
    pub inputs: Vec<FiniteInput>,
    /// Numeric caller fields absent under this profile, including derived fields.
    pub absent_inputs: Vec<PropertyId>,
    /// Sequential source derivations, before preconditions and stored defaults.
    pub derivations: Vec<Derivation>,
    /// Final value source, keyed by the corresponding stored state symbol.
    pub origins: std::collections::BTreeMap<String, AssignmentOrigin>,
    /// Simultaneous post-state assignments, keyed by state symbol.
    pub updates: Vec<(String, ZenExpression)>,
    /// Ordered native preconditions after derivation substitution.
    pub guards: Vec<ZenExpression>,
    /// Law applicability on an existing object.
    pub condition: ZenExpression,
    /// Required predicate, including an admitted Never negation.
    pub response: ZenExpression,
}
