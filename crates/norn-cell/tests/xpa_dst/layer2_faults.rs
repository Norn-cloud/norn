//! Layer 2B — Adversarial Fault Injection Suite.
//!
//! Exercises fail-closed refusals and withholding across:
//! - Out-of-order & exact duplicate feed redelivery across ALL 4 retail feeds
//! - Damaged receipt quarantine & remediation closing open findings
//! - Stale watermark heartbeats, observed-vs-consumed cursor drift, and dropped
//!   durable feed partitions
//! - Authority binding mismatch, substituted question plan, and a REAL concurrent
//!   `SQLite` commit between read-cut capture and completion (`core.compute.stale_input`)
//! - Mid-window uncosted SKU withholding while keeping unaffected stores exact

use std::collections::BTreeMap;

use norn_cell::combined_view::{CellViewInput, evaluate_cell_cut};
use norn_cell::compute::QueryContext;
use norn_cell::compute::transform::RowAccess;
use norn_cell::engine::NativeEngine;
use norn_cell::{CellError, CellStore, KnowledgeCutoff, ObjectRecord, StoreOptions};
use norn_kernel::authz::CommitBinding;
use norn_kernel::{ActionCall, ActionOutcome, DecisionTurn, LogicalTime, commit_action};
use serde_json::json;

use super::actions::StoreKpiInput;
use super::cuts::{
    bucket_decimal_exact, cell_id_for_entity, dst_coverage_spec, evaluate_spec_cut,
    resolve_costed_bucket,
};
use super::env::{
    AllowAll, COSTS_FEED_ID, COUNTS_FEED_ID, DEFAULT_CLOCK_MS, EVIDENCE_TYPES, EntityCell,
    FEED_MAPPINGS, FEED_MAX_AGE_MS, FeedRow, POS_FEED_ID, RECEIPTS_FEED_ID, REQUIRED_FEEDS,
    RetailCell, TEST_HANDLE_KEY,
};
use super::prng::SplitMix64;

/// Layer 2 Adversarial Fault Injection Suite driven by `seed`.
///
/// # Errors
/// Returns an error if any adversarial fault fails to refuse or withhold fail-closed.
pub fn run_adversarial_fault_suite(seed: u64) -> Result<(), String> {
    let mut rng = SplitMix64::new(seed).fork(0xFA01_7001);
    let mut cell = seed_fault_cell(seed)?;
    verify_out_of_order_and_duplicate_feeds(&mut cell, &mut rng)?;
    verify_quarantine_and_remediation(&mut cell)?;
    verify_watermark_cursor_and_partition_faults(&mut cell)?;
    verify_authority_plan_and_mid_read_commit_faults(&mut cell)?;
    verify_uncosted_mid_window_withholding(&mut cell)
}

fn seed_fault_cell(seed: u64) -> Result<EntityCell, String> {
    let mut cell = EntityCell::open(
        &cell_id_for_entity(seed, 99),
        "LE-FAULT",
        "EGP",
        "org_xpa_fault",
        "prod",
        DEFAULT_CLOCK_MS,
    )?;
    cell.register_book("Fault Injection Book", "sole_proprietorship")?;
    cell.register_sku("SKU-CLEAN", "dry_goods", "each")?;
    cell.register_sku("SKU-UNCOSTED", "dry_goods", "each")?;
    for (store, name) in [("ST-CLEAN", "Clean Store"), ("ST-TAINTED", "Tainted Store")] {
        cell.register_store(store, name, "CUST-1", "mall_store", Some("MALL-CFC"))?;
    }
    cell.record_unit_cost(
        "SKU-CLEAN",
        "COST-CLEAN-1",
        "SUP-1",
        "100",
        "2026-07-01T00:00:00Z",
    )?;
    for store in ["ST-CLEAN", "ST-TAINTED"] {
        cell.record_costed_lot(
            &format!("LOT-{store}"),
            store,
            "SKU-CLEAN",
            "CUST-1",
            "50",
            "each",
            "100",
            "2026-08-03T09:00:00Z",
        )?;
        cell.record_costed_sale(
            &format!("SALE-{store}-1"),
            store,
            "SKU-CLEAN",
            "5",
            "each",
            "1000",
            "100",
            "COST-CLEAN-1",
            "2026-09-02T12:00:00Z",
        )?;
    }
    Ok(cell)
}

type RawFeedSpec = (
    &'static str,
    &'static str,
    &'static str,
    &'static str,
    &'static str,
    &'static str,
);

const OOO_FEED_ROWS: [RawFeedSpec; 5] = [
    (
        POS_FEED_ID,
        "pos_sale_line",
        "synthetic-pos",
        "ev-ooo-pos-2",
        "POS-OOO-2",
        r#"{"pos_line_id":"POS-OOO-2","branch_code":"ST-CLEAN","barcode_sku":"SKU-CLEAN","qty":3,"uom":"PCS","net_amount":600,"ccy":"EGP","sold_at":"2026-09-03T15:00:00Z"}"#,
    ),
    (
        POS_FEED_ID,
        "pos_sale_line",
        "synthetic-pos",
        "ev-ooo-pos-1",
        "POS-OOO-1",
        r#"{"pos_line_id":"POS-OOO-1","branch_code":"ST-CLEAN","barcode_sku":"SKU-CLEAN","qty":2,"uom":"PCS","net_amount":400,"ccy":"EGP","sold_at":"2026-09-01T10:00:00Z"}"#,
    ),
    (
        RECEIPTS_FEED_ID,
        "retail_receipt",
        "synthetic-retail-receipt",
        "ev-ooo-rcpt-1",
        "RCPT-OOO-1",
        r#"{"receipt_line_id":"RCPT-OOO-1","store_code":"ST-CLEAN","sku_code":"SKU-CLEAN","vendor_code":"ATELIER-01","received_qty":20,"damaged_qty":2,"uom":"PCS","unit_cost":100,"ccy":"EGP","received_at":"2026-09-01T09:00:00Z"}"#,
    ),
    (
        COUNTS_FEED_ID,
        "store_count",
        "synthetic-store-count",
        "ev-ooo-cnt-1",
        "CNT-OOO-1",
        r#"{"count_id":"CNT-OOO-1","store_code":"ST-CLEAN","sku_code":"SKU-CLEAN","counted_qty":45,"uom":"PCS","basis":"on_hand","counted_at":"2026-09-04T09:00:00Z"}"#,
    ),
    (
        COSTS_FEED_ID,
        "unit_cost",
        "synthetic-cost",
        "ev-ooo-cost-1",
        "COST-OOO-1",
        r#"{"cost_id":"COST-OOO-1","sku_code":"SKU-CLEAN","vendor_code":"ATELIER-01","unit_cost":100,"ccy":"EGP","effective_from":"2026-07-01T00:00:00Z"}"#,
    ),
];

fn verify_out_of_order_and_duplicate_feeds(
    cell: &mut EntityCell,
    rng: &mut SplitMix64,
) -> Result<(), String> {
    let mut rows = OOO_FEED_ROWS.to_vec();
    rng.shuffle(&mut rows);
    for (feed, dataset, source, event, record, payload) in &rows {
        let (_, clean) = cell.ingest_feed(&FeedRow {
            feed_id: feed,
            mapping: feed,
            dataset,
            source,
            event,
            record,
            payload,
        })?;
        if !clean {
            return Err(format!("out-of-order clean row {record} quarantined"));
        }
    }

    // Inject exact duplicate deliveries across ALL 4 retail feeds (`OOO_FEED_ROWS[1..5]`).
    for (feed, dataset, source, event, record, payload) in OOO_FEED_ROWS.iter().skip(1) {
        let (_, dup_clean) = cell.ingest_feed(&FeedRow {
            feed_id: feed,
            mapping: feed,
            dataset,
            source,
            event,
            record,
            payload,
        })?;
        if !dup_clean {
            return Err(format!("duplicate {feed}/{record} should remain clean"));
        }
    }

    for (ev_type, expected_count) in [
        ("supply.PosSaleEvidence", 2_usize),
        ("supply.RetailReceiptEvidence", 1_usize),
        ("supply.StoreCountEvidence", 1_usize),
        ("supply.UnitCostEvidence", 1_usize),
    ] {
        let count = cell.object_count(ev_type)?;
        if count != expected_count {
            return Err(format!(
                "duplicate redelivery altered {ev_type} count: got {count}, expected {expected_count}"
            ));
        }
    }
    let pos_feed_q = cell.answer("supply.retail.pos_feed_revenue_weekly")?;
    let total_pos_feed = bucket_decimal_exact(&pos_feed_q, &["EGP", "ST-CLEAN", "2026-08-31"])
        .ok_or("missing POS feed revenue bucket")?;
    if total_pos_feed.canonical() != "1000" {
        return Err(format!(
            "duplicate redelivery double-counted POS revenue: got {}",
            total_pos_feed.canonical()
        ));
    }
    Ok(())
}

fn verify_quarantine_and_remediation(cell: &mut EntityCell) -> Result<(), String> {
    let (_, bad_landed) = cell.ingest_feed(&FeedRow {
        feed_id: RECEIPTS_FEED_ID,
        mapping: RECEIPTS_FEED_ID,
        dataset: "retail_receipt",
        source: "synthetic-retail-receipt",
        event: "ev-bad-rcpt-1",
        record: "RCPT-BAD-DMG",
        payload: r#"{"receipt_line_id":"RCPT-BAD-DMG","store_code":"ST-CLEAN","sku_code":"SKU-CLEAN","vendor_code":"ATELIER-01","received_qty":5,"damaged_qty":9,"uom":"PCS","unit_cost":100,"ccy":"EGP","received_at":"2026-09-02T09:00:00Z"}"#,
    })?;
    if !bad_landed || cell.open_finding_count("supply.RetailReceiptEvidence", "RCPT-BAD-DMG")? == 0
    {
        return Err(
            "damaged_qty > received_qty must land with an open data-quality finding".to_owned(),
        );
    }
    let spec = dst_coverage_spec();
    let quarantined_cut = evaluate_spec_cut(cell, &spec)?;
    if quarantined_cut.complete
        || quarantined_cut.answers.is_some()
        || quarantined_cut.open_findings == 0
    {
        return Err("open data-quality finding must fail-closed withhold answers".to_owned());
    }

    let (_, fixed_clean) = cell.ingest_feed(&FeedRow {
        feed_id: RECEIPTS_FEED_ID,
        mapping: RECEIPTS_FEED_ID,
        dataset: "retail_receipt",
        source: "synthetic-retail-receipt",
        event: "ev-fix-rcpt-1",
        record: "RCPT-BAD-DMG",
        payload: r#"{"receipt_line_id":"RCPT-BAD-DMG","store_code":"ST-CLEAN","sku_code":"SKU-CLEAN","vendor_code":"ATELIER-01","received_qty":5,"damaged_qty":1,"uom":"PCS","unit_cost":100,"ccy":"EGP","received_at":"2026-09-02T09:05:00Z"}"#,
    })?;
    if !fixed_clean || cell.open_finding_count("supply.RetailReceiptEvidence", "RCPT-BAD-DMG")? != 0
    {
        return Err("remediated receipt row must close open finding".to_owned());
    }
    cell.refresh_watermarks();
    let recovered = evaluate_spec_cut(cell, &spec)?;
    if !recovered.complete || recovered.answers.is_none() {
        return Err(format!(
            "remediated cut expected complete, got missing={:?} gates={:?} findings={} pending={}",
            recovered.missing_feeds,
            recovered.failed_coverage_gates,
            recovered.open_findings,
            recovered.pending_observations
        ));
    }
    Ok(())
}

fn verify_watermark_cursor_and_partition_faults(cell: &mut EntityCell) -> Result<(), String> {
    let spec = dst_coverage_spec();
    let saved_cursors = cell.source_cursors.clone();
    let saved_watermarks = cell.watermarks.clone();

    let now_ms = cell.now().as_millis();
    if let Some(parts) = cell.watermarks.get_mut(POS_FEED_ID)
        && let Some(wm) = parts.get_mut("pos_sale_line")
    {
        let stale_lag = FEED_MAX_AGE_MS.saturating_add(10_000);
        wm.heartbeat_ms = Some(now_ms.saturating_sub(stale_lag));
    }
    let stale_cut = evaluate_spec_cut(cell, &spec)?;
    if stale_cut.complete
        || !stale_cut.missing_feeds.contains(POS_FEED_ID)
        || stale_cut.answers.is_some()
    {
        return Err("stale watermark heartbeat must mark POS feed missing and withhold".to_owned());
    }
    saved_watermarks.clone_into(&mut cell.watermarks);

    if let Some(parts) = cell.watermarks.get_mut(COUNTS_FEED_ID)
        && let Some(wm) = parts.get_mut("store_count")
    {
        "cursor:ahead-unconsumed".clone_into(&mut wm.observed);
    }
    let drift_cut = evaluate_spec_cut(cell, &spec)?;
    if drift_cut.complete
        || !drift_cut.missing_feeds.contains(COUNTS_FEED_ID)
        || drift_cut.answers.is_some()
    {
        return Err("observed-vs-consumed cursor drift must withhold answers".to_owned());
    }
    saved_watermarks.clone_into(&mut cell.watermarks);

    if let Some(parts) = cell.source_cursors.get_mut(COSTS_FEED_ID) {
        parts.remove("unit_cost");
    }
    if evaluate_spec_cut(cell, &spec).is_ok() {
        return Err("dropped durable feed partition must fail closed".to_owned());
    }
    cell.source_cursors = saved_cursors;
    cell.watermarks = saved_watermarks;
    Ok(())
}

fn verify_authority_plan_and_mid_read_commit_faults(cell: &mut EntityCell) -> Result<(), String> {
    let rev_plan = cell
        .pack
        .retail_question("supply.retail.store_revenue_weekly")?;
    let sales_plan = cell
        .pack
        .retail_question("supply.retail.store_sales_weekly")?;
    let allow_all = |_: &ObjectRecord| Ok(RowAccess::Read);
    let frontier = || Ok((cell.source_cursors.clone(), cell.watermarks.clone()));
    let (anchor_key, authority) = cell.read_authority();
    let bad_auth = CommitBinding {
        resource_id: "LE-WRONG-ENTITY".to_owned(),
        ..authority.clone()
    };
    let base_input = CellViewInput {
        store: cell.cell()?.store(),
        anchor_key: &anchor_key,
        authority: &bad_auth,
        context: cell.query_context(),
        handle_key: &TEST_HANDLE_KEY,
        whole_world_values: false,
        member_access: &allow_all,
        min_disclosed_bucket: 2,
        required_feeds: &REQUIRED_FEEDS,
        read_frontier: &frontier,
        min_fresh_ms: cell.now().as_millis().saturating_sub(FEED_MAX_AGE_MS),
        evidence_types: &EVIDENCE_TYPES,
        feed_mappings: &FEED_MAPPINGS,
        evidence_access: &allow_all,
        coverage_transforms: &[],
        reconciliation_gates: &[],
        expected_grain_gates: &[],
        questions: &[("supply.retail.store_revenue_weekly", &rev_plan)],
    };
    let auth_res = evaluate_cell_cut(&base_input, &mut |_, plan, ctx| {
        cell.cell()
            .map_err(CellError::Engine)?
            .store()
            .evaluate_question(plan, ctx, |_| Ok(true))
    });
    if !matches!(auth_res, Err(CellError::Encoding(_))) {
        return Err(format!(
            "mismatched authority must refuse Encoding, got {auth_res:?}"
        ));
    }

    let valid_input = CellViewInput {
        authority: &authority,
        ..base_input
    };
    let sub_res = evaluate_cell_cut(&valid_input, &mut |_, _, ctx| {
        cell.cell()
            .map_err(CellError::Engine)?
            .store()
            .evaluate_question(&sales_plan, ctx, |_| Ok(true))
    });
    if !matches!(
        sub_res,
        Err(CellError::ComputeRefused("core.compute.stale_input"))
    ) {
        return Err(format!(
            "substituted question plan must refuse stale_input, got {sub_res:?}"
        ));
    }

    verify_real_mid_read_commit_refusal(cell, &valid_input)
}

/// Exercise BOTH genuine capture-to-evaluation commit interleavings on a second
/// `SQLite` handle against the same cell database, proving `evaluate_cell_cut`
/// refuses `core.compute.stale_input`:
/// 1. Capture read-cut -> concurrent `supply.pos_sale.record` commit BEFORE
///    `evaluate_question` runs -> evaluation sees advanced head/data -> refuses
///    `core.compute.stale_input`!
/// 2. Capture read-cut -> `evaluate_question` finishes -> concurrent
///    `supply.sku.register` commit BEFORE post-eval fence finishes -> refuses
///    `core.compute.stale_input`!
fn verify_real_mid_read_commit_refusal(
    cell: &EntityCell,
    valid_input: &CellViewInput<'_, NativeEngine>,
) -> Result<(), String> {
    // Interleaving 1: commit on second SQLite handle AFTER read-cut capture
    // and BEFORE question evaluation executes.
    let head_initial = cell.head()?;
    let pre_eval_race = evaluate_cell_cut(valid_input, &mut |_, plan, ctx| {
        commit_concurrent_w8_sale_on_same_db(cell).map_err(CellError::Engine)?;
        cell.cell()
            .map_err(CellError::Engine)?
            .store()
            .evaluate_question(plan, ctx, |_| Ok(true))
    });
    if !matches!(
        pre_eval_race,
        Err(CellError::ComputeRefused("core.compute.stale_input"))
    ) {
        return Err(format!(
            "capture-then-commit-then-evaluate interleaving must refuse stale_input, got {pre_eval_race:?}"
        ));
    }
    let head_after_first = cell.head()?;
    if head_after_first.commits != head_initial.commits.saturating_add(1) {
        return Err(format!(
            "pre-eval concurrent commit did not advance ledger head: before={} after={}",
            head_initial.commits, head_after_first.commits
        ));
    }

    // Interleaving 2: commit on second SQLite handle AFTER question evaluation
    // returns at the captured cut and BEFORE post-evaluation fence checks finish.
    let post_eval_race = evaluate_cell_cut(valid_input, &mut |_, plan, ctx| {
        let answer = cell
            .cell()
            .map_err(CellError::Engine)?
            .store()
            .evaluate_question(plan, ctx, |_| Ok(true))?;
        commit_concurrent_sku_on_same_db(cell).map_err(CellError::Engine)?;
        Ok(answer)
    });
    if !matches!(
        post_eval_race,
        Err(CellError::ComputeRefused("core.compute.stale_input"))
    ) {
        return Err(format!(
            "post-eval mid-read commit must refuse stale_input, got {post_eval_race:?}"
        ));
    }
    let head_after_second = cell.head()?;
    if head_after_second.commits != head_after_first.commits.saturating_add(1) {
        return Err(format!(
            "post-eval concurrent commit did not advance ledger head: before={} after={}",
            head_after_first.commits, head_after_second.commits
        ));
    }
    Ok(())
}

fn commit_concurrent_w8_sale_on_same_db(cell: &EntityCell) -> Result<(), String> {
    commit_concurrent_action_on_same_db(
        cell,
        500,
        "supply.pos_sale.record",
        "clerk:hana",
        "concurrent-mid-read-sale-w8",
        json!({
            "sale_id": "SALE-MID-READ-RACE-W8",
            "store_id": "ST-CLEAN",
            "sku_id": "SKU-CLEAN",
            "sold_qty": "2",
            "unit": "each",
            "net_revenue": "400",
            "applied_unit_cost": "100",
            "cost_status": "costed",
            "costed_ref": "COST-CLEAN-1",
            "currency": "EGP",
            "sold_at": "2026-08-26T12:00:00Z",
        }),
    )
}

fn commit_concurrent_sku_on_same_db(cell: &EntityCell) -> Result<(), String> {
    commit_concurrent_action_on_same_db(
        cell,
        700,
        "supply.sku.register",
        "eng:dina",
        "concurrent-mid-read-race-1",
        json!({
            "sku_id": "SKU-MID-READ-RACE",
            "category": "dry_goods",
            "unit_of_measure": "each",
            "shelf_life_days": 365,
        }),
    )
}

fn commit_concurrent_action_on_same_db(
    cell: &EntityCell,
    tick_offset_ms: u64,
    action_id: &str,
    actor: &str,
    idempotency_key: &str,
    payload: serde_json::Value,
) -> Result<(), String> {
    let engine = NativeEngine::open(&cell.db_path, StoreOptions::default())
        .map_err(|e| format!("open concurrent handle: {e}"))?;
    let store = CellStore::from_engine_bound(engine, cell.pack.model().clone(), &cell.cell_id)
        .map_err(|e| format!("bind concurrent store: {e}"))?;
    let mut writer = RetailCell::new(store, cell.scope.clone(), |c| c.payload.clone())
        .map_err(|e| format!("open concurrent cell: {e}"))?;
    let next_ts = LogicalTime::from_millis(cell.now().as_millis().saturating_add(tick_offset_ms));
    writer.advance_to(next_ts);
    let world = writer.store().world();
    let admitted = cell
        .pack
        .definitions
        .action(action_id)
        .ok_or_else(|| format!("missing {action_id}"))?;
    let input: norn_kernel::cell_host::JsonObject =
        serde_json::from_value(payload).map_err(|e| format!("concurrent input: {e}"))?;
    let catalog = cell.pack.definitions.catalog();
    let action_req = ActionCall {
        action: &admitted.root,
        action_artifact_id: &admitted.artifact_id,
        action_version: &admitted.version,
        scope: BTreeMap::from([
            ("orgId".into(), cell.org_id.clone()),
            ("environmentId".into(), cell.env_id.clone()),
        ]),
        actor: actor.to_owned(),
        approved_intent: None,
        input,
        idempotency_key: idempotency_key.to_owned(),
        object_type: None,
        turn: DecisionTurn {
            id: idempotency_key.to_owned(),
            now: next_ts,
            world,
            consumes: None,
        },
        trace: None,
    };
    match commit_action(&mut writer, &catalog, &action_req, &AllowAll) {
        Ok(ActionOutcome::Committed { .. }) => Ok(()),
        other => Err(format!("concurrent commit unexpected outcome: {other:?}")),
    }
}

fn verify_uncosted_mid_window_withholding(cell: &mut EntityCell) -> Result<(), String> {
    cell.record_uncosted_lot(
        "LOT-UNCOSTED-1",
        "ST-TAINTED",
        "SKU-UNCOSTED",
        "CUST-1",
        "15",
        "each",
        "2026-09-02T09:00:00Z",
    )?;
    cell.record_uncosted_sale(
        "SALE-UNCOSTED-1",
        "ST-TAINTED",
        "SKU-UNCOSTED",
        "3",
        "each",
        "750",
        "2026-09-03T11:00:00Z",
    )?;
    cell.refresh_watermarks();

    let gm_q = cell.answer("supply.retail.store_gross_margin_weekly")?;
    if resolve_costed_bucket(&gm_q, "EGP", "ST-TAINTED", "2026-08-31").is_some() {
        return Err("ST-TAINTED with missing_cost sale MUST withhold gross margin".to_owned());
    }
    let clean_gm = resolve_costed_bucket(&gm_q, "EGP", "ST-CLEAN", "2026-08-31")
        .ok_or("unaffected ST-CLEAN must still report exact gross margin")?;
    if clean_gm.canonical() != "500" {
        return Err(format!("expected ST-CLEAN margin 500, got {clean_gm}"));
    }

    let val_q = cell.answer("supply.retail.store_stock_value_weekly")?;
    if resolve_costed_bucket(&val_q, "EGP", "ST-TAINTED", "2026-08-31").is_some() {
        return Err("ST-TAINTED with uncosted lot MUST withhold stock valuation".to_owned());
    }
    let clean_val = resolve_costed_bucket(&val_q, "EGP", "ST-CLEAN", "2026-08-31")
        .ok_or("unaffected ST-CLEAN must still report exact stock valuation")?;
    if clean_val.canonical() != "5000" {
        return Err(format!(
            "expected ST-CLEAN stock value 5000, got {clean_val}"
        ));
    }

    let rev_q = cell.answer("supply.retail.store_revenue_weekly")?;
    let clean_rev = bucket_decimal_exact(&rev_q, &["EGP", "ST-CLEAN", "2026-08-31"])
        .ok_or("missing ST-CLEAN revenue")?;
    let tainted_rev = bucket_decimal_exact(&rev_q, &["EGP", "ST-TAINTED", "2026-08-31"])
        .ok_or("missing ST-TAINTED revenue")?;
    if clean_rev.canonical() != "1000" || tainted_rev.canonical() != "1750" {
        return Err(format!(
            "cost-independent revenue mismatch: ST-CLEAN={clean_rev}, ST-TAINTED={tainted_rev}"
        ));
    }

    let spec = dst_coverage_spec();
    let cut = evaluate_spec_cut(cell, &spec)?;
    if cut.complete
        || cut.answers.is_some()
        || !cut
            .failed_coverage_gates
            .contains("supply.retail.entity_management_summary")
    {
        return Err(format!(
            "uncosted SKU must fail entity_management_summary gate: {:?}",
            cut.failed_coverage_gates
        ));
    }
    Ok(())
}

/// Dedicated bitemporal check performing genuine **in-place** `update_only`
/// restatements on existing W9 (`2026-08-31`) objects — `InventorySnapshot`
/// (`supply.position.restate`), `StockLot` (`supply.stock_lot.update_balance`),
/// `StoreCount` (`supply.store_count.recount`), `StoreCommercialRollup`
/// (`supply.store_kpi.refresh`), and `UnitCostEvidence` (in-place feed upsert
/// on the same `cost_id`) — asserting via `evaluate_question_known_at` that
/// `prior_cutoff` returns the exact ORIGINAL W9 answers while live evaluation
/// returns the exact RESTATED W9 answers for those SAME existing object IDs.
///
/// # Errors
/// Returns an error if bitemporal preservation or restated W9 assertion fails.
pub fn verify_late_arriving_unit_cost_on_existing_sku(seed: u64) -> Result<(), String> {
    let (mut cell, prior_cutoff, prior_ctx) = seed_late_cost_cell(seed)?;
    apply_w9_in_place_restatements(&mut cell)?;
    assert_bitemporal_w9_pair(
        &cell,
        &cell.pack.supply_question("supply.on_hand_weekly")?,
        &prior_ctx,
        prior_cutoff,
        &["each", "WH-LATE-1", "2026-08-31"],
        "40",
        "20",
        "InventorySnapshot restatement",
    )?;
    assert_bitemporal_w9_pair(
        &cell,
        &cell
            .pack
            .retail_question("supply.retail.store_stock_value_weekly")?,
        &prior_ctx,
        prior_cutoff,
        &["EGP", "ST-LATE-1", "costed", "2026-08-31"],
        "4000",
        "2000",
        "StockLot in-place update",
    )?;
    assert_bitemporal_w9_pair(
        &cell,
        &cell
            .pack
            .retail_question("supply.retail.store_weeks_of_supply_weekly")?,
        &prior_ctx,
        prior_cutoff,
        &["each", "ST-LATE-1", "SKU-LATE-0", "2026-08-31"],
        "2",
        "0.5",
        "StoreCommercialRollup WOS refresh",
    )?;
    assert_bitemporal_w9_pair(
        &cell,
        &cell
            .pack
            .retail_question("supply.retail.store_gmroi_weekly")?,
        &prior_ctx,
        prior_cutoff,
        &["EGP", "ST-LATE-1", "SKU-LATE-0", "2026-08-31"],
        "0.2",
        "0.5",
        "StoreCommercialRollup GMROI refresh",
    )
}

fn apply_w9_in_place_restatements(cell: &mut EntityCell) -> Result<(), String> {
    let prior_cost_ev_count = cell.object_count("supply.UnitCostEvidence")?;
    let cost_payload = json!({
        "cost_id": "COST-LATE-0-EV", "sku_code": "SKU-LATE-0", "vendor_code": "ATELIER-01",
        "unit_cost": 120, "ccy": "EGP", "effective_from": "2026-08-31T00:00:00Z",
    })
    .to_string();
    let (_, clean) = cell.ingest_feed(&FeedRow {
        feed_id: COSTS_FEED_ID,
        mapping: COSTS_FEED_ID,
        dataset: "unit_cost",
        source: "synthetic-cost",
        event: "ev-late-cost-restate-1",
        record: "COST-LATE-0-EV",
        payload: &cost_payload,
    })?;
    if !clean || cell.object_count("supply.UnitCostEvidence")? != prior_cost_ev_count {
        return Err(
            "in-place UnitCostEvidence restatement must update existing cost_id".to_owned(),
        );
    }
    // Commit genuine in-place updates on the SAME W9 (`2026-08-31`) object IDs
    // (`SNAP-LATE-W9`, `LOT-LATE-1`, `ST-LATE-1/SKU-LATE-0/on_hand`, `KPI-LATE-W9`).
    cell.record_inventory_position(
        true,
        "SNAP-LATE-W9",
        "WH-LATE-1",
        "SKU-LATE-0",
        "20",
        "2026-09-04T12:00:00Z",
        "AUDIT-LATE-W9-2",
    )?;
    cell.record_costed_sale(
        "SALE-LATE-W9-2",
        "ST-LATE-1",
        "SKU-LATE-0",
        "20",
        "each",
        "3000",
        "100",
        "COST-LATE-0-V1",
        "2026-09-03T12:00:00Z",
    )?;
    cell.update_lot_balance(
        "LOT-LATE-1",
        "ST-LATE-1",
        "SKU-LATE-0",
        "20",
        "40",
        "100",
        "2026-09-04T12:00:00Z",
    )?;
    cell.record_store_count(
        true,
        "ST-LATE-1/SKU-LATE-0/on_hand",
        "ST-LATE-1",
        "SKU-LATE-0",
        "20",
        "each",
        "on_hand",
        "2026-09-04T14:00:00Z",
        "CC-LATE-2",
    )?;
    cell.refresh_store_kpi(&StoreKpiInput {
        rollup_id: "KPI-LATE-W9",
        store_id: "ST-LATE-1",
        sku_id: "SKU-LATE-0",
        period_week: "2026-08-31",
        unit: "each",
        on_hand_count_id: "ST-LATE-1/SKU-LATE-0/on_hand",
        opening_cut_id: "CUT-LATE-W9-OPEN",
        count_ref: "CC-LATE-2",
        count_as_of: "2026-09-04T14:00:00Z",
        costed_ref: "COST-LATE-0-V1",
        period_start: "2026-08-31T00:00:00Z",
        period_end: "2026-09-06T23:59:59.999999999Z",
        sales_weeks: 1,
        on_hand_qty: "20",
        avg_weekly_sales_qty: "40",
        period_gross_margin: "2000",
        opening_stock_value: "6000",
        closing_stock_value: "2000",
    })
}

#[allow(
    clippy::too_many_arguments,
    reason = "bitemporal prior-cut vs live comparison on one existing W9 bucket"
)]
fn assert_bitemporal_w9_pair(
    cell: &EntityCell,
    plan: &norn_checker::metric::compute::QuestionPlan,
    prior_ctx: &QueryContext,
    prior_cutoff: KnowledgeCutoff,
    bucket: &[&str],
    expected_prior: &str,
    expected_live: &str,
    label: &str,
) -> Result<(), String> {
    let prior = cell
        .cell()?
        .store()
        .evaluate_question_known_at(plan, prior_ctx.clone(), prior_cutoff, |_| Ok(true))
        .map_err(|e| format!("{label} known_at: {e}"))?;
    let prior_val = bucket_decimal_exact(&prior.result, bucket)
        .ok_or_else(|| format!("{label}: missing prior W9 bucket {bucket:?}"))?;
    if prior_val.canonical() != expected_prior {
        return Err(format!(
            "{label}: prior cut expected {expected_prior}, got {}",
            prior_val.canonical()
        ));
    }
    let live = cell
        .cell()?
        .store()
        .evaluate_question(plan, cell.query_context(), |_| Ok(true))
        .map_err(|e| format!("{label} live: {e}"))?;
    let live_val = bucket_decimal_exact(&live, bucket)
        .ok_or_else(|| format!("{label}: missing live W9 bucket {bucket:?}"))?;
    if live_val.canonical() != expected_live {
        return Err(format!(
            "{label}: live cut expected {expected_live}, got {}",
            live_val.canonical()
        ));
    }
    Ok(())
}

fn seed_late_cost_cell(seed: u64) -> Result<(EntityCell, KnowledgeCutoff, QueryContext), String> {
    let mut cell = EntityCell::open(
        &cell_id_for_entity(seed, 88),
        "LE-LATE-COST",
        "EGP",
        "org_xpa_late_cost",
        "prod",
        DEFAULT_CLOCK_MS,
    )?;
    cell.register_book("Late Restatement Entity", "sole_proprietorship")?;
    cell.register_sku("SKU-LATE-0", "dry_goods", "each")?;
    cell.register_store(
        "ST-LATE-1",
        "Late Restatement Store",
        "CUST-1",
        "mall_store",
        Some("MALL-CFC"),
    )?;
    cell.register_warehouse("WH-LATE-1", "Late Restatement Warehouse")?;
    cell.record_unit_cost(
        "SKU-LATE-0",
        "COST-LATE-0-V1",
        "SUP-ATELIER",
        "100",
        "2026-07-01T00:00:00Z",
    )?;
    let init_ev = json!({
        "cost_id": "COST-LATE-0-EV", "sku_code": "SKU-LATE-0", "vendor_code": "ATELIER-01",
        "unit_cost": 100, "ccy": "EGP", "effective_from": "2026-07-01T00:00:00Z",
    })
    .to_string();
    let (_, clean) = cell.ingest_feed(&FeedRow {
        feed_id: COSTS_FEED_ID,
        mapping: COSTS_FEED_ID,
        dataset: "unit_cost",
        source: "synthetic-cost",
        event: "ev-late-cost-init-1",
        record: "COST-LATE-0-EV",
        payload: &init_ev,
    })?;
    if !clean {
        return Err("initial UnitCostEvidence quarantined unexpectedly".to_owned());
    }
    seed_late_cost_w9_position(&mut cell)?;
    let cutoff = KnowledgeCutoff {
        recorded_at: cell.now(),
        frontier: cell.head()?.commits,
    };
    let ctx = cell.query_context();
    Ok((cell, cutoff, ctx))
}

fn seed_late_cost_w9_position(cell: &mut EntityCell) -> Result<(), String> {
    cell.record_costed_lot(
        "LOT-LATE-1",
        "ST-LATE-1",
        "SKU-LATE-0",
        "CUST-1",
        "60",
        "each",
        "100",
        "2026-08-03T09:00:00Z",
    )?;
    cell.record_stock_valuation_cut(
        "CUT-LATE-W9-OPEN",
        "ST-LATE-1",
        "SKU-LATE-0",
        "each",
        "2026-08-31T00:00:00Z",
        "60",
        "6000",
    )?;
    cell.record_costed_sale(
        "SALE-LATE-W9-1",
        "ST-LATE-1",
        "SKU-LATE-0",
        "20",
        "each",
        "3000",
        "100",
        "COST-LATE-0-V1",
        "2026-09-01T12:00:00Z",
    )?;
    cell.update_lot_balance(
        "LOT-LATE-1",
        "ST-LATE-1",
        "SKU-LATE-0",
        "40",
        "20",
        "100",
        "2026-09-02T12:00:00Z",
    )?;
    cell.record_store_count(
        false,
        "ST-LATE-1/SKU-LATE-0/on_hand",
        "ST-LATE-1",
        "SKU-LATE-0",
        "40",
        "each",
        "on_hand",
        "2026-09-02T14:00:00Z",
        "CC-LATE-1",
    )?;
    cell.record_store_kpi(&StoreKpiInput {
        rollup_id: "KPI-LATE-W9",
        store_id: "ST-LATE-1",
        sku_id: "SKU-LATE-0",
        period_week: "2026-08-31",
        unit: "each",
        on_hand_count_id: "ST-LATE-1/SKU-LATE-0/on_hand",
        opening_cut_id: "CUT-LATE-W9-OPEN",
        count_ref: "CC-LATE-1",
        count_as_of: "2026-09-02T14:00:00Z",
        costed_ref: "COST-LATE-0-V1",
        period_start: "2026-08-31T00:00:00Z",
        period_end: "2026-09-06T23:59:59.999999999Z",
        sales_weeks: 1,
        on_hand_qty: "40",
        avg_weekly_sales_qty: "20",
        period_gross_margin: "1000",
        opening_stock_value: "6000",
        closing_stock_value: "4000",
    })?;
    cell.record_inventory_position(
        false,
        "SNAP-LATE-W9",
        "WH-LATE-1",
        "SKU-LATE-0",
        "40",
        "2026-09-02T12:00:00Z",
        "INIT-LATE-W9-1",
    )
}
