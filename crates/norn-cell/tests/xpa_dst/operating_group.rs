//! Test-harness 4-tier `OperatingGroup` reporting projection model (`sampled`).
//!
//! Projects independent legal-entity cell cuts (`READ-LOCAL-1`) through four
//! explicit, fail-closed reporting tiers:
//! 1. `side_by_side` (native currencies, read-only, non-consolidated)
//! 2. `fx_translated` (pinned as-of FX rate table required)
//! 3. `inter_entity_netted` (bilateral matched `main` commit [`Receipt`]s
//!    cryptographically verified via [`verify_main_receipt`])
//! 4. `statutory_consolidated` (auditor close approval bound to the canonical
//!    `Norn1` elimination journal pin over authenticated cuts and receipts)
//!
//! **Honest evidence note:** Production `OperatingGroup` consolidation kernel
//! and pack seams are `unknown` / pending S4. This module is a `sampled`
//! test-harness projection model over real `CellCutSnapshot`s and real `main`
//! commit receipts.

use std::collections::BTreeMap;

use norn_cell::combined_view::CellCutSnapshot;
use norn_cell::{Receipt, verify_main_receipt};
use norn_checker::metric::compute::arithmetic::FACTOR;
use norn_ir::Decimal;
use norn_ir::canonical::HashProfile;
use norn_ir::cell_id::CellId;
use norn_kernel::read_envelope::{Completeness, ReadSemantics, ReadToken};
use serde::{Deserialize, Serialize};
use serde_json::json;

use super::prng::{HALF_FACTOR, decimal_from_scaled, div_i128};

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

/// One bilateral inter-entity flow matched by cryptographically verified `main`
/// commit [`Receipt`]s from both legal-entity cells.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InterEntityTransfer {
    pub transfer_id: String,
    pub seller_entity_id: String,
    pub buyer_entity_id: String,
    pub currency: String,
    pub seller_amount_scaled: i128,
    pub buyer_amount_scaled: i128,
    pub seller_receipt: Option<Receipt>,
    pub buyer_receipt: Option<Receipt>,
}

/// Statutory close sign-off required exclusively for Tier 4 (`statutory_consolidated`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StatutoryCloseApproval {
    pub period_week: String,
    pub auditor_principal: String,
    pub elimination_journal_pin: String,
    pub approved_at: String,
}

/// One entity's exact local revenue & margin slice extracted from a real pinned
/// [`CellCutSnapshot`] carrying authentic `cell_id`, `query_pin`, `definition_pin`,
/// and `read_token`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EntityReportingSlice {
    pub entity_id: String,
    pub cell_id: CellId,
    pub currency: String,
    pub revenue_scaled: i128,
    pub gross_margin_scaled: i128,
    pub query_pin: String,
    pub definition_pin: String,
    pub read_token: ReadToken,
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
/// complete [`CellCutSnapshot`] and its disclosed question answer.
///
/// # Errors
/// Returns an error if the snapshot is incomplete or lacks the requested question.
pub fn slice_from_cut_snapshot(
    entity_id: &str,
    currency: &str,
    snap: &CellCutSnapshot,
    question_name: &str,
    revenue_scaled: i128,
    gross_margin_scaled: i128,
) -> Result<EntityReportingSlice, String> {
    let answers = snap.answers.as_ref().ok_or_else(|| {
        format!(
            "{entity_id}: cut snapshot withheld answers (missing={:?} gates={:?} findings={})",
            snap.missing_feeds, snap.failed_coverage_gates, snap.open_findings
        )
    })?;
    let answer = answers
        .get(question_name)
        .ok_or_else(|| format!("{entity_id}: missing question {question_name} in cut snapshot"))?;
    Ok(EntityReportingSlice {
        entity_id: entity_id.to_owned(),
        cell_id: snap.cell_id.clone(),
        currency: currency.to_owned(),
        revenue_scaled,
        gross_margin_scaled,
        query_pin: answer.query_pin.clone(),
        definition_pin: snap.definition_pin.clone(),
        read_token: snap.read_token.clone(),
    })
}

/// Compute the deterministic `Norn1` canonical elimination journal pin binding
/// `period_week`, every entity slice's authenticated cut pins, and every
/// bilateral transfer's verified receipt hashes.
///
/// # Errors
/// Returns [`OperatingGroupRefusal::MissingStatutoryApproval`] if any transfer
/// lacks verified receipt hashes or canonical hashing fails.
pub fn compute_elimination_journal_pin(
    period_week: &str,
    slices: &[EntityReportingSlice],
    transfers: &[InterEntityTransfer],
) -> Result<String, OperatingGroupRefusal> {
    let mut slice_pins: Vec<serde_json::Value> = slices
        .iter()
        .map(|s| {
            json!({
                "entityId": s.entity_id,
                "cellId": s.cell_id.as_str(),
                "queryPin": s.query_pin,
                "definitionPin": s.definition_pin,
                "revision": s.read_token.context.frontier.revision,
            })
        })
        .collect();
    slice_pins.sort_by_key(ToString::to_string);

    let mut transfer_pins: Vec<serde_json::Value> = Vec::with_capacity(transfers.len());
    for xfer in transfers {
        let (Some(seller), Some(buyer)) = (&xfer.seller_receipt, &xfer.buyer_receipt) else {
            return Err(OperatingGroupRefusal::MissingStatutoryApproval {
                detail: format!("transfer {} missing bilateral receipts", xfer.transfer_id),
            });
        };
        transfer_pins.push(json!({
            "transferId": xfer.transfer_id,
            "sellerHash": seller.hash,
            "buyerHash": buyer.hash,
            "amountScaled": xfer.seller_amount_scaled.to_string(),
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
/// Refuses missing/inconsistent cell cut pins, missing FX tables, unverified or
/// asymmetric bilateral transfer receipts, and unauthenticated statutory close
/// approvals.
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
        if slice.query_pin.trim().is_empty() || slice.definition_pin.trim().is_empty() {
            return Err(OperatingGroupRefusal::InvalidCellCutPin {
                entity_id: slice.entity_id.clone(),
                reason: "missing query_pin or definition_pin".to_owned(),
            });
        }
        let ctx = &slice.read_token.context;
        if ctx.frontier.cell != slice.cell_id {
            return Err(OperatingGroupRefusal::InvalidCellCutPin {
                entity_id: slice.entity_id.clone(),
                reason: "read_token cell_id does not match slice cell_id".to_owned(),
            });
        }
        if ctx.authority_pin.resource_id != slice.entity_id {
            return Err(OperatingGroupRefusal::InvalidCellCutPin {
                entity_id: slice.entity_id.clone(),
                reason: "read_token authority_pin resource_id does not match entity_id".to_owned(),
            });
        }
        if slice.read_token.completeness != Completeness::Complete
            || slice.read_token.semantics != ReadSemantics::Exact
        {
            return Err(OperatingGroupRefusal::InvalidCellCutPin {
                entity_id: slice.entity_id.clone(),
                reason: "read_token must be Complete and Exact".to_owned(),
            });
        }
        let expected_def = ctx
            .definition_pin
            .admission_receipt
            .as_deref()
            .unwrap_or(ctx.definition_pin.model_hash.as_str());
        if slice.definition_pin != expected_def {
            return Err(OperatingGroupRefusal::InvalidCellCutPin {
                entity_id: slice.entity_id.clone(),
                reason: "definition_pin disagrees with read_token definition_pin".to_owned(),
            });
        }
    }
    Ok(())
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
    slices: &[EntityReportingSlice],
    transfers: &[InterEntityTransfer],
    target_ccy: &str,
    fx_rates: &[FxRateEntry],
) -> Result<i128, OperatingGroupRefusal> {
    let slice_revs: BTreeMap<&str, u64> = slices
        .iter()
        .map(|s| (s.entity_id.as_str(), s.read_token.context.frontier.revision))
        .collect();
    let mut eliminated: i128 = 0;
    for xfer in transfers {
        verify_single_transfer(xfer, &slice_revs)?;
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

fn verify_single_transfer(
    xfer: &InterEntityTransfer,
    slice_revs: &BTreeMap<&str, u64>,
) -> Result<(), OperatingGroupRefusal> {
    let unmatched = |reason: String| OperatingGroupRefusal::UnmatchedInterEntityTransfer {
        transfer_id: xfer.transfer_id.clone(),
        reason,
    };
    if xfer.seller_entity_id == xfer.buyer_entity_id {
        return Err(unmatched(
            "self-transfer cannot be an inter-entity elimination".to_owned(),
        ));
    }
    let Some(&seller_rev) = slice_revs.get(xfer.seller_entity_id.as_str()) else {
        return Err(unmatched(
            "seller entity absent from evaluated cell cuts".to_owned(),
        ));
    };
    let Some(&buyer_rev) = slice_revs.get(xfer.buyer_entity_id.as_str()) else {
        return Err(unmatched(
            "buyer entity absent from evaluated cell cuts".to_owned(),
        ));
    };
    let (Some(seller_rcpt), Some(buyer_rcpt)) = (&xfer.seller_receipt, &xfer.buyer_receipt) else {
        return Err(unmatched(
            "missing bilateral main receipt on seller or buyer side".to_owned(),
        ));
    };
    verify_main_receipt(seller_rcpt)
        .map_err(|e| unmatched(format!("seller receipt failed verification: {e}")))?;
    verify_main_receipt(buyer_rcpt)
        .map_err(|e| unmatched(format!("buyer receipt failed verification: {e}")))?;
    if seller_rcpt.hash == buyer_rcpt.hash {
        return Err(unmatched(
            "seller and buyer receipts cannot share the same commit hash".to_owned(),
        ));
    }
    if seller_rcpt.body.commit_seq > seller_rev || buyer_rcpt.body.commit_seq > buyer_rev {
        return Err(unmatched(
            "bilateral receipt commit_seq exceeds pinned cell cut revision".to_owned(),
        ));
    }
    if xfer.seller_amount_scaled <= 0 || xfer.seller_amount_scaled != xfer.buyer_amount_scaled {
        return Err(unmatched(format!(
            "invalid/asymmetric bilateral amounts: seller={} buyer={}",
            xfer.seller_amount_scaled, xfer.buyer_amount_scaled
        )));
    }
    Ok(())
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
            detail: "elimination_journal_pin does not match authenticated cuts and receipts"
                .to_owned(),
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
