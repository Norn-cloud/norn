//! The in-memory reference [`CellHost`] and the host-agnostic `CellHost` laws
//! (T4-CELL-1).
//!
//! [`MemoryCell`] is the reference host: plain collections, no IO, the
//! outside world a deterministic responder function. It is test support and
//! makes no durability claim.
//!
//! [`CellHostMachine`] is a `proptest-state-machine` adapter that runs any
//! [`SimulatedCell`] against an independent reference model and checks, after
//! every generated operation:
//!
//! 1. **Atomicity.** A refused commit changes nothing observable: no fact, no
//!    ledger position, no enqueued command or timer, no consumption.
//! 2. **Commit = check, then apply all.** A commit is accepted exactly when
//!    the model admits it (shape and link declarations → delivery pending →
//!    compare-and-swap and concrete types → link endpoints → cardinality, the
//!    [`norn_kernel::check_commit`] order, by canonical identity), with the same
//!    refusal; an accepted commit's object and link writes, events, enqueued
//!    commands, timer changes, and consumption are all visible from the next
//!    operation on, and its receipt carries the next ledger position and the
//!    written object and link versions.
//! 3. **Exactly-once delivery.** `perform`, `observe`, and `clock` redeliver
//!    the same pending item until a commit consumes it; a consumed item is
//!    never delivered again; `perform` executes a command at most once per
//!    generation (a re-issue at the same or an older generation, pending or
//!    completed, is a no-op); an observation id is ingested at most once.
//!    A host that declares [`SimulatedCell::COMMITS_PERFORM_CLAIMS`] also
//!    advances the total commit order with an empty claim commit before each
//!    fresh execution. Under an unreliable outside world
//!    ([`SimulatedCell::NETWORK`], law 7) the same holds for results that
//!    arrive late, out of order, twice, or never.
//! 4. **Clock.** `clock` delivers only due timers, earliest first (ties by
//!    id); rescheduling a delivered timer (even identically) makes its old
//!    delivery unconsumable, because the delivery names the incarnation.
//! 5. **Refinement.** After every operation every object's and every law
//!    link's committed state, the full ledger (positions, consumption, written versions, events), the
//!    pending commands and timers, the next due timer, the next
//!    observation, and the execution count equal the model's.
//! 6. **Crash and I/O atomicity.** For a host that lists
//!    [`SimulatedCell::FAULT_KINDS`], a generated [`CellOp::Faulted`] fails
//!    one write transaction of a commit or perform ([`FaultPlan`]).
//!    A commit is one write transaction: faulted, it fails with a host error
//!    and changes nothing observable (a refused commit may report its refusal
//!    instead). A fresh perform is two: a faulted claim changes nothing and
//!    executes nothing; a faulted result record keeps the claim but no
//!    result, so the command is never executed again for that generation
//!    (every later perform fails until a newer generation replaces it). A
//!    fault past an operation's last write transaction never fires. After a
//!    [`FaultKind::Crash`] the host reopens from durable state, and
//!    refinement (law 5) then holds against the unchanged model. A failed
//!    storage commit ([`FaultKind::CommitFails`]) is held to the same law: the
//!    operation fails with a host error and changes nothing.
//!
//! 7. **Responder faults.** For a host that declares
//!    [`SimulatedCell::NETWORK`], the outside world is an explicit network
//!    held by the model: [`CellOp::Dispatch`] claims the fresh outbox head
//!    and puts its result in flight; [`CellOp::Deliver`] hands a result to
//!    the host in order, out of order, or again (a duplicate); and
//!    [`CellOp::Lose`] drops one in flight. Other operations interleave, so
//!    results are delayed. A delivered result is recorded exactly when it
//!    is for the current outbox head at its generation and no result is
//!    recorded yet; recording is that generation's one execution. A
//!    stale-generation, duplicate or late result is refused and changes
//!    nothing. While a dispatched result is outstanding, `perform` fails
//!    (the outcome is unknown) rather than execute again. Observations are
//!    redelivered too (duplicate ids, late after consumption) and ingested
//!    at most once.
//!
//! Hosts that inject faults or declare a network also run in swarm mode: each case first draws
//! which operation kinds it uses and how often, and a failure report prints
//! that mix with the seed.
//!
//! A host driver (for example `norn-cell`'s SQLite driver) implements
//! [`SimulatedCell`] and calls [`check_cell_host`] to run the same laws.

use std::collections::{BTreeMap, BTreeSet};
use std::convert::Infallible;
use std::marker::PhantomData;

use norn_kernel::cell_host::{
    InterfaceDef, JsonObject, JsonValue, Link, LinkCardinality, LinkId, Multiplicity,
    ObjectTypeDef, ObjectTypeName, PropertyTypeDef, ResourceStatus,
};
use norn_kernel::{
    BoundedFact, CellCommit, CellHost, Command, CommandId, CommitError, CommitModel, CommitReceipt,
    CommitVersions, CommitView, Consumed, DueTimer, Effect, FactExpectation, FactKey, FactMutation,
    FactState, FactVersion, FactWrite, FreshnessPolicy, Generation, IdempotencyKey, LedgerSeq,
    LinkKey, LinkMutation, LinkReadTarget, LinkState, LinkWrite, LogicalTime,
    ObservationAuthorization, ObservationEvent, ObservationId, Observed, OwnedRowsCommitProof,
    Performed, PredicateObject, PredicateQuery, PredicateReadMember, Provenance, Timer, TimerId,
    TypeCatalog, WrittenLinkVersion, WrittenVersion, check_commit, check_commit_observed,
    check_commit_owned_rows,
};
use proptest::prelude::*;
use proptest::strategy::Union;
use proptest::test_runner::{TestError, TestRunner};
use proptest_state_machine::{ReferenceStateMachine, StateMachineTest};

use crate::coverage::{self, Coverage};
use crate::runner::{Failure, Settings};

// ------------------------------------------------------------ reference host

/// One committed ledger entry of a [`MemoryCell`].
#[derive(Debug, Clone, PartialEq)]
pub struct LedgerEntry<E> {
    /// The entry's position.
    pub seq: LedgerSeq,
    /// The delivery the commit consumed.
    pub consumed: Option<Consumed>,
    /// The objects it wrote and their new versions.
    pub written: Vec<WrittenVersion>,
    /// The links it wrote and their new versions.
    pub linked: Vec<WrittenLinkVersion>,
    /// Its events, in order.
    pub events: Vec<E>,
}

/// The outside world of a [`MemoryCell`]: maps a performed command to the
/// kernel input its result becomes.
pub type Responder<C, I> = Box<dyn FnMut(&Command<C>) -> I>;

/// The in-memory reference [`CellHost`].
///
/// Every verb is a plain collection operation; [`CellHost::commit`] runs
/// [`check_commit`] and then applies everything, so it is atomic by
/// construction. Time and external observations are injected by the test
/// ([`MemoryCell::advance_to`], [`MemoryCell::inject`]).
///
/// Objects and links are stored by canonical identity (#3018): one object
/// answers to every type of its hierarchy ([`MemoryCell::with_types`]), and
/// reads through a type the live object is not report its version only.
/// Without types the cell declares no links (fail closed).
pub struct MemoryCell<E, C, I> {
    now: LogicalTime,
    seq: LedgerSeq,
    types: Vec<ObjectTypeDef>,
    interfaces: Vec<InterfaceDef>,
    property_types: Vec<PropertyTypeDef>,
    law_artifacts: Vec<norn_kernel::cell_host::ArtifactEnvelope>,
    facts: BTreeMap<FactKey, FactState>,
    concrete: BTreeMap<FactKey, ObjectTypeName>,
    links: BTreeMap<LinkKey, LinkState>,
    ledger: Vec<LedgerEntry<E>>,
    outbox: Vec<Command<C>>,
    performed: Option<Performed<I>>,
    generations: BTreeMap<CommandId, Generation>,
    timers: BTreeMap<TimerId, DueTimer<I>>,
    inbox: Vec<Observed<I>>,
    seen: BTreeSet<ObservationId>,
    published: Vec<CommitReceipt>,
    executions: u64,
    responder: Responder<C, I>,
}

impl<E, C, I> MemoryCell<E, C, I> {
    /// An empty cell at the epoch whose outside world is `responder`.
    pub fn new(responder: impl FnMut(&Command<C>) -> I + 'static) -> Self {
        Self {
            now: LogicalTime::EPOCH,
            seq: LedgerSeq::default(),
            types: Vec::new(),
            interfaces: Vec::new(),
            property_types: Vec::new(),
            law_artifacts: Vec::new(),
            facts: BTreeMap::new(),
            concrete: BTreeMap::new(),
            links: BTreeMap::new(),
            ledger: Vec::new(),
            outbox: Vec::new(),
            performed: None,
            generations: BTreeMap::new(),
            timers: BTreeMap::new(),
            inbox: Vec::new(),
            seen: BTreeSet::new(),
            published: Vec::new(),
            executions: 0,
            responder: Box::new(responder),
        }
    }

    /// The same cell over the object types `types` (its model: hierarchy,
    /// link declarations).
    #[must_use]
    pub fn with_types(mut self, types: Vec<ObjectTypeDef>) -> Self {
        self.types = types;
        self
    }

    /// The same cell with the interface declarations implemented by its
    /// object types.
    #[must_use]
    pub fn with_interfaces(mut self, interfaces: Vec<InterfaceDef>) -> Self {
        self.interfaces = interfaces;
        self
    }

    /// The same cell with the shared property-type definitions referenced by
    /// its object types.
    #[must_use]
    pub fn with_property_types(mut self, property_types: Vec<PropertyTypeDef>) -> Self {
        self.property_types = property_types;
        self
    }

    /// The same reference host with the retained laws of its definition snapshot.
    #[must_use]
    pub fn with_governing_laws(
        mut self,
        artifacts: Vec<norn_kernel::cell_host::ArtifactEnvelope>,
    ) -> Self {
        self.law_artifacts = artifacts;
        self
    }

    /// The host's current time.
    #[must_use]
    pub const fn now(&self) -> LogicalTime {
        self.now
    }

    /// Moves the host clock forward to `now` (never backwards).
    pub fn advance_to(&mut self, now: LogicalTime) {
        self.now = self.now.max(now);
    }

    /// Ingests one external observation. An id already ingested (pending or
    /// consumed) is ignored; returns whether it was accepted.
    pub fn inject(&mut self, observed: Observed<I>) -> bool {
        if self.seen.insert(observed.id.clone()) {
            self.inbox.push(observed);
            true
        } else {
            false
        }
    }

    /// How many times the outside world executed a command.
    #[must_use]
    pub const fn executions(&self) -> u64 {
        self.executions
    }

    /// Every committed ledger entry, in order.
    #[must_use]
    pub fn ledger(&self) -> &[LedgerEntry<E>] {
        &self.ledger
    }

    /// The pending commands, oldest first.
    #[must_use]
    pub fn outbox(&self) -> &[Command<C>] {
        &self.outbox
    }

    /// The pending timers by id.
    #[must_use]
    pub const fn timers(&self) -> &BTreeMap<TimerId, DueTimer<I>> {
        &self.timers
    }

    /// Every receipt passed to [`CellHost::publish`], in order.
    #[must_use]
    pub fn published(&self) -> &[CommitReceipt] {
        &self.published
    }

    fn is_pending(&self, consumed: &Consumed) -> bool {
        match consumed {
            Consumed::Timer { id, incarnation } => self
                .timers
                .get(id)
                .is_some_and(|due| due.incarnation == *incarnation),
            Consumed::Result {
                command,
                generation,
            } => self.performed.as_ref().is_some_and(|performed| {
                performed.command == *command && performed.generation == *generation
            }),
            Consumed::Observation { id } => self.inbox.iter().any(|observed| observed.id == *id),
        }
    }

    fn consume(&mut self, consumed: &Consumed) {
        match consumed {
            Consumed::Timer { id, .. } => {
                self.timers.remove(id);
            }
            Consumed::Result { command, .. } => {
                self.performed = None;
                self.outbox.retain(|pending| pending.id != *command);
            }
            Consumed::Observation { id } => self.inbox.retain(|observed| observed.id != *id),
        }
    }

    fn enqueue(&mut self, effect: Effect<C, I>, seq: LedgerSeq) {
        match effect {
            Effect::Perform(command) => {
                // Same or older generation than any enqueued before: a
                // re-issue, never a re-execution.
                if self
                    .generations
                    .get(&command.id)
                    .is_some_and(|known| *known >= command.generation)
                {
                    return;
                }
                self.generations
                    .insert(command.id.clone(), command.generation);
                if self
                    .performed
                    .as_ref()
                    .is_some_and(|performed| performed.command == command.id)
                {
                    self.performed = None;
                }
                if let Some(slot) = self.outbox.iter_mut().find(|slot| slot.id == command.id) {
                    *slot = command;
                } else {
                    self.outbox.push(command);
                }
            }
            Effect::Schedule(timer) => {
                self.timers.insert(
                    timer.id.clone(),
                    DueTimer {
                        timer,
                        incarnation: seq,
                    },
                );
            }
            Effect::CancelTimer(id) => {
                self.timers.remove(&id);
            }
        }
    }
}

impl<E, C, I> MemoryCell<E, C, I> {
    /// The live objects of the queried object type (or a subtype) matching
    /// `predicate`, in key order, at most `limit`. Every object is in
    /// memory, so the enumeration is complete; an interface predicate is
    /// unsupported (`None`), as on every host.
    fn predicate_objects(
        &self,
        predicate: &PredicateQuery,
        limit: usize,
    ) -> Option<Vec<PredicateObject>> {
        let catalog = TypeCatalog::new(&self.types)
            .with_governing_laws(&self.law_artifacts)
            .with_interfaces(&self.interfaces)
            .with_property_types(&self.property_types);
        if !catalog.contains(&predicate.object_type) {
            return None;
        }
        let model = CommitModel::new(&catalog);
        let candidates: Vec<(FactKey, FactVersion, &JsonObject)> = self
            .facts
            .iter()
            .filter_map(|(key, state)| {
                let values = state.values.as_ref()?;
                let concrete = self.concrete.get(key)?;
                model.is_a(concrete, &predicate.object_type).then(|| {
                    (
                        FactKey {
                            object_type: concrete.clone(),
                            key: key.key.clone(),
                        },
                        state.version,
                        values,
                    )
                })
            })
            .collect();
        Some(norn_kernel::predicate_read::select_members(
            predicate,
            candidates
                .iter()
                .map(|(key, version, values)| (key, *version, *values)),
            limit,
        ))
    }
}

/// What a [`MemoryCell`] has committed, as [`check_commit`] reads it.
struct MemoryView<'c, E, C, I>(&'c MemoryCell<E, C, I>);

impl<E, C, I> CommitView for MemoryView<'_, E, C, I> {
    type Error = Infallible;

    fn pending(&self, consumed: &Consumed) -> Result<bool, Infallible> {
        Ok(self.0.is_pending(consumed))
    }

    fn version(&self, key: &FactKey) -> Result<FactVersion, Infallible> {
        Ok(self
            .0
            .facts
            .get(key)
            .map_or(FactVersion::ABSENT, |state| state.version))
    }

    fn concrete(&self, key: &FactKey) -> Result<Option<ObjectTypeName>, Infallible> {
        Ok(self.0.concrete.get(key).cloned())
    }

    fn can_read_observed_source_owners(&self) -> bool {
        true
    }

    fn observed_source_owner(
        &self,
        marker: &FactKey,
    ) -> Result<Option<norn_kernel::snapshot_observe::ObservedSourceOwner>, Infallible> {
        Ok(self
            .0
            .facts
            .get(marker)
            .and_then(|state| state.values.as_ref())
            .and_then(|values| values.get("owner"))
            .and_then(norn_kernel::snapshot_observe::ObservedSourceOwner::from_value))
    }

    fn link_version(&self, key: &LinkKey) -> Result<FactVersion, Infallible> {
        Ok(self
            .0
            .links
            .get(key)
            .map_or(FactVersion::ABSENT, |state| state.version))
    }

    fn link_targets(
        &self,
        source: &FactKey,
        link: &str,
    ) -> Result<Option<Vec<LinkReadTarget>>, Infallible> {
        let targets = self
            .0
            .links
            .iter()
            .filter(|(key, state)| key.source == *source && key.link == link && state.exists());
        let mut reads = Vec::new();
        let mut bytes = 0usize;
        for (key, state) in targets {
            bytes = bytes.saturating_add(key.target.key.len());
            if reads.len() == 1_000 || bytes > 262_144 {
                return Ok(None);
            }
            reads.push(LinkReadTarget {
                key: key.clone(),
                version: state.version,
            });
        }
        Ok(Some(reads))
    }

    fn predicate_members(
        &self,
        predicate: &PredicateQuery,
        limit: usize,
    ) -> Result<Option<Vec<PredicateReadMember>>, Infallible> {
        Ok(self
            .0
            .predicate_objects(predicate, limit)
            .map(|objects| objects.iter().map(PredicateObject::member).collect()))
    }

    fn has_link_target(
        &self,
        source: &FactKey,
        link: &str,
        excluded: &BTreeSet<FactKey>,
    ) -> Result<Option<bool>, Infallible> {
        Ok(Some(self.0.links.iter().any(|(key, state)| {
            key.source == *source
                && key.link == link
                && state.exists()
                && !excluded.contains(&key.target)
        })))
    }

    fn links_of(&self, key: &FactKey) -> Result<Vec<LinkKey>, Infallible> {
        Ok(self
            .0
            .links
            .iter()
            .filter(|(link, state)| state.exists() && (link.source == *key || link.target == *key))
            .map(|(link, _)| link.clone())
            .collect())
    }
}

impl<E, C, I: Clone + norn_kernel::serde::Serialize> MemoryCell<E, C, I> {
    #[allow(
        clippy::too_many_lines,
        reason = "one atomic commit materializes object and link values before applying any effects"
    )]
    fn commit_with_authorization(
        &mut self,
        mut commit: CellCommit<E, C, I>,
        authorization: Option<&norn_kernel::snapshot_observe::ObservationAuthorization>,
        owned_rows: Option<OwnedRowsCommitProof>,
    ) -> Result<CommitReceipt, CommitError<Infallible>> {
        let (CommitVersions { written, linked }, fact_keys, link_keys) = {
            let catalog = TypeCatalog::new(&self.types)
                .with_governing_laws(&self.law_artifacts)
                .with_interfaces(&self.interfaces)
                .with_property_types(&self.property_types);
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
            let model = CommitModel::new(&catalog);
            let link_values = commit
                .links
                .iter()
                .map(|write| match &write.mutation {
                    LinkMutation::Add { values } => model
                        .effective_link_values(&commit, &MemoryView(self), &write.key, values)
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
            let versions = match (authorization, owned_rows) {
                (Some(authorization), None) => {
                    check_commit_observed(&commit, &model, &MemoryView(self), authorization)?
                }
                (None, Some(proof)) => {
                    check_commit_owned_rows(&commit, &model, &MemoryView(self), proof)?
                }
                (None, None) => check_commit(&commit, &model, &MemoryView(self))?,
                (Some(_), Some(_)) => return Err(CommitError::ObservationAuthorization),
            };
            let fact_keys: Vec<FactKey> = commit
                .writes
                .iter()
                .map(|write| model.canonical(&write.key))
                .collect();
            let link_keys: Vec<LinkKey> = commit
                .links
                .iter()
                .map(|write| model.canonical_link(&write.key))
                .collect();
            (versions, fact_keys, link_keys)
        };
        let seq = self.seq.checked_next().ok_or(CommitError::Overflow)?;
        let CellCommit {
            consumes,
            writes,
            events,
            effects,
            links,
            ..
        } = commit;
        if let Some(consumed) = &consumes {
            self.consume(consumed);
        }
        for ((write, version), canonical) in writes.into_iter().zip(&written).zip(fact_keys) {
            let values = match write.mutation {
                FactMutation::Put { values } => {
                    self.concrete
                        .insert(canonical.clone(), write.key.object_type);
                    Some(values)
                }
                FactMutation::Delete => {
                    self.concrete.remove(&canonical);
                    None
                }
            };
            self.facts.insert(
                canonical,
                FactState {
                    version: version.version,
                    values,
                },
            );
        }
        for ((write, version), canonical) in links.into_iter().zip(&linked).zip(link_keys) {
            let values = match write.mutation {
                LinkMutation::Add { values } => Some(values),
                LinkMutation::Remove => None,
            };
            self.links.insert(
                canonical,
                LinkState {
                    version: version.version,
                    values,
                },
            );
        }
        for effect in effects {
            self.enqueue(effect, seq);
        }
        self.seq = seq;
        self.ledger.push(LedgerEntry {
            seq,
            consumed: consumes,
            written: written.clone(),
            linked: linked.clone(),
            events,
        });
        Ok(CommitReceipt {
            seq,
            written,
            linked,
        })
    }
}

impl<E, C, I: Clone + norn_kernel::serde::Serialize> CellHost for MemoryCell<E, C, I> {
    type Event = E;
    type Command = C;
    type Input = I;
    type Error = Infallible;

    fn load(&self, key: &FactKey) -> Result<FactState, Infallible> {
        let catalog = TypeCatalog::new(&self.types)
            .with_governing_laws(&self.law_artifacts)
            .with_interfaces(&self.interfaces)
            .with_property_types(&self.property_types);
        let model = CommitModel::new(&catalog);
        let canonical = model.canonical(key);
        let mut state = self.facts.get(&canonical).cloned().unwrap_or_default();
        // One object, one concrete type: through another type it reads absent.
        if !self
            .concrete
            .get(&canonical)
            .is_some_and(|concrete| model.is_a(concrete, &key.object_type))
        {
            state.values = None;
        }
        Ok(state)
    }

    fn load_bounded(
        &self,
        key: &FactKey,
        max_bytes: u64,
    ) -> Result<Option<BoundedFact>, Infallible> {
        let state = CellHost::load(self, key)?;
        let bytes = state
            .values
            .as_ref()
            .and_then(|values| JsonValue::Object(values.clone()).canonical_json().ok())
            .map_or(0, |value| u64::try_from(value.len()).unwrap_or(u64::MAX));
        if bytes > max_bytes {
            return Ok(None);
        }
        let catalog = TypeCatalog::new(&self.types)
            .with_governing_laws(&self.law_artifacts)
            .with_interfaces(&self.interfaces)
            .with_property_types(&self.property_types);
        let concrete_type = if state.exists() {
            let canonical = CommitModel::new(&catalog).canonical(key);
            self.concrete.get(&canonical).cloned()
        } else {
            None
        };
        Ok(Some(BoundedFact {
            concrete_type,
            state,
        }))
    }

    fn load_link(&self, key: &LinkKey) -> Result<LinkState, Infallible> {
        let catalog = TypeCatalog::new(&self.types)
            .with_governing_laws(&self.law_artifacts)
            .with_interfaces(&self.interfaces)
            .with_property_types(&self.property_types);
        let canonical = CommitModel::new(&catalog).canonical_link(key);
        Ok(self.links.get(&canonical).cloned().unwrap_or_default())
    }

    fn read_predicate(
        &self,
        predicate: &PredicateQuery,
        max_members: usize,
        _max_bytes: u64,
    ) -> Result<Option<Vec<PredicateObject>>, Infallible> {
        Ok(self.predicate_objects(predicate, max_members.saturating_add(1)))
    }

    fn read_link_targets(
        &self,
        source: &FactKey,
        link: &str,
        max_targets: usize,
        max_key_bytes: u64,
    ) -> Result<Option<Vec<LinkReadTarget>>, Infallible> {
        let catalog = TypeCatalog::new(&self.types)
            .with_governing_laws(&self.law_artifacts)
            .with_interfaces(&self.interfaces)
            .with_property_types(&self.property_types);
        let model = CommitModel::new(&catalog);
        let requested_source = (*source).clone();
        let canonical_source = model.canonical(source);
        let targets: Vec<_> = self
            .links
            .iter()
            .filter(|(key, state)| {
                key.source == canonical_source && key.link == link && state.exists()
            })
            .map(|(key, state)| {
                let mut key = key.clone();
                key.source = requested_source.clone();
                let canonical_target = model.canonical(&key.target);
                key.target.object_type = self
                    .concrete
                    .get(&canonical_target)
                    .cloned()
                    .unwrap_or(canonical_target.object_type);
                LinkReadTarget {
                    key,
                    version: state.version,
                }
            })
            .collect();
        let key_bytes: u64 = targets
            .iter()
            .map(|target| u64::try_from(target.key.target.key.len()).unwrap_or(u64::MAX))
            .fold(0, u64::saturating_add);
        if targets.len() > max_targets || key_bytes > max_key_bytes {
            return Ok(None);
        }
        Ok(Some(targets))
    }

    fn commit(
        &mut self,
        commit: CellCommit<E, C, I>,
    ) -> Result<CommitReceipt, CommitError<Infallible>> {
        self.commit_with_authorization(commit, None, None)
    }

    fn commit_owned_rows(
        &mut self,
        commit: CellCommit<E, C, I>,
        proof: OwnedRowsCommitProof,
    ) -> Result<CommitReceipt, CommitError<Infallible>> {
        if proof.quantity().is_some() {
            return Err(CommitError::ObservationAuthorization);
        }
        self.commit_with_authorization(commit, None, Some(proof))
    }

    fn commit_observed(
        &mut self,
        mut commit: CellCommit<E, C, I>,
        authorization: ObservationAuthorization,
    ) -> Result<CommitReceipt, CommitError<Infallible>>
    where
        E: From<ObservationEvent>,
    {
        if !authorization.validates(&commit) {
            return Err(CommitError::ObservationAuthorization);
        }
        authorization.append_findings(&mut commit);
        self.commit_with_authorization(commit, Some(&authorization), None)
    }

    fn perform(&mut self) -> Result<Option<Performed<I>>, Infallible> {
        if let Some(performed) = &self.performed {
            return Ok(Some(performed.clone()));
        }
        let Some(command) = self.outbox.first() else {
            return Ok(None);
        };
        let input = (self.responder)(command);
        let performed = Performed {
            command: command.id.clone(),
            generation: command.generation,
            input,
        };
        self.executions = self.executions.saturating_add(1);
        self.performed = Some(performed.clone());
        Ok(Some(performed))
    }

    fn observe(&mut self) -> Result<Option<Observed<I>>, Infallible> {
        Ok(self.inbox.first().cloned())
    }

    fn clock(&mut self) -> Result<Option<DueTimer<I>>, Infallible> {
        let now = self.now;
        Ok(self
            .timers
            .values()
            .filter(|due| due.timer.due <= now)
            .min_by(|a, b| (a.timer.due, &a.timer.id).cmp(&(b.timer.due, &b.timer.id)))
            .cloned())
    }

    fn publish(&mut self, receipt: &CommitReceipt) -> Result<(), Infallible> {
        self.published.push(receipt.clone());
        Ok(())
    }
}

// ------------------------------------------------------------ law vocabulary

/// Ledger event type of the law machine.
pub type LawEvent = u8;
/// Command payload type of the law machine.
pub type LawCommand = u8;
/// A commit of the law machine.
pub type LawCommit = CellCommit<LawEvent, LawCommand, LawInput>;

/// Kernel input type of the law machine.
#[derive(Debug, Clone, PartialEq, Eq, Hash, norn_kernel::serde::Serialize)]
#[serde(crate = "norn_kernel::serde")]
pub enum LawInput {
    /// A fired timer's payload.
    Timer(u8),
    /// A performed command's result (the command payload, echoed).
    Result(u8),
    /// An observation's payload.
    Observed(u8),
}

/// How many distinct objects, commands, timers, and observations the law
/// machine uses; small, so conflicts and replacements are frequent.
pub const LAW_UNIVERSE: u8 = 3;

/// The law machine's base type: links `rel` (many to many) and `owner` (to
/// one) to any `law.Thing`.
pub const LAW_THING: &str = "law.Thing";
/// The law machine's subtype of [`LAW_THING`] (#3018): object `k2` is one,
/// and is also addressed through its base type ([`law_alias`]).
pub const LAW_CHILD: &str = "law.Child";

/// The law machine's object types: its model ([`MemoryCell::with_types`]).
/// Hosts with a store model declare the same types (with the `v` property).
#[must_use]
pub fn law_object_types() -> Vec<ObjectTypeDef> {
    let link = |id: &str, to: Multiplicity| Link {
        id: LinkId(id.to_owned()),
        description: None,
        target: ObjectTypeName(LAW_THING.to_owned()),
        cardinality: LinkCardinality {
            to,
            from: Multiplicity::Many,
            required: false,
        },
        inverse: None,
        acyclic: false,
        properties: Vec::new(),
    };
    let object_type = |name: &str, extends: Option<&str>, links: Vec<Link>| ObjectTypeDef {
        immutable: false,
        role: None,
        name: ObjectTypeName(name.to_owned()),
        description: None,
        status: ResourceStatus::default(),
        extends: extends.map(|name| ObjectTypeName(name.to_owned())),
        implements: Vec::new(),
        markings: Vec::new(),
        properties: Vec::new(),
        links,
        owned_collections: Vec::new(),
        identity: Vec::new(),
        invariants: Vec::new(),
        lifecycles: Vec::new(),
        accounting_leg: None,
    };
    vec![
        object_type(
            LAW_THING,
            None,
            vec![
                link(LAW_LINK, Multiplicity::Many),
                link(LAW_OWNER, Multiplicity::One),
            ],
        ),
        object_type(LAW_CHILD, Some(LAW_THING), Vec::new()),
    ]
}

/// Object `n` of the law machine: `law.Thing/k<n>`, except `k2`, a
/// `law.Child`.
#[must_use]
pub fn law_key(n: u8) -> FactKey {
    let object_type = if n == 2 { LAW_CHILD } else { LAW_THING };
    FactKey::new(object_type, format!("k{n}"))
}

/// Object `n` named through the base type: `law.Thing/k<n>` (an alias of
/// [`law_key`] for `k2`).
#[must_use]
pub fn law_alias(n: u8) -> FactKey {
    FactKey::new(LAW_THING, format!("k{n}"))
}

/// The objects the law machine compares after every operation: every
/// object, and `k2` through its base type too.
#[must_use]
pub fn law_fact_keys() -> Vec<FactKey> {
    let mut keys: Vec<FactKey> = (0..LAW_UNIVERSE).map(law_key).collect();
    keys.push(law_alias(2));
    keys
}

/// The many-to-many link id of the law machine.
pub const LAW_LINK: &str = "rel";
/// The to-one link id of the law machine.
pub const LAW_OWNER: &str = "owner";
/// A link id the law types do not declare: every write is refused.
pub const LAW_STRAY: &str = "stray";

/// How many distinct links the law machine uses: every ordered pair of its
/// objects, self-links included, for `rel`, `owner`, and `stray`.
pub const LAW_LINKS: u8 = 3 * LAW_UNIVERSE * LAW_UNIVERSE;

/// How many of the law links are declared (`rel` and `owner`): the prefix
/// of [`law_link_key`] that state snapshots read. Reading an undeclared link
/// is host-defined (the SQLite driver refuses it), so only writes to
/// `stray` are exercised.
pub const LAW_DECLARED_LINKS: u8 = 2 * LAW_UNIVERSE * LAW_UNIVERSE;

/// Link `n` of the law machine: `rel`, then `owner`, then `stray`, each from
/// object `m % LAW_UNIVERSE` to object `m / LAW_UNIVERSE`.
#[must_use]
pub fn law_link_key(n: u8) -> LinkKey {
    let pairs = LAW_UNIVERSE * LAW_UNIVERSE;
    let link = match n / pairs {
        0 => LAW_LINK,
        1 => LAW_OWNER,
        _ => LAW_STRAY,
    };
    let m = n % pairs;
    LinkKey::new(law_key(m % LAW_UNIVERSE), link, law_key(m / LAW_UNIVERSE))
}

/// Property values `{ "v": value }`.
#[must_use]
pub fn law_values(value: u8) -> JsonObject {
    JsonObject::from([("v".to_owned(), JsonValue::from(i64::from(value)))])
}

/// Command `n` of the law machine at `generation`.
#[must_use]
pub fn law_command(n: u8, generation: u64, payload: u8) -> Command<LawCommand> {
    Command {
        id: CommandId(format!("c{n}")),
        generation: Generation(generation),
        idempotency_key: IdempotencyKey(format!("c{n}/{generation}")),
        freshness: FreshnessPolicy::AppendOnly,
        deadline: None,
        payload,
    }
}

/// Timer `n` of the law machine.
#[must_use]
pub fn law_timer_id(n: u8) -> TimerId {
    TimerId(format!("t{n}"))
}

/// Observation `n` of the law machine.
#[must_use]
pub fn law_observed(n: u8, payload: u8) -> Observed<LawInput> {
    Observed {
        id: ObservationId(format!("o{n}")),
        provenance: Provenance {
            source: "law".to_owned(),
            source_id: format!("record-{n}"),
            observed_at: LogicalTime::EPOCH,
            mapping_version: "law/1".to_owned(),
        },
        input: LawInput::Observed(payload),
    }
}

/// The law machine's outside world: a command's result echoes its payload.
#[must_use]
pub fn law_responder(command: &Command<LawCommand>) -> LawInput {
    LawInput::Result(command.payload)
}

/// What an injected storage fault does to a host that lists it in
/// [`SimulatedCell::FAULT_KINDS`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FaultKind {
    /// The host dies at the faulted statement: its transaction never
    /// commits, and the host reopens from durable state before the faulted
    /// operation returns. Only file-backed hosts survive it.
    Crash,
    /// The faulted statement fails with an I/O error: its transaction rolls
    /// back and the host keeps its connection.
    IoError,
    /// The storage's own commit of the faulted transaction fails after its
    /// body succeeded (`at_statement` is ignored): the storage rolls the
    /// transaction back and the operation fails with a host error. Only
    /// hosts whose storage commits itself (a Durable Object's
    /// `transactionSync`) list it.
    CommitFails,
}

/// One storage fault, armed for one operation ([`CellOp::Faulted`]).
///
/// The fault fails statement `at_statement` (counting from 0) of the
/// operation's write transaction `transaction` (counting from 0). A faulted
/// transaction never commits: one that would commit with fewer statements
/// fails at its commit point instead. An operation that runs fewer write
/// transactions is not faulted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct FaultPlan {
    /// What the fault does.
    pub kind: FaultKind,
    /// Which write transaction of the operation fails: a commit runs one, a
    /// fresh perform two (its claim, then its result).
    pub transaction: u8,
    /// Which SQL statement of that transaction fails.
    pub at_statement: u8,
}

/// A host the law machine can drive: a [`CellHost`] over the law vocabulary
/// plus the test controls a real host gets from its environment.
pub trait SimulatedCell:
    CellHost<Event = LawEvent, Command = LawCommand, Input = LawInput> + 'static
{
    /// Whether a fresh `perform` records an empty internal claim commit before
    /// dispatch. The reference model keeps receipt sequences exact for hosts
    /// that expose this stronger native durability behavior.
    const COMMITS_PERFORM_CLAIMS: bool = false;

    /// The storage faults this host survives (none by default). When any are
    /// listed, the law machine arms them ([`CellOp::Faulted`]) and runs in
    /// swarm mode. Such a host must also declare
    /// [`Self::COMMITS_PERFORM_CLAIMS`], and its execution count
    /// ([`Self::sim_executions`]) counts durably recorded results.
    const FAULT_KINDS: &'static [FaultKind] = &[];

    /// Arms `plan` for the next operation's write transactions. Called only
    /// on hosts that list [`Self::FAULT_KINDS`].
    fn sim_arm_fault(&mut self, _plan: FaultPlan) {}

    /// Disarms the fault armed by [`Self::sim_arm_fault`] and returns whether
    /// it fired. A host that crashed has already reopened.
    fn sim_disarm_fault(&mut self) -> bool {
        false
    }

    /// Whether the host splits `perform` into a dispatch and a later result
    /// delivery, so the law machine can put an unreliable network between
    /// the cell and its outside world (law 7). Such a host runs in swarm mode
    /// and must also declare [`Self::COMMITS_PERFORM_CLAIMS`]: a dispatch is
    /// a durable claim.
    const NETWORK: bool = false;

    /// Claims the fresh outbox head for external dispatch and returns it;
    /// `None` when the outbox is empty or its head was already claimed or
    /// has a recorded result. Called only on hosts that declare
    /// [`Self::NETWORK`].
    ///
    /// # Errors
    /// A host failure, as its message.
    fn sim_dispatch(&mut self) -> Result<Option<Command<LawCommand>>, String> {
        Ok(None)
    }

    /// Hands the host one result from the outside world for `command` (at
    /// its generation) and returns whether the host recorded it. Called only
    /// on hosts that declare [`Self::NETWORK`].
    ///
    /// # Errors
    /// A host failure, as its message.
    fn sim_deliver(
        &mut self,
        _command: &Command<LawCommand>,
        _input: &LawInput,
    ) -> Result<bool, String> {
        Ok(false)
    }

    /// A fresh, empty cell at the epoch whose outside world answers every
    /// command with [`law_responder`].
    fn sim_new() -> Self;
    /// Moves the host clock forward to `now`.
    fn sim_advance_to(&mut self, now: LogicalTime);
    /// Ingests one external observation (ignored if its id was seen before).
    fn sim_inject(&mut self, observed: Observed<LawInput>);
    /// How many times the outside world executed a command.
    fn sim_executions(&self) -> u64;
    /// The pending commands, oldest first (test introspection).
    fn sim_outbox(&self) -> Vec<Command<LawCommand>>;
    /// The pending timers, ordered by id (test introspection).
    fn sim_timers(&self) -> Vec<DueTimer<LawInput>>;
    /// Every committed ledger entry, in order (test introspection).
    fn sim_ledger(&self) -> Vec<LedgerEntry<LawEvent>>;
}

impl SimulatedCell for MemoryCell<LawEvent, LawCommand, LawInput> {
    fn sim_new() -> Self {
        Self::new(law_responder).with_types(law_object_types())
    }

    fn sim_advance_to(&mut self, now: LogicalTime) {
        self.advance_to(now);
    }

    fn sim_inject(&mut self, observed: Observed<LawInput>) {
        self.inject(observed);
    }

    fn sim_executions(&self) -> u64 {
        self.executions()
    }

    fn sim_outbox(&self) -> Vec<Command<LawCommand>> {
        self.outbox().to_vec()
    }

    fn sim_timers(&self) -> Vec<DueTimer<LawInput>> {
        self.timers().values().cloned().collect()
    }

    fn sim_ledger(&self) -> Vec<LedgerEntry<LawEvent>> {
        self.ledger().to_vec()
    }
}

// ------------------------------------------------------------ model

/// One generated operation on a cell.
#[derive(Debug, Clone, PartialEq)]
pub enum CellOp {
    /// The `commit` verb.
    Commit(LawCommit),
    /// The `perform` verb.
    Perform,
    /// The `observe` verb.
    Observe,
    /// The `clock` verb.
    Clock,
    /// Advance the host clock by this many milliseconds.
    Advance(u64),
    /// An external observation arrives.
    Inject {
        /// Observation number.
        id: u8,
        /// Its payload.
        payload: u8,
    },
    /// A commit or perform under a storage fault (hosts that list
    /// [`SimulatedCell::FAULT_KINDS`] only).
    Faulted(Box<Faulted>),
    /// The host claims its fresh outbox head and dispatches it; the outside
    /// world's result goes in flight (hosts that declare
    /// [`SimulatedCell::NETWORK`] only).
    Dispatch(Box<Dispatching>),
    /// The network hands the host a result (law 7).
    Deliver(Box<Delivery>),
    /// The network loses the in-flight result at this index (law 7).
    Lose(u8),
}

/// A dispatch of the law machine ([`CellOp::Dispatch`]).
#[derive(Debug, Clone, PartialEq)]
pub struct Dispatching {
    /// An unfaulted commit run first that leaves a fresh command at the
    /// outbox head (enqueueing one, consuming the head's delivered result,
    /// or re-issuing a head whose result is outstanding), so the dispatch
    /// always claims something.
    pub prepare: Option<LawCommit>,
    /// After the dispatch, a commit re-issuing the dispatched head at its
    /// next generation, dispatched again: two results for one command are
    /// then in flight, the older one stale.
    pub reissue: Option<LawCommit>,
}

/// One result the network hands the host ([`CellOp::Deliver`]).
#[derive(Debug, Clone, PartialEq)]
pub struct Delivery {
    /// The dispatched command, at the generation it was dispatched with.
    pub command: Command<LawCommand>,
    /// The outside world's result for it.
    pub input: LawInput,
    /// How the network delivers it.
    pub route: Route,
}

/// How the network delivers a result.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Route {
    /// The oldest result in flight.
    InOrder,
    /// A later result in flight, ahead of older ones.
    OutOfOrder,
    /// A copy of a result sent before, delivered or not.
    Duplicate,
}

/// A faulted operation of the law machine ([`CellOp::Faulted`]).
#[derive(Debug, Clone, PartialEq)]
pub struct Faulted {
    /// The fault armed for `target`.
    pub plan: FaultPlan,
    /// An unfaulted commit run first. For a faulted perform it makes a fresh
    /// command the outbox head (consuming a delivered result, or replacing a
    /// head whose result was lost), so the fault meets a claim and a result
    /// record rather than an empty outbox.
    pub prepare: Option<LawCommit>,
    /// The faulted operation.
    pub target: FaultTarget,
}

/// The operation a [`Faulted`] transition arms its fault for.
#[derive(Debug, Clone, PartialEq)]
pub enum FaultTarget {
    /// The `commit` verb (boxed: a commit dwarfs the unit variant).
    Commit(Box<LawCommit>),
    /// The `perform` verb.
    Perform,
}

/// What the model expects of the last applied operation.
#[derive(Debug, Clone, PartialEq)]
enum Expect {
    Commit(Result<CommitReceipt, CommitError<Infallible>>),
    Perform {
        delivered: Option<Performed<LawInput>>,
        executed: bool,
    },
    Observe(Option<Observed<LawInput>>),
    Clock(Option<DueTimer<LawInput>>),
    /// The operation fails with a host error, executes nothing, and changes
    /// nothing the model does not.
    Failed,
    /// The command a dispatch claims, if any.
    Dispatch(Option<Command<LawCommand>>),
    /// Whether a delivered result is recorded; if not, whether it was for a
    /// stale generation; and whether it waited behind other operations.
    Deliver {
        recorded: bool,
        stale: bool,
        delayed: bool,
    },
    /// Whether an injected observation's id was seen before, and whether it
    /// was already consumed.
    Inject {
        duplicate: bool,
        late: bool,
    },
    Nothing,
}

/// Whether an armed fault reaches a write transaction of the operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Firing {
    /// The operation runs the faulted transaction and would commit it.
    Fires,
    /// The operation runs no such transaction.
    Never,
    /// A refused commit: the host may refuse before the faulted statement.
    Either,
}

/// The fault armed for the last applied operation, and the model's
/// prediction of whether it fires.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ArmedFault {
    plan: FaultPlan,
    firing: Firing,
}

/// Everything about a cell the laws compare after each operation.
#[derive(Debug, Clone, PartialEq)]
struct Snapshot {
    facts: Vec<Result<FactState, String>>,
    links: Vec<Result<LinkState, String>>,
    clock: Result<Option<DueTimer<LawInput>>, String>,
    observe: Result<Option<Observed<LawInput>>, String>,
    outbox: Vec<Command<LawCommand>>,
    timers: Vec<DueTimer<LawInput>>,
    ledger: Vec<LedgerEntry<LawEvent>>,
    executions: u64,
}

/// The independent reference model of a cell. Model facts (canonical
/// identity, `is_a`, link declarations) come from the law types through
/// [`CommitModel`]; the commit semantics are re-implemented here.
#[derive(Debug, Clone)]
pub struct CellModel {
    now: u64,
    seq: u64,
    commits_perform_claims: bool,
    types: Vec<ObjectTypeDef>,
    facts: BTreeMap<FactKey, FactState>,
    concrete: BTreeMap<FactKey, ObjectTypeName>,
    links: BTreeMap<LinkKey, LinkState>,
    outbox: Vec<Command<LawCommand>>,
    head_performed: bool,
    /// The head was claimed but its result never recorded (a fault hit the
    /// result record): it is never executed again at this generation.
    head_unknown: bool,
    generations: BTreeMap<CommandId, Generation>,
    timers: BTreeMap<TimerId, DueTimer<LawInput>>,
    inbox: Vec<Observed<LawInput>>,
    seen: BTreeSet<ObservationId>,
    ledger: Vec<LedgerEntry<LawEvent>>,
    executions: u64,
    /// Results dispatched and not yet delivered or lost, oldest first, with
    /// the operation count when each was sent (law 7).
    in_flight: Vec<(Command<LawCommand>, LawInput, u64)>,
    /// Every result ever dispatched, for duplicates.
    sent: Vec<(Command<LawCommand>, LawInput)>,
    /// Operations applied so far: the network's clock.
    steps: u64,
}

impl Default for CellModel {
    fn default() -> Self {
        Self {
            now: 0,
            seq: 0,
            commits_perform_claims: false,
            types: law_object_types(),
            facts: BTreeMap::new(),
            concrete: BTreeMap::new(),
            links: BTreeMap::new(),
            outbox: Vec::new(),
            head_performed: false,
            head_unknown: false,
            generations: BTreeMap::new(),
            timers: BTreeMap::new(),
            inbox: Vec::new(),
            seen: BTreeSet::new(),
            ledger: Vec::new(),
            executions: 0,
            in_flight: Vec::new(),
            sent: Vec::new(),
            steps: 0,
        }
    }
}

impl CellModel {
    fn with_model<R>(&self, f: impl FnOnce(&CommitModel<'_, '_>) -> R) -> R {
        let catalog = TypeCatalog::new(&self.types);
        f(&CommitModel::new(&catalog))
    }

    fn version(&self, key: &FactKey) -> FactVersion {
        let canonical = self.with_model(|model| model.canonical(key));
        self.facts
            .get(&canonical)
            .map_or(FactVersion::ABSENT, |state| state.version)
    }

    fn link_version(&self, key: &LinkKey) -> FactVersion {
        let canonical = self.with_model(|model| model.canonical_link(key));
        self.links
            .get(&canonical)
            .map_or(FactVersion::ABSENT, |state| state.version)
    }

    fn load(&self, key: &FactKey) -> FactState {
        self.with_model(|model| {
            let canonical = model.canonical(key);
            let mut state = self.facts.get(&canonical).cloned().unwrap_or_default();
            if !self
                .concrete
                .get(&canonical)
                .is_some_and(|concrete| model.is_a(concrete, &key.object_type))
            {
                state.values = None;
            }
            state
        })
    }

    fn pending(&self, consumed: &Consumed) -> bool {
        match consumed {
            Consumed::Timer { id, incarnation } => self
                .timers
                .get(id)
                .is_some_and(|due| due.incarnation == *incarnation),
            Consumed::Result {
                command,
                generation,
            } => {
                self.head_performed
                    && self
                        .outbox
                        .first()
                        .is_some_and(|head| head.id == *command && head.generation == *generation)
            }
            Consumed::Observation { id } => self.inbox.iter().any(|observed| observed.id == *id),
        }
    }

    fn admit(&self, commit: &LawCommit) -> Result<CommitVersions, CommitError<Infallible>> {
        self.with_model(|model| {
            Self::admit_shape(commit, *model)?;
            if let Some(consumed) = &commit.consumes
                && !self.pending(consumed)
            {
                return Err(CommitError::NotPending {
                    consumed: consumed.clone(),
                });
            }
            self.admit_versions(commit, *model)?;
            let after = self.links_after(commit, *model);
            self.admit_endpoints(commit, *model, &after)?;
            self.admit_cardinality(commit, *model, &after)?;
            let mut written = Vec::new();
            for write in &commit.writes {
                let version = write.expected.checked_next().ok_or(CommitError::Overflow)?;
                written.push(WrittenVersion {
                    key: write.key.clone(),
                    version,
                });
            }
            let mut linked = Vec::new();
            for write in &commit.links {
                let version = write.expected.checked_next().ok_or(CommitError::Overflow)?;
                linked.push(WrittenLinkVersion {
                    key: write.key.clone(),
                    version,
                });
            }
            Ok(CommitVersions { written, linked })
        })
    }

    fn admit_shape(
        commit: &LawCommit,
        model: CommitModel<'_, '_>,
    ) -> Result<(), CommitError<Infallible>> {
        let mut keys = BTreeSet::new();
        for key in commit
            .reads
            .iter()
            .map(|read| &read.key)
            .chain(commit.writes.iter().map(|write| &write.key))
        {
            if !keys.insert(model.canonical(key)) {
                return Err(CommitError::DuplicateKey { key: key.clone() });
            }
        }
        let mut links = BTreeSet::new();
        for write in &commit.links {
            if !links.insert(model.canonical_link(&write.key)) {
                return Err(CommitError::DuplicateLink {
                    key: Box::new(write.key.clone()),
                });
            }
        }
        for write in &commit.links {
            let declared = model.link(&write.key.source.object_type, &write.key.link);
            if !declared.is_some_and(|link| model.is_a(&write.key.target.object_type, &link.target))
            {
                return Err(CommitError::LinkTarget {
                    key: Box::new(write.key.clone()),
                });
            }
        }
        Ok(())
    }

    fn admit_versions(
        &self,
        commit: &LawCommit,
        model: CommitModel<'_, '_>,
    ) -> Result<(), CommitError<Infallible>> {
        for write in &commit.writes {
            let key = model.canonical(&write.key);
            if matches!(write.mutation, FactMutation::Put { .. })
                && self
                    .facts
                    .get(&key)
                    .is_some_and(|state| state.version != FactVersion::ABSENT)
                && !self.concrete.contains_key(&key)
            {
                return Err(CommitError::IdentityReuse);
            }
        }
        let version = |key: &FactKey| {
            self.facts
                .get(&model.canonical(key))
                .map_or(FactVersion::ABSENT, |state| state.version)
        };
        for (key, expected) in commit
            .reads
            .iter()
            .map(|read| (&read.key, read.expected))
            .chain(
                commit
                    .writes
                    .iter()
                    .map(|write| (&write.key, write.expected)),
            )
        {
            let current = version(key);
            if current != expected {
                return Err(CommitError::Conflict {
                    key: key.clone(),
                    expected,
                    current,
                });
            }
        }
        for write in &commit.writes {
            if let FactMutation::Put { .. } = write.mutation
                && let Some(concrete) = self.concrete.get(&model.canonical(&write.key))
                && *concrete != write.key.object_type
            {
                return Err(CommitError::TypeMismatch {
                    key: write.key.clone(),
                    concrete: concrete.clone(),
                });
            }
        }
        for write in &commit.links {
            let current = self
                .links
                .get(&model.canonical_link(&write.key))
                .map_or(FactVersion::ABSENT, |state| state.version);
            if current != write.expected {
                return Err(CommitError::LinkConflict {
                    key: Box::new(write.key.clone()),
                    expected: write.expected,
                    current,
                });
            }
        }
        Ok(())
    }

    /// The canonical live links once `commit`'s link writes apply.
    fn links_after(&self, commit: &LawCommit, model: CommitModel<'_, '_>) -> BTreeSet<LinkKey> {
        let mut live: BTreeSet<LinkKey> = self
            .links
            .iter()
            .filter(|(_, state)| state.exists())
            .map(|(key, _)| key.clone())
            .collect();
        for write in &commit.links {
            let key = model.canonical_link(&write.key);
            match write.mutation {
                LinkMutation::Add { .. } => {
                    live.insert(key);
                }
                LinkMutation::Remove => {
                    live.remove(&key);
                }
            }
        }
        live
    }

    /// The concrete type of canonical object `canonical` once `commit`
    /// applies (as the kernel's check derives it).
    fn concrete_after(
        &self,
        commit: &LawCommit,
        model: CommitModel<'_, '_>,
        canonical: &FactKey,
    ) -> Option<ObjectTypeName> {
        match commit
            .writes
            .iter()
            .find(|fact| model.canonical(&fact.key) == *canonical)
        {
            Some(fact) => match fact.mutation {
                FactMutation::Put { .. } => Some(fact.key.object_type.clone()),
                FactMutation::Delete => None,
            },
            None => self.concrete.get(canonical).cloned(),
        }
    }

    fn admit_endpoints(
        &self,
        commit: &LawCommit,
        model: CommitModel<'_, '_>,
        after: &BTreeSet<LinkKey>,
    ) -> Result<(), CommitError<Infallible>> {
        for write in &commit.links {
            if matches!(write.mutation, LinkMutation::Remove) {
                continue;
            }
            let mut concrete = Vec::new();
            for endpoint in [&write.key.source, &write.key.target] {
                match self
                    .concrete_after(commit, model, &model.canonical(endpoint))
                    .filter(|concrete| model.is_a(concrete, &endpoint.object_type))
                {
                    Some(found) => concrete.push(found),
                    None => {
                        return Err(CommitError::DanglingLink {
                            key: Box::new(write.key.clone()),
                            missing: endpoint.clone(),
                        });
                    }
                }
            }
            if let [source, target] = concrete.as_slice()
                && !model
                    .link(source, &write.key.link)
                    .is_some_and(|declared| model.is_a(target, &declared.target))
            {
                return Err(CommitError::LinkTarget {
                    key: Box::new(write.key.clone()),
                });
            }
        }
        for write in &commit.writes {
            if !matches!(write.mutation, FactMutation::Delete) {
                continue;
            }
            let canonical = model.canonical(&write.key);
            if let Some(link) = after
                .iter()
                .find(|link| link.source == canonical || link.target == canonical)
            {
                return Err(CommitError::DanglingLink {
                    key: Box::new(link.clone()),
                    missing: write.key.clone(),
                });
            }
        }
        Ok(())
    }

    fn admit_cardinality(
        &self,
        commit: &LawCommit,
        model: CommitModel<'_, '_>,
        after: &BTreeSet<LinkKey>,
    ) -> Result<(), CommitError<Infallible>> {
        for write in &commit.links {
            if matches!(write.mutation, LinkMutation::Remove) {
                continue;
            }
            let key = model.canonical_link(&write.key);
            let Some(declared) = self
                .concrete_after(commit, model, &key.source)
                .and_then(|source| model.link(&source, &write.key.link))
            else {
                continue;
            };
            for (multiplicity, outgoing) in [
                (declared.cardinality.to, true),
                (declared.cardinality.from, false),
            ] {
                if multiplicity != Multiplicity::One {
                    continue;
                }
                let endpoint = if outgoing { &key.source } else { &key.target };
                let count = after
                    .iter()
                    .filter(|link| {
                        link.link == key.link
                            && if outgoing {
                                link.source == *endpoint
                            } else {
                                link.target == *endpoint
                            }
                    })
                    .count();
                if count > 1 {
                    return Err(CommitError::LinkCardinality {
                        key: Box::new(write.key.clone()),
                        endpoint: endpoint.clone(),
                    });
                }
            }
        }
        Ok(())
    }

    #[allow(
        clippy::too_many_lines,
        reason = "the reference model keeps admission and atomic application of one commit together"
    )]
    fn commit(&mut self, commit: &LawCommit) -> Result<CommitReceipt, CommitError<Infallible>> {
        let CommitVersions { written, linked } = self.admit(commit)?;
        let effective_writes = {
            let catalog = TypeCatalog::new(&self.types);
            commit
                .writes
                .iter()
                .map(|write| match &write.mutation {
                    FactMutation::Put { values } => catalog
                        .effective_object_values(&write.key.object_type, values)
                        .map(Some)
                        .map_err(|error| CommitError::EffectiveValues {
                            key: write.key.clone(),
                            reason: error.to_string(),
                        }),
                    FactMutation::Delete => Ok(None),
                })
                .collect::<Result<Vec<_>, _>>()?
        };
        let effective_links = {
            let catalog = TypeCatalog::new(&self.types);
            let model = CommitModel::new(&catalog);
            commit
                .links
                .iter()
                .map(|write| match &write.mutation {
                    LinkMutation::Add { values } => {
                        let source = self
                            .concrete_after(commit, model, &model.canonical(&write.key.source))
                            .unwrap_or_else(|| write.key.source.object_type.clone());
                        catalog
                            .effective_link_values(&source, &write.key.link, values)
                            .map(Some)
                            .map_err(|error| CommitError::EffectiveValues {
                                key: write.key.source.clone(),
                                reason: format!("link `{}`: {error}", write.key.link),
                            })
                    }
                    LinkMutation::Remove => Ok(None),
                })
                .collect::<Result<Vec<_>, _>>()?
        };
        let seq = self.seq.checked_add(1).ok_or(CommitError::Overflow)?;
        match &commit.consumes {
            Some(Consumed::Timer { id, .. }) => {
                self.timers.remove(id);
            }
            Some(Consumed::Result { .. }) => {
                self.head_performed = false;
                if !self.outbox.is_empty() {
                    self.outbox.remove(0);
                }
            }
            Some(Consumed::Observation { id }) => self.inbox.retain(|observed| observed.id != *id),
            None => {}
        }
        for ((write, version), effective) in
            commit.writes.iter().zip(&written).zip(effective_writes)
        {
            let canonical = self.with_model(|model| model.canonical(&write.key));
            let values = match (&write.mutation, effective) {
                (FactMutation::Put { .. }, Some(values)) => {
                    self.concrete
                        .insert(canonical.clone(), write.key.object_type.clone());
                    Some(values)
                }
                (FactMutation::Delete, _) => {
                    self.concrete.remove(&canonical);
                    None
                }
                (FactMutation::Put { .. }, None) => {
                    return Err(CommitError::EffectiveValues {
                        key: write.key.clone(),
                        reason: "the effective object was not materialized".to_owned(),
                    });
                }
            };
            self.facts.insert(
                canonical,
                FactState {
                    version: version.version,
                    values,
                },
            );
        }
        for ((write, version), values) in commit.links.iter().zip(&linked).zip(effective_links) {
            let canonical = self.with_model(|model| model.canonical_link(&write.key));
            self.links.insert(
                canonical,
                LinkState {
                    version: version.version,
                    values,
                },
            );
        }
        self.apply_effects(&commit.effects, seq);
        self.seq = seq;
        self.ledger.push(LedgerEntry {
            seq: LedgerSeq(seq),
            consumed: commit.consumes.clone(),
            written: written.clone(),
            linked: linked.clone(),
            events: commit.events.clone(),
        });
        Ok(CommitReceipt {
            seq: LedgerSeq(seq),
            written,
            linked,
        })
    }

    fn apply_effects(&mut self, effects: &[Effect<LawCommand, LawInput>], seq: u64) {
        for effect in effects {
            match effect {
                Effect::Perform(command) => {
                    let known = self.generations.get(&command.id).copied();
                    if known.is_some_and(|known| known >= command.generation) {
                        continue;
                    }
                    self.generations
                        .insert(command.id.clone(), command.generation);
                    let position = self.outbox.iter().position(|slot| slot.id == command.id);
                    if position == Some(0) {
                        self.head_performed = false;
                        self.head_unknown = false;
                    }
                    match position.and_then(|ix| self.outbox.get_mut(ix)) {
                        Some(slot) => *slot = command.clone(),
                        None => self.outbox.push(command.clone()),
                    }
                }
                Effect::Schedule(timer) => {
                    self.timers.insert(
                        timer.id.clone(),
                        DueTimer {
                            timer: timer.clone(),
                            incarnation: LedgerSeq(seq),
                        },
                    );
                }
                Effect::CancelTimer(id) => {
                    self.timers.remove(id);
                }
            }
        }
    }

    fn perform(&mut self) -> Expect {
        let Some(head) = self.outbox.first() else {
            return Expect::Perform {
                delivered: None,
                executed: false,
            };
        };
        if self.head_unknown {
            return Expect::Failed;
        }
        let delivered = Performed {
            command: head.id.clone(),
            generation: head.generation,
            input: law_responder(head),
        };
        let executed = !self.head_performed;
        if executed {
            if self.commits_perform_claims {
                self.claim();
            }
            self.executions = self.executions.saturating_add(1);
            self.head_performed = true;
        }
        Expect::Perform {
            delivered: Some(delivered),
            executed,
        }
    }

    /// The empty claim commit a claim-committing host records before a
    /// fresh execution.
    fn claim(&mut self) {
        self.seq = self.seq.saturating_add(1);
        self.ledger.push(LedgerEntry {
            seq: LedgerSeq(self.seq),
            consumed: None,
            written: Vec::new(),
            linked: Vec::new(),
            events: Vec::new(),
        });
    }

    /// Whether the outbox head is a command never claimed.
    fn fresh_head(&self) -> bool {
        !self.outbox.is_empty() && !self.head_performed && !self.head_unknown
    }

    /// A commit re-issuing a claimed head whose result is not recorded at its
    /// next generation, so its outstanding result turns stale; `None`
    /// otherwise.
    fn reissue_head(&self) -> Option<LawCommit> {
        let head = self.outbox.first().filter(|_| self.head_unknown)?;
        let generation = self
            .generations
            .get(&head.id)
            .map_or(head.generation.0, |known| known.0)
            .saturating_add(1);
        let command = Command {
            generation: Generation(generation),
            idempotency_key: IdempotencyKey(format!("{}/{generation}", head.id.0)),
            ..head.clone()
        };
        Some(CellCommit {
            effects: vec![Effect::Perform(command)],
            ..CellCommit::empty()
        })
    }

    /// A dispatch (law 7): the fresh head is claimed and its result sent.
    fn dispatch(&mut self) -> Expect {
        let Some(head) = self.outbox.first().filter(|_| self.fresh_head()).cloned() else {
            return Expect::Dispatch(None);
        };
        if self.commits_perform_claims {
            self.claim();
        }
        self.head_unknown = true;
        let input = law_responder(&head);
        self.in_flight
            .push((head.clone(), input.clone(), self.steps));
        self.sent.push((head.clone(), input));
        Expect::Dispatch(Some(head))
    }

    /// A [`Dispatching`]: the expectations of its preparing commit, its
    /// dispatch, and its re-issue and second dispatch, in order.
    fn dispatching(&mut self, dispatching: &Dispatching) -> Vec<Expect> {
        let mut chain = Vec::new();
        if let Some(commit) = &dispatching.prepare {
            chain.push(Expect::Commit(self.commit(commit)));
        }
        chain.push(self.dispatch());
        if let Some(commit) = &dispatching.reissue {
            chain.push(Expect::Commit(self.commit(commit)));
            chain.push(self.dispatch());
        }
        chain
    }

    /// A dispatch prepared to claim a fresh head; with `twice`, re-issued and
    /// dispatched again, the re-issue computed on the state the dispatch
    /// leaves.
    fn dispatch_strategy(&self, twice: bool) -> BoxedStrategy<CellOp> {
        let model = self.clone();
        self.prepare_perform()
            .prop_map(move |prepare| {
                let reissue = if twice {
                    let mut after = model.clone();
                    if let Some(commit) = &prepare {
                        after.commit(commit).ok();
                    }
                    after.dispatch();
                    after.reissue_head()
                } else {
                    None
                };
                CellOp::Dispatch(Box::new(Dispatching { prepare, reissue }))
            })
            .boxed()
    }

    /// A delivery (law 7): recorded exactly when it is the current head's
    /// first result at that generation.
    fn deliver(&mut self, delivery: &Delivery) -> Expect {
        let mut delayed = false;
        if delivery.route != Route::Duplicate
            && let Some(ix) = self.in_flight.iter().position(|(command, input, _)| {
                *command == delivery.command && *input == delivery.input
            })
        {
            let (_, _, sent_at) = self.in_flight.remove(ix);
            delayed = self.steps.saturating_sub(sent_at) > 1;
        }
        let current = self.outbox.first().is_some_and(|head| {
            head.id == delivery.command.id && head.generation == delivery.command.generation
        });
        if current && !self.head_performed {
            self.head_performed = true;
            self.head_unknown = false;
            self.executions = self.executions.saturating_add(1);
            return Expect::Deliver {
                recorded: true,
                stale: false,
                delayed,
            };
        }
        let stale = self
            .generations
            .get(&delivery.command.id)
            .is_some_and(|known| *known > delivery.command.generation);
        Expect::Deliver {
            recorded: false,
            stale,
            delayed,
        }
    }

    /// An injected observation: ingested once, a seen id ignored.
    fn inject(&mut self, id: u8, payload: u8) -> Expect {
        let observed = law_observed(id, payload);
        let duplicate = self.seen.contains(&observed.id);
        let late = duplicate && !self.inbox.iter().any(|pending| pending.id == observed.id);
        if !duplicate {
            self.seen.insert(observed.id.clone());
            self.inbox.push(observed);
        }
        Expect::Inject { duplicate, late }
    }

    /// The network's transitions (law 7) that the model's state admits,
    /// weighted by `mix`.
    fn network_strategy(&self, mix: &NetworkMix) -> Vec<(u32, BoxedStrategy<CellOp>)> {
        let deliver = |(command, input): (Command<LawCommand>, LawInput), route| {
            CellOp::Deliver(Box::new(Delivery {
                command,
                input,
                route,
            }))
        };
        let mut options = vec![
            (mix.dispatch, self.dispatch_strategy(false)),
            (mix.twin, self.dispatch_strategy(true)),
        ];
        let flying: Vec<(Command<LawCommand>, LawInput)> = self
            .in_flight
            .iter()
            .map(|(command, input, _)| (command.clone(), input.clone()))
            .collect();
        if let Some(oldest) = flying.first() {
            options.push((
                mix.deliver,
                Just(deliver(oldest.clone(), Route::InOrder)).boxed(),
            ));
            let count = u8::try_from(flying.len()).unwrap_or(u8::MAX);
            options.push((mix.lose, (0..count).prop_map(CellOp::Lose).boxed()));
        }
        if let Some(later) = flying.get(1..).filter(|later| !later.is_empty()) {
            options.push((
                mix.reorder,
                proptest::sample::select(later.to_vec())
                    .prop_map(move |message| deliver(message, Route::OutOfOrder))
                    .boxed(),
            ));
        }
        if !self.sent.is_empty() {
            options.push((
                mix.duplicate,
                proptest::sample::select(self.sent.clone())
                    .prop_map(move |message| deliver(message, Route::Duplicate))
                    .boxed(),
            ));
        }
        if let Some(reissue) = self.reissue_head() {
            options.push((mix.reissue, Just(CellOp::Commit(reissue)).boxed()));
        }
        let seen: Vec<u8> = (0..=LAW_UNIVERSE)
            .filter(|n| self.seen.contains(&law_observed(*n, 0).id))
            .collect();
        if !seen.is_empty() {
            options.push((
                mix.redeliver,
                (proptest::sample::select(seen), any::<u8>())
                    .prop_map(|(id, payload)| CellOp::Inject { id, payload })
                    .boxed(),
            ));
        }
        options
    }

    /// The expectation for `target` under the armed `plan`, applying to the
    /// model only what survives the fault (law 6).
    fn faulted(&mut self, target: &FaultTarget, plan: FaultPlan) -> (Expect, Firing) {
        match target {
            FaultTarget::Commit(commit) => {
                if plan.transaction != 0 {
                    return (Expect::Commit(self.commit(commit)), Firing::Never);
                }
                // A trial copy: a faulted commit applies nothing.
                match self.clone().commit(commit) {
                    Ok(_) => (Expect::Failed, Firing::Fires),
                    Err(refusal) => (Expect::Commit(Err(refusal)), Firing::Either),
                }
            }
            FaultTarget::Perform => match plan.transaction {
                0 if self.fresh_head() => (Expect::Failed, Firing::Fires),
                1 if self.fresh_head() => {
                    // The claim is durable and the outside world ran, but no
                    // result was recorded.
                    self.claim();
                    self.head_unknown = true;
                    (Expect::Failed, Firing::Fires)
                }
                _ => (self.perform(), Firing::Never),
            },
        }
    }

    /// A commit that leaves a fresh command at the outbox head, or `None`
    /// when the head already is one: it replaces a head whose result was
    /// lost with its next generation, or consumes a delivered head result
    /// (if any) and enqueues the next generation of a command.
    fn prepare_perform(&self) -> BoxedStrategy<Option<LawCommit>> {
        if self.fresh_head() {
            return Just(None).boxed();
        }
        // The generation after the newest one enqueued; 0 for a new command.
        let next = |id: &CommandId| {
            self.generations
                .get(id)
                .map_or(0, |known| known.0.saturating_add(1))
        };
        if let Some(reissue) = self.reissue_head() {
            return Just(Some(reissue)).boxed();
        }
        let consumes = self.outbox.first().map(|head| Consumed::Result {
            command: head.id.clone(),
            generation: head.generation,
        });
        let generations: Vec<u64> = (0..LAW_UNIVERSE)
            .map(|n| next(&CommandId(format!("c{n}"))))
            .collect();
        (0..LAW_UNIVERSE, any::<u8>())
            .prop_map(move |(n, payload)| {
                let generation = generations.get(usize::from(n)).copied().unwrap_or_default();
                Some(CellCommit {
                    consumes: consumes.clone(),
                    effects: vec![Effect::Perform(law_command(n, generation, payload))],
                    ..CellCommit::empty()
                })
            })
            .boxed()
    }

    fn next_timer(&self) -> Option<DueTimer<LawInput>> {
        self.timers
            .values()
            .filter(|due| due.timer.due.as_millis() <= self.now)
            .min_by(|a, b| (a.timer.due, &a.timer.id).cmp(&(b.timer.due, &b.timer.id)))
            .cloned()
    }

    fn snapshot(&self) -> Snapshot {
        Snapshot {
            facts: law_fact_keys()
                .iter()
                .map(|key| Ok(self.load(key)))
                .collect(),
            links: (0..LAW_DECLARED_LINKS)
                .map(|n| {
                    let canonical = self.with_model(|model| model.canonical_link(&law_link_key(n)));
                    Ok(self.links.get(&canonical).cloned().unwrap_or_default())
                })
                .collect(),
            clock: Ok(self.next_timer()),
            observe: Ok(self.inbox.first().cloned()),
            outbox: self.outbox.clone(),
            timers: self.timers.values().cloned().collect(),
            ledger: self.ledger.clone(),
            executions: self.executions,
        }
    }

    /// Deliveries a commit may consume: every pending one, plus stale and
    /// unknown references, plus `None` (weighted).
    fn consume_candidates(&self) -> Vec<Option<Consumed>> {
        let mut candidates = vec![None, None, None];
        candidates.extend(self.timers.values().map(|due| Some(due.consumed())));
        // A stale incarnation of each pending timer (an earlier schedule).
        candidates.extend(self.timers.values().map(|due| {
            Some(Consumed::Timer {
                id: due.timer.id.clone(),
                incarnation: LedgerSeq(due.incarnation.0.saturating_sub(1)),
            })
        }));
        if let Some(head) = self.outbox.first() {
            candidates.push(Some(Consumed::Result {
                command: head.id.clone(),
                generation: head.generation,
            }));
        }
        candidates.extend(self.inbox.iter().map(|observed| Some(observed.consumed())));
        candidates.push(Some(Consumed::Timer {
            id: law_timer_id(LAW_UNIVERSE),
            incarnation: LedgerSeq(0),
        }));
        candidates.push(Some(Consumed::Result {
            command: CommandId("c-unknown".to_owned()),
            generation: Generation(0),
        }));
        candidates.push(Some(Consumed::Observation {
            id: ObservationId("o-unknown".to_owned()),
        }));
        candidates
    }

    /// Up to two link writes over the law links, fresh or stale, adds or
    /// removes. Weighted toward the to-one `owner` link and fresh versions so
    /// `commit.link_cardinality` (which needs a commit to pass every earlier
    /// check) is reached; the undeclared `stray` link is rare.
    fn link_strategy(&self) -> BoxedStrategy<Vec<LinkWrite>> {
        let link_versions: Vec<FactVersion> = (0..LAW_LINKS)
            .map(|n| self.link_version(&law_link_key(n)))
            .collect();
        let pairs = LAW_UNIVERSE * LAW_UNIVERSE;
        let link = prop_oneof![
            3 => 0..pairs,
            4 => pairs..2 * pairs,
            1 => 2 * pairs..LAW_LINKS,
        ];
        proptest::collection::vec(
            (
                link,
                proptest::bool::weighted(0.8),
                0_u64..3,
                proptest::bool::weighted(0.7),
            ),
            0..3,
        )
        .prop_map(move |links| {
            links
                .into_iter()
                .map(|(n, fresh, stale, add)| LinkWrite {
                    key: law_link_key(n),
                    expected: if fresh {
                        link_versions
                            .get(usize::from(n))
                            .copied()
                            .unwrap_or(FactVersion::ABSENT)
                    } else {
                        FactVersion(stale)
                    },
                    mutation: if add {
                        LinkMutation::Add {
                            values: JsonObject::new(),
                        }
                    } else {
                        LinkMutation::Remove
                    },
                })
                .collect::<Vec<_>>()
        })
        .boxed()
    }

    /// One effect of a law commit: perform, schedule, or cancel.
    fn effect_strategy(&self) -> BoxedStrategy<Effect<LawCommand, LawInput>> {
        let now = self.now;
        prop_oneof![
            (0..LAW_UNIVERSE, 0_u64..3, any::<u8>()).prop_map(|(n, generation, payload)| {
                Effect::Perform(law_command(n, generation, payload))
            }),
            (0..LAW_UNIVERSE, 0_u64..20, any::<u8>()).prop_map(move |(n, delay, payload)| {
                Effect::Schedule(Timer {
                    id: law_timer_id(n),
                    due: LogicalTime::from_millis(now.saturating_add(delay)),
                    payload: LawInput::Timer(payload),
                })
            }),
            (0..LAW_UNIVERSE).prop_map(|n| Effect::CancelTimer(law_timer_id(n))),
        ]
        .boxed()
    }

    /// A commit built to pass every check before cardinality: it consumes
    /// nothing or a pending delivery, carries any effects, freshly Puts a
    /// source and its targets, and adds the to-one `owner` link from the
    /// source to one or two distinct targets. Two targets (or one beside a live `owner`) reach
    /// `commit.link_cardinality`; one is otherwise accepted. Generic commits
    /// seldom pass every earlier check, so this keeps the rarer accepted and
    /// cardinality outcomes covered for every host.
    fn admissible_commit_strategy(&self) -> BoxedStrategy<LawCommit> {
        let fact_versions: Vec<FactVersion> = (0..LAW_UNIVERSE)
            .map(|n| self.version(&law_key(n)))
            .collect();
        let link_versions: Vec<FactVersion> = (0..LAW_LINKS)
            .map(|n| self.link_version(&law_link_key(n)))
            .collect();
        let mut consumes: Vec<Option<Consumed>> = vec![None];
        consumes.extend(
            self.consume_candidates()
                .into_iter()
                .flatten()
                .filter(|consumed| self.pending(consumed))
                .map(Some),
        );
        let pairs = LAW_UNIVERSE * LAW_UNIVERSE;
        (
            proptest::sample::select(consumes),
            0..LAW_UNIVERSE,
            0..LAW_UNIVERSE,
            1..LAW_UNIVERSE,
            any::<bool>(),
            any::<u8>(),
            proptest::collection::vec(self.effect_strategy(), 0..3),
        )
            .prop_map(
                move |(consumes, source, first, step, two, value, effects)| {
                    let mut targets = vec![first];
                    if two {
                        targets.push(first.wrapping_add(step) % LAW_UNIVERSE);
                    }
                    let mut objects = targets.clone();
                    objects.push(source);
                    objects.sort_unstable();
                    objects.dedup();
                    let writes = objects
                        .into_iter()
                        .map(|n| FactWrite {
                            key: law_key(n),
                            expected: fact_versions
                                .get(usize::from(n))
                                .copied()
                                .unwrap_or(FactVersion::ABSENT),
                            mutation: FactMutation::Put {
                                values: law_values(value),
                            },
                        })
                        .collect();
                    let links = targets
                        .into_iter()
                        .map(|target| {
                            let n = pairs
                                .wrapping_add(target.wrapping_mul(LAW_UNIVERSE))
                                .wrapping_add(source);
                            LinkWrite {
                                key: law_link_key(n),
                                expected: link_versions
                                    .get(usize::from(n))
                                    .copied()
                                    .unwrap_or(FactVersion::ABSENT),
                                mutation: LinkMutation::Add {
                                    values: JsonObject::new(),
                                },
                            }
                        })
                        .collect();
                    CellCommit {
                        consumes,
                        reads: Vec::new(),
                        writes,
                        events: Vec::new(),
                        effects,
                        links,
                        link_reads: Vec::new(),
                        predicate_reads: Vec::new(),
                    }
                },
            )
            .boxed()
    }

    fn commit_strategy(&self) -> BoxedStrategy<LawCommit> {
        let versions: Vec<FactVersion> = (0..LAW_UNIVERSE)
            .map(|n| self.version(&law_key(n)))
            .collect();
        // Sometimes through the base type: `law.Thing/k2` is `law.Child/k2`.
        let expectation = (
            0..LAW_UNIVERSE,
            any::<bool>(),
            0_u64..3,
            proptest::bool::weighted(0.2),
        )
            .prop_map(move |(n, fresh, stale, alias)| {
                let expected = if fresh {
                    versions
                        .get(usize::from(n))
                        .copied()
                        .unwrap_or(FactVersion::ABSENT)
                } else {
                    FactVersion(stale)
                };
                (if alias { law_alias(n) } else { law_key(n) }, expected)
            });
        let reads = proptest::collection::vec(expectation.clone(), 0..2).prop_map(|reads| {
            reads
                .into_iter()
                .map(|(key, expected)| FactExpectation { key, expected })
                .collect::<Vec<_>>()
        });
        let writes = proptest::collection::vec(
            (expectation, proptest::option::weighted(0.8, any::<u8>())),
            0..3,
        )
        .prop_map(|writes| {
            writes
                .into_iter()
                .map(|((key, expected), value)| FactWrite {
                    key,
                    expected,
                    mutation: value.map_or(FactMutation::Delete, |value| FactMutation::Put {
                        values: law_values(value),
                    }),
                })
                .collect::<Vec<_>>()
        });
        let links = self.link_strategy();
        let effect = self.effect_strategy();
        (
            proptest::sample::select(self.consume_candidates()),
            reads,
            writes,
            proptest::collection::vec(any::<u8>(), 0..3),
            proptest::collection::vec(effect, 0..3),
            links,
        )
            .prop_map(
                |(consumes, reads, writes, events, effects, links)| CellCommit {
                    consumes,
                    reads,
                    writes,
                    events,
                    effects,
                    links,
                    link_reads: Vec::new(),
                    predicate_reads: Vec::new(),
                },
            )
            .boxed()
    }
}

/// One case's transition mix (swarm testing): the relative weight of each
/// operation kind, 0 when the case never generates it. Drawn once per case
/// for hosts that inject faults, never shrunk, and printed with a failure.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Swarm {
    commit: u32,
    admissible_commit: u32,
    perform: u32,
    observe: u32,
    clock: u32,
    advance: u32,
    inject: u32,
    io_error: u32,
    crash: u32,
    commit_fails: u32,
    /// The network's mix, for hosts that declare one (law 7).
    network: Option<NetworkMix>,
}

/// The weights of the network's transitions in one case (law 7). Each is
/// always enabled, at weight 1 to 4: the interleavings that reach a stale or
/// duplicate result take several of them in a row.
#[derive(Debug, Clone, PartialEq, Eq)]
struct NetworkMix {
    dispatch: u32,
    twin: u32,
    deliver: u32,
    reorder: u32,
    duplicate: u32,
    lose: u32,
    reissue: u32,
    redeliver: u32,
}

impl NetworkMix {
    fn strategy() -> impl Strategy<Value = Self> {
        let weight = || 1_u32..5;
        (
            weight(),
            weight(),
            weight(),
            weight(),
            weight(),
            weight(),
            weight(),
            weight(),
        )
            .prop_map(
                |(dispatch, twin, deliver, reorder, duplicate, lose, reissue, redeliver)| Self {
                    dispatch,
                    twin,
                    deliver,
                    reorder,
                    duplicate,
                    lose,
                    reissue,
                    redeliver,
                },
            )
    }
}

impl Swarm {
    /// Each kind is enabled with probability 3/4 at weight 1 to 4; faults the
    /// host does not list stay disabled, and at least one cell operation
    /// is enabled. Commit failures take the I/O-error weight: no extra draw,
    /// so the mix of a host without them is unchanged. The network's mix is
    /// drawn after the rest, and only for a host that declares one.
    fn strategy(kinds: &'static [FaultKind], network: bool) -> BoxedStrategy<Self> {
        let base = Self::base(kinds);
        if network {
            (base, NetworkMix::strategy())
                .prop_map(|(base, mix)| Self {
                    network: Some(mix),
                    ..base
                })
                .no_shrink()
                .boxed()
        } else {
            base.no_shrink().boxed()
        }
    }

    fn base(kinds: &'static [FaultKind]) -> impl Strategy<Value = Self> {
        let weight =
            || proptest::option::weighted(0.75, 1_u32..5).prop_map(Option::unwrap_or_default);
        let operations = (
            weight(),
            weight(),
            weight(),
            weight(),
            weight(),
            weight(),
            weight(),
        );
        let faults = (weight(), weight());
        (operations, faults).prop_map(move |(operations, (io_error, crash))| {
            let (commit, admissible_commit, perform, observe, clock, advance, inject) = operations;
            let enabled = |kind: FaultKind, weight: u32| {
                if kinds.contains(&kind) { weight } else { 0 }
            };
            let operating = [
                commit,
                admissible_commit,
                perform,
                observe,
                clock,
                advance,
                inject,
            ]
            .iter()
            .any(|weight| *weight > 0);
            Self {
                commit: if operating { commit } else { 1 },
                admissible_commit,
                perform,
                observe,
                clock,
                advance,
                inject,
                io_error: enabled(FaultKind::IoError, io_error),
                crash: enabled(FaultKind::Crash, crash),
                commit_fails: enabled(FaultKind::CommitFails, io_error),
                network: None,
            }
        })
    }

    fn transitions(&self, model: &CellModel) -> BoxedStrategy<CellOp> {
        let mut options: Vec<(u32, BoxedStrategy<CellOp>)> = Vec::new();
        if self.commit > 0 {
            options.push((
                self.commit,
                model.commit_strategy().prop_map(CellOp::Commit).boxed(),
            ));
        }
        if self.admissible_commit > 0 {
            options.push((
                self.admissible_commit,
                model
                    .admissible_commit_strategy()
                    .prop_map(CellOp::Commit)
                    .boxed(),
            ));
        }
        if self.perform > 0 {
            options.push((self.perform, Just(CellOp::Perform).boxed()));
        }
        if self.observe > 0 {
            options.push((self.observe, Just(CellOp::Observe).boxed()));
        }
        if self.clock > 0 {
            options.push((self.clock, Just(CellOp::Clock).boxed()));
        }
        if self.advance > 0 {
            options.push((self.advance, (0_u64..12).prop_map(CellOp::Advance).boxed()));
        }
        if self.inject > 0 {
            options.push((
                self.inject,
                (0..=LAW_UNIVERSE, any::<u8>())
                    .prop_map(|(id, payload)| CellOp::Inject { id, payload })
                    .boxed(),
            ));
        }
        for (weight, kind) in [
            (self.io_error, FaultKind::IoError),
            (self.crash, FaultKind::Crash),
            (self.commit_fails, FaultKind::CommitFails),
        ] {
            if weight > 0 {
                options.push((weight, fault_strategy(model, kind)));
            }
        }
        if let Some(mix) = &self.network {
            options.extend(model.network_strategy(mix));
        }
        Union::new_weighted(options).boxed()
    }
}

/// A fault of `kind`, half on a commit, half on a perform, mostly at an
/// early statement and sometimes past the last (the commit point). A commit
/// fault mostly hits its one write transaction and sometimes the second it
/// must not run; a perform fault hits its claim or its result record equally
/// often, after a preparing commit when the head is not fresh. With a head
/// whose result was lost, half the time a plain perform is generated
/// instead, to check it is never executed again.
fn fault_strategy(model: &CellModel, kind: FaultKind) -> BoxedStrategy<CellOp> {
    let statement = || prop_oneof![3 => 0_u8..16, 1 => any::<u8>()];
    let faulted = move |transaction: u8,
                        at_statement: u8,
                        prepare: Option<LawCommit>,
                        target: FaultTarget| {
        CellOp::Faulted(Box::new(Faulted {
            plan: FaultPlan {
                kind,
                transaction,
                at_statement,
            },
            prepare,
            target,
        }))
    };
    let commit = (
        prop_oneof![4 => Just(0_u8), 1 => Just(1_u8)],
        statement(),
        prop_oneof![model.admissible_commit_strategy(), model.commit_strategy()],
    )
        .prop_map(move |(transaction, at_statement, commit)| {
            faulted(
                transaction,
                at_statement,
                None,
                FaultTarget::Commit(Box::new(commit)),
            )
        });
    let perform = (
        prop_oneof![Just(0_u8), Just(1_u8)],
        statement(),
        model.prepare_perform(),
    )
        .prop_map(move |(transaction, at_statement, prepare)| {
            faulted(transaction, at_statement, prepare, FaultTarget::Perform)
        });
    if model.head_unknown {
        prop_oneof![
            commit,
            perform,
            Just(CellOp::Perform),
            Just(CellOp::Perform)
        ]
        .boxed()
    } else {
        prop_oneof![commit, perform].boxed()
    }
}

/// Reference-machine state: the model plus the expectation for the most
/// recently applied operation.
#[derive(Debug, Clone, Default)]
pub struct CellReference {
    model: CellModel,
    expected: Option<Expect>,
    /// The expectation for the preparing commit of the most recently
    /// applied [`Faulted`] transition.
    prepared: Option<Expect>,
    /// The fault the most recently applied operation ran under.
    fault: Option<ArmedFault>,
    /// The expectations of the most recently applied [`Dispatching`], in
    /// order.
    chain: Vec<Expect>,
    /// The case's transition mix; `None` keeps the fixed mix.
    swarm: Option<Swarm>,
}

/// The reference and concrete `proptest-state-machine` for host `H`.
pub struct CellHostMachine<H>(PhantomData<fn() -> H>);

impl<H: SimulatedCell> ReferenceStateMachine for CellHostMachine<H> {
    type State = CellReference;
    type Transition = CellOp;

    fn init_state() -> BoxedStrategy<CellReference> {
        let model = CellModel {
            commits_perform_claims: H::COMMITS_PERFORM_CLAIMS,
            ..CellModel::default()
        };
        if H::FAULT_KINDS.is_empty() && !H::NETWORK {
            return Just(CellReference {
                model,
                ..CellReference::default()
            })
            .boxed();
        }
        Swarm::strategy(H::FAULT_KINDS, H::NETWORK)
            .prop_map(move |swarm| CellReference {
                model: model.clone(),
                swarm: Some(swarm),
                ..CellReference::default()
            })
            .boxed()
    }

    fn transitions(state: &CellReference) -> BoxedStrategy<CellOp> {
        if let Some(swarm) = &state.swarm {
            return swarm.transitions(&state.model);
        }
        prop_oneof![
            5 => state.model.commit_strategy().prop_map(CellOp::Commit),
            2 => state.model.admissible_commit_strategy().prop_map(CellOp::Commit),
            2 => Just(CellOp::Perform),
            1 => Just(CellOp::Observe),
            2 => Just(CellOp::Clock),
            2 => (0_u64..12).prop_map(CellOp::Advance),
            2 => (0..=LAW_UNIVERSE, any::<u8>()).prop_map(|(id, payload)| CellOp::Inject { id, payload }),
        ]
        .boxed()
    }

    fn apply(mut state: CellReference, op: &CellOp) -> CellReference {
        state.fault = None;
        state.prepared = None;
        state.chain = Vec::new();
        let model = &mut state.model;
        model.steps = model.steps.saturating_add(1);
        let expected = match op {
            CellOp::Commit(commit) => Expect::Commit(model.commit(commit)),
            CellOp::Perform => model.perform(),
            CellOp::Observe => Expect::Observe(model.inbox.first().cloned()),
            CellOp::Clock => Expect::Clock(model.next_timer()),
            CellOp::Advance(millis) => {
                model.now = model.now.saturating_add(*millis);
                Expect::Nothing
            }
            CellOp::Inject { id, payload } => model.inject(*id, *payload),
            CellOp::Dispatch(dispatching) => {
                state.chain = model.dispatching(dispatching);
                Expect::Nothing
            }
            CellOp::Deliver(delivery) => model.deliver(delivery),
            CellOp::Lose(ix) => {
                if usize::from(*ix) < model.in_flight.len() {
                    model.in_flight.remove(usize::from(*ix));
                }
                Expect::Nothing
            }
            CellOp::Faulted(faulted) => {
                state.prepared = faulted
                    .prepare
                    .as_ref()
                    .map(|commit| Expect::Commit(model.commit(commit)));
                let (expected, firing) = model.faulted(&faulted.target, faulted.plan);
                state.fault = Some(ArmedFault {
                    plan: faulted.plan,
                    firing,
                });
                expected
            }
        };
        state.expected = Some(expected);
        state
    }
}

fn snapshot<H: SimulatedCell>(host: &mut H) -> Snapshot {
    Snapshot {
        facts: law_fact_keys()
            .iter()
            .map(|key| host.load(key).map_err(|error| error.to_string()))
            .collect(),
        links: (0..LAW_DECLARED_LINKS)
            .map(|n| {
                host.load_link(&law_link_key(n))
                    .map_err(|error| error.to_string())
            })
            .collect(),
        clock: host.clock().map_err(|error| error.to_string()),
        observe: host.observe().map_err(|error| error.to_string()),
        outbox: host.sim_outbox(),
        timers: host.sim_timers(),
        ledger: host.sim_ledger(),
        executions: host.sim_executions(),
    }
}

/// Rebuild every listed field-only variant with an uninhabited host error.
/// The generated `match` is exhaustive, so a new [`CommitError`] variant
/// fails to compile until it is listed here.
macro_rules! rehost {
    ($error:expr; $($variant:ident $({ $($field:ident),* })?),* $(,)?) => {
        match $error {
            $(norn_kernel::CommitError::$variant $({ $($field),* })? =>
                norn_kernel::CommitError::$variant $({ $($field),* })?,)*
            norn_kernel::CommitError::Host(error) => return Err(error),
        }
    };
}

/// The refusal without its host error, or the host error itself.
fn without_host<X>(error: CommitError<X>) -> Result<CommitError<Infallible>, X> {
    Ok(rehost!(error;
        IdentityReuse, AtomicBoundary { key }, AtomicDeliveryBoundary { consumed }, ImmutableObject { key },
        Law { key, reason }, IdentityCollision { owner, key, conflicts },
        OwnedRowsProtected { key }, DuplicateKey { key }, NotPending { consumed },
        Conflict { key, expected, current }, DuplicateLink { key },
        IdentifierTooLong { field, actual_bytes, max_bytes },
        LinkConflict { key, expected, current }, LinkReadConflict { source_key, link },
        UnsupportedLinkRead { source_key, link }, DuplicateLinkRead { source_key, link },
        LinkReadTooLarge { source_key, link, actual, max }, InvalidLinkRead { source_key, link },
        PredicateReadConflict { object_type }, UnsupportedPredicateRead { object_type },
        InvalidPredicateRead { object_type, reason }, DanglingLink { key, missing },
        LinkTarget { key }, LinkCardinality { key, endpoint }, LinkRequired { parent, link },
        TypeMismatch { key, concrete }, EffectiveValues { key, reason },
        TraversalLimitExceeded { key, reason }, AccountingPosting { key, reason },
        InvariantViolation { key, owner, version, id, message, outcome },
        InvariantBudgetExceeded { key, limit }, InvariantModel { key, interface },
        ObservationAuthorization, EventLimitExceeded { limit, actual }, Overflow,
    ))
}

fn consumed_label(consumed: Option<&Consumed>) -> &'static str {
    match consumed {
        None => "commit.accepted",
        Some(Consumed::Timer { .. }) => "commit.accepted.consumed_timer",
        Some(Consumed::Result { .. }) => "commit.accepted.consumed_result",
        Some(Consumed::Observation { .. }) => "commit.accepted.consumed_observation",
    }
}

/// Disarms the host's fault and checks it fired exactly when the model says
/// the operation runs the faulted write transaction (law 6).
fn fault_fired<H: SimulatedCell>(host: &mut H, fault: ArmedFault) -> bool {
    let fired = host.sim_disarm_fault();
    match fault.firing {
        Firing::Fires => assert!(
            fired,
            "law violated: a fault armed for a write transaction the operation runs did not fire\nfault: {:?}",
            fault.plan
        ),
        Firing::Never => assert!(
            !fired,
            "law violated: a fault fired in a write transaction the operation must not run (a commit is one transaction, a fresh perform two)\nfault: {:?}",
            fault.plan
        ),
        Firing::Either => {}
    }
    let label = if fired {
        match fault.plan.kind {
            FaultKind::Crash => "fault.crash",
            FaultKind::IoError => "fault.io_error",
            FaultKind::CommitFails => "fault.commit_failed",
        }
    } else {
        "fault.unfired"
    };
    coverage::hit_prefixed("cell", label);
    fired
}

fn apply_commit<H: SimulatedCell>(
    host: &mut H,
    commit: &LawCommit,
    expected: &Expect,
    fault: Option<ArmedFault>,
) {
    let before = snapshot(host);
    if let Some(fault) = fault {
        host.sim_arm_fault(fault.plan);
    }
    // A host failure is reported as its message and can never equal the
    // model's expectation.
    let actual = match host.commit(commit.clone()) {
        Ok(receipt) => Ok(Ok(receipt)),
        Err(error) => without_host(error)
            .map(Err)
            .map_err(|error| error.to_string()),
    };
    if fault.is_some_and(|fault| fault_fired(host, fault)) {
        coverage::hit_prefixed("cell", "fault.commit");
        // Refinement against the unchanged model follows.
        assert!(
            actual.is_err(),
            "law violated: a faulted commit did not fail with a host error\ncommit: {commit:?}\noutcome: {actual:?}"
        );
        return;
    }
    assert_eq!(
        actual.clone().map(Expect::Commit),
        Ok(expected.clone()),
        "law violated: commit outcome differs from check-then-apply\ncommit: {commit:?}"
    );
    match actual {
        Ok(Ok(receipt)) => {
            coverage::hit_prefixed("cell", consumed_label(commit.consumes.as_ref()));
            if !receipt.linked.is_empty() {
                coverage::hit_prefixed("cell", "commit.accepted.linked");
            }
            let published = host.publish(&receipt).map_err(|error| error.to_string());
            assert_eq!(published, Ok(()), "publish of an accepted commit failed");
        }
        Ok(Err(refusal)) => {
            coverage::hit_prefixed("cell", refusal.code());
            assert_eq!(
                snapshot(host),
                before,
                "law violated: a refused commit changed the cell\ncommit: {commit:?}"
            );
        }
        Err(_) => {}
    }
}

impl<H: SimulatedCell> StateMachineTest for CellHostMachine<H> {
    type SystemUnderTest = H;
    type Reference = Self;

    fn init_test(_: &CellReference) -> H {
        H::sim_new()
    }

    fn apply(mut host: H, reference: &CellReference, op: CellOp) -> H {
        let expected = reference.expected.clone().unwrap_or(Expect::Nothing);
        match op {
            CellOp::Commit(commit) => apply_commit(&mut host, &commit, &expected, None),
            CellOp::Perform => apply_perform(&mut host, expected, None),
            CellOp::Observe => {
                let observed = host.observe().map_err(|error| error.to_string());
                if matches!(observed, Ok(Some(_))) {
                    coverage::hit_prefixed("cell", "observe.delivered");
                }
                assert_eq!(
                    observed.map(Expect::Observe),
                    Ok(expected),
                    "law violated: observe delivered the wrong observation"
                );
            }
            CellOp::Clock => {
                let due = host.clock().map_err(|error| error.to_string());
                if matches!(due, Ok(Some(_))) {
                    coverage::hit_prefixed("cell", "clock.due");
                }
                assert_eq!(
                    due.map(Expect::Clock),
                    Ok(expected),
                    "law violated: clock delivered the wrong timer"
                );
            }
            CellOp::Advance(_) => {
                host.sim_advance_to(LogicalTime::from_millis(reference.model.now));
            }
            CellOp::Inject { id, payload } => {
                host.sim_inject(law_observed(id, payload));
                if let Expect::Inject { duplicate, late } = expected {
                    match (duplicate, late) {
                        (true, true) => coverage::hit_prefixed("cell", "observe.late_ignored"),
                        (true, false) => {
                            coverage::hit_prefixed("cell", "observe.duplicate_ignored");
                        }
                        (false, _) => {}
                    }
                }
            }
            CellOp::Dispatch(dispatching) => {
                apply_dispatching(&mut host, &dispatching, &reference.chain);
            }
            CellOp::Deliver(_) | CellOp::Lose(_) => apply_network(&mut host, &op, expected),
            CellOp::Faulted(faulted) => {
                let Faulted {
                    prepare, target, ..
                } = *faulted;
                if let Some(commit) = prepare {
                    let prepared = reference.prepared.clone().unwrap_or(Expect::Nothing);
                    apply_commit(&mut host, &commit, &prepared, None);
                }
                match target {
                    FaultTarget::Commit(commit) => {
                        apply_commit(&mut host, &commit, &expected, reference.fault);
                    }
                    FaultTarget::Perform => apply_perform(&mut host, expected, reference.fault),
                }
            }
        }
        Self::refines(host, reference)
    }
}

/// A [`Dispatching`] (law 7): its commits as commits, and each dispatch
/// claims exactly what the model predicts.
fn apply_dispatching<H: SimulatedCell>(host: &mut H, dispatching: &Dispatching, chain: &[Expect]) {
    let mut expected = chain.iter().cloned();
    let mut next = || expected.next().unwrap_or(Expect::Nothing);
    if let Some(commit) = &dispatching.prepare {
        apply_commit(host, commit, &next(), None);
    }
    dispatch_once(host, next());
    if let Some(commit) = &dispatching.reissue {
        apply_commit(host, commit, &next(), None);
        dispatch_once(host, next());
    }
}

fn dispatch_once<H: SimulatedCell>(host: &mut H, expected: Expect) {
    let dispatched = host.sim_dispatch();
    let label = if matches!(dispatched, Ok(Some(_))) {
        "network.dispatched"
    } else {
        "network.dispatch_idle"
    };
    coverage::hit_prefixed("cell", label);
    assert_eq!(
        dispatched.map(Expect::Dispatch),
        Ok(expected),
        "law violated: dispatch claims a command other than the fresh head"
    );
}

/// One network transition (law 7): a delivery is recorded exactly when the
/// model records it, and a loss is the network's alone.
fn apply_network<H: SimulatedCell>(host: &mut H, op: &CellOp, expected: Expect) {
    match op {
        CellOp::Deliver(delivery) => {
            let recorded = host.sim_deliver(&delivery.command, &delivery.input);
            if let Expect::Deliver { stale, delayed, .. } = expected {
                let outcome = match (&recorded, stale) {
                    (Ok(true), _) => "network.delivered",
                    (_, true) => "result.stale_refused",
                    (_, false) => "network.duplicate_ignored",
                };
                coverage::hit_prefixed("cell", outcome);
                if delayed {
                    coverage::hit_prefixed("cell", "network.delayed");
                }
            }
            match delivery.route {
                Route::InOrder => {}
                Route::OutOfOrder => coverage::hit_prefixed("cell", "network.out_of_order"),
                Route::Duplicate => coverage::hit_prefixed("cell", "network.duplicate"),
            }
            let expected_recorded = matches!(expected, Expect::Deliver { recorded: true, .. });
            assert_eq!(
                recorded,
                Ok(expected_recorded),
                "law violated: a result is recorded only as the current head's first result at its generation\ndelivery: {delivery:?}"
            );
        }
        CellOp::Lose(_) => coverage::hit_prefixed("cell", "network.dropped"),
        CellOp::Dispatch(_)
        | CellOp::Commit(_)
        | CellOp::Perform
        | CellOp::Observe
        | CellOp::Clock
        | CellOp::Advance(_)
        | CellOp::Inject { .. }
        | CellOp::Faulted(_) => {}
    }
}

/// One `perform`, under `fault` if armed: exactly once per generation, and
/// never delivered or executed when the model expects it to fail.
fn apply_perform<H: SimulatedCell>(host: &mut H, expected: Expect, fault: Option<ArmedFault>) {
    let executions = host.sim_executions();
    if let Some(fault) = fault {
        host.sim_arm_fault(fault.plan);
    }
    let delivered = host.perform().map_err(|error| error.to_string());
    let fired = fault.is_some_and(|fault| fault_fired(host, fault));
    let executed = host.sim_executions() != executions;
    if expected == Expect::Failed {
        let label = match fault {
            Some(fault) if fired && fault.plan.transaction == 0 => "fault.perform.claim",
            Some(_) if fired => "fault.perform.result",
            Some(_) | None => "perform.outcome_unknown",
        };
        coverage::hit_prefixed("cell", label);
        assert!(
            delivered.is_err() && !executed,
            "law violated: a perform whose claim or result is lost delivered or executed\noutcome: {delivered:?}, executed: {executed}"
        );
        return;
    }
    let label = match (&delivered, executed) {
        (Ok(None), _) => "perform.empty",
        (_, true) => "perform.executed",
        (_, false) => "perform.redelivered",
    };
    coverage::hit_prefixed("cell", label);
    assert_eq!(
        delivered.map(|delivered| Expect::Perform {
            delivered,
            executed
        }),
        Ok(expected),
        "law violated: perform is not exactly-once per generation"
    );
}

impl<H: SimulatedCell> CellHostMachine<H> {
    /// Law 5: the host's state equals the model's.
    fn refines(mut host: H, reference: &CellReference) -> H {
        assert_eq!(
            snapshot(&mut host),
            reference.model.snapshot(),
            "host state does not refine the cell model"
        );
        host
    }
}

/// The base type of [`override_object_types`].
pub const OVERRIDE_BASE: &str = "override.Base";
/// The subtype of [`override_object_types`].
pub const OVERRIDE_CHILD: &str = "override.Child";

/// A vocabulary whose subtype narrows an inherited link (#3018 review):
/// `override.Base` declares `owner` (many, to `override.Base`);
/// `override.Child` extends it and overrides `owner` as to-one, to
/// `override.Child`.
#[must_use]
pub fn override_object_types() -> Vec<ObjectTypeDef> {
    let owner = |target: &str, to: Multiplicity| Link {
        id: LinkId(LAW_OWNER.to_owned()),
        description: None,
        target: ObjectTypeName(target.to_owned()),
        cardinality: LinkCardinality {
            to,
            from: Multiplicity::Many,
            required: false,
        },
        inverse: None,
        acyclic: false,
        properties: Vec::new(),
    };
    let object_type = |name: &str, extends: Option<&str>, link: Link| ObjectTypeDef {
        immutable: false,
        role: None,
        name: ObjectTypeName(name.to_owned()),
        description: None,
        status: ResourceStatus::default(),
        extends: extends.map(|name| ObjectTypeName(name.to_owned())),
        implements: Vec::new(),
        markings: Vec::new(),
        properties: Vec::new(),
        links: vec![link],
        owned_collections: Vec::new(),
        identity: Vec::new(),
        invariants: Vec::new(),
        lifecycles: Vec::new(),
        accounting_leg: None,
    };
    vec![
        object_type(
            OVERRIDE_BASE,
            None,
            owner(OVERRIDE_BASE, Multiplicity::Many),
        ),
        object_type(
            OVERRIDE_CHILD,
            Some(OVERRIDE_BASE),
            owner(OVERRIDE_CHILD, Multiplicity::One),
        ),
    ]
}

/// A narrowing override binds however its source is named (#3018 review):
/// on a fresh host over [`override_object_types`], a `Child` source named
/// as `Base` is held to `Child.owner` — its `Base` target is refused
/// (`commit.link_target`), two targets are refused
/// (`commit.link_cardinality`), and one `Child` target commits — while a
/// `Base` source keeps the wide `Base.owner`.
///
/// # Panics
///
/// If the host admits or refuses differently.
pub fn check_link_override<H: CellHost>(cell: &mut H)
where
    H::Error: std::fmt::Debug,
{
    let put = |object_type: &str, key: &str| FactWrite {
        key: FactKey::new(object_type, key),
        expected: FactVersion::ABSENT,
        mutation: FactMutation::Put {
            values: JsonObject::new(),
        },
    };
    let created = cell.commit(CellCommit {
        writes: vec![
            put(OVERRIDE_CHILD, "s"),
            put(OVERRIDE_BASE, "t"),
            put(OVERRIDE_CHILD, "u"),
            put(OVERRIDE_CHILD, "v"),
            put(OVERRIDE_BASE, "b"),
        ],
        ..CellCommit::empty()
    });
    assert!(created.is_ok(), "creates the objects: {created:?}");
    let owner = |source: &str, target: &str| LinkWrite {
        key: LinkKey::new(
            FactKey::new(OVERRIDE_BASE, source),
            LAW_OWNER,
            FactKey::new(OVERRIDE_BASE, target),
        ),
        expected: FactVersion::ABSENT,
        mutation: LinkMutation::Add {
            values: JsonObject::new(),
        },
    };
    let refused = |cell: &mut H, links: Vec<LinkWrite>| {
        cell.commit(CellCommit {
            links,
            ..CellCommit::empty()
        })
        .map(|_| ())
        .map_err(|error| error.code())
    };
    assert_eq!(
        refused(cell, vec![owner("s", "t")]),
        Err("commit.link_target"),
        "Child.owner targets override.Child, even through a Base name"
    );
    assert_eq!(
        refused(cell, vec![owner("s", "u"), owner("s", "v")]),
        Err("commit.link_cardinality"),
        "Child.owner is to-one, even through a Base name"
    );
    assert_eq!(refused(cell, vec![owner("s", "u")]), Ok(()));
    assert_eq!(
        refused(cell, vec![owner("b", "t"), owner("b", "u")]),
        Ok(()),
        "Base.owner stays many, to override.Base"
    );
}

/// Coverage labels a healthy run of the law machine hits.
pub const CELL_HOST_LABELS: &[&str] = &[
    "cell:commit.accepted",
    "cell:commit.accepted.consumed_timer",
    "cell:commit.accepted.consumed_result",
    "cell:commit.accepted.consumed_observation",
    "cell:commit.conflict",
    "cell:commit.not_pending",
    "cell:commit.duplicate_key",
    "cell:commit.accepted.linked",
    "cell:commit.link_conflict",
    "cell:commit.dangling_link",
    "cell:commit.duplicate_link",
    "cell:commit.link_target",
    "cell:commit.link_cardinality",
    "cell:commit.type_mismatch",
    "cell:perform.executed",
    "cell:perform.redelivered",
    "cell:perform.empty",
    "cell:observe.delivered",
    "cell:clock.due",
];

/// Coverage labels a healthy run with injected faults hits (law 6): a fault
/// fired in a commit, in a perform's claim, and in its result record; a
/// fault the operation never reached; and a perform refused after its result
/// was lost. Per-kind labels are `cell:fault.io_error`, `cell:fault.crash`
/// and `cell:fault.commit_failed`.
pub const CELL_FAULT_LABELS: &[&str] = &[
    "cell:fault.commit",
    "cell:fault.perform.claim",
    "cell:fault.perform.result",
    "cell:fault.unfired",
    "cell:perform.outcome_unknown",
];

/// Coverage labels a healthy run under responder faults hits (law 7): every
/// network transition (dispatch, in-order, out-of-order, delayed and
/// duplicated delivery, loss), a stale-generation result refused, a
/// duplicate result ignored, and observations redelivered before and after
/// their consumption.
pub const CELL_NETWORK_LABELS: &[&str] = &[
    "cell:network.dispatched",
    "cell:network.delivered",
    "cell:network.out_of_order",
    "cell:network.delayed",
    "cell:network.duplicate",
    "cell:network.dropped",
    "cell:network.duplicate_ignored",
    "cell:result.stale_refused",
    "cell:observe.duplicate_ignored",
    "cell:observe.late_ignored",
];

/// Runs the `CellHost` laws against host `H` and returns the coverage, or the
/// minimal counterexample. For a host that injects faults, the failure
/// reason also names the case's transition mix and the run's seed.
///
/// # Errors
///
/// Returns the shrunk [`Failure`] if any law fails.
pub fn run_cell_host<H: SimulatedCell>(settings: &Settings) -> Result<Coverage, Failure<CellOp>> {
    const {
        assert!(
            H::FAULT_KINDS.is_empty() || H::COMMITS_PERFORM_CLAIMS,
            "a host that injects faults must commit perform claims (law 6)"
        );
        assert!(
            !H::NETWORK || H::COMMITS_PERFORM_CLAIMS,
            "a host behind a network must commit perform claims (law 7)"
        );
    }
    let config = settings.config();
    let mut runner = TestRunner::new(config.clone());
    let strategy = CellHostMachine::<H>::sequential_strategy(settings.transition_range());
    coverage::reset();
    let result = runner.run(&strategy, |(initial, transitions, seen)| {
        CellHostMachine::<H>::test_sequential(config.clone(), initial, transitions, seen);
        Ok(())
    });
    let counts = coverage::take();
    match result {
        Ok(()) => Ok(counts),
        Err(TestError::Fail(reason, (initial, transitions, _))) => Err(Failure {
            reason: match &initial.swarm {
                None => reason.to_string(),
                Some(swarm) => {
                    let seed = config.rng_seed;
                    format!(
                        "{reason}\nswarm: {swarm:?}\nseed: {seed} (replay the run with PROPTEST_RNG_SEED={seed})"
                    )
                }
            },
            transitions,
        }),
        Err(TestError::Abort(reason)) => Err(Failure {
            reason: format!("run aborted: {reason}"),
            transitions: Vec::new(),
        }),
    }
}

/// Runs the laws over one fixed `transitions` sequence from a fresh host,
/// for a mutant self-test whose counterexample is known: generation alone
/// may reach it too rarely. Returns the first violation's message.
///
/// # Errors
///
/// The violation, as the law machine's panic message.
pub fn run_cell_host_sequence<H: SimulatedCell>(transitions: Vec<CellOp>) -> Result<(), String> {
    let initial = CellReference {
        model: CellModel {
            commits_perform_claims: H::COMMITS_PERFORM_CLAIMS,
            ..CellModel::default()
        },
        ..CellReference::default()
    };
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        CellHostMachine::<H>::test_sequential(
            proptest::test_runner::Config::default(),
            initial,
            transitions,
            None,
        );
    }))
    .map_err(|panic| {
        panic
            .downcast_ref::<String>()
            .cloned()
            .or_else(|| panic.downcast_ref::<&str>().map(|text| (*text).to_owned()))
            .unwrap_or_default()
    })
}

/// [`run_cell_host`], panicking with the minimal counterexample on failure.
///
/// # Panics
///
/// Panics if any law fails.
#[must_use = "assert the coverage with `Coverage::assert_covered`"]
#[track_caller]
#[expect(
    clippy::panic,
    reason = "test-support entry point: a failed property must fail the calling test"
)]
pub fn check_cell_host<H: SimulatedCell>(settings: &Settings) -> Coverage {
    match run_cell_host::<H>(settings) {
        Ok(coverage) => coverage,
        Err(failure) => panic!("{failure}"),
    }
}
