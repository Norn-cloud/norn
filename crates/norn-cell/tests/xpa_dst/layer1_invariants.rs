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
use norn_cell::{CellError, KnowledgeCutoff};
use norn_checker::metric::compute::arithmetic::ratio;
use norn_ir::Decimal;
use serde_json::json;

use super::actions::StoreKpiInput;
use super::cuts::{
    TwoBookFixture, bucket_decimal_exact, cell_id_for_entity, dst_coverage_spec,
    evaluate_multi_cell_report, seed_two_book_cells,
};
use super::env::{DEFAULT_CLOCK_MS, EntityCell, retail_pack};
use super::operating_group::{
    EntityReportingSlice, FxRateEntry, InterEntityTransfer, OperatingGroupRefusal,
    OperatingGroupTier, StatutoryCloseApproval, TransferLegEvidence,
    compute_elimination_journal_pin, evaluate_operating_group_tier, slice_from_cut_snapshot,
    transfer_lot_id, transfer_sale_id,
};
use super::prng::{
    PRIMARY_SEED, SplitMix64, decimal_from_scaled, decimal_to_scaled, div_i128,
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
    for (question, dim_prop, expected_code) in [
        (
            "supply.retail.store_revenue_weekly",
            "currency",
            "metric.validation.mixed_currency",
        ),
        (
            "supply.retail.store_sales_weekly",
            "unit",
            "metric.validation.mixed_unit",
        ),
    ] {
        let mut ir = pack.retail_question_ir(question)?;
        ir.dimensions
            .retain(|dim| dim.path.property.as_str() != dim_prop);
        let err = pack
            .admit_question_ir(&ir, 200)
            .err()
            .ok_or_else(|| format!("unpartitioned {dim_prop} question MUST refuse"))?;
        if !err.contains(expected_code) {
            return Err(format!("expected {expected_code}, got: {err}"));
        }
    }
    Ok(())
}

fn verify_checked_decimal_and_ratio_algebra() -> Result<(), String> {
    let mut rng = SplitMix64::new(PRIMARY_SEED);
    for _ in 0..64 {
        let primary_revenue = scaled(i128::from(rng.range_u64(100, 10_000)));
        let secondary_revenue = scaled(i128::from(rng.range_u64(100, 10_000)));
        let primary_cogs =
            div_i128(primary_revenue, 4).saturating_mul(i128::from(rng.range_u64(1, 3)));
        let secondary_cogs =
            div_i128(secondary_revenue, 5).saturating_mul(i128::from(rng.range_u64(1, 4)));
        let sum_gm = primary_revenue
            .checked_sub(primary_cogs)
            .and_then(|first_gm| {
                secondary_revenue
                    .checked_sub(secondary_cogs)
                    .and_then(|second_gm| first_gm.checked_add(second_gm))
            })
            .ok_or("checked margin overflow")?;
        let sum_rev = primary_revenue
            .checked_add(secondary_revenue)
            .ok_or("checked revenue overflow")?;
        let engine_scaled = ratio(sum_gm, sum_rev).ok_or("engine ratio returned None")?;
        let oracle_scaled =
            independent_ratio_scaled(sum_gm, sum_rev).ok_or("independent oracle returned None")?;
        if engine_scaled != oracle_scaled {
            return Err(format!(
                "ratio oracle divergence: engine={engine_scaled} oracle={oracle_scaled}"
            ));
        }
        let exact_dec = exact_ratio_of_sums(sum_gm, sum_rev)?.ok_or("expected exact ratio")?;
        if exact_dec != decimal_from_scaled(oracle_scaled)? {
            return Err(format!("exact_ratio_of_sums mismatch: {exact_dec:?}"));
        }
        if sum_gm.checked_neg().and_then(|neg| sum_gm.checked_add(neg)) != Some(0) {
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
    for (numerator, denominator, expected_str, label) in checks {
        let got = exact_ratio_of_sums(numerator, denominator)?
            .ok_or_else(|| format!("expected {label}"))?
            .canonical();
        let oracle_dec = decimal_from_scaled(
            independent_ratio_scaled(numerator, denominator)
                .ok_or_else(|| format!("oracle none for {label}"))?,
        )?
        .canonical();
        if got != expected_str || oracle_dec != expected_str {
            return Err(format!(
                "{label}: expected {expected_str}, got engine={got}, oracle={oracle_dec}"
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
    // Unequal weights: sale 1 GM rate = 0.2; sale 2 GM rate = 0.6 -> ratio-of-sums = 0.5.
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
    numerator_scaled: i128,
    denominator_scaled: i128,
    expected_text: &str,
) -> Result<(), String> {
    let actual = bucket_decimal_exact(&book.answer(question)?, bucket)
        .ok_or_else(|| format!("{question}: missing bucket {bucket:?}"))?;
    let oracle_scaled = independent_ratio_scaled(numerator_scaled, denominator_scaled)
        .ok_or("independent oracle None")?;
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
    let prior_cutoff = KnowledgeCutoff {
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
    verify_bitemporal_recount_preservation(&mut book, prior_cutoff)
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
    )?;
    Ok(book)
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
    prior_cutoff: KnowledgeCutoff,
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
    let recounted_cutoff = KnowledgeCutoff {
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

    let mut prior_ctx = book.query_context();
    prior_ctx.now_ms = prior_cutoff.recorded_at.as_millis();
    let hist = book
        .cell()?
        .store()
        .evaluate_question_known_at(&avail_plan, prior_ctx.clone(), prior_cutoff, |_| Ok(true))
        .map_err(|e| format!("evaluate_question_known_at prior_cutoff: {e}"))?;
    let hist_val = bucket_decimal_exact(&hist.result, &["each", "ST-T1", "2026-08-31"])
        .ok_or("missing prior historical availability")?;
    if hist_val.canonical() != "38" {
        return Err(format!(
            "ASOF-KNOWLEDGE-1 violated: expected prior availability 38, got {hist_val}"
        ));
    }

    let leak_cutoff = KnowledgeCutoff {
        recorded_at: prior_cutoff.recorded_at,
        frontier: recounted_cutoff.frontier,
    };
    let leak_res = book.cell()?.store().evaluate_question_known_at(
        &avail_plan,
        prior_ctx,
        leak_cutoff,
        |_| Ok(true),
    );
    if !matches!(leak_res, Err(CellError::TemporalLeakage)) {
        return Err(format!(
            "expected CellError::TemporalLeakage on future frontier, got {leak_res:?}"
        ));
    }
    Ok(())
}

/// Layer 1C: Verify single-book-per-cell isolation (`READ-LOCAL-1`), read-only
/// combined management views, and the fail-closed 4-tier `OperatingGroup`
/// projection ladder bound to real cell cuts and leg-bound verified `main`
/// export records.
///
/// # Errors
/// Returns an error if any isolation or ladder refusal rule fails.
pub fn verify_single_book_and_operating_group_ladder() -> Result<(), String> {
    let fixture = seed_two_book_cells()?;
    let spec = dst_coverage_spec();
    let entity_books = [&fixture.sole_egp_book, &fixture.partner_usd_book];
    let report = evaluate_multi_cell_report(&entity_books, &spec, |idx, _, plan, ctx| {
        let current = entity_books
            .get(idx)
            .ok_or_else(|| CellError::Engine("missing cell".to_owned()))?;
        current
            .cell()
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

    let sole_snapshot = report.cells.first().ok_or("missing EGP snapshot")?;
    let partner_snapshot = report.cells.get(1).ok_or("missing USD snapshot")?;
    let slices = vec![
        slice_from_cut_snapshot(
            "LE-EGP",
            "EGP",
            sole_snapshot,
            "supply.retail.store_revenue_weekly",
        )?,
        slice_from_cut_snapshot(
            "LE-USD",
            "USD",
            partner_snapshot,
            "supply.retail.store_revenue_weekly",
        )?,
    ];
    verify_four_tier_ladder(&entity_books, &slices, &fixture)
}

fn verify_four_tier_ladder(
    cells: &[&EntityCell],
    slices: &[EntityReportingSlice],
    fixture: &TwoBookFixture,
) -> Result<(), String> {
    let fx_table = [FxRateEntry {
        from_currency: "USD".to_owned(),
        to_currency: "EGP".to_owned(),
        rate_scaled: scaled(50),
        as_of: "2026-09-06T23:59:59Z".to_owned(),
        rate_table_pin: "fx:cbe-2026-09-06".to_owned(),
    }];
    verify_tiers_one_and_two(cells, slices, &fx_table)?;
    let matched = verify_tier_three(cells, slices, &fx_table, fixture)?;
    verify_tier_four(cells, slices, &fx_table, &matched)
}

fn verify_tiers_one_and_two(
    cells: &[&EntityCell],
    slices: &[EntityReportingSlice],
    fx_table: &[FxRateEntry],
) -> Result<(), String> {
    assert_tampered_slices_refused(cells, slices)?;
    let side_by_side_proj = evaluate_operating_group_tier(
        OperatingGroupTier::SideBySide,
        cells,
        slices,
        None,
        &[],
        &[],
        None,
    )
    .map_err(|e| format!("tier 1 failed: {e:?}"))?;
    if side_by_side_proj.is_consolidated_financial_statement
        || side_by_side_proj
            .per_currency_revenue
            .get("EGP")
            .map(Decimal::canonical)
            .as_deref()
            != Some("2000")
        || side_by_side_proj
            .per_currency_revenue
            .get("USD")
            .map(Decimal::canonical)
            .as_deref()
            != Some("1000")
    {
        return Err(format!(
            "unexpected tier 1 projection: {side_by_side_proj:?}"
        ));
    }

    if !matches!(
        evaluate_operating_group_tier(
            OperatingGroupTier::FxTranslated,
            cells,
            slices,
            Some("EGP"),
            &[],
            &[],
            None,
        ),
        Err(OperatingGroupRefusal::MissingFxTable { .. })
    ) {
        return Err("expected MissingFxTable refusal".to_owned());
    }
    let fx_translated_proj = evaluate_operating_group_tier(
        OperatingGroupTier::FxTranslated,
        cells,
        slices,
        Some("EGP"),
        fx_table,
        &[],
        None,
    )
    .map_err(|e| format!("tier 2 failed: {e:?}"))?;
    if fx_translated_proj.is_consolidated_financial_statement
        || fx_translated_proj
            .translated_revenue
            .as_ref()
            .map(Decimal::canonical)
            .as_deref()
            != Some("52000")
    {
        return Err(format!(
            "unexpected tier 2 projection: {fx_translated_proj:?}"
        ));
    }
    Ok(())
}

fn assert_tampered_slices_refused(
    cells: &[&EntityCell],
    slices: &[EntityReportingSlice],
) -> Result<(), String> {
    let mut bad_pin_slices = slices.to_vec();
    if let Some(first) = bad_pin_slices.first_mut() {
        "forged-query-pin".clone_into(&mut first.query_pin);
    }
    if !matches!(
        evaluate_operating_group_tier(
            OperatingGroupTier::SideBySide,
            cells,
            &bad_pin_slices,
            None,
            &[],
            &[],
            None,
        ),
        Err(OperatingGroupRefusal::InvalidCellCutPin { .. })
    ) {
        return Err("mismatched query_pin must refuse InvalidCellCutPin".to_owned());
    }
    let mut mutated_row_slices = slices.to_vec();
    if let Some(first) = mutated_row_slices.first_mut()
        && let Some(row) = first.revenue_answer.rows.values_mut().next()
    {
        row.value = Some(Decimal::parse("999999").map_err(|e| format!("{e}"))?);
    }
    if !matches!(
        evaluate_operating_group_tier(
            OperatingGroupTier::SideBySide,
            cells,
            &mutated_row_slices,
            None,
            &[],
            &[],
            None,
        ),
        Err(OperatingGroupRefusal::InvalidCellCutPin { .. })
    ) {
        return Err("mutated QueryResult row value must refuse InvalidCellCutPin".to_owned());
    }
    Ok(())
}

fn verify_tier_three(
    cells: &[&EntityCell],
    slices: &[EntityReportingSlice],
    fx_table: &[FxRateEntry],
    fixture: &TwoBookFixture,
) -> Result<Vec<InterEntityTransfer>, String> {
    let valid_seller_leg = TransferLegEvidence {
        export_record: fixture.seller_transfer_export.clone(),
        object_type: "supply.PosSale".to_owned(),
        object_id: transfer_sale_id("IE-OK-1"),
    };
    let valid_buyer_leg = TransferLegEvidence {
        export_record: fixture.buyer_transfer_export.clone(),
        object_type: "supply.StockLot".to_owned(),
        object_id: transfer_lot_id("IE-OK-1"),
    };
    verify_tier_three_refusals(
        cells,
        slices,
        fx_table,
        fixture,
        &valid_seller_leg,
        &valid_buyer_leg,
    )?;

    let single_xfer = InterEntityTransfer {
        transfer_id: "IE-OK-1".to_owned(),
        seller_entity_id: "LE-USD".to_owned(),
        buyer_entity_id: "LE-EGP".to_owned(),
        currency: "USD".to_owned(),
        settlement_fx_rate: None,
        seller_leg: Some(valid_seller_leg),
        buyer_leg: Some(valid_buyer_leg),
    };
    // Submitting the exact same transfer leg pair twice MUST refuse duplicate_transfer_leg!
    let duplicate_pair = [single_xfer.clone(), single_xfer.clone()];
    assert_transfers_refused(
        "duplicate transfer leg pair",
        cells,
        slices,
        fx_table,
        &duplicate_pair,
        Some("duplicate_transfer_leg"),
    )?;
    let matched = vec![single_xfer];
    let netted_proj = evaluate_operating_group_tier(
        OperatingGroupTier::InterEntityNetted,
        cells,
        slices,
        Some("EGP"),
        fx_table,
        &matched,
        None,
    )
    .map_err(|e| format!("tier 3 failed: {e:?}"))?;
    if netted_proj.is_consolidated_financial_statement
        || netted_proj
            .net_group_revenue
            .as_ref()
            .map(Decimal::canonical)
            .as_deref()
            != Some("42000")
    {
        return Err(format!("unexpected tier 3 projection: {netted_proj:?}"));
    }
    Ok(matched)
}

fn two_book_xfer(
    id: &str,
    rate: Option<FxRateEntry>,
    seller_leg: TransferLegEvidence,
    buyer_leg: TransferLegEvidence,
) -> InterEntityTransfer {
    InterEntityTransfer {
        transfer_id: id.to_owned(),
        seller_entity_id: "LE-USD".to_owned(),
        buyer_entity_id: "LE-EGP".to_owned(),
        currency: "USD".to_owned(),
        settlement_fx_rate: rate,
        seller_leg: Some(seller_leg),
        buyer_leg: Some(buyer_leg),
    }
}

fn verify_tier_three_refusals(
    cells: &[&EntityCell],
    slices: &[EntityReportingSlice],
    fx_table: &[FxRateEntry],
    fixture: &TwoBookFixture,
    valid_seller_leg: &TransferLegEvidence,
    valid_buyer_leg: &TransferLegEvidence,
) -> Result<(), String> {
    let leg = |rec: &norn_cell::export::ExportRecord, kind: &str, id: &str| TransferLegEvidence {
        export_record: rec.clone(),
        object_type: kind.to_owned(),
        object_id: id.to_owned(),
    };
    let mut tampered = fixture.seller_transfer_export.clone();
    "sha256:forged-pin".clone_into(&mut tampered.receipt.hash);
    let wrong_spot = FxRateEntry {
        from_currency: "USD".to_owned(),
        to_currency: "EGP".to_owned(),
        rate_scaled: scaled(40),
        as_of: "2026-09-02T12:30:00Z".to_owned(),
        rate_table_pin: "fx:wrong-spot".to_owned(),
    };
    let s_id = transfer_sale_id("IE-OK-1");
    let l_id = transfer_lot_id("IE-OK-1");
    let cases = [
        (
            "tampered receipt hash",
            two_book_xfer(
                "IE-OK-1",
                None,
                leg(&tampered, "supply.PosSale", &s_id),
                valid_buyer_leg.clone(),
            ),
        ),
        (
            "unrelated ledger-tip export",
            two_book_xfer(
                "IE-OK-1",
                None,
                valid_seller_leg.clone(),
                leg(&fixture.unrelated_tip_export, "supply.StockLot", &l_id),
            ),
        ),
        (
            "wrong-cell export record",
            two_book_xfer(
                "IE-OK-1",
                None,
                leg(&fixture.buyer_transfer_export, "supply.PosSale", &s_id),
                valid_buyer_leg.clone(),
            ),
        ),
        (
            "unrelated same-amount sale & lot",
            two_book_xfer(
                "IE-UNRELATED-AMT",
                None,
                leg(
                    &fixture.unrelated_same_amt_sale_export,
                    "supply.PosSale",
                    "SALE-USD-EXT-A",
                ),
                leg(
                    &fixture.unrelated_same_amt_lot_export,
                    "supply.StockLot",
                    "LOT-EGP-UNRELATED",
                ),
            ),
        ),
        (
            "mismatched bilateral amounts",
            two_book_xfer(
                "IE-OK-1",
                Some(wrong_spot),
                valid_seller_leg.clone(),
                valid_buyer_leg.clone(),
            ),
        ),
    ];
    for (label, case) in cases {
        assert_transfers_refused(label, cells, slices, fx_table, &[case], None)?;
    }
    Ok(())
}

fn assert_transfers_refused(
    label: &str,
    cells: &[&EntityCell],
    slices: &[EntityReportingSlice],
    fx_table: &[FxRateEntry],
    transfers: &[InterEntityTransfer],
    expected_substr: Option<&str>,
) -> Result<(), String> {
    let outcome = evaluate_operating_group_tier(
        OperatingGroupTier::InterEntityNetted,
        cells,
        slices,
        Some("EGP"),
        fx_table,
        transfers,
        None,
    );
    match outcome {
        Err(OperatingGroupRefusal::UnmatchedInterEntityTransfer { reason, .. }) => {
            if let Some(sub) = expected_substr
                && !reason.contains(sub)
            {
                return Err(format!("{label}: expected `{sub}` in `{reason}`"));
            }
            Ok(())
        }
        other => Err(format!("{label} must refuse, got {other:?}")),
    }
}

fn verify_tier_four(
    cells: &[&EntityCell],
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
    if !matches!(
        evaluate_operating_group_tier(
            OperatingGroupTier::StatutoryConsolidated,
            cells,
            slices,
            Some("EGP"),
            fx_table,
            matched,
            Some(&bogus_approval),
        ),
        Err(OperatingGroupRefusal::MissingStatutoryApproval { .. })
    ) {
        return Err("fabricated elimination_journal_pin must refuse".to_owned());
    }

    let authentic_pin = compute_elimination_journal_pin("2026-08-31", slices, matched)
        .map_err(|e| format!("compute elimination pin: {e:?}"))?;
    let approval = StatutoryCloseApproval {
        period_week: "2026-08-31".to_owned(),
        auditor_principal: "cpa:salma".to_owned(),
        elimination_journal_pin: authentic_pin,
        approved_at: "2026-09-07T09:00:00Z".to_owned(),
    };
    let statutory_proj = evaluate_operating_group_tier(
        OperatingGroupTier::StatutoryConsolidated,
        cells,
        slices,
        Some("EGP"),
        fx_table,
        matched,
        Some(&approval),
    )
    .map_err(|e| format!("tier 4 failed: {e:?}"))?;
    if !statutory_proj.is_consolidated_financial_statement
        || statutory_proj
            .net_group_revenue
            .as_ref()
            .map(Decimal::canonical)
            .as_deref()
            != Some("42000")
    {
        return Err(format!("unexpected tier 4 projection: {statutory_proj:?}"));
    }
    Ok(())
}
