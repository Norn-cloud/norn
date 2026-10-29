//! Generic exact posting admission, independent of domain reference state.

use norn_ir::posting::{AccountingError, AccountingLegIr};
use std::collections::{BTreeMap, BTreeSet};

/// Validate a runtime posting group's exact accounting legs without requiring
/// an accounting state snapshot.
///
/// Cell commit admission uses this adapter for GL leg records declared on an
/// object type. It reuses the same canonical `i128` parser and grouped balance
/// authority as the quantity checker; account existence and unit
/// permissions still require the full account graph and are checked by the
/// accounting-pack API.
pub fn validate_balanced_accounting_legs(
    journal_id: &str,
    legs: &[AccountingLegIr],
) -> Result<(), AccountingError> {
    validate_identity(journal_id)?;
    if legs.len() < 2 {
        return Err(AccountingError::TooFewLegs);
    }

    let mut leg_ids = BTreeSet::new();
    let mut unit_precisions = BTreeMap::<&str, u8>::new();
    let Some(first) = legs.first() else {
        return Err(AccountingError::TooFewLegs);
    };
    let authority = (
        first.account.scope_key.as_str(),
        first.account.book_id.as_str(),
        first.account.graph_id.as_str(),
        first.account.graph_version,
    );
    for leg in legs {
        for identity in [
            leg.leg_id.as_str(),
            leg.account.scope_key.as_str(),
            leg.account.book_id.as_str(),
            leg.account.graph_id.as_str(),
            leg.account.account_id.as_str(),
            leg.unit_id.as_str(),
            leg.layer_id.as_str(),
        ] {
            validate_identity(identity)?;
        }
        if !leg_ids.insert(leg.leg_id.as_str()) {
            return Err(AccountingError::DuplicateLeg);
        }
        if (
            leg.account.scope_key.as_str(),
            leg.account.book_id.as_str(),
            leg.account.graph_id.as_str(),
            leg.account.graph_version,
        ) != authority
        {
            return Err(AccountingError::AccountReferenceMismatch);
        }
        if leg.unit_precision > norn_ir::quantity_ledger::MAX_LEDGER_SCALE
            || unit_precisions
                .insert(leg.unit_id.as_str(), leg.unit_precision)
                .is_some_and(|precision| precision != leg.unit_precision)
        {
            return Err(AccountingError::UnitPrecisionMismatch);
        }
        let amount = parse_amount(leg.amount_unscaled.as_str())?;
        if amount == i128::MIN {
            return Err(AccountingError::AmountNotReversible);
        }
    }

    validate_balance(legs)
}

pub(crate) fn validate_balance(legs: &[AccountingLegIr]) -> Result<(), AccountingError> {
    let mut partitions = BTreeMap::<(&str, &str), i128>::new();
    let mut units = BTreeMap::<&str, i128>::new();
    let mut total = 0_i128;
    for leg in legs {
        let amount = parse_amount(leg.amount_unscaled.as_str())?;
        let partition = partitions
            .entry((leg.unit_id.as_str(), leg.layer_id.as_str()))
            .or_default();
        *partition = partition
            .checked_add(amount)
            .ok_or(AccountingError::ArithmeticOverflow)?;
        let unit = units.entry(leg.unit_id.as_str()).or_default();
        *unit = unit
            .checked_add(amount)
            .ok_or(AccountingError::ArithmeticOverflow)?;
        total = total
            .checked_add(amount)
            .ok_or(AccountingError::ArithmeticOverflow)?;
    }
    if partitions.values().all(|amount| *amount == 0) {
        return Ok(());
    }
    if units.values().all(|amount| *amount == 0) {
        return Err(AccountingError::JournalUnbalancedByLayer);
    }
    if total == 0 {
        return Err(AccountingError::JournalUnbalancedByUnit);
    }
    Err(AccountingError::JournalUnbalanced)
}

pub(crate) fn parse_amount(value: &str) -> Result<i128, AccountingError> {
    let parsed = value
        .parse::<i128>()
        .map_err(|_parse_error| AccountingError::InvalidAmount)?;
    if parsed.to_string() != value {
        return Err(AccountingError::InvalidAmount);
    }
    Ok(parsed)
}

pub(crate) fn validate_identity(value: &str) -> Result<(), AccountingError> {
    if value.is_empty() || value.trim() != value {
        Err(AccountingError::InvalidIdentity)
    } else {
        Ok(())
    }
}
