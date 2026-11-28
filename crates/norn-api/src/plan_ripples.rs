//! Exact transitive ripple-effect DTOs for `POST /v1/cell/definitions:plan`.

use norn_checker::ripple::{BudgetRefusal, ImpactKind, RippleEntry, RippleReport, RippleStatus};
use norn_ir::ArtifactEnvelope;
use serde::Serialize;
use utoipa::ToSchema;

use crate::cell_definitions::PlannedArtifact;

/// Completeness status of a planned ripple-effect report.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum PlanRippleStatus {
    /// Exact transitive closure completed within the graph budget.
    Exact,
    /// Graph budget was exhausted or projection failed; no partial set is claimed complete.
    Unknown,
}

impl From<RippleStatus> for PlanRippleStatus {
    fn from(status: RippleStatus) -> Self {
        match status {
            RippleStatus::Exact => Self::Exact,
            RippleStatus::Unknown => Self::Unknown,
        }
    }
}

/// Classification of how a planned root change ripples into an impacted artifact.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum PlanRippleImpactKind {
    /// Artifact itself was directly added, changed, or removed.
    DirectChange,
    /// Subtype, interface implementor, extension, or defined concept affected by a base change.
    SpecializationAffected,
    /// Action, rule, connector, or mapping writer affected by a dependency or type change.
    WriterAffected,
    /// Law artifact affected by a subject, subtype, writer, or bound-response change.
    LawAffected,
    /// Lifecycle-bearing entity or relation affected by an upstream change.
    LifecycleAffected,
    /// Policy or access artifact affected by a target resource or action change.
    PolicyAffected,
    /// Derived view, model, label catalog, widget, or UI page affected by an upstream change.
    ProjectionOrUiAffected,
    /// Artifact depends directly on a root artifact that was removed in the candidate set.
    RemovedDependencyBroken,
}

impl From<ImpactKind> for PlanRippleImpactKind {
    fn from(kind: ImpactKind) -> Self {
        match kind {
            ImpactKind::DirectChange => Self::DirectChange,
            ImpactKind::SpecializationAffected => Self::SpecializationAffected,
            ImpactKind::WriterAffected => Self::WriterAffected,
            ImpactKind::LawAffected => Self::LawAffected,
            ImpactKind::LifecycleAffected => Self::LifecycleAffected,
            ImpactKind::PolicyAffected => Self::PolicyAffected,
            ImpactKind::ProjectionOrUiAffected => Self::ProjectionOrUiAffected,
            ImpactKind::RemovedDependencyBroken => Self::RemovedDependencyBroken,
        }
    }
}

/// One artifact impacted directly or transitively by a planned definition change.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct PlanRippleEntry {
    /// Impacted artifact envelope id.
    pub artifact_id: String,
    /// Snake-case artifact kind (`entity`, `action`, `law`, `widget`, …).
    pub kind: String,
    /// Sorted, deduplicated root changed or removed artifact ids causing this ripple.
    pub triggered_by: Vec<String>,
    /// Shortest concrete dependency chain from a triggering root to `artifactId`.
    pub via: Vec<String>,
    /// Classification of the impact on `artifactId`.
    pub impact_kind: PlanRippleImpactKind,
}

impl From<RippleEntry> for PlanRippleEntry {
    fn from(entry: RippleEntry) -> Self {
        Self {
            artifact_id: entry.artifact_id,
            kind: entry.kind,
            triggered_by: entry.triggered_by,
            via: entry.via,
            impact_kind: entry.impact_kind.into(),
        }
    }
}

/// Explicit budget refusal metadata when [`PlanRippleReport::status`] is [`PlanRippleStatus::Unknown`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct PlanRippleBudgetExceeded {
    /// Exhausted resource (`nodes`, `edges`, or `steps`).
    pub resource: String,
    /// Caller-supplied ceiling that was exceeded.
    pub limit: u64,
    /// Human-readable explanation.
    pub detail: String,
}

impl From<BudgetRefusal> for PlanRippleBudgetExceeded {
    fn from(refusal: BudgetRefusal) -> Self {
        Self {
            resource: refusal.resource,
            limit: u64::try_from(refusal.limit).unwrap_or(u64::MAX),
            detail: refusal.detail,
        }
    }
}

/// Deterministic transitive ripple-effect report attached to a [`crate::cell_definitions::DefinitionsPlan`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct PlanRippleReport {
    /// [`PlanRippleStatus::Exact`] when traversal finished within budget; [`PlanRippleStatus::Unknown`] on refusal.
    pub status: PlanRippleStatus,
    /// Sorted, deduplicated root changed or removed artifact ids evaluated.
    pub roots: Vec<String>,
    /// Impacted artifacts in deterministic wave order (empty when [`PlanRippleStatus::Unknown`]).
    pub ripples: Vec<PlanRippleEntry>,
    /// Explicit budget exhaustion metadata when [`PlanRippleStatus::Unknown`] due to a budget limit.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false)]
    pub budget_exceeded: Option<PlanRippleBudgetExceeded>,
    /// Explicit explanation when [`PlanRippleStatus::Unknown`] (projection error or budget exhaustion).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false)]
    pub reason: Option<String>,
}

impl From<RippleReport> for PlanRippleReport {
    fn from(report: RippleReport) -> Self {
        Self {
            status: report.status.into(),
            roots: report.roots,
            ripples: report.ripples.into_iter().map(Into::into).collect(),
            budget_exceeded: report.budget_exceeded.map(Into::into),
            reason: report.reason,
        }
    }
}

/// Compute the bounded transitive ripple report for `added` and `changed` artifacts between
/// `before` (active set) and `after` (candidate admitted set).
#[must_use]
pub(crate) fn compute(
    before: &[ArtifactEnvelope],
    after: &[ArtifactEnvelope],
    added: &[PlannedArtifact],
    changed: &[PlannedArtifact],
) -> PlanRippleReport {
    let changed_ids: Vec<String> = added
        .iter()
        .chain(changed)
        .map(|artifact| artifact.artifact_id.clone())
        .collect();
    norn_checker::ripple::compute_ripple_effects(before, after, &changed_ids).into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn converts_exact_and_unknown_checker_reports_cleanly() {
        let exact: PlanRippleReport = RippleReport {
            status: RippleStatus::Exact,
            roots: vec!["core.Holding".to_owned()],
            ripples: vec![RippleEntry {
                artifact_id: "jewelry.Metal".to_owned(),
                kind: "entity".to_owned(),
                triggered_by: vec!["core.Holding".to_owned()],
                via: vec!["core.Holding".to_owned(), "jewelry.Metal".to_owned()],
                impact_kind: ImpactKind::SpecializationAffected,
            }],
            budget_exceeded: None,
            reason: None,
        }
        .into();
        assert_eq!(exact.status, PlanRippleStatus::Exact);
        assert_eq!(exact.ripples.len(), 1);
        assert!(exact.budget_exceeded.is_none());
        assert!(exact.reason.is_none());

        let unknown: PlanRippleReport = RippleReport {
            status: RippleStatus::Unknown,
            roots: vec!["core.Holding".to_owned()],
            ripples: Vec::new(),
            budget_exceeded: Some(BudgetRefusal {
                resource: "steps".to_owned(),
                limit: 16,
                detail: "ripple graph steps budget (16) exceeded".to_owned(),
            }),
            reason: Some("ripple graph steps budget (16) exceeded".to_owned()),
        }
        .into();
        assert_eq!(unknown.status, PlanRippleStatus::Unknown);
        assert!(unknown.ripples.is_empty());
        assert_eq!(unknown.budget_exceeded.as_ref().map(|b| b.limit), Some(16));
        assert!(unknown.reason.is_some());
    }
}
