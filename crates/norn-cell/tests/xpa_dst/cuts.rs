//! Combined-view spec evaluation, bucket resolution, and deterministic cell-id
//! helpers shared across Layers 1–5 of the xP&A DST harness.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;

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
    RetailPack, TEST_HANDLE_KEY, Watermarks, repo_root, retail_pack,
};
use super::prng::{decimal_from_scaled, decimal_to_scaled, scaled};

type SpecPlans = (
    BTreeMap<String, TransformPlan>,
    BTreeMap<String, QuestionPlan>,
);

type FrontierFn<'a> = Box<dyn Fn() -> Result<(Frontier, Watermarks), CellError> + 'a>;

/// Load the pack-authored declarative coverage manifest
/// `registry/packs/supply-lab/questions/retail.coverage.json` including all
/// reconciliation and expected-grain gates.
///
/// # Errors
/// Returns an error if the manifest file cannot be read or decoded.
pub fn retail_coverage_spec() -> Result<CellCoverageSpec, String> {
    let text = fs::read_to_string(
        repo_root().join("registry/packs/supply-lab/questions/retail.coverage.json"),
    )
    .map_err(|e| format!("retail.coverage.json: {e}"))?;
    CellCoverageSpec::from_json_str(&text).map_err(|e| format!("decode retail.coverage.json: {e}"))
}

/// Build a general multi-week coverage spec covering all 4 retail feeds,
/// evidence types, cost-completeness transforms, and all 7 declared retail
/// questions (without W9-specific rollup reconciliation gates, for 16–52 week
/// multi-cell simulator horizons).
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
/// caller-supplied question evaluator (exact Cell, `DuckDB`, or `DataFusion`),
/// forwarding the spec-projected `plan` and `context` faithfully.
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
    let frontiers: Vec<FrontierFn<'_>> = books
        .iter()
        .map(|b| {
            let f: FrontierFn<'_> =
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

/// Independent exact arithmetic oracle tracked alongside each simulated cell
/// across the entire simulated horizon (`0..config.weeks`).
#[derive(Debug, Default, Clone)]
pub struct EntityOracle {
    /// `(currency, store_id, week_monday) -> exact scaled net_revenue`
    pub weekly_revenue: BTreeMap<(String, String, String), i128>,
    /// `(currency, store_id, week_monday) -> exact scaled gross_margin`
    pub weekly_costed_margin: BTreeMap<(String, String, String), i128>,
    /// `(store_id, week_monday)` keys containing any uncosted sale.
    pub uncosted_weeks: BTreeSet<(String, String)>,
    /// `(store_id, sku_id) -> (received_qty, depleted_qty)`.
    pub lot_conservation: BTreeMap<(String, String), (i64, i64)>,
    pub total_sales: usize,
    pub total_receipts: usize,
    pub total_recounts: usize,
    pub total_cost_restatements: usize,
    pub restated_sales: usize,
}

/// Assert exact numerical AND complete bucket/lot-set agreement between a
/// simulated cell's evaluated retail questions + transforms and its independent
/// exact arithmetic oracle across the FULL simulated horizon (`0..weeks`).
///
/// # Errors
/// Returns an error on any revenue, margin, bucket-set, or lot-conservation divergence.
pub fn verify_cell_matches_oracle(
    cell: &EntityCell,
    oracle: &EntityOracle,
    weeks: u32,
) -> Result<(), String> {
    verify_revenue_buckets_exact(cell, oracle, weeks)?;
    verify_margin_buckets_exact(cell, oracle, weeks)?;
    verify_lot_conservation_transform(cell, oracle)
}

fn verify_revenue_buckets_exact(
    cell: &EntityCell,
    oracle: &EntityOracle,
    weeks: u32,
) -> Result<(), String> {
    let rev_q = cell.answer_over_weeks("supply.retail.store_revenue_weekly", weeks)?;
    if rev_q.rows.len() != oracle.weekly_revenue.len() {
        return Err(format!(
            "{}: revenue bucket-set count mismatch across {weeks}w: cell={} oracle={}",
            cell.entity_id,
            rev_q.rows.len(),
            oracle.weekly_revenue.len()
        ));
    }
    for ((ccy, store_id, week), expected_scaled) in &oracle.weekly_revenue {
        let actual = bucket_decimal_exact(&rev_q, &[ccy, store_id, week]).ok_or_else(|| {
            format!(
                "{}: missing revenue bucket ({ccy}, {store_id}, {week})",
                cell.entity_id
            )
        })?;
        if decimal_to_scaled(actual)? != *expected_scaled {
            return Err(format!(
                "{}: revenue mismatch for ({ccy},{store_id},{week}): actual={} expected={}",
                cell.entity_id,
                actual.canonical(),
                decimal_from_scaled(*expected_scaled)?
            ));
        }
    }
    Ok(())
}

fn verify_margin_buckets_exact(
    cell: &EntityCell,
    oracle: &EntityOracle,
    weeks: u32,
) -> Result<(), String> {
    let gm_q = cell.answer_over_weeks("supply.retail.store_gross_margin_weekly", weeks)?;
    let expected_gm_keys: BTreeSet<(String, String, String)> = oracle
        .weekly_costed_margin
        .keys()
        .filter(|(_, store_id, week)| {
            !oracle
                .uncosted_weeks
                .contains(&(store_id.clone(), week.clone()))
        })
        .cloned()
        .collect();
    let observed_costed_gm_keys: BTreeSet<(String, String, String)> = gm_q
        .rows
        .values()
        .filter_map(|row| {
            let parts = bucket_parts(&row.bucket);
            if parts.get(2).map(String::as_str) == Some("costed") {
                Some((
                    parts.first()?.clone(),
                    parts.get(1)?.clone(),
                    parts.get(3)?.clone(),
                ))
            } else {
                None
            }
        })
        .collect();
    if observed_costed_gm_keys != expected_gm_keys {
        return Err(format!(
            "{}: costed margin bucket-set mismatch across {weeks}w: cell={} oracle={}",
            cell.entity_id,
            observed_costed_gm_keys.len(),
            expected_gm_keys.len()
        ));
    }
    for ((ccy, store_id, week), expected_scaled) in &oracle.weekly_costed_margin {
        if oracle
            .uncosted_weeks
            .contains(&(store_id.clone(), week.clone()))
        {
            if resolve_costed_bucket(&gm_q, ccy, store_id, week).is_some() {
                return Err(format!(
                    "{}: uncosted bucket ({ccy},{store_id},{week}) MUST withhold margin",
                    cell.entity_id
                ));
            }
            continue;
        }
        let actual = resolve_costed_bucket(&gm_q, ccy, store_id, week).ok_or_else(|| {
            format!(
                "{}: missing costed margin bucket ({ccy}, {store_id}, {week})",
                cell.entity_id
            )
        })?;
        if decimal_to_scaled(actual)? != *expected_scaled {
            return Err(format!(
                "{}: margin mismatch for ({ccy},{store_id},{week}): actual={} expected={}",
                cell.entity_id,
                actual.canonical(),
                decimal_from_scaled(*expected_scaled)?
            ));
        }
    }
    Ok(())
}

fn verify_lot_conservation_transform(
    cell: &EntityCell,
    oracle: &EntityOracle,
) -> Result<(), String> {
    let active_tf = cell.evaluate_transform("supply.retail.store_active_positions")?;
    let mut observed_lots = BTreeSet::new();
    for row in &active_tf.rows {
        let store_id = row
            .values
            .get("store_id")
            .map(scalar)
            .ok_or("missing store_id")?;
        let sku_id = row
            .values
            .get("sku_id")
            .map(scalar)
            .ok_or("missing sku_id")?;
        let key = (store_id.clone(), sku_id.clone());
        observed_lots.insert(key.clone());
        let (received, depleted) = oracle.lot_conservation.get(&key).ok_or_else(|| {
            format!(
                "{}: unexpected lot position ({store_id},{sku_id})",
                cell.entity_id
            )
        })?;
        let expected_rem = received.saturating_sub(*depleted);
        let actual_rem = match row.values.get("derived_on_hand_qty") {
            Some(RuntimeValue::Number(d)) => *d,
            other => return Err(format!("unexpected derived_on_hand_qty: {other:?}")),
        };
        if decimal_to_scaled(actual_rem)? != scaled(i128::from(expected_rem)) {
            return Err(format!(
                "{}: lot conservation mismatch on ({store_id},{sku_id}): actual={actual_rem} expected={expected_rem}",
                cell.entity_id
            ));
        }
    }
    let expected_lots: BTreeSet<(String, String)> =
        oracle.lot_conservation.keys().cloned().collect();
    if observed_lots != expected_lots {
        return Err(format!(
            "{}: active lot set mismatch: cell={} oracle={}",
            cell.entity_id,
            observed_lots.len(),
            expected_lots.len()
        ));
    }
    Ok(())
}
