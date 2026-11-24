//! GRAPH-STATE-1 lifecycle initialization at the shared commit boundary.
//!
//! Every host admits a commit through [`crate::check_commit`], whose
//! compare-and-swap stage calls [`check_first_writes`]: each object a commit
//! writes for the first time begins every effective lifecycle in an admitted
//! initial state, and whatever the commit records about it replays from that
//! initialization under the decision path's own rules
//! ([`check_first_write_lifecycles`]). Direct `Put`s and decision commits
//! share this one rule.

use std::collections::{BTreeMap, BTreeSet};
use std::convert::Infallible;

use norn_ir::{Lifecycle, PropertyId, TransitionId};

use crate::cell_host::{
    CellCommit, CommitError, CommitModel, CommitView, FactKey, FactMutation, JsonObject, JsonValue,
};
use crate::decision_commit::{
    DecisionRefusal, FactEvent, TypeCatalog, check_guards, resolve_transition, with_initial_states,
};
use crate::snapshot_observe::{ObservationAuthorization, ObservationEvent};

/// Why the shared commit admission refused a first write's lifecycle
/// initialization (GRAPH-STATE-1). The same rule as a decision `create`
/// followed by its `transition`s, so both paths refuse the same states. Every
/// variant carries the commit code `lifecycle.validation.undeclared_write`.
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
            | LifecycleInitRefusal::UnrecordedChange { .. }) => Self::InvalidOutput {
                reason: format!("{key}: {refusal}"),
            },
        }
    }
}

/// GRAPH-STATE-1: every object a non-observed commit writes for the first
/// time — absent from the transaction's pre-state — begins each effective
/// lifecycle in an admitted initial state, or reaches its written state only
/// through the transitions the commit records for it, replayed in order from
/// initialization ([`check_first_write_lifecycles`],
/// the decision `create` and `transition` rules). This holds for every
/// admission of every host, so a direct `Put` cannot place a new object in a
/// state its lifecycle never reached. There is no caller-selectable flag: only
/// the exact writes a sealed observation's opaque, kernel-prepared
/// authorization covers reconcile lifecycles under their own evidence
/// contract instead ([`crate::snapshot_observe`]); every other write in the
/// same commit is checked here.
pub(crate) fn check_first_writes<E: 'static, C, T, V: CommitView + ?Sized>(
    commit: &CellCommit<E, C, T>,
    model: CommitModel<'_, '_>,
    view: &V,
    authorization: Option<&ObservationAuthorization>,
    trusted_owned_rows: bool,
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
        let canonical = model.canonical(&write.key);
        if view
            .concrete(&canonical)
            .map_err(CommitError::Host)?
            .is_some()
        {
            continue;
        }
        let recorded = recorded.get_or_insert_with(|| recorded_fact_events(commit, model));
        let (events, observed) = recorded
            .get(&canonical)
            .map_or((&[][..], None), |recorded| {
                (recorded.facts.as_slice(), recorded.observed)
            });
        check_first_write_lifecycles(
            model.catalog,
            &write.key,
            values,
            events,
            trusted_owned_rows,
            observed,
        )
        .map_err(|refusal| CommitError::LifecycleInitialization {
            key: write.key.clone(),
            refusal: Box::new(refusal),
        })?;
    }
    Ok(())
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
/// exactly the written values. `kernel_origin` is the kernel-prepared
/// owned-row admission, whose creations may initialize rules-only properties
/// under the kernel's private origin (FA-PROTECT-1). `observed` is why an
/// observed transition or deletion names the object: that is history of an
/// existing object, so it contradicts a first write.
pub(crate) fn check_first_write_lifecycles(
    catalog: &TypeCatalog<'_>,
    key: &FactKey,
    values: &JsonObject,
    events: &[&FactEvent],
    kernel_origin: bool,
    observed: Option<&str>,
) -> Result<(), LifecycleInitRefusal> {
    let lifecycle_properties = catalog.lifecycle_properties(&key.object_type);
    let lifecycles = catalog.lifecycles(&key.object_type);
    if lifecycle_properties.is_empty() && lifecycles.is_empty() {
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
    let replay = FirstWriteReplay {
        catalog,
        key,
        lifecycle_properties,
        lifecycles,
        kernel_origin,
    };
    let mut current: Option<JsonObject> = None;
    for event in events {
        replay.apply(&mut current, event)?;
    }
    let Some(replayed) = current else {
        return Err(unrecorded("the recorded changes leave the object absent"));
    };
    let effective = |values: &JsonObject| {
        catalog
            .effective_object_values(&key.object_type, values)
            .map_err(|error| unrecorded(&format!("the effective values are refused: {error}")))
    };
    if effective(&replayed)? != effective(values)? {
        return Err(unrecorded(
            "the written values differ from the values its recorded changes produce",
        ));
    }
    Ok(())
}

fn unrecorded(reason: &str) -> LifecycleInitRefusal {
    LifecycleInitRefusal::UnrecordedChange {
        reason: reason.to_owned(),
    }
}

/// The decision path's rules for one first-written object, replayed over
/// the fact events its commit records ([`check_first_write_lifecycles`]).
struct FirstWriteReplay<'r, 'a> {
    catalog: &'r TypeCatalog<'a>,
    /// The written object: its exact concrete type and key.
    key: &'r FactKey,
    lifecycle_properties: BTreeSet<PropertyId>,
    lifecycles: BTreeMap<PropertyId, Lifecycle>,
    /// Whether the kernel's owned-row admission prepared the commit.
    kernel_origin: bool,
}

impl FirstWriteReplay<'_, '_> {
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
    /// without that input.
    fn create(
        &self,
        current: &mut Option<JsonObject>,
        created: &JsonObject,
    ) -> Result<(), LifecycleInitRefusal> {
        if current.is_some() {
            return Err(unrecorded("the commit records its creation twice"));
        }
        let materialized = self.materialize(created)?;
        if !self.kernel_origin {
            let mut caller = created.clone();
            caller.retain(|property, _| !self.rules_only(property));
            if caller.len() != created.len() {
                let defaults = self.materialize(&caller)?;
                if let Some(property) = created.keys().find(|property| {
                    self.rules_only(property)
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
            // commit; it cannot describe one the commit creates.
            FactEvent::Modified { .. } => {
                return Err(unrecorded(
                    "a modification aggregate names an object the commit creates",
                ));
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
