//! Method-owned valuation effects; transfers never create returnable deliveries.
use crate::stock_average::{AverageMovement, AverageState, StockAmount, allocate_average};

/// Fold contract: all valuation comes from the selected method.
pub trait Valuation: Default + Clone {
    /// Admitted receipt, delivery or original-cost return.
    type Movement: Clone;
    /// Apply without modifying the input.
    /// # Errors
    /// Refuses invalid valuation.
    fn apply(&self, movement: &Self::Movement) -> Result<(Self, StockEffect), StockHistoryError>;
    /// Source issue for a transfer.
    fn delivery(id: &str, quantity: u128) -> Self::Movement;
    /// Deplete a transfer source without creating a returnable delivery.
    /// # Errors
    /// Refuses invalid or insufficient source stock.
    fn transfer(&self, id: &str, quantity: u128) -> Result<(Self, StockEffect), StockHistoryError>;
    /// Destination receipt carries exactly the source allocation cost.
    fn receipt(id: &str, amount: StockAmount) -> Self::Movement;
}

/// Exact stock event magnitude and direction; inverse flips direction only.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StockEffect {
    /// True for receipt/return, false for delivery.
    pub incoming: bool,
    /// Quantity and value in millionths.
    pub amount: StockAmount,
    /// Consumed source identities/costs; empty for moving average.
    pub allocations: Vec<(String, StockAmount)>,
}

/// Partition-tagged stock effect for atomic posting lowering.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StockChange {
    /// Company/item/warehouse/ownership partition identity, admitted by the caller.
    pub partition: String,
    /// Exact amount and direction.
    pub effect: StockEffect,
}

/// Stable refusal; no input history is modified.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum StockHistoryError {
    /// Missing/duplicate identity, same-partition transfer or invalid cancellation.
    #[error("invalid stock history")]
    Invalid,
    /// The selected valuation method refused the movement.
    #[error("valuation refused")]
    Valuation,
    /// A live subsequent posting still requires the original allocation.
    #[error("dependent-stock-event")]
    Dependents,
}

impl Valuation for AverageState {
    type Movement = AverageMovement;
    fn apply(&self, movement: &Self::Movement) -> Result<(Self, StockEffect), StockHistoryError> {
        let result = allocate_average(self, movement).map_err(StockHistoryError::from)?;
        Ok((
            result.state,
            StockEffect {
                incoming: !matches!(movement, AverageMovement::Delivery(..)),
                amount: result.allocation,
                allocations: Vec::new(),
            },
        ))
    }
    fn delivery(id: &str, quantity: u128) -> Self::Movement {
        AverageMovement::Delivery(id.into(), quantity)
    }
    fn transfer(&self, id: &str, quantity: u128) -> Result<(Self, StockEffect), StockHistoryError> {
        let result = self
            .transfer_out(id, quantity)
            .map_err(StockHistoryError::from)?;
        Ok((
            result.state,
            StockEffect {
                incoming: false,
                amount: result.allocation,
                allocations: Vec::new(),
            },
        ))
    }
    fn receipt(_: &str, amount: StockAmount) -> Self::Movement {
        AverageMovement::Receipt(amount)
    }
}

impl From<crate::stock_average::AverageError> for StockHistoryError {
    fn from(error: crate::stock_average::AverageError) -> Self {
        match error {
            crate::stock_average::AverageError::Invalid => Self::Invalid,
            crate::stock_average::AverageError::InsufficientStock
            | crate::stock_average::AverageError::OverReturn => Self::Valuation,
        }
    }
}
