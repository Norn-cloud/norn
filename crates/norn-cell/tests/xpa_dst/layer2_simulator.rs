//! Layer 2A — Seeded Multi-Cell Event-Log Simulator.
//!
//! Drives 16–52 simulated weeks of concurrent/interleaved POS sales, supplier
//! receipts, physical cycle counts, genuine existing-SKU cost restatements
//! (later-dated `UnitCostEvidence` + restated-cost `StockLot` on the SAME
//! already-sold SKU verified bitemporally via `evaluate_question_known_at`,
//! plus late-arriving `UnitCost` activation on an already-sold SKU), bilateral
//! inter-entity stock transfers (real seller `PosSale` + buyer `StockLot`
//! commits bound via verified `main` [`norn_cell::export::ExportRecord`]s),
//! and pinned reporting-view FX shifts evaluated over real multi-cell combined
//! views, verifying every cell against an independent exact arithmetic oracle
//! across the FULL simulated horizon.

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
use super::operating_group::{FxRateEntry, InterEntityTransfer, TransferLegEvidence};
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
    latest_lot_cost: i64,
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
    for store_ordinal in 0..config.stores_per_entity {
        let store_id = format!("ST-{entity_idx}-{store_ordinal}");
        let mall = (store_ordinal == 0).then_some("MALL-CFC");
        cell.register_store(
            &store_id,
            &format!("Store {entity_idx}-{store_ordinal}"),
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
    let mut oracle = EntityOracle::default();
    for sku_ordinal in 0..config.skus_per_entity {
        seed_initial_sim_sku(
            &mut cell,
            &mut oracle,
            config.stores_per_entity,
            entity_idx,
            sku_ordinal,
            rng,
            active_costs,
            lot_state,
        )?;
    }
    Ok((cell, oracle))
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
    sku_ordinal: usize,
    rng: &mut SplitMix64,
    active_costs: &mut BTreeMap<(usize, usize), ActiveSkuCost>,
    lot_state: &mut BTreeMap<(usize, usize, String), LotTracker>,
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
    active_costs.insert(
        (entity_idx, sku_ordinal),
        ActiveSkuCost {
            sku_id: sku_id.clone(),
            cost_ref,
            unit_cost: cost_val,
            latest_lot_cost: cost_val,
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
            if store_ordinal == 0 {
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

    let (seller_ccy, seller_unit_price, seller_leg) =
        commit_transfer_seller_leg(week, week_monday, qty, markup, &ts, state)?;
    let buyer_unit_cost_egp = seller_unit_price.saturating_mul(whole_egp_per_usd);
    let buyer_leg = commit_transfer_buyer_leg(week, qty, buyer_unit_cost_egp, &ts, state)?;

    state.inter_entity_transfers.push(InterEntityTransfer {
        transfer_id: format!("IE-SIM-{}-W{week}", config.seed),
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
    let sale_object_id = format!("SALE-IE-W{week}");
    let seller_ccy = state
        .cells
        .get(seller_idx)
        .map(|c| c.currency.clone())
        .ok_or("missing seller")?;
    let seller_export = {
        let seller = state.cells.get_mut(seller_idx).ok_or("missing seller")?;
        seller.record_costed_sale(
            &sale_object_id,
            seller_store,
            &seller_sku,
            &qty.to_string(),
            "each",
            &sale_rev.to_string(),
            &seller_cost.to_string(),
            &seller_cost_ref,
            ts,
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
    week: u32,
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
    let lot_id = format!("LOT-IE-W{week}");
    let buyer_export = {
        let buyer = state.cells.get_mut(buyer_idx).ok_or("missing buyer")?;
        buyer.record_costed_lot(
            &lot_id,
            buyer_store,
            &buyer_sku,
            "CUST-MALL-CFC-OPS",
            &qty.to_string(),
            "each",
            &buyer_unit_cost_egp.to_string(),
            ts,
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

    maybe_commit_existing_sku_cost_restatement(
        config,
        week,
        week_monday,
        entity_idx,
        sku_ordinal,
        state,
        rng,
    )?;
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

/// Genuine cost restatement of an EXISTING, ALREADY-SOLD SKU (no new SKU):
/// captures `prior_cutoff`, ingests later-dated `UnitCostEvidence` + restated
/// `StockLot`s on that SAME existing SKU, and asserts via
/// `evaluate_question_known_at` that `prior_cutoff` preserves original costed
/// valuation while the current cut reflects the restated cost.
fn maybe_commit_existing_sku_cost_restatement(
    config: SimConfig,
    week: u32,
    week_monday: &str,
    entity_idx: usize,
    sku_ordinal: usize,
    state: &mut SimState,
    rng: &mut SplitMix64,
) -> Result<(), String> {
    if week == 0 || rem_u32(week, 6) != 0 {
        return Ok(());
    }
    let (existing_sku, previous_lot_cost) = state
        .active_costs
        .get(&(entity_idx, sku_ordinal))
        .map(|active| (active.sku_id.clone(), active.latest_lot_cost))
        .ok_or("missing existing SKU for cost restatement")?;
    let restated_cost =
        previous_lot_cost.saturating_add(i64::try_from(rng.range_u64(15, 45)).unwrap_or(20));
    let restated_ref = format!("COST-RESTATE-{entity_idx}-{sku_ordinal}-W{week}");
    let eff_ts = week_timestamp_utc(week, 0, 0)?;
    let primary_store = format!("ST-{entity_idx}-0");

    let (prior_cutoff, prior_ctx, prior_val_scaled, stock_val_plan) = capture_prior_stock_value(
        state.cells.get(entity_idx).ok_or("missing entity cell")?,
        config.weeks,
        &primary_store,
        week_monday,
    )?;
    ingest_existing_sku_cost_evidence(
        state,
        entity_idx,
        sku_ordinal,
        week,
        &existing_sku,
        &restated_ref,
        restated_cost,
        &eff_ts,
    )?;
    let primary_added_val = receive_restated_lots_for_existing_sku(
        state,
        config.stores_per_entity,
        entity_idx,
        sku_ordinal,
        week,
        &existing_sku,
        restated_cost,
        &eff_ts,
        rng,
    )?;
    assert_bitemporal_stock_value(
        state.cells.get(entity_idx).ok_or("missing entity cell")?,
        &stock_val_plan,
        prior_ctx,
        prior_cutoff,
        &primary_store,
        week_monday,
        prior_val_scaled,
        primary_added_val,
    )?;
    if let Some(active) = state.active_costs.get_mut(&(entity_idx, sku_ordinal)) {
        active.latest_lot_cost = restated_cost;
        active.restated = true;
    }
    if let Some(oracle) = state.oracles.get_mut(entity_idx) {
        oracle.total_cost_restatements = oracle.total_cost_restatements.saturating_add(1);
    }
    Ok(())
}

fn capture_prior_stock_value(
    cell: &EntityCell,
    weeks: u32,
    store_id: &str,
    week_monday: &str,
) -> Result<(KnowledgeCutoff, QueryContext, i128, QuestionPlan), String> {
    let cutoff = KnowledgeCutoff {
        recorded_at: cell.now(),
        frontier: cell.head()?.commits,
    };
    let ctx = cell.query_context();
    let plan = cell.pack.retail_question_for_weeks(
        "supply.retail.store_stock_value_weekly",
        weeks,
        5_000,
    )?;
    let prior_res = cell
        .cell()?
        .store()
        .evaluate_question_known_at(&plan, ctx.clone(), cutoff, |_| Ok(true))
        .map_err(|e| format!("prior stock value known_at: {e}"))?;
    let prior_val = bucket_decimal_exact(
        &prior_res.result,
        &[&cell.currency, store_id, "costed", week_monday],
    )
    .map(decimal_to_scaled)
    .transpose()?
    .unwrap_or(0);
    Ok((cutoff, ctx, prior_val, plan))
}

#[allow(
    clippy::too_many_arguments,
    reason = "bitemporal stock valuation comparison"
)]
fn assert_bitemporal_stock_value(
    cell: &EntityCell,
    plan: &QuestionPlan,
    prior_ctx: QueryContext,
    prior_cutoff: KnowledgeCutoff,
    store_id: &str,
    week_monday: &str,
    prior_val_scaled: i128,
    added_val_scaled: i128,
) -> Result<(), String> {
    let rechecked = cell
        .cell()?
        .store()
        .evaluate_question_known_at(plan, prior_ctx, prior_cutoff, |_| Ok(true))
        .map_err(|e| format!("recheck prior stock value: {e}"))?;
    let rechecked_scaled = bucket_decimal_exact(
        &rechecked.result,
        &[&cell.currency, store_id, "costed", week_monday],
    )
    .map(decimal_to_scaled)
    .transpose()?
    .unwrap_or(0);
    if rechecked_scaled != prior_val_scaled {
        return Err(format!(
            "ASOF-KNOWLEDGE-1 violated on existing-SKU cost restatement: prior={prior_val_scaled} rechecked={rechecked_scaled}"
        ));
    }
    let current = cell
        .cell()?
        .store()
        .evaluate_question(plan, cell.query_context(), |_| Ok(true))
        .map_err(|e| format!("current stock value: {e}"))?;
    let current_scaled =
        bucket_decimal_exact(&current, &[&cell.currency, store_id, "costed", week_monday])
            .ok_or("missing current restated stock value bucket")
            .and_then(decimal_to_scaled)?;
    let expected_current = prior_val_scaled
        .checked_add(added_val_scaled)
        .ok_or("restated stock value overflow")?;
    if current_scaled != expected_current {
        return Err(format!(
            "restated stock value mismatch: current={current_scaled} expected={expected_current}"
        ));
    }
    Ok(())
}

#[allow(
    clippy::too_many_arguments,
    reason = "existing-SKU restated UnitCostEvidence ingestion"
)]
fn ingest_existing_sku_cost_evidence(
    state: &mut SimState,
    entity_idx: usize,
    sku_ordinal: usize,
    week: u32,
    existing_sku: &str,
    restated_ref: &str,
    restated_cost: i64,
    eff_ts: &str,
) -> Result<(), String> {
    let cell = state
        .cells
        .get_mut(entity_idx)
        .ok_or("missing entity cell")?;
    let payload = json!({
        "cost_id": restated_ref, "sku_code": existing_sku, "vendor_code": "ATELIER-01",
        "unit_cost": restated_cost, "ccy": cell.currency, "effective_from": eff_ts,
    })
    .to_string();
    let (_, clean) = cell.ingest_feed(&FeedRow {
        feed_id: COSTS_FEED_ID,
        mapping: COSTS_FEED_ID,
        dataset: "unit_cost",
        source: "synthetic-cost",
        event: &format!("ev-cost-restate-{entity_idx}-{sku_ordinal}-w{week}"),
        record: restated_ref,
        payload: &payload,
    })?;
    if !clean {
        return Err(format!("cost restatement w{week} quarantined unexpectedly"));
    }
    Ok(())
}

#[allow(
    clippy::too_many_arguments,
    reason = "restated cost stock lots on existing SKU"
)]
fn receive_restated_lots_for_existing_sku(
    state: &mut SimState,
    stores_per_entity: usize,
    entity_idx: usize,
    sku_ordinal: usize,
    week: u32,
    existing_sku: &str,
    restated_cost: i64,
    eff_ts: &str,
    rng: &mut SplitMix64,
) -> Result<i128, String> {
    let cell = state
        .cells
        .get_mut(entity_idx)
        .ok_or("missing entity cell")?;
    let mut primary_store_added_val: i128 = 0;
    for store_ordinal in 0..stores_per_entity {
        let store_id = format!("ST-{entity_idx}-{store_ordinal}");
        let lot_id = format!("LOT-{entity_idx}-{store_ordinal}-{sku_ordinal}-R{week}");
        let qty = i64::try_from(rng.range_u64(250, 500)).unwrap_or(300);
        cell.record_costed_lot(
            &lot_id,
            &store_id,
            existing_sku,
            if store_ordinal == 0 {
                "CUST-MALL-CFC-OPS"
            } else {
                "CUST-BRANCH-OPS"
            },
            &qty.to_string(),
            "each",
            &restated_cost.to_string(),
            eff_ts,
        )?;
        if store_ordinal == 0 {
            primary_store_added_val =
                scaled(i128::from(qty).saturating_mul(i128::from(restated_cost)));
        }
        state.lot_state.insert(
            (entity_idx, store_ordinal, existing_sku.to_owned()),
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
                .entry((store_id, existing_sku.to_owned()))
                .or_insert((0, 0));
            entry.0 = entry.0.saturating_add(qty);
            oracle.total_receipts = oracle.total_receipts.saturating_add(1);
        }
    }
    Ok(primary_store_added_val)
}

fn record_simulated_sale(
    week: u32,
    entity_idx: usize,
    sku_ordinal: usize,
    store_id: &str,
    state: &mut SimState,
    rng: &mut SplitMix64,
) -> Result<SimulatedSale, String> {
    let (sku_id, cost_ref, unit_cost, restated) = state
        .active_costs
        .get(&(entity_idx, sku_ordinal))
        .map(|active| {
            (
                active.sku_id.clone(),
                active.cost_ref.clone(),
                active.unit_cost,
                active.restated,
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
    if rem_u32(week, 4) == 2
        && let Some((received, depleted)) = state
            .oracles
            .get(entity_idx)
            .and_then(|o| {
                o.lot_conservation
                    .get(&(store_id.to_owned(), sku_id.to_owned()))
            })
            .copied()
    {
        let total_on_hand = received.saturating_sub(depleted);
        let recount = !state
            .counted_once
            .insert((entity_idx, store_ordinal, sku_id.to_owned()));
        if let Some(cell) = state.cells.get_mut(entity_idx) {
            cell.record_store_count(
                recount,
                &format!("{store_id}/{sku_id}/on_hand"),
                store_id,
                sku_id,
                &total_on_hand.to_string(),
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
            "simulator must commit existing-SKU cost restatements and apply them to subsequent steps"
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
