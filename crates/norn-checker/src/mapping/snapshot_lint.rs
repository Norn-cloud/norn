//! Advisory findings for lifecycle snapshots the kernel could not reconcile.

use norn_ir::mapping::SnapshotDeviationReason;
use norn_ir::{DataQualityFinding, DataQualityOutcome};

use crate::{Diagnostic, DiagnosticCode, DiagnosticSeverity};

/// Report observed lifecycle findings with no unique shortest declared path.
///
/// The kernel remains authoritative: this is a warning projection of typed
/// findings it already recorded, not a second reconciliation attempt. Each
/// subject/lineage pair is retained in the diagnostic projection.
#[must_use]
pub fn lint_observed_deviations(findings: &[DataQualityFinding]) -> Vec<Diagnostic> {
    let mut diagnostics = Vec::new();
    for finding in findings {
        let DataQualityOutcome::ObservedOutOfBand {
            property,
            from,
            state,
            reason,
        } = &finding.outcome
        else {
            continue;
        };
        let from = from.as_deref().unwrap_or("<initial>");
        let reason = match reason {
            SnapshotDeviationReason::NoDeclaredPath => "no_declared_path",
            SnapshotDeviationReason::AmbiguousShortestPath => "ambiguous_shortest_path",
        };
        for subject in &finding.subjects {
            for lineage in &finding.provenance {
                let provenance = &lineage.provenance;
                diagnostics.push(Diagnostic {
                    code: DiagnosticCode::new(super::codes::OBSERVED_OUT_OF_BAND),
                    severity: DiagnosticSeverity::Warning,
                    message: format!(
                        "the observed lifecycle snapshot from {}/{} for {}/{}.{} ({} -> {}) has no unique shortest declared path; mappingVersion={}, observedAt={}, reason={}",
                        provenance.source.system,
                        provenance.source.record,
                        subject.object_type.as_str(),
                        subject.key,
                        property.as_str(),
                        from,
                        state,
                        provenance.mapping_version.as_str(),
                        provenance.observed_at.as_ref().map_or("<unspecified>", |at| at.as_str()),
                        reason,
                    ),
                    span: None,
                    artifact_id: None,
                    remediation: None,
                });
            }
        }
    }
    diagnostics
}
