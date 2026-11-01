//! FIFO adapter for the shared immutable transfer/cancellation journal.
use crate::{
    stock_average::StockAmount,
    stock_fifo::{FifoLayer, FifoMovement, allocate_fifo},
    stock_history::{StockEffect, StockHistoryError, Valuation},
};

/// Remaining receipt layers in authoritative arrival order.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct FifoState {
    layers: Vec<FifoLayer>,
}
impl FifoState {
    /// Exact remaining layers with acquisition provenance.
    #[must_use]
    pub fn layers(&self) -> &[FifoLayer] {
        &self.layers
    }
}
impl Valuation for FifoState {
    type Movement = FifoMovement;
    fn apply(&self, movement: &Self::Movement) -> Result<(Self, StockEffect), StockHistoryError> {
        let result = allocate_fifo(&self.layers, movement)
            .map_err(|_refusal| StockHistoryError::Valuation)?;
        let amount = match movement {
            FifoMovement::Receipt(layer) => StockAmount {
                quantity: layer.quantity,
                value: layer.value,
            },
            FifoMovement::Delivery(_) => {
                result
                    .allocations
                    .iter()
                    .try_fold(StockAmount::default(), |total, layer| {
                        Ok::<StockAmount, StockHistoryError>(StockAmount {
                            quantity: total
                                .quantity
                                .checked_add(layer.quantity)
                                .ok_or(StockHistoryError::Valuation)?,
                            value: total
                                .value
                                .checked_add(layer.value)
                                .ok_or(StockHistoryError::Valuation)?,
                        })
                    })?
            }
        };
        let allocations = result
            .allocations
            .into_iter()
            .map(|layer| {
                (
                    layer.receipt,
                    StockAmount {
                        quantity: layer.quantity,
                        value: layer.value,
                    },
                )
            })
            .collect();
        Ok((
            Self {
                layers: result.layers,
            },
            StockEffect {
                incoming: matches!(movement, FifoMovement::Receipt(_)),
                amount,
                allocations,
            },
        ))
    }
    fn delivery(_: &str, quantity: u128) -> Self::Movement {
        FifoMovement::Delivery(quantity)
    }
    fn transfer(&self, id: &str, quantity: u128) -> Result<(Self, StockEffect), StockHistoryError> {
        self.apply(&Self::delivery(id, quantity))
    }
    fn receipt(id: &str, amount: StockAmount) -> Self::Movement {
        FifoMovement::Receipt(FifoLayer {
            receipt: id.into(),
            quantity: amount.quantity,
            value: amount.value,
        })
    }
}
