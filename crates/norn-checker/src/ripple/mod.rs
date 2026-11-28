#![forbid(unsafe_code)]
//! Exact deterministic transitive ripple-effect computation over admitted or candidate artifact sets.
//!
//! Projects the cross-artifact dependency graph from [`crate::definition_catalog::project`] and
//! typed artifact envelopes (`entity` types/interfaces/extensions/links, `relation`, `action`,
//! `rule`, `law`, `policy`, `derived`, `mapping`, `defined_concept`, `model`, `widget`,
//! `customization`, `labels`, `business_pack`, `connector`, `setting`, `source_schema`), then
//! runs metered reverse [`norn_graph::reachability`] (`Direction::Upstream`) under a caller-supplied
//! [`Budget`] to compute the exact transitive ripple set with shortest concrete explanation chains
//! (`via`) and impact classifications.
//!
//! Fail-closed completeness: exhausting `nodes`, `edges`, or `steps` returns
//! [`RippleStatus::Unknown`] with explicit [`BudgetRefusal`] metadata and never silently truncates.

mod project;

use std::collections::{BTreeMap, BTreeSet, VecDeque};

use norn_graph::{Budget, Direction, Error as GraphError, Graph, Limit, reachability};
use norn_ir::{ArtifactEnvelope, ArtifactKind};
use serde_json::{Map, Value, json};

use self::project::{Projection, Universe};

/// Format tag of [`RippleReport::to_json_value`].
pub const RIPPLE_FORMAT: &str = "norn-checker/ripple@1";

/// Default graph resource ceiling for ripple-effect computation.
pub const DEFAULT_RIPPLE_BUDGET: Budget = Budget {
    max_nodes: 16_384,
    max_edges: 131_072,
    max_steps: 1_048_576,
};

/// Completeness status of a [`RippleReport`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum RippleStatus {
    /// Exact transitive closure completed within the graph budget.
    Exact,
    /// Graph budget was exhausted; no partial ripple set is claimed complete.
    Unknown,
}

impl RippleStatus {
    /// Stable wire string for this status.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Exact => "exact",
            Self::Unknown => "unknown",
        }
    }
}

/// Classification of how a root change ripples into an impacted artifact.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum ImpactKind {
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

impl ImpactKind {
    /// Stable snake-case wire string for this impact kind.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::DirectChange => "direct_change",
            Self::SpecializationAffected => "specialization_affected",
            Self::WriterAffected => "writer_affected",
            Self::LawAffected => "law_affected",
            Self::LifecycleAffected => "lifecycle_affected",
            Self::PolicyAffected => "policy_affected",
            Self::ProjectionOrUiAffected => "projection_or_ui_affected",
            Self::RemovedDependencyBroken => "removed_dependency_broken",
        }
    }
}

/// One artifact impacted directly or transitively by a planned definition change.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RippleEntry {
    /// Impacted artifact envelope id.
    pub artifact_id: String,
    /// Snake-case artifact kind (`entity`, `action`, `law`, `widget`, ...).
    pub kind: String,
    /// Sorted, deduplicated root changed or removed artifact ids causing this ripple.
    pub triggered_by: Vec<String>,
    /// Shortest concrete dependency chain from a triggering root to `artifact_id`.
    pub via: Vec<String>,
    /// Classification of the impact on `artifact_id`.
    pub impact_kind: ImpactKind,
}

impl RippleEntry {
    /// Canonical `camelCase` JSON representation for CLI and WASM projections.
    #[must_use]
    pub fn to_json_value(&self) -> Value {
        json!({
            "artifactId": self.artifact_id,
            "kind": self.kind,
            "triggeredBy": self.triggered_by,
            "via": self.via,
            "impactKind": self.impact_kind.as_str(),
        })
    }
}

/// Explicit budget refusal metadata when [`RippleReport::status`] is [`RippleStatus::Unknown`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BudgetRefusal {
    /// Exhausted resource (`nodes`, `edges`, or `steps`).
    pub resource: String,
    /// Caller-supplied ceiling that was exceeded.
    pub limit: usize,
    /// Human-readable explanation.
    pub detail: String,
}

impl BudgetRefusal {
    pub(crate) fn from_limit(limit_kind: Limit, limit: usize) -> Self {
        let resource = match limit_kind {
            Limit::Nodes => "nodes",
            Limit::Edges => "edges",
            Limit::Steps => "steps",
        };
        Self {
            resource: resource.to_owned(),
            limit,
            detail: format!("ripple graph {resource} budget ({limit}) exceeded"),
        }
    }

    /// Canonical `camelCase` JSON representation.
    #[must_use]
    pub fn to_json_value(&self) -> Value {
        json!({
            "resource": self.resource,
            "limit": self.limit,
            "detail": self.detail,
        })
    }
}

/// Deterministic transitive ripple-effect report for a definition change.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RippleReport {
    /// [`RippleStatus::Exact`] when traversal finished within budget; [`RippleStatus::Unknown`] on refusal.
    pub status: RippleStatus,
    /// Sorted, deduplicated root changed or removed artifact ids evaluated.
    pub roots: Vec<String>,
    /// Impacted artifacts in deterministic wave order (empty when [`RippleStatus::Unknown`]).
    pub ripples: Vec<RippleEntry>,
    /// Explicit budget exhaustion metadata when [`RippleStatus::Unknown`].
    pub budget_exceeded: Option<BudgetRefusal>,
}

impl RippleReport {
    fn unknown(roots: Vec<String>, refusal: BudgetRefusal) -> Self {
        Self {
            status: RippleStatus::Unknown,
            roots,
            ripples: Vec::new(),
            budget_exceeded: Some(refusal),
        }
    }

    /// Canonical `camelCase` JSON representation for CLI and WASM projections.
    #[must_use]
    pub fn to_json_value(&self) -> Value {
        let mut map = Map::new();
        map.insert("format".to_owned(), json!(RIPPLE_FORMAT));
        map.insert("status".to_owned(), json!(self.status.as_str()));
        map.insert("roots".to_owned(), json!(self.roots));
        map.insert(
            "ripples".to_owned(),
            Value::Array(
                self.ripples
                    .iter()
                    .map(RippleEntry::to_json_value)
                    .collect(),
            ),
        );
        if let Some(refusal) = &self.budget_exceeded {
            map.insert("budgetExceeded".to_owned(), refusal.to_json_value());
        }
        Value::Object(map)
    }
}

/// Compute exact transitive ripple effects under [`DEFAULT_RIPPLE_BUDGET`].
#[must_use]
pub fn compute_ripple_effects(
    before_artifacts: &[ArtifactEnvelope],
    after_artifacts: &[ArtifactEnvelope],
    changed_artifact_ids: &[String],
) -> RippleReport {
    compute_ripple_effects_with_budget(
        before_artifacts,
        after_artifacts,
        changed_artifact_ids,
        DEFAULT_RIPPLE_BUDGET,
    )
}

/// Compute exact transitive ripple effects under an explicit metered [`Budget`].
///
/// Exhausting `budget` fails closed with [`RippleStatus::Unknown`] and an empty `ripples`
/// vector so callers never mistake a truncated traversal for a complete impact set.
#[must_use]
pub fn compute_ripple_effects_with_budget(
    before_artifacts: &[ArtifactEnvelope],
    after_artifacts: &[ArtifactEnvelope],
    changed_artifact_ids: &[String],
    budget: Budget,
) -> RippleReport {
    let universe = Universe::build(before_artifacts, after_artifacts, changed_artifact_ids);
    let root_list: Vec<String> = universe.roots.iter().cloned().collect();
    if universe.all_ids.len() > budget.max_nodes {
        return RippleReport::unknown(
            root_list,
            BudgetRefusal::from_limit(Limit::Nodes, budget.max_nodes),
        );
    }
    if universe.roots.is_empty() {
        return RippleReport {
            status: RippleStatus::Exact,
            roots: Vec::new(),
            ripples: Vec::new(),
            budget_exceeded: None,
        };
    }

    let mut projection = Projection::new(&universe, budget);
    if let Err(refusal) = projection.collect(before_artifacts, after_artifacts) {
        return RippleReport::unknown(root_list, refusal);
    }

    let graph = Graph {
        nodes: universe.all_ids.iter().cloned().collect(),
        edges: projection.edges.iter().cloned().collect(),
    };
    let reached = match reachability(&graph, &root_list, Direction::Upstream, budget) {
        Ok(reached) => reached,
        Err(GraphError::BudgetExceeded(exceeded)) => {
            return RippleReport::unknown(
                root_list,
                BudgetRefusal::from_limit(exceeded.resource, exceeded.limit),
            );
        }
        Err(
            GraphError::DuplicateNode(_)
            | GraphError::UnknownNode(_)
            | GraphError::MissingValue(_)
            | GraphError::NegativeValue(_)
            | GraphError::Cycle(_)
            | GraphError::NotTree
            | GraphError::Arithmetic(_)
            | GraphError::Invariant,
        ) => {
            return RippleReport::unknown(
                root_list,
                BudgetRefusal {
                    resource: "steps".to_owned(),
                    limit: budget.max_steps,
                    detail: "ripple graph reachability could not complete".to_owned(),
                },
            );
        }
    };

    let (triggered_by, best_via) = match trace_paths(&universe.roots, &projection.edges, budget) {
        Ok(paths) => paths,
        Err(refusal) => return RippleReport::unknown(root_list, refusal),
    };

    let mut ripples = Vec::with_capacity(reached.len());
    for artifact_id in reached {
        let kind = universe
            .artifact_kinds
            .get(&artifact_id)
            .map_or("unknown", kind_name)
            .to_owned();
        let triggers = triggered_by
            .get(&artifact_id)
            .map_or_else(Vec::new, |set| set.iter().cloned().collect());
        let via = best_via
            .get(&artifact_id)
            .map_or_else(|| vec![artifact_id.clone()], Clone::clone);
        let impact_kind = classify_impact(&artifact_id, &via, &universe, &projection);
        ripples.push(RippleEntry {
            artifact_id,
            kind,
            triggered_by: triggers,
            via,
            impact_kind,
        });
    }
    ripples.sort_by(|left, right| {
        left.via
            .len()
            .cmp(&right.via.len())
            .then_with(|| left.artifact_id.cmp(&right.artifact_id))
    });

    RippleReport {
        status: RippleStatus::Exact,
        roots: root_list,
        ripples,
        budget_exceeded: None,
    }
}

type PathTraceResult = (
    BTreeMap<String, BTreeSet<String>>,
    BTreeMap<String, Vec<String>>,
);

fn trace_paths(
    roots: &BTreeSet<String>,
    edges: &BTreeSet<(String, String)>,
    budget: Budget,
) -> Result<PathTraceResult, BudgetRefusal> {
    let mut dependents: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    for (dependent, dependency) in edges {
        dependents
            .entry(dependency.as_str())
            .or_default()
            .push(dependent.as_str());
    }
    let mut triggered_by: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    let mut best_via: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut steps = 0usize;

    for root in roots {
        let mut parent: BTreeMap<&str, Option<&str>> = BTreeMap::from([(root.as_str(), None)]);
        let mut queue = VecDeque::from([root.as_str()]);
        while let Some(current) = queue.pop_front() {
            steps = steps.saturating_add(1);
            if steps > budget.max_steps {
                return Err(BudgetRefusal::from_limit(Limit::Steps, budget.max_steps));
            }
            triggered_by
                .entry(current.to_owned())
                .or_default()
                .insert(root.clone());
            let path = reconstruct_path(current, &parent);
            update_best_path(&mut best_via, current, path);
            if let Some(next_nodes) = dependents.get(current) {
                for &next in next_nodes {
                    steps = steps.saturating_add(1);
                    if steps > budget.max_steps {
                        return Err(BudgetRefusal::from_limit(Limit::Steps, budget.max_steps));
                    }
                    if !parent.contains_key(next) {
                        parent.insert(next, Some(current));
                        queue.push_back(next);
                    }
                }
            }
        }
    }
    Ok((triggered_by, best_via))
}

fn reconstruct_path(target: &str, parent: &BTreeMap<&str, Option<&str>>) -> Vec<String> {
    let mut path = vec![target.to_owned()];
    let mut cursor = target;
    while let Some(Some(prev)) = parent.get(cursor) {
        path.push((*prev).to_owned());
        cursor = prev;
    }
    path.reverse();
    path
}

fn update_best_path(
    best_via: &mut BTreeMap<String, Vec<String>>,
    target: &str,
    candidate: Vec<String>,
) {
    match best_via.get_mut(target) {
        None => {
            best_via.insert(target.to_owned(), candidate);
        }
        Some(existing) => {
            if candidate.len() < existing.len()
                || (candidate.len() == existing.len() && candidate < *existing)
            {
                *existing = candidate;
            }
        }
    }
}

fn classify_impact(
    artifact_id: &str,
    via: &[String],
    universe: &Universe,
    projection: &Projection<'_>,
) -> ImpactKind {
    if universe.roots.contains(artifact_id) {
        return ImpactKind::DirectChange;
    }
    if universe.removed_ids.iter().any(|removed| {
        projection
            .edges
            .contains(&(artifact_id.to_owned(), removed.clone()))
    }) {
        return ImpactKind::RemovedDependencyBroken;
    }
    match universe.artifact_kinds.get(artifact_id) {
        Some(ArtifactKind::Law) => ImpactKind::LawAffected,
        Some(ArtifactKind::Policy) => ImpactKind::PolicyAffected,
        Some(
            ArtifactKind::Action
            | ArtifactKind::Rule
            | ArtifactKind::Connector
            | ArtifactKind::Mapping,
        ) => ImpactKind::WriterAffected,
        Some(
            ArtifactKind::Widget
            | ArtifactKind::Derived
            | ArtifactKind::Model
            | ArtifactKind::Labels
            | ArtifactKind::Customization
            | ArtifactKind::Setting
            | ArtifactKind::BusinessPack
            | ArtifactKind::SourceSchema,
        )
        | None => ImpactKind::ProjectionOrUiAffected,
        Some(ArtifactKind::DefinedConcept) => ImpactKind::SpecializationAffected,
        Some(ArtifactKind::Entity) => {
            let prev = via.len().checked_sub(2).and_then(|idx| via.get(idx));
            if prev.is_some_and(|parent| {
                projection
                    .specialization_edges
                    .contains(&(artifact_id.to_owned(), parent.clone()))
            }) {
                ImpactKind::SpecializationAffected
            } else if universe.has_lifecycle.contains(artifact_id) {
                ImpactKind::LifecycleAffected
            } else {
                ImpactKind::SpecializationAffected
            }
        }
        Some(ArtifactKind::Relation) => ImpactKind::LifecycleAffected,
    }
}

const fn kind_name(kind: &ArtifactKind) -> &'static str {
    match kind {
        ArtifactKind::Entity => "entity",
        ArtifactKind::Relation => "relation",
        ArtifactKind::Action => "action",
        ArtifactKind::Rule => "rule",
        ArtifactKind::Connector => "connector",
        ArtifactKind::Widget => "widget",
        ArtifactKind::Customization => "customization",
        ArtifactKind::Setting => "setting",
        ArtifactKind::BusinessPack => "business_pack",
        ArtifactKind::Policy => "policy",
        ArtifactKind::Derived => "derived",
        ArtifactKind::SourceSchema => "source_schema",
        ArtifactKind::Mapping => "mapping",
        ArtifactKind::Law => "law",
        ArtifactKind::DefinedConcept => "defined_concept",
        ArtifactKind::Labels => "labels",
        ArtifactKind::Model => "model",
    }
}
