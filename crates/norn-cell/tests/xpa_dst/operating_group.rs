//! Test-harness 4-tier `OperatingGroup` reporting projection model (`sampled`).
//!
//! Projects independent legal-entity cell cuts (`READ-LOCAL-1`) through four
//! explicit, fail-closed reporting tiers:
//! 1. `side_by_side` (native currencies, read-only, non-consolidated)
//! 2. `fx_translated` (pinned as-of FX rate table required)
//! 3. `inter_entity_netted` (bilateral matched `main` commit [`ExportRecord`]s
//!    cryptographically verified via [`verify_main_export_record`], verified
//!    against the owning cell's ledger at `commit_seq`, bound to a shared
//!    committed transfer identity on BOTH seller and buyer legs, and guarded
//!    against leg replay/reuse via a `BTreeSet` of seen leg keys)
//! 4. `statutory_consolidated` (auditor close approval bound to the canonical
//!    `Norn1` elimination journal pin over authenticated cuts and transfer legs)
//!
//! Every entity revenue total is re-verified directly against the owning cell's
//! ledger at `cut_revision` via `evaluate_question_known_at` — mutated rows,
//! forged pins, or mismatched cuts are refused fail-closed.
//!
//! **Honest evidence note:** Production `OperatingGroup` consolidation kernel
//! and pack seams are `unknown` / pending S4. This module is a `sampled`
//! test-harness projection model over real `CellCutSnapshot`s and real `main`
//! commit export records.

use std::collections::{BTreeMap, BTreeSet};

use norn_cell::KnowledgeCutoff;
use norn_cell::combined_view::CellCutSnapshot;
use norn_cell::compute::QueryResult;
use norn_cell::export::ExportRecord;
use norn_checker::metric::compute::arithmetic::FACTOR;
use norn_ir::Decimal;
use norn_ir::canonical::HashProfile;
use norn_ir::cell_id::CellId;
use norn_kernel::LogicalTime;
use norn_kernel::read_envelope::{Completeness, ReadSemantics, ReadToken};
use serde::{Deserialize, Serialize};
use serde_json::json;

use super::cuts::bucket_parts;
use super::env::EntityCell;
use super::operating_group_transfers::{extract_seller_sale_leg, sum_verified_eliminations};
use super::prng::{HALF_FACTOR, decimal_from_scaled, decimal_to_scaled, div_i128};

/// Explicit 4-tier `OperatingGroup` reporting ladder (`sampled` test-harness
/// model over real `READ-LOCAL-1` cell cuts; production seams pending S4).
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

/// Cryptographically verifiable evidence for one leg of a bilateral
/// inter-entity transfer: the owning cell's `main` [`ExportRecord`] plus the
/// committed transfer object type and identifier (`supply.PosSale` on seller,
/// `supply.StockLot` on buyer).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TransferLegEvidence {
    pub export_record: ExportRecord,
    pub object_type: String,
    pub object_id: String,
}

/// One bilateral inter-entity flow matched by cryptographically verified `main`
/// commit [`ExportRecord`]s from both legal-entity cells. Transfer amounts are
/// extracted directly from the verified committed object payloads (never
/// accepted as unverified caller scalars).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InterEntityTransfer {
    pub transfer_id: String,
    pub seller_entity_id: String,
    pub buyer_entity_id: String,
    pub currency: String,
    /// Optional pinned spot FX rate converting `seller_currency -> buyer_currency`
    /// when the two legal entities keep books in different currencies. When
    /// `None`, the reporting `fx_rates` table is consulted.
    pub settlement_fx_rate: Option<FxRateEntry>,
    pub seller_leg: Option<TransferLegEvidence>,
    pub buyer_leg: Option<TransferLegEvidence>,
}

/// Statutory close sign-off required exclusively for Tier 4 (`statutory_consolidated`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StatutoryCloseApproval {
    pub period_week: String,
    pub auditor_principal: String,
    pub elimination_journal_pin: String,
    pub approved_at: String,
}

/// One entity's authenticated reporting slice carrying its disclosed
/// [`QueryResult`] and pinned [`CellCutSnapshot`] metadata (`cell_id`,
/// `question_name`, `window_weeks`, `query_pin`, `definition_pin`, and
/// `read_token`). Every slice is re-verified directly against the owning
/// [`EntityCell`] at `cut_revision` before any tier projection succeeds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EntityReportingSlice {
    pub entity_id: String,
    pub cell_id: CellId,
    pub currency: String,
    pub question_name: String,
    pub window_weeks: Option<u32>,
    pub query_pin: String,
    pub definition_pin: String,
    pub read_token: ReadToken,
    pub revenue_answer: QueryResult,
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
    InvalidCellCutPin {
        entity_id: String,
        reason: String,
    },
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

/// Extract an authenticated [`EntityReportingSlice`] directly from a real
/// complete [`CellCutSnapshot`] and its disclosed revenue [`QueryResult`]
/// using the pack's default question window.
///
/// # Errors
/// Returns an error if the snapshot is incomplete, lacks the requested
/// question, or carries inconsistent pins.
pub fn slice_from_cut_snapshot(
    entity_id: &str,
    currency: &str,
    snap: &CellCutSnapshot,
    question_name: &str,
) -> Result<EntityReportingSlice, String> {
    slice_from_cut_snapshot_for_weeks(entity_id, currency, snap, question_name, None)
}

/// Extract an authenticated [`EntityReportingSlice`] directly from a real
/// complete [`CellCutSnapshot`] with an optional widened ISO-week horizon.
///
/// # Errors
/// Returns an error if the snapshot is incomplete, lacks the requested
/// question, or carries inconsistent pins.
pub fn slice_from_cut_snapshot_for_weeks(
    entity_id: &str,
    currency: &str,
    snap: &CellCutSnapshot,
    question_name: &str,
    window_weeks: Option<u32>,
) -> Result<EntityReportingSlice, String> {
    if !snap.complete {
        return Err(format!(
            "{entity_id}: cut snapshot incomplete (missing={:?} gates={:?} findings={})",
            snap.missing_feeds, snap.failed_coverage_gates, snap.open_findings
        ));
    }
    let answers = snap
        .answers
        .as_ref()
        .ok_or_else(|| format!("{entity_id}: cut snapshot withheld answers"))?;
    let answer = answers
        .get(question_name)
        .ok_or_else(|| format!("{entity_id}: missing question {question_name} in cut snapshot"))?;
    if !answer.complete || answer.context.scope != snap.scope || answer.context.world != snap.world
    {
        return Err(format!(
            "{entity_id}: disclosed answer {question_name} disagrees with cut snapshot scope/world"
        ));
    }
    Ok(EntityReportingSlice {
        entity_id: entity_id.to_owned(),
        cell_id: snap.cell_id.clone(),
        currency: currency.to_owned(),
        question_name: question_name.to_owned(),
        window_weeks,
        query_pin: answer.query_pin.clone(),
        definition_pin: snap.definition_pin.clone(),
        read_token: snap.read_token.clone(),
        revenue_answer: answer.clone(),
    })
}

/// Canonical idempotency key binding a committed leg to `transfer_id`.
#[must_use]
pub fn transfer_leg_key(transfer_id: &str, role: &str) -> String {
    format!("ie-transfer:{transfer_id}:{role}")
}

/// Canonical seller `supply.PosSale` object ID bound to `transfer_id`.
#[must_use]
pub fn transfer_sale_id(transfer_id: &str) -> String {
    format!("SALE-XFER-{transfer_id}")
}

/// Canonical buyer `supply.StockLot` object ID bound to `transfer_id`.
#[must_use]
pub fn transfer_lot_id(transfer_id: &str) -> String {
    format!("LOT-XFER-{transfer_id}")
}

/// Compute the deterministic `Norn1` canonical elimination journal pin binding
/// `period_week`, every entity slice's authenticated cut pins, and every
/// bilateral transfer's verified export hashes, committed object IDs, and
/// verified committed amounts.
///
/// # Errors
/// Returns [`OperatingGroupRefusal::MissingStatutoryApproval`] if any transfer
/// lacks verified bilateral leg evidence or canonical hashing fails.
pub fn compute_elimination_journal_pin(
    period_week: &str,
    slices: &[EntityReportingSlice],
    transfers: &[InterEntityTransfer],
) -> Result<String, OperatingGroupRefusal> {
    let mut slice_pins: Vec<serde_json::Value> = slices
        .iter()
        .map(|slice| {
            json!({
                "entityId": slice.entity_id,
                "cellId": slice.cell_id.as_str(),
                "queryPin": slice.query_pin,
                "definitionPin": slice.definition_pin,
                "revision": slice.read_token.context.frontier.revision,
            })
        })
        .collect();
    slice_pins.sort_by_key(ToString::to_string);

    let mut transfer_pins: Vec<serde_json::Value> = Vec::with_capacity(transfers.len());
    let mut seen_legs: BTreeSet<(String, u64)> = BTreeSet::new();
    for xfer in transfers {
        let (Some(seller_leg), Some(buyer_leg)) = (&xfer.seller_leg, &xfer.buyer_leg) else {
            return Err(OperatingGroupRefusal::MissingStatutoryApproval {
                detail: format!(
                    "transfer {} missing bilateral leg evidence",
                    xfer.transfer_id
                ),
            });
        };
        let seller_leg_key = (
            seller_leg.object_id.clone(),
            seller_leg.export_record.receipt.body.commit_seq,
        );
        let buyer_leg_key = (
            buyer_leg.object_id.clone(),
            buyer_leg.export_record.receipt.body.commit_seq,
        );
        if !seen_legs.insert(seller_leg_key) || !seen_legs.insert(buyer_leg_key) {
            return Err(OperatingGroupRefusal::MissingStatutoryApproval {
                detail: format!(
                    "duplicate_transfer_leg in journal pin for {}",
                    xfer.transfer_id
                ),
            });
        }
        let committed_seller =
            extract_seller_sale_leg(&xfer.transfer_id, seller_leg).map_err(|err| {
                OperatingGroupRefusal::MissingStatutoryApproval {
                    detail: format!("seller leg invalid for {}: {err:?}", xfer.transfer_id),
                }
            })?;
        transfer_pins.push(json!({
            "transferId": xfer.transfer_id,
            "sellerObjectId": seller_leg.object_id,
            "buyerObjectId": buyer_leg.object_id,
            "sellerHash": seller_leg.export_record.receipt.hash,
            "buyerHash": buyer_leg.export_record.receipt.hash,
            "amountScaled": committed_seller.amount_scaled.to_string(),
        }));
    }
    transfer_pins.sort_by_key(ToString::to_string);

    let payload = json!({
        "periodWeek": period_week,
        "slices": slice_pins,
        "transfers": transfer_pins,
    })
    .to_string();
    HashProfile::Norn1.hash_json(&payload).map_err(|e| {
        OperatingGroupRefusal::MissingStatutoryApproval {
            detail: format!("hash elimination journal: {e:?}"),
        }
    })
}

/// Project `slices` at `requested_tier` under fail-closed evidence rules bound
/// directly to `owning_cells`.
///
/// Refuses missing/inconsistent cell cut pins, mutated `QueryResult` rows,
/// `query_pin`/`read_token` mismatches, missing FX tables, unverified or
/// foreign-cell or ledger-absent bilateral transfer exports, missing shared
/// committed transfer identity, replayed/reused transfer legs, mismatched
/// seller vs buyer leg amounts, and unauthenticated statutory close approvals.
///
/// # Errors
/// Returns [`OperatingGroupRefusal`] whenever any tier prerequisite fails closed.
#[allow(
    clippy::too_many_arguments,
    reason = "explicit fail-closed operating group projection inputs"
)]
pub fn evaluate_operating_group_tier(
    requested_tier: OperatingGroupTier,
    owning_cells: &[&EntityCell],
    slices: &[EntityReportingSlice],
    reporting_currency: Option<&str>,
    fx_rates: &[FxRateEntry],
    inter_entity_transfers: &[InterEntityTransfer],
    statutory_approval: Option<&StatutoryCloseApproval>,
) -> Result<OperatingGroupProjection, OperatingGroupRefusal> {
    let cell_map: BTreeMap<&str, &EntityCell> = owning_cells
        .iter()
        .map(|cell| (cell.entity_id.as_str(), *cell))
        .collect();
    validate_slice_cut_pins(slices, &cell_map)?;
    let (by_ccy_scaled, per_currency_revenue) = aggregate_verified_revenue_by_currency(slices)?;
    if requested_tier == OperatingGroupTier::SideBySide {
        return Ok(build_projection(
            OperatingGroupTier::SideBySide,
            None,
            per_currency_revenue,
            None,
            None,
            None,
        ));
    }
    let target_ccy = reporting_currency.ok_or_else(|| OperatingGroupRefusal::MissingFxTable {
        from_currency: "*".to_owned(),
        to_currency: "<unspecified>".to_owned(),
    })?;
    let translated_total = translate_currency_totals(&by_ccy_scaled, target_ccy, fx_rates)?;
    let translated_dec = decimal_from_scaled(translated_total)
        .map_err(|detail| OperatingGroupRefusal::ArithmeticOverflow { detail })?;
    if requested_tier == OperatingGroupTier::FxTranslated {
        return Ok(build_projection(
            OperatingGroupTier::FxTranslated,
            Some(target_ccy.to_owned()),
            per_currency_revenue,
            Some(translated_dec),
            None,
            None,
        ));
    }
    let eliminated_scaled = sum_verified_eliminations(
        &cell_map,
        slices,
        inter_entity_transfers,
        target_ccy,
        fx_rates,
    )?;
    let net_scaled = translated_total
        .checked_sub(eliminated_scaled)
        .ok_or_else(|| OperatingGroupRefusal::ArithmeticOverflow {
            detail: "net group revenue underflow".to_owned(),
        })?;
    let eliminated_dec = decimal_from_scaled(eliminated_scaled)
        .map_err(|detail| OperatingGroupRefusal::ArithmeticOverflow { detail })?;
    let net_dec = decimal_from_scaled(net_scaled)
        .map_err(|detail| OperatingGroupRefusal::ArithmeticOverflow { detail })?;
    if requested_tier == OperatingGroupTier::StatutoryConsolidated {
        validate_statutory_approval(statutory_approval, slices, inter_entity_transfers)?;
    }
    Ok(build_projection(
        requested_tier,
        Some(target_ccy.to_owned()),
        per_currency_revenue,
        Some(translated_dec),
        Some(eliminated_dec),
        Some(net_dec),
    ))
}

fn build_projection(
    tier: OperatingGroupTier,
    reporting_currency: Option<String>,
    per_currency_revenue: BTreeMap<String, Decimal>,
    translated_revenue: Option<Decimal>,
    eliminated_inter_entity_revenue: Option<Decimal>,
    net_group_revenue: Option<Decimal>,
) -> OperatingGroupProjection {
    OperatingGroupProjection {
        tier,
        is_consolidated_financial_statement: tier == OperatingGroupTier::StatutoryConsolidated,
        read_only: true,
        reporting_currency,
        per_currency_revenue,
        translated_revenue,
        eliminated_inter_entity_revenue,
        net_group_revenue,
    }
}

fn validate_slice_cut_pins(
    slices: &[EntityReportingSlice],
    cell_map: &BTreeMap<&str, &EntityCell>,
) -> Result<(), OperatingGroupRefusal> {
    if slices.is_empty() {
        return Err(OperatingGroupRefusal::InvalidCellCutPin {
            entity_id: "<none>".to_owned(),
            reason: "at least one entity slice is required".to_owned(),
        });
    }
    for slice in slices {
        let invalid = |reason: &str| OperatingGroupRefusal::InvalidCellCutPin {
            entity_id: slice.entity_id.clone(),
            reason: reason.to_owned(),
        };
        if slice.query_pin.trim().is_empty() || slice.definition_pin.trim().is_empty() {
            return Err(invalid("missing query_pin or definition_pin"));
        }
        let ctx = &slice.read_token.context;
        if ctx.frontier.cell != slice.cell_id
            || ctx.authority_pin.resource_id != slice.entity_id
            || slice.read_token.completeness != Completeness::Complete
            || slice.read_token.semantics != ReadSemantics::Exact
        {
            return Err(invalid("read_token envelope invalid for slice"));
        }
        let expected_def = ctx
            .definition_pin
            .admission_receipt
            .as_deref()
            .unwrap_or(ctx.definition_pin.model_hash.as_str());
        let answer = &slice.revenue_answer;
        if slice.definition_pin != expected_def
            || answer.query_pin != slice.query_pin
            || answer.definition_pin != slice.definition_pin
            || !answer.complete
            || answer.known_as_of != ctx.frontier.revision
            || answer.revision.commits != ctx.frontier.revision
            || answer.context.world != ctx.frontier.world
        {
            return Err(invalid(
                "revenue QueryResult pins disagree with slice query_pin/read_token",
            ));
        }
        let Some(owning_cell) = cell_map.get(slice.entity_id.as_str()).copied() else {
            return Err(invalid("owning cell absent from verification set"));
        };
        verify_slice_against_owning_cell(slice, owning_cell)?;
    }
    Ok(())
}

fn verify_slice_against_owning_cell(
    slice: &EntityReportingSlice,
    owning_cell: &EntityCell,
) -> Result<(), OperatingGroupRefusal> {
    let invalid = |reason: String| OperatingGroupRefusal::InvalidCellCutPin {
        entity_id: slice.entity_id.clone(),
        reason,
    };
    if owning_cell.cell_id != slice.cell_id
        || owning_cell.currency != slice.currency
        || owning_cell.scope != slice.revenue_answer.context.scope
    {
        return Err(invalid(
            "slice cell_id/currency/scope disagrees with owning cell".to_owned(),
        ));
    }
    let cut_revision = slice.read_token.context.frontier.revision;
    let head = owning_cell.head().map_err(invalid)?;
    let store = owning_cell.cell().map_err(invalid)?.store();
    if cut_revision > head.commits
        || store
            .read()
            .receipt_hash_at(cut_revision)
            .map_err(|e| invalid(format!("read cut receipt_hash: {e}")))?
            .is_none()
    {
        return Err(invalid(format!(
            "cut_revision {cut_revision} not present in owning cell ledger"
        )));
    }
    let plan = match slice.window_weeks {
        Some(weeks) => {
            owning_cell
                .pack
                .retail_question_for_weeks(&slice.question_name, weeks, 5_000)
        }
        None => owning_cell.pack.retail_question(&slice.question_name),
    }
    .map_err(invalid)?;
    let cutoff = KnowledgeCutoff {
        recorded_at: LogicalTime::from_millis(slice.revenue_answer.context.now_ms),
        frontier: cut_revision,
    };
    let verified = store
        .evaluate_question_known_at(&plan, slice.revenue_answer.context.clone(), cutoff, |_| {
            Ok(true)
        })
        .map_err(|e| invalid(format!("re-evaluate pinned cut at {cut_revision}: {e}")))?;
    if verified.result.grain != slice.revenue_answer.grain
        || verified.result.definition_pin != slice.definition_pin
        || verified.result.rows.len() != slice.revenue_answer.rows.len()
    {
        return Err(invalid(
            "slice QueryResult disagrees with owning cell ledger cut".to_owned(),
        ));
    }
    for (bucket_key, row) in &slice.revenue_answer.rows {
        let Some(auth_row) = verified.result.rows.get(bucket_key) else {
            return Err(invalid(format!(
                "slice row `{bucket_key}` absent from owning cell ledger cut"
            )));
        };
        if row.suppressed || auth_row.bucket != row.bucket || auth_row.value != row.value {
            return Err(invalid(format!(
                "slice row `{bucket_key}` mutated relative to owning cell ledger cut"
            )));
        }
    }
    if cut_revision == head.commits {
        let live = store
            .evaluate_question(&plan, slice.revenue_answer.context.clone(), |_| Ok(true))
            .map_err(|e| invalid(format!("verify head query_pin: {e}")))?;
        if live.query_pin != slice.query_pin {
            return Err(invalid(
                "slice query_pin disagrees with owning cell question plan".to_owned(),
            ));
        }
    }
    Ok(())
}

fn derive_slice_revenue_scaled(
    slice: &EntityReportingSlice,
) -> Result<i128, OperatingGroupRefusal> {
    let invalid = |reason: String| OperatingGroupRefusal::InvalidCellCutPin {
        entity_id: slice.entity_id.clone(),
        reason,
    };
    if slice.revenue_answer.rows.is_empty() {
        return Err(invalid(
            "disclosed revenue QueryResult has zero rows".to_owned(),
        ));
    }
    let mut total_scaled: i128 = 0;
    for row in slice.revenue_answer.rows.values() {
        if row.suppressed {
            return Err(invalid(
                "disclosed revenue bucket is suppressed under DATA-VALUES-1".to_owned(),
            ));
        }
        let parts = bucket_parts(&row.bucket);
        let row_ccy = parts
            .first()
            .map(String::as_str)
            .ok_or_else(|| invalid("revenue bucket missing currency partition".to_owned()))?;
        if row_ccy != slice.currency {
            return Err(invalid(format!(
                "revenue bucket currency `{row_ccy}` disagrees with entity book currency `{}`",
                slice.currency
            )));
        }
        let value = row
            .value
            .ok_or_else(|| invalid("revenue bucket missing exact numeric value".to_owned()))?;
        let scaled_val = decimal_to_scaled(value).map_err(invalid)?;
        total_scaled = total_scaled.checked_add(scaled_val).ok_or_else(|| {
            OperatingGroupRefusal::ArithmeticOverflow {
                detail: format!("revenue sum overflow for {}", slice.entity_id),
            }
        })?;
    }
    Ok(total_scaled)
}

fn aggregate_verified_revenue_by_currency(
    slices: &[EntityReportingSlice],
) -> Result<(BTreeMap<String, i128>, BTreeMap<String, Decimal>), OperatingGroupRefusal> {
    let mut by_ccy: BTreeMap<String, i128> = BTreeMap::new();
    for slice in slices {
        let derived_rev = derive_slice_revenue_scaled(slice)?;
        let entry = by_ccy.entry(slice.currency.clone()).or_insert(0);
        *entry = entry.checked_add(derived_rev).ok_or_else(|| {
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

fn validate_statutory_approval(
    approval: Option<&StatutoryCloseApproval>,
    slices: &[EntityReportingSlice],
    transfers: &[InterEntityTransfer],
) -> Result<(), OperatingGroupRefusal> {
    let Some(signoff) = approval else {
        return Err(OperatingGroupRefusal::MissingStatutoryApproval {
            detail: "statutory_consolidated requires auditor close approval and elimination pin"
                .to_owned(),
        });
    };
    if signoff.auditor_principal.trim().is_empty()
        || signoff.period_week.trim().is_empty()
        || signoff.approved_at.trim().is_empty()
    {
        return Err(OperatingGroupRefusal::MissingStatutoryApproval {
            detail: "auditor_principal, period_week, and approved_at must be non-empty".to_owned(),
        });
    }
    let expected_pin = compute_elimination_journal_pin(&signoff.period_week, slices, transfers)?;
    if signoff.elimination_journal_pin != expected_pin {
        return Err(OperatingGroupRefusal::MissingStatutoryApproval {
            detail: "elimination_journal_pin does not match authenticated cuts and transfer legs"
                .to_owned(),
        });
    }
    Ok(())
}

pub(super) fn convert_scaled(
    amount_scaled: i128,
    source_ccy: &str,
    target_ccy: &str,
    fx_rates: &[FxRateEntry],
) -> Result<i128, OperatingGroupRefusal> {
    if source_ccy == target_ccy {
        return Ok(amount_scaled);
    }
    let rate = fx_rates
        .iter()
        .find(|entry| {
            entry.from_currency == source_ccy
                && entry.to_currency == target_ccy
                && !entry.as_of.trim().is_empty()
                && !entry.rate_table_pin.trim().is_empty()
                && entry.rate_scaled > 0
        })
        .ok_or_else(|| OperatingGroupRefusal::MissingFxTable {
            from_currency: source_ccy.to_owned(),
            to_currency: target_ccy.to_owned(),
        })?;
    let product = amount_scaled.checked_mul(rate.rate_scaled).ok_or_else(|| {
        OperatingGroupRefusal::ArithmeticOverflow {
            detail: format!("FX multiply overflow for {source_ccy}->{target_ccy}"),
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
