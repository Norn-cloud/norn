//! Layer 2A — Seeded Multi-Cell Event-Log Simulator.
//!
//! Drives 16–52 simulated weeks of concurrent/interleaved POS sales, supplier
//! receipts, physical cycle counts, genuine **in-place** restatements of
//! existing committed facts (`supply.position.restate` `update_only` on the
//! exact same `InventorySnapshot` object ID verified bitemporally via
//! `evaluate_question_known_at`, plus in-place `UnitCostEvidence` updates on
//! the same `cost_id` and activation of restated `cost_ref`/`unit_cost` routed
//! into subsequent sales), bilateral inter-entity stock transfers (real seller
//! `PosSale` + buyer `StockLot` commits bound via shared transfer correlation
//! IDs and verified `main` [`norn_cell::export::ExportRecord`]s), and pinned
//! reporting-view FX shifts evaluated over real multi-cell combined views.

use std::collections::{BTreeMap, BTreeSet};

use norn_cell::KnowledgeCutoff;
use norn_cell::compute::QueryContext;
use norn_checker::metric::compute::QuestionPlan;
use serde_json::json;

use super::cuts::{
    EntityOracle, bucket_decimal_exact, cell_id_for_entity, verify_cell_matches_oracle,
    verify_multi_cell_combined_view_and_fx_shifts,
};
use super::env::{COSTS_FEED_ID, EntityCell, FeedRow};
use super::layer2_faults::verify_late_arriving_unit_cost_on_existing_sku;
use super::operating_group::{
    FxRateEntry, InterEntityTransfer, TransferLegEvidence, transfer_leg_key, transfer_lot_id,
    transfer_sale_id,
};
use super::prng::{
    SplitMix64, decimal_to_scaled, rem_u32, rem_usize, scaled, week_epoch_ms, week_monday_iso,
    week_timestamp_utc,
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
    /// commits and cryptographically verified `main` export records.
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
    snapshot_qty: BTreeMap<(usize, usize), i64>,
    restated_late_sku: BTreeSet<usize>,
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
    verify_late_arriving_unit_cost_on_existing_sku(config.seed)?;
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
        snapshot_qty: BTreeMap::new(),
        restated_late_sku: BTreeSet::new(),
        counted_once: BTreeSet::new(),
        fx_history: Vec::new(),
        inter_entity_transfers: Vec::new(),
        seq: 0,
    };
    for entity_idx in 0..config.entity_count {
        init_one_sim_entity(config, entity_idx, end_clock_ms, rng, &mut state)?;
    }
    Ok(state)
}

fn init_one_sim_entity(
    config: SimConfig,
    entity_idx: usize,
    end_clock_ms: u64,
    rng: &mut SplitMix64,
    state: &mut SimState,
) -> Result<(), String> {
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
    cell.register_warehouse(
        &format!("WH-SIM-{entity_idx}"),
        &format!("Sim Warehouse {entity_idx}"),
    )?;
    for store_ordinal in 0..config.stores_per_entity {
        let store_id = format!("ST-{entity_idx}-{store_ordinal}");
        let (loc, mall) = if store_ordinal == 0 {
            ("mall_store", Some("MALL-CFC"))
        } else {
            ("standalone", None)
        };
        cell.register_store(
            &store_id,
            &format!("Store {entity_idx}-{store_ordinal}"),
            custodian_for_store(store_ordinal),
            loc,
            mall,
        )?;
    }
    let mut oracle = EntityOracle::default();
    for sku_ordinal in 0..config.skus_per_entity {
        seed_initial_sim_sku(
            &mut cell,
            &mut oracle,
            config.stores_per_entity,
            entity_idx,
            sku_ordinal,
            rng,
            state,
        )?;
    }
    // Pre-register a late-costed SKU with initial UnitCostEvidence (restated in-place at W6).
    let late_sku = format!("SKU-{entity_idx}-LATE");
    cell.register_sku(&late_sku, "dry_goods", "each")?;
    ingest_cost_ev_row(
        &mut cell,
        &format!("COST-EV-{entity_idx}-LATE"),
        &late_sku,
        90,
        "2026-07-01T00:00:00Z",
        &format!("ev-cost-late-init-{entity_idx}"),
    )?;
    state.cells.push(cell);
    state.oracles.push(oracle);
    Ok(())
}

fn ingest_cost_ev_row(
    cell: &mut EntityCell,
    cost_id: &str,
    sku_code: &str,
    unit_cost: i64,
    eff_ts: &str,
    event: &str,
) -> Result<(), String> {
    let payload = json!({
        "cost_id": cost_id, "sku_code": sku_code, "vendor_code": "ATELIER-01",
        "unit_cost": unit_cost, "ccy": cell.currency, "effective_from": eff_ts,
    })
    .to_string();
    let (_, clean) = cell.ingest_feed(&FeedRow {
        feed_id: COSTS_FEED_ID,
        mapping: COSTS_FEED_ID,
        dataset: "unit_cost",
        source: "synthetic-cost",
        event,
        record: cost_id,
        payload: &payload,
    })?;
    if !clean {
        return Err(format!(
            "UnitCostEvidence {cost_id} quarantined unexpectedly"
        ));
    }
    Ok(())
}

fn seed_initial_sim_sku(
    cell: &mut EntityCell,
    oracle: &mut EntityOracle,
    stores_per_entity: usize,
    entity_idx: usize,
    sku_ordinal: usize,
    rng: &mut SplitMix64,
    state: &mut SimState,
) -> Result<(), String> {
    let sku_id = format!("SKU-{entity_idx}-{sku_ordinal}");
    let cost_ref = format!("COST-{entity_idx}-{sku_ordinal}-W0");
    let cost_val = i64::try_from(rng.range_u64(40, 220)).unwrap_or(100);
    cell.register_sku(&sku_id, "dry_goods", "each")?;
    cell.record_unit_cost(
        &sku_id,
        &cost_ref,
        &format!("SUP-{}", rem_usize(sku_ordinal, 3)),
        &cost_val.to_string(),
        "2026-06-01T00:00:00Z",
    )?;
    cell.record_inventory_position(
        false,
        &format!("SNAP-{entity_idx}-{sku_ordinal}"),
        &format!("WH-SIM-{entity_idx}"),
        &sku_id,
        "200",
        "2026-07-06T08:00:00Z",
        &format!("INIT-{entity_idx}-{sku_ordinal}"),
    )?;
    state.snapshot_qty.insert((entity_idx, sku_ordinal), 200);
    state.active_costs.insert(
        (entity_idx, sku_ordinal),
        ActiveSkuCost {
            sku_id: sku_id.clone(),
            cost_ref,
            unit_cost: cost_val,
            restated: false,
        },
    );
    for store_ordinal in 0..stores_per_entity {
        let store_id = format!("ST-{entity_idx}-{store_ordinal}");
        let lot_id = format!("LOT-{entity_idx}-{store_ordinal}-{sku_ordinal}-INIT");
        let initial_qty = i64::try_from(rng.range_u64(400, 900)).unwrap_or(500);
        cell.record_costed_lot(
            &lot_id,
            &store_id,
            &sku_id,
            custodian_for_store(store_ordinal),
            &initial_qty.to_string(),
            "each",
            &cost_val.to_string(),
            "2026-07-06T08:00:00Z",
        )?;
        state.lot_state.insert(
            (entity_idx, store_ordinal, sku_id.clone()),
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
        let whole_egp_per_usd = i64::try_from(rng.range_u64(48, 52)).unwrap_or(50);
        let usd_rate = scaled(i128::from(whole_egp_per_usd));
        let as_of = week_timestamp_utc(week, 2, 12)?;
        state.fx_history.push(FxRateEntry {
            from_currency: "USD".to_owned(),
            to_currency: "EGP".to_owned(),
            rate_scaled: usd_rate,
            as_of: as_of.clone(),
            rate_table_pin: format!("fx:sim:{}:w{week}", config.seed),
        });
        state.fx_history.push(FxRateEntry {
            from_currency: "EUR".to_owned(),
            to_currency: "EGP".to_owned(),
            rate_scaled: usd_rate.saturating_add(scaled(3)),
            as_of,
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
    let spot_fx = state
        .fx_history
        .iter()
        .rev()
        .find(|entry| entry.from_currency == "USD" && entry.to_currency == "EGP")
        .cloned()
        .ok_or("missing pinned USD->EGP spot FX entry for bilateral transfer")?;
    let whole_egp_per_usd = i64::try_from(
        spot_fx
            .rate_scaled
            .checked_div(scaled(1))
            .ok_or("spot FX divide failed")?,
    )
    .map_err(|e| format!("spot FX integer rate: {e}"))?;

    let transfer_id = format!("IE-SIM-{}-W{week}", config.seed);
    let (seller_ccy, seller_unit_price, seller_leg) =
        commit_transfer_seller_leg(&transfer_id, week, week_monday, qty, markup, &ts, state)?;
    let buyer_unit_cost_egp = seller_unit_price.saturating_mul(whole_egp_per_usd);
    let buyer_leg = commit_transfer_buyer_leg(&transfer_id, qty, buyer_unit_cost_egp, &ts, state)?;

    state.inter_entity_transfers.push(InterEntityTransfer {
        transfer_id,
        seller_entity_id: "LE-SIM-1".to_owned(),
        buyer_entity_id: "LE-SIM-0".to_owned(),
        currency: seller_ccy,
        settlement_fx_rate: Some(spot_fx),
        seller_leg: Some(seller_leg),
        buyer_leg: Some(buyer_leg),
    });
    Ok(())
}

fn commit_transfer_seller_leg(
    transfer_id: &str,
    week: u32,
    week_monday: &str,
    qty: i64,
    markup: i64,
    ts: &str,
    state: &mut SimState,
) -> Result<(String, i64, TransferLegEvidence), String> {
    let seller_idx = 1_usize;
    let seller_store = "ST-1-0";
    let (seller_sku, seller_cost_ref, seller_cost) = state
        .active_costs
        .get(&(seller_idx, 0))
        .map(|active| {
            (
                active.sku_id.clone(),
                active.cost_ref.clone(),
                active.unit_cost,
            )
        })
        .ok_or("missing seller active cost")?;
    let seller_unit_price = seller_cost.saturating_add(markup);
    let sale_rev = qty.saturating_mul(seller_unit_price);
    let seller_gross_margin = qty.saturating_mul(markup);
    let sale_object_id = transfer_sale_id(transfer_id);
    let seller_ccy = state
        .cells
        .get(seller_idx)
        .map(|c| c.currency.clone())
        .ok_or("missing seller")?;
    let seller_export = {
        let seller = state.cells.get_mut(seller_idx).ok_or("missing seller")?;
        seller.record_costed_sale_keyed(
            &sale_object_id,
            seller_store,
            &seller_sku,
            &qty.to_string(),
            "each",
            &sale_rev.to_string(),
            &seller_cost.to_string(),
            &seller_cost_ref,
            ts,
            &transfer_leg_key(transfer_id, "seller"),
        )?;
        seller.export_main_record(seller.head()?.commits)?
    };
    deplete_lot_for_sale(week, seller_idx, 0, seller_store, &seller_sku, qty, state)?;
    if let Some(oracle) = state.oracles.get_mut(seller_idx) {
        record_oracle_sale(
            oracle,
            &seller_ccy,
            seller_store,
            week_monday,
            sale_rev,
            seller_gross_margin,
        )?;
    }
    Ok((
        seller_ccy,
        seller_unit_price,
        TransferLegEvidence {
            export_record: seller_export,
            object_type: "supply.PosSale".to_owned(),
            object_id: sale_object_id,
        },
    ))
}

fn commit_transfer_buyer_leg(
    transfer_id: &str,
    qty: i64,
    buyer_unit_cost_egp: i64,
    ts: &str,
    state: &mut SimState,
) -> Result<TransferLegEvidence, String> {
    let buyer_idx = 0_usize;
    let buyer_store = "ST-0-0";
    let buyer_sku = state
        .active_costs
        .get(&(buyer_idx, 0))
        .map(|active| active.sku_id.clone())
        .ok_or("missing buyer active SKU")?;
    let lot_id = transfer_lot_id(transfer_id);
    let buyer_export = {
        let buyer = state.cells.get_mut(buyer_idx).ok_or("missing buyer")?;
        buyer.record_costed_lot_keyed(
            &lot_id,
            buyer_store,
            &buyer_sku,
            "CUST-MALL-CFC-OPS",
            &qty.to_string(),
            "each",
            &buyer_unit_cost_egp.to_string(),
            ts,
            &transfer_leg_key(transfer_id, "buyer"),
        )?;
        buyer.export_main_record(buyer.head()?.commits)?
    };
    if let Some(oracle) = state.oracles.get_mut(buyer_idx) {
        let entry = oracle
            .lot_conservation
            .entry((buyer_store.to_owned(), buyer_sku))
            .or_insert((0, 0));
        entry.0 = entry.0.saturating_add(qty);
        oracle.total_receipts = oracle.total_receipts.saturating_add(1);
    }
    Ok(TransferLegEvidence {
        export_record: buyer_export,
        object_type: "supply.StockLot".to_owned(),
        object_id: lot_id,
    })
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
    let store_ordinal = rng.index(config.stores_per_entity);
    let sku_ordinal = rng.index(config.skus_per_entity);
    let store_id = format!("ST-{entity_idx}-{store_ordinal}");

    maybe_commit_in_place_restatement(config, week, entity_idx, sku_ordinal, state, rng)?;
    let sale = record_simulated_sale(week, entity_idx, sku_ordinal, &store_id, state, rng)?;
    deplete_lot_for_sale(
        week,
        entity_idx,
        store_ordinal,
        &store_id,
        &sale.sku_id,
        sale.sold_qty,
        state,
    )?;
    maybe_recount_store(
        week,
        entity_idx,
        store_ordinal,
        &store_id,
        &sale.sku_id,
        sale.seq,
        state,
    )?;

    let cell_ccy = state
        .cells
        .get(entity_idx)
        .map(|c| c.currency.clone())
        .ok_or("missing cell")?;
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
        .ok_or("oracle rev overflow")?;
    let gm_entry = oracle.weekly_costed_margin.entry(key).or_insert(0);
    *gm_entry = gm_entry
        .checked_add(scaled(i128::from(gross_margin)))
        .ok_or("oracle gm overflow")?;
    oracle.total_sales = oracle.total_sales.saturating_add(1);
    Ok(())
}

/// Genuine in-place restatement of an EXISTING committed fact (`update_only`
/// `supply.position.restate` on the exact same `InventorySnapshot` object ID),
/// verified bitemporally via `evaluate_question_known_at` (prior knowledge cut
/// preserves the original quantity; live cut reflects the restated quantity),
/// plus in-place `UnitCostEvidence` restatement on the same `cost_id` and
/// routing of subsequent sales to the restated `cost_ref`/`unit_cost`.
fn maybe_commit_in_place_restatement(
    config: SimConfig,
    week: u32,
    entity_idx: usize,
    sku_ordinal: usize,
    state: &mut SimState,
    rng: &mut SplitMix64,
) -> Result<(), String> {
    if week == 0 || rem_u32(week, 6) != 0 {
        return Ok(());
    }
    restate_warehouse_snapshot_bitemporal(week, entity_idx, sku_ordinal, state)?;
    restate_late_sku_cost(config, week, entity_idx, sku_ordinal, state, rng)?;
    if let Some(oracle) = state.oracles.get_mut(entity_idx) {
        oracle.total_cost_restatements = oracle.total_cost_restatements.saturating_add(1);
    }
    Ok(())
}

fn restate_warehouse_snapshot_bitemporal(
    week: u32,
    entity_idx: usize,
    sku_ordinal: usize,
    state: &mut SimState,
) -> Result<(), String> {
    let prior_snap_qty = state
        .snapshot_qty
        .get(&(entity_idx, sku_ordinal))
        .copied()
        .ok_or("missing existing InventorySnapshot quantity")?;
    let restated_snap_qty = if prior_snap_qty >= 40 {
        prior_snap_qty.saturating_sub(15)
    } else {
        prior_snap_qty.saturating_add(25)
    };
    let delta_scaled = scaled(i128::from(restated_snap_qty.saturating_sub(prior_snap_qty)));
    let warehouse_id = format!("WH-SIM-{entity_idx}");
    let (prior_cutoff, prior_ctx, prior_total_scaled, on_hand_plan) = {
        let cell = state.cells.get(entity_idx).ok_or("missing entity cell")?;
        let cutoff = KnowledgeCutoff {
            recorded_at: cell.now(),
            frontier: cell.head()?.commits,
        };
        let ctx = cell.query_context();
        let plan = cell.pack.supply_question("supply.on_hand_weekly")?;
        let prior_total =
            eval_wh_on_hand_scaled(cell, &plan, ctx.clone(), Some(cutoff), &warehouse_id)?;
        (cutoff, ctx, prior_total, plan)
    };
    if let Some(cell) = state.cells.get_mut(entity_idx) {
        let restate_as_of = format!("2026-07-06T09:{:02}:00Z", rem_u32(week, 60));
        cell.record_inventory_position(
            true,
            &format!("SNAP-{entity_idx}-{sku_ordinal}"),
            &warehouse_id,
            &format!("SKU-{entity_idx}-{sku_ordinal}"),
            &restated_snap_qty.to_string(),
            &restate_as_of,
            &format!("AUDIT-{entity_idx}-{sku_ordinal}-W{week}"),
        )?;
    }
    let cell_ref = state.cells.get(entity_idx).ok_or("missing entity cell")?;
    let rechecked = eval_wh_on_hand_scaled(
        cell_ref,
        &on_hand_plan,
        prior_ctx,
        Some(prior_cutoff),
        &warehouse_id,
    )?;
    if rechecked != prior_total_scaled {
        return Err(format!(
            "ASOF-KNOWLEDGE-1 violated: prior={prior_total_scaled} rechecked={rechecked}"
        ));
    }
    let current = eval_wh_on_hand_scaled(
        cell_ref,
        &on_hand_plan,
        cell_ref.query_context(),
        None,
        &warehouse_id,
    )?;
    let expected_current = prior_total_scaled
        .checked_add(delta_scaled)
        .ok_or("restated on_hand overflow")?;
    if current != expected_current {
        return Err(format!(
            "in-place restated on_hand mismatch: current={current} expected={expected_current}"
        ));
    }
    state
        .snapshot_qty
        .insert((entity_idx, sku_ordinal), restated_snap_qty);
    Ok(())
}

fn restate_late_sku_cost(
    config: SimConfig,
    week: u32,
    entity_idx: usize,
    sku_ordinal: usize,
    state: &mut SimState,
    rng: &mut SplitMix64,
) -> Result<(), String> {
    let restated_cost = i64::try_from(rng.range_u64(110, 175)).unwrap_or(135);
    let late_sku = format!("SKU-{entity_idx}-LATE");
    let eff_ts = week_timestamp_utc(week, 0, 0)?;
    {
        let cell = state
            .cells
            .get_mut(entity_idx)
            .ok_or("missing entity cell")?;
        let count_before = cell.object_count("supply.UnitCostEvidence")?;
        ingest_cost_ev_row(
            cell,
            &format!("COST-EV-{entity_idx}-LATE"),
            &late_sku,
            restated_cost,
            &eff_ts,
            &format!("ev-cost-restate-{entity_idx}-w{week}"),
        )?;
        if cell.object_count("supply.UnitCostEvidence")? != count_before {
            return Err(format!("UnitCostEvidence w{week} did not update in-place"));
        }
    }
    if state.restated_late_sku.insert(entity_idx) {
        let restated_ref = format!("COST-RESTATE-{entity_idx}-W{week}");
        if let Some(cell) = state.cells.get_mut(entity_idx) {
            cell.record_unit_cost(
                &late_sku,
                &restated_ref,
                "SUP-ATELIER",
                &restated_cost.to_string(),
                "2026-07-01T00:00:00Z",
            )?;
        }
        receive_restated_lots_for_sku(
            state,
            config.stores_per_entity,
            entity_idx,
            week,
            &late_sku,
            restated_cost,
            &eff_ts,
            rng,
        )?;
        if let Some(active) = state.active_costs.get_mut(&(entity_idx, sku_ordinal)) {
            active.sku_id = late_sku;
            active.cost_ref = restated_ref;
            active.unit_cost = restated_cost;
            active.restated = true;
        }
    }
    Ok(())
}

fn eval_wh_on_hand_scaled(
    cell: &EntityCell,
    plan: &QuestionPlan,
    ctx: QueryContext,
    cutoff: Option<KnowledgeCutoff>,
    warehouse_id: &str,
) -> Result<i128, String> {
    let result = match cutoff {
        Some(cut) => {
            cell.cell()?
                .store()
                .evaluate_question_known_at(plan, ctx, cut, |_| Ok(true))
                .map_err(|e| format!("on_hand known_at: {e}"))?
                .result
        }
        None => cell
            .cell()?
            .store()
            .evaluate_question(plan, ctx, |_| Ok(true))
            .map_err(|e| format!("on_hand live: {e}"))?,
    };
    bucket_decimal_exact(&result, &["each", warehouse_id, "2026-07-06"])
        .ok_or_else(|| format!("missing warehouse on_hand bucket for {warehouse_id}"))
        .and_then(decimal_to_scaled)
}

#[allow(
    clippy::too_many_arguments,
    reason = "restated cost stock lots for newly activated late-cost SKU"
)]
fn receive_restated_lots_for_sku(
    state: &mut SimState,
    stores_per_entity: usize,
    entity_idx: usize,
    week: u32,
    late_sku: &str,
    restated_cost: i64,
    eff_ts: &str,
    rng: &mut SplitMix64,
) -> Result<(), String> {
    let cell = state
        .cells
        .get_mut(entity_idx)
        .ok_or("missing entity cell")?;
    for store_ordinal in 0..stores_per_entity {
        let store_id = format!("ST-{entity_idx}-{store_ordinal}");
        let lot_id = format!("LOT-{entity_idx}-{store_ordinal}-LATE-W{week}");
        let qty = i64::try_from(rng.range_u64(300, 600)).unwrap_or(400);
        let cust = custodian_for_store(store_ordinal);
        cell.record_costed_lot(
            &lot_id,
            &store_id,
            late_sku,
            cust,
            &qty.to_string(),
            "each",
            &restated_cost.to_string(),
            eff_ts,
        )?;
        state.lot_state.insert(
            (entity_idx, store_ordinal, late_sku.to_owned()),
            LotTracker {
                lot_id,
                received: qty,
                remaining: qty,
                unit_cost: restated_cost,
            },
        );
        if let Some(oracle) = state.oracles.get_mut(entity_idx) {
            let entry = oracle
                .lot_conservation
                .entry((store_id, late_sku.to_owned()))
                .or_insert((0, 0));
            entry.0 = entry.0.saturating_add(qty);
            oracle.total_receipts = oracle.total_receipts.saturating_add(1);
        }
    }
    Ok(())
}

const fn custodian_for_store(store_ordinal: usize) -> &'static str {
    if store_ordinal == 0 {
        "CUST-MALL-CFC-OPS"
    } else {
        "CUST-BRANCH-OPS"
    }
}

fn record_simulated_sale(
    week: u32,
    entity_idx: usize,
    sku_ordinal: usize,
    store_id: &str,
    state: &mut SimState,
    rng: &mut SplitMix64,
) -> Result<SimulatedSale, String> {
    let active = state
        .active_costs
        .get(&(entity_idx, sku_ordinal))
        .ok_or("missing active cost")?;
    let (sku_id, cost_ref, unit_cost, restated) = (
        active.sku_id.clone(),
        active.cost_ref.clone(),
        active.unit_cost,
        active.restated,
    );
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
    store_ordinal: usize,
    store_id: &str,
    sku_id: &str,
    sold_qty: i64,
    state: &mut SimState,
) -> Result<(), String> {
    if let Some(lot) = state
        .lot_state
        .get_mut(&(entity_idx, store_ordinal, sku_id.to_owned()))
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
    store_ordinal: usize,
    store_id: &str,
    sku_id: &str,
    seq: u64,
    state: &mut SimState,
) -> Result<(), String> {
    if rem_u32(week, 4) != 2 {
        return Ok(());
    }
    let Some((received, depleted)) = state
        .oracles
        .get(entity_idx)
        .and_then(|o| {
            o.lot_conservation
                .get(&(store_id.to_owned(), sku_id.to_owned()))
        })
        .copied()
    else {
        return Ok(());
    };
    let recount = !state
        .counted_once
        .insert((entity_idx, store_ordinal, sku_id.to_owned()));
    if let Some(cell) = state.cells.get_mut(entity_idx) {
        cell.record_store_count(
            recount,
            &format!("{store_id}/{sku_id}/on_hand"),
            store_id,
            sku_id,
            &received.saturating_sub(depleted).to_string(),
            "each",
            "on_hand",
            &week_timestamp_utc(week, 6, 10)?,
            &format!("CC-W{week}-{seq}"),
        )?;
    }
    if let Some(oracle) = state.oracles.get_mut(entity_idx) {
        oracle.total_recounts = oracle.total_recounts.saturating_add(1);
    }
    Ok(())
}

fn finalize_and_verify_sim(config: SimConfig, state: &mut SimState) -> Result<(), String> {
    let (mut any_restated, mut any_restated_sale) = (false, false);
    for entity_idx in 0..config.entity_count {
        let store_id = format!("ST-{entity_idx}-0");
        let (sku_id, cost_val) = state.active_costs.get(&(entity_idx, 0)).map_or_else(
            || (format!("SKU-{entity_idx}-0"), 100),
            |active| (active.sku_id.clone(), active.unit_cost),
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
            any_restated |= oracle.total_cost_restatements > 0;
            any_restated_sale |= oracle.restated_sales > 0;
            verify_cell_matches_oracle(cell, oracle, config.weeks)?;
        }
    }
    if config.weeks >= 12 && (!any_restated || !any_restated_sale) {
        return Err(
            "simulator must commit in-place restatements and apply them to subsequent sales"
                .to_owned(),
        );
    }
    verify_multi_cell_combined_view_and_fx_shifts(
        config.weeks,
        &state.cells,
        &state.fx_history,
        &state.inter_entity_transfers,
    )
}
