//! Append-only stock transfer/reversal journal over a deterministic valuation fold.
use crate::stock_average::{AverageMovement, AverageState, StockAmount, allocate_average};
use std::collections::{BTreeMap, BTreeSet};

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

/// One admitted operation; transfers are indivisible.
#[derive(Debug, Clone)]
pub enum StockOperation<M> {
    /// One partition's native valuation movement.
    Movement(String, M),
    /// Source, destination and quantity; value is computed only by the fold.
    Transfer(String, String, u128),
}

/// Posting or exact inverse of an earlier posting.
#[derive(Debug, Clone)]
pub enum StockEventKind<M> {
    /// Newly allocated movement/transfer.
    Post(StockOperation<M>),
    /// Identity of the posting whose effects are inverted.
    Cancel(String),
}
/// Immutable posting or cancellation record.
#[derive(Debug, Clone)]
pub struct StockEvent<M> {
    /// Unique journal event identity, including cancellation events.
    pub id: String,
    /// Mutually exclusive posting/cancellation.
    pub kind: StockEventKind<M>,
    /// Exact changes; cancellation contains the original changes' inverses.
    pub changes: Vec<StockChange>,
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

/// Immutable history. Partition keys and method selection are admitted upstream.
#[derive(Debug, Clone)]
pub struct StockHistory<S: Valuation> {
    events: Vec<StockEvent<S::Movement>>,
}
impl<S: Valuation> Default for StockHistory<S> {
    fn default() -> Self {
        Self { events: Vec::new() }
    }
}

fn apply<S: Valuation>(
    states: &mut BTreeMap<String, S>,
    id: &str,
    operation: &StockOperation<S::Movement>,
) -> Result<Vec<StockChange>, StockHistoryError> {
    let mut movement = |partition: &String, event: &S::Movement| {
        let (after, effect) = states
            .get(partition)
            .cloned()
            .unwrap_or_default()
            .apply(event)?;
        states.insert(partition.clone(), after);
        Ok(StockChange {
            partition: partition.clone(),
            effect,
        })
    };
    match operation {
        StockOperation::Movement(partition, event) => Ok(vec![movement(partition, event)?]),
        StockOperation::Transfer(source, destination, quantity) => {
            if source == destination {
                return Err(StockHistoryError::Invalid);
            }
            let out = movement(source, &S::delivery(id, *quantity))?;
            let incoming = movement(destination, &S::receipt(id, out.effect.amount))?;
            if out.effect.incoming
                || !incoming.effect.incoming
                || out.effect.amount != incoming.effect.amount
            {
                return Err(StockHistoryError::Valuation);
            }
            Ok(vec![out, incoming])
        }
    }
}

impl<S: Valuation> StockHistory<S> {
    /// Immutable events in authoritative commit order.
    #[must_use]
    pub fn events(&self) -> &[StockEvent<S::Movement>] {
        &self.events
    }
    /// Replay active postings, requiring every later allocation to remain exact.
    /// # Errors
    /// Refuses a cancellation while any remaining posting depends on its cost/layers.
    pub fn positions(&self) -> Result<BTreeMap<String, S>, StockHistoryError> {
        self.without(None)
    }
    fn without(&self, candidate: Option<&str>) -> Result<BTreeMap<String, S>, StockHistoryError> {
        let cancelled: BTreeSet<_> = self
            .events
            .iter()
            .filter_map(|event| match &event.kind {
                StockEventKind::Cancel(id) => Some(id.as_str()),
                StockEventKind::Post(_) => None,
            })
            .chain(candidate)
            .collect();
        let mut states = BTreeMap::new();
        for event in &self.events {
            if cancelled.contains(event.id.as_str()) {
                continue;
            }
            if let StockEventKind::Post(operation) = &event.kind {
                let changes = apply(&mut states, &event.id, operation)
                    .map_err(|_refusal| StockHistoryError::Dependents)?;
                if changes != event.changes {
                    return Err(StockHistoryError::Dependents);
                }
            }
        }
        Ok(states)
    }
    fn check_id(&self, id: &str) -> Result<(), StockHistoryError> {
        if id.is_empty() || self.events.iter().any(|event| event.id == id) {
            return Err(StockHistoryError::Invalid);
        }
        Ok(())
    }
    /// Append a movement or atomic transfer, with no caller-supplied valuation.
    /// # Errors
    /// Refuses duplicate identity or invalid movement; neither partition changes.
    pub fn post(
        &self,
        id: String,
        operation: StockOperation<S::Movement>,
    ) -> Result<Self, StockHistoryError> {
        self.check_id(&id)?;
        let changes = apply(&mut self.positions()?, &id, &operation)?;
        let mut after = self.clone();
        after.events.push(StockEvent {
            id,
            kind: StockEventKind::Post(operation),
            changes,
        });
        Ok(after)
    }
    /// Append exact inverse effects only after all live allocation dependents reverse.
    /// # Errors
    /// Refuses unknown/repeated cancellation, duplicate identity or live dependents.
    pub fn cancel(&self, id: String, original: &str) -> Result<Self, StockHistoryError> {
        self.check_id(&id)?;
        let event = self
            .events
            .iter()
            .find(|event| event.id == original && matches!(event.kind, StockEventKind::Post(_)))
            .ok_or(StockHistoryError::Invalid)?;
        if self
            .events
            .iter()
            .any(|event| matches!(&event.kind, StockEventKind::Cancel(id) if id == original))
        {
            return Err(StockHistoryError::Invalid);
        }
        self.without(Some(original))?;
        let changes = event
            .changes
            .iter()
            .map(|change| StockChange {
                partition: change.partition.clone(),
                effect: StockEffect {
                    incoming: !change.effect.incoming,
                    amount: change.effect.amount,
                    allocations: change.effect.allocations.clone(),
                },
            })
            .collect();
        let mut after = self.clone();
        after.events.push(StockEvent {
            id,
            kind: StockEventKind::Cancel(original.into()),
            changes,
        });
        Ok(after)
    }
}

impl Valuation for AverageState {
    type Movement = AverageMovement;
    fn apply(&self, movement: &Self::Movement) -> Result<(Self, StockEffect), StockHistoryError> {
        let result =
            allocate_average(self, movement).map_err(|_refusal| StockHistoryError::Valuation)?;
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
    fn receipt(_: &str, amount: StockAmount) -> Self::Movement {
        AverageMovement::Receipt(amount)
    }
}
