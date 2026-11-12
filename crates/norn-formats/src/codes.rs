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
use norn_ir::DiagnosticSeverity::{self, Error, Info, Warning};

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

/// Source bytes or their admission are invalid; nothing was imported.
pub const SOURCE_INVALID: Code = code("format.source.invalid", Error, FixDefinition);
/// Recorded in `SourceSchemaIR` (or its provenance extension) only; lowering
/// it into `EntityIR` is a later mapping step. Deliberate and lossless.
pub const SOURCE_SCHEMA_ONLY: Code = code("format.source_schema_only", Info, FixDefinition);
/// A reference whose target or binding the source does not declare; the
/// evidence that is known is kept for a later mapping step.
pub const REFERENCE_UNRESOLVED: Code = code("format.reference_unresolved", Warning, FixDefinition);
/// Preserved source semantics that need further lowering (view definitions,
/// data rows, storage and security settings).
pub const SOURCE_DEFERRED: Code = code("format.source_deferred", Warning, FixDefinition);

/// Every registered importer code.
pub const ALL: &[Code] = &[
    XSD_INVALID,
    XSD_DEFERRED,
    SOURCE_INVALID,
    SOURCE_SCHEMA_ONLY,
    REFERENCE_UNRESOLVED,
    SOURCE_DEFERRED,
];
