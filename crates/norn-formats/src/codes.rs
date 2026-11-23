//! Importer diagnostic codes.
//!
//! Importer findings are not checker findings: they describe how source
//! metadata was lowered (what was skipped, downgraded, or not translated), so
//! they live here rather than in `norn-checker`'s registry. Codes are stable:
//! never renamed or reused. Every code carries one fixed severity and a
//! remediation class, and is listed in [`ALL`].
//!
//! An `error` means the import is incomplete (a document, a transition, or a
//! lifecycle was dropped); `warning` means something was downgraded or not
//! translated; `info` records a deliberate, lossless choice.

use norn_ir::DiagnosticRemediation::{self, FixDefinition};
use norn_ir::DiagnosticSeverity::{self, Error, Warning};

/// One registered importer diagnostic code.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Code {
    /// The stable dotted code.
    pub code: &'static str,
    /// The severity every emission carries.
    pub severity: DiagnosticSeverity,
    /// The remediation class every emission carries.
    pub remediation: DiagnosticRemediation,
}

const fn code(
    code: &'static str,
    severity: DiagnosticSeverity,
    remediation: DiagnosticRemediation,
) -> Code {
    Code {
        code,
        severity,
        remediation,
    }
}

/// Invalid XML Schema or source admission.
pub const XSD_INVALID: Code = code("format.xsd.invalid", Error, FixDefinition);
/// Preserved XSD semantics requiring further lowering.
pub const XSD_DEFERRED: Code = code("format.xsd.deferred", Warning, FixDefinition);

/// Invalid RDF/OWL input or source admission.
pub const OWL_INVALID: Code = code("format.owl.invalid", Error, FixDefinition);
/// A statement outside the OWL 2 constructs the reader lowers.
pub const OWL_DEFERRED: Code = code("format.owl.deferred", Warning, FixDefinition);
/// Lowered with a weaker representation (lexical text for an unbounded or
/// offset-free datatype, one value for a multi-valued datatype property).
pub const OWL_DEGRADED: Code = code("format.owl.degraded", Warning, FixDefinition);
/// Class disjointness: no interface-disjointness construct; it needs a law.
pub const OWL_DISJOINTNESS: Code = code("format.owl.disjointness", Warning, FixDefinition);
/// Class equivalence or a cycle of subclass axioms: Norn types are asserted,
/// never classified by sufficient conditions.
pub const OWL_EQUIVALENCE: Code = code("format.owl.equivalence", Warning, FixDefinition);
/// A union, complement, enumeration or nested expression where one named type
/// is needed.
pub const OWL_CLASS_EXPRESSION: Code = code("format.owl.class_expression", Warning, FixDefinition);
/// A count bound other than zero or one; kept as many, or deferred to a law.
pub const OWL_CARDINALITY_BOUND: Code =
    code("format.owl.cardinality_bound", Warning, FixDefinition);
/// A fixed-value (`hasValue`, `hasSelf`) restriction; it needs an invariant.
pub const OWL_VALUE_RESTRICTION: Code =
    code("format.owl.value_restriction", Warning, FixDefinition);
/// Sub-, inverse, equivalent or chained properties and property
/// characteristics other than functional: inference rules.
pub const OWL_PROPERTY_AXIOM: Code = code("format.owl.property_axiom", Warning, FixDefinition);
/// A reference to a class or property outside the pinned import set.
pub const OWL_EXTERNAL_REFERENCE: Code =
    code("format.owl.external_reference", Warning, FixDefinition);
/// A property no domain or restriction gives an owner type, or a link with no
/// single named target.
pub const OWL_UNPLACED_PROPERTY: Code =
    code("format.owl.unplaced_property", Warning, FixDefinition);
/// A refinement not lowered because inherited requirements would conflict.
pub const OWL_INHERITANCE_CONFLICT: Code =
    code("format.owl.inheritance_conflict", Warning, FixDefinition);
/// A qualified restriction (`some F`, `min n F`, `max n F`, `exactly n F`)
/// whose filler no established range covers: it counts only the values that
/// are `F`, and separate values may satisfy other restrictions on the same
/// property, so neither its target nor its bound holds for the whole link.
pub const OWL_FILLER_SCOPED: Code = code("format.owl.filler_scoped", Warning, FixDefinition);
/// A value OWL asserts to exist (`some`, `min n`, `exactly n`): an
/// open-world claim, lowered as a required member only under the import's
/// closed-world witness policy.
pub const OWL_OPEN_WORLD: Code = code("format.owl.open_world", Warning, FixDefinition);
/// A named individual: reference data for rows, not a type.
pub const OWL_INDIVIDUAL: Code = code("format.owl.individual", Warning, FixDefinition);
/// Documentation (annotations other than label and definition, the ontology
/// header).
pub const OWL_ANNOTATION: Code = code("format.owl.annotation", Warning, FixDefinition);

/// Every registered importer code.
pub const ALL: &[Code] = &[
    XSD_INVALID,
    XSD_DEFERRED,
    OWL_INVALID,
    OWL_DEFERRED,
    OWL_DEGRADED,
    OWL_DISJOINTNESS,
    OWL_EQUIVALENCE,
    OWL_CLASS_EXPRESSION,
    OWL_CARDINALITY_BOUND,
    OWL_VALUE_RESTRICTION,
    OWL_PROPERTY_AXIOM,
    OWL_EXTERNAL_REFERENCE,
    OWL_UNPLACED_PROPERTY,
    OWL_INHERITANCE_CONFLICT,
    OWL_FILLER_SCOPED,
    OWL_OPEN_WORLD,
    OWL_INDIVIDUAL,
    OWL_ANNOTATION,
];
