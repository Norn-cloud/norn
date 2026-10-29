//! The [`CellHost`] driver over the normalized SQLite store
//! (T4-CELL-2, #2676).
//!
//! [`SqliteCell`] implements the kernel's four-verb trait over one
//! [`CellStore`]:
//!
//! - **commit** runs the kernel's own admission check
//!   ([`check_commit`]) *inside* the store's commit transaction
//!   ([`CellStore::commit_guarded`]), then writes the consumed delivery, the
//!   whole-object writes, the ledger events, and the command/timer enqueues
//!   in that one SQLite transaction: commit and enqueue are atomic, and a
//!   refused commit writes nothing.
//! - **perform** durably claims the outbox head before executing it through
//!   the driver's responder (the simulated outside world), then records its
//!   result before returning it. A recorded result is redelivered without
//!   another execution; a claim left without a result is surfaced for
//!   external reconciliation. The result remains pending until a commit
//!   consumes it.
//! - **observe** and **clock** peek the inbox and the timer table; nothing is
//!   removed until a commit consumes it.
//!
//! Time is the driver's logical clock ([`SqliteCell::advance_to`]); external
//! observations arrive through [`SqliteCell::inject`]. The default engine is
//! the native SQLite test driver; portable hosts supply their own engine.

use std::collections::BTreeMap;
use std::marker::PhantomData;
#[cfg(feature = "native")]
use std::path::Path;

use norn_ir::RuntimeValue as Value;
use norn_kernel::cell_host::{JsonObject, ObjectTypeName};
use norn_kernel::{
    BoundedFact, CellCommit, CellHost, Command, CommandId, CommitError, CommitModel, CommitReceipt,
    CommitVersions, CommitView, Consumed, DueTimer, Effect, FactKey, FactMutation, FactState,
    FactVersion, Generation, LedgerSeq, LinkKey, LinkMutation, LinkReadTarget, LinkState,
    LogicalTime, ObservationAuthorization, ObservationEvent, ObservationId, Observed,
    OwnedRowsCommitProof, Performed, TimerId, TypeCatalog, WrittenLinkVersion, WrittenVersion,
    check_commit, check_commit_identifiers, check_commit_observed, check_commit_owned_rows,
};
use serde::Serialize;
use serde::de::DeserializeOwned;

#[cfg(feature = "native")]
use crate::engine::NativeEngine;
use crate::engine::{DefaultEngine, SqlEngine};
use crate::error::CellError;
use crate::model::Model;
use crate::reactive::{BudgetedRead, DecodeBudget, ReactiveLimits};
use crate::read::{PendingTimer, StoreReader};
#[cfg(feature = "native")]
use crate::store::StoreOptions;
use crate::store::{
    CellStore, CommitBatch, CommitOutcome, Consumption, EffectRow, FactWriteRow, GuardedError,
    InstalledDefinitionInput, InstalledDefinitionSet, LinkChange, MAIN_WORLD,
    MarkingDeclassification, OutboxCommand, TimerEntry,
};

/// The simulated outside world: maps a performed command to the kernel input
/// its result becomes. `Send`, so a driver can be shared behind a mutex by a
/// server host.
pub type Responder<C, I> = Box<dyn FnMut(&Command<C>) -> I + Send>;

/// Durable state of the outbox head before an external `perform` dispatch.
#[derive(Debug, Clone, PartialEq)]
pub enum PerformClaim<C, I> {
    /// This generation was durably claimed by this call and may be dispatched.
    Fresh(Command<C>),
    /// A result was already recorded and must be redelivered without dispatch.
    Recorded {
        /// The command whose result was recorded.
        command: Command<C>,
        /// The recorded result delivery.
        performed: Performed<I>,
    },
    /// A prior process durably claimed this generation, but no result was
    /// recorded before it stopped. The external outcome must be reconciled.
    OutcomeUnknown(Command<C>),
}

/// A `CellHost` over a [`CellStore`], using its selected SQL engine.
pub struct SqliteCell<E, C, I, S: SqlEngine = DefaultEngine> {
    store: CellStore<S>,
    scope: String,
    now: LogicalTime,
    responder: Responder<C, I>,
    published: Vec<CommitReceipt>,
    events: PhantomData<fn() -> E>,
}

impl<E, C, I, S> std::fmt::Debug for SqliteCell<E, C, I, S>
where
    S: SqlEngine + std::fmt::Debug,
{
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SqliteCell")
            .field("store", &self.store)
            .field("scope", &self.scope)
            .field("now", &self.now)
            .field("published", &self.published.len())
            .finish_non_exhaustive()
    }
}

impl<E, C, I, S: SqlEngine> SqliteCell<E, C, I, S> {
    /// A driver over `store`, recording created objects under tenant
    /// `scope`, at the epoch, whose outside world is `responder`. The store
    /// is bound to `scope` ([`CellStore::bind`]), so a file is never served
    /// under another tenant scope.
    ///
    /// # Errors
    ///
    /// [`CellError::BindingMismatch`] for a file of another scope.
    pub fn new(
        mut store: CellStore<S>,
        scope: impl Into<String>,
        responder: impl FnMut(&Command<C>) -> I + Send + 'static,
    ) -> Result<Self, CellError> {
        let scope = scope.into();
        store.bind("scope", &scope)?;
        let now = LogicalTime::from_millis(store.read().clock_floor()?);
        Ok(Self {
            store,
            scope,
            now,
            responder: Box::new(responder),
            published: Vec::new(),
            events: PhantomData,
        })
    }

    /// The underlying store.
    #[must_use]
    pub const fn store(&self) -> &CellStore<S> {
        &self.store
    }

    /// Atomically append host-owned events under a durable idempotency key.
    ///
    /// This boundary intentionally accepts events only: it cannot write
    /// business facts, consume deliveries, or enqueue effects. It is for
    /// projections that publish deterministic host findings to the ledger.
    ///
    /// # Errors
    ///
    /// Serialization or store errors.
    pub fn commit_host_events_idempotently(
        &mut self,
        idempotency_key: &str,
        events: Vec<E>,
    ) -> Result<(), CellError>
    where
        E: Serialize,
    {
        if self
            .store
            .read()
            .receipt_by_idempotency_key(MAIN_WORLD, idempotency_key)?
            .is_some()
        {
            return Ok(());
        }
        let events = events
            .into_iter()
            .map(|event| to_runtime(&event))
            .collect::<Result<Vec<_>, _>>()?;
        self.store.commit(&CommitBatch {
            world: MAIN_WORLD.to_owned(),
            scope: self.scope.clone(),
            logical_time: self.now.as_millis(),
            idempotency_key: Some(idempotency_key.to_owned()),
            events,
            ..CommitBatch::default()
        })?;
        Ok(())
    }

    /// Atomically remove selected current markings from one exact fact
    /// version and append its authorization receipt to the ledger.
    ///
    /// The caller must authorize the trusted actor while holding exclusive
    /// access to this cell. The store rechecks the object version, current
    /// markings, event payload, and idempotency hash inside the same SQL
    /// transaction before applying the visibility change.
    ///
    /// # Errors
    ///
    /// A version conflict, invalid declassification, serialization, or
    /// storage failure.
    pub fn declassify_markings(
        &mut self,
        idempotency_key: &str,
        change: MarkingDeclassification,
    ) -> Result<CommitOutcome, CellError> {
        // Keep retries identical after the host clock advances. The store
        // still compares the complete request hash, so reusing a key with a
        // different change remains a conflict.
        let logical_time = self
            .store
            .read()
            .receipt_by_idempotency_key(MAIN_WORLD, idempotency_key)?
            .map_or(self.now.as_millis(), |receipt| receipt.body.logical_time);
        self.store.commit(&CommitBatch {
            world: MAIN_WORLD.to_owned(),
            scope: self.scope.clone(),
            logical_time,
            idempotency_key: Some(idempotency_key.to_owned()),
            events: vec![change.ledger_event()?],
            marking_declassifications: vec![change],
            ..CommitBatch::default()
        })
    }

    /// Commit one complete quantity operation using this host's injected clock
    /// and bound scope. Exact retries retain the original trusted timestamp.
    ///
    /// # Errors
    /// Typed quantity refusals, changed-input reuse, or storage errors.
    pub fn commit_quantity(
        &mut self,
        request: &norn_ir::quantity_ledger::QuantityRequest,
        idempotency_key: &str,
    ) -> Result<crate::store::CommitOutcome, CellError> {
        if idempotency_key.is_empty() || idempotency_key.len() > 128 {
            return Err(norn_kernel::quantity::QuantityError::InvalidContract.into());
        }
        let previous = self
            .store
            .read()
            .receipt_by_idempotency_key(MAIN_WORLD, idempotency_key)?;
        let logical_time =
            previous.map_or(self.now.as_millis(), |receipt| receipt.body.logical_time);
        self.store.commit(&CommitBatch {
            world: MAIN_WORLD.to_owned(),
            scope: self.scope.clone(),
            logical_time,
            idempotency_key: Some(idempotency_key.to_owned()),
            quantity: Some(request.clone()),
            ..CommitBatch::default()
        })
    }

    /// Submit one due quantity Expire through the ordinary atomic commit path.
    /// The host runtime calls this after injecting its Clock; the kernel never
    /// creates timers. Timer consumption and hold release share the receipt.
    ///
    /// # Errors
    /// Corrupt timer/reservation correspondence or normal commit refusals.
    pub fn expire_quantity_due(&mut self) -> Result<bool, CellError> {
        #[derive(serde::Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Deadline {
            world: String,
            ledger: String,
            reservation: String,
            due: u64,
        }
        let Some(pending) = self.store.read().due_quantity_timer(self.now.as_millis())? else {
            return Ok(false);
        };
        let deadline: Deadline = from_runtime(&pending.timer.timer)?;
        let id = crate::quantity::expiry_timer_id(
            &deadline.world,
            &deadline.ledger,
            &deadline.reservation,
        )?;
        let reservation = self
            .store
            .read()
            .quantity_reservation(&deadline.world, &deadline.ledger, &deadline.reservation)?
            .ok_or(norn_kernel::quantity::QuantityError::Corrupt)?;
        if id != pending.timer.timer_id
            || deadline.due != pending.timer.due
            || reservation.expires_at != deadline.due
            || reservation.status != "open"
        {
            return Err(norn_kernel::quantity::QuantityError::Corrupt.into());
        }
        let mut request = norn_ir::quantity_ledger::QuantityRequest {
            version: norn_ir::quantity_ledger::QUANTITY_CELL_VERSION.into(),
            ledger_id: deadline.ledger,
            balance_versions: Vec::new(),
            operation: norn_ir::quantity_ledger::QuantityOperation::Expire {
                reservation_id: deadline.reservation,
            },
        };
        request.balance_versions = self
            .store
            .read()
            .quantity_balance_versions(&deadline.world, &request)?;
        self.store.commit(&CommitBatch {
            world: deadline.world,
            scope: self.scope.clone(),
            logical_time: self.now.as_millis(),
            idempotency_key: Some(format!("{}:{}", id, pending.commit_seq)),
            consumes: Some(crate::Consumption::Timer {
                timer_id: id,
                incarnation: pending.commit_seq,
            }),
            quantity: Some(request),
            ..CommitBatch::default()
        })?;
        Ok(true)
    }

    /// The driver's current logical time.
    #[must_use]
    pub const fn now(&self) -> LogicalTime {
        self.now
    }

    /// Moves the clock forward to `now` (never backwards).
    pub fn advance_to(&mut self, now: LogicalTime) {
        self.now = self.now.max(now);
    }

    /// How many times the outside world executed a command.
    ///
    /// # Errors
    ///
    /// Engine errors.
    pub fn executions(&self) -> Result<u64, CellError> {
        self.store.read().executions()
    }

    /// Every receipt passed to [`CellHost::publish`], in order.
    #[must_use]
    pub fn published(&self) -> &[CommitReceipt] {
        &self.published
    }

    /// Install a checker-admitted definition set into this cell. The store
    /// commits the projected model, retained set, active version, and ledger
    /// event atomically before the in-memory model is replaced.
    ///
    /// # Errors
    ///
    /// A version conflict, unsafe model migration, or store error.
    pub fn install_definitions(
        &mut self,
        candidate_model: Model,
        input: InstalledDefinitionInput,
        expected_version: Option<u64>,
    ) -> Result<InstalledDefinitionSet, CellError> {
        let installed = self.store.install_definitions(
            candidate_model,
            input,
            expected_version,
            &self.scope,
        )?;
        self.advance_to(LogicalTime::from_millis(installed.installed_at));
        Ok(installed)
    }

    /// Check the same version-bound definition candidate against this cell's
    /// live values and return the installation preview without modifying it.
    ///
    /// # Errors
    /// A version conflict, unsafe model migration, or store error.
    pub fn check_definitions(
        &self,
        candidate_model: &Model,
        input: &InstalledDefinitionInput,
        expected_version: Option<u64>,
    ) -> Result<InstalledDefinitionSet, CellError> {
        self.store
            .check_definitions(candidate_model, input, expected_version)
    }
}

#[cfg(feature = "native")]
impl<E, C, I> SqliteCell<E, C, I, NativeEngine> {
    /// A driver over the store file at `path` (see [`CellStore::open`]).
    ///
    /// # Errors
    ///
    /// As [`CellStore::open`].
    pub fn open(
        path: &Path,
        model: Model,
        options: StoreOptions,
        scope: impl Into<String>,
        responder: impl FnMut(&Command<C>) -> I + Send + 'static,
    ) -> Result<Self, CellError> {
        Self::new(CellStore::open(path, model, options)?, scope, responder)
    }

    /// A driver over an in-memory store.
    ///
    /// # Errors
    ///
    /// As [`CellStore::open_in_memory`].
    pub fn open_in_memory(
        model: Model,
        scope: impl Into<String>,
        responder: impl FnMut(&Command<C>) -> I + Send + 'static,
    ) -> Result<Self, CellError> {
        Self::new(CellStore::open_in_memory(model)?, scope, responder)
    }
}

impl<E, C, I: Serialize, S: SqlEngine> SqliteCell<E, C, I, S> {
    /// Ingests one external observation; an id ingested before (pending or
    /// consumed) is ignored. Returns whether it was accepted.
    ///
    /// # Errors
    ///
    /// Serialization or engine errors.
    pub fn inject(&mut self, observed: &Observed<I>) -> Result<bool, CellError> {
        self.store.inject(&observed.id.0, &to_runtime(observed)?)
    }
}

/// The store's consumption reference for a kernel one.
fn consumption(consumed: &Consumed) -> Consumption {
    match consumed {
        Consumed::Timer { id, incarnation } => Consumption::Timer {
            timer_id: id.0.clone(),
            incarnation: incarnation.0,
        },
        Consumed::Result {
            command,
            generation,
        } => Consumption::Result {
            command_id: command.0.clone(),
            generation: generation.0,
        },
        Consumed::Observation { id } => Consumption::Observation {
            observation_id: id.0.clone(),
        },
    }
}

/// The kernel's consumption reference for a store one.
fn consumed(consumption: &Consumption) -> Consumed {
    match consumption {
        Consumption::Timer {
            timer_id,
            incarnation,
        } => Consumed::Timer {
            id: TimerId(timer_id.clone()),
            incarnation: LedgerSeq(*incarnation),
        },
        Consumption::Result {
            command_id,
            generation,
        } => Consumed::Result {
            command: CommandId(command_id.clone()),
            generation: Generation(*generation),
        },
        Consumption::Observation { observation_id } => Consumed::Observation {
            id: ObservationId(observation_id.clone()),
        },
    }
}

/// One committed ledger entry as the kernel sees it: the commit's position,
/// the delivery it consumed, the objects it wrote, and its events.
#[derive(Debug, Clone, PartialEq)]
pub struct CommitRecord<E> {
    /// The commit's ledger position.
    pub seq: LedgerSeq,
    /// The delivery it consumed.
    pub consumed: Option<Consumed>,
    /// The objects it wrote and their new versions.
    pub written: Vec<WrittenVersion>,
    /// The links it wrote and their new versions.
    pub linked: Vec<WrittenLinkVersion>,
    /// Its events, in order.
    pub events: Vec<E>,
}

/// Typed data as an exact runtime value (#2770: through exact JSON text,
/// never `serde_json`'s binary numbers).
fn to_runtime<T: Serialize + ?Sized>(value: &T) -> Result<Value, CellError> {
    norn_ir::runtime::encode(value).map_err(CellError::Encoding)
}

/// Typed data decoded from an exact runtime value.
fn from_runtime<T: DeserializeOwned>(value: &Value) -> Result<T, CellError> {
    norn_ir::runtime::decode(value).map_err(CellError::Encoding)
}

/// The version of `key` as the kernel sees it.
fn version_of<S: SqlEngine>(
    reader: &StoreReader<'_, S>,
    key: &FactKey,
) -> Result<FactVersion, CellError> {
    reader
        .version(MAIN_WORLD, key.object_type.as_str(), &key.key)
        .map(FactVersion)
}

/// The committed state of link `key` as the kernel sees it.
fn link_state<S: SqlEngine>(
    reader: &StoreReader<'_, S>,
    key: &LinkKey,
) -> Result<LinkState, CellError> {
    let source_type = key.source.object_type.as_str();
    let version = reader.link_version(
        MAIN_WORLD,
        source_type,
        &key.link,
        &key.source.key,
        key.target.object_type.as_str(),
        &key.target.key,
    )?;
    let values = reader
        .link(
            MAIN_WORLD,
            source_type,
            &key.link,
            &key.source.key,
            &key.target.key,
        )?
        .map(|record| {
            record
                .values
                .into_iter()
                // A stored null is an absent property.
                .filter(|(_, value)| !value.is_null())
                .collect::<JsonObject>()
        });
    Ok(LinkState {
        version: FactVersion(version),
        values,
    })
}

/// What the kernel's admission check reads, inside the commit transaction.
struct GuardView<'r, 'c, S: SqlEngine>(&'r StoreReader<'c, S>);

impl<S: SqlEngine> CommitView for GuardView<'_, '_, S> {
    type Error = CellError;

    fn pending(&self, consumed: &Consumed) -> Result<bool, CellError> {
        self.0.is_pending(&consumption(consumed))
    }

    fn version(&self, key: &FactKey) -> Result<FactVersion, CellError> {
        version_of(self.0, key)
    }

    fn concrete(&self, key: &FactKey) -> Result<Option<ObjectTypeName>, CellError> {
        Ok(self
            .0
            .root_row(MAIN_WORLD, key.object_type.as_str(), &key.key)?
            .map(|(concrete, _, _)| ObjectTypeName(concrete)))
    }

    fn can_read_observed_source_owners(&self) -> bool {
        true
    }

    fn observed_source_owner(
        &self,
        marker: &FactKey,
    ) -> Result<Option<norn_kernel::snapshot_observe::ObservedSourceOwner>, CellError> {
        self.0
            .object(MAIN_WORLD, marker.object_type.as_str(), &marker.key)?
            .and_then(|record| record.values.get("owner").cloned())
            .filter(|owner| !owner.is_null())
            .map(|owner| norn_ir::runtime::decode(&owner).map_err(CellError::Encoding))
            .transpose()
    }

    fn link_version(&self, key: &LinkKey) -> Result<FactVersion, CellError> {
        // The key is canonical (hierarchy roots); the version is too.
        self.0
            .link_version(
                MAIN_WORLD,
                key.source.object_type.as_str(),
                &key.link,
                &key.source.key,
                key.target.object_type.as_str(),
                &key.target.key,
            )
            .map(FactVersion)
    }

    fn links_of(&self, key: &FactKey) -> Result<Vec<LinkKey>, CellError> {
        let mut links: Vec<LinkKey> = self
            .0
            .links_touching(MAIN_WORLD, key.object_type.as_str(), &key.key)?
            .into_iter()
            .map(|link| {
                LinkKey::new(
                    FactKey::new(link.source_type, link.source),
                    link.link,
                    FactKey::new(link.target_type, link.target),
                )
            })
            .collect();
        links.sort();
        Ok(links)
    }

    fn has_link_target(
        &self,
        source: &FactKey,
        link: &str,
        excluded: &std::collections::BTreeSet<FactKey>,
    ) -> Result<Option<bool>, CellError> {
        let Some((source_type, _, _)) =
            self.0
                .root_row(MAIN_WORLD, source.object_type.as_str(), &source.key)?
        else {
            return Ok(Some(false));
        };
        // At most N excluded targets can occupy the first N rows. Read one
        // extra witness, bounding records and key bytes before materialization.
        let limits = norn_ir::ResourceLimitProfile::norn_limits_v1().slice;
        let fetch = excluded.len().saturating_add(1).min(
            usize::try_from(limits.max_records)
                .unwrap_or(usize::MAX)
                .saturating_add(1),
        );
        let Some(targets) = self.0.link_targets_with_versions(
            MAIN_WORLD,
            &source_type,
            link,
            &source.key,
            fetch,
            limits.max_canonical_bytes,
        )?
        else {
            return Ok(None);
        };
        if targets
            .iter()
            .any(|(key, ty, _)| !excluded.contains(&FactKey::new(self.0.canonical_type(ty), key)))
        {
            return Ok(Some(true));
        }
        // A short page is a complete absence proof; a full page containing
        // only excluded targets cannot establish final presence.
        Ok((targets.len() < fetch).then_some(false))
    }

    fn link_targets(
        &self,
        source: &FactKey,
        link: &str,
    ) -> Result<Option<Vec<LinkReadTarget>>, CellError> {
        let Some((source_type, _, _)) =
            self.0
                .root_row(MAIN_WORLD, source.object_type.as_str(), &source.key)?
        else {
            // A missing source has an empty adjacency set. If it appears
            // before this commit, the same complete-set comparison refuses.
            return Ok(Some(Vec::new()));
        };
        let limits = norn_ir::ResourceLimitProfile::norn_limits_v1().slice;
        let targets = self.0.link_targets_with_versions(
            MAIN_WORLD,
            &source_type,
            link,
            &source.key,
            usize::try_from(limits.max_records)
                .unwrap_or(usize::MAX)
                .saturating_add(1),
            limits.max_canonical_bytes,
        )?;
        let Some(targets) = targets else {
            return Ok(None);
        };
        if targets.len() > usize::try_from(limits.max_records).unwrap_or(usize::MAX) {
            return Ok(None);
        }
        let mut reads = Vec::with_capacity(targets.len());
        for (target, target_type, version) in targets {
            let key = LinkKey::new(source.clone(), link, FactKey::new(target_type, target));
            reads.push(LinkReadTarget {
                version: FactVersion(version),
                key,
            });
        }
        reads.sort_by(|left, right| {
            let left_target_type = self.0.canonical_type(left.key.target.object_type.as_str());
            let right_target_type = self.0.canonical_type(right.key.target.object_type.as_str());
            (&left_target_type, &left.key.target.key)
                .cmp(&(&right_target_type, &right.key.target.key))
        });
        Ok(Some(reads))
    }
}

impl<E, C, I, S: SqlEngine> SqliteCell<E, C, I, S>
where
    E: Serialize,
    C: Serialize,
    I: Serialize,
{
    /// The store batch of one kernel commit, at the driver's time.
    #[allow(
        clippy::too_many_lines,
        reason = "one conversion retains kernel CAS separately from trusted historical Function lineage"
    )]
    fn batch(&self, commit: &CellCommit<E, C, I>) -> Result<CommitBatch, CellError> {
        let mut writes = Vec::with_capacity(commit.writes.len());
        for write in &commit.writes {
            let object_type = write.key.object_type.as_str().to_owned();
            let key = write.key.key.clone();
            writes.push(match &write.mutation {
                FactMutation::Put { values } => FactWriteRow::Put {
                    object_type,
                    key,
                    values: values.clone(),
                },
                FactMutation::Delete => FactWriteRow::Delete { object_type, key },
            });
        }
        let mut effects = Vec::with_capacity(commit.effects.len());
        for effect in &commit.effects {
            effects.push(match effect {
                Effect::Perform(command) => EffectRow::Enqueue {
                    command: OutboxCommand {
                        command_id: command.id.0.clone(),
                        generation: command.generation.0,
                        idempotency_key: command.idempotency_key.0.clone(),
                        deadline: command.deadline.map(LogicalTime::as_millis),
                        command: to_runtime(command)?,
                    },
                },
                Effect::Schedule(timer) => EffectRow::Schedule {
                    timer: TimerEntry {
                        timer_id: timer.id.0.clone(),
                        due: timer.due.as_millis(),
                        timer: to_runtime(timer)?,
                    },
                },
                Effect::CancelTimer(id) => EffectRow::CancelTimer {
                    timer_id: id.0.clone(),
                },
            });
        }
        let links: Vec<LinkChange> = commit
            .links
            .iter()
            .map(|write| {
                let object_type = write.key.source.object_type.as_str().to_owned();
                let link = write.key.link.clone();
                let source = write.key.source.key.clone();
                let target = write.key.target.key.clone();
                let target_type = Some(write.key.target.object_type.as_str().to_owned());
                match &write.mutation {
                    LinkMutation::Add { values } => LinkChange::Add {
                        object_type,
                        link,
                        source,
                        target,
                        values: values.clone(),
                        target_type,
                    },
                    LinkMutation::Remove => LinkChange::Remove {
                        object_type,
                        link,
                        source,
                        target,
                        target_type,
                    },
                }
            })
            .collect();
        let marking_value_reads = commit
            .writes
            .iter()
            .filter(|write| write.expected.0 != 0)
            .map(|write| norn_kernel::FactExpectation {
                key: write.key.clone(),
                expected: write.expected,
            })
            .collect::<Vec<_>>();
        let marking_reads = [commit.reads.as_slice(), marking_value_reads.as_slice()].concat();
        let marking_events = self.rule_event_reads(&writes)?;
        // CAS-only expectations preserve destination values through the
        // writer's immutable snapshots. Only the kernel's declared reads are
        // decision inputs on ordinary turns; attributed rule/action turns also
        // retain reads folded into their write expectations by the kernel.
        let mut marking_reads =
            Self::occurrence_reads(commit, &marking_reads, !marking_events.is_empty());
        let mut marking_events = marking_events;
        let mut marking_historical_reads = Vec::new();
        // Function freshness and information lineage are separate contracts.
        // Resolve lineage from the cell-owned dispatch, never from a producer's
        // receipt. Dispatch snapshots survive destination-CAS/append-only
        // acceptance, refusals, and rescheduling just like ordinary read inputs.
        if let Some(Consumed::Result {
            command,
            generation,
        }) = &commit.consumes
            && let Some(pending) = self.store.read().head_command()?
            && pending.command.command_id == command.0
            && pending.command.generation == generation.0
            && pending
                .command
                .command
                .get("payload")
                .and_then(|payload| payload.get("type"))
                .and_then(Value::as_str)
                == Some("norn.function/v1")
        {
            self.dispatch_lineage(&pending, &mut marking_historical_reads, &mut marking_events)?;
        }
        for effect in &effects {
            if let EffectRow::Enqueue { command } = effect {
                Self::function_lineage(&command.command, &mut marking_reads, &mut marking_events);
            }
        }
        marking_historical_reads.sort_unstable_by(|left, right| {
            (&left.key, left.expected.0).cmp(&(&right.key, right.expected.0))
        });
        marking_historical_reads.dedup();
        marking_events.sort_unstable();
        marking_events.dedup();
        Ok(CommitBatch {
            world: MAIN_WORLD.to_owned(),
            scope: self.scope.clone(),
            logical_time: self.now.as_millis(),
            consumes: commit.consumes.as_ref().map(consumption),
            writes,
            marking_reads,
            marking_reference_reads: Vec::new(),
            marking_property_reads: Vec::new(),
            marking_historical_reads,
            marking_value_reads,
            marking_occurrence_reads: None,
            marking_events,
            marking_declassifications: Vec::new(),
            facts: Vec::new(),
            links,
            events: commit
                .events
                .iter()
                .map(to_runtime)
                .collect::<Result<_, _>>()?,
            effects,
            ..CommitBatch::default()
        })
    }

    fn dispatch_lineage(
        &self,
        pending: &crate::read::PendingCommand,
        reads: &mut Vec<norn_kernel::FactExpectation>,
        events: &mut Vec<u64>,
    ) -> Result<(), CellError> {
        // One exact journal lookup, never a scan or a recursive walk. A retry
        // enqueue shares the preceding result commit, so its retained batch
        // already carries the complete materialized lineage of earlier runs.
        let Some(record) = self.store.export_record(pending.commit_seq)? else {
            events.push(0);
            return Ok(());
        };
        let batch = record.batch;
        if batch.world != MAIN_WORLD || batch.scope != self.scope
            || !batch.effects.iter().any(|effect| {
                matches!(effect, EffectRow::Enqueue { command } if command == &pending.command)
            })
        {
            return Err(CellError::HistoricalValueUnproven(
                "Function dispatch does not match its trusted enqueue record".to_owned(),
            ));
        }
        // The command-state snapshot was written alongside enqueue, under
        // every dispatch input (current reads, immutable events and prior
        // generation state). Its immutable classification freezes exactly
        // the visibility admitted then: a pre-dispatch declassification is
        // respected, and a later one cannot declassify in-flight inputs.
        let state_key = FactKey::new("norn.FunctionCommand", pending.command.command_id.as_str());
        let state = batch.writes.iter().find_map(|write| match write {
            FactWriteRow::Put {
                object_type,
                key,
                values,
            } if object_type == state_key.object_type.as_str() && key == &state_key.key => {
                values.get("state").and_then(|value| {
                    norn_ir::runtime::decode::<norn_kernel::result_reconciliation::CommandState>(
                        value,
                    )
                    .ok()
                })
            }
            FactWriteRow::Put { .. } | FactWriteRow::Delete { .. } => None,
        });
        if state.is_none_or(|state| {
            state.generation.0 != pending.command.generation
                || state.cancelled
                || state.accepted.is_some()
        }) {
            return Err(CellError::HistoricalValueUnproven(
                "Function enqueue lacks its exact dispatch-state snapshot".to_owned(),
            ));
        }
        let version = record
            .receipt
            .body
            .written
            .iter()
            .find(|row| {
                row.object_type == state_key.object_type.as_str() && row.key == state_key.key
            })
            .map(|row| row.version)
            .ok_or_else(|| {
                CellError::HistoricalValueUnproven(
                    "Function dispatch state lacks its committed version".to_owned(),
                )
            })?;
        reads.push(norn_kernel::FactExpectation {
            key: state_key,
            expected: FactVersion(version),
        });
        events.extend(batch.marking_events);
        Ok(())
    }

    fn function_lineage(
        command: &Value,
        reads: &mut Vec<norn_kernel::FactExpectation>,
        events: &mut Vec<u64>,
    ) {
        let Some(payload) = command.get("payload") else {
            return;
        };
        if payload.get("type").and_then(Value::as_str) != Some("norn.function/v1") {
            return;
        }
        let invocation = payload.get("invocation").and_then(|value| {
            norn_ir::runtime::decode::<norn_ir::function::FunctionInvocation>(value).ok()
        });
        let Some(invocation) = invocation else {
            events.push(0); // Unproven dispatch attribution fails closed.
            return;
        };
        let mut pins = invocation.dependency.facts;
        for predicate in invocation.dependency.predicates {
            pins.extend(
                predicate
                    .members
                    .into_iter()
                    .map(|member| norn_ir::function::FactPin {
                        object_type: predicate.object_type.clone(),
                        key: member.key,
                        version: member.version,
                    }),
            );
        }
        // An epoch does not identify the historical members that contributed
        // values. Until that proof exists it cannot authorize public output.
        if !invocation.dependency.collections.is_empty() {
            events.push(0);
        }
        reads.extend(pins.into_iter().map(|pin| norn_kernel::FactExpectation {
            key: FactKey::new(pin.object_type, pin.key),
            expected: FactVersion(pin.version),
        }));
    }

    fn occurrence_reads(
        commit: &CellCommit<E, C, I>,
        marking_reads: &[norn_kernel::FactExpectation],
        attributed_rule: bool,
    ) -> Vec<norn_kernel::FactExpectation> {
        // Ordinary destination expectations retain property-value lineage;
        // they are not evidence of a classified decision predicate. Kernel
        // decisions may fold reads of written targets into their expectations,
        // so retain that full set for attributed rule/action turns.
        if attributed_rule
            || commit
                .writes
                .iter()
                .any(|write| write.key.object_type.as_str() == norn_kernel::ACTION_CALL_TYPE)
        {
            marking_reads.to_vec()
        } else {
            commit.reads.clone()
        }
    }

    fn rule_event_reads(&self, writes: &[FactWriteRow]) -> Result<Vec<u64>, CellError> {
        // Rule inputs include the immutable trigger event even when the
        // declared current-state slice is empty. Verify the recorded input
        // against its ledger source before admitting its receipt lineage.
        let mut marking_events = Vec::new();
        for write in writes {
            let FactWriteRow::Put {
                object_type,
                values,
                ..
            } = write
            else {
                continue;
            };
            if object_type != "norn.RuleRun"
                || values.get("outcome").and_then(Value::as_str) != Some("decided")
            {
                continue;
            }
            let seq = values
                .get("triggerSeq")
                .and_then(Value::as_i64)
                .and_then(|seq| u64::try_from(seq).ok())
                .filter(|seq| *seq > 0);
            let input = values
                .get("detail")
                .and_then(|detail| detail.get("inputs"))
                .and_then(|inputs| inputs.get("event"));
            let mut attributed = None;
            if let Some(seq) = seq {
                for entry in self.store.read().ledger_page(seq.saturating_sub(1), 1)? {
                    let event = entry.rule_input();
                    if entry.seq == seq && entry.world == MAIN_WORLD && input == Some(&event) {
                        attributed = Some(seq);
                    }
                }
            }
            marking_events.push(attributed.unwrap_or(0));
        }
        Ok(marking_events)
    }
}

fn due_timer<I: DeserializeOwned>(pending: &PendingTimer) -> Result<DueTimer<I>, CellError> {
    Ok(DueTimer {
        timer: from_runtime(&pending.timer.timer)?,
        incarnation: LedgerSeq(pending.commit_seq),
    })
}

impl<E, C, I, S: SqlEngine> SqliteCell<E, C, I, S>
where
    E: DeserializeOwned,
    C: DeserializeOwned,
    I: DeserializeOwned,
{
    /// The pending commands, oldest first.
    ///
    /// # Errors
    ///
    /// Engine or decode errors.
    pub fn pending_commands(&self) -> Result<Vec<Command<C>>, CellError> {
        self.store
            .read()
            .pending_commands()?
            .into_iter()
            .map(|pending| from_runtime(&pending.command.command))
            .collect()
    }

    /// The pending timers, ordered by id.
    ///
    /// # Errors
    ///
    /// Engine or decode errors.
    pub fn pending_timers(&self) -> Result<Vec<DueTimer<I>>, CellError> {
        let mut timers = self
            .store
            .read()
            .pending_timers()?
            .into_iter()
            .filter(|pending| {
                !pending
                    .timer
                    .timer_id
                    .starts_with(crate::quantity::EXPIRY_TIMER_PREFIX)
            })
            .map(|pending| due_timer(&pending))
            .collect::<Result<Vec<DueTimer<I>>, CellError>>()?;
        timers.sort_by(|a, b| a.timer.id.cmp(&b.timer.id));
        Ok(timers)
    }

    /// Every committed ledger entry, in order: receipts joined with their
    /// events.
    ///
    /// # Errors
    ///
    /// Engine or decode errors.
    pub fn commits(&self) -> Result<Vec<CommitRecord<E>>, CellError> {
        let reader = self.store.read();
        let mut events: BTreeMap<u64, Vec<E>> = BTreeMap::new();
        for entry in reader.ledger(0)? {
            events
                .entry(entry.commit_seq)
                .or_default()
                .push(from_runtime(&entry.event)?);
        }
        reader
            .receipts()?
            .into_iter()
            .map(|receipt| {
                let body = receipt.body;
                Ok(CommitRecord {
                    seq: LedgerSeq(body.commit_seq),
                    consumed: body.consumed.as_ref().map(consumed),
                    written: body
                        .written
                        .iter()
                        .map(|row| WrittenVersion {
                            key: FactKey::new(row.object_type.clone(), row.key.clone()),
                            version: FactVersion(row.version),
                        })
                        .collect(),
                    linked: body
                        .linked
                        .iter()
                        .map(|row| WrittenLinkVersion {
                            key: LinkKey::new(
                                FactKey::new(row.object_type.clone(), row.source.clone()),
                                row.link.clone(),
                                FactKey::new(row.target_type.clone(), row.target.clone()),
                            ),
                            version: FactVersion(row.version),
                        })
                        .collect(),
                    events: events.remove(&body.commit_seq).unwrap_or_default(),
                })
            })
            .collect()
    }
}

impl<E, C, I, S: SqlEngine> SqliteCell<E, C, I, S>
where
    C: DeserializeOwned,
    I: DeserializeOwned,
{
    /// Inspect and durably claim the outbox head before external dispatch.
    ///
    /// A restored perform fence takes precedence over both a saved result and
    /// a saved claim. A fresh command is returned only after the claim's
    /// zero-event commit and export-journal record are durable.
    ///
    /// # Errors
    ///
    /// SQLite or decode errors, or an outbox race that invalidates the claim
    /// before its commit.
    pub fn claim_perform(&mut self) -> Result<Option<PerformClaim<C, I>>, CellError> {
        self.claim_perform_with_reconciliation(None)
    }

    /// Claim the exact later generation authorized by the host's ledgered
    /// reconciliation policy. The restore fence itself remains intact; a new
    /// restore fences this generation again. Callers must atomically ledger
    /// the authorization and enqueue before invoking this method.
    ///
    /// # Errors
    /// Store or decode errors, as for [`Self::claim_perform`].
    pub fn claim_reconciled_perform(
        &mut self,
        command_id: &CommandId,
        generation: Generation,
    ) -> Result<Option<PerformClaim<C, I>>, CellError> {
        self.claim_perform_with_reconciliation(Some((command_id, generation)))
    }

    fn claim_perform_with_reconciliation(
        &mut self,
        authorized: Option<(&CommandId, Generation)>,
    ) -> Result<Option<PerformClaim<C, I>>, CellError> {
        let Some(head) = self.store.read().head_command()? else {
            return Ok(None);
        };
        let fence = self
            .store
            .perform_fence_generation(&head.command.command_id)?;
        let admitted = authorized.is_some_and(|(id, generation)| {
            id.0 == head.command.command_id
                && generation.0 == head.command.generation
                && fence.is_some_and(|fenced| generation.0 > fenced)
        });
        if fence.is_some() && !admitted {
            tracing::warn!(command_id = %head.command.command_id, "restored command fenced pending external-effect reconciliation");
            return Ok(None);
        }
        let command: Command<C> = from_runtime(&head.command.command)?;
        if let Some(result) = head.result {
            let performed = Performed {
                command: command.id.clone(),
                generation: command.generation,
                input: from_runtime(&result)?,
            };
            return Ok(Some(PerformClaim::Recorded { command, performed }));
        }
        if self
            .store
            .read()
            .perform_claimed(&head.command.command_id, head.command.generation)?
        {
            return Ok(Some(PerformClaim::OutcomeUnknown(command)));
        }
        self.store.commit(&CommitBatch {
            world: MAIN_WORLD.to_owned(),
            scope: self.scope.clone(),
            logical_time: self.now.as_millis(),
            effects: vec![EffectRow::ClaimPerform {
                command_id: head.command.command_id,
                generation: head.command.generation,
            }],
            ..CommitBatch::default()
        })?;
        Ok(Some(PerformClaim::Fresh(command)))
    }
}

impl<E, C, I, S: SqlEngine> SqliteCell<E, C, I, S>
where
    I: Serialize,
{
    /// Record the result of a previously claimed command generation.
    ///
    /// Returns `None` without writing when `command` is no longer the current
    /// unperformed head, so a late completion can never be attributed to a
    /// replacement generation.
    ///
    /// # Errors
    ///
    /// Serialization or SQLite errors.
    pub fn complete_perform(
        &mut self,
        command: &Command<C>,
        input: I,
    ) -> Result<Option<Performed<I>>, CellError> {
        if !self.store.record_performed(
            &command.id.0,
            command.generation.0,
            &to_runtime(&input)?,
        )? {
            return Ok(None);
        }
        Ok(Some(Performed {
            command: command.id.clone(),
            generation: command.generation,
            input,
        }))
    }
}

impl<E, C, I, S: SqlEngine> SqliteCell<E, C, I, S>
where
    E: Serialize,
    C: Serialize + DeserializeOwned,
    I: Serialize + DeserializeOwned,
{
    /// Admit ordinary fact/link writes and one quantity operation atomically.
    /// The ordinary kernel guards and quantity CAS/availability checks share
    /// the store transaction. The trusted caller supplies authorization.
    ///
    /// # Errors
    /// Shared commit admission, typed quantity refusals or changed-input reuse.
    pub fn commit_with_quantity(
        &mut self,
        commit: CellCommit<E, C, I>,
        request: &norn_ir::quantity_ledger::QuantityRequest,
        idempotency_key: &str,
    ) -> Result<CommitReceipt, CommitError<CellError>> {
        self.commit_with_authorization(
            commit,
            None,
            None,
            Some((request, idempotency_key)),
            &[],
            &[],
            &[],
        )
    }

    /// Commit trusted adapter outputs that were recomputed from the declared
    /// reads, including outputs equal to their prior serialized values.
    /// Ordinary preserved values retain the ordinary commit semantics.
    /// `derived` names properties of whole-object writes, never labels supplied
    /// by an executor; the store resolves their trusted input classifications.
    ///
    /// # Errors
    /// The ordinary commit refusals, or invalid derived-property attribution.
    pub fn commit_derived(
        &mut self,
        commit: CellCommit<E, C, I>,
        derived: &[(FactKey, Vec<String>)],
    ) -> Result<CommitReceipt, CommitError<CellError>> {
        self.commit_with_authorization(commit, None, None, None, derived, &[], &[])
    }

    #[allow(
        clippy::too_many_lines,
        clippy::too_many_arguments,
        reason = "shared admission and optional quantity data join one guarded transaction"
    )]
    fn commit_with_authorization(
        &mut self,
        mut commit: CellCommit<E, C, I>,
        authorization: Option<&ObservationAuthorization>,
        owned_rows: Option<OwnedRowsCommitProof>,
        quantity: Option<(&norn_ir::quantity_ledger::QuantityRequest, &str)>,
        derived: &[(FactKey, Vec<String>)],
        references: &[norn_kernel::FactExpectation],
        property_reads: &[(norn_kernel::FactExpectation, Vec<String>)],
    ) -> Result<CommitReceipt, CommitError<CellError>> {
        check_commit_identifiers(&commit)?;
        // Include the complete kernel input, including read/write expectations,
        // before defaults. The host's injected logical time is not retry input.
        let submitted = quantity
            .map(|(request, key)| to_runtime(&(MAIN_WORLD, &self.scope, key, request, &commit)))
            .transpose()
            .map_err(CommitError::Host)?;
        // Exact submitted-intent retries reach the store before current-model
        // materialization; the store rechecks identity inside its transaction.
        let prior = if let Some((_, key)) = quantity {
            self.store
                .read()
                .receipt_by_idempotency_key(MAIN_WORLD, key)
                .map_err(CommitError::Host)?
        } else {
            None
        };
        if prior.is_none() {
            let store_model = self.store.model();
            let catalog = TypeCatalog::new(store_model.object_types())
                .with_interfaces(store_model.interfaces())
                .with_property_types(store_model.property_types());
            let model = CommitModel::new(&catalog);
            let reader = self.store.read();
            let view = GuardView(&reader);
            for write in &mut commit.writes {
                if let FactMutation::Put { values } = &mut write.mutation {
                    *values = catalog
                        .effective_object_values(&write.key.object_type, values)
                        .map_err(|error| CommitError::EffectiveValues {
                            key: write.key.clone(),
                            reason: error.to_string(),
                        })?;
                }
            }
            let link_values = commit
                .links
                .iter()
                .map(|write| match &write.mutation {
                    LinkMutation::Add { values } => model
                        .effective_link_values(&commit, &view, &write.key, values)
                        .map(Some),
                    LinkMutation::Remove => Ok(None),
                })
                .collect::<Result<Vec<_>, _>>()?;
            for (write, values) in commit.links.iter_mut().zip(link_values) {
                if let (LinkMutation::Add { values: stored }, Some(values)) =
                    (&mut write.mutation, values)
                {
                    *stored = values;
                }
            }
        }
        let mut batch = self.batch(&commit).map_err(CommitError::Host)?;
        // Only the non-serializable, kernel-bound proof reaches this path.
        // Persist additive occurrence inputs so export/restore replays the
        // same classification without trusting a serialized subtraction.
        batch
            .marking_reads
            .retain(|read| !references.contains(read));
        batch.marking_reference_reads = references.to_vec();
        batch.marking_property_reads = property_reads.to_vec();
        // A proven input-only patch preserves each old property's own labels
        // through immutable snapshots. Its CAS does not read every old value
        // to derive the changed fields; retain only actual expression operands.
        batch
            .marking_value_reads
            .retain(|read| !property_reads.iter().any(|(source, _)| source == read));
        batch.marking_derived_properties = derived.to_vec();
        if let Some((request, key)) = quantity {
            batch.quantity = Some(request.clone());
            batch.idempotency_key = Some(key.to_owned());
            if let Some(receipt) = self
                .store
                .read()
                .receipt_by_idempotency_key(MAIN_WORLD, key)
                .map_err(CommitError::Host)?
            {
                batch.logical_time = receipt.body.logical_time;
            }
        }
        let guard = |reader: &StoreReader<'_, S>| {
            let model = reader.model();
            let catalog = TypeCatalog::new(model.object_types())
                .with_interfaces(model.interfaces())
                .with_property_types(model.property_types());
            let model = CommitModel::new(&catalog);
            match (authorization, owned_rows) {
                (Some(authorization), None) => {
                    check_commit_observed(&commit, &model, &GuardView(reader), authorization)
                }
                (None, Some(proof)) => {
                    check_commit_owned_rows(&commit, &model, &GuardView(reader), proof)
                }
                (None, None) => check_commit(&commit, &model, &GuardView(reader)),
                (Some(_), Some(_)) => Err(CommitError::ObservationAuthorization),
            }
        };
        match self.store.commit_guarded_submitted(
            &batch,
            submitted.as_ref(),
            guard,
            &mut |_point| Ok(()),
        ) {
            Ok((CommitVersions { written, linked }, outcome)) => Ok(CommitReceipt {
                seq: LedgerSeq(outcome.receipt().body.commit_seq),
                written: if matches!(outcome, crate::store::CommitOutcome::Replayed(_)) {
                    outcome
                        .receipt()
                        .body
                        .written
                        .iter()
                        .map(|row| WrittenVersion {
                            key: FactKey::new(row.object_type.clone(), row.key.clone()),
                            version: FactVersion(row.version),
                        })
                        .collect()
                } else {
                    written
                },
                linked: if matches!(outcome, crate::store::CommitOutcome::Replayed(_)) {
                    outcome
                        .receipt()
                        .body
                        .linked
                        .iter()
                        .map(|row| WrittenLinkVersion {
                            key: LinkKey::new(
                                FactKey::new(row.object_type.clone(), row.source.clone()),
                                row.link.clone(),
                                FactKey::new(row.target_type.clone(), row.target.clone()),
                            ),
                            version: FactVersion(row.version),
                        })
                        .collect()
                } else {
                    linked
                },
            }),
            Err(GuardedError::Guard(refusal)) => Err(refusal),
            Err(GuardedError::Store(error)) => Err(CommitError::Host(error)),
        }
    }
}

impl<E, C, I, S: SqlEngine> CellHost for SqliteCell<E, C, I, S>
where
    E: Serialize,
    C: Serialize + DeserializeOwned,
    I: Serialize + DeserializeOwned,
{
    type Event = E;
    type Command = C;
    type Input = I;
    type Error = CellError;

    fn load(&self, key: &FactKey) -> Result<FactState, CellError> {
        let reader = self.store.read();
        let version = version_of(&reader, key)?;
        let values = reader
            .object(MAIN_WORLD, key.object_type.as_str(), &key.key)?
            .map(|record| record.values);
        Ok(FactState { version, values })
    }

    fn supports_reference_reads(&self) -> bool {
        true
    }

    fn load_reference(
        &self,
        key: &FactKey,
        max_bytes: u64,
    ) -> Result<Option<BoundedFact>, CellError> {
        let reader = self.store.read();
        let mut limits =
            ReactiveLimits::from_profile(&norn_ir::ResourceLimitProfile::norn_limits_v1());
        limits.max_candidate_bytes_per_commit = max_bytes;
        limits.max_scan = 1;
        let mut budget = DecodeBudget::new(&limits);
        let version = version_of(&reader, key)?;
        let identity = reader.object_identity_with_budget(
            MAIN_WORLD,
            key.object_type.as_str(),
            &key.key,
            &mut budget,
        )?;
        let (concrete_type, values) = match identity {
            crate::reactive::BudgetedIdentityRead::Found { object_type, .. } => (
                Some(norn_ir::ObjectTypeName(object_type)),
                Some(BTreeMap::new()),
            ),
            crate::reactive::BudgetedIdentityRead::Missing => (None, None),
            crate::reactive::BudgetedIdentityRead::Exhausted => return Ok(None),
        };
        Ok(Some(BoundedFact {
            concrete_type,
            state: FactState { version, values },
        }))
    }

    fn load_bounded(
        &self,
        key: &FactKey,
        max_bytes: u64,
    ) -> Result<Option<BoundedFact>, CellError> {
        let reader = self.store.read();
        let profile = norn_ir::ResourceLimitProfile::norn_limits_v1();
        let mut limits = ReactiveLimits::from_profile(&profile);
        limits.max_candidate_bytes_per_commit = max_bytes;
        limits.max_scan = 1;
        let mut budget = DecodeBudget::new(&limits);
        let version = version_of(&reader, key)?;
        let state = match reader.object_with_budget(
            MAIN_WORLD,
            key.object_type.as_str(),
            &key.key,
            &mut budget,
        )? {
            BudgetedRead::Found(record) => BoundedFact {
                concrete_type: Some(norn_ir::ObjectTypeName(record.object_type)),
                state: FactState {
                    version,
                    values: Some(record.values),
                },
            },
            BudgetedRead::Missing => BoundedFact {
                concrete_type: None,
                state: FactState {
                    version,
                    values: None,
                },
            },
            BudgetedRead::Exhausted => return Ok(None),
        };
        Ok(Some(state))
    }

    fn load_link(&self, key: &LinkKey) -> Result<LinkState, CellError> {
        link_state(&self.store.read(), key)
    }

    fn read_link_targets(
        &self,
        source: &FactKey,
        link: &str,
        max_targets: usize,
        max_key_bytes: u64,
    ) -> Result<Option<Vec<LinkReadTarget>>, CellError> {
        let reader = self.store.read();
        let targets = reader.link_targets_with_versions(
            MAIN_WORLD,
            source.object_type.as_str(),
            link,
            &source.key,
            max_targets.saturating_add(1),
            max_key_bytes,
        )?;
        let Some(targets) = targets else {
            return Ok(None);
        };
        if targets.len() > max_targets {
            return Ok(None);
        }
        let mut reads = Vec::with_capacity(targets.len());
        for (target, target_type, version) in targets {
            let key = LinkKey::new(source.clone(), link, FactKey::new(target_type, target));
            reads.push(LinkReadTarget {
                version: FactVersion(version),
                key,
            });
        }
        reads.sort_by(|left, right| {
            let left_target_type = reader.canonical_type(left.key.target.object_type.as_str());
            let right_target_type = reader.canonical_type(right.key.target.object_type.as_str());
            (&left_target_type, &left.key.target.key)
                .cmp(&(&right_target_type, &right.key.target.key))
        });
        Ok(Some(reads))
    }

    fn commit(
        &mut self,
        commit: CellCommit<E, C, I>,
    ) -> Result<CommitReceipt, CommitError<CellError>> {
        self.commit_with_authorization(commit, None, None, None, &[], &[], &[])
    }

    fn commit_action_references(
        &mut self,
        commit: CellCommit<E, C, I>,
        proof: norn_kernel::action_commit::ActionReferenceReads,
        quantity: Option<(&norn_ir::quantity_ledger::QuantityRequest, &str)>,
    ) -> Result<CommitReceipt, CommitError<CellError>> {
        if !proof.validates(&commit) {
            return Err(CommitError::ObservationAuthorization);
        }
        self.commit_with_authorization(
            commit,
            None,
            None,
            quantity,
            &[],
            proof.reference_reads(),
            proof.property_reads(),
        )
    }

    fn commit_action_quantity(
        &mut self,
        commit: CellCommit<E, C, I>,
        request: &norn_ir::quantity_ledger::QuantityRequest,
        idempotency_key: &str,
    ) -> Result<CommitReceipt, CommitError<CellError>> {
        self.commit_with_quantity(commit, request, idempotency_key)
    }

    fn commit_owned_rows(
        &mut self,
        commit: CellCommit<E, C, I>,
        proof: OwnedRowsCommitProof,
    ) -> Result<CommitReceipt, CommitError<CellError>> {
        self.commit_with_authorization(commit, None, Some(proof), None, &[], &[], &[])
    }

    fn commit_observed(
        &mut self,
        mut commit: CellCommit<E, C, I>,
        authorization: ObservationAuthorization,
    ) -> Result<CommitReceipt, CommitError<CellError>>
    where
        E: From<ObservationEvent>,
    {
        if !authorization.validates(&commit) {
            return Err(CommitError::ObservationAuthorization);
        }
        let references = authorization.reference_reads(&commit).to_vec();
        authorization.append_findings(&mut commit);
        self.commit_with_authorization(
            commit,
            Some(&authorization),
            None,
            None,
            &[],
            &references,
            &[],
        )
    }

    fn perform(&mut self) -> Result<Option<Performed<I>>, CellError> {
        match self.claim_perform()? {
            None => Ok(None),
            Some(PerformClaim::Recorded { performed, .. }) => Ok(Some(performed)),
            Some(PerformClaim::Fresh(command)) => {
                let input = (self.responder)(&command);
                self.complete_perform(&command, input)
            }
            Some(PerformClaim::OutcomeUnknown(command)) => Err(CellError::PerformOutcomeUnknown {
                command_id: command.id.0,
                generation: command.generation.0,
            }),
        }
    }

    fn observe(&mut self) -> Result<Option<Observed<I>>, CellError> {
        self.store
            .read()
            .pending_observations()?
            .into_iter()
            .next()
            .map(|pending| from_runtime(&pending.observed))
            .transpose()
    }

    fn clock(&mut self) -> Result<Option<DueTimer<I>>, CellError> {
        self.store
            .read()
            .due_timer(self.now.as_millis())?
            .map(|pending| due_timer(&pending))
            .transpose()
    }

    fn publish(&mut self, receipt: &CommitReceipt) -> Result<(), CellError> {
        self.published.push(receipt.clone());
        Ok(())
    }
}
