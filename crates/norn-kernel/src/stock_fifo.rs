//! Pure FIFO allocation over one admitted stock partition, independent of hosts.
//! Amounts are integer millionths. This fold grants no posting authority.

use serde::{Deserialize, Serialize};

/// Maximum quantity of a receipt: twelve integer and six fractional digits.
pub const MAX_RECEIPT_QUANTITY: u128 = 999_999_999_999_999_999;

/// Remaining receipt cost, with immutable provenance.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FifoLayer {
    /// Original receipt identity.
    pub receipt: String,
    /// Remaining quantity in millionths.
    pub quantity: u128,
    /// Remaining value in millionths.
    pub value: u128,
}

/// Immutable accepted movements in authoritative commit order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FifoMovement {
    /// Receipt with its already rounded acquisition value.
    Receipt(FifoLayer),
    /// Delivery quantity in millionths.
    Delivery(u128),
}

/// Exact issue allocation and resulting inventory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FifoResult {
    /// Remaining layers in receipt order.
    pub layers: Vec<FifoLayer>,
    /// Consumed source layers, including their exact allocated value.
    pub allocations: Vec<FifoLayer>,
}

/// Invalid history or an issue that would create negative stock.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum FifoError {
    /// Nonpositive quantity, invalid layer, duplicate active receipt or overflow.
    #[error("invalid FIFO input")]
    Invalid,
    /// Delivery exceeds available quantity.
    #[error("insufficient-stock")]
    InsufficientStock,
}

/// Half-up proportional value without overflowing the intermediate product.
fn share(quantity: u128, layer: &FifoLayer) -> Option<u128> {
    if quantity == layer.quantity {
        return Some(layer.value);
    }
    let whole = layer.value.checked_div(layer.quantity)?;
    let remainder = layer.value.checked_rem(layer.quantity)?;
    let rounded = remainder
        .checked_mul(quantity)?
        .checked_add(layer.quantity.checked_div(2)?)?
        .checked_div(layer.quantity)?;
    whole.checked_mul(quantity)?.checked_add(rounded)
}

/// Apply one movement without mutating the caller's state on success or refusal.
///
/// # Errors
/// Returns invalid input/overflow or insufficient stock without changing layers.
pub fn allocate_fifo(
    layers: &[FifoLayer],
    movement: &FifoMovement,
) -> Result<FifoResult, FifoError> {
    if layers.iter().any(|layer| {
        layer.quantity == 0 || layer.quantity > MAX_RECEIPT_QUANTITY || layer.receipt.is_empty()
    }) {
        return Err(FifoError::Invalid);
    }
    match movement {
        FifoMovement::Receipt(layer) => {
            if layer.quantity == 0
                || layer.quantity > MAX_RECEIPT_QUANTITY
                || layer.receipt.is_empty()
                || layers.iter().any(|prior| prior.receipt == layer.receipt)
            {
                return Err(FifoError::Invalid);
            }
            let mut layers = layers.to_vec();
            layers.push(layer.clone());
            Ok(FifoResult {
                layers,
                allocations: vec![],
            })
        }
        FifoMovement::Delivery(quantity) => {
            if *quantity == 0 {
                return Err(FifoError::Invalid);
            }
            let mut remaining = *quantity;
            let mut allocations = Vec::new();
            let mut after = Vec::new();
            for layer in layers {
                let taken = remaining.min(layer.quantity);
                let value = share(taken, layer).ok_or(FifoError::Invalid)?;
                remaining = remaining.checked_sub(taken).ok_or(FifoError::Invalid)?;
                if taken > 0 {
                    allocations.push(FifoLayer {
                        receipt: layer.receipt.clone(),
                        quantity: taken,
                        value,
                    });
                }
                if taken < layer.quantity {
                    after.push(FifoLayer {
                        receipt: layer.receipt.clone(),
                        quantity: layer
                            .quantity
                            .checked_sub(taken)
                            .ok_or(FifoError::Invalid)?,
                        value: layer.value.checked_sub(value).ok_or(FifoError::Invalid)?,
                    });
                }
            }
            if remaining != 0 {
                return Err(FifoError::InsufficientStock);
            }
            Ok(FifoResult {
                layers: after,
                allocations,
            })
        }
    }
}

/// Reconstruct inventory from the same immutable sequence; no stored snapshot needed.
///
/// # Errors
/// Returns the first invalid movement or insufficient-stock refusal in the history.
pub fn replay_fifo(movements: &[FifoMovement]) -> Result<Vec<FifoLayer>, FifoError> {
    movements.iter().try_fold(Vec::new(), |layers, movement| {
        allocate_fifo(&layers, movement).map(|result| result.layers)
    })
}
