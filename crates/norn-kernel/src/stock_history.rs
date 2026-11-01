//! Append-only stock transfer/reversal journal over a deterministic valuation fold.
pub use crate::stock_valuation::{StockChange, StockEffect, StockHistoryError, Valuation};
use std::collections::{BTreeMap, BTreeSet};

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
    let movement = |states: &mut BTreeMap<String, S>,
                    partition: &String,
                    event: &S::Movement|
     -> Result<StockChange, StockHistoryError> {
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
        StockOperation::Movement(partition, event) => Ok(vec![movement(states, partition, event)?]),
        StockOperation::Transfer(source, destination, quantity) => {
            if source == destination {
                return Err(StockHistoryError::Invalid);
            }
            let (after, effect) = states
                .get(source)
                .cloned()
                .unwrap_or_default()
                .transfer(id, *quantity)?;
            states.insert(source.clone(), after);
            let out = StockChange {
                partition: source.clone(),
                effect,
            };
            let incoming = movement(states, destination, &S::receipt(id, out.effect.amount))?;
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
