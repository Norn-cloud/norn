//! Generic exact posting admission, independent of domain reference state.

use norn_ir::posting::{AccountingError, AccountingLegIr};
use norn_ir::{Amount, ExactSum};
use std::collections::{BTreeMap, BTreeSet};

/// Validate a runtime posting group's exact accounting legs without requiring
/// an accounting state snapshot.
///
/// Cell commit admission uses this adapter for GL leg records declared on an
/// object type. It reuses the same canonical `i128` parser and grouped balance
/// authority as the quantity checker; account existence and unit
/// permissions still require the full account graph and are checked by the
/// accounting-pack API.
///
/// # Errors
///
/// Returns an [`AccountingError`] when an identifier is invalid, fewer than two
/// legs are supplied, legs disagree on account authority or unit precision, a
/// leg amount cannot be parsed or reversed, arithmetic overflows, or the legs
/// do not balance per `(unit, layer)` partition.
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
    let mut balance = Balance::default();
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
        let parsed = parse_amount(&leg.amount_unscaled)?;
        let amount = Amount::new(parsed).ok_or(AccountingError::AmountNotReversible)?;
        balance.record(leg, amount)?;
    }
    balance.validate_balance()
}

#[derive(Default)]
struct Balance<'a> {
    partitions: BTreeMap<(&'a str, &'a str), ExactSum>,
    units: BTreeMap<&'a str, ExactSum>,
    total: ExactSum,
}

impl<'a> Balance<'a> {
    fn record(&mut self, leg: &'a AccountingLegIr, amount: Amount) -> Result<(), AccountingError> {
        let unit = leg.unit_id.as_str();
        for sum in [
            self.partitions.entry((unit, &leg.layer_id)).or_default(),
            self.units.entry(unit).or_default(),
            &mut self.total,
        ] {
            *sum = sum
                .add(amount.get())
                .ok_or(AccountingError::ArithmeticOverflow)?;
        }
        Ok(())
    }

    fn validate_balance(&self) -> Result<(), AccountingError> {
        if self.partitions.values().all(|s| s.is_zero()) {
            Ok(())
        } else if self.units.values().all(|s| s.is_zero()) {
            Err(AccountingError::JournalUnbalancedByLayer)
        } else if self.total.is_zero() {
            Err(AccountingError::JournalUnbalancedByUnit)
        } else {
            Err(AccountingError::JournalUnbalanced)
        }
    }
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

#[cfg(test)]
mod tests {
    use super::*;
    use norn_ir::posting::AccountingAccountRef;
    use proptest::prelude::*;

    fn leg(id: usize, unit: &str, amount: i128) -> AccountingLegIr {
        AccountingLegIr {
            leg_id: id.to_string(),
            account: AccountingAccountRef {
                scope_key: "scope".into(),
                book_id: "book".into(),
                graph_id: "graph".into(),
                graph_version: 1,
                account_id: "account".into(),
            },
            unit_id: unit.into(),
            unit_precision: 0,
            layer_id: "posted".into(),
            amount_unscaled: amount.to_string(),
        }
    }

    #[test]
    fn balanced_two_unit_journal_accepts_all_24_orders() {
        let legs = [
            leg(0, "U", i128::MAX),
            leg(1, "V", 1),
            leg(2, "U", -i128::MAX),
            leg(3, "V", -1),
        ];
        let mut checked = 0;
        for a in &legs {
            for b in legs.iter().filter(|b| *b != a) {
                for c in legs.iter().filter(|c| *c != a && *c != b) {
                    for d in legs.iter().filter(|d| *d != a && *d != b && *d != c) {
                        assert_eq!(
                            validate_balanced_accounting_legs(
                                "journal",
                                &[a.clone(), b.clone(), c.clone(), d.clone()]
                            ),
                            Ok(())
                        );
                        checked += 1;
                    }
                }
            }
        }
        assert_eq!(checked, 24);
    }

    proptest! {
        #[test]
        fn permuting_postings_preserves_exact_state_and_verdict(
            inputs in prop::collection::vec((any::<u8>(), any::<bool>(), -i128::MAX..=i128::MAX), 2..32)
        ) {
            let mut ordered: Vec<_> = inputs.into_iter().enumerate()
                .map(|(id,(key,u,v))| (key, leg(id, if u { "U" } else { "V" }, v), v)).collect();
            let evaluate = |entries: &[(u8, AccountingLegIr, i128)]| {
                let legs: Vec<_> = entries.iter().map(|(_,leg,_)| leg.clone()).collect();
                (entries.iter().try_fold(ExactSum::ZERO, |sum, (_,_,v)| sum.add(*v)),
                 validate_balanced_accounting_legs("journal", &legs))
            };
            let original = evaluate(&ordered);
            ordered.sort_by_key(|(key,_,_)| *key);
            prop_assert_eq!(evaluate(&ordered), original);
        }
    }
}
