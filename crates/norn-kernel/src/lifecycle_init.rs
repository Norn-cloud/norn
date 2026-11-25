//! GRAPH-STATE-1 lifecycle legality at the shared commit boundary.
//!
//! Every host admits a commit through [`crate::check_commit`], whose
//! compare-and-swap stage calls [`check_lifecycle_writes`] for every `Put`:
//!
//! - an object written for the first time begins every effective lifecycle
//!   in an admitted initial state, and whatever the commit records about it
//!   replays from that initialization ([`check_first_write_lifecycles`]);
//! - an object that already exists keeps every lifecycle state the commit's
//!   recorded transitions do not move, replayed from its stored row, and is
//!   not otherwise changed in an immutable state
//!   ([`check_update_lifecycles`]).
//!
//! Both replay the decision path's own rules ([`LifecycleReplay`]), so
//! direct `Put`s and decision commits share one rule.

use std::collections::{BTreeMap, BTreeSet};
use std::convert::Infallible;

use norn_ir::{Lifecycle, ObjectTypeName, PropertyId, TransitionId};

use crate::cell_host::{
    CellCommit, CommitError, CommitModel, CommitView, FactKey, FactMutation, JsonObject, JsonValue,
};
use crate::creator_stamp::{CreatorStamp, KernelTrust};
use crate::decision_commit::{
    DecisionRefusal, FactEvent, TypeCatalog, check_guards, materialize_lifecycle_defaults,
    resolve_transition,
};
use crate::snapshot_observe::{ObservationAuthorization, ObservationEvent};

/// Why the shared commit admission refused a write's lifecycle states
/// (GRAPH-STATE-1): a first write's initialization, or an existing object's
/// update. The same rule as a decision `create` followed by its `set`s and
/// `transition`s, so both paths refuse the same states. Every variant carries
/// the commit code `lifecycle.validation.undeclared_write`.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum LifecycleInitRefusal {
    /// A lifecycle is declared over the property, but it does not resolve to
    /// an effective state machine; refused fail-closed.
    #[error("{lifecycle} has no effective lifecycle")]
    Unresolved {
        /// The lifecycle property.
        lifecycle: String,
    },
    /// The lifecycle does not begin in an admitted initial state.
    #[error("{lifecycle} must start in {allowed}, not {actual}", allowed = initial.join(" or "), actual = state.as_deref().unwrap_or("unset"))]
    InvalidInitialState {
        /// The lifecycle property.
        lifecycle: String,
        /// The state written (`None` when there is none and no default).
        state: Option<String>,
        /// The declared initial states.
        initial: Vec<String>,
    },
    /// A recorded transition is undeclared, leaves another state than the
    /// one the replay holds, or its guards do not hold there.
    #[error("transition {transition} of {lifecycle}: {reason}")]
    IllegalTransition {
        /// The lifecycle property.
        lifecycle: String,
        /// The recorded transition.
        transition: String,
        /// Why it cannot fire.
        reason: String,
    },
    /// The recorded events do not reconstruct the written object: no
    /// creation, a direct lifecycle or immutable-state write, or a final
    /// value the recorded changes never produced.
    #[error("{reason}")]
    UnrecordedChange {
        /// What the replay could not reconstruct.
        reason: String,
    },
    /// An existing object's written lifecycle state is not the state its
    /// recorded transitions reach from its stored one.
    #[error("{lifecycle} is written as {written}, but its recorded transitions leave it {reached}", written = written.as_deref().unwrap_or("unset"), reached = reached.as_deref().unwrap_or("unset"))]
    UnrecordedTransition {
        /// The lifecycle property.
        lifecycle: String,
        /// The state the recorded transitions reach from the stored row.
        reached: Option<String>,
        /// The state the commit writes.
        written: Option<String>,
    },
    /// An existing object is changed, beyond what the commit records, while
    /// it holds an immutable lifecycle state.
    #[error("{property} is changed in immutable {lifecycle} state {state}")]
    ImmutableState {
        /// The lifecycle property holding the immutable state.
        lifecycle: String,
        /// The immutable state.
        state: String,
        /// The first property changed without a recorded change.
        property: String,
    },
}

impl<X> DecisionRefusal<X> {
    /// The decision form of a commit-boundary lifecycle refusal.
    pub(crate) fn from_init(key: FactKey, refusal: LifecycleInitRefusal) -> Self {
        match refusal {
            LifecycleInitRefusal::InvalidInitialState {
                lifecycle,
                state,
                initial,
            } => Self::InvalidInitialState {
                key,
                lifecycle,
                state,
                initial,
            },
            LifecycleInitRefusal::Unresolved { lifecycle } => {
                Self::UnresolvedLifecycle { key, lifecycle }
            }
            refusal @ (LifecycleInitRefusal::IllegalTransition { .. }
            | LifecycleInitRefusal::UnrecordedChange { .. }
            | LifecycleInitRefusal::UnrecordedTransition { .. }
            | LifecycleInitRefusal::ImmutableState { .. }) => Self::InvalidOutput {
                reason: format!("{key}: {refusal}"),
            },
        }
    }
}

/// GRAPH-STATE-1 for every `Put` of a commit, at every admission of every
/// host.
///
/// An object absent from the transaction's pre-state begins each effective
/// lifecycle in an admitted initial state, or reaches its written state only
/// through the transitions the commit records for it, replayed in order from
/// initialization ([`check_first_write_lifecycles`], the decision `create`
/// and `transition` rules). An object that exists changes a lifecycle state
/// only through the transitions the commit records for it, replayed in order
/// from its stored row, and is not otherwise changed in an immutable state
/// ([`check_update_lifecycles`]). So a direct `Put` can neither place a new
/// object in a state its lifecycle never reached nor move an existing one
/// there. A host that cannot prove an existing lifecycle-bearing object's
/// stored row refuses its update.
///
/// There is no caller-selectable flag: only the exact writes a sealed
/// observation's opaque, kernel-prepared authorization covers reconcile
/// lifecycles under their own evidence contract instead
/// ([`crate::snapshot_observe`]); every other write in the same commit is
/// checked here. History restore and pinned migration do not pass through
/// commit admission at all.
pub(crate) fn check_lifecycle_writes<E: 'static, C, T, V: CommitView + ?Sized>(
    commit: &CellCommit<E, C, T>,
    model: CommitModel<'_, '_>,
    view: &V,
    authorization: Option<&ObservationAuthorization>,
    trust: KernelTrust<'_>,
) -> Result<(), CommitError<V::Error>> {
    let authorized = authorization.map(ObservationAuthorization::indexed);
    let mut recorded: Option<BTreeMap<FactKey, Recorded<'_>>> = None;
    for write in &commit.writes {
        let FactMutation::Put { values } = &write.mutation else {
            continue;
        };
        // Only the exact write a sealed observation prepared is exempt; any
        // other write in the same commit is an ordinary command mutation.
        if authorized
            .as_ref()
            .is_some_and(|index| index.authorizes(write))
        {
            continue;
        }
        let refused = |refusal: LifecycleInitRefusal| -> CommitError<V::Error> {
            CommitError::LifecycleInitialization {
                key: write.key.clone(),
                refusal: Box::new(refusal),
            }
        };
        let canonical = model.canonical(&write.key);
        let exists = view
            .concrete(&canonical)
            .map_err(CommitError::Host)?
            .is_some();
        if exists && !bears_lifecycle(model.catalog, &write.key.object_type) {
            continue;
        }
        let stored = if exists {
            let stored = view.object_values(&canonical).map_err(CommitError::Host)?;
            Some(stored.ok_or_else(|| {
                refused(unrecorded("the host cannot prove the object's stored row"))
            })?)
        } else {
            None
        };
        let recorded = recorded.get_or_insert_with(|| recorded_fact_events(commit, model));
        let (events, observed) = recorded
            .get(&canonical)
            .map_or((&[][..], None), |recorded| {
                (recorded.facts.as_slice(), recorded.observed)
            });
        let catalog = model.catalog;
        match stored {
            None => {
                check_first_write_lifecycles(catalog, &write.key, values, events, trust, observed)
            }
            Some(stored) => {
                check_update_lifecycles(catalog, &write.key, &stored, values, events, trust)
            }
        }
        .map_err(refused)?;
    }
    Ok(())
}

/// Whether `object_type` declares or inherits any lifecycle, resolved or not.
fn bears_lifecycle(catalog: &TypeCatalog<'_>, object_type: &ObjectTypeName) -> bool {
    !catalog.lifecycle_properties(object_type).is_empty()
        || !catalog.lifecycles(object_type).is_empty()
}

/// What one commit records about one canonical identity.
#[derive(Default)]
struct Recorded<'c> {
    /// The fact events naming it, in ledger order: the kernel's own, and
    /// those an observation event wraps.
    facts: Vec<&'c FactEvent>,
    /// Why an observed lifecycle event names it: an observed transition or
    /// deletion is history of an object that already existed.
    observed: Option<&'static str>,
}

/// What `commit` records for each object, by canonical identity.
fn recorded_fact_events<'c, E: 'static, C, T>(
    commit: &'c CellCommit<E, C, T>,
    model: CommitModel<'_, '_>,
) -> BTreeMap<FactKey, Recorded<'c>> {
    let mut recorded: BTreeMap<FactKey, Recorded<'c>> = BTreeMap::new();
    // Evidence is identified by type: a host whose ledger events are neither
    // records none, so it creates a lifecycle-bearing object only in an
    // admitted initial state.
    for event in &commit.events {
        let event: &dyn std::any::Any = event;
        let fact = if let Some(fact) = event.downcast_ref::<FactEvent>() {
            fact
        } else {
            match event.downcast_ref::<ObservationEvent>() {
                Some(ObservationEvent::ObservedFact { event, .. }) => event,
                Some(ObservationEvent::ObservedTransition { key, .. }) => {
                    let entry = recorded.entry(model.canonical(key)).or_default();
                    if entry.observed.is_none() {
                        entry.observed =
                            Some("an observed transition names an unsealed first write");
                    }
                    continue;
                }
                Some(ObservationEvent::ObservedDeleted { key, .. }) => {
                    let entry = recorded.entry(model.canonical(key)).or_default();
                    if entry.observed.is_none() {
                        entry.observed = Some("an observed deletion names an unsealed first write");
                    }
                    continue;
                }
                Some(
                    ObservationEvent::DataQualityFinding { .. }
                    | ObservationEvent::ObservedRecreated { .. },
                )
                | None => continue,
            }
        };
        let key = match fact {
            FactEvent::Created { key, .. }
            | FactEvent::Set { key, .. }
            | FactEvent::Modified { key, .. }
            | FactEvent::Deleted { key }
            | FactEvent::Transitioned { key, .. } => key,
            FactEvent::CommandEnqueued { .. }
            | FactEvent::NamingSeriesAllocated { .. }
            | FactEvent::NamingSeriesCounterSeeded { .. }
            | FactEvent::InvariantRefused { .. } => continue,
        };
        recorded
            .entry(model.canonical(key))
            .or_default()
            .facts
            .push(fact);
    }
    recorded
}

/// `values` with every effective lifecycle property's default filled in,
/// after checking each lifecycle property holds an initial state: the
/// decision `create` rule, shared with the commit boundary below.
pub(crate) fn with_initial_states<X>(
    catalog: &TypeCatalog<'_>,
    key: &FactKey,
    values: &JsonObject,
) -> Result<JsonObject, DecisionRefusal<X>> {
    materialize_lifecycle_defaults(catalog, key, values, true)
}

/// GRAPH-STATE-1 at the shared commit boundary: the first write of `key`
/// (absent in the transaction's pre-state) with final `values`, given the
/// fact events the same commit records for its canonical identity, in
/// ledger order.
///
/// With no recorded evidence for the object, the final row is the creation:
/// each effective lifecycle, inherited ones included, must hold an admitted
/// initial state after defaults ([`with_initial_states`], the decision
/// `create` rule). Whenever the commit records evidence (`Created`, `Set`,
/// `Transitioned`, `Deleted`), that evidence is validated even when the row
/// ends in an initial state, since a final row alone cannot tell a legal
/// sequence from a forged one: it is replayed from initialization under the
/// decision path's own rules ([`FirstWriteReplay`]) and must end in
/// exactly the written values. Under `trust`, the kernel-prepared owned-row
/// admission's creations may initialize rules-only properties under the
/// kernel's private origin (FA-PROTECT-1), and a governed create may hold its
/// creator stamp (ACTOR-STAMP-1). `observed` is why an
/// observed transition or deletion names the object: that is history of an
/// existing object, so it contradicts a first write.
pub(crate) fn check_first_write_lifecycles(
    catalog: &TypeCatalog<'_>,
    key: &FactKey,
    values: &JsonObject,
    events: &[&FactEvent],
    trust: KernelTrust<'_>,
    observed: Option<&str>,
) -> Result<(), LifecycleInitRefusal> {
    if !bears_lifecycle(catalog, &key.object_type) {
        return Ok(());
    }
    if let Some(reason) = observed {
        return Err(unrecorded(reason));
    }
    if events.is_empty() {
        return with_initial_states::<Infallible>(catalog, key, values)
            .map(drop)
            .map_err(lifecycle_init_refusal);
    }
    let replay = LifecycleReplay::new(catalog, key, trust, None);
    let replayed = replay.replay(events)?;
    if replay.effective(&replayed)? != replay.effective(values)? {
        return Err(unrecorded(
            "the written values differ from the values its recorded changes produce",
        ));
    }
    Ok(())
}

/// GRAPH-STATE-1 at the shared commit boundary for an object that exists:
/// the `Put` of `key` with final `values` over its `stored` row, given the
/// fact events the same commit records for its canonical identity, in
/// ledger order.
///
/// The recorded events replay from the stored row under the decision path's
/// own rules ([`LifecycleReplay`]): a transition fires from the state the
/// replay holds with its guards evaluated there, a set never touches a
/// lifecycle property and never lands in an immutable state. Then:
///
/// - every lifecycle property, inherited ones included, is written in exactly
///   the state that replay reaches. With no recorded transition that is the
///   stored state, so a `Put` alone never moves a lifecycle, a terminal state
///   is never left, and a skipped or forged transition is refused
///   ([`LifecycleInitRefusal::UnrecordedTransition`]);
/// - any other property the commit changes without recording it is a write
///   after the recorded ones, so it is refused when the replayed row holds an
///   immutable state ([`LifecycleInitRefusal::ImmutableState`]), as a
///   decision `set` there is.
///
/// A row whose update changes nothing a lifecycle governs is admitted as
/// before: properties outside lifecycles remain freely writable in a mutable
/// state. An observed transition or deletion naming an unsealed write is no
/// evidence for it and moves nothing.
pub(crate) fn check_update_lifecycles(
    catalog: &TypeCatalog<'_>,
    key: &FactKey,
    stored: &JsonObject,
    values: &JsonObject,
    events: &[&FactEvent],
    trust: KernelTrust<'_>,
) -> Result<(), LifecycleInitRefusal> {
    if !bears_lifecycle(catalog, &key.object_type) {
        return Ok(());
    }
    let replay = LifecycleReplay::new(catalog, key, trust, Some(stored));
    // Compared as the host stores both rows, so a stored row that predates a
    // declared default is not a change.
    let reached = replay.effective(&replay.replay(events)?)?;
    let written = replay.effective(values)?;
    let state = |values: &JsonObject, lifecycle: &PropertyId| match values.get(lifecycle.as_str()) {
        Some(JsonValue::String(state)) => Some(state.clone()),
        _ => None,
    };
    let governed: BTreeSet<&PropertyId> = replay
        .lifecycle_properties
        .iter()
        .chain(replay.lifecycles.keys())
        .collect();
    for lifecycle in governed {
        let (reached, written) = (state(&reached, lifecycle), state(&written, lifecycle));
        if reached != written {
            return Err(LifecycleInitRefusal::UnrecordedTransition {
                lifecycle: lifecycle.as_str().to_owned(),
                reached,
                written,
            });
        }
    }
    let Some((lifecycle, state)) = catalog.immutable_state(&key.object_type, &reached) else {
        return Ok(());
    };
    let present = |values: &JsonObject, property: &str| {
        values
            .get(property)
            .filter(|value| !matches!(value, JsonValue::Null))
            .cloned()
    };
    let changed = reached
        .keys()
        .chain(written.keys())
        .find(|property| present(&reached, property) != present(&written, property));
    match changed {
        Some(property) => Err(LifecycleInitRefusal::ImmutableState {
            lifecycle: lifecycle.as_str().to_owned(),
            state,
            property: property.clone(),
        }),
        None => Ok(()),
    }
}

fn unrecorded(reason: &str) -> LifecycleInitRefusal {
    LifecycleInitRefusal::UnrecordedChange {
        reason: reason.to_owned(),
    }
}

/// The decision path's rules for one written object, replayed over the fact
/// events its commit records: from absence for a first write
/// ([`check_first_write_lifecycles`]), from the stored row for an object
/// that exists ([`check_update_lifecycles`]).
struct LifecycleReplay<'r, 'a> {
    catalog: &'r TypeCatalog<'a>,
    /// The written object: its exact concrete type and key.
    key: &'r FactKey,
    lifecycle_properties: BTreeSet<PropertyId>,
    lifecycles: BTreeMap<PropertyId, Lifecycle>,
    /// Whether the kernel's owned-row admission prepared the commit.
    kernel_origin: bool,
    /// The creator stamp of a kernel-prepared governed create (ACTOR-STAMP-1).
    stamp: Option<&'r CreatorStamp>,
    /// The object's stored row before the commit; `None` for a first write.
    stored: Option<&'r JsonObject>,
}

impl<'r, 'a> LifecycleReplay<'r, 'a> {
    fn new(
        catalog: &'r TypeCatalog<'a>,
        key: &'r FactKey,
        trust: KernelTrust<'r>,
        stored: Option<&'r JsonObject>,
    ) -> Self {
        Self {
            catalog,
            key,
            lifecycle_properties: catalog.lifecycle_properties(&key.object_type),
            lifecycles: catalog.lifecycles(&key.object_type),
            kernel_origin: trust.owned_rows(),
            stamp: trust.stamp(),
            stored,
        }
    }

    /// The object after `events`, applied in order to its pre-state.
    fn replay(&self, events: &[&FactEvent]) -> Result<JsonObject, LifecycleInitRefusal> {
        let mut current = self.stored.cloned();
        for event in events {
            self.apply(&mut current, event)?;
        }
        current.ok_or_else(|| unrecorded("the recorded changes leave the object absent"))
    }

    /// `values` as the host stores them.
    fn effective(&self, values: &JsonObject) -> Result<JsonObject, LifecycleInitRefusal> {
        self.catalog
            .effective_object_values(&self.key.object_type, values)
            .map_err(|error| unrecorded(&format!("the effective values are refused: {error}")))
    }

    /// Whether only rules may write `property` (FA-PROTECT-1). A type with no
    /// effective view cannot prove otherwise, so it fails closed.
    fn rules_only(&self, property: &str) -> bool {
        self.catalog
            .effective_type(&self.key.object_type)
            .is_none_or(|effective| {
                effective
                    .property(property)
                    .is_some_and(norn_checker::EffectiveProperty::protected)
            })
    }

    /// Initial states, then effective values: the decision `create` rule.
    fn materialize(&self, values: &JsonObject) -> Result<JsonObject, LifecycleInitRefusal> {
        let values = with_initial_states::<Infallible>(self.catalog, self.key, values)
            .map_err(lifecycle_init_refusal)?;
        self.catalog
            .effective_object_values(&self.key.object_type, &values)
            .map_err(|error| unrecorded(&format!("the created values are refused: {error}")))
    }

    /// A recorded creation, materialized as a decision `create` would be, so
    /// every guard reads the values the final comparison does. A caller
    /// cannot supply a rules-only property: outside the kernel's owned-row
    /// origin, each one must hold exactly the value materialization gives it
    /// without that input, or the kernel's creator stamp.
    fn create(
        &self,
        current: &mut Option<JsonObject>,
        created: &JsonObject,
    ) -> Result<(), LifecycleInitRefusal> {
        if current.is_some() {
            return Err(unrecorded(
                "the commit records the creation of an object that exists",
            ));
        }
        let materialized = self.materialize(created)?;
        if !self.kernel_origin {
            let mut caller = created.clone();
            caller.retain(|property, _| !self.rules_only(property));
            if caller.len() != created.len() {
                let defaults = self.materialize(&caller)?;
                let stamped = |property: &str| {
                    self.stamp
                        .zip(created.get(property))
                        .is_some_and(|(stamp, value)| stamp.permits(self.key, property, value))
                };
                if let Some(property) = created.keys().find(|property| {
                    self.rules_only(property)
                        && !stamped(property.as_str())
                        && materialized.get(property.as_str()) != defaults.get(property.as_str())
                }) {
                    return Err(unrecorded(&format!("{property} is writable only by rules")));
                }
            }
        }
        *current = Some(materialized);
        Ok(())
    }

    /// A recorded deletion, refused as decision preparation refuses it: a
    /// type with rules-only properties, or with no effective view to prove
    /// it has none, cannot be deleted (FA-PROTECT-1).
    fn delete(&self, current: &mut Option<JsonObject>) -> Result<(), LifecycleInitRefusal> {
        let protected = self
            .catalog
            .effective_type(&self.key.object_type)
            .is_none_or(|effective| {
                effective
                    .properties
                    .iter()
                    .any(norn_checker::EffectiveProperty::protected)
            });
        if protected {
            return Err(unrecorded(
                "an object with rules-only properties cannot be deleted",
            ));
        }
        if current.take().is_none() {
            return Err(unrecorded("the object is deleted before it exists"));
        }
        Ok(())
    }

    /// Apply one recorded event to the replayed object `current`.
    fn apply(
        &self,
        current: &mut Option<JsonObject>,
        event: &FactEvent,
    ) -> Result<(), LifecycleInitRefusal> {
        let subject = match event {
            FactEvent::Created { key, .. }
            | FactEvent::Set { key, .. }
            | FactEvent::Modified { key, .. }
            | FactEvent::Deleted { key }
            | FactEvent::Transitioned { key, .. } => Some(key),
            FactEvent::CommandEnqueued { .. }
            | FactEvent::NamingSeriesAllocated { .. }
            | FactEvent::NamingSeriesCounterSeeded { .. }
            | FactEvent::InvariantRefused { .. } => None,
        };
        // Evidence binds to the exact concrete type created: a sibling
        // subtype sharing the hierarchy root and key proves nothing here.
        if subject.is_some_and(|subject| subject != self.key) {
            return Err(unrecorded(
                "the recorded evidence names another type than the written object",
            ));
        }
        match event {
            FactEvent::Created {
                values: created, ..
            } => self.create(current, created)?,
            FactEvent::Set {
                property, value, ..
            } => self.set(current, property, value)?,
            FactEvent::Transitioned {
                lifecycle,
                transition,
                from,
                to,
                ..
            } => self.transition(current, lifecycle, transition, from, to)?,
            FactEvent::Deleted { .. } => self.delete(current)?,
            // An aggregate restates an object that existed before the
            // commit: it cannot describe one the commit creates, and for one
            // that exists it restates the granular events beside it, so it
            // neither moves nor justifies anything.
            FactEvent::Modified { .. } => {
                if self.stored.is_none() {
                    return Err(unrecorded(
                        "a modification aggregate names an object the commit creates",
                    ));
                }
            }
            // No object state of this subject.
            FactEvent::CommandEnqueued { .. }
            | FactEvent::NamingSeriesAllocated { .. }
            | FactEvent::NamingSeriesCounterSeeded { .. }
            | FactEvent::InvariantRefused { .. } => {}
        }
        Ok(())
    }

    /// Apply a recorded ordinary property write, as decision preparation
    /// admits and stores it.
    fn set(
        &self,
        current: &mut Option<JsonObject>,
        property: &PropertyId,
        value: &JsonValue,
    ) -> Result<(), LifecycleInitRefusal> {
        let (catalog, key) = (self.catalog, self.key);
        let Some(replayed) = current.as_mut() else {
            return Err(unrecorded("a property is set before the object exists"));
        };
        if self.lifecycle_properties.contains(property) || self.lifecycles.contains_key(property) {
            return Err(unrecorded(&format!(
                "{} is a lifecycle property and changes only through a declared transition",
                property.as_str()
            )));
        }
        if self.rules_only(property.as_str()) {
            return Err(unrecorded(&format!(
                "{} is writable only by rules",
                property.as_str()
            )));
        }
        if let Some((lifecycle, state)) = catalog.immutable_state(&key.object_type, replayed) {
            return Err(unrecorded(&format!(
                "a property is set in immutable {} state {state}",
                lifecycle.as_str()
            )));
        }
        // As decision preparation stores it, so a guard never reads
        // two representations of one instant as different values.
        let value =
            catalog.canonicalize_working_temporal_value(&key.object_type, property.as_str(), value);
        replayed.insert(property.as_str().to_owned(), value);
        Ok(())
    }

    /// Fire the recorded `transition` from the state the replay holds, with
    /// its guards evaluated on the replayed values.
    fn transition(
        &self,
        current: &mut Option<JsonObject>,
        lifecycle: &PropertyId,
        transition: &TransitionId,
        from: &str,
        to: &str,
    ) -> Result<(), LifecycleInitRefusal> {
        let illegal = |reason: String| LifecycleInitRefusal::IllegalTransition {
            lifecycle: lifecycle.as_str().to_owned(),
            transition: transition.as_str().to_owned(),
            reason,
        };
        let Some(replayed) = current.as_mut() else {
            return Err(illegal("the object does not exist".to_owned()));
        };
        if self.rules_only(lifecycle.as_str()) {
            return Err(illegal(
                "the lifecycle is writable only by rules".to_owned(),
            ));
        }
        let (declared, left) = resolve_transition::<Infallible>(
            self.catalog,
            self.key,
            replayed,
            lifecycle,
            transition,
        )
        .map_err(|refusal| illegal(refusal.to_string()))?;
        if left != from || declared.to != to {
            return Err(illegal(format!(
                "it fires {left} -> {}, but the commit records {from} -> {to}",
                declared.to
            )));
        }
        check_guards::<Infallible>(self.key, replayed, &declared)
            .map_err(|refusal| illegal(refusal.to_string()))?;
        replayed.insert(
            lifecycle.as_str().to_owned(),
            JsonValue::String(declared.to),
        );
        Ok(())
    }
}

/// The commit-boundary form of a decision lifecycle refusal.
fn lifecycle_init_refusal(refusal: DecisionRefusal<Infallible>) -> LifecycleInitRefusal {
    if let DecisionRefusal::InvalidInitialState {
        lifecycle,
        state,
        initial,
        ..
    } = refusal
    {
        return LifecycleInitRefusal::InvalidInitialState {
            lifecycle,
            state,
            initial,
        };
    }
    if let DecisionRefusal::UnresolvedLifecycle { lifecycle, .. } = refusal {
        return LifecycleInitRefusal::Unresolved { lifecycle };
    }
    LifecycleInitRefusal::UnrecordedChange {
        reason: refusal.to_string(),
    }
}

#[cfg(test)]
mod stamped_replay_tests {
    //! ACTOR-STAMP-1 in the first-write replay itself, with no
    //! [`crate::creator_stamp::check_protected`] in front of it: the replay
    //! alone admits a protected creation value only as the kernel's stamp.

    use std::collections::BTreeSet;

    use norn_ir::ObjectTypeDef;

    use super::{
        CreatorStamp, FactEvent, FactKey, JsonObject, KernelTrust, LifecycleInitRefusal,
        TypeCatalog, check_first_write_lifecycles,
    };

    const PRINCIPAL: &str = "person:author";

    #[allow(clippy::expect_used, reason = "test fixture")]
    fn definition() -> ObjectTypeDef {
        serde_json::from_value(serde_json::json!({
            "name": "test.Note",
            "properties": [
                { "id": "state", "default": "open", "type": { "kind": "enum", "values": [
                    { "value": "open" }, { "value": "closed" }
                ] } },
                { "id": "written_by", "protected": true, "type": { "kind": "text" },
                  "constraints": { "nullable": true } },
                { "id": "checked_by", "protected": true, "type": { "kind": "text" },
                  "constraints": { "nullable": true } }
            ],
            "lifecycles": [{
                "property": "state",
                "initial": ["open"],
                "terminal": ["closed"],
                "transitions": [{ "id": "close", "from": ["open"], "to": "closed" }]
            }]
        }))
        .expect("definition decodes")
    }

    fn note(key: &str) -> FactKey {
        FactKey::new("test.Note", key)
    }

    #[allow(clippy::expect_used, reason = "test fixture")]
    fn values(value: serde_json::Value) -> JsonObject {
        serde_json::from_value(value).expect("values")
    }

    /// The kernel's stamp of `written_by` on `N-1`.
    fn stamp() -> CreatorStamp {
        CreatorStamp {
            key: note("N-1"),
            properties: BTreeSet::from(["written_by".to_owned()]),
            principal: PRINCIPAL.to_owned(),
        }
    }

    /// Replay a first write of `key` whose one recorded creation holds
    /// exactly the written `row`, under `trust`.
    fn replay(
        key: &FactKey,
        row: serde_json::Value,
        trust: KernelTrust<'_>,
    ) -> Result<(), LifecycleInitRefusal> {
        let definition = definition();
        let catalog = TypeCatalog::new([&definition]);
        let created = FactEvent::Created {
            key: key.clone(),
            values: values(row.clone()),
        };
        let mut written = values(row);
        written
            .entry("state".to_owned())
            .or_insert_with(|| "open".into());
        check_first_write_lifecycles(&catalog, key, &written, &[&created], trust, None)
    }

    fn rules_only(result: Result<(), LifecycleInitRefusal>, property: &str) -> bool {
        matches!(
            result,
            Err(LifecycleInitRefusal::UnrecordedChange { reason })
                if reason == format!("{property} is writable only by rules")
        )
    }

    #[test]
    fn the_stamp_admits_exactly_the_stamped_value_on_the_stamped_object() {
        let stamp = stamp();
        assert_eq!(
            replay(
                &note("N-1"),
                serde_json::json!({ "written_by": PRINCIPAL }),
                KernelTrust::Stamped(&stamp),
            ),
            Ok(())
        );
        // The same row with no stamp is a caller writing a rules-only value.
        assert!(rules_only(
            replay(
                &note("N-1"),
                serde_json::json!({ "written_by": PRINCIPAL }),
                KernelTrust::Plain,
            ),
            "written_by"
        ));
    }

    #[test]
    fn another_principal_than_the_stamped_one_is_refused() {
        let stamp = stamp();
        assert!(rules_only(
            replay(
                &note("N-1"),
                serde_json::json!({ "written_by": "person:other" }),
                KernelTrust::Stamped(&stamp),
            ),
            "written_by"
        ));
    }

    #[test]
    fn a_protected_property_the_stamp_does_not_name_is_refused() {
        let stamp = stamp();
        assert!(rules_only(
            replay(
                &note("N-1"),
                serde_json::json!({ "written_by": PRINCIPAL, "checked_by": PRINCIPAL }),
                KernelTrust::Stamped(&stamp),
            ),
            "checked_by"
        ));
    }

    #[test]
    fn a_second_object_cannot_ride_the_stamp_of_the_first() {
        let stamp = stamp();
        assert!(rules_only(
            replay(
                &note("N-2"),
                serde_json::json!({ "written_by": PRINCIPAL }),
                KernelTrust::Stamped(&stamp),
            ),
            "written_by"
        ));
    }
}
