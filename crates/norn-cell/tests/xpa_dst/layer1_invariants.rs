//! Layer 1 — Structural & Formal Invariants for Whole-Business xP&A:
//!
//! 1. **Exact decimal & unit-partition algebra (`METRIC-UNIT-PARTITION-1`):**
//!    checker refusals (`mixed_currency`, `mixed_unit`), checked 6-decimal `i128`
//!    arithmetic verified against an independent integer oracle, ratio-of-sums vs
//!    average-of-ratios separation, and real governed cell KPI evaluation
//!    (`gross_margin_rate`, `sell_through`, `WOS`, `GMROI`).
//! 2. **Temporal & grain laws:** canonical UTC ISO-week Monday binding
//!    (`period_week` <-> `period_end`), snapshot evidence cutoffs, and bitemporal
//!    restatement preservation (`ASOF-KNOWLEDGE-1` + `CellError::TemporalLeakage`)
//!    without freezing future recounts.
//! 3. **Single-book & multi-entity isolation (`READ-LOCAL-1`):** single
//!    `LegalEntity` book per cell, read-only combined management views, and the
//!    fail-closed 4-tier `OperatingGroup` projection ladder in
//!    [`super::operating_group`].

use norn_cell::combined_view::CombinedViewKind;
use norn_cell::{CellError, KnowledgeCutoff, Receipt};
use norn_checker::metric::compute::arithmetic::ratio;
use norn_ir::Decimal;
use serde_json::json;

use super::actions::StoreKpiInput;
use super::cuts::{
    bucket_decimal_exact, cell_id_for_entity, dst_coverage_spec, evaluate_multi_cell_report,
};
use super::env::{DEFAULT_CLOCK_MS, EntityCell, retail_pack};
use super::operating_group::{
    EntityReportingSlice, FxRateEntry, InterEntityTransfer, OperatingGroupRefusal,
    OperatingGroupTier, StatutoryCloseApproval, compute_elimination_journal_pin,
    evaluate_operating_group_tier, slice_from_cut_snapshot,
};
use super::prng::{
    PRIMARY_SEED, SECONDARY_SEED, SplitMix64, decimal_from_scaled, decimal_to_scaled, div_i128,
    exact_ratio_of_sums, independent_ratio_scaled, scaled, week_monday_iso,
};

/// Layer 1A: Verify `METRIC-UNIT-PARTITION-1`, checked exact decimal algebra
/// against an independent integer oracle, and real governed KPI ratio-of-sums
/// evaluation inside a live cell.
///
/// # Errors
/// Returns an error if any structural invariant or refusal fails.
pub fn verify_decimal_and_unit_partition_laws() -> Result<(), String> {
    verify_unit_partition_checker_refusals()?;
    verify_checked_decimal_and_ratio_algebra()?;
    verify_governed_cell_kpi_ratio_of_sums()
}

fn verify_unit_partition_checker_refusals() -> Result<(), String> {
    let pack = retail_pack()?;
    let mut mixed_ccy_q = pack.retail_question_ir("supply.retail.store_revenue_weekly")?;
    mixed_ccy_q
        .dimensions
        .retain(|dim| dim.path.property.as_str() != "currency");
    let ccy_err = pack
        .admit_question_ir(&mixed_ccy_q, 200)
        .err()
        .ok_or("unpartitioned currency question MUST refuse")?;
    if !ccy_err.contains("metric.validation.mixed_currency") {
        return Err(format!(
            "expected metric.validation.mixed_currency, got: {ccy_err}"
        ));
    }

    let mut mixed_unit_q = pack.retail_question_ir("supply.retail.store_sales_weekly")?;
    mixed_unit_q
        .dimensions
        .retain(|dim| dim.path.property.as_str() != "unit");
    let unit_err = pack
        .admit_question_ir(&mixed_unit_q, 200)
        .err()
        .ok_or("unpartitioned unit question MUST refuse")?;
    if !unit_err.contains("metric.validation.mixed_unit") {
        return Err(format!(
            "expected metric.validation.mixed_unit, got: {unit_err}"
        ));
    }
    Ok(())
}

fn verify_checked_decimal_and_ratio_algebra() -> Result<(), String> {
    let mut rng = SplitMix64::new(PRIMARY_SEED);
    for _ in 0..64 {
        let rev_a = scaled(i128::from(rng.range_u64(100, 10_000)));
        let rev_b = scaled(i128::from(rng.range_u64(100, 10_000)));
        let cogs_a = div_i128(rev_a, 4).saturating_mul(i128::from(rng.range_u64(1, 3)));
        let cogs_b = div_i128(rev_b, 5).saturating_mul(i128::from(rng.range_u64(1, 4)));
        let sum_gm = rev_a
            .checked_sub(cogs_a)
            .and_then(|ga| rev_b.checked_sub(cogs_b).and_then(|gb| ga.checked_add(gb)))
            .ok_or("checked margin overflow")?;
        let sum_rev = rev_a.checked_add(rev_b).ok_or("checked revenue overflow")?;
        let engine_scaled =
            ratio(sum_gm, sum_rev).ok_or("engine ratio unexpectedly returned None")?;
        let oracle_scaled =
            independent_ratio_scaled(sum_gm, sum_rev).ok_or("independent oracle returned None")?;
        if engine_scaled != oracle_scaled {
            return Err(format!(
                "ratio oracle divergence: engine={engine_scaled} oracle={oracle_scaled}"
            ));
        }
        let exact_dec = exact_ratio_of_sums(sum_gm, sum_rev)?.ok_or("expected exact ratio")?;
        if exact_dec != decimal_from_scaled(oracle_scaled)? {
            return Err(format!(
                "exact_ratio_of_sums mismatch: {exact_dec:?} vs oracle {oracle_scaled}"
            ));
        }
        let neg_gm = sum_gm.checked_neg().ok_or("checked_neg overflow")?;
        if sum_gm.checked_add(neg_gm) != Some(0) {
            return Err("exact additive inverse failed".to_owned());
        }
        if exact_ratio_of_sums(sum_gm, 0)?.is_some() || ratio(sum_gm, 0).is_some() {
            return Err("zero denominator must return None".to_owned());
        }
        if decimal_to_scaled(decimal_from_scaled(sum_gm)?)? != sum_gm {
            return Err("decimal_from_scaled round-trip mismatch".to_owned());
        }
    }

    let checks = [
        (scaled(2_400), scaled(3_800), "0.631579", "GM rate"),
        (scaled(60), scaled(10), "6", "WOS"),
        (scaled(5_000), scaled(8_000), "0.625", "GMROI"),
        (scaled(40), scaled(100), "0.4", "sell-through"),
        (1, 128, "0.007813", "half-away-from-zero +"),
        (-1, 128, "-0.007813", "half-away-from-zero -"),
    ];
    for (num, den, want, label) in checks {
        let got = exact_ratio_of_sums(num, den)?
            .ok_or_else(|| format!("expected {label}"))?
            .canonical();
        let oracle_dec = decimal_from_scaled(
            independent_ratio_scaled(num, den).ok_or_else(|| format!("oracle none for {label}"))?,
        )?
        .canonical();
        if got != want || oracle_dec != want {
            return Err(format!(
                "{label}: expected {want}, got engine={got}, oracle={oracle_dec}"
            ));
        }
    }
    Ok(())
}

fn verify_governed_cell_kpi_ratio_of_sums() -> Result<(), String> {
    let book = seed_kpi_ratio_book()?;
    assert_cell_kpi_matches_oracle(
        &book,
        "supply.retail.store_gross_margin_rate_weekly",
        &["EGP", "ST-KPI-1", "costed", "2026-08-31"],
        scaled(2_000),
        scaled(4_000),
        "0.5",
    )?;
    assert_cell_kpi_matches_oracle(
        &book,
        "supply.retail.store_sell_through_weekly",
        &["each", "ST-KPI-1"],
        scaled(20),
        scaled(100),
        "0.2",
    )?;
    assert_cell_kpi_matches_oracle(
        &book,
        "supply.retail.store_weeks_of_supply_weekly",
        &["each", "ST-KPI-1", "SKU-KPI-1", "2026-08-31"],
        scaled(80),
        scaled(20),
        "4",
    )?;
    assert_cell_kpi_matches_oracle(
        &book,
        "supply.retail.store_gmroi_weekly",
        &["EGP", "ST-KPI-1", "SKU-KPI-1", "2026-08-31"],
        scaled(2_000),
        scaled(9_000),
        "0.222222",
    )
}

fn seed_kpi_ratio_book() -> Result<EntityCell, String> {
    let mut book = EntityCell::open(
        &cell_id_for_entity(PRIMARY_SEED, 7),
        "LE-KPI-L1",
        "EGP",
        "org_xpa_kpi_l1",
        "prod",
        DEFAULT_CLOCK_MS,
    )?;
    book.register_book("KPI Ratio Law Entity", "sole_proprietorship")?;
    book.register_sku("SKU-KPI-1", "dry_goods", "each")?;
    book.register_store(
        "ST-KPI-1",
        "KPI Store",
        "CUST-1",
        "mall_store",
        Some("MALL-CFC"),
    )?;
    book.record_unit_cost(
        "SKU-KPI-1",
        "COST-KPI-1",
        "SUP-1",
        "100",
        "2026-07-01T00:00:00Z",
    )?;
    book.record_costed_lot(
        "LOT-KPI-1",
        "ST-KPI-1",
        "SKU-KPI-1",
        "CUST-1",
        "100",
        "each",
        "100",
        "2026-08-03T09:00:00Z",
    )?;
    book.record_stock_valuation_cut(
        "CUT-KPI-OPEN",
        "ST-KPI-1",
        "SKU-KPI-1",
        "each",
        "2026-08-31T00:00:00Z",
        "100",
        "10000",
    )?;
    seed_kpi_ratio_sales_and_rollup(&mut book)?;
    Ok(book)
}

fn seed_kpi_ratio_sales_and_rollup(book: &mut EntityCell) -> Result<(), String> {
    // Unequal weights: sale 1 GM rate = 0.2; sale 2 GM rate = 0.6.
    // Average-of-ratios would be 0.4; ratio-of-sums MUST be 2000/4000 = 0.5.
    book.record_costed_sale(
        "SALE-KPI-1",
        "ST-KPI-1",
        "SKU-KPI-1",
        "8",
        "each",
        "1000",
        "100",
        "COST-KPI-1",
        "2026-09-01T12:00:00Z",
    )?;
    book.record_costed_sale(
        "SALE-KPI-2",
        "ST-KPI-1",
        "SKU-KPI-1",
        "12",
        "each",
        "3000",
        "100",
        "COST-KPI-1",
        "2026-09-03T12:00:00Z",
    )?;
    book.update_lot_balance(
        "LOT-KPI-1",
        "ST-KPI-1",
        "SKU-KPI-1",
        "80",
        "20",
        "100",
        "2026-09-04T18:00:00Z",
    )?;
    book.record_store_count(
        false,
        "ST-KPI-1/SKU-KPI-1/on_hand",
        "ST-KPI-1",
        "SKU-KPI-1",
        "80",
        "each",
        "on_hand",
        "2026-09-05T10:00:00Z",
        "CC-KPI-1",
    )?;
    book.record_store_kpi(&StoreKpiInput {
        rollup_id: "KPI-L1-W9",
        store_id: "ST-KPI-1",
        sku_id: "SKU-KPI-1",
        period_week: "2026-08-31",
        unit: "each",
        on_hand_count_id: "ST-KPI-1/SKU-KPI-1/on_hand",
        opening_cut_id: "CUT-KPI-OPEN",
        count_ref: "CC-KPI-1",
        count_as_of: "2026-09-05T10:00:00Z",
        costed_ref: "COST-KPI-1",
        period_start: "2026-08-31T00:00:00Z",
        period_end: "2026-09-06T23:59:59.999999999Z",
        sales_weeks: 1,
        on_hand_qty: "80",
        avg_weekly_sales_qty: "20",
        period_gross_margin: "2000",
        opening_stock_value: "10000",
        closing_stock_value: "8000",
    })
}

fn assert_cell_kpi_matches_oracle(
    book: &EntityCell,
    question: &str,
    bucket: &[&str],
    num_scaled: i128,
    den_scaled: i128,
    expected_text: &str,
) -> Result<(), String> {
    let actual = bucket_decimal_exact(&book.answer(question)?, bucket)
        .ok_or_else(|| format!("{question}: missing bucket {bucket:?}"))?;
    let oracle_scaled =
        independent_ratio_scaled(num_scaled, den_scaled).ok_or("independent oracle None")?;
    let oracle_dec = decimal_from_scaled(oracle_scaled)?;
    if actual != oracle_dec || actual.canonical() != expected_text {
        return Err(format!(
            "{question}: actual={} oracle={} expected={expected_text}",
            actual.canonical(),
            oracle_dec.canonical()
        ));
    }
    Ok(())
}

/// Layer 1B: Verify temporal & grain laws (ISO-week Monday binding, snapshot
/// evidence cutoffs, future-cost refusal, and `ASOF-KNOWLEDGE-1` bitemporal
/// restatement preservation without freezing future recounts).
///
/// # Errors
/// Returns an error if any temporal or bitemporal invariant fails.
pub fn verify_temporal_grain_and_bitemporal_laws() -> Result<(), String> {
    let mut book = seed_temporal_book()?;
    let k1 = KnowledgeCutoff {
        recorded_at: book.now(),
        frontier: book.head()?.commits,
    };
    book.record_store_count(
        false,
        "ST-T1/SKU-A/on_hand",
        "ST-T1",
        "SKU-A",
        "40",
        "each",
        "on_hand",
        "2026-09-04T15:00:00Z",
        "CC-OH-1",
    )?;
    verify_rollup_temporal_and_cutoff_laws(&mut book)?;
    verify_bitemporal_recount_preservation(&mut book, k1)
}

fn seed_temporal_book() -> Result<EntityCell, String> {
    let mut book = EntityCell::open(
        &cell_id_for_entity(PRIMARY_SEED, 0),
        "LE-TEMP",
        "EGP",
        "org_xpa_temp",
        "prod",
        DEFAULT_CLOCK_MS,
    )?;
    book.register_book("Temporal Law Entity", "sole_proprietorship")?;
    book.register_sku("SKU-A", "dry_goods", "each")?;
    book.register_sku("SKU-FUTURE", "dry_goods", "each")?;
    book.register_store(
        "ST-T1",
        "Temporal Flagship",
        "CUST-1",
        "mall_store",
        Some("MALL-CFC"),
    )?;
    book.record_unit_cost("SKU-A", "COST-A-1", "SUP-1", "100", "2026-07-01T00:00:00Z")?;
    book.record_unit_cost(
        "SKU-FUTURE",
        "COST-FUT-1",
        "SUP-1",
        "50",
        "2026-10-01T00:00:00Z",
    )?;
    let (code, msg) = book.refused(
        "supply.pos_sale.record",
        "clerk:hana",
        json!({
            "sale_id": "SALE-FUT-COST", "store_id": "ST-T1", "sku_id": "SKU-FUTURE",
            "sold_qty": "2", "unit": "each", "net_revenue": "200",
            "applied_unit_cost": "50", "cost_status": "costed",
            "costed_ref": "COST-FUT-1", "currency": "EGP", "sold_at": "2026-09-01T10:00:00Z",
        }),
    )?;
    if code != "core.laws.violated" || !msg.contains("sale_matches_cost_record") {
        return Err(format!("future-effective cost must refuse: {code} {msg}"));
    }
    seed_temporal_positions(&mut book)?;
    Ok(book)
}

fn seed_temporal_positions(book: &mut EntityCell) -> Result<(), String> {
    book.record_costed_lot(
        "LOT-T1",
        "ST-T1",
        "SKU-A",
        "CUST-1",
        "50",
        "each",
        "100",
        "2026-08-03T09:00:00Z",
    )?;
    book.record_stock_valuation_cut(
        "CUT-T1-W6",
        "ST-T1",
        "SKU-A",
        "each",
        "2026-08-31T00:00:00Z",
        "50",
        "5000",
    )?;
    book.update_lot_balance(
        "LOT-T1",
        "ST-T1",
        "SKU-A",
        "40",
        "10",
        "100",
        "2026-09-04T12:00:00Z",
    )?;
    book.record_costed_sale(
        "SALE-T1-W9",
        "ST-T1",
        "SKU-A",
        "10",
        "each",
        "2000",
        "100",
        "COST-A-1",
        "2026-09-02T12:00:00Z",
    )?;
    book.record_store_count(
        false,
        "ST-T1/SKU-A/available",
        "ST-T1",
        "SKU-A",
        "38",
        "each",
        "available",
        "2026-09-04T10:00:00Z",
        "CC-K1",
    )
}

fn verify_rollup_temporal_and_cutoff_laws(book: &mut EntityCell) -> Result<(), String> {
    let monday_w9 = week_monday_iso(8)?;
    let make_rollup = |id: &str, period_week: &str, count_as_of: &str| {
        json!({
            "rollup_id": id, "store_id": "ST-T1", "sku_id": "SKU-A",
            "period_week": period_week, "unit": "each", "currency": "EGP",
            "cost_status": "costed", "count_status": "verified",
            "on_hand_count_id": "ST-T1/SKU-A/on_hand", "opening_cut_id": "CUT-T1-W6",
            "count_ref": "CC-OH-1", "count_as_of": count_as_of,
            "costed_ref": "COST-A-1", "book_epoch": "1970-01-01T00:00:00Z",
            "period_start": "2026-08-31T00:00:00Z", "period_end": "2026-09-05T18:00:00Z",
            "sales_weeks": 1, "on_hand_qty": "40", "avg_weekly_sales_qty": "10",
            "period_gross_margin": "1000", "opening_stock_value": "5000",
            "closing_stock_value": "4000",
        })
    };
    let (bad_week, _) = book.refused(
        "supply.store_kpi.record",
        "planner:omar",
        make_rollup("KPI-BAD-WEEK", "2026-09-01", "2026-09-04T15:00:00Z"),
    )?;
    if bad_week != "action.precondition_failed" {
        return Err(format!(
            "misaligned period_week must refuse, got {bad_week}"
        ));
    }
    let (bad_cutoff, _) = book.refused(
        "supply.store_kpi.record",
        "planner:omar",
        make_rollup("KPI-BAD-CUTOFF", &monday_w9, "2026-09-05T10:00:00Z"),
    )?;
    if bad_cutoff != "action.precondition_failed" {
        return Err(format!(
            "mismatched count_as_of must refuse, got {bad_cutoff}"
        ));
    }
    book.ok(
        "supply.store_kpi.record",
        "planner:omar",
        make_rollup("KPI-T1-W9", &monday_w9, "2026-09-04T15:00:00Z"),
    )
}

fn verify_bitemporal_recount_preservation(
    book: &mut EntityCell,
    k1: KnowledgeCutoff,
) -> Result<(), String> {
    book.record_store_count(
        true,
        "ST-T1/SKU-A/available",
        "ST-T1",
        "SKU-A",
        "35",
        "each",
        "available",
        "2026-09-05T12:00:00Z",
        "CC-K2",
    )?;
    let k2 = KnowledgeCutoff {
        recorded_at: book.now(),
        frontier: book.head()?.commits,
    };
    let avail_plan = book
        .pack
        .retail_question("supply.retail.store_availability_weekly")?;
    let live_val = bucket_decimal_exact(
        &book.answer("supply.retail.store_availability_weekly")?,
        &["each", "ST-T1", "2026-08-31"],
    )
    .ok_or("missing live availability")?;
    if live_val.canonical() != "35" {
        return Err(format!("expected live availability 35, got {live_val}"));
    }

    let mut k1_ctx = book.query_context();
    k1_ctx.now_ms = k1.recorded_at.as_millis();
    let hist = book
        .cell()?
        .store()
        .evaluate_question_known_at(&avail_plan, k1_ctx.clone(), k1, |_| Ok(true))
        .map_err(|e| format!("evaluate_question_known_at K1: {e}"))?;
    let hist_val = bucket_decimal_exact(&hist.result, &["each", "ST-T1", "2026-08-31"])
        .ok_or("missing K1 historical availability")?;
    if hist_val.canonical() != "38" {
        return Err(format!(
            "ASOF-KNOWLEDGE-1 violated: expected K1 availability 38, got {hist_val}"
        ));
    }

    let leak_cutoff = KnowledgeCutoff {
        recorded_at: k1.recorded_at,
        frontier: k2.frontier,
    };
    let leak_res =
        book.cell()?
            .store()
            .evaluate_question_known_at(&avail_plan, k1_ctx, leak_cutoff, |_| Ok(true));
    if !matches!(leak_res, Err(CellError::TemporalLeakage)) {
        return Err(format!(
            "expected CellError::TemporalLeakage on future frontier, got {leak_res:?}"
        ));
    }
    Ok(())
}

/// Layer 1C: Verify single-book-per-cell isolation (`READ-LOCAL-1`), read-only
/// combined management views, and the fail-closed 4-tier `OperatingGroup`
/// projection ladder bound to real cell cuts and verified `main` receipts.
///
/// # Errors
/// Returns an error if any isolation or ladder refusal rule fails.
pub fn verify_single_book_and_operating_group_ladder() -> Result<(), String> {
    let (egp_cell, usd_cell) = seed_two_book_cells()?;
    let spec = dst_coverage_spec();
    let books = [&egp_cell, &usd_cell];
    let report = evaluate_multi_cell_report(&books, &spec, |idx, _, plan, ctx| {
        let book = books
            .get(idx)
            .ok_or_else(|| CellError::Engine("missing cell".to_owned()))?;
        book.cell()
            .map_err(CellError::Engine)?
            .store()
            .evaluate_question(plan, ctx, |_| Ok(true))
    })?;
    if report.view_kind != CombinedViewKind::CombinedManagementView
        || report.is_consolidated_financial_statement
        || !report.read_only
    {
        return Err("combined management view violated read-only/non-consolidated rule".to_owned());
    }

    let egp_snap = report.cells.first().ok_or("missing EGP snapshot")?;
    let usd_snap = report.cells.get(1).ok_or("missing USD snapshot")?;
    let egp_rev = bucket_decimal_exact(
        egp_snap
            .answers
            .as_ref()
            .and_then(|a| a.get("supply.retail.store_revenue_weekly"))
            .ok_or("missing EGP revenue answer in cut")?,
        &["EGP", "ST-EGP-1", "2026-08-31"],
    )
    .ok_or("missing EGP revenue")?;
    let usd_rev = bucket_decimal_exact(
        usd_snap
            .answers
            .as_ref()
            .and_then(|a| a.get("supply.retail.store_revenue_weekly"))
            .ok_or("missing USD revenue answer in cut")?,
        &["USD", "ST-USD-1", "2026-08-31"],
    )
    .ok_or("missing USD revenue")?;

    let slices = vec![
        slice_from_cut_snapshot(
            "LE-EGP",
            "EGP",
            egp_snap,
            "supply.retail.store_revenue_weekly",
            decimal_to_scaled(egp_rev)?,
            scaled(1_600),
        )?,
        slice_from_cut_snapshot(
            "LE-USD",
            "USD",
            usd_snap,
            "supply.retail.store_revenue_weekly",
            decimal_to_scaled(usd_rev)?,
            0,
        )?,
    ];
    let seller_receipt = usd_cell.export_main_receipt(usd_cell.head()?.commits)?;
    let buyer_receipt = egp_cell.export_main_receipt(egp_cell.head()?.commits)?;
    verify_four_tier_ladder(&slices, &seller_receipt, &buyer_receipt)
}

fn seed_two_book_cells() -> Result<(EntityCell, EntityCell), String> {
    let mut egp_cell = EntityCell::open(
        &cell_id_for_entity(SECONDARY_SEED, 0),
        "LE-EGP",
        "EGP",
        "org_group_egp",
        "prod",
        DEFAULT_CLOCK_MS,
    )?;
    let mut usd_cell = EntityCell::open(
        &cell_id_for_entity(SECONDARY_SEED, 1),
        "LE-USD",
        "USD",
        "org_group_usd",
        "prod",
        DEFAULT_CLOCK_MS,
    )?;
    egp_cell.register_book("Nile Sole Prop", "sole_proprietorship")?;
    usd_cell.register_book("Delta Partnership", "partnership")?;

    let (second_book_code, _) = egp_cell.refused(
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

    for (book, store, sku, cost, rev) in [
        (&mut egp_cell, "ST-EGP-1", "SKU-DRESS", "100", "2000"),
        (&mut usd_cell, "ST-USD-1", "SKU-RING", "250", "1000"),
    ] {
        book.register_sku(sku, "dry_goods", "each")?;
        book.register_store(store, store, "CUST-CFC", "mall_store", Some("MALL-CFC"))?;
        book.record_unit_cost(sku, "COST-1", "SUP-1", cost, "2026-07-01T00:00:00Z")?;
        book.record_costed_lot(
            "LOT-1",
            store,
            sku,
            "CUST-CFC",
            "20",
            "each",
            cost,
            "2026-08-10T09:00:00Z",
        )?;
        book.record_costed_sale(
            "SALE-1",
            store,
            sku,
            "4",
            "each",
            rev,
            cost,
            "COST-1",
            "2026-09-02T12:00:00Z",
        )?;
        book.ingest_baseline_feeds(store, sku, cost, "2026-09-02T12:00:00Z", store)?;
        book.refresh_watermarks();
    }
    Ok((egp_cell, usd_cell))
}

fn verify_four_tier_ladder(
    slices: &[EntityReportingSlice],
    seller_receipt: &Receipt,
    buyer_receipt: &Receipt,
) -> Result<(), String> {
    let fx_table = [FxRateEntry {
        from_currency: "USD".to_owned(),
        to_currency: "EGP".to_owned(),
        rate_scaled: scaled(50),
        as_of: "2026-09-06T23:59:59Z".to_owned(),
        rate_table_pin: "fx:cbe-2026-09-06".to_owned(),
    }];
    verify_tiers_one_and_two(slices, &fx_table)?;
    let matched = verify_tier_three(slices, &fx_table, seller_receipt, buyer_receipt)?;
    verify_tier_four(slices, &fx_table, &matched)
}

fn verify_tiers_one_and_two(
    slices: &[EntityReportingSlice],
    fx_table: &[FxRateEntry],
) -> Result<(), String> {
    let mut bad_slices = slices.to_vec();
    if let Some(first) = bad_slices.first_mut() {
        first.query_pin.clear();
    }
    let bad_pin = evaluate_operating_group_tier(
        OperatingGroupTier::SideBySide,
        &bad_slices,
        None,
        &[],
        &[],
        None,
    );
    if !matches!(
        bad_pin,
        Err(OperatingGroupRefusal::InvalidCellCutPin { .. })
    ) {
        return Err(format!("empty query_pin must refuse, got {bad_pin:?}"));
    }

    let t1 =
        evaluate_operating_group_tier(OperatingGroupTier::SideBySide, slices, None, &[], &[], None)
            .map_err(|e| format!("tier 1 failed: {e:?}"))?;
    if t1.is_consolidated_financial_statement
        || t1
            .per_currency_revenue
            .get("EGP")
            .map(Decimal::canonical)
            .as_deref()
            != Some("2000")
        || t1
            .per_currency_revenue
            .get("USD")
            .map(Decimal::canonical)
            .as_deref()
            != Some("1000")
    {
        return Err(format!("unexpected tier 1 projection: {t1:?}"));
    }

    let missing_fx = evaluate_operating_group_tier(
        OperatingGroupTier::FxTranslated,
        slices,
        Some("EGP"),
        &[],
        &[],
        None,
    );
    if !matches!(
        missing_fx,
        Err(OperatingGroupRefusal::MissingFxTable { .. })
    ) {
        return Err(format!(
            "expected MissingFxTable refusal, got {missing_fx:?}"
        ));
    }
    let t2 = evaluate_operating_group_tier(
        OperatingGroupTier::FxTranslated,
        slices,
        Some("EGP"),
        fx_table,
        &[],
        None,
    )
    .map_err(|e| format!("tier 2 failed: {e:?}"))?;
    if t2.is_consolidated_financial_statement
        || t2
            .translated_revenue
            .as_ref()
            .map(Decimal::canonical)
            .as_deref()
            != Some("52000")
    {
        return Err(format!("unexpected tier 2 projection: {t2:?}"));
    }
    Ok(())
}

fn verify_tier_three(
    slices: &[EntityReportingSlice],
    fx_table: &[FxRateEntry],
    seller_receipt: &Receipt,
    buyer_receipt: &Receipt,
) -> Result<Vec<InterEntityTransfer>, String> {
    let mut tampered = seller_receipt.clone();
    "sha256:forged-pin".clone_into(&mut tampered.hash);
    let forged = [InterEntityTransfer {
        transfer_id: "IE-FORGED-1".to_owned(),
        seller_entity_id: "LE-USD".to_owned(),
        buyer_entity_id: "LE-EGP".to_owned(),
        currency: "USD".to_owned(),
        seller_amount_scaled: scaled(200),
        buyer_amount_scaled: scaled(200),
        seller_receipt: Some(tampered),
        buyer_receipt: Some(buyer_receipt.clone()),
    }];
    let forged_res = evaluate_operating_group_tier(
        OperatingGroupTier::InterEntityNetted,
        slices,
        Some("EGP"),
        fx_table,
        &forged,
        None,
    );
    if !matches!(
        forged_res,
        Err(OperatingGroupRefusal::UnmatchedInterEntityTransfer { .. })
    ) {
        return Err(format!(
            "tampered receipt hash must refuse, got {forged_res:?}"
        ));
    }

    let matched = vec![InterEntityTransfer {
        transfer_id: "IE-OK-1".to_owned(),
        seller_entity_id: "LE-USD".to_owned(),
        buyer_entity_id: "LE-EGP".to_owned(),
        currency: "USD".to_owned(),
        seller_amount_scaled: scaled(200),
        buyer_amount_scaled: scaled(200),
        seller_receipt: Some(seller_receipt.clone()),
        buyer_receipt: Some(buyer_receipt.clone()),
    }];
    let t3 = evaluate_operating_group_tier(
        OperatingGroupTier::InterEntityNetted,
        slices,
        Some("EGP"),
        fx_table,
        &matched,
        None,
    )
    .map_err(|e| format!("tier 3 failed: {e:?}"))?;
    if t3.is_consolidated_financial_statement
        || t3
            .net_group_revenue
            .as_ref()
            .map(Decimal::canonical)
            .as_deref()
            != Some("42000")
    {
        return Err(format!("unexpected tier 3 projection: {t3:?}"));
    }
    Ok(matched)
}

fn verify_tier_four(
    slices: &[EntityReportingSlice],
    fx_table: &[FxRateEntry],
    matched: &[InterEntityTransfer],
) -> Result<(), String> {
    let bogus_approval = StatutoryCloseApproval {
        period_week: "2026-08-31".to_owned(),
        auditor_principal: "cpa:salma".to_owned(),
        elimination_journal_pin: "elim:fabricated-string".to_owned(),
        approved_at: "2026-09-07T09:00:00Z".to_owned(),
    };
    let bad_stat = evaluate_operating_group_tier(
        OperatingGroupTier::StatutoryConsolidated,
        slices,
        Some("EGP"),
        fx_table,
        matched,
        Some(&bogus_approval),
    );
    if !matches!(
        bad_stat,
        Err(OperatingGroupRefusal::MissingStatutoryApproval { .. })
    ) {
        return Err(format!(
            "fabricated elimination_journal_pin must refuse, got {bad_stat:?}"
        ));
    }

    let authentic_pin = compute_elimination_journal_pin("2026-08-31", slices, matched)
        .map_err(|e| format!("compute elimination pin: {e:?}"))?;
    let approval = StatutoryCloseApproval {
        period_week: "2026-08-31".to_owned(),
        auditor_principal: "cpa:salma".to_owned(),
        elimination_journal_pin: authentic_pin,
        approved_at: "2026-09-07T09:00:00Z".to_owned(),
    };
    let t4 = evaluate_operating_group_tier(
        OperatingGroupTier::StatutoryConsolidated,
        slices,
        Some("EGP"),
        fx_table,
        matched,
        Some(&approval),
    )
    .map_err(|e| format!("tier 4 failed: {e:?}"))?;
    if !t4.is_consolidated_financial_statement
        || t4
            .net_group_revenue
            .as_ref()
            .map(Decimal::canonical)
            .as_deref()
            != Some("42000")
    {
        return Err(format!("unexpected tier 4 projection: {t4:?}"));
    }
    Ok(())
}
