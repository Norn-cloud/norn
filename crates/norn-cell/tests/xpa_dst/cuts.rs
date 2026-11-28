//! Combined-view spec evaluation, bucket resolution, and deterministic cell-id
//! helpers shared across Layers 1–5 of the xP&A DST harness.

use std::collections::{BTreeMap, BTreeSet};

use norn_cell::combined_view::{
    CellCoverageSpec, CellCutSnapshot, CombinedViewReport, CoverageTransformSpec, SpecCellBinding,
    evaluate_cell_cut_from_spec, evaluate_combined_management_view_from_spec,
};
use norn_cell::compute::transform::RowAccess;
use norn_cell::compute::{Frontier, QueryContext, QueryResult};
use norn_cell::{CellError, ObjectRecord};
use norn_checker::metric::compute::QuestionPlan;
use norn_checker::transform::TransformPlan;
use norn_ir::{Decimal, RuntimeValue};
use norn_kernel::authz::CommitBinding;
use norn_kernel::cell_host::FactKey;

use super::env::{
    EVIDENCE_TYPES, EntityCell, FEED_MAPPINGS, FEED_MAX_AGE_MS, REQUIRED_FEEDS, RETAIL_QUESTIONS,
    RetailPack, TEST_HANDLE_KEY, Watermarks, retail_pack,
};

type SpecPlans = (
    BTreeMap<String, TransformPlan>,
    BTreeMap<String, QuestionPlan>,
);

/// Build a coverage spec covering all 4 retail feeds, evidence types,
/// cost-completeness transforms, and all 7 declared retail questions.
#[must_use]
pub fn dst_coverage_spec() -> CellCoverageSpec {
    CellCoverageSpec {
        required_feeds: REQUIRED_FEEDS.iter().map(|s| (*s).to_owned()).collect(),
        feed_max_age_ms: FEED_MAX_AGE_MS,
        evidence_types: EVIDENCE_TYPES.iter().map(|s| (*s).to_owned()).collect(),
        feed_mappings: FEED_MAPPINGS
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect(),
        questions: RETAIL_QUESTIONS.iter().map(|s| (*s).to_owned()).collect(),
        coverage_transforms: vec![
            CoverageTransformSpec {
                name: "supply.retail.entity_management_summary".to_owned(),
                transform_id: "supply.retail.entity_management_summary".to_owned(),
                required_true_column: "cost_complete".to_owned(),
            },
            CoverageTransformSpec {
                name: "supply.retail.store_lot_aging".to_owned(),
                transform_id: "supply.retail.store_lot_aging".to_owned(),
                required_true_column: "cost_complete".to_owned(),
            },
            CoverageTransformSpec {
                name: "supply.retail.store_active_positions".to_owned(),
                transform_id: "supply.retail.store_active_positions".to_owned(),
                required_true_column: "cost_complete".to_owned(),
            },
        ],
        reconciliation_gates: Vec::new(),
        expected_grain_gates: Vec::new(),
    }
}

/// Load admitted transform and question plans needed by `spec`.
///
/// # Errors
/// Returns an error if any plan fails admission.
pub fn load_spec_plans(pack: &RetailPack, spec: &CellCoverageSpec) -> Result<SpecPlans, String> {
    let mut tf_ids = BTreeSet::new();
    for g in &spec.coverage_transforms {
        tf_ids.insert(g.transform_id.as_str());
    }
    for g in &spec.reconciliation_gates {
        tf_ids.insert(g.expected_transform_id.as_str());
        tf_ids.insert(g.observed_transform_id.as_str());
    }
    for g in &spec.expected_grain_gates {
        tf_ids.insert(g.expected_transform_id.as_str());
    }
    let mut transforms = BTreeMap::new();
    for id in tf_ids {
        transforms.insert(id.to_owned(), pack.transform_plan(id)?);
    }
    let mut questions = BTreeMap::new();
    for q in &spec.questions {
        questions.insert(q.clone(), pack.retail_question(q)?);
    }
    Ok((transforms, questions))
}

/// Evaluate one cell cut from `spec` using the exact cell evaluator.
///
/// # Errors
/// Returns an error if the cut refuses or fails.
pub fn evaluate_spec_cut(
    book: &EntityCell,
    spec: &CellCoverageSpec,
) -> Result<CellCutSnapshot, String> {
    let (transforms, questions) = load_spec_plans(book.pack, spec)?;
    let allow_tf = |_: &TransformPlan| Ok(());
    let allow_all = |_: &ObjectRecord| Ok(RowAccess::Read);
    let frontier = || Ok((book.source_cursors.clone(), book.watermarks.clone()));
    let (anchor_key, authority) = book.read_authority();
    let binding = SpecCellBinding {
        store: book.cell()?.store(),
        anchor_key: &anchor_key,
        authority: &authority,
        context: book.query_context(),
        handle_key: &TEST_HANDLE_KEY,
        whole_world_values: false,
        min_disclosed_bucket: 2,
        read_frontier: &frontier,
        authorize_transform: &allow_tf,
        row_access: &allow_all,
        transforms: &transforms,
        questions: &questions,
    };
    evaluate_cell_cut_from_spec(spec, &binding, &mut |_, plan, ctx| {
        book.cell()
            .map_err(CellError::Engine)?
            .store()
            .evaluate_question(plan, ctx, |_| Ok(true))
    })
    .map_err(|e| format!("evaluate_cell_cut_from_spec: {e}"))
}

/// Evaluate a multi-cell read-only `CombinedViewReport` across `books` using a
/// caller-supplied question evaluator (exact Cell, `DuckDB`, or `DataFusion`).
///
/// # Errors
/// Returns an error if any cell cut refuses or fails.
pub fn evaluate_multi_cell_report(
    books: &[&EntityCell],
    spec: &CellCoverageSpec,
    mut eval_fn: impl FnMut(usize, &str, &QuestionPlan, QueryContext) -> Result<QueryResult, CellError>,
) -> Result<CombinedViewReport, String> {
    let pack = retail_pack()?;
    let (transforms, questions) = load_spec_plans(pack, spec)?;
    let allow_tf = |_: &TransformPlan| Ok(());
    let allow_all = |_: &ObjectRecord| Ok(RowAccess::Read);
    let authorities: Vec<(FactKey, CommitBinding)> =
        books.iter().map(|b| b.read_authority()).collect();
    let frontiers: Vec<Box<dyn Fn() -> Result<(Frontier, Watermarks), CellError> + '_>> = books
        .iter()
        .map(|b| {
            let f: Box<dyn Fn() -> Result<(Frontier, Watermarks), CellError> + '_> =
                Box::new(move || Ok((b.source_cursors.clone(), b.watermarks.clone())));
            f
        })
        .collect();
    let mut bindings = Vec::with_capacity(books.len());
    for (idx, book) in books.iter().enumerate() {
        let (anchor_key, authority) = authorities
            .get(idx)
            .ok_or_else(|| format!("missing authority at {idx}"))?;
        let frontier_fn = frontiers
            .get(idx)
            .ok_or_else(|| format!("missing frontier at {idx}"))?;
        bindings.push(SpecCellBinding {
            store: book.cell()?.store(),
            anchor_key,
            authority,
            context: book.query_context(),
            handle_key: &TEST_HANDLE_KEY,
            whole_world_values: false,
            min_disclosed_bucket: 2,
            read_frontier: frontier_fn.as_ref(),
            authorize_transform: &allow_tf,
            row_access: &allow_all,
            transforms: &transforms,
            questions: &questions,
        });
    }
    evaluate_combined_management_view_from_spec(spec, &bindings, |idx, name, plan, ctx| {
        eval_fn(idx, name, plan, ctx)
    })
    .map_err(|e| format!("evaluate_combined_management_view_from_spec: {e}"))
}

/// Format a [`RuntimeValue`] as its canonical scalar string.
#[must_use]
pub fn scalar(value: &RuntimeValue) -> String {
    match value {
        RuntimeValue::String(s) => s.clone(),
        RuntimeValue::Number(d) => d.canonical(),
        RuntimeValue::Bool(b) => b.to_string(),
        RuntimeValue::Null => "null".to_owned(),
        RuntimeValue::Array(items) => format!("{items:?}"),
        RuntimeValue::Object(obj) => format!("{obj:?}"),
    }
}

/// Extract the exact tuple of grouping bucket parts from `QueryRow::bucket`.
#[must_use]
pub fn bucket_parts(bucket: &RuntimeValue) -> Vec<String> {
    if let RuntimeValue::Array(items) = bucket {
        items.iter().map(scalar).collect()
    } else {
        vec![scalar(bucket)]
    }
}

/// Find the exact [`Decimal`] value for a bucket matching `expected`.
#[must_use]
pub fn bucket_decimal_exact(result: &QueryResult, expected: &[&str]) -> Option<Decimal> {
    result.rows.values().find_map(|row| {
        let parts = bucket_parts(&row.bucket);
        let matches = parts.len() == expected.len()
            && parts
                .iter()
                .zip(expected.iter())
                .all(|(actual, want)| actual == *want);
        matches.then_some(row.value).flatten()
    })
}

/// Fail-closed lookup on a `[currency, store_id, cost_status, week]` question:
/// returns `None` if ANY `missing_cost` slice is present for `(currency, store_id, week)`.
#[must_use]
pub fn resolve_costed_bucket(
    result: &QueryResult,
    currency: &str,
    store_id: &str,
    week: &str,
) -> Option<Decimal> {
    let has_missing = result.rows.values().any(|row| {
        let parts = bucket_parts(&row.bucket);
        parts.first().map(String::as_str) == Some(currency)
            && parts.get(1).map(String::as_str) == Some(store_id)
            && parts.get(2).map(String::as_str) == Some("missing_cost")
            && parts.get(3).map(String::as_str) == Some(week)
    });
    if has_missing {
        return None;
    }
    bucket_decimal_exact(result, &[currency, store_id, "costed", week])
}

/// Deterministic hex cell id for synthetic legal entity index `idx`.
#[must_use]
pub fn cell_id_for_entity(seed: u64, entity_idx: usize) -> String {
    let tag = (seed & 0xFFFF_FFFF) ^ u64::try_from(entity_idx).unwrap_or(0).wrapping_mul(0x101);
    let idx_u32 = u32::try_from(entity_idx.saturating_add(1)).unwrap_or(1);
    format!("0000000000000000{tag:08x}{idx_u32:08x}")
}
