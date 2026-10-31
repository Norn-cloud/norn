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
//!    fresh execution.
//! 4. **Clock.** `clock` delivers only due timers, earliest first (ties by
//!    id); rescheduling a delivered timer (even identically) makes its old
//!    delivery unconsumable, because the delivery names the incarnation.
//! 5. **Refinement.** After every operation every object's and every law
//!    link's committed state, the full ledger (positions, consumption, written versions, events), the
//!    pending commands and timers, the next due timer, the next
//!    observation, and the execution count equal the model's.
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
    Performed, Provenance, Timer, TimerId, TypeCatalog, WrittenLinkVersion, WrittenVersion,
    check_commit, check_commit_observed, check_commit_owned_rows,
};
use proptest::prelude::*;
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
            .with_interfaces(&self.interfaces)
            .with_property_types(&self.property_types);
        let canonical = CommitModel::new(&catalog).canonical_link(key);
        Ok(self.links.get(&canonical).cloned().unwrap_or_default())
    }

    fn read_link_targets(
        &self,
        source: &FactKey,
        link: &str,
        max_targets: usize,
        max_key_bytes: u64,
    ) -> Result<Option<Vec<LinkReadTarget>>, Infallible> {
        let catalog = TypeCatalog::new(&self.types)
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

/// A host the law machine can drive: a [`CellHost`] over the law vocabulary
/// plus the test controls a real host gets from its environment.
pub trait SimulatedCell:
    CellHost<Event = LawEvent, Command = LawCommand, Input = LawInput> + 'static
{
    /// Whether a fresh `perform` records an empty internal claim commit before
    /// dispatch. The reference model keeps receipt sequences exact for hosts
    /// that expose this stronger native durability behavior.
    const COMMITS_PERFORM_CLAIMS: bool = false;

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
    Nothing,
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
    generations: BTreeMap<CommandId, Generation>,
    timers: BTreeMap<TimerId, DueTimer<LawInput>>,
    inbox: Vec<Observed<LawInput>>,
    seen: BTreeSet<ObservationId>,
    ledger: Vec<LedgerEntry<LawEvent>>,
    executions: u64,
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
            generations: BTreeMap::new(),
            timers: BTreeMap::new(),
            inbox: Vec::new(),
            seen: BTreeSet::new(),
            ledger: Vec::new(),
            executions: 0,
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
        let delivered = Performed {
            command: head.id.clone(),
            generation: head.generation,
            input: law_responder(head),
        };
        let executed = !self.head_performed;
        if executed {
            if self.commits_perform_claims {
                self.seq = self.seq.saturating_add(1);
                self.ledger.push(LedgerEntry {
                    seq: LedgerSeq(self.seq),
                    consumed: None,
                    written: Vec::new(),
                    linked: Vec::new(),
                    events: Vec::new(),
                });
            }
            self.executions = self.executions.saturating_add(1);
            self.head_performed = true;
        }
        Expect::Perform {
            delivered: Some(delivered),
            executed,
        }
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
                },
            )
            .boxed()
    }
}

/// Reference-machine state: the model plus the expectation for the most
/// recently applied operation.
#[derive(Debug, Clone, Default)]
pub struct CellReference {
    model: CellModel,
    expected: Option<Expect>,
}

/// The reference and concrete `proptest-state-machine` for host `H`.
pub struct CellHostMachine<H>(PhantomData<fn() -> H>);

impl<H: SimulatedCell> ReferenceStateMachine for CellHostMachine<H> {
    type State = CellReference;
    type Transition = CellOp;

    fn init_state() -> BoxedStrategy<CellReference> {
        Just(CellReference {
            model: CellModel {
                commits_perform_claims: H::COMMITS_PERFORM_CLAIMS,
                ..CellModel::default()
            },
            expected: None,
        })
        .boxed()
    }

    fn transitions(state: &CellReference) -> BoxedStrategy<CellOp> {
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
        let model = &mut state.model;
        let expected = match op {
            CellOp::Commit(commit) => Expect::Commit(model.commit(commit)),
            CellOp::Perform => model.perform(),
            CellOp::Observe => Expect::Observe(model.inbox.first().cloned()),
            CellOp::Clock => Expect::Clock(model.next_timer()),
            CellOp::Advance(millis) => {
                model.now = model.now.saturating_add(*millis);
                Expect::Nothing
            }
            CellOp::Inject { id, payload } => {
                let observed = law_observed(*id, *payload);
                if model.seen.insert(observed.id.clone()) {
                    model.inbox.push(observed);
                }
                Expect::Nothing
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

/// The refusal without its host error, or the host error itself.
fn without_host<X>(error: CommitError<X>) -> Result<CommitError<Infallible>, X> {
    Ok(match error {
        CommitError::OwnedRowsProtected { key } => CommitError::OwnedRowsProtected { key },
        CommitError::DuplicateKey { key } => CommitError::DuplicateKey { key },
        CommitError::NotPending { consumed } => CommitError::NotPending { consumed },
        CommitError::Conflict {
            key,
            expected,
            current,
        } => CommitError::Conflict {
            key,
            expected,
            current,
        },
        CommitError::DuplicateLink { key } => CommitError::DuplicateLink { key },
        CommitError::IdentifierTooLong {
            field,
            actual_bytes,
            max_bytes,
        } => CommitError::IdentifierTooLong {
            field,
            actual_bytes,
            max_bytes,
        },
        CommitError::LinkConflict {
            key,
            expected,
            current,
        } => CommitError::LinkConflict {
            key,
            expected,
            current,
        },
        CommitError::LinkReadConflict { source_key, link } => {
            CommitError::LinkReadConflict { source_key, link }
        }
        CommitError::UnsupportedLinkRead { source_key, link } => {
            CommitError::UnsupportedLinkRead { source_key, link }
        }
        CommitError::DuplicateLinkRead { source_key, link } => {
            CommitError::DuplicateLinkRead { source_key, link }
        }
        CommitError::LinkReadTooLarge {
            source_key,
            link,
            actual,
            max,
        } => CommitError::LinkReadTooLarge {
            source_key,
            link,
            actual,
            max,
        },
        CommitError::InvalidLinkRead { source_key, link } => {
            CommitError::InvalidLinkRead { source_key, link }
        }
        CommitError::DanglingLink { key, missing } => CommitError::DanglingLink { key, missing },
        CommitError::LinkTarget { key } => CommitError::LinkTarget { key },
        CommitError::LinkCardinality { key, endpoint } => {
            CommitError::LinkCardinality { key, endpoint }
        }
        CommitError::LinkRequired { parent, link } => CommitError::LinkRequired { parent, link },
        CommitError::TypeMismatch { key, concrete } => CommitError::TypeMismatch { key, concrete },
        CommitError::EffectiveValues { key, reason } => {
            CommitError::EffectiveValues { key, reason }
        }
        CommitError::TraversalLimitExceeded { key, reason } => {
            CommitError::TraversalLimitExceeded { key, reason }
        }
        CommitError::AccountingPosting { key, reason } => {
            CommitError::AccountingPosting { key, reason }
        }
        CommitError::InvariantViolation {
            key,
            owner,
            id,
            message,
            outcome,
        } => CommitError::InvariantViolation {
            key,
            owner,
            id,
            message,
            outcome,
        },
        CommitError::InvariantBudgetExceeded { key, limit } => {
            CommitError::InvariantBudgetExceeded { key, limit }
        }
        CommitError::InvariantModel { key, interface } => {
            CommitError::InvariantModel { key, interface }
        }
        CommitError::ObservationAuthorization => CommitError::ObservationAuthorization,
        CommitError::EventLimitExceeded { limit, actual } => {
            CommitError::EventLimitExceeded { limit, actual }
        }
        CommitError::Overflow => CommitError::Overflow,
        CommitError::Host(error) => return Err(error),
    })
}

fn consumed_label(consumed: Option<&Consumed>) -> &'static str {
    match consumed {
        None => "commit.accepted",
        Some(Consumed::Timer { .. }) => "commit.accepted.consumed_timer",
        Some(Consumed::Result { .. }) => "commit.accepted.consumed_result",
        Some(Consumed::Observation { .. }) => "commit.accepted.consumed_observation",
    }
}

fn apply_commit<H: SimulatedCell>(host: &mut H, commit: &LawCommit, expected: &Expect) {
    let before = snapshot(host);
    // A host failure is reported as its message and can never equal the
    // model's expectation.
    let actual = match host.commit(commit.clone()) {
        Ok(receipt) => Ok(Ok(receipt)),
        Err(error) => without_host(error)
            .map(Err)
            .map_err(|error| error.to_string()),
    };
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
            CellOp::Commit(commit) => apply_commit(&mut host, &commit, &expected),
            CellOp::Perform => {
                let executions = host.sim_executions();
                let delivered = host.perform().map_err(|error| error.to_string());
                let executed = host.sim_executions() != executions;
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
            CellOp::Inject { id, payload } => host.sim_inject(law_observed(id, payload)),
        }
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

/// Runs the `CellHost` laws against host `H` and returns the coverage, or the
/// minimal counterexample.
///
/// # Errors
///
/// Returns the shrunk [`Failure`] if any law fails.
pub fn run_cell_host<H: SimulatedCell>(settings: &Settings) -> Result<Coverage, Failure<CellOp>> {
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
        Err(TestError::Fail(reason, (_, transitions, _))) => Err(Failure {
            reason: reason.to_string(),
            transitions,
        }),
        Err(TestError::Abort(reason)) => Err(Failure {
            reason: format!("run aborted: {reason}"),
            transitions: Vec::new(),
        }),
    }
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
