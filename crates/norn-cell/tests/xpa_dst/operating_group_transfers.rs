//! Bilateral inter-entity transfer leg verification, owning-cell ledger
//! membership binding, shared transfer correlation identity enforcement, and
//! replay/reuse prevention (`duplicate_transfer_leg`) for [`super::operating_group`].

use std::collections::{BTreeMap, BTreeSet};

use norn_cell::cell_identity::FORMAT as CELL_ID_FORMAT;
use norn_cell::{FactWriteRow, verify_main_export_record};
use norn_ir::{Decimal, RuntimeValue};

use super::env::EntityCell;
use super::operating_group::{
    EntityReportingSlice, FxRateEntry, InterEntityTransfer, OperatingGroupRefusal,
    TransferLegEvidence, convert_scaled, transfer_leg_key, transfer_lot_id, transfer_sale_id,
};
use super::prng::decimal_to_scaled;

/// Verified committed seller/buyer leg figures extracted directly from
/// `ExportRecord.batch.writes`.
pub(super) struct CommittedLegDetails {
    pub currency: String,
    pub qty_scaled: i128,
    pub amount_scaled: i128,
}

pub(super) fn sum_verified_eliminations(
    cell_map: &BTreeMap<&str, &EntityCell>,
    slices: &[EntityReportingSlice],
    transfers: &[InterEntityTransfer],
    target_ccy: &str,
    fx_rates: &[FxRateEntry],
) -> Result<i128, OperatingGroupRefusal> {
    let slice_map: BTreeMap<&str, &EntityReportingSlice> = slices
        .iter()
        .map(|slice| (slice.entity_id.as_str(), slice))
        .collect();
    let mut seen_transfers: BTreeSet<&str> = BTreeSet::new();
    let mut seen_legs: BTreeSet<(String, String, u64)> = BTreeSet::new();
    let mut eliminated: i128 = 0;
    for xfer in transfers {
        if xfer.transfer_id.trim().is_empty() {
            return Err(OperatingGroupRefusal::UnmatchedInterEntityTransfer {
                transfer_id: xfer.transfer_id.clone(),
                reason: "empty transfer_id".to_owned(),
            });
        }
        if !seen_transfers.insert(xfer.transfer_id.as_str()) {
            return Err(OperatingGroupRefusal::UnmatchedInterEntityTransfer {
                transfer_id: xfer.transfer_id.clone(),
                reason: "duplicate_transfer_leg: transfer_id already eliminated".to_owned(),
            });
        }
        let seller_amount_scaled =
            verify_single_transfer(xfer, cell_map, &slice_map, fx_rates, &mut seen_legs)?;
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
    cell_map: &BTreeMap<&str, &EntityCell>,
    slice_map: &BTreeMap<&str, &EntityReportingSlice>,
    fx_rates: &[FxRateEntry],
    seen_legs: &mut BTreeSet<(String, String, u64)>,
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
    let Some(seller_cell) = cell_map.get(xfer.seller_entity_id.as_str()).copied() else {
        return Err(unmatched(
            "seller cell absent from owning cell map".to_owned(),
        ));
    };
    let Some(buyer_cell) = cell_map.get(xfer.buyer_entity_id.as_str()).copied() else {
        return Err(unmatched(
            "buyer cell absent from owning cell map".to_owned(),
        ));
    };
    let (Some(seller_leg), Some(buyer_leg)) = (&xfer.seller_leg, &xfer.buyer_leg) else {
        return Err(unmatched(
            "missing bilateral main export record on seller or buyer side".to_owned(),
        ));
    };
    let seller_key = (
        seller_slice.cell_id.as_str().to_owned(),
        seller_leg.object_id.clone(),
        seller_leg.export_record.receipt.body.commit_seq,
    );
    let buyer_key = (
        buyer_slice.cell_id.as_str().to_owned(),
        buyer_leg.object_id.clone(),
        buyer_leg.export_record.receipt.body.commit_seq,
    );
    if !seen_legs.insert(seller_key) || !seen_legs.insert(buyer_key) {
        return Err(unmatched(
            "duplicate_transfer_leg: transfer leg already consumed in elimination journal"
                .to_owned(),
        ));
    }
    verify_leg_ledger_binding(
        &xfer.transfer_id,
        "seller",
        seller_leg,
        seller_slice,
        seller_cell,
    )?;
    verify_leg_ledger_binding(
        &xfer.transfer_id,
        "buyer",
        buyer_leg,
        buyer_slice,
        buyer_cell,
    )?;
    if seller_leg.export_record.receipt.hash == buyer_leg.export_record.receipt.hash {
        return Err(unmatched(
            "seller and buyer export records cannot share the same commit hash".to_owned(),
        ));
    }
    verify_shared_transfer_identity(&xfer.transfer_id, seller_leg, buyer_leg)?;

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

fn verify_shared_transfer_identity(
    transfer_id: &str,
    seller_leg: &TransferLegEvidence,
    buyer_leg: &TransferLegEvidence,
) -> Result<(), OperatingGroupRefusal> {
    let unmatched = |reason: String| OperatingGroupRefusal::UnmatchedInterEntityTransfer {
        transfer_id: transfer_id.to_owned(),
        reason,
    };
    let expected_seller_key = transfer_leg_key(transfer_id, "seller");
    let expected_buyer_key = transfer_leg_key(transfer_id, "buyer");
    if seller_leg
        .export_record
        .receipt
        .body
        .idempotency_key
        .as_deref()
        != Some(expected_seller_key.as_str())
        || buyer_leg
            .export_record
            .receipt
            .body
            .idempotency_key
            .as_deref()
            != Some(expected_buyer_key.as_str())
    {
        return Err(unmatched(
            "seller and buyer commits must carry matching committed transfer correlation keys"
                .to_owned(),
        ));
    }
    if seller_leg.object_id != transfer_sale_id(transfer_id)
        || buyer_leg.object_id != transfer_lot_id(transfer_id)
    {
        return Err(unmatched(
            "seller sale_id and buyer lot_id must carry the shared committed transfer_id"
                .to_owned(),
        ));
    }
    Ok(())
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
    owning_cell: &EntityCell,
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
            slice.cell_id.as_str()
        )));
    }
    if leg.export_record.batch.scope != slice.revenue_answer.context.scope {
        return Err(unmatched(
            "export record scope does not match owning cell cut scope".to_owned(),
        ));
    }
    let cut_revision = slice.read_token.context.frontier.revision;
    let commit_seq = leg.export_record.receipt.body.commit_seq;
    if commit_seq > cut_revision {
        return Err(unmatched(format!(
            "commit_seq {commit_seq} exceeds pinned cell cut revision {cut_revision}"
        )));
    }
    let store = owning_cell.cell().map_err(unmatched)?.store();
    let stored_receipt_hash = store
        .read()
        .receipt_hash_at(commit_seq)
        .map_err(|e| unmatched(format!("read ledger receipt_hash_at({commit_seq}): {e}")))?;
    if stored_receipt_hash.as_deref() != Some(leg.export_record.receipt.hash.as_str()) {
        return Err(unmatched(format!(
            "receipt hash not present in owning cell ledger at commit_seq {commit_seq}"
        )));
    }
    let stored_export = store
        .export_record(commit_seq)
        .map_err(|e| unmatched(format!("read ledger export_record({commit_seq}): {e}")))?
        .ok_or_else(|| {
            unmatched(format!(
                "missing owning cell export_record at commit_seq {commit_seq}"
            ))
        })?;
    let stored_digest = stored_export
        .hash()
        .map_err(|e| unmatched(format!("hash stored export: {e}")))?;
    let given_digest = leg
        .export_record
        .hash()
        .map_err(|e| unmatched(format!("hash export record: {e}")))?;
    if stored_digest != given_digest {
        return Err(unmatched(format!(
            "export record digest disagrees with owning cell ledger at commit_seq {commit_seq}"
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

pub(super) fn extract_seller_sale_leg(
    transfer_id: &str,
    leg: &TransferLegEvidence,
) -> Result<CommittedLegDetails, OperatingGroupRefusal> {
    extract_committed_leg_fields(
        transfer_id,
        leg,
        "supply.PosSale",
        "sale_id",
        "sold_qty",
        "net_revenue",
    )
}

fn extract_buyer_lot_leg(
    transfer_id: &str,
    leg: &TransferLegEvidence,
) -> Result<CommittedLegDetails, OperatingGroupRefusal> {
    extract_committed_leg_fields(
        transfer_id,
        leg,
        "supply.StockLot",
        "lot_id",
        "received_qty",
        "opening_stock_value",
    )
}

fn extract_committed_leg_fields(
    transfer_id: &str,
    leg: &TransferLegEvidence,
    expected_type: &str,
    id_field: &str,
    qty_field: &str,
    amount_field: &str,
) -> Result<CommittedLegDetails, OperatingGroupRefusal> {
    let unmatched = |reason: String| OperatingGroupRefusal::UnmatchedInterEntityTransfer {
        transfer_id: transfer_id.to_owned(),
        reason,
    };
    if leg.object_type != expected_type {
        return Err(unmatched(format!(
            "leg must commit {expected_type}, got {}",
            leg.object_type
        )));
    }
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
                "commit batch missing Put for {}/{}",
                leg.object_type, leg.object_id
            ))
        })?;
    if !matches!(
        values.get(id_field),
        Some(RuntimeValue::String(actual)) if actual == &leg.object_id
    ) {
        return Err(unmatched(format!(
            "committed `{id_field}` disagrees with declared transfer object {}",
            leg.object_id
        )));
    }
    let currency = match values.get("currency") {
        Some(RuntimeValue::String(code)) if !code.trim().is_empty() => code.clone(),
        other => {
            return Err(unmatched(format!(
                "invalid currency on {}: {other:?}",
                leg.object_id
            )));
        }
    };
    let qty_scaled = decimal_to_scaled(extract_leg_decimal(values, qty_field, &unmatched)?)
        .map_err(unmatched)?;
    let amount_scaled = decimal_to_scaled(extract_leg_decimal(values, amount_field, &unmatched)?)
        .map_err(unmatched)?;
    if amount_scaled <= 0 {
        return Err(unmatched(format!(
            "committed `{amount_field}` must be positive, got {amount_scaled}"
        )));
    }
    Ok(CommittedLegDetails {
        currency,
        qty_scaled,
        amount_scaled,
    })
}

fn extract_leg_decimal(
    values: &BTreeMap<String, RuntimeValue>,
    field: &str,
    unmatched: &impl Fn(String) -> OperatingGroupRefusal,
) -> Result<Decimal, OperatingGroupRefusal> {
    match values.get(field) {
        Some(RuntimeValue::Number(num)) => Ok(*num),
        other => Err(unmatched(format!("missing numeric `{field}`: {other:?}"))),
    }
}
