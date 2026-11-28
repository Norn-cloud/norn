//! Pack-authored declarative coverage manifests and spec-driven evaluation
//! entrypoints for [`super::evaluate_cell_cut`].

use std::collections::{BTreeMap, BTreeSet};

use norn_checker::metric::compute::QuestionPlan;
use norn_checker::transform::TransformPlan;
use norn_ir::RuntimeValue;
use norn_kernel::FactKey;
use norn_kernel::authz::CommitBinding;
use serde::{Deserialize, Serialize};

use crate::compute::{Frontier, QueryContext, QueryResult, Watermark};
use crate::engine::SqlEngine;
use crate::error::CellError;
use crate::store::CellStore;

use super::{
    CellCutSnapshot, CellViewInput, CombinedViewKind, CombinedViewReport, CoverageTransformGate,
    ExpectedGrainGate, FrontierReader, ReconciliationComparison, ReconciliationGate,
    RowAccessDecider, TransformAuthorizer, evaluate_cell_cut,
};

/// Serializable declarative spec for one [`CoverageTransformGate`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CoverageTransformSpec {
    /// Gate identifier reported in [`CellCutSnapshot::failed_coverage_gates`].
    pub name: String,
    /// Admitted transform artifact identifier.
    pub transform_id: String,
    /// Boolean output column name that must evaluate to `true` on every row.
    pub required_true_column: String,
}

/// Serializable declarative spec for one [`ReconciliationGate`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ReconciliationSpec {
    /// Gate identifier reported in [`CellCutSnapshot::failed_coverage_gates`].
    pub name: String,
    /// Expected population transform artifact identifier.
    pub expected_transform_id: String,
    /// Observed transform artifact identifier.
    pub observed_transform_id: String,
    /// Shared grain column names.
    pub key_columns: Vec<String>,
    /// Numeric column name in `expected_transform_id`.
    pub expected_value_column: String,
    /// Numeric column name in `observed_transform_id`.
    pub observed_value_column: String,
    /// Numeric comparison rule.
    pub comparison: ReconciliationComparison,
    /// Optional RFC 3339 timestamp column in `observed_transform_id`.
    pub observed_timestamp_column: Option<String>,
    /// Optional maximum timestamp age in milliseconds.
    pub max_age_ms: Option<u64>,
    /// Optional exact string column filters on `expected_transform_id`.
    #[serde(default)]
    pub expected_filters: Vec<(String, String)>,
    /// Optional exact string column filters on `observed_transform_id`.
    #[serde(default)]
    pub observed_filters: Vec<(String, String)>,
    /// When `true`, every observed row must match an expected row (absent expected never falls back to `0`).
    #[serde(default)]
    pub require_expected_match: bool,
}

/// Serializable declarative spec for one [`ExpectedGrainGate`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ExpectedGrainSpec {
    /// Gate identifier reported in [`CellCutSnapshot::failed_coverage_gates`].
    pub name: String,
    /// Expected population transform artifact identifier.
    pub expected_transform_id: String,
    /// Grain column names in `expected_transform_id`.
    pub expected_columns: Vec<String>,
    /// Target question name that must cover every expected grain tuple.
    pub question_name: String,
    /// Zero-based indices into `QueryRow::bucket`.
    pub bucket_indices: Vec<usize>,
    /// Whether covered buckets must carry a non-null, unsuppressed value.
    pub require_non_null_value: bool,
    /// Optional exact string bucket-position filters (for example pinning a target period week).
    #[serde(default)]
    pub required_bucket_filters: Vec<(usize, String)>,
}

/// Pack-authored declarative coverage manifest binding feeds, evidence types,
/// questions, and gates for [`evaluate_cell_cut_from_spec`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CellCoverageSpec {
    /// Required feed names whose durable cursors and live watermarks must match and be fresh.
    pub required_feeds: Vec<String>,
    /// Maximum allowed feed watermark age (milliseconds) relative to `QueryContext::now_ms`.
    pub feed_max_age_ms: u64,
    /// Evidence object types scanned for open data-quality findings.
    pub evidence_types: Vec<String>,
    /// Mapping artifact ID -> target evidence object type for scoped inbox quarantine diagnostics.
    #[serde(default)]
    pub feed_mappings: BTreeMap<String, String>,
    /// Named questions evaluated at this cut.
    pub questions: Vec<String>,
    /// Boolean coverage transform gates.
    pub coverage_transforms: Vec<CoverageTransformSpec>,
    /// Numeric reconciliation and freshness gates.
    pub reconciliation_gates: Vec<ReconciliationSpec>,
    /// Expected-grain question coverage gates.
    pub expected_grain_gates: Vec<ExpectedGrainSpec>,
}

impl CellCoverageSpec {
    /// Decode a pack-authored combined-view coverage manifest from JSON.
    ///
    /// # Errors
    /// Returns `CellError::Encoding` if the manifest JSON is malformed or contains unknown fields.
    pub fn from_json_str(json: &str) -> Result<Self, CellError> {
        serde_json::from_str(json).map_err(|e| CellError::Encoding(e.to_string()))
    }
}

/// Bindings supplied by a host executing a pack-authored [`CellCoverageSpec`].
pub struct SpecCellBinding<'a, E: SqlEngine> {
    /// Cell store to inspect.
    pub store: &'a CellStore<E>,
    /// Anchor object key covered by `authority`.
    pub anchor_key: &'a FactKey,
    /// Authorized PDP binding pinning this cell's read cut.
    pub authority: &'a CommitBinding,
    /// Query context pinning world, scope, mapping pin, clock, cursors, and watermarks.
    pub context: QueryContext,
    /// Cell handle key for `DATA-VALUES-1` keyed-handle disclosure.
    pub handle_key: &'a [u8],
    /// Whether the caller holds whole-world unmarked Values disclosure.
    pub whole_world_values: bool,
    /// Minimum bucket size disclosed when any contributing member lacks Values (`DATA-VALUES-1`).
    pub min_disclosed_bucket: usize,
    /// Required live feed-frontier reader.
    pub read_frontier: FrontierReader<'a>,
    /// Pre-evaluation transform authorization callback (`VIEW-READ-1`).
    pub authorize_transform: TransformAuthorizer<'a>,
    /// Per-row visibility callback (`HOST-READ-2` / `DATA-VALUES-1`).
    pub row_access: RowAccessDecider<'a>,
    /// Admitted transforms keyed by artifact id.
    pub transforms: &'a BTreeMap<String, TransformPlan>,
    /// Admitted questions keyed by question name.
    pub questions: &'a BTreeMap<String, QuestionPlan>,
}

fn build_spec_reconciliation_gates<'a, E: SqlEngine>(
    spec: &'a CellCoverageSpec,
    binding: &SpecCellBinding<'a, E>,
    key_refs: &'a [Vec<&'a str>],
    expected_filters: &'a [Vec<(&'a str, RuntimeValue)>],
    observed_filters: &'a [Vec<(&'a str, RuntimeValue)>],
) -> Result<Vec<ReconciliationGate<'a>>, CellError> {
    let stale = || CellError::ComputeRefused("core.compute.stale_input");
    spec.reconciliation_gates
        .iter()
        .enumerate()
        .map(|(idx, g)| {
            Ok(ReconciliationGate {
                name: g.name.as_str(),
                expected_plan: binding
                    .transforms
                    .get(&g.expected_transform_id)
                    .ok_or_else(stale)?,
                observed_plan: binding
                    .transforms
                    .get(&g.observed_transform_id)
                    .ok_or_else(stale)?,
                key_columns: key_refs.get(idx).ok_or_else(stale)?,
                expected_value_column: g.expected_value_column.as_str(),
                observed_value_column: g.observed_value_column.as_str(),
                comparison: g.comparison,
                observed_timestamp_column: g.observed_timestamp_column.as_deref(),
                max_age_ms: g.max_age_ms,
                expected_filters: expected_filters.get(idx).ok_or_else(stale)?,
                observed_filters: observed_filters.get(idx).ok_or_else(stale)?,
                require_expected_match: g.require_expected_match,
                authorize_expected: binding.authorize_transform,
                authorize_observed: binding.authorize_transform,
                row_access: binding.row_access,
            })
        })
        .collect()
}

fn build_spec_grain_gates<'a, E: SqlEngine>(
    spec: &'a CellCoverageSpec,
    binding: &SpecCellBinding<'a, E>,
    col_refs: &'a [Vec<&'a str>],
    filters: &'a [Vec<(usize, RuntimeValue)>],
) -> Result<Vec<ExpectedGrainGate<'a>>, CellError> {
    let stale = || CellError::ComputeRefused("core.compute.stale_input");
    spec.expected_grain_gates
        .iter()
        .enumerate()
        .map(|(idx, g)| {
            Ok(ExpectedGrainGate {
                name: g.name.as_str(),
                expected_plan: binding
                    .transforms
                    .get(&g.expected_transform_id)
                    .ok_or_else(stale)?,
                expected_columns: col_refs.get(idx).ok_or_else(stale)?,
                question_name: g.question_name.as_str(),
                bucket_indices: &g.bucket_indices,
                require_non_null_value: g.require_non_null_value,
                required_bucket_filters: filters.get(idx).ok_or_else(stale)?,
                authorize_transform: binding.authorize_transform,
                row_access: binding.row_access,
            })
        })
        .collect()
}

fn string_filters(raw: &[(String, String)]) -> Vec<(&str, RuntimeValue)> {
    raw.iter()
        .map(|(col, val)| (col.as_str(), RuntimeValue::String(val.clone())))
        .collect()
}

fn build_spec_coverage_transforms<'a, E: SqlEngine>(
    spec: &'a CellCoverageSpec,
    binding: &SpecCellBinding<'a, E>,
) -> Result<Vec<CoverageTransformGate<'a>>, CellError> {
    let stale = || CellError::ComputeRefused("core.compute.stale_input");
    spec.coverage_transforms
        .iter()
        .map(|g| {
            Ok(CoverageTransformGate {
                name: g.name.as_str(),
                plan: binding.transforms.get(&g.transform_id).ok_or_else(stale)?,
                required_true_column: g.required_true_column.as_str(),
                authorize_transform: binding.authorize_transform,
                row_access: binding.row_access,
            })
        })
        .collect()
}

fn build_spec_questions<'a, E: SqlEngine>(
    spec: &'a CellCoverageSpec,
    binding: &SpecCellBinding<'a, E>,
) -> Result<Vec<(&'a str, &'a QuestionPlan)>, CellError> {
    let stale = || CellError::ComputeRefused("core.compute.stale_input");
    spec.questions
        .iter()
        .map(|name| {
            Ok((
                name.as_str(),
                binding.questions.get(name).ok_or_else(stale)?,
            ))
        })
        .collect()
}

fn retain_required_feeds(
    required: &BTreeSet<&str>,
    cursors: &mut Frontier,
    watermarks: &mut BTreeMap<String, BTreeMap<String, Watermark>>,
) {
    cursors.retain(|feed, _| required.contains(feed.as_str()));
    watermarks.retain(|feed, _| required.contains(feed.as_str()));
}

/// Evaluate one cell's pinned cut directly from a pack-authored
/// [`CellCoverageSpec`], removing any need for hard-coded host/test gate wiring.
///
/// Projects `binding.context` and `binding.read_frontier` strictly onto
/// `spec.required_feeds` so unrelated cell feeds do not contaminate question
/// freshness for this coverage cut.
///
/// # Errors
/// Returns `CellError::ComputeRefused("core.compute.stale_input")` if any
/// referenced transform or question is missing from `binding`, or propagates
/// any error from [`evaluate_cell_cut`].
pub fn evaluate_cell_cut_from_spec<E: SqlEngine>(
    spec: &CellCoverageSpec,
    binding: &SpecCellBinding<'_, E>,
    evaluate: &mut impl FnMut(&str, &QuestionPlan, QueryContext) -> Result<QueryResult, CellError>,
) -> Result<CellCutSnapshot, CellError> {
    let required_feeds: Vec<&str> = spec.required_feeds.iter().map(String::as_str).collect();
    let required_set: BTreeSet<&str> = required_feeds.iter().copied().collect();
    let evidence_types: Vec<&str> = spec.evidence_types.iter().map(String::as_str).collect();
    let feed_mappings: Vec<(&str, &str)> = spec
        .feed_mappings
        .iter()
        .map(|(k, v)| (k.as_str(), v.as_str()))
        .collect();
    let questions = build_spec_questions(spec, binding)?;
    let coverage_transforms = build_spec_coverage_transforms(spec, binding)?;
    let recon_key_refs: Vec<Vec<&str>> = spec
        .reconciliation_gates
        .iter()
        .map(|g| g.key_columns.iter().map(String::as_str).collect())
        .collect();
    let recon_expected_filters: Vec<Vec<(&str, RuntimeValue)>> = spec
        .reconciliation_gates
        .iter()
        .map(|g| string_filters(&g.expected_filters))
        .collect();
    let recon_observed_filters: Vec<Vec<(&str, RuntimeValue)>> = spec
        .reconciliation_gates
        .iter()
        .map(|g| string_filters(&g.observed_filters))
        .collect();
    let reconciliation_gates = build_spec_reconciliation_gates(
        spec,
        binding,
        &recon_key_refs,
        &recon_expected_filters,
        &recon_observed_filters,
    )?;
    let grain_col_refs: Vec<Vec<&str>> = spec
        .expected_grain_gates
        .iter()
        .map(|g| g.expected_columns.iter().map(String::as_str).collect())
        .collect();
    let grain_filters: Vec<Vec<(usize, RuntimeValue)>> = spec
        .expected_grain_gates
        .iter()
        .map(|g| {
            g.required_bucket_filters
                .iter()
                .map(|(pos, val)| (*pos, RuntimeValue::String(val.clone())))
                .collect()
        })
        .collect();
    let expected_grain_gates =
        build_spec_grain_gates(spec, binding, &grain_col_refs, &grain_filters)?;
    let mut scoped_context = binding.context.clone();
    retain_required_feeds(
        &required_set,
        &mut scoped_context.source_cursors,
        &mut scoped_context.watermarks,
    );
    let scoped_frontier = || {
        let (mut cursors, mut watermarks) = (binding.read_frontier)()?;
        retain_required_feeds(&required_set, &mut cursors, &mut watermarks);
        Ok((cursors, watermarks))
    };
    let input = CellViewInput {
        store: binding.store,
        anchor_key: binding.anchor_key,
        authority: binding.authority,
        context: scoped_context,
        handle_key: binding.handle_key,
        whole_world_values: binding.whole_world_values,
        member_access: binding.row_access,
        min_disclosed_bucket: binding.min_disclosed_bucket,
        required_feeds: &required_feeds,
        read_frontier: &scoped_frontier,
        min_fresh_ms: binding.context.now_ms.saturating_sub(spec.feed_max_age_ms),
        evidence_types: &evidence_types,
        feed_mappings: &feed_mappings,
        evidence_access: binding.row_access,
        coverage_transforms: &coverage_transforms,
        reconciliation_gates: &reconciliation_gates,
        expected_grain_gates: &expected_grain_gates,
        questions: &questions,
    };
    evaluate_cell_cut(&input, evaluate)
}

/// Evaluate a read-only combined management view across independent cells from
/// a shared pack-authored [`CellCoverageSpec`].
///
/// # Errors
/// Returns `CellError::ComputeRefused("core.compute.duplicate_cell")` if two
/// bindings resolve to the same [`norn_ir::cell_id::CellId`], or propagates any error from
/// [`evaluate_cell_cut_from_spec`].
pub fn evaluate_combined_management_view_from_spec<E: SqlEngine>(
    spec: &CellCoverageSpec,
    bindings: &[SpecCellBinding<'_, E>],
    mut evaluate: impl FnMut(usize, &str, &QuestionPlan, QueryContext) -> Result<QueryResult, CellError>,
) -> Result<CombinedViewReport, CellError> {
    let mut seen_cells = BTreeSet::new();
    let mut cells = Vec::with_capacity(bindings.len());
    for (idx, binding) in bindings.iter().enumerate() {
        let snap = evaluate_cell_cut_from_spec(spec, binding, &mut |name, plan, ctx| {
            evaluate(idx, name, plan, ctx)
        })?;
        if !seen_cells.insert(snap.cell_id.clone()) {
            return Err(CellError::ComputeRefused("core.compute.duplicate_cell"));
        }
        cells.push(snap);
    }
    Ok(CombinedViewReport {
        view_kind: CombinedViewKind::CombinedManagementView,
        is_consolidated_financial_statement: false,
        read_only: true,
        cells,
    })
}
