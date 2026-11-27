//! P3 (one write door): the engine's own writes, as a closed set.
//!
//! A business write reaches storage only through the governed door
//! ([`CellStore::commit_guarded_submitted`]), whose guard must hand back the
//! [`norn_kernel::CommitSeal`] that the kernel's shared admission check mints.
//! Everything else the engine writes is one of the [`SystemWrite`] variants
//! below: each takes a fixed, typed input and builds its batch here, so no
//! caller can hand the store an arbitrary batch. There is no raw, unchecked
//! commit in any build another crate links; the crate's own suites reach a raw
//! batch through `crate::test_door` (`#[cfg(test)]` only).
//!
//! Adding a variant is a reviewed edit: it widens what the engine writes
//! without the kernel check.
//!
//! ```compile_fail,E0599
//! // The raw, unchecked commit is gone.
//! use norn_cell::{CellStore, CommitBatch};
//! fn bypass(store: &mut CellStore) {
//!     let _ = store.commit_unchecked(&CommitBatch::default());
//! }
//! ```
//!
//! ```compile_fail,E0599
//! // So is its probed variant, outside this crate's own test build.
//! use norn_cell::{CellStore, CommitBatch};
//! fn bypass(store: &mut CellStore) {
//!     let _ = store.commit_probed_unchecked(&CommitBatch::default(), &mut |_| Ok(()));
//! }
//! ```
//!
//! ```compile_fail,E0599
//! // And a guarded commit whose guard admits anything.
//! use norn_cell::{CellStore, CommitBatch};
//! fn bypass(store: &mut CellStore) {
//!     let _ = store.commit_guarded_unchecked(
//!         &CommitBatch::default(),
//!         |_reader| Ok::<(), ()>(()),
//!         &mut |_| Ok(()),
//!     );
//! }
//! ```
//!
//! ```compile_fail,E0624
//! // The system writes are private to the engine.
//! use norn_cell::CellStore;
//! fn bypass(store: &mut CellStore) {
//!     let _ = store.apply_system_guarded(todo!(), |_| Ok(()), &mut |_| Ok(()));
//! }
//! ```
//!
//! ```compile_fail,E0624
//! // The governed door is private to the engine too.
//! use norn_cell::{CellStore, CommitBatch};
//! fn bypass(store: &mut CellStore) {
//!     let _ = store.commit_guarded_submitted(
//!         &CommitBatch::default(),
//!         None,
//!         None,
//!         |_reader| Err::<(norn_kernel::CommitSeal, (), Option<()>), ()>(()),
//!         &mut |_| Ok(()),
//!     );
//! }
//! ```

use norn_ir::RuntimeValue as Value;

use super::{
    CellStore, CommitBatch, CommitOrigin, CommitOutcome, CommitPoint, Consumption, EffectRow,
    FactWriteRow, GuardedError, MarkingDeclassification,
};
use crate::engine::SqlEngine;
use crate::error::CellError;
use crate::read::StoreReader;

/// Where and when a system write lands.
pub(crate) struct At<'a> {
    /// The world written.
    pub(crate) world: &'a str,
    /// The tenant scope the store is bound to.
    pub(crate) scope: &'a str,
    /// The commit's logical time.
    pub(crate) logical_time: u64,
}

/// One engine-internal write. A closed, reviewed set (see the [module](self)).
pub(crate) enum SystemWrite {
    /// A host projection's deterministic findings, once per idempotency key.
    HostEvents {
        /// The retry key.
        idempotency_key: String,
        /// The finding events.
        events: Vec<Value>,
    },
    /// One privacy receipt event (a reveal, a guarded model call), or a
    /// declassification with its receipt event.
    Privacy {
        /// The retry key.
        idempotency_key: String,
        /// The receipt event.
        event: Value,
        /// The declassification the event records, if any.
        declassification: Option<Box<MarkingDeclassification>>,
    },
    /// One quantity operation; an Expire consumes its timer incarnation.
    Quantity {
        /// The retry key.
        idempotency_key: String,
        /// The operation.
        request: Box<norn_ir::quantity_ledger::QuantityRequest>,
        /// The due timer (id, incarnation) an Expire consumes.
        timer: Option<(String, u64)>,
    },
    /// Durably claim the outbox head generation before external dispatch.
    ClaimPerform {
        /// The head command.
        command_id: String,
        /// Its generation.
        generation: u64,
    },
    /// Durably record an outbox reconciliation decision (D1).
    ReconcileOutbox {
        /// The command being reconciled.
        command_id: String,
        /// The generation being reconciled.
        generation: u64,
        /// The durable reconciliation decision.
        decision: norn_kernel::world::outbox_ledger::ReconciliationDecision,
        /// Optional confirmed outcome payload when `decision` is
        /// `ConfirmedOutcome`.
        confirmed_outcome: Option<Value>,
        /// Optional restore-fence generation active when committed.
        fenced_at: Option<u64>,
    },
    /// Durably record a compensation attempt outcome for an outbox operation (D1).
    RecordCompensation {
        /// The command being compensated.
        command_id: String,
        /// The generation being compensated.
        generation: u64,
        /// Whether compensation completed.
        ok: bool,
        /// Optional restore-fence generation active when committed.
        fenced_at: Option<u64>,
    },
    /// Durably attach an external outcome verified by `proof` (`D1`).
    RecordLateOutcome {
        /// The command id.
        command_id: String,
        /// Exact immutable claim proof of the generation that produced this outcome.
        proof: norn_kernel::world::outbox_ledger::DispatchProof,
        /// Whether the external outcome succeeded.
        ok: bool,
        /// The external outcome payload.
        outcome: Value,
    },
    /// Fixed provisioning rows (the demo canon, recorded K1 conversions).
    Provision {
        /// The retry key, for a load that runs once.
        idempotency_key: Option<String>,
        /// The rows.
        writes: Vec<FactWriteRow>,
    },
    /// A dataset manifest's ledger event (MANIFEST-COMMIT-1).
    DatasetManifest {
        /// The request's retry key.
        idempotency_key: Option<String>,
        /// The manifest event.
        event: Value,
    },
    /// One restatement slot incarnation (OUTPUT-ATOMIC).
    RestatementSlot {
        /// The slot row.
        write: Box<FactWriteRow>,
        /// Its lifecycle transition, if any.
        transition: Option<Box<norn_ir::FactChange>>,
    },
}

impl SystemWrite {
    fn outbox_effects(self) -> Vec<EffectRow> {
        match self {
            Self::ClaimPerform {
                command_id,
                generation,
            } => vec![EffectRow::ClaimPerform {
                command_id,
                generation,
            }],
            Self::ReconcileOutbox {
                command_id,
                generation,
                decision,
                confirmed_outcome,
                fenced_at,
            } => vec![EffectRow::ReconcileOutbox {
                command_id,
                generation,
                decision,
                confirmed_outcome,
                fenced_at,
            }],
            Self::RecordCompensation {
                command_id,
                generation,
                ok,
                fenced_at,
            } => vec![EffectRow::RecordCompensation {
                command_id,
                generation,
                ok,
                fenced_at,
            }],
            Self::RecordLateOutcome {
                command_id,
                proof,
                ok,
                outcome,
            } => vec![EffectRow::RecordLateOutcome {
                command_id,
                proof,
                ok,
                outcome,
            }],
            Self::HostEvents { .. }
            | Self::Privacy { .. }
            | Self::Quantity { .. }
            | Self::Provision { .. }
            | Self::DatasetManifest { .. }
            | Self::RestatementSlot { .. } => Vec::new(),
        }
    }

    /// The batch this write commits at `at`.
    fn batch(self, at: &At<'_>) -> Result<CommitBatch, CellError> {
        let mut batch = CommitBatch {
            world: at.world.to_owned(),
            scope: at.scope.to_owned(),
            logical_time: at.logical_time,
            ..CommitBatch::default()
        };
        match self {
            Self::HostEvents {
                idempotency_key,
                events,
            } => {
                batch.idempotency_key = Some(idempotency_key);
                batch.events = events;
            }
            Self::Privacy {
                idempotency_key,
                event,
                declassification,
            } => {
                batch.idempotency_key = Some(idempotency_key);
                batch.events = vec![event];
                batch.marking_declassifications =
                    declassification.into_iter().map(|change| *change).collect();
            }
            Self::Quantity {
                idempotency_key,
                request,
                timer,
            } => {
                batch.idempotency_key = Some(idempotency_key);
                batch.quantity = Some((*request).into());
                batch.consumes = timer.map(|(timer_id, incarnation)| Consumption::Timer {
                    timer_id,
                    incarnation,
                });
            }
            write @ (Self::ClaimPerform { .. }
            | Self::ReconcileOutbox { .. }
            | Self::RecordCompensation { .. }
            | Self::RecordLateOutcome { .. }) => {
                batch.effects = write.outbox_effects();
            }
            Self::Provision {
                idempotency_key,
                writes,
            } => {
                batch.idempotency_key = idempotency_key;
                batch.writes = writes;
            }
            Self::DatasetManifest {
                idempotency_key,
                event,
            } => {
                batch.idempotency_key = idempotency_key;
                batch.events = vec![event];
            }
            Self::RestatementSlot { write, transition } => {
                if !matches!(*write, FactWriteRow::Put { .. }) {
                    return Err(CellError::Encoding(
                        "a restatement slot is written whole".to_owned(),
                    ));
                }
                batch.writes = vec![*write];
                batch.facts = transition.into_iter().map(|change| *change).collect();
            }
        }
        Ok(batch)
    }
}

impl<E: SqlEngine> CellStore<E> {
    /// Commit one [`SystemWrite`] at `at`, atomically.
    ///
    /// # Errors
    ///
    /// Any [`CellError`]; on error nothing was written.
    pub(crate) fn apply_system(
        &mut self,
        at: &At<'_>,
        write: SystemWrite,
    ) -> Result<CommitOutcome, CellError> {
        self.apply_system_guarded(at, write, |_reader| Ok(()), &mut |_point| Ok(()))
    }

    /// [`Self::apply_system`] after `guard` admits it inside the same
    /// transaction, with a fault probe at every [`CommitPoint`].
    ///
    /// # Errors
    ///
    /// The guard's refusal, or any [`CellError`]; on error nothing was written.
    pub(crate) fn apply_system_guarded(
        &mut self,
        at: &At<'_>,
        write: SystemWrite,
        guard: impl FnOnce(&StoreReader<'_, E>) -> Result<(), CellError>,
        probe: &mut dyn FnMut(CommitPoint) -> Result<(), CellError>,
    ) -> Result<CommitOutcome, CellError> {
        let batch = write.batch(at)?;
        match self.commit_guarded_inner(
            &batch,
            None,
            None,
            CommitOrigin::Current,
            |reader| guard(reader).map(|()| ((), None)),
            probe,
        ) {
            Ok(((), outcome)) => Ok(outcome),
            Err(GuardedError::Guard(error) | GuardedError::Store(error)) => Err(error),
        }
    }
}
