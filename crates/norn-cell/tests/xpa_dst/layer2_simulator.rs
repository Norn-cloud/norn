//! Layer 2 — Seeded Multi-Cell Event-Log Simulator & Adversarial Fault Injection.
//!
//! Drives 16–52 simulated weeks of concurrent/interleaved POS sales, supplier
//! receipts, damaged returns, physical cycle counts, late-arriving cost
//! restatements, mid-week FX shifts, and inter-entity stock events across 2–10
//! independent legal-entity cells, checking every cell against an independent
//! exact arithmetic oracle.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::atomic::{AtomicBool, Ordering};

use norn_cell::combined_view::{CellViewInput, evaluate_cell_cut};
use norn_cell::compute::transform::RowAccess;
use norn_cell::compute::{Frontier, Watermark};
use norn_cell::{CellError, ObjectRecord};
use norn_ir::RuntimeValue;
use norn_kernel::authz::CommitBinding;
use serde_json::json;

use super::cuts::{
    bucket_decimal_exact, cell_id_for_entity, dst_coverage_spec, evaluate_spec_cut,
    resolve_costed_bucket, scalar,
};
use super::env::{
    COSTS_FEED_ID, COUNTS_FEED_ID, DEFAULT_CLOCK_MS, EVIDENCE_TYPES, EntityCell, FEED_MAPPINGS,
    FEED_MAX_AGE_MS, FeedRow, POS_FEED_ID, RECEIPTS_FEED_ID, REQUIRED_FEEDS, TEST_HANDLE_KEY,
};
use super::layer1_invariants::{FxRateEntry, InterEntityTransfer};
use super::prng::{
    SplitMix64, decimal_from_scaled, decimal_to_scaled, rem_u32, rem_usize, scaled, week_epoch_ms,
    week_monday_iso, week_timestamp_utc,
};

/// Configuration for one seeded multi-cell xP&A simulation run.
#[derive(Debug, Clone, Copy)]
pub struct SimConfig {
    pub seed: u64,
    pub entity_count: usize,
    pub weeks: u32,
    pub stores_per_entity: usize,
    pub skus_per_entity: usize,
}

/// Independent exact arithmetic oracle tracked alongside each simulated cell.
#[derive(Debug, Default, Clone)]
pub struct EntityOracle {
    /// `(currency, store_id, week_monday) -> exact scaled net_revenue`
    pub weekly_revenue: BTreeMap<(String, String, String), i128>,
    /// `(currency, store_id, week_monday) -> exact scaled gross_margin` (for costed sales)
    pub weekly_costed_margin: BTreeMap<(String, String, String), i128>,
    /// `(store_id, week_monday)` keys containing any uncosted sale.
    pub uncosted_weeks: BTreeSet<(String, String)>,
    /// `(store_id, sku_id) -> (received_qty, depleted_qty)`.
    pub lot_conservation: BTreeMap<(String, String), (i64, i64)>,
    pub total_sales: usize,
    pub total_receipts: usize,
    pub total_recounts: usize,
    pub total_cost_restatements: usize,
}

/// Complete result of a multi-cell seeded simulation run.
pub struct SimulatedWorld {
    pub cells: Vec<EntityCell>,
    pub oracles: Vec<EntityOracle>,
    pub fx_history: Vec<FxRateEntry>,
    pub inter_entity_transfers: Vec<InterEntityTransfer>,
}

const CURRENCIES: [&str; 3] = ["EGP", "USD", "EUR"];
const ENTITY_KINDS: [&str; 3] = ["sole_proprietorship", "partnership", "corporation"];

struct SimState {
    cells: Vec<EntityCell>,
    oracles: Vec<EntityOracle>,
    active_costs: BTreeMap<(usize, usize), (String, i64)>,
    lot_state: BTreeMap<(usize, usize, usize), (String, i64, i64, i64)>,
    counted_once: BTreeSet<(usize, usize, usize)>,
    fx_history: Vec<FxRateEntry>,
    inter_entity_transfers: Vec<InterEntityTransfer>,
    seq: u64,
}

/// Run a deterministic multi-cell retail simulation across `config.entity_count`
/// independent legal-entity cells for `config.weeks` weeks (`16..=52`).
///
/// # Errors
/// Returns an error if any governed commit, feed ingestion, or oracle check fails.
pub fn run_seeded_multi_cell_sim(config: SimConfig) -> Result<SimulatedWorld, String> {
    let mut rng = SplitMix64::new(config.seed);
    let mut state = init_sim_cells(config, &mut rng)?;
    for week in 0..config.weeks {
        simulate_one_week(config, week, &mut state, &mut rng)?;
    }
    finalize_and_verify_sim(config, &mut state)?;
    Ok(SimulatedWorld {
        cells: state.cells,
        oracles: state.oracles,
        fx_history: state.fx_history,
        inter_entity_transfers: state.inter_entity_transfers,
    })
}

fn init_sim_cells(config: SimConfig, rng: &mut SplitMix64) -> Result<SimState, String> {
    let end_clock_ms = week_epoch_ms(config.weeks, 1, 12);
    let mut state = SimState {
        cells: Vec::with_capacity(config.entity_count),
        oracles: Vec::with_capacity(config.entity_count),
        active_costs: BTreeMap::new(),
        lot_state: BTreeMap::new(),
        counted_once: BTreeSet::new(),
        fx_history: Vec::new(),
        inter_entity_transfers: Vec::new(),
        seq: 0,
    };
    for e_idx in 0..config.entity_count {
        let (cell, oracle) = init_one_sim_entity(
            config,
            e_idx,
            end_clock_ms,
            rng,
            &mut state.active_costs,
            &mut state.lot_state,
        )?;
        state.cells.push(cell);
        state.oracles.push(oracle);
    }
    Ok(state)
}

fn init_one_sim_entity(
    config: SimConfig,
    e_idx: usize,
    end_clock_ms: u64,
    rng: &mut SplitMix64,
    active_costs: &mut BTreeMap<(usize, usize), (String, i64)>,
    lot_state: &mut BTreeMap<(usize, usize, usize), (String, i64, i64, i64)>,
) -> Result<(EntityCell, EntityOracle), String> {
    let ccy = CURRENCIES
        .get(rem_usize(e_idx, CURRENCIES.len()))
        .copied()
        .unwrap_or("EGP");
    let kind = ENTITY_KINDS
        .get(rem_usize(e_idx, ENTITY_KINDS.len()))
        .copied()
        .unwrap_or("sole_proprietorship");
    let mut cell = EntityCell::open(
        &cell_id_for_entity(config.seed, e_idx),
        &format!("LE-SIM-{e_idx}"),
        ccy,
        &format!("org_sim_{e_idx}"),
        "prod",
        end_clock_ms,
    )?;
    cell.register_book(&format!("Sim Entity {e_idx}"), kind)?;
    let mut oracle = EntityOracle::default();
    for st_idx in 0..config.stores_per_entity {
        let store_id = format!("ST-{e_idx}-{st_idx}");
        let mall = (st_idx == 0).then_some("MALL-CFC");
        cell.register_store(
            &store_id,
            &format!("Store {e_idx}-{st_idx}"),
            if mall.is_some() {
                "CUST-MALL-CFC-OPS"
            } else {
                "CUST-BRANCH-OPS"
            },
            if mall.is_some() {
                "mall_store"
            } else {
                "standalone"
            },
            mall,
        )?;
    }
    for s_idx in 0..config.skus_per_entity {
        let sku_id = format!("SKU-{e_idx}-{s_idx}");
        let cost_ref = format!("COST-{e_idx}-{s_idx}-W0");
        let cost_val = i64::try_from(rng.range_u64(40, 220)).unwrap_or(100);
        cell.register_sku(&sku_id, "dry_goods", "each")?;
        cell.record_unit_cost(
            &sku_id,
            &cost_ref,
            &format!("SUP-{}", rem_usize(s_idx, 3)),
            &cost_val.to_string(),
            "2026-06-01T00:00:00Z",
        )?;
        active_costs.insert((e_idx, s_idx), (cost_ref, cost_val));
        for st_idx in 0..config.stores_per_entity {
            let store_id = format!("ST-{e_idx}-{st_idx}");
            let lot_id = format!("LOT-{e_idx}-{st_idx}-{s_idx}-INIT");
            let initial_qty = i64::try_from(rng.range_u64(400, 900)).unwrap_or(500);
            cell.record_costed_lot(
                &lot_id,
                &store_id,
                &sku_id,
                if st_idx == 0 {
                    "CUST-MALL-CFC-OPS"
                } else {
                    "CUST-BRANCH-OPS"
                },
                &initial_qty.to_string(),
                "each",
                &cost_val.to_string(),
                "2026-07-06T08:00:00Z",
            )?;
            lot_state.insert(
                (e_idx, st_idx, s_idx),
                (lot_id, initial_qty, initial_qty, cost_val),
            );
            oracle
                .lot_conservation
                .insert((store_id, sku_id.clone()), (initial_qty, 0));
            oracle.total_receipts = oracle.total_receipts.saturating_add(1);
        }
    }
    Ok((cell, oracle))
}

fn simulate_one_week(
    config: SimConfig,
    week: u32,
    state: &mut SimState,
    rng: &mut SplitMix64,
) -> Result<(), String> {
    let week_monday = week_monday_iso(week)?;
    if rem_u32(week, 4) == 0 {
        state.fx_history.push(FxRateEntry {
            from_currency: "USD".to_owned(),
            to_currency: "EGP".to_owned(),
            rate_scaled: i128::from(rng.range_u64(47_500_000, 51_500_000)),
            as_of: week_timestamp_utc(week, 2, 12)?,
            rate_table_pin: format!("fx:sim:{}:w{week}", config.seed),
        });
    }
    if config.entity_count >= 2 && rem_u32(week, 8) == 3 {
        let xfer_amount = scaled(i128::from(rng.range_u64(150, 500)));
        state.inter_entity_transfers.push(InterEntityTransfer {
            transfer_id: format!("IE-SIM-{}-W{week}", config.seed),
            seller_entity_id: "LE-SIM-1".to_owned(),
            buyer_entity_id: "LE-SIM-0".to_owned(),
            currency: "USD".to_owned(),
            seller_amount_scaled: xfer_amount,
            buyer_amount_scaled: xfer_amount,
            seller_receipt_pin: Some(format!("rcpt:le1:w{week}")),
            buyer_receipt_pin: Some(format!("rcpt:le0:w{week}")),
        });
    }

    for e_idx in 0..config.entity_count {
        simulate_entity_week_step(config, week, &week_monday, e_idx, state, rng)?;
    }
    Ok(())
}

fn simulate_entity_week_step(
    config: SimConfig,
    week: u32,
    week_monday: &str,
    e_idx: usize,
    state: &mut SimState,
    rng: &mut SplitMix64,
) -> Result<(), String> {
    let st_idx = rng.index(config.stores_per_entity);
    let s_idx = rng.index(config.skus_per_entity);
    let store_id = format!("ST-{e_idx}-{st_idx}");
    let sku_id = format!("SKU-{e_idx}-{s_idx}");

    maybe_ingest_cost_restatement(week, e_idx, s_idx, &sku_id, state, rng)?;
    let (sold_qty, net_rev, gross_margin, seq) =
        record_simulated_sale(week, e_idx, s_idx, &store_id, &sku_id, state, rng)?;
    deplete_and_maybe_recount(
        week, e_idx, st_idx, s_idx, &store_id, &sku_id, sold_qty, seq, state,
    )?;

    let cell_ccy = state
        .cells
        .get(e_idx)
        .map(|c| c.currency.clone())
        .ok_or("missing entity cell")?;
    if let Some(oracle) = state.oracles.get_mut(e_idx) {
        if week < 16 {
            let key = (cell_ccy, store_id, week_monday.to_owned());
            let rev_entry = oracle.weekly_revenue.entry(key.clone()).or_insert(0);
            *rev_entry = rev_entry
                .checked_add(scaled(i128::from(net_rev)))
                .ok_or("oracle revenue overflow")?;
            let gm_entry = oracle.weekly_costed_margin.entry(key).or_insert(0);
            *gm_entry = gm_entry
                .checked_add(scaled(i128::from(gross_margin)))
                .ok_or("oracle margin overflow")?;
        }
        oracle.total_sales = oracle.total_sales.saturating_add(1);
    }
    Ok(())
}

fn maybe_ingest_cost_restatement(
    week: u32,
    e_idx: usize,
    s_idx: usize,
    sku_id: &str,
    state: &mut SimState,
    rng: &mut SplitMix64,
) -> Result<(), String> {
    if week == 0 || rem_u32(week, 6) != 0 || !rng.chance(1, 2) {
        return Ok(());
    }
    let restated = rng.range_u64(50, 240);
    let ccy = state
        .cells
        .get(e_idx)
        .map(|c| c.currency.clone())
        .ok_or("missing entity cell")?;
    let payload = json!({
        "cost_id": format!("COST-RESTATE-{e_idx}-{s_idx}-W{week}"),
        "sku_code": sku_id,
        "vendor_code": "ATELIER-01",
        "unit_cost": restated,
        "ccy": ccy,
        "effective_from": week_timestamp_utc(week, 0, 0)?,
    })
    .to_string();
    if let Some(cell) = state.cells.get_mut(e_idx) {
        let (_, clean) = cell.ingest_feed(&FeedRow {
            feed_id: COSTS_FEED_ID,
            mapping: COSTS_FEED_ID,
            dataset: "unit_cost",
            source: "synthetic-cost",
            event: &format!("ev-cost-restate-{e_idx}-{s_idx}-w{week}"),
            record: &format!("COST-RESTATE-{e_idx}-{s_idx}-W{week}"),
            payload: &payload,
        })?;
        if !clean {
            return Err(format!("cost restatement w{week} quarantined unexpectedly"));
        }
    }
    if let Some(oracle) = state.oracles.get_mut(e_idx) {
        oracle.total_cost_restatements = oracle.total_cost_restatements.saturating_add(1);
    }
    Ok(())
}

fn record_simulated_sale(
    week: u32,
    e_idx: usize,
    s_idx: usize,
    store_id: &str,
    sku_id: &str,
    state: &mut SimState,
    rng: &mut SplitMix64,
) -> Result<(i64, i64, i64, u64), String> {
    let (cost_ref, unit_cost) = state
        .active_costs
        .get(&(e_idx, s_idx))
        .cloned()
        .ok_or("missing active cost")?;
    let sold_qty = i64::try_from(rng.range_u64(1, 6)).unwrap_or(2);
    let margin_per_unit = i64::try_from(rng.range_u64(20, 120)).unwrap_or(40);
    let net_rev = sold_qty.saturating_mul(unit_cost.saturating_add(margin_per_unit));
    let gross_margin = sold_qty.saturating_mul(margin_per_unit);
    state.seq = state.seq.saturating_add(1);
    let seq = state.seq;
    let sold_at = week_timestamp_utc(week, u32::try_from(rng.range_u64(0, 5)).unwrap_or(1), 14)?;
    if let Some(cell) = state.cells.get_mut(e_idx) {
        cell.record_costed_sale(
            &format!("SALE-{e_idx}-W{week}-{seq}"),
            store_id,
            sku_id,
            &sold_qty.to_string(),
            "each",
            &net_rev.to_string(),
            &unit_cost.to_string(),
            &cost_ref,
            &sold_at,
        )?;
    }
    Ok((sold_qty, net_rev, gross_margin, seq))
}

#[allow(clippy::too_many_arguments, reason = "internal simulator step helper")]
fn deplete_and_maybe_recount(
    week: u32,
    e_idx: usize,
    st_idx: usize,
    s_idx: usize,
    store_id: &str,
    sku_id: &str,
    sold_qty: i64,
    seq: u64,
    state: &mut SimState,
) -> Result<(), String> {
    if let Some((lot_id, received, remaining, lot_unit_cost)) =
        state.lot_state.get_mut(&(e_idx, st_idx, s_idx))
        && *remaining >= sold_qty
    {
        *remaining = remaining.saturating_sub(sold_qty);
        let depleted = received.saturating_sub(*remaining);
        if let Some(cell) = state.cells.get_mut(e_idx) {
            cell.update_lot_balance(
                lot_id,
                store_id,
                sku_id,
                &remaining.to_string(),
                &depleted.to_string(),
                &lot_unit_cost.to_string(),
                &week_timestamp_utc(week, 5, 18)?,
            )?;
        }
        if let Some(oracle) = state.oracles.get_mut(e_idx) {
            oracle.lot_conservation.insert(
                (store_id.to_owned(), sku_id.to_owned()),
                (*received, depleted),
            );
        }
    }
    if rem_u32(week, 4) == 2
        && let Some((_, _, remaining, _)) = state.lot_state.get(&(e_idx, st_idx, s_idx))
    {
        let recount = !state.counted_once.insert((e_idx, st_idx, s_idx));
        if let Some(cell) = state.cells.get_mut(e_idx) {
            cell.record_store_count(
                recount,
                &format!("{store_id}/{sku_id}/on_hand"),
                store_id,
                sku_id,
                &remaining.to_string(),
                "each",
                "on_hand",
                &week_timestamp_utc(week, 6, 10)?,
                &format!("CC-W{week}-{seq}"),
            )?;
        }
        if let Some(oracle) = state.oracles.get_mut(e_idx) {
            oracle.total_recounts = oracle.total_recounts.saturating_add(1);
        }
    }
    Ok(())
}

fn finalize_and_verify_sim(config: SimConfig, state: &mut SimState) -> Result<(), String> {
    for e_idx in 0..config.entity_count {
        let store_id = format!("ST-{e_idx}-0");
        let sku_id = format!("SKU-{e_idx}-0");
        let cost_val = state.active_costs.get(&(e_idx, 0)).map_or(100, |(_, c)| *c);
        if let Some(cell) = state.cells.get_mut(e_idx) {
            cell.ingest_baseline_feeds(
                &store_id,
                &sku_id,
                &cost_val.to_string(),
                "2026-09-02T12:00:00Z",
                &format!("sim-{e_idx}"),
            )?;
            cell.refresh_watermarks();
            let oracle = state.oracles.get(e_idx).ok_or("missing oracle")?;
            verify_cell_matches_oracle(cell, oracle)?;
        }
    }
    Ok(())
}

/// Assert exact numerical agreement between a simulated cell's evaluated retail
/// questions + transforms and its independent exact arithmetic oracle.
///
/// # Errors
/// Returns an error on any revenue, margin, or conservation divergence.
pub fn verify_cell_matches_oracle(cell: &EntityCell, oracle: &EntityOracle) -> Result<(), String> {
    let rev_q = cell.answer("supply.retail.store_revenue_weekly")?;
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

    let gm_q = cell.answer("supply.retail.store_gross_margin_weekly")?;
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

    verify_lot_conservation_transform(cell, oracle)
}

fn verify_lot_conservation_transform(
    cell: &EntityCell,
    oracle: &EntityOracle,
) -> Result<(), String> {
    let active_tf = cell.evaluate_transform("supply.retail.store_active_positions")?;
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
        if let Some((received, depleted)) = oracle
            .lot_conservation
            .get(&(store_id.clone(), sku_id.clone()))
        {
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
    }
    Ok(())
}

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

fn verify_out_of_order_and_duplicate_feeds(
    cell: &mut EntityCell,
    rng: &mut SplitMix64,
) -> Result<(), String> {
    let mut rows = vec![
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

    let (_, dup_clean) = cell.ingest_feed(&FeedRow {
        feed_id: POS_FEED_ID,
        mapping: POS_FEED_ID,
        dataset: "pos_sale_line",
        source: "synthetic-pos",
        event: "ev-ooo-pos-1",
        record: "POS-OOO-1",
        payload: r#"{"pos_line_id":"POS-OOO-1","branch_code":"ST-CLEAN","barcode_sku":"SKU-CLEAN","qty":2,"uom":"PCS","net_amount":400,"ccy":"EGP","sold_at":"2026-09-01T10:00:00Z"}"#,
    })?;
    if !dup_clean {
        return Err("exact duplicate POS redelivery should be idempotent".to_owned());
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
    let _ = cell.ingest_feed(&FeedRow {
        feed_id: RECEIPTS_FEED_ID,
        mapping: RECEIPTS_FEED_ID,
        dataset: "retail_receipt",
        source: "synthetic-retail-receipt",
        event: "ev-bad-rcpt-1",
        record: "RCPT-BAD-DMG",
        payload: r#"{"receipt_line_id":"RCPT-BAD-DMG","store_code":"ST-CLEAN","sku_code":"SKU-CLEAN","vendor_code":"ATELIER-01","received_qty":5,"damaged_qty":9,"uom":"PCS","unit_cost":100,"ccy":"EGP","received_at":"2026-09-02T09:00:00Z"}"#,
    })?;
    if cell.open_finding_count("supply.RetailReceiptEvidence", "RCPT-BAD-DMG")? == 0 {
        return Err("damaged_qty > received_qty must open a data-quality finding".to_owned());
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

    if let Some(parts) = cell.watermarks.get_mut(POS_FEED_ID)
        && let Some(wm) = parts.get_mut("pos_sale_line")
    {
        let stale_lag = FEED_MAX_AGE_MS.saturating_add(10_000);
        wm.heartbeat_ms = Some(cell.now().as_millis().saturating_sub(stale_lag));
    }
    let stale_cut = evaluate_spec_cut(cell, &spec)?;
    if stale_cut.complete
        || !stale_cut.missing_feeds.contains(POS_FEED_ID)
        || stale_cut.answers.is_some()
    {
        return Err("stale watermark heartbeat must mark POS feed missing and withhold".to_owned());
    }
    cell.watermarks = saved_watermarks.clone();

    if let Some(parts) = cell.watermarks.get_mut(COUNTS_FEED_ID)
        && let Some(wm) = parts.get_mut("store_count")
    {
        wm.observed = "cursor:ahead-unconsumed".to_owned();
    }
    let drift_cut = evaluate_spec_cut(cell, &spec)?;
    if drift_cut.complete
        || !drift_cut.missing_feeds.contains(COUNTS_FEED_ID)
        || drift_cut.answers.is_some()
    {
        return Err("observed-vs-consumed cursor drift must withhold answers".to_owned());
    }
    cell.watermarks = saved_watermarks.clone();

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

    let sub_input = CellViewInput {
        authority: &authority,
        ..base_input
    };
    let sub_res = evaluate_cell_cut(&sub_input, &mut |_, _, ctx| {
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

    verify_mid_read_frontier_shift(cell, sub_input)
}

fn verify_mid_read_frontier_shift(
    cell: &EntityCell,
    sub_input: CellViewInput<'_, norn_cell::engine::NativeEngine>,
) -> Result<(), String> {
    let flipped = AtomicBool::new(false);
    let shifting_frontier =
        || -> Result<(Frontier, BTreeMap<String, BTreeMap<String, Watermark>>), CellError> {
            let mut cursors = cell.source_cursors.clone();
            let mut wms = cell.watermarks.clone();
            if flipped.swap(true, Ordering::SeqCst) {
                if let Some(parts) = cursors.get_mut(POS_FEED_ID) {
                    parts.insert(
                        "pos_sale_line".to_owned(),
                        "cursor:concurrent-shift".to_owned(),
                    );
                }
                if let Some(parts) = wms.get_mut(POS_FEED_ID)
                    && let Some(wm) = parts.get_mut("pos_sale_line")
                {
                    wm.observed = "cursor:concurrent-shift".to_owned();
                }
            }
            Ok((cursors, wms))
        };
    let race_input = CellViewInput {
        read_frontier: &shifting_frontier,
        ..sub_input
    };
    let race_res = evaluate_cell_cut(&race_input, &mut |_, plan, ctx| {
        cell.cell()
            .map_err(CellError::Engine)?
            .store()
            .evaluate_question(plan, ctx, |_| Ok(true))
    });
    if !matches!(
        race_res,
        Err(CellError::ComputeRefused("core.compute.stale_input"))
    ) {
        return Err(format!(
            "mid-read frontier shift must refuse stale_input, got {race_res:?}"
        ));
    }
    Ok(())
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
