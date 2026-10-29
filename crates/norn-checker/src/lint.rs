//! Ontology lint ("clippy for ontologies"; 2026-09-09 ruling §6): a T3
//! ontology profile of deterministic rules over an artifact set, with
//! Spectral-style severity kept separate from gating.
//!
//! # Rules
//!
//! Every rule is registered in [`rules::RULES`] under a stable rule id
//! (`NORN-NAME-001`) and emits one stable diagnostic code
//! (`ontology.lint.*`, registered in [`crate::codes::REGISTRY`] with its
//! severity). Each rule is a pure function of the [`LintInput`]:
//!
//! | rule | code | severity | module |
//! | --- | --- | --- | --- |
//! | `NORN-NAME-001` | `ontology.lint.plural_type_name` | warning | [`naming`] |
//! | `NORN-NAME-002` | `ontology.lint.redundant_member_prefix` | warning | [`naming`] |
//! | `NORN-NAME-003` | `ontology.lint.versioned_name` | warning | [`naming`] |
//! | `NORN-OBJ-001` | `ontology.lint.duplicate_definition` | error | [`duplicates`] |
//! | `NORN-SRC-001` | `ontology.lint.untraceable_provenance` | error | [`provenance`] |
//! | `NORN-SRC-002` | `ontology.lint.incomplete_provenance` | warning | [`provenance`] |
//! | `NORN-MAP-001` | `ontology.lint.unknown_mapping_target` | error | [`mapping`] |
//! | `NORN-MAP-002` | `ontology.lint.unmapped_required_member` | error | [`mapping`] |
//! | `NORN-MAP-003` | `ontology.lint.unmapped_source_field` | warning | [`mapping`] |
//! | `NORN-OBJ-002` | `ontology.lint.mega_object` | warning | [`smells`] |
//! | `NORN-OBJ-003` | `ontology.lint.deep_hierarchy` | warning | [`smells`] |
//! | `NORN-REL-001` | `ontology.lint.weak_relationship` | warning | [`smells`] |
//! | `NORN-SRC-003` | `ontology.lint.source_mirroring` | warning | [`smells`] |
//!
//! Each module documents its rule's exact, deterministic test. The smells
//! (tier 2) take [`LintThresholds`], whose [`Default`] is the documented
//! profile (mega-object above 40 own members, more than 3 supertypes,
//! source mirroring at 5 or more shared names covering 80% of both sides);
//! the same values are listed per rule in [`rules::RULES`]. [`fingerprint`]
//! is the structural fingerprint the smells and nearest-concept retrieval
//! (T8-SIM-1) share.
//!
//! # Severity and gating
//!
//! Severity says how bad a finding is; whether it fails a run is a separate
//! choice, [`FailSeverity`] (the `--fail-severity` knob, as in Spectral). The
//! default fails on errors only, so warnings inform without blocking.
//!
//! # Inputs
//!
//! Findings are reported on the candidate artifacts only. The installed base
//! resolves names (supertypes, duplicates of base definitions, mapping
//! targets) and is never linted. Mapping coverage arrives as
//! [`MappingCoverage`] values computed by the `MappingIR` compile step
//! (T8-MAP-1b); the lint does not read mappings itself.

pub mod coverage;
pub mod duplicates;
pub mod fingerprint;
pub mod mapping;
mod model;
pub mod naming;
pub mod provenance;
pub mod rules;
pub mod smells;

use norn_ir::{DiagnosticRemediation, DiagnosticSeverity};

pub use coverage::{
    IgnoredSourceField, MappingCoverage, MemberCoverage, MemberRef, MemberStatus, SourceRef,
};
pub use fingerprint::{StructuralFingerprint, fingerprint as structural_fingerprint};
pub use model::LintInput;
pub use rules::{LintRule, LintTier, RULES, Threshold};
pub use smells::LintThresholds;

use crate::{Diagnostic, DiagnosticCode, Span};
use model::LintSet;

/// Run every lint rule over `input` with the default [`LintThresholds`].
/// Findings come grouped by rule family (naming, duplicates, provenance,
/// mapping coverage, smells) and within a family in artifact order, so the
/// output is deterministic.
#[must_use]
pub fn lint_artifact_set(input: &LintInput<'_>) -> Vec<Diagnostic> {
    lint_artifact_set_with(input, &LintThresholds::default())
}

/// [`lint_artifact_set`] with explicit smell thresholds.
#[must_use]
pub fn lint_artifact_set_with(
    input: &LintInput<'_>,
    thresholds: &LintThresholds,
) -> Vec<Diagnostic> {
    let set = LintSet::new(input);
    let mut out = naming::lint_naming(&set);
    out.extend(duplicates::lint_duplicates(&set));
    out.extend(provenance::lint_provenance(&set));
    out.extend(mapping::lint_mapping_coverage(&set));
    out.extend(smells::lint_smells(&set, thresholds));
    out
}

/// The lowest severity that fails a lint run (Spectral's `--fail-severity`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum FailSeverity {
    /// Fail on errors (the default).
    #[default]
    Error,
    /// Fail on warnings and errors.
    Warning,
    /// Fail on any finding.
    Info,
    /// Never fail; report only.
    Never,
}

impl FailSeverity {
    /// Parse a `--fail-severity` value: `error`, `warning` (or `warn`),
    /// `info`, or `never`.
    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "error" => Some(Self::Error),
            "warning" | "warn" => Some(Self::Warning),
            "info" => Some(Self::Info),
            "never" => Some(Self::Never),
            _ => None,
        }
    }

    /// Whether a finding of `severity` fails the run.
    #[must_use]
    pub const fn fails_on(self, severity: &DiagnosticSeverity) -> bool {
        match self {
            Self::Error => matches!(severity, DiagnosticSeverity::Error),
            Self::Warning => matches!(
                severity,
                DiagnosticSeverity::Error | DiagnosticSeverity::Warning
            ),
            Self::Info => true,
            Self::Never => false,
        }
    }

    /// Whether `findings` fail the run.
    #[must_use]
    pub fn fails(self, findings: &[Diagnostic]) -> bool {
        findings
            .iter()
            .any(|finding| self.fails_on(&finding.severity))
    }
}

/// A lint finding with `code`'s registered severity (an unregistered code,
/// which the registry tests forbid, is an error: fail closed).
fn finding(
    code: &'static str,
    message: String,
    artifact_id: &str,
    pointer: Option<String>,
) -> Diagnostic {
    let severity = crate::codes::lookup(code)
        .map_or(DiagnosticSeverity::Error, |entry| entry.severity.clone());
    Diagnostic {
        code: DiagnosticCode::new(code),
        severity,
        message,
        span: pointer.map(Span::pointer),
        artifact_id: Some(artifact_id.to_owned()),
        remediation: Some(DiagnosticRemediation::FixDefinition),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fail_severity_gates_independently_of_severity() {
        assert_eq!(FailSeverity::default(), FailSeverity::Error);
        assert_eq!(FailSeverity::parse("warn"), Some(FailSeverity::Warning));
        assert_eq!(FailSeverity::parse("hint"), None);
        let warning = DiagnosticSeverity::Warning;
        assert!(!FailSeverity::Error.fails_on(&warning));
        assert!(FailSeverity::Warning.fails_on(&warning));
        assert!(FailSeverity::Info.fails_on(&DiagnosticSeverity::Info));
        assert!(!FailSeverity::Never.fails_on(&DiagnosticSeverity::Error));
    }
}
