//! Layer 2A — Seeded Multi-Cell Event-Log Simulator.
//!
//! Drives 16–52 simulated weeks of concurrent/interleaved POS sales, supplier
//! receipts, physical cycle counts, durable late-arriving cost restatements
//! (`supply.unit_cost.record` + restated lots updating active costs), bilateral
//! inter-entity stock transfers (real seller sale + buyer receipt commits with
//! verified `main` [`norn_cell::Receipt`]s), and pinned reporting-view FX shifts
//! evaluated over real multi-cell combined views, verifying every cell against
//! an independent exact arithmetic oracle across the FULL simulated horizon.

use std::collections::{BTreeMap, BTreeSet};

use norn_cell::CellError;
use serde_json::json;

use super::cuts::{
    EntityOracle, cell_id_for_entity, dst_coverage_spec, evaluate_multi_cell_report,
    verify_cell_matches_oracle,
};
use super::env::{COSTS_FEED_ID, EntityCell, FeedRow};
use super::operating_group::{
    EntityReportingSlice, FxRateEntry, InterEntityTransfer, OperatingGroupTier,
    evaluate_operating_group_tier, slice_from_cut_snapshot,
};
use super::prng::{
    SplitMix64, rem_u32, rem_usize, scaled, week_epoch_ms, week_monday_iso, week_timestamp_utc,
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

/// Complete result of a multi-cell seeded simulation run.
pub struct SimulatedWorld {
    pub cells: Vec<EntityCell>,
    pub oracles: Vec<EntityOracle>,
    /// Pinned reporting-view FX rate snapshots (evaluated over real multi-cell
    /// combined-view cuts; `supply-lab` has no `FxRate` cell entity yet).
    pub fx_history: Vec<FxRateEntry>,
    /// Bilateral inter-entity transfers backed by real seller & buyer cell
    /// commits and cryptographically verified `main` [`norn_cell::Receipt`]s.
    pub inter_entity_transfers: Vec<InterEntityTransfer>,
}

const CURRENCIES: [&str; 3] = ["EGP", "USD", "EUR"];
const ENTITY_KINDS: [&str; 3] = ["sole_proprietorship", "partnership", "corporation"];

struct ActiveSkuCost {
    sku_id: String,
    cost_ref: String,
    unit_cost: i64,
    restated: bool,
}

struct LotTracker {
    lot_id: String,
    received: i64,
    remaining: i64,
    unit_cost: i64,
}

struct SimState {
    cells: Vec<EntityCell>,
    oracles: Vec<EntityOracle>,
    active_costs: BTreeMap<(usize, usize), ActiveSkuCost>,
    lot_state: BTreeMap<(usize, usize, String), LotTracker>,
    counted_once: BTreeSet<(usize, usize, String)>,
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
    for entity_idx in 0..config.entity_count {
        let (cell, oracle) = init_one_sim_entity(
            config,
            entity_idx,
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
    entity_idx: usize,
    end_clock_ms: u64,
    rng: &mut SplitMix64,
    active_costs: &mut BTreeMap<(usize, usize), ActiveSkuCost>,
    lot_state: &mut BTreeMap<(usize, usize, String), LotTracker>,
) -> Result<(EntityCell, EntityOracle), String> {
    let ccy = CURRENCIES
        .get(rem_usize(entity_idx, CURRENCIES.len()))
        .copied()
        .unwrap_or("EGP");
    let kind = ENTITY_KINDS
        .get(rem_usize(entity_idx, ENTITY_KINDS.len()))
        .copied()
        .unwrap_or("sole_proprietorship");
    let mut cell = EntityCell::open(
        &cell_id_for_entity(config.seed, entity_idx),
        &format!("LE-SIM-{entity_idx}"),
        ccy,
        &format!("org_sim_{entity_idx}"),
        "prod",
        end_clock_ms,
    )?;
    cell.register_book(&format!("Sim Entity {entity_idx}"), kind)?;
    register_sim_stores(&mut cell, entity_idx, config.stores_per_entity)?;
    let mut oracle = EntityOracle::default();
    for sku_idx in 0..config.skus_per_entity {
        seed_initial_sim_sku(
            &mut cell,
            &mut oracle,
            config.stores_per_entity,
            entity_idx,
            sku_idx,
            rng,
            active_costs,
            lot_state,
        )?;
    }
    Ok((cell, oracle))
}

fn register_sim_stores(
    cell: &mut EntityCell,
    entity_idx: usize,
    stores_per_entity: usize,
) -> Result<(), String> {
    for store_idx in 0..stores_per_entity {
        let store_id = format!("ST-{entity_idx}-{store_idx}");
        let mall = (store_idx == 0).then_some("MALL-CFC");
        cell.register_store(
            &store_id,
            &format!("Store {entity_idx}-{store_idx}"),
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
    Ok(())
}

#[allow(
    clippy::too_many_arguments,
    reason = "initial simulator SKU and stock lot seeding"
)]
fn seed_initial_sim_sku(
    cell: &mut EntityCell,
    oracle: &mut EntityOracle,
    stores_per_entity: usize,
    entity_idx: usize,
    sku_idx: usize,
    rng: &mut SplitMix64,
    active_costs: &mut BTreeMap<(usize, usize), ActiveSkuCost>,
    lot_state: &mut BTreeMap<(usize, usize, String), LotTracker>,
) -> Result<(), String> {
    let sku_id = format!("SKU-{entity_idx}-{sku_idx}");
    let cost_ref = format!("COST-{entity_idx}-{sku_idx}-W0");
    let cost_val = i64::try_from(rng.range_u64(40, 220)).unwrap_or(100);
    cell.register_sku(&sku_id, "dry_goods", "each")?;
    cell.record_unit_cost(
        &sku_id,
        &cost_ref,
        &format!("SUP-{}", rem_usize(sku_idx, 3)),
        &cost_val.to_string(),
        "2026-06-01T00:00:00Z",
    )?;
    active_costs.insert(
        (entity_idx, sku_idx),
        ActiveSkuCost {
            sku_id: sku_id.clone(),
            cost_ref,
            unit_cost: cost_val,
            restated: false,
        },
    );
    for store_idx in 0..stores_per_entity {
        let store_id = format!("ST-{entity_idx}-{store_idx}");
        let lot_id = format!("LOT-{entity_idx}-{store_idx}-{sku_idx}-INIT");
        let initial_qty = i64::try_from(rng.range_u64(400, 900)).unwrap_or(500);
        cell.record_costed_lot(
            &lot_id,
            &store_id,
            &sku_id,
            if store_idx == 0 {
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
            (entity_idx, store_idx, sku_id.clone()),
            LotTracker {
                lot_id,
                received: initial_qty,
                remaining: initial_qty,
                unit_cost: cost_val,
            },
        );
        oracle
            .lot_conservation
            .insert((store_id, sku_id.clone()), (initial_qty, 0));
        oracle.total_receipts = oracle.total_receipts.saturating_add(1);
    }
    Ok(())
}

fn simulate_one_week(
    config: SimConfig,
    week: u32,
    state: &mut SimState,
    rng: &mut SplitMix64,
) -> Result<(), String> {
    let week_monday = week_monday_iso(week)?;
    if rem_u32(week, 4) == 0 {
        let base_rate = i128::from(rng.range_u64(47_500_000, 51_500_000));
        state.fx_history.push(FxRateEntry {
            from_currency: "USD".to_owned(),
            to_currency: "EGP".to_owned(),
            rate_scaled: base_rate,
            as_of: week_timestamp_utc(week, 2, 12)?,
            rate_table_pin: format!("fx:sim:{}:w{week}", config.seed),
        });
        state.fx_history.push(FxRateEntry {
            from_currency: "EUR".to_owned(),
            to_currency: "EGP".to_owned(),
            rate_scaled: base_rate.saturating_add(3_500_000),
            as_of: week_timestamp_utc(week, 2, 12)?,
            rate_table_pin: format!("fx:sim:{}:eur:w{week}", config.seed),
        });
    }
    if config.entity_count >= 2 && rem_u32(week, 8) == 3 {
        commit_bilateral_inter_entity_transfer(config, week, &week_monday, state, rng)?;
    }
    for entity_idx in 0..config.entity_count {
        simulate_entity_week_step(config, week, &week_monday, entity_idx, state, rng)?;
    }
    Ok(())
}

/// Commit a REAL bilateral inter-entity transfer across seller cell `1` (USD)
/// and buyer cell `0` (EGP), updating both cells' durable state & oracles and
/// binding verified `main` [`norn_cell::Receipt`]s into `state.inter_entity_transfers`.
fn commit_bilateral_inter_entity_transfer(
    config: SimConfig,
    week: u32,
    week_monday: &str,
    state: &mut SimState,
    rng: &mut SplitMix64,
) -> Result<(), String> {
    let qty = i64::try_from(rng.range_u64(2, 5)).unwrap_or(2);
    let markup = i64::try_from(rng.range_u64(15, 45)).unwrap_or(20);
    let ts = week_timestamp_utc(week, 3, 11)?;

    let (seller_ccy, sale_rev, seller_receipt) =
        commit_transfer_seller_leg(week, week_monday, qty, markup, &ts, state)?;
    let buyer_receipt = commit_transfer_buyer_leg(week, qty, &ts, state)?;

    let xfer_amount = scaled(i128::from(sale_rev));
    state.inter_entity_transfers.push(InterEntityTransfer {
        transfer_id: format!("IE-SIM-{}-W{week}", config.seed),
        seller_entity_id: "LE-SIM-1".to_owned(),
        buyer_entity_id: "LE-SIM-0".to_owned(),
        currency: seller_ccy,
        seller_amount_scaled: xfer_amount,
        buyer_amount_scaled: xfer_amount,
        seller_receipt: Some(seller_receipt),
        buyer_receipt: Some(buyer_receipt),
    });
    Ok(())
}

fn commit_transfer_seller_leg(
    week: u32,
    week_monday: &str,
    qty: i64,
    markup: i64,
    ts: &str,
    state: &mut SimState,
) -> Result<(String, i64, norn_cell::Receipt), String> {
    let seller_idx = 1_usize;
    let seller_store = "ST-1-0";
    let (seller_sku, seller_cost_ref, seller_cost) = state
        .active_costs
        .get(&(seller_idx, 0))
        .map(|a| (a.sku_id.clone(), a.cost_ref.clone(), a.unit_cost))
        .ok_or("missing seller active cost")?;
    let sale_rev = qty.saturating_mul(seller_cost.saturating_add(markup));
    let sale_gm = qty.saturating_mul(markup);
    let seller_ccy = state
        .cells
        .get(seller_idx)
        .map(|c| c.currency.clone())
        .ok_or("missing seller cell")?;
    let seller_receipt = {
        let seller = state.cells.get_mut(seller_idx).ok_or("missing seller")?;
        seller.record_costed_sale(
            &format!("SALE-IE-W{week}"),
            seller_store,
            &seller_sku,
            &qty.to_string(),
            "each",
            &sale_rev.to_string(),
            &seller_cost.to_string(),
            &seller_cost_ref,
            ts,
        )?;
        let commit_seq = seller.head()?.commits;
        seller.export_main_receipt(commit_seq)?
    };
    deplete_lot_for_sale(week, seller_idx, 0, seller_store, &seller_sku, qty, state)?;
    if let Some(oracle) = state.oracles.get_mut(seller_idx) {
        record_oracle_sale(
            oracle,
            &seller_ccy,
            seller_store,
            week_monday,
            sale_rev,
            sale_gm,
        )?;
    }
    Ok((seller_ccy, sale_rev, seller_receipt))
}

fn commit_transfer_buyer_leg(
    week: u32,
    qty: i64,
    ts: &str,
    state: &mut SimState,
) -> Result<norn_cell::Receipt, String> {
    let buyer_idx = 0_usize;
    let buyer_store = "ST-0-0";
    let (buyer_sku, buyer_cost) = state
        .active_costs
        .get(&(buyer_idx, 0))
        .map(|a| (a.sku_id.clone(), a.unit_cost))
        .ok_or("missing buyer active cost")?;
    let buyer_receipt = {
        let buyer = state.cells.get_mut(buyer_idx).ok_or("missing buyer")?;
        buyer.record_costed_lot(
            &format!("LOT-IE-W{week}"),
            buyer_store,
            &buyer_sku,
            "CUST-MALL-CFC-OPS",
            &qty.to_string(),
            "each",
            &buyer_cost.to_string(),
            ts,
        )?;
        let commit_seq = buyer.head()?.commits;
        buyer.export_main_receipt(commit_seq)?
    };
    if let Some(oracle) = state.oracles.get_mut(buyer_idx) {
        let entry = oracle
            .lot_conservation
            .entry((buyer_store.to_owned(), buyer_sku))
            .or_insert((0, 0));
        entry.0 = entry.0.saturating_add(qty);
        oracle.total_receipts = oracle.total_receipts.saturating_add(1);
    }
    Ok(buyer_receipt)
}

struct SimulatedSale {
    sku_id: String,
    sold_qty: i64,
    net_rev: i64,
    gross_margin: i64,
    restated: bool,
    seq: u64,
}

fn simulate_entity_week_step(
    config: SimConfig,
    week: u32,
    week_monday: &str,
    entity_idx: usize,
    state: &mut SimState,
    rng: &mut SplitMix64,
) -> Result<(), String> {
    let store_idx = rng.index(config.stores_per_entity);
    let sku_idx = rng.index(config.skus_per_entity);
    let store_id = format!("ST-{entity_idx}-{store_idx}");

    maybe_commit_cost_restatement(config, week, entity_idx, sku_idx, state, rng)?;
    let sale = record_simulated_sale(week, entity_idx, sku_idx, &store_id, state, rng)?;
    deplete_lot_for_sale(
        week,
        entity_idx,
        store_idx,
        &store_id,
        &sale.sku_id,
        sale.sold_qty,
        state,
    )?;
    maybe_recount_store(
        week,
        entity_idx,
        store_idx,
        &store_id,
        &sale.sku_id,
        sale.seq,
        state,
    )?;

    let cell_ccy = state
        .cells
        .get(entity_idx)
        .map(|c| c.currency.clone())
        .ok_or("missing entity cell")?;
    if let Some(oracle) = state.oracles.get_mut(entity_idx) {
        record_oracle_sale(
            oracle,
            &cell_ccy,
            &store_id,
            week_monday,
            sale.net_rev,
            sale.gross_margin,
        )?;
        if sale.restated {
            oracle.restated_sales = oracle.restated_sales.saturating_add(1);
        }
    }
    Ok(())
}

fn record_oracle_sale(
    oracle: &mut EntityOracle,
    ccy: &str,
    store_id: &str,
    week_monday: &str,
    net_rev: i64,
    gross_margin: i64,
) -> Result<(), String> {
    let key = (ccy.to_owned(), store_id.to_owned(), week_monday.to_owned());
    let rev_entry = oracle.weekly_revenue.entry(key.clone()).or_insert(0);
    *rev_entry = rev_entry
        .checked_add(scaled(i128::from(net_rev)))
        .ok_or("oracle revenue overflow")?;
    let gm_entry = oracle.weekly_costed_margin.entry(key).or_insert(0);
    *gm_entry = gm_entry
        .checked_add(scaled(i128::from(gross_margin)))
        .ok_or("oracle margin overflow")?;
    oracle.total_sales = oracle.total_sales.saturating_add(1);
    Ok(())
}

/// Drive a late-arriving cost restatement through REAL durable writes:
/// ingest `UnitCostEvidence`, register restated SKU + `supply.unit_cost.record`,
/// receive costed `StockLot`s at the restated unit cost, and switch active cost.
fn maybe_commit_cost_restatement(
    config: SimConfig,
    week: u32,
    entity_idx: usize,
    sku_idx: usize,
    state: &mut SimState,
    rng: &mut SplitMix64,
) -> Result<(), String> {
    if week == 0 || rem_u32(week, 6) != 0 {
        return Ok(());
    }
    let old_cost = state
        .active_costs
        .get(&(entity_idx, sku_idx))
        .map_or(100, |a| a.unit_cost);
    let delta = i64::try_from(rng.range_u64(10, 45)).unwrap_or(15);
    let restated_cost = old_cost.saturating_add(delta);
    let restated_sku = format!("SKU-{entity_idx}-{sku_idx}-R{week}");
    let restated_ref = format!("COST-RESTATE-{entity_idx}-{sku_idx}-W{week}");
    let eff_ts = week_timestamp_utc(week, 0, 0)?;

    commit_restated_cost_header(
        state,
        entity_idx,
        sku_idx,
        week,
        &restated_sku,
        &restated_ref,
        restated_cost,
        &eff_ts,
    )?;
    seed_restated_lots(
        state,
        config.stores_per_entity,
        entity_idx,
        sku_idx,
        week,
        &restated_sku,
        restated_cost,
        &eff_ts,
        rng,
    )?;
    state.active_costs.insert(
        (entity_idx, sku_idx),
        ActiveSkuCost {
            sku_id: restated_sku,
            cost_ref: restated_ref,
            unit_cost: restated_cost,
            restated: true,
        },
    );
    if let Some(oracle) = state.oracles.get_mut(entity_idx) {
        oracle.total_cost_restatements = oracle.total_cost_restatements.saturating_add(1);
    }
    Ok(())
}

#[allow(
    clippy::too_many_arguments,
    reason = "restated cost feed + governed unit cost commit"
)]
fn commit_restated_cost_header(
    state: &mut SimState,
    entity_idx: usize,
    sku_idx: usize,
    week: u32,
    restated_sku: &str,
    restated_ref: &str,
    restated_cost: i64,
    eff_ts: &str,
) -> Result<(), String> {
    let cell = state
        .cells
        .get_mut(entity_idx)
        .ok_or("missing entity cell")?;
    let payload = json!({
        "cost_id": restated_ref,
        "sku_code": restated_sku,
        "vendor_code": "ATELIER-01",
        "unit_cost": restated_cost,
        "ccy": cell.currency,
        "effective_from": eff_ts,
    })
    .to_string();
    let (_, clean) = cell.ingest_feed(&FeedRow {
        feed_id: COSTS_FEED_ID,
        mapping: COSTS_FEED_ID,
        dataset: "unit_cost",
        source: "synthetic-cost",
        event: &format!("ev-cost-restate-{entity_idx}-{sku_idx}-w{week}"),
        record: restated_ref,
        payload: &payload,
    })?;
    if !clean {
        return Err(format!("cost restatement w{week} quarantined unexpectedly"));
    }
    cell.register_sku(restated_sku, "dry_goods", "each")?;
    cell.record_unit_cost(
        restated_sku,
        restated_ref,
        "ATELIER-01",
        &restated_cost.to_string(),
        eff_ts,
    )
}

#[allow(
    clippy::too_many_arguments,
    reason = "restated cost stock lots across entity stores"
)]
fn seed_restated_lots(
    state: &mut SimState,
    stores_per_entity: usize,
    entity_idx: usize,
    sku_idx: usize,
    week: u32,
    restated_sku: &str,
    restated_cost: i64,
    eff_ts: &str,
    rng: &mut SplitMix64,
) -> Result<(), String> {
    let cell = state
        .cells
        .get_mut(entity_idx)
        .ok_or("missing entity cell")?;
    for store_idx in 0..stores_per_entity {
        let store_id = format!("ST-{entity_idx}-{store_idx}");
        let lot_id = format!("LOT-{entity_idx}-{store_idx}-{sku_idx}-R{week}");
        let qty = i64::try_from(rng.range_u64(250, 500)).unwrap_or(300);
        cell.record_costed_lot(
            &lot_id,
            &store_id,
            restated_sku,
            if store_idx == 0 {
                "CUST-MALL-CFC-OPS"
            } else {
                "CUST-BRANCH-OPS"
            },
            &qty.to_string(),
            "each",
            &restated_cost.to_string(),
            eff_ts,
        )?;
        state.lot_state.insert(
            (entity_idx, store_idx, restated_sku.to_owned()),
            LotTracker {
                lot_id,
                received: qty,
                remaining: qty,
                unit_cost: restated_cost,
            },
        );
        if let Some(oracle) = state.oracles.get_mut(entity_idx) {
            oracle
                .lot_conservation
                .insert((store_id, restated_sku.to_owned()), (qty, 0));
            oracle.total_receipts = oracle.total_receipts.saturating_add(1);
        }
    }
    Ok(())
}

fn record_simulated_sale(
    week: u32,
    entity_idx: usize,
    sku_idx: usize,
    store_id: &str,
    state: &mut SimState,
    rng: &mut SplitMix64,
) -> Result<SimulatedSale, String> {
    let (sku_id, cost_ref, unit_cost, restated) = state
        .active_costs
        .get(&(entity_idx, sku_idx))
        .map(|a| {
            (
                a.sku_id.clone(),
                a.cost_ref.clone(),
                a.unit_cost,
                a.restated,
            )
        })
        .ok_or("missing active cost")?;
    let sold_qty = i64::try_from(rng.range_u64(1, 6)).unwrap_or(2);
    let margin_per_unit = i64::try_from(rng.range_u64(20, 120)).unwrap_or(40);
    let net_rev = sold_qty.saturating_mul(unit_cost.saturating_add(margin_per_unit));
    let gross_margin = sold_qty.saturating_mul(margin_per_unit);
    state.seq = state.seq.saturating_add(1);
    let seq = state.seq;
    let sold_at = week_timestamp_utc(week, u32::try_from(rng.range_u64(0, 5)).unwrap_or(1), 14)?;
    if let Some(cell) = state.cells.get_mut(entity_idx) {
        cell.record_costed_sale(
            &format!("SALE-{entity_idx}-W{week}-{seq}"),
            store_id,
            &sku_id,
            &sold_qty.to_string(),
            "each",
            &net_rev.to_string(),
            &unit_cost.to_string(),
            &cost_ref,
            &sold_at,
        )?;
    }
    Ok(SimulatedSale {
        sku_id,
        sold_qty,
        net_rev,
        gross_margin,
        restated,
        seq,
    })
}

fn deplete_lot_for_sale(
    week: u32,
    entity_idx: usize,
    store_idx: usize,
    store_id: &str,
    sku_id: &str,
    sold_qty: i64,
    state: &mut SimState,
) -> Result<(), String> {
    if let Some(lot) = state
        .lot_state
        .get_mut(&(entity_idx, store_idx, sku_id.to_owned()))
        && lot.remaining >= sold_qty
    {
        lot.remaining = lot.remaining.saturating_sub(sold_qty);
        let depleted = lot.received.saturating_sub(lot.remaining);
        if let Some(cell) = state.cells.get_mut(entity_idx) {
            cell.update_lot_balance(
                &lot.lot_id,
                store_id,
                sku_id,
                &lot.remaining.to_string(),
                &depleted.to_string(),
                &lot.unit_cost.to_string(),
                &week_timestamp_utc(week, 5, 18)?,
            )?;
        }
        if let Some(oracle) = state.oracles.get_mut(entity_idx) {
            let entry = oracle
                .lot_conservation
                .entry((store_id.to_owned(), sku_id.to_owned()))
                .or_insert((lot.received, 0));
            entry.1 = entry.1.saturating_add(sold_qty);
        }
    }
    Ok(())
}

fn maybe_recount_store(
    week: u32,
    entity_idx: usize,
    store_idx: usize,
    store_id: &str,
    sku_id: &str,
    seq: u64,
    state: &mut SimState,
) -> Result<(), String> {
    if rem_u32(week, 4) == 2
        && let Some(lot) = state
            .lot_state
            .get(&(entity_idx, store_idx, sku_id.to_owned()))
    {
        let recount = !state
            .counted_once
            .insert((entity_idx, store_idx, sku_id.to_owned()));
        if let Some(cell) = state.cells.get_mut(entity_idx) {
            cell.record_store_count(
                recount,
                &format!("{store_id}/{sku_id}/on_hand"),
                store_id,
                sku_id,
                &lot.remaining.to_string(),
                "each",
                "on_hand",
                &week_timestamp_utc(week, 6, 10)?,
                &format!("CC-W{week}-{seq}"),
            )?;
        }
        if let Some(oracle) = state.oracles.get_mut(entity_idx) {
            oracle.total_recounts = oracle.total_recounts.saturating_add(1);
        }
    }
    Ok(())
}

fn finalize_and_verify_sim(config: SimConfig, state: &mut SimState) -> Result<(), String> {
    let mut any_restated = false;
    let mut any_restated_sale = false;
    for entity_idx in 0..config.entity_count {
        let store_id = format!("ST-{entity_idx}-0");
        let (sku_id, cost_val) = state.active_costs.get(&(entity_idx, 0)).map_or_else(
            || (format!("SKU-{entity_idx}-0"), 100),
            |a| (a.sku_id.clone(), a.unit_cost),
        );
        if let Some(cell) = state.cells.get_mut(entity_idx) {
            cell.ingest_baseline_feeds(
                &store_id,
                &sku_id,
                &cost_val.to_string(),
                "2026-09-02T12:00:00Z",
                &format!("sim-{entity_idx}"),
            )?;
            cell.refresh_watermarks();
            let oracle = state.oracles.get(entity_idx).ok_or("missing oracle")?;
            if oracle.total_cost_restatements > 0 {
                any_restated = true;
            }
            if oracle.restated_sales > 0 {
                any_restated_sale = true;
            }
            verify_cell_matches_oracle(cell, oracle, config.weeks)?;
        }
    }
    if config.weeks >= 12 && (!any_restated || !any_restated_sale) {
        return Err(
            "simulator must commit durable cost restatements and apply them to subsequent sales"
                .to_owned(),
        );
    }
    verify_multi_cell_combined_view_and_fx_shifts(state)
}

fn verify_multi_cell_combined_view_and_fx_shifts(state: &SimState) -> Result<(), String> {
    let spec = dst_coverage_spec();
    let refs: Vec<&EntityCell> = state.cells.iter().collect();
    let report = evaluate_multi_cell_report(&refs, &spec, |idx, _, plan, ctx| {
        let book = refs
            .get(idx)
            .ok_or_else(|| CellError::Engine("missing cell".to_owned()))?;
        book.cell()
            .map_err(CellError::Engine)?
            .store()
            .evaluate_question(plan, ctx, |_| Ok(true))
    })?;
    let mut slices: Vec<EntityReportingSlice> = Vec::with_capacity(state.cells.len());
    for (idx, (cell, snap)) in state.cells.iter().zip(&report.cells).enumerate() {
        let oracle = state.oracles.get(idx).ok_or("missing oracle")?;
        let total_rev = oracle
            .weekly_revenue
            .values()
            .try_fold(0_i128, |acc, v| acc.checked_add(*v))
            .ok_or("slice revenue sum overflow")?;
        let total_gm = oracle
            .weekly_costed_margin
            .values()
            .try_fold(0_i128, |acc, v| acc.checked_add(*v))
            .ok_or("slice margin sum overflow")?;
        slices.push(slice_from_cut_snapshot(
            &cell.entity_id,
            &cell.currency,
            snap,
            "supply.retail.store_revenue_weekly",
            total_rev,
            total_gm,
        )?);
    }
    let first_rates: Vec<FxRateEntry> = state.fx_history.iter().take(2).cloned().collect();
    let latest_rates: Vec<FxRateEntry> = state.fx_history.iter().rev().take(2).cloned().collect();
    let early_fx = evaluate_operating_group_tier(
        OperatingGroupTier::FxTranslated,
        &slices,
        Some("EGP"),
        &first_rates,
        &[],
        None,
    )
    .map_err(|e| format!("early FxTranslated projection failed: {e:?}"))?;
    let netted = evaluate_operating_group_tier(
        OperatingGroupTier::InterEntityNetted,
        &slices,
        Some("EGP"),
        &latest_rates,
        &state.inter_entity_transfers,
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
