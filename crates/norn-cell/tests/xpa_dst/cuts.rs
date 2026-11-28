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
use norn_cell::export::ExportRecord;
use norn_cell::{CellError, ObjectRecord};
use norn_checker::metric::compute::QuestionPlan;
use norn_checker::transform::TransformPlan;
use norn_ir::{Decimal, RuntimeValue};
use norn_kernel::authz::CommitBinding;
use norn_kernel::cell_host::FactKey;
use serde_json::json;

use super::env::{
    DEFAULT_CLOCK_MS, EVIDENCE_TYPES, EntityCell, FEED_MAPPINGS, FEED_MAX_AGE_MS, REQUIRED_FEEDS,
    RETAIL_QUESTIONS, RetailPack, TEST_HANDLE_KEY, Watermarks, repo_root, retail_pack,
};
use super::operating_group::{
    EntityReportingSlice, FxRateEntry, InterEntityTransfer, OperatingGroupTier,
    evaluate_operating_group_tier, slice_from_cut_snapshot_for_weeks, transfer_leg_key,
    transfer_lot_id, transfer_sale_id,
};
use super::prng::{SECONDARY_SEED, decimal_from_scaled, decimal_to_scaled, scaled};

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

/// Load admitted transform and question plans needed by `spec`, optionally
/// widening question windows to cover `weeks` ISO weeks (`None` keeps the
/// pack's declared window).
///
/// # Errors
/// Returns an error if any plan fails admission.
pub fn load_spec_plans_with_weeks(
    pack: &RetailPack,
    spec: &CellCoverageSpec,
    weeks: Option<u32>,
) -> Result<SpecPlans, String> {
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
        let plan = match weeks {
            Some(horizon_weeks) => pack.retail_question_for_weeks(q, horizon_weeks, 5_000)?,
            None => pack.retail_question(q)?,
        };
        questions.insert(q.clone(), plan);
    }
    Ok((transforms, questions))
}

/// Load admitted transform and question plans needed by `spec`.
///
/// # Errors
/// Returns an error if any plan fails admission.
pub fn load_spec_plans(pack: &RetailPack, spec: &CellCoverageSpec) -> Result<SpecPlans, String> {
    load_spec_plans_with_weeks(pack, spec, None)
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
    eval_fn: impl FnMut(usize, &str, &QuestionPlan, QueryContext) -> Result<QueryResult, CellError>,
) -> Result<CombinedViewReport, String> {
    evaluate_multi_cell_report_for_weeks(books, spec, None, eval_fn)
}

/// Evaluate a multi-cell read-only `CombinedViewReport` across `books` with an
/// optional widened ISO-week horizon (`weeks`).
///
/// # Errors
/// Returns an error if any cell cut refuses or fails.
pub fn evaluate_multi_cell_report_for_weeks(
    books: &[&EntityCell],
    spec: &CellCoverageSpec,
    weeks: Option<u32>,
    mut eval_fn: impl FnMut(usize, &str, &QuestionPlan, QueryContext) -> Result<QueryResult, CellError>,
) -> Result<CombinedViewReport, String> {
    let pack = retail_pack()?;
    let (transforms, questions) = load_spec_plans_with_weeks(pack, spec, weeks)?;
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

/// Evaluate a real multi-cell combined management view over `weeks` and verify
/// both early-rate `FxTranslated` and bilateral `InterEntityNetted` reporting
/// projections against the live cell cuts and verified inter-entity transfers.
///
/// # Errors
/// Returns an error if combined-view evaluation or either tier projection fails.
pub fn verify_multi_cell_combined_view_and_fx_shifts(
    weeks: u32,
    cells: &[EntityCell],
    fx_history: &[FxRateEntry],
    inter_entity_transfers: &[InterEntityTransfer],
) -> Result<(), String> {
    let spec = dst_coverage_spec();
    let refs: Vec<&EntityCell> = cells.iter().collect();
    let report =
        evaluate_multi_cell_report_for_weeks(&refs, &spec, Some(weeks), |idx, _, plan, ctx| {
            let current = refs
                .get(idx)
                .ok_or_else(|| CellError::Engine("missing cell".to_owned()))?;
            current
                .cell()
                .map_err(CellError::Engine)?
                .store()
                .evaluate_question(plan, ctx, |_| Ok(true))
        })?;
    let mut slices: Vec<EntityReportingSlice> = Vec::with_capacity(cells.len());
    for (cell, snap) in cells.iter().zip(&report.cells) {
        slices.push(slice_from_cut_snapshot_for_weeks(
            &cell.entity_id,
            &cell.currency,
            snap,
            "supply.retail.store_revenue_weekly",
            Some(weeks),
        )?);
    }
    let first_rates: Vec<FxRateEntry> = fx_history.iter().take(2).cloned().collect();
    let latest_rates: Vec<FxRateEntry> = fx_history.iter().rev().take(2).cloned().collect();
    let early_fx = evaluate_operating_group_tier(
        OperatingGroupTier::FxTranslated,
        &refs,
        &slices,
        Some("EGP"),
        &first_rates,
        &[],
        None,
    )
    .map_err(|e| format!("early FxTranslated projection failed: {e:?}"))?;
    let netted = evaluate_operating_group_tier(
        OperatingGroupTier::InterEntityNetted,
        &refs,
        &slices,
        Some("EGP"),
        &latest_rates,
        inter_entity_transfers,
        None,
    )
    .map_err(|e| format!("multi-cell InterEntityNetted projection failed: {e:?}"))?;
    if early_fx.translated_revenue.is_none()
        || netted.net_group_revenue.is_none()
        || netted.eliminated_inter_entity_revenue.is_none()
    {
        return Err(
            "FX/InterEntityNetted must produce exact translated and net revenue".to_owned(),
        );
    }
    Ok(())
}

/// Seeded two-book fixture (EGP sole proprietorship + USD partnership) with real
/// bilateral transfer exports, an unrelated ledger-tip export, and unrelated
/// same-amount sale & lot exports (without transfer correlation keys).
pub struct TwoBookFixture {
    pub sole_egp_book: EntityCell,
    pub partner_usd_book: EntityCell,
    pub seller_transfer_export: ExportRecord,
    pub buyer_transfer_export: ExportRecord,
    pub unrelated_tip_export: ExportRecord,
    pub unrelated_same_amt_sale_export: ExportRecord,
    pub unrelated_same_amt_lot_export: ExportRecord,
}

/// Seed two isolated single-book cells (`LE-EGP`, `LE-USD`) including a real
/// bilateral inter-entity transfer (`IE-OK-1`) plus unrelated same-amount commits.
///
/// # Errors
/// Returns an error if any governed setup commit or single-book refusal fails.
pub fn seed_two_book_cells() -> Result<TwoBookFixture, String> {
    let (sole_egp_book, buyer_transfer_export, unrelated_tip_export, unrelated_same_amt_lot_export) =
        seed_sole_egp_book()?;
    let (partner_usd_book, seller_transfer_export, unrelated_same_amt_sale_export) =
        seed_partner_usd_book()?;
    Ok(TwoBookFixture {
        sole_egp_book,
        partner_usd_book,
        seller_transfer_export,
        buyer_transfer_export,
        unrelated_tip_export,
        unrelated_same_amt_sale_export,
        unrelated_same_amt_lot_export,
    })
}

fn seed_sole_egp_book() -> Result<(EntityCell, ExportRecord, ExportRecord, ExportRecord), String> {
    let mut book = EntityCell::open(
        &cell_id_for_entity(SECONDARY_SEED, 0),
        "LE-EGP",
        "EGP",
        "org_group_egp",
        "prod",
        DEFAULT_CLOCK_MS,
    )?;
    book.register_book("Nile Sole Prop", "sole_proprietorship")?;
    let (second_book_code, _) = book.refused(
        "supply.legal_entity.register",
        "eng:dina",
        json!({
            "entity_id": "LE-ROGUE", "legal_name": "Rogue Second Book",
            "entity_kind": "corporation", "book_code": "BOOK-ROGUE-EGP",
            "functional_currency": "EGP",
        }),
    )?;
    if second_book_code != "action.precondition_failed" {
        return Err(format!(
            "second LegalEntity in one cell must refuse, got {second_book_code}"
        ));
    }
    book.register_sku("SKU-DRESS", "dry_goods", "each")?;
    book.register_sku("SKU-RING-IE", "dry_goods", "each")?;
    book.register_store(
        "ST-EGP-1",
        "ST-EGP-1",
        "CUST-CFC",
        "mall_store",
        Some("MALL-CFC"),
    )?;
    book.record_unit_cost(
        "SKU-DRESS",
        "COST-1",
        "SUP-1",
        "100",
        "2026-07-01T00:00:00Z",
    )?;
    book.record_costed_lot(
        "LOT-EGP-BASE",
        "ST-EGP-1",
        "SKU-DRESS",
        "CUST-CFC",
        "20",
        "each",
        "100",
        "2026-08-10T09:00:00Z",
    )?;
    book.record_costed_sale(
        "SALE-EGP-1",
        "ST-EGP-1",
        "SKU-DRESS",
        "4",
        "each",
        "2000",
        "100",
        "COST-1",
        "2026-09-02T12:00:00Z",
    )?;
    // Unrelated same-amount lot (2 units * 5000 = 10,000 EGP, NO transfer correlation key).
    book.record_costed_lot(
        "LOT-EGP-UNRELATED",
        "ST-EGP-1",
        "SKU-RING-IE",
        "CUST-CFC",
        "2",
        "each",
        "5000",
        "2026-09-02T12:45:00Z",
    )?;
    let unrelated_lot_export = book.export_main_record(book.head()?.commits)?;
    // Buyer leg: 2 units * 5000 EGP = 10,000 EGP (= 200 USD * 50), transfer-correlated.
    book.record_costed_lot_keyed(
        &transfer_lot_id("IE-OK-1"),
        "ST-EGP-1",
        "SKU-RING-IE",
        "CUST-CFC",
        "2",
        "each",
        "5000",
        "2026-09-02T13:00:00Z",
        &transfer_leg_key("IE-OK-1", "buyer"),
    )?;
    let buyer_export = book.export_main_record(book.head()?.commits)?;
    book.ingest_baseline_feeds(
        "ST-EGP-1",
        "SKU-DRESS",
        "100",
        "2026-09-02T12:00:00Z",
        "ST-EGP-1",
    )?;
    book.refresh_watermarks();
    let unrelated_tip = book.export_main_record(book.head()?.commits)?;
    Ok((book, buyer_export, unrelated_tip, unrelated_lot_export))
}

fn seed_partner_usd_book() -> Result<(EntityCell, ExportRecord, ExportRecord), String> {
    let mut book = EntityCell::open(
        &cell_id_for_entity(SECONDARY_SEED, 1),
        "LE-USD",
        "USD",
        "org_group_usd",
        "prod",
        DEFAULT_CLOCK_MS,
    )?;
    book.register_book("Delta Partnership", "partnership")?;
    book.register_sku("SKU-RING", "dry_goods", "each")?;
    book.register_store(
        "ST-USD-1",
        "ST-USD-1",
        "CUST-CFC",
        "mall_store",
        Some("MALL-CFC"),
    )?;
    book.record_unit_cost("SKU-RING", "COST-1", "SUP-1", "80", "2026-07-01T00:00:00Z")?;
    book.record_costed_lot(
        "LOT-USD-BASE",
        "ST-USD-1",
        "SKU-RING",
        "CUST-CFC",
        "20",
        "each",
        "80",
        "2026-08-10T09:00:00Z",
    )?;
    // Unrelated same-amount external sale (2 units, 200 USD, NO transfer correlation key).
    book.record_costed_sale(
        "SALE-USD-EXT-A",
        "ST-USD-1",
        "SKU-RING",
        "2",
        "each",
        "200",
        "80",
        "COST-1",
        "2026-09-02T11:00:00Z",
    )?;
    let unrelated_sale_export = book.export_main_record(book.head()?.commits)?;
    book.record_costed_sale(
        "SALE-USD-EXT-B",
        "ST-USD-1",
        "SKU-RING",
        "3",
        "each",
        "600",
        "80",
        "COST-1",
        "2026-09-02T11:30:00Z",
    )?;
    // Seller leg: 2 units, 200 USD net revenue (total USD = 1000), transfer-correlated.
    book.record_costed_sale_keyed(
        &transfer_sale_id("IE-OK-1"),
        "ST-USD-1",
        "SKU-RING",
        "2",
        "each",
        "200",
        "80",
        "COST-1",
        "2026-09-02T12:30:00Z",
        &transfer_leg_key("IE-OK-1", "seller"),
    )?;
    let seller_export = book.export_main_record(book.head()?.commits)?;
    book.ingest_baseline_feeds(
        "ST-USD-1",
        "SKU-RING",
        "80",
        "2026-09-02T12:00:00Z",
        "ST-USD-1",
    )?;
    book.refresh_watermarks();
    Ok((book, seller_export, unrelated_sale_export))
}

/// Seeded Egyptian family-business fixture (sole proprietorship in EGP +
/// partnership in USD sharing `MALL-CFC` custodian) with real bilateral
/// inter-entity transfer exports (`IE-CFC-TRIM-01`) plus unrelated same-amount commits.
pub struct EgyptianFamilyFixture {
    pub sole_book: EntityCell,
    pub partner_book: EntityCell,
    pub partner_sale_export: ExportRecord,
    pub sole_buyer_lot_export: ExportRecord,
    pub sole_unrelated_tip_export: ExportRecord,
    pub partner_unrelated_sale_export: ExportRecord,
    pub sole_unrelated_lot_export: ExportRecord,
}

/// Seed the Egyptian family-business multi-entity cells and bilateral transfer commits.
///
/// # Errors
/// Returns an error if any governed setup commit fails.
pub fn seed_egyptian_family_fixture(seed: u64) -> Result<EgyptianFamilyFixture, String> {
    let (sole_book, sole_buyer_lot_export, sole_unrelated_tip_export, sole_unrelated_lot_export) =
        seed_egyptian_sole_book(seed)?;
    let (partner_book, partner_sale_export, partner_unrelated_sale_export) =
        seed_egyptian_partner_book(seed)?;
    Ok(EgyptianFamilyFixture {
        sole_book,
        partner_book,
        partner_sale_export,
        sole_buyer_lot_export,
        sole_unrelated_tip_export,
        partner_unrelated_sale_export,
        sole_unrelated_lot_export,
    })
}

fn seed_egyptian_sole_book(
    seed: u64,
) -> Result<(EntityCell, ExportRecord, ExportRecord, ExportRecord), String> {
    let mut book = EntityCell::open(
        &cell_id_for_entity(seed, 51),
        "LE-NILE-SOLE",
        "EGP",
        "org_family_nile_sole",
        "prod",
        DEFAULT_CLOCK_MS,
    )?;
    book.register_book("Nile Apparel Sole Proprietorship", "sole_proprietorship")?;
    book.register_sku("SKU-ABAYA", "dry_goods", "each")?;
    book.register_sku("SKU-GOLD-TRIM", "spares", "each")?;
    book.register_store(
        "ST-SOLE-CFC",
        "Nile CFC Booth",
        "CUST-MALL-CFC-OPS",
        "mall_store",
        Some("MALL-CFC"),
    )?;
    book.record_unit_cost(
        "SKU-ABAYA",
        "COST-ABAYA-1",
        "SUP-ATELIER",
        "400",
        "2026-07-01T00:00:00Z",
    )?;
    book.record_costed_lot(
        "LOT-SOLE-1",
        "ST-SOLE-CFC",
        "SKU-ABAYA",
        "CUST-MALL-CFC-OPS",
        "100",
        "each",
        "400",
        "2026-08-03T09:00:00Z",
    )?;
    book.record_costed_sale(
        "SALE-SOLE-1",
        "ST-SOLE-CFC",
        "SKU-ABAYA",
        "30",
        "each",
        "27000",
        "400",
        "COST-ABAYA-1",
        "2026-09-02T14:00:00Z",
    )?;
    // Unrelated same-amount lot (2 units * 7275 = 14,550 EGP, NO transfer correlation key).
    book.record_costed_lot(
        "LOT-SOLE-UNRELATED",
        "ST-SOLE-CFC",
        "SKU-GOLD-TRIM",
        "CUST-MALL-CFC-OPS",
        "2",
        "each",
        "7275",
        "2026-09-02T15:15:00Z",
    )?;
    let unrelated_lot = book.export_main_record(book.head()?.commits)?;
    // Buyer leg: 2 units * 7275 EGP = 14,550 EGP (= 300 USD * 48.5), transfer-correlated.
    book.record_costed_lot_keyed(
        &transfer_lot_id("IE-CFC-TRIM-01"),
        "ST-SOLE-CFC",
        "SKU-GOLD-TRIM",
        "CUST-MALL-CFC-OPS",
        "2",
        "each",
        "7275",
        "2026-09-02T15:30:00Z",
        &transfer_leg_key("IE-CFC-TRIM-01", "buyer"),
    )?;
    let buyer_export = book.export_main_record(book.head()?.commits)?;
    book.ingest_baseline_feeds(
        "ST-SOLE-CFC",
        "SKU-ABAYA",
        "400",
        "2026-09-02T14:00:00Z",
        "egypt-sole",
    )?;
    book.refresh_watermarks();
    let unrelated_tip = book.export_main_record(book.head()?.commits)?;
    Ok((book, buyer_export, unrelated_tip, unrelated_lot))
}

fn seed_egyptian_partner_book(
    seed: u64,
) -> Result<(EntityCell, ExportRecord, ExportRecord), String> {
    let mut book = EntityCell::open(
        &cell_id_for_entity(seed, 52),
        "LE-CAIRO-PARTNER",
        "USD",
        "org_family_cairo_partner",
        "prod",
        DEFAULT_CLOCK_MS,
    )?;
    book.register_book("Cairo Atelier Partnership", "partnership")?;
    book.register_sku("SKU-GOLD-TRIM", "spares", "each")?;
    book.register_store(
        "ST-PART-CFC",
        "Cairo CFC Gallery",
        "CUST-MALL-CFC-OPS",
        "mall_store",
        Some("MALL-CFC"),
    )?;
    book.record_unit_cost(
        "SKU-GOLD-TRIM",
        "COST-TRIM-1",
        "SUP-ATELIER",
        "80",
        "2026-07-01T00:00:00Z",
    )?;
    book.record_costed_lot(
        "LOT-PART-1",
        "ST-PART-CFC",
        "SKU-GOLD-TRIM",
        "CUST-MALL-CFC-OPS",
        "40",
        "each",
        "80",
        "2026-08-03T09:00:00Z",
    )?;
    // Unrelated same-amount sale (2 units, 300 USD, NO transfer correlation key).
    book.record_costed_sale(
        "SALE-PART-EXT-A",
        "ST-PART-CFC",
        "SKU-GOLD-TRIM",
        "2",
        "each",
        "300",
        "80",
        "COST-TRIM-1",
        "2026-09-02T14:20:00Z",
    )?;
    let unrelated_sale = book.export_main_record(book.head()?.commits)?;
    book.record_costed_sale(
        "SALE-PART-EXT-B",
        "ST-PART-CFC",
        "SKU-GOLD-TRIM",
        "6",
        "each",
        "900",
        "80",
        "COST-TRIM-1",
        "2026-09-02T14:30:00Z",
    )?;
    // Seller leg: 2 units, 300 USD net revenue (total USD = 1500), transfer-correlated.
    book.record_costed_sale_keyed(
        &transfer_sale_id("IE-CFC-TRIM-01"),
        "ST-PART-CFC",
        "SKU-GOLD-TRIM",
        "2",
        "each",
        "300",
        "80",
        "COST-TRIM-1",
        "2026-09-02T15:00:00Z",
        &transfer_leg_key("IE-CFC-TRIM-01", "seller"),
    )?;
    let seller_export = book.export_main_record(book.head()?.commits)?;
    book.ingest_baseline_feeds(
        "ST-PART-CFC",
        "SKU-GOLD-TRIM",
        "80",
        "2026-09-02T15:00:00Z",
        "egypt-partner",
    )?;
    book.refresh_watermarks();
    Ok((book, seller_export, unrelated_sale))
}
