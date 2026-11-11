//! The Decider form and the tick-shaped [`step`].

use std::fmt;
use std::marker::PhantomData;

use serde::{Deserialize, Serialize};

// The pure decision core lives in its own file so Charon can extract it
// standalone (`scripts/verify-kernel-decider-aeneas.sh`).
#[path = "decider_fold.rs"]
pub(crate) mod decider_fold;

use self::decider_fold::{
    Counts, EnvelopeCap, Evolve, StepRefusal, StepTurn, fold_events, replay_events, run_step,
};
use crate::effect::Effect;
use crate::envelope::{decision_cap, decision_caps, wide};
use crate::limits::EnvelopeLimits;
use crate::observations::Observations;

/// What a decision produces: ledger events plus closed effects.
///
/// The host commits the events and enqueues the effects in one transaction
/// (the `commit` verb); nothing here has happened yet.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Decision<E, C, T> {
    /// New ledger events, in order.
    pub events: Vec<E>,
    /// Effects to enqueue atomically with the events.
    pub effects: Vec<Effect<C, T>>,
}

impl<E, C, T> Decision<E, C, T> {
    /// A decision that records nothing and requests nothing.
    #[must_use]
    pub const fn none() -> Self {
        Self {
            events: Vec::new(),
            effects: Vec::new(),
        }
    }

    /// A decision with the given events and no effects.
    #[must_use]
    pub const fn events(events: Vec<E>) -> Self {
        Self {
            events,
            effects: Vec::new(),
        }
    }

    /// Adds one event.
    #[must_use]
    pub fn with_event(mut self, event: E) -> Self {
        self.events.push(event);
        self
    }

    /// Adds one effect.
    #[must_use]
    pub fn with_effect(mut self, effect: Effect<C, T>) -> Self {
        self.effects.push(effect);
        self
    }
}

impl<E, C, T> Default for Decision<E, C, T> {
    fn default() -> Self {
        Self::none()
    }
}

/// The decision type of decider `D`: its events, commands, and input-typed
/// timers.
pub type DecisionOf<D> =
    Decision<<D as Decider>::Event, <D as Decider>::Command, <D as Decider>::Input>;

/// A pure, synchronous decider (ruling 2026-09-25 §3, §5).
///
/// Implementations must be deterministic functions of their arguments: no IO,
/// no clock, no entropy other than [`Observations`], no interior mutability
/// that changes results. `decide` must not return effects on rejection (it
/// cannot: a rejection carries none), and `evolve` must be total.
pub trait Decider {
    /// Materialized state: a fold of [`Decider::evolve`] over events.
    type State: Clone;
    /// What arrives at a turn: an action request, a delivered timer, a
    /// performed command's result, or an observed fact. Timers carry an
    /// `Input` so `clock` delivers them without conversion.
    type Input;
    /// A ledger event.
    type Event;
    /// Typed payload of outbound commands.
    type Command;
    /// Typed domain rejection.
    type Rejection: std::error::Error;

    /// The state before any event.
    fn initial_state(&self) -> Self::State;

    /// Decides what happens for `input` against `state`.
    ///
    /// # Errors
    ///
    /// Returns the domain rejection when the input is not admissible; a
    /// rejection records no events and requests no effects.
    fn decide(
        &self,
        state: &Self::State,
        input: &Self::Input,
        observations: &Observations,
    ) -> Result<DecisionOf<Self>, Self::Rejection>;

    /// Applies one event. Total: every event the decider emits must evolve.
    fn evolve(&self, state: &Self::State, event: &Self::Event) -> Self::State;
}

/// A decider folding a ledger of borrowed events, in the order the caller's
/// iterator yields them.
struct Ledger<'d, 'a, D: ?Sized + Decider>(&'d D, PhantomData<&'a D::Event>);

impl<'a, D: Decider + ?Sized> Evolve for Ledger<'_, 'a, D> {
    type State = D::State;
    type Event = &'a D::Event;

    fn initial_state(&self) -> D::State {
        self.0.initial_state()
    }

    fn evolve(&self, state: &D::State, event: &&'a D::Event) -> D::State {
        self.0.evolve(state, event)
    }
}

/// Folds `events` into `state` with [`Decider::evolve`], first event first.
pub fn fold<'a, D, I>(decider: &D, state: D::State, events: I) -> D::State
where
    D: Decider + ?Sized,
    D::Event: 'a,
    I: IntoIterator<Item = &'a D::Event>,
{
    let events: Vec<&'a D::Event> = events.into_iter().collect();
    fold_events(&Ledger(decider, PhantomData), state, &events)
}

/// Rebuilds state from the ledger: `fold(evolve)` from the initial state.
pub fn replay<'a, D, I>(decider: &D, events: I) -> D::State
where
    D: Decider + ?Sized,
    D::Event: 'a,
    I: IntoIterator<Item = &'a D::Event>,
{
    let events: Vec<&'a D::Event> = events.into_iter().collect();
    replay_events(&Ledger(decider, PhantomData), &events)
}

/// Which envelope cap a decision exceeded.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EnvelopeKind {
    /// [`EnvelopeLimits::max_events`].
    Events,
    /// [`EnvelopeLimits::max_commands`].
    Commands,
    /// [`EnvelopeLimits::max_timers`].
    Timers,
}

impl From<EnvelopeCap> for EnvelopeKind {
    fn from(cap: EnvelopeCap) -> Self {
        match cap {
            EnvelopeCap::Events => Self::Events,
            EnvelopeCap::Commands => Self::Commands,
            EnvelopeCap::Timers => Self::Timers,
        }
    }
}

impl fmt::Display for EnvelopeKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Events => "events",
            Self::Commands => "commands",
            Self::Timers => "timers",
        })
    }
}

/// Why a turn produced no transition. Every variant carries zero events and
/// zero effects; the input state is untouched.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum StepError<R> {
    /// The decider rejected the input.
    #[error("decision rejected")]
    Rejected(#[source] R),
    /// The turn targeted a historical, read-only world.
    #[error("historical worlds are read-only")]
    ReadOnlyWorld,
    /// A non-main world requested `perform` effects.
    #[error("only the main world may perform; {count} perform effect(s) requested in a fork")]
    PerformOutsideMain {
        /// How many perform effects were requested.
        count: usize,
    },
    /// The decision exceeded an envelope cap.
    #[error("decision emitted {actual} {kind}, limit is {limit}")]
    EnvelopeExceeded {
        /// Which cap.
        kind: EnvelopeKind,
        /// The cap.
        limit: u32,
        /// What the decision emitted.
        actual: usize,
    },
}

/// The result of one accepted turn.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Step<S, E, C, T> {
    /// State after evolving the new events.
    pub state: S,
    /// New ledger events, in order.
    pub events: Vec<E>,
    /// Effects to enqueue atomically with the events.
    pub effects: Vec<Effect<C, T>>,
}

/// The accepted-turn type of decider `D`.
pub type StepOf<D> = Step<
    <D as Decider>::State,
    <D as Decider>::Event,
    <D as Decider>::Command,
    <D as Decider>::Input,
>;

/// The outcome of [`step`] for decider `D`.
pub type StepResult<D> = Result<StepOf<D>, StepError<<D as Decider>::Rejection>>;

/// One turn of decider `D`, as the step driver in `decider_fold` sees it.
struct Turn<'t, D: Decider + ?Sized> {
    decider: &'t D,
    input: &'t D::Input,
    observations: &'t Observations,
    limits: &'t EnvelopeLimits,
}

impl<D: Decider + ?Sized> Evolve for Turn<'_, D> {
    type State = D::State;
    type Event = D::Event;

    fn initial_state(&self) -> D::State {
        self.decider.initial_state()
    }

    fn evolve(&self, state: &D::State, event: &D::Event) -> D::State {
        self.decider.evolve(state, event)
    }
}

impl<D: Decider + ?Sized> StepTurn for Turn<'_, D> {
    type Decision = DecisionOf<D>;
    type Rejection = D::Rejection;

    fn writable(&self) -> bool {
        self.observations.world.is_writable()
    }

    fn may_perform(&self) -> bool {
        self.observations.world.may_perform()
    }

    fn decide(&self, state: &D::State) -> Result<DecisionOf<D>, D::Rejection> {
        self.decider.decide(state, self.input, self.observations)
    }

    fn counts(&self, decision: &DecisionOf<D>) -> Counts {
        let effects = &decision.effects;
        Counts {
            events: wide(decision.events.len()),
            commands: wide(effects.iter().filter(|effect| effect.is_perform()).count()),
            timers: wide(effects.iter().filter(|effect| effect.is_timer()).count()),
        }
    }

    fn caps(&self) -> Counts {
        decision_caps(self.limits)
    }

    fn events<'d>(&self, decision: &'d DecisionOf<D>) -> &'d [D::Event] {
        &decision.events
    }

    fn clone_state(&self, state: &D::State) -> D::State {
        state.clone()
    }
}

/// A count back in `usize`: every count the driver sees came from a `usize`.
fn narrow(count: u64) -> usize {
    usize::try_from(count).unwrap_or(usize::MAX)
}

/// One tick-shaped turn: `decide`, check, then `evolve`.
///
/// All-or-nothing: on any error no event is produced, no effect is requested,
/// and `state` (borrowed) is unchanged. Checks, in order: the world is
/// writable; the decider accepts; the decision fits `limits`; a non-main world
/// requests no `perform`. The order is `decider_fold::run_step`'s.
///
/// # Errors
///
/// Returns a [`StepError`] describing the first failed check.
pub fn step<D>(
    decider: &D,
    state: &D::State,
    input: &D::Input,
    observations: &Observations,
    limits: &EnvelopeLimits,
) -> StepResult<D>
where
    D: Decider + ?Sized,
{
    let turn = Turn {
        decider,
        input,
        observations,
        limits,
    };
    match run_step(&turn, state) {
        Ok((next, Decision { events, effects })) => Ok(Step {
            state: next,
            events,
            effects,
        }),
        Err(StepRefusal::ReadOnlyWorld) => Err(StepError::ReadOnlyWorld),
        Err(StepRefusal::Rejected(rejection)) => Err(StepError::Rejected(rejection)),
        Err(StepRefusal::EnvelopeExceeded { cap, actual }) => {
            let kind = EnvelopeKind::from(cap);
            Err(StepError::EnvelopeExceeded {
                kind,
                limit: decision_cap(limits, cap),
                actual: narrow(actual),
            })
        }
        Err(StepRefusal::PerformOutsideMain { commands }) => Err(StepError::PerformOutsideMain {
            count: narrow(commands),
        }),
    }
}
