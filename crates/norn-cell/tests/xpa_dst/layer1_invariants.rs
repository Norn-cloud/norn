//! Layer 1 — Structural & Formal Invariants for Whole-Business xP&A:
//!
//! 1. **Exact decimal & unit-partition algebra (`METRIC-UNIT-PARTITION-1`):**
//!    checker refusals (`mixed_currency`, `mixed_unit`), checked 6-decimal `i128`
//!    arithmetic, and ratio-of-sums exactness (`WOS`, `GMROI`, `gross_margin_rate`,
//!    `sell_through_rate`).
//! 2. **Temporal & grain laws:** canonical UTC ISO-week Monday binding
//!    (`period_week` <-> `period_end`), snapshot evidence cutoffs, and bitemporal
//!    restatement preservation (`ASOF-KNOWLEDGE-1` + `CellError::TemporalLeakage`)
//!    without freezing future recounts.
//! 3. **Single-book & multi-entity isolation (`READ-LOCAL-1`):** single
//!    `LegalEntity` book per cell, read-only combined management views, and the
//!    explicit 4-tier `OperatingGroup` reporting ladder (`side_by_side` ->
//!    `fx_translated` -> `inter_entity_netted` -> `statutory_consolidated`).

use std::collections::BTreeMap;

use norn_cell::combined_view::CombinedViewKind;
use norn_cell::{CellError, KnowledgeCutoff};
use norn_checker::metric::compute::arithmetic::{FACTOR, ratio};
use norn_ir::Decimal;
use serde::{Deserialize, Serialize};
use serde_json::json;

use super::cuts::{
    bucket_decimal_exact, cell_id_for_entity, dst_coverage_spec, evaluate_multi_cell_report,
};
use super::env::{DEFAULT_CLOCK_MS, EntityCell, retail_pack};
use super::prng::{
    HALF_FACTOR, PRIMARY_SEED, SECONDARY_SEED, SplitMix64, decimal_from_scaled, decimal_to_scaled,
    div_i128, exact_ratio_of_sums, scaled, week_monday_iso,
};

/// Explicit 4-tier `OperatingGroup` reporting ladder (`READ-LOCAL-1`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OperatingGroupTier {
    /// Tier 1: Read-only side-by-side per-entity figures in native currencies.
    SideBySide,
    /// Tier 2: Translated into one reporting currency via a pinned as-of FX table.
    FxTranslated,
    /// Tier 3: Translated + bilateral matched inter-entity flows eliminated.
    InterEntityNetted,
    /// Tier 4: Audited statutory consolidated financial statements with close sign-off.
    StatutoryConsolidated,
}

/// Pinned FX rate converting `1 unit` of `from_currency` into `to_currency` at `as_of`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FxRateEntry {
    pub from_currency: String,
    pub to_currency: String,
    /// 6-decimal scaled exact rate (e.g. `48_500_000` = `48.500000` EGP per USD).
    pub rate_scaled: i128,
    pub as_of: String,
    pub rate_table_pin: String,
}

/// One bilateral inter-entity stock/sale flow matched between two legal entities.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InterEntityTransfer {
    pub transfer_id: String,
    pub seller_entity_id: String,
    pub buyer_entity_id: String,
    pub currency: String,
    pub seller_amount_scaled: i128,
    pub buyer_amount_scaled: i128,
    pub seller_receipt_pin: Option<String>,
    pub buyer_receipt_pin: Option<String>,
}

/// Statutory close sign-off required exclusively for Tier 4 (`statutory_consolidated`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StatutoryCloseApproval {
    pub period_week: String,
    pub auditor_principal: String,
    pub elimination_journal_pin: String,
    pub approved_at: String,
}

/// One entity's exact local revenue & margin slice extracted from a pinned cell cut.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EntityReportingSlice {
    pub entity_id: String,
    pub currency: String,
    pub revenue_scaled: i128,
    pub gross_margin_scaled: i128,
}

/// Result of projecting an `OperatingGroup` at a requested tier.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OperatingGroupProjection {
    pub tier: OperatingGroupTier,
    pub is_consolidated_financial_statement: bool,
    pub read_only: bool,
    pub reporting_currency: Option<String>,
    pub per_currency_revenue: BTreeMap<String, Decimal>,
    pub translated_revenue: Option<Decimal>,
    pub eliminated_inter_entity_revenue: Option<Decimal>,
    pub net_group_revenue: Option<Decimal>,
}

/// Deterministic refusal codes for the 4-tier `OperatingGroup` ladder.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OperatingGroupRefusal {
    MissingFxTable {
        from_currency: String,
        to_currency: String,
    },
    UnmatchedInterEntityTransfer {
        transfer_id: String,
        reason: String,
    },
    MissingStatutoryApproval {
        detail: String,
    },
    ArithmeticOverflow {
        detail: String,
    },
}

/// Project `slices` at `requested_tier` under fail-closed evidence rules.
///
/// # Errors
/// Returns [`OperatingGroupRefusal`] whenever a higher tier lacks pinned FX,
/// bilateral matched inter-entity receipts, or statutory close approval.
pub fn evaluate_operating_group_tier(
    requested_tier: OperatingGroupTier,
    slices: &[EntityReportingSlice],
    reporting_currency: Option<&str>,
    fx_rates: &[FxRateEntry],
    inter_entity_transfers: &[InterEntityTransfer],
    statutory_approval: Option<&StatutoryCloseApproval>,
) -> Result<OperatingGroupProjection, OperatingGroupRefusal> {
    let (by_ccy_scaled, per_currency_revenue) = aggregate_by_currency(slices)?;
    if requested_tier == OperatingGroupTier::SideBySide {
        return Ok(OperatingGroupProjection {
            tier: OperatingGroupTier::SideBySide,
            is_consolidated_financial_statement: false,
            read_only: true,
            reporting_currency: None,
            per_currency_revenue,
            translated_revenue: None,
            eliminated_inter_entity_revenue: None,
            net_group_revenue: None,
        });
    }

    let target_ccy = reporting_currency.ok_or_else(|| OperatingGroupRefusal::MissingFxTable {
        from_currency: "*".to_owned(),
        to_currency: "<unspecified>".to_owned(),
    })?;
    let translated_total = translate_currency_totals(&by_ccy_scaled, target_ccy, fx_rates)?;
    let translated_dec = decimal_from_scaled(translated_total)
        .map_err(|detail| OperatingGroupRefusal::ArithmeticOverflow { detail })?;
    if requested_tier == OperatingGroupTier::FxTranslated {
        return Ok(OperatingGroupProjection {
            tier: OperatingGroupTier::FxTranslated,
            is_consolidated_financial_statement: false,
            read_only: true,
            reporting_currency: Some(target_ccy.to_owned()),
            per_currency_revenue,
            translated_revenue: Some(translated_dec),
            eliminated_inter_entity_revenue: None,
            net_group_revenue: None,
        });
    }

    let eliminated_scaled =
        sum_verified_eliminations(inter_entity_transfers, target_ccy, fx_rates)?;
    let net_scaled = translated_total
        .checked_sub(eliminated_scaled)
        .ok_or_else(|| OperatingGroupRefusal::ArithmeticOverflow {
            detail: "net group revenue underflow".to_owned(),
        })?;
    let eliminated_dec = decimal_from_scaled(eliminated_scaled)
        .map_err(|detail| OperatingGroupRefusal::ArithmeticOverflow { detail })?;
    let net_dec = decimal_from_scaled(net_scaled)
        .map_err(|detail| OperatingGroupRefusal::ArithmeticOverflow { detail })?;

    if requested_tier == OperatingGroupTier::InterEntityNetted {
        return Ok(OperatingGroupProjection {
            tier: OperatingGroupTier::InterEntityNetted,
            is_consolidated_financial_statement: false,
            read_only: true,
            reporting_currency: Some(target_ccy.to_owned()),
            per_currency_revenue,
            translated_revenue: Some(translated_dec),
            eliminated_inter_entity_revenue: Some(eliminated_dec),
            net_group_revenue: Some(net_dec),
        });
    }

    validate_statutory_approval(statutory_approval)?;
    Ok(OperatingGroupProjection {
        tier: OperatingGroupTier::StatutoryConsolidated,
        is_consolidated_financial_statement: true,
        read_only: true,
        reporting_currency: Some(target_ccy.to_owned()),
        per_currency_revenue,
        translated_revenue: Some(translated_dec),
        eliminated_inter_entity_revenue: Some(eliminated_dec),
        net_group_revenue: Some(net_dec),
    })
}

fn aggregate_by_currency(
    slices: &[EntityReportingSlice],
) -> Result<(BTreeMap<String, i128>, BTreeMap<String, Decimal>), OperatingGroupRefusal> {
    let mut by_ccy: BTreeMap<String, i128> = BTreeMap::new();
    for slice in slices {
        let entry = by_ccy.entry(slice.currency.clone()).or_insert(0);
        *entry = entry.checked_add(slice.revenue_scaled).ok_or_else(|| {
            OperatingGroupRefusal::ArithmeticOverflow {
                detail: format!("revenue sum overflow for {}", slice.currency),
            }
        })?;
    }
    let mut formatted = BTreeMap::new();
    for (ccy, val) in &by_ccy {
        let dec = decimal_from_scaled(*val)
            .map_err(|detail| OperatingGroupRefusal::ArithmeticOverflow { detail })?;
        formatted.insert(ccy.clone(), dec);
    }
    Ok((by_ccy, formatted))
}

fn translate_currency_totals(
    by_ccy: &BTreeMap<String, i128>,
    target_ccy: &str,
    fx_rates: &[FxRateEntry],
) -> Result<i128, OperatingGroupRefusal> {
    let mut total: i128 = 0;
    for (ccy, val) in by_ccy {
        let converted = convert_scaled(*val, ccy, target_ccy, fx_rates)?;
        total = total.checked_add(converted).ok_or_else(|| {
            OperatingGroupRefusal::ArithmeticOverflow {
                detail: "translated revenue sum overflow".to_owned(),
            }
        })?;
    }
    Ok(total)
}

fn sum_verified_eliminations(
    transfers: &[InterEntityTransfer],
    target_ccy: &str,
    fx_rates: &[FxRateEntry],
) -> Result<i128, OperatingGroupRefusal> {
    let mut eliminated: i128 = 0;
    for xfer in transfers {
        if xfer.seller_entity_id == xfer.buyer_entity_id {
            return Err(OperatingGroupRefusal::UnmatchedInterEntityTransfer {
                transfer_id: xfer.transfer_id.clone(),
                reason: "self-transfer cannot be an inter-entity elimination".to_owned(),
            });
        }
        if xfer.seller_receipt_pin.is_none() || xfer.buyer_receipt_pin.is_none() {
            return Err(OperatingGroupRefusal::UnmatchedInterEntityTransfer {
                transfer_id: xfer.transfer_id.clone(),
                reason: "missing bilateral receipt pin on seller or buyer side".to_owned(),
            });
        }
        if xfer.seller_amount_scaled != xfer.buyer_amount_scaled {
            return Err(OperatingGroupRefusal::UnmatchedInterEntityTransfer {
                transfer_id: xfer.transfer_id.clone(),
                reason: format!(
                    "asymmetric bilateral amounts: seller={} buyer={}",
                    xfer.seller_amount_scaled, xfer.buyer_amount_scaled
                ),
            });
        }
        let converted = convert_scaled(
            xfer.seller_amount_scaled,
            &xfer.currency,
            target_ccy,
            fx_rates,
        )?;
        eliminated = eliminated.checked_add(converted).ok_or_else(|| {
            OperatingGroupRefusal::ArithmeticOverflow {
                detail: "eliminated inter-entity sum overflow".to_owned(),
            }
        })?;
    }
    Ok(eliminated)
}

fn validate_statutory_approval(
    approval: Option<&StatutoryCloseApproval>,
) -> Result<(), OperatingGroupRefusal> {
    let Some(signoff) = approval else {
        return Err(OperatingGroupRefusal::MissingStatutoryApproval {
            detail: "statutory_consolidated requires auditor close approval and elimination pin"
                .to_owned(),
        });
    };
    if signoff.auditor_principal.trim().is_empty()
        || signoff.elimination_journal_pin.trim().is_empty()
    {
        return Err(OperatingGroupRefusal::MissingStatutoryApproval {
            detail: "auditor_principal and elimination_journal_pin must be non-empty".to_owned(),
        });
    }
    Ok(())
}

fn convert_scaled(
    amount_scaled: i128,
    from_ccy: &str,
    to_ccy: &str,
    fx_rates: &[FxRateEntry],
) -> Result<i128, OperatingGroupRefusal> {
    if from_ccy == to_ccy {
        return Ok(amount_scaled);
    }
    let rate = fx_rates
        .iter()
        .find(|r| {
            r.from_currency == from_ccy
                && r.to_currency == to_ccy
                && !r.as_of.trim().is_empty()
                && !r.rate_table_pin.trim().is_empty()
                && r.rate_scaled > 0
        })
        .ok_or_else(|| OperatingGroupRefusal::MissingFxTable {
            from_currency: from_ccy.to_owned(),
            to_currency: to_ccy.to_owned(),
        })?;
    let product = amount_scaled.checked_mul(rate.rate_scaled).ok_or_else(|| {
        OperatingGroupRefusal::ArithmeticOverflow {
            detail: format!("FX multiply overflow for {from_ccy}->{to_ccy}"),
        }
    })?;
    let adjusted = if product >= 0 {
        product.checked_add(HALF_FACTOR)
    } else {
        product.checked_sub(HALF_FACTOR)
    }
    .ok_or_else(|| OperatingGroupRefusal::ArithmeticOverflow {
        detail: "FX rounding overflow".to_owned(),
    })?;
    Ok(div_i128(adjusted, FACTOR))
}

/// Layer 1A: Verify `METRIC-UNIT-PARTITION-1`, checked exact decimal algebra,
/// and ratio-of-sums exactness (never average-of-ratios).
///
/// # Errors
/// Returns an error if any structural invariant or refusal fails.
pub fn verify_decimal_and_unit_partition_laws() -> Result<(), String> {
    verify_unit_partition_checker_refusals()?;
    verify_checked_decimal_and_ratio_algebra()
}

fn verify_unit_partition_checker_refusals() -> Result<(), String> {
    let pack = retail_pack()?;
    let mut mixed_ccy_q = pack.retail_question_ir("supply.retail.store_revenue_weekly")?;
    mixed_ccy_q
        .dimensions
        .retain(|dim| dim.path.property != "currency");
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
        .retain(|dim| dim.path.property != "unit");
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
        let exact_gmr = exact_ratio_of_sums(sum_gm, sum_rev)?.ok_or("expected exact ratio")?;
        let direct = ratio(sum_gm, sum_rev)
            .map_err(|e| format!("ratio: {e:?}"))?
            .ok_or("direct ratio")?;
        if exact_gmr != direct {
            return Err(format!("ratio mismatch: {exact_gmr:?} vs {direct:?}"));
        }
        let neg_gm = sum_gm.checked_neg().ok_or("checked_neg overflow")?;
        if sum_gm.checked_add(neg_gm) != Some(0) {
            return Err("exact additive inverse failed".to_owned());
        }
        if exact_ratio_of_sums(sum_gm, 0)?.is_some() {
            return Err("zero denominator must return None".to_owned());
        }
        let round_trip = decimal_to_scaled(decimal_from_scaled(sum_gm)?)?;
        if round_trip != sum_gm {
            return Err("decimal_from_scaled round-trip mismatch".to_owned());
        }
    }

    let checks = [
        (scaled(2_400), scaled(3_800), "0.631579", "GM rate"),
        (scaled(60), scaled(10), "6", "WOS"),
        (scaled(5_000), scaled(8_000), "0.625", "GMROI"),
        (scaled(40), scaled(100), "0.4", "sell-through"),
    ];
    for (num, den, want, label) in checks {
        let got = exact_ratio_of_sums(num, den)?
            .ok_or_else(|| format!("expected {label}"))?
            .canonical();
        if got != want {
            return Err(format!("{label}: expected {want}, got {got}"));
        }
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
    book.ok(
        "supply.stock_valuation_cut.record",
        "planner:omar",
        json!({
            "cut_id": "CUT-T1-W6", "store_id": "ST-T1", "sku_id": "SKU-A",
            "unit": "each", "currency": "EGP", "cost_status": "costed",
            "book_epoch": "1970-01-01T00:00:00Z", "cut_at": "2026-08-31T00:00:00Z",
            "remaining_qty": "50", "stock_value": "5000",
        }),
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

    // Anti-leakage: naming K2's frontier under K1's earlier clock MUST refuse `TemporalLeakage`.
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
/// combined management views, and the explicit 4-tier `OperatingGroup` ladder.
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

    let egp_rev = bucket_decimal_exact(
        &egp_cell.answer("supply.retail.store_revenue_weekly")?,
        &["EGP", "ST-EGP-1", "2026-08-31"],
    )
    .ok_or("missing EGP revenue")?;
    let usd_rev = bucket_decimal_exact(
        &usd_cell.answer("supply.retail.store_revenue_weekly")?,
        &["USD", "ST-USD-1", "2026-08-31"],
    )
    .ok_or("missing USD revenue")?;
    let slices = vec![
        EntityReportingSlice {
            entity_id: "LE-EGP".to_owned(),
            currency: "EGP".to_owned(),
            revenue_scaled: decimal_to_scaled(egp_rev)?,
            gross_margin_scaled: scaled(1_600),
        },
        EntityReportingSlice {
            entity_id: "LE-USD".to_owned(),
            currency: "USD".to_owned(),
            revenue_scaled: decimal_to_scaled(usd_rev)?,
            gross_margin_scaled: 0,
        },
    ];
    verify_four_tier_ladder(&slices)
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
    }
    Ok((egp_cell, usd_cell))
}

fn verify_four_tier_ladder(slices: &[EntityReportingSlice]) -> Result<(), String> {
    let fx_table = [FxRateEntry {
        from_currency: "USD".to_owned(),
        to_currency: "EGP".to_owned(),
        rate_scaled: scaled(50),
        as_of: "2026-09-06T23:59:59Z".to_owned(),
        rate_table_pin: "fx:cbe-2026-09-06".to_owned(),
    }];
    verify_tiers_one_and_two(slices, &fx_table)?;
    verify_tiers_three_and_four(slices, &fx_table)
}

fn verify_tiers_one_and_two(
    slices: &[EntityReportingSlice],
    fx_table: &[FxRateEntry],
) -> Result<(), String> {
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
        || t2.translated_revenue.map(Decimal::canonical).as_deref() != Some("52000")
    {
        return Err(format!("unexpected tier 2 projection: {t2:?}"));
    }
    Ok(())
}

fn verify_tiers_three_and_four(
    slices: &[EntityReportingSlice],
    fx_table: &[FxRateEntry],
) -> Result<(), String> {
    let unmatched = [InterEntityTransfer {
        transfer_id: "IE-BAD-1".to_owned(),
        seller_entity_id: "LE-USD".to_owned(),
        buyer_entity_id: "LE-EGP".to_owned(),
        currency: "USD".to_owned(),
        seller_amount_scaled: scaled(200),
        buyer_amount_scaled: scaled(180),
        seller_receipt_pin: Some("rcpt:seller-1".to_owned()),
        buyer_receipt_pin: Some("rcpt:buyer-1".to_owned()),
    }];
    let bad_ie = evaluate_operating_group_tier(
        OperatingGroupTier::InterEntityNetted,
        slices,
        Some("EGP"),
        fx_table,
        &unmatched,
        None,
    );
    if !matches!(
        bad_ie,
        Err(OperatingGroupRefusal::UnmatchedInterEntityTransfer { .. })
    ) {
        return Err(format!("asymmetric transfer must refuse, got {bad_ie:?}"));
    }

    let matched = [InterEntityTransfer {
        transfer_id: "IE-OK-1".to_owned(),
        seller_entity_id: "LE-USD".to_owned(),
        buyer_entity_id: "LE-EGP".to_owned(),
        currency: "USD".to_owned(),
        seller_amount_scaled: scaled(200),
        buyer_amount_scaled: scaled(200),
        seller_receipt_pin: Some("rcpt:seller-1".to_owned()),
        buyer_receipt_pin: Some("rcpt:buyer-1".to_owned()),
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
        || t3.net_group_revenue.map(Decimal::canonical).as_deref() != Some("42000")
    {
        return Err(format!("unexpected tier 3 projection: {t3:?}"));
    }

    let no_approval = evaluate_operating_group_tier(
        OperatingGroupTier::StatutoryConsolidated,
        slices,
        Some("EGP"),
        fx_table,
        &matched,
        None,
    );
    if !matches!(
        no_approval,
        Err(OperatingGroupRefusal::MissingStatutoryApproval { .. })
    ) {
        return Err(format!(
            "Tier 4 without approval must refuse, got {no_approval:?}"
        ));
    }
    let approval = StatutoryCloseApproval {
        period_week: "2026-08-31".to_owned(),
        auditor_principal: "cpa:salma".to_owned(),
        elimination_journal_pin: "elim:2026-08-31:sha256:abc".to_owned(),
        approved_at: "2026-09-07T09:00:00Z".to_owned(),
    };
    let t4 = evaluate_operating_group_tier(
        OperatingGroupTier::StatutoryConsolidated,
        slices,
        Some("EGP"),
        fx_table,
        &matched,
        Some(&approval),
    )
    .map_err(|e| format!("tier 4 failed: {e:?}"))?;
    if !t4.is_consolidated_financial_statement
        || t4.net_group_revenue.map(Decimal::canonical).as_deref() != Some("42000")
    {
        return Err(format!("unexpected tier 4 projection: {t4:?}"));
    }
    Ok(())
}
