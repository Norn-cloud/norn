//! Moving-average valuation for one admitted partition. No posting authority.
use std::collections::BTreeMap;

/// Nonnegative amounts in integer millionths.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct StockAmount {
    /// Quantity in millionths.
    pub quantity: u128,
    /// Value in millionths.
    pub value: u128,
}

/// Original delivery cost and cumulative returned amounts.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Issue {
    original: StockAmount,
    returned: StockAmount,
}

/// Constructed only by the fold; an empty position always has zero value.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct AverageState {
    balance: StockAmount,
    issues: BTreeMap<String, Issue>,
}

impl AverageState {
    /// Current inventory at its remaining acquisition cost.
    pub const fn balance(&self) -> StockAmount {
        self.balance
    }
}

/// Immutable movements in authoritative commit order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AverageMovement {
    /// Acquisition at its already rounded cost.
    Receipt(StockAmount),
    /// Delivery with a unique identity retained for original-cost returns.
    Delivery(String, u128),
    /// Return against the named original delivery.
    Return(String, u128),
}

/// Inventory and exact magnitude added or consumed by the movement.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AverageResult {
    /// New immutable inventory state.
    pub state: AverageState,
    /// Unsigned magnitude; delivery subtracts, receipt/return adds.
    pub allocation: StockAmount,
}

/// Refused movement; the input remains unchanged.
#[derive(Debug, Copy, Clone, PartialEq, Eq, thiserror::Error)]
pub enum AverageError {
    /// Zero quantity, missing/duplicate delivery, or arithmetic overflow.
    #[error("invalid average movement")]
    Invalid,
    /// Delivery exceeds available stock.
    #[error("insufficient-stock")]
    InsufficientStock,
    /// Cumulative returns exceed original delivered quantity.
    #[error("over-return")]
    OverReturn,
}

/// Checked half-up proportional allocation, with exact final depletion.
fn share(quantity: u128, amount: StockAmount) -> Option<u128> {
    if quantity == amount.quantity {
        return Some(amount.value);
    }
    let whole = amount.value.checked_div(amount.quantity)?;
    let remainder = amount.value.checked_rem(amount.quantity)?;
    whole.checked_mul(quantity)?.checked_add(
        remainder
            .checked_mul(quantity)?
            .checked_add(amount.quantity.checked_div(2)?)?
            .checked_div(amount.quantity)?,
    )
}

/// Fold one movement, keeping original delivery cost after later repricing.
/// Quantities and total on-hand quantity have at most 18 decimal digits.
///
/// # Errors
/// Refuses invalid input, overflow, insufficient stock or cumulative over-return.
pub fn allocate_average(
    before: &AverageState,
    movement: &AverageMovement,
) -> Result<AverageResult, AverageError> {
    let mut state = before.clone();
    let allocation = match movement {
        AverageMovement::Receipt(amount) => *amount,
        AverageMovement::Delivery(id, quantity) => {
            if id.is_empty() || state.issues.contains_key(id) || *quantity == 0 {
                return Err(AverageError::Invalid);
            }
            if *quantity > state.balance.quantity {
                return Err(AverageError::InsufficientStock);
            }
            let original = StockAmount {
                quantity: *quantity,
                value: share(*quantity, state.balance).ok_or(AverageError::Invalid)?,
            };
            state.issues.insert(
                id.clone(),
                Issue {
                    original,
                    returned: StockAmount::default(),
                },
            );
            original
        }
        AverageMovement::Return(id, quantity) => {
            let issue = state.issues.get_mut(id).ok_or(AverageError::Invalid)?;
            let returned = issue
                .returned
                .quantity
                .checked_add(*quantity)
                .ok_or(AverageError::Invalid)?;
            if returned > issue.original.quantity {
                return Err(AverageError::OverReturn);
            }
            let remaining = issue
                .original
                .value
                .checked_sub(issue.returned.value)
                .ok_or(AverageError::Invalid)?;
            let value = if returned == issue.original.quantity {
                remaining
            } else {
                share(*quantity, issue.original)
                    .ok_or(AverageError::Invalid)?
                    .min(remaining)
            };
            issue.returned = StockAmount {
                quantity: returned,
                value: issue
                    .returned
                    .value
                    .checked_add(value)
                    .ok_or(AverageError::Invalid)?,
            };
            StockAmount {
                quantity: *quantity,
                value,
            }
        }
    };
    if allocation.quantity == 0 || allocation.quantity > 999_999_999_999_999_999 {
        return Err(AverageError::Invalid);
    }
    let balance = state.balance;
    state.balance = if matches!(movement, AverageMovement::Delivery(..)) {
        StockAmount {
            quantity: balance
                .quantity
                .checked_sub(allocation.quantity)
                .ok_or(AverageError::Invalid)?,
            value: balance
                .value
                .checked_sub(allocation.value)
                .ok_or(AverageError::Invalid)?,
        }
    } else {
        StockAmount {
            quantity: balance
                .quantity
                .checked_add(allocation.quantity)
                .ok_or(AverageError::Invalid)?,
            value: balance
                .value
                .checked_add(allocation.value)
                .ok_or(AverageError::Invalid)?,
        }
    };
    if state.balance.quantity > 999_999_999_999_999_999 {
        return Err(AverageError::Invalid);
    }
    Ok(AverageResult { state, allocation })
}

/// Replay immutable movements without a stored snapshot.
///
/// # Errors
/// Returns the first refused movement.
pub fn replay_average(movements: &[AverageMovement]) -> Result<AverageState, AverageError> {
    movements
        .iter()
        .try_fold(AverageState::default(), |state, movement| {
            allocate_average(&state, movement).map(|result| result.state)
        })
}
