//! The reference-model contract a test implements for one Decider.

use std::fmt;

use norn_kernel::{Decider, Decision, EnvelopeKind, EnvelopeLimits, Observations};
use proptest::strategy::{BoxedStrategy, Strategy};

use crate::strategy;

/// Input type of model `M`'s decider.
pub type InputOf<M> = <<M as DeciderModel>::Decider as Decider>::Input;
/// Event type of model `M`'s decider.
pub type EventOf<M> = <<M as DeciderModel>::Decider as Decider>::Event;
/// State type of model `M`'s decider.
pub type StateOf<M> = <<M as DeciderModel>::Decider as Decider>::State;
/// The generated transition for model `M`.
pub type TurnOf<M> = Turn<InputOf<M>>;

/// One generated turn: the input plus everything the host would supply.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Turn<I> {
    /// The input handed to `decide`.
    pub input: I,
    /// Host observations (time, entropy, world).
    pub observations: Observations,
    /// Envelope caps the turn runs under.
    pub limits: EnvelopeLimits,
}

/// Counts that characterize a decision without its payloads.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct DecisionShape {
    /// Ledger events.
    pub events: usize,
    /// `perform` effects.
    pub performs: usize,
    /// Timer effects (schedule + cancel).
    pub timers: usize,
}

impl DecisionShape {
    /// The shape of an actual decision.
    #[must_use]
    pub fn of<E, C, T>(decision: &Decision<E, C, T>) -> Self {
        Self {
            events: decision.events.len(),
            performs: decision
                .effects
                .iter()
                .filter(|effect| effect.is_perform())
                .count(),
            timers: decision
                .effects
                .iter()
                .filter(|effect| effect.is_timer())
                .count(),
        }
    }
}

/// What the model predicts `decide` does, ignoring world and envelope rules
/// (the harness applies those itself, identically for every decider).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Prediction<S> {
    /// `decide` accepts with a decision of this shape; the model moves to
    /// `next` if the kernel then admits the turn.
    Accept {
        /// Model state after the turn.
        next: S,
        /// Expected decision shape.
        shape: DecisionShape,
    },
    /// `decide` returns a domain rejection.
    Reject,
}

/// The kernel-level outcome of one turn.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Outcome {
    /// `step` returned `Ok`.
    Accepted,
    /// `StepError::Rejected`.
    Rejected,
    /// `StepError::ReadOnlyWorld`.
    ReadOnlyWorld,
    /// `StepError::PerformOutsideMain`.
    PerformOutsideMain,
    /// `StepError::EnvelopeExceeded` for this cap.
    EnvelopeExceeded(EnvelopeKind),
}

impl Outcome {
    /// Coverage label, `outcome:<name>`.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Accepted => "outcome:accepted",
            Self::Rejected => "outcome:rejected",
            Self::ReadOnlyWorld => "outcome:read_only_world",
            Self::PerformOutsideMain => "outcome:perform_outside_main",
            Self::EnvelopeExceeded(EnvelopeKind::Events) => "outcome:envelope_exceeded:events",
            Self::EnvelopeExceeded(EnvelopeKind::Commands) => "outcome:envelope_exceeded:commands",
            Self::EnvelopeExceeded(EnvelopeKind::Timers) => "outcome:envelope_exceeded:timers",
        }
    }
}

impl fmt::Display for Outcome {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.label())
    }
}

/// An independent reference model of one [`Decider`].
///
/// Keep the model deliberately simpler than the decider (a transition table
/// over a small abstract state); a model that calls the decider proves
/// nothing. The harness supplies the kernel rules (worlds, envelope caps,
/// replay), so the model predicts only the domain verdict and decision shape.
pub trait DeciderModel: 'static {
    /// The decider under test.
    type Decider: Decider<
            State: fmt::Debug + PartialEq,
            Input: Clone + fmt::Debug + PartialEq + 'static,
            Event: Clone + fmt::Debug + PartialEq,
            Command: fmt::Debug + PartialEq,
            Rejection: PartialEq,
        >;
    /// Abstract model state.
    type Model: Clone + fmt::Debug + PartialEq + 'static;

    /// Builds the decider for one test case.
    fn decider() -> Self::Decider;

    /// The model state matching `Decider::initial_state`.
    fn initial_model() -> Self::Model;

    /// Inputs to generate from `model`. Include inputs the decider must
    /// reject: rejections are half of what the harness checks.
    fn inputs(model: &Self::Model) -> BoxedStrategy<InputOf<Self>>;

    /// Full turns to generate from `model`. The default pairs
    /// [`DeciderModel::inputs`] with [`strategy::observations`] and
    /// [`strategy::limits`]; override to constrain time or worlds.
    fn turns(model: &Self::Model) -> BoxedStrategy<TurnOf<Self>> {
        (
            Self::inputs(model),
            strategy::observations(),
            strategy::limits(),
        )
            .prop_map(|(input, observations, limits)| Turn {
                input,
                observations,
                limits,
            })
            .boxed()
    }

    /// Predicts `decide` for `input` against `model`.
    fn predict(
        model: &Self::Model,
        input: &InputOf<Self>,
        observations: &Observations,
    ) -> Prediction<Self::Model>;

    /// Abstracts a decider state into the model's terms.
    fn project(state: &StateOf<Self>) -> Self::Model;

    /// Coverage label for an input (`input:<label>`), e.g. its variant name.
    fn input_label(input: &InputOf<Self>) -> &'static str;

    /// Coverage label for an event (`event:<label>`); defaults to `event`.
    fn event_label(event: &EventOf<Self>) -> &'static str {
        let _ = event;
        "event"
    }
}
