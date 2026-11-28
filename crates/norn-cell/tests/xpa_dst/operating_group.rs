//! Test-harness 4-tier `OperatingGroup` reporting projection model (`sampled`).
//!
//! Projects independent legal-entity cell cuts (`READ-LOCAL-1`) through four
//! explicit, fail-closed reporting tiers:
//! 1. `side_by_side` (native currencies, read-only, non-consolidated)
//! 2. `fx_translated` (pinned as-of FX rate table required)
//! 3. `inter_entity_netted` (bilateral matched `main` commit [`ExportRecord`]s
//!    cryptographically verified via [`verify_main_export_record`], bound to the
//!    owning cell's `cell_id`, `scope`, ledger revision, committed transfer
//!    object/event ID, and committed seller/buyer leg amounts)
//! 4. `statutory_consolidated` (auditor close approval bound to the canonical
//!    `Norn1` elimination journal pin over authenticated cuts and transfer legs)
//!
//! Every entity revenue total is derived directly from its verified disclosed
//! [`QueryResult`] inside [`EntityReportingSlice`] — caller-supplied arbitrary
//! totals are never accepted.
//!
//! **Honest evidence note:** Production `OperatingGroup` consolidation kernel
//! and pack seams are `unknown` / pending S4. This module is a `sampled`
//! test-harness projection model over real `CellCutSnapshot`s and real `main`
//! commit export records.

use std::collections::BTreeMap;

use norn_cell::cell_identity::FORMAT as CELL_ID_FORMAT;
use norn_cell::combined_view::CellCutSnapshot;
use norn_cell::compute::QueryResult;
use norn_cell::export::ExportRecord;
use norn_cell::{FactWriteRow, verify_main_export_record};
use norn_checker::metric::compute::arithmetic::FACTOR;
use norn_ir::canonical::HashProfile;
use norn_ir::cell_id::CellId;
use norn_ir::{Decimal, RuntimeValue};
use norn_kernel::read_envelope::{Completeness, ReadSemantics, ReadToken};
use serde::{Deserialize, Serialize};
use serde_json::json;

use super::cuts::bucket_parts;
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
/// `query_pin`, `definition_pin`, and `read_token`). Revenue totals are always
/// computed directly from `revenue_answer`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EntityReportingSlice {
    pub entity_id: String,
    pub cell_id: CellId,
    pub currency: String,
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
/// complete [`CellCutSnapshot`] and its disclosed revenue [`QueryResult`].
///
/// Never accepts caller-supplied arbitrary totals: revenue is always derived
/// from `snap.answers[question_name]` and checked against `snap`'s cut pins.
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
        query_pin: answer.query_pin.clone(),
        definition_pin: snap.definition_pin.clone(),
        read_token: snap.read_token.clone(),
        revenue_answer: answer.clone(),
    })
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
    for xfer in transfers {
        let (Some(seller_leg), Some(buyer_leg)) = (&xfer.seller_leg, &xfer.buyer_leg) else {
            return Err(OperatingGroupRefusal::MissingStatutoryApproval {
                detail: format!(
                    "transfer {} missing bilateral leg evidence",
                    xfer.transfer_id
                ),
            });
        };
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

/// Project `slices` at `requested_tier` under fail-closed evidence rules.
///
/// Refuses missing/inconsistent cell cut pins, query_pin/read_token mismatches,
/// missing FX tables, unverified or foreign-cell or unrelated-commit bilateral
/// transfer evidence, mismatched seller vs buyer leg amounts, and
/// unauthenticated statutory close approvals.
///
/// # Errors
/// Returns [`OperatingGroupRefusal`] whenever any tier prerequisite fails closed.
pub fn evaluate_operating_group_tier(
    requested_tier: OperatingGroupTier,
    slices: &[EntityReportingSlice],
    reporting_currency: Option<&str>,
    fx_rates: &[FxRateEntry],
    inter_entity_transfers: &[InterEntityTransfer],
    statutory_approval: Option<&StatutoryCloseApproval>,
) -> Result<OperatingGroupProjection, OperatingGroupRefusal> {
    validate_slice_cut_pins(slices)?;
    let (by_ccy_scaled, per_currency_revenue) = aggregate_verified_revenue_by_currency(slices)?;
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
        sum_verified_eliminations(slices, inter_entity_transfers, target_ccy, fx_rates)?;
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

    validate_statutory_approval(statutory_approval, slices, inter_entity_transfers)?;
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

fn validate_slice_cut_pins(slices: &[EntityReportingSlice]) -> Result<(), OperatingGroupRefusal> {
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
        if ctx.frontier.cell != slice.cell_id {
            return Err(invalid("read_token cell_id does not match slice cell_id"));
        }
        if ctx.authority_pin.resource_id != slice.entity_id {
            return Err(invalid(
                "read_token authority_pin resource_id does not match entity_id",
            ));
        }
        if slice.read_token.completeness != Completeness::Complete
            || slice.read_token.semantics != ReadSemantics::Exact
        {
            return Err(invalid("read_token must be Complete and Exact"));
        }
        let expected_def = ctx
            .definition_pin
            .admission_receipt
            .as_deref()
            .unwrap_or(ctx.definition_pin.model_hash.as_str());
        if slice.definition_pin != expected_def {
            return Err(invalid(
                "definition_pin disagrees with read_token definition_pin",
            ));
        }
        let answer = &slice.revenue_answer;
        if answer.query_pin != slice.query_pin
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

struct CommittedLegDetails {
    currency: String,
    qty_scaled: i128,
    amount_scaled: i128,
}

fn sum_verified_eliminations(
    slices: &[EntityReportingSlice],
    transfers: &[InterEntityTransfer],
    target_ccy: &str,
    fx_rates: &[FxRateEntry],
) -> Result<i128, OperatingGroupRefusal> {
    let slice_map: BTreeMap<&str, &EntityReportingSlice> = slices
        .iter()
        .map(|slice| (slice.entity_id.as_str(), slice))
        .collect();
    let mut eliminated: i128 = 0;
    for xfer in transfers {
        let seller_amount_scaled = verify_single_transfer(xfer, &slice_map, fx_rates)?;
        let converted = convert_scaled(seller_amount_scaled, &xfer.currency, target_ccy, fx_rates)?;
        eliminated = eliminated.checked_add(converted).ok_or_else(|| {
            OperatingGroupRefusal::ArithmeticOverflow {
                detail: "eliminated inter-entity sum overflow".to_owned(),
            }
        })?;
    }
    Ok(eliminated)
}

fn verify_single_transfer(
    xfer: &InterEntityTransfer,
    slice_map: &BTreeMap<&str, &EntityReportingSlice>,
    fx_rates: &[FxRateEntry],
) -> Result<i128, OperatingGroupRefusal> {
    let unmatched = |reason: String| OperatingGroupRefusal::UnmatchedInterEntityTransfer {
        transfer_id: xfer.transfer_id.clone(),
        reason,
    };
    if xfer.seller_entity_id == xfer.buyer_entity_id {
        return Err(unmatched(
            "self-transfer cannot be an inter-entity elimination".to_owned(),
        ));
    }
    let Some(seller_slice) = slice_map.get(xfer.seller_entity_id.as_str()).copied() else {
        return Err(unmatched(
            "seller entity absent from evaluated cell cuts".to_owned(),
        ));
    };
    let Some(buyer_slice) = slice_map.get(xfer.buyer_entity_id.as_str()).copied() else {
        return Err(unmatched(
            "buyer entity absent from evaluated cell cuts".to_owned(),
        ));
    };
    let (Some(seller_leg), Some(buyer_leg)) = (&xfer.seller_leg, &xfer.buyer_leg) else {
        return Err(unmatched(
            "missing bilateral main export record on seller or buyer side".to_owned(),
        ));
    };
    verify_leg_ledger_binding(&xfer.transfer_id, "seller", seller_leg, seller_slice)?;
    verify_leg_ledger_binding(&xfer.transfer_id, "buyer", buyer_leg, buyer_slice)?;
    if seller_leg.export_record.receipt.hash == buyer_leg.export_record.receipt.hash {
        return Err(unmatched(
            "seller and buyer export records cannot share the same commit hash".to_owned(),
        ));
    }

    let seller_committed = extract_seller_sale_leg(&xfer.transfer_id, seller_leg)?;
    let buyer_committed = extract_buyer_lot_leg(&xfer.transfer_id, buyer_leg)?;
    verify_bilateral_amounts(
        xfer,
        seller_slice,
        buyer_slice,
        &seller_committed,
        &buyer_committed,
        fx_rates,
    )?;
    Ok(seller_committed.amount_scaled)
}

fn verify_bilateral_amounts(
    xfer: &InterEntityTransfer,
    seller_slice: &EntityReportingSlice,
    buyer_slice: &EntityReportingSlice,
    seller_committed: &CommittedLegDetails,
    buyer_committed: &CommittedLegDetails,
    fx_rates: &[FxRateEntry],
) -> Result<(), OperatingGroupRefusal> {
    let unmatched = |reason: String| OperatingGroupRefusal::UnmatchedInterEntityTransfer {
        transfer_id: xfer.transfer_id.clone(),
        reason,
    };
    if seller_committed.currency != xfer.currency
        || seller_committed.currency != seller_slice.currency
    {
        return Err(unmatched(format!(
            "seller leg currency `{}` disagrees with transfer `{}` / book `{}`",
            seller_committed.currency, xfer.currency, seller_slice.currency
        )));
    }
    if buyer_committed.currency != buyer_slice.currency {
        return Err(unmatched(format!(
            "buyer leg currency `{}` disagrees with buyer book `{}`",
            buyer_committed.currency, buyer_slice.currency
        )));
    }
    if seller_committed.qty_scaled <= 0 || seller_committed.qty_scaled != buyer_committed.qty_scaled
    {
        return Err(unmatched(format!(
            "bilateral transfer quantity mismatch: seller={} buyer={}",
            seller_committed.qty_scaled, buyer_committed.qty_scaled
        )));
    }

    let expected_buyer_amount = if seller_committed.currency == buyer_committed.currency {
        seller_committed.amount_scaled
    } else if let Some(spot_rate) = &xfer.settlement_fx_rate {
        convert_scaled(
            seller_committed.amount_scaled,
            &seller_committed.currency,
            &buyer_committed.currency,
            std::slice::from_ref(spot_rate),
        )?
    } else {
        convert_scaled(
            seller_committed.amount_scaled,
            &seller_committed.currency,
            &buyer_committed.currency,
            fx_rates,
        )?
    };
    if expected_buyer_amount != buyer_committed.amount_scaled {
        return Err(unmatched(format!(
            "asymmetric bilateral amounts: seller_translated={expected_buyer_amount} buyer_committed={}",
            buyer_committed.amount_scaled
        )));
    }
    Ok(())
}

fn verify_leg_ledger_binding(
    transfer_id: &str,
    role: &str,
    leg: &TransferLegEvidence,
    slice: &EntityReportingSlice,
) -> Result<(), OperatingGroupRefusal> {
    let unmatched = |reason: String| OperatingGroupRefusal::UnmatchedInterEntityTransfer {
        transfer_id: transfer_id.to_owned(),
        reason: format!("{role} leg: {reason}"),
    };
    verify_main_export_record(&leg.export_record).map_err(|e| {
        unmatched(format!(
            "export record failed cryptographic verification: {e}"
        ))
    })?;
    let bindings = &leg.export_record.bindings;
    if bindings.get("cell_identity_format").map(String::as_str) != Some(CELL_ID_FORMAT) {
        return Err(unmatched(
            "export record missing canonical cell_identity_format binding".to_owned(),
        ));
    }
    if bindings.get("cell_id").map(String::as_str) != Some(slice.cell_id.as_str()) {
        return Err(unmatched(format!(
            "export record cell_id {:?} does not match owning cell {}",
            bindings.get("cell_id"),
            slice.cell_id
        )));
    }
    if leg.export_record.batch.scope != slice.revenue_answer.context.scope {
        return Err(unmatched(
            "export record scope does not match owning cell cut scope".to_owned(),
        ));
    }
    let cut_revision = slice.read_token.context.frontier.revision;
    if leg.export_record.receipt.body.commit_seq > cut_revision {
        return Err(unmatched(format!(
            "commit_seq {} exceeds pinned cell cut revision {cut_revision}",
            leg.export_record.receipt.body.commit_seq
        )));
    }
    let written_present = leg
        .export_record
        .receipt
        .body
        .written
        .iter()
        .any(|row| row.object_type == leg.object_type && row.key == leg.object_id);
    if !written_present {
        return Err(unmatched(format!(
            "receipt does not commit declared transfer object {}/{}",
            leg.object_type, leg.object_id
        )));
    }
    Ok(())
}

fn extract_seller_sale_leg(
    transfer_id: &str,
    leg: &TransferLegEvidence,
) -> Result<CommittedLegDetails, OperatingGroupRefusal> {
    if leg.object_type != "supply.PosSale" {
        return Err(OperatingGroupRefusal::UnmatchedInterEntityTransfer {
            transfer_id: transfer_id.to_owned(),
            reason: format!(
                "seller leg must commit supply.PosSale, got {}",
                leg.object_type
            ),
        });
    }
    extract_committed_leg_fields(transfer_id, leg, "sold_qty", "net_revenue")
}

fn extract_buyer_lot_leg(
    transfer_id: &str,
    leg: &TransferLegEvidence,
) -> Result<CommittedLegDetails, OperatingGroupRefusal> {
    if leg.object_type != "supply.StockLot" {
        return Err(OperatingGroupRefusal::UnmatchedInterEntityTransfer {
            transfer_id: transfer_id.to_owned(),
            reason: format!(
                "buyer leg must commit supply.StockLot, got {}",
                leg.object_type
            ),
        });
    }
    extract_committed_leg_fields(transfer_id, leg, "received_qty", "opening_stock_value")
}

fn extract_committed_leg_fields(
    transfer_id: &str,
    leg: &TransferLegEvidence,
    qty_field: &str,
    amount_field: &str,
) -> Result<CommittedLegDetails, OperatingGroupRefusal> {
    let unmatched = |reason: String| OperatingGroupRefusal::UnmatchedInterEntityTransfer {
        transfer_id: transfer_id.to_owned(),
        reason,
    };
    let values = leg
        .export_record
        .batch
        .writes
        .iter()
        .find_map(|write| match write {
            FactWriteRow::Put {
                object_type,
                key,
                values,
            } if object_type == &leg.object_type && key == &leg.object_id => Some(values),
            FactWriteRow::Put { .. } | FactWriteRow::Delete { .. } => None,
        })
        .ok_or_else(|| {
            unmatched(format!(
                "commit batch does not contain Put for {}/{}",
                leg.object_type, leg.object_id
            ))
        })?;
    let currency = match values.get("currency") {
        Some(RuntimeValue::String(code)) if !code.trim().is_empty() => code.clone(),
        other => {
            return Err(unmatched(format!(
                "committed object {}/{} missing valid currency: {other:?}",
                leg.object_type, leg.object_id
            )));
        }
    };
    let qty_dec = match values.get(qty_field) {
        Some(RuntimeValue::Number(num)) => *num,
        other => {
            return Err(unmatched(format!(
                "committed object {}/{} missing numeric `{qty_field}`: {other:?}",
                leg.object_type, leg.object_id
            )));
        }
    };
    let amount_dec = match values.get(amount_field) {
        Some(RuntimeValue::Number(num)) => *num,
        other => {
            return Err(unmatched(format!(
                "committed object {}/{} missing numeric `{amount_field}`: {other:?}",
                leg.object_type, leg.object_id
            )));
        }
    };
    let qty_scaled = decimal_to_scaled(qty_dec).map_err(unmatched)?;
    let amount_scaled = decimal_to_scaled(amount_dec).map_err(unmatched)?;
    if amount_scaled <= 0 {
        return Err(unmatched(format!(
            "committed transfer amount `{amount_field}` must be strictly positive, got {amount_scaled}"
        )));
    }
    Ok(CommittedLegDetails {
        currency,
        qty_scaled,
        amount_scaled,
    })
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

fn convert_scaled(
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
