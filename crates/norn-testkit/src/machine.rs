//! [`DeciderMachine`]: the `proptest-state-machine` adapter for a
//! [`DeciderModel`].

use std::marker::PhantomData;

use norn_kernel::{
    Decider, Effect, EnvelopeKind, StepError, StepResult, World, fold, replay, step,
};
use proptest::strategy::{BoxedStrategy, Just, Strategy};
use proptest_state_machine::{ReferenceStateMachine, StateMachineTest};

use crate::coverage;
use crate::laws;
use crate::model::{DeciderModel, DecisionShape, Outcome, Prediction, Turn, TurnOf};

/// The reference and concrete machine for model `M`.
///
/// Implements both halves of `proptest-state-machine`: the reference side
/// advances `M::Model` with [`DeciderModel::predict`] plus the kernel rules,
/// and the concrete side runs the real decider through
/// [`step`](norn_kernel::step) and checks the Decider laws (crate docs).
pub struct DeciderMachine<M>(PhantomData<fn() -> M>);

/// What the reference side expects of the last applied turn.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Expected {
    outcome: Outcome,
    /// The decision shape when the world is writable and the model accepts;
    /// `None` when the model rejects (or `decide` is never consulted).
    shape: Option<DecisionShape>,
}

/// Reference-machine state: the abstract model plus the expectation for the
/// most recent turn.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReferenceState<S> {
    /// The abstract model state after the applied turns.
    pub model: S,
    expected: Option<Expected>,
}

/// The concrete side: the decider, its materialized state, and the ledger of
/// every accepted event.
pub struct SystemUnderTest<D: Decider> {
    decider: D,
    state: D::State,
    ledger: Vec<D::Event>,
}

impl<D: Decider> SystemUnderTest<D> {
    /// Materialized state after the accepted turns.
    pub const fn state(&self) -> &D::State {
        &self.state
    }

    /// Every accepted event, in commit order.
    pub fn ledger(&self) -> &[D::Event] {
        &self.ledger
    }
}

/// Kernel rules applied to a model prediction: the expected outcome of `turn`.
fn expect<S>(turn: &Turn<impl Sized>, prediction: &Prediction<S>) -> Expected {
    let world = &turn.observations.world;
    if !world.is_writable() {
        return Expected {
            outcome: Outcome::ReadOnlyWorld,
            shape: None,
        };
    }
    let Prediction::Accept { shape, .. } = prediction else {
        return Expected {
            outcome: Outcome::Rejected,
            shape: None,
        };
    };
    let limits = &turn.limits;
    let over = [
        (EnvelopeKind::Events, shape.events, limits.max_events),
        (EnvelopeKind::Commands, shape.performs, limits.max_commands),
        (EnvelopeKind::Timers, shape.timers, limits.max_timers),
    ]
    .into_iter()
    .find(|(_, actual, limit)| laws::exceeds(*actual, *limit));
    let outcome = match over {
        Some((kind, _, _)) => Outcome::EnvelopeExceeded(kind),
        None if shape.performs > 0 && !world.may_perform() => Outcome::PerformOutsideMain,
        None => Outcome::Accepted,
    };
    Expected {
        outcome,
        shape: Some(*shape),
    }
}

impl<M: DeciderModel> ReferenceStateMachine for DeciderMachine<M> {
    type State = ReferenceState<M::Model>;
    type Transition = TurnOf<M>;

    fn init_state() -> BoxedStrategy<Self::State> {
        Just(ReferenceState {
            model: M::initial_model(),
            expected: None,
        })
        .boxed()
    }

    fn transitions(state: &Self::State) -> BoxedStrategy<Self::Transition> {
        M::turns(&state.model)
    }

    fn apply(state: Self::State, turn: &Self::Transition) -> Self::State {
        let prediction = M::predict(&state.model, &turn.input, &turn.observations);
        let expected = expect(turn, &prediction);
        let model = match prediction {
            Prediction::Accept { next, .. } if expected.outcome == Outcome::Accepted => next,
            Prediction::Accept { .. } | Prediction::Reject => state.model,
        };
        ReferenceState {
            model,
            expected: Some(expected),
        }
    }
}

/// An independent oracle for [`step`]: [`laws::kernel_rules`] applied to one
/// `decide` call.
///
/// Also asserts `decide` is deterministic for every turn (historical worlds
/// included, even though `step` never consults it there) and, in writable
/// worlds, agrees with the model's verdict and decision shape.
fn oracle<M: DeciderModel>(
    decider: &M::Decider,
    before: &<M::Decider as Decider>::State,
    turn: &TurnOf<M>,
    expected: Expected,
) -> StepResult<M::Decider> {
    // Determinism is checked for every generated observation, including
    // historical worlds that `step` refuses before consulting `decide`.
    let first = decider.decide(before, &turn.input, &turn.observations);
    let second = decider.decide(before, &turn.input, &turn.observations);
    assert_eq!(
        first, second,
        "law violated: decide is not deterministic for identical state, input, and observations\nturn: {turn:?}"
    );
    if turn.observations.world.is_writable() {
        assert_eq!(
            first.as_ref().ok().map(DecisionShape::of),
            expected.shape,
            "decider and model disagree on the verdict or decision shape (None = rejected)\nstate: {before:?}\nturn: {turn:?}\ndecision: {first:?}"
        );
    }
    laws::kernel_rules(decider, before, turn, first)
}

fn outcome_of<R>(result: &Result<impl Sized, StepError<R>>) -> Outcome {
    match result {
        Ok(_) => Outcome::Accepted,
        Err(StepError::Rejected(_)) => Outcome::Rejected,
        Err(StepError::ReadOnlyWorld) => Outcome::ReadOnlyWorld,
        Err(StepError::PerformOutsideMain { .. }) => Outcome::PerformOutsideMain,
        Err(StepError::EnvelopeExceeded { kind, .. }) => Outcome::EnvelopeExceeded(*kind),
    }
}

const fn world_label(world: &World) -> &'static str {
    match world {
        World::Main => "main",
        World::Fork { .. } => "fork",
        World::Historical { .. } => "historical",
    }
}

const fn effect_label<C, T>(effect: &Effect<C, T>) -> &'static str {
    match effect {
        Effect::Perform(_) => "perform",
        Effect::Schedule(_) => "schedule",
        Effect::CancelTimer(_) => "cancel_timer",
    }
}

impl<M: DeciderModel> StateMachineTest for DeciderMachine<M> {
    type SystemUnderTest = SystemUnderTest<M::Decider>;
    type Reference = Self;

    fn init_test(_: &ReferenceState<M::Model>) -> Self::SystemUnderTest {
        let decider = M::decider();
        let state = decider.initial_state();
        SystemUnderTest {
            decider,
            state,
            ledger: Vec::new(),
        }
    }

    fn apply(
        mut sut: Self::SystemUnderTest,
        reference: &ReferenceState<M::Model>,
        turn: TurnOf<M>,
    ) -> Self::SystemUnderTest {
        coverage::hit_prefixed("input", M::input_label(&turn.input));
        coverage::hit_prefixed("world", world_label(&turn.observations.world));
        let Some(expected) = reference.expected else {
            unreachable!("the reference machine records an expectation for every applied turn")
        };

        let before = sut.state.clone();
        let actual = step(
            &sut.decider,
            &before,
            &turn.input,
            &turn.observations,
            &turn.limits,
        );
        let again = step(
            &sut.decider,
            &before,
            &turn.input,
            &turn.observations,
            &turn.limits,
        );
        assert_eq!(
            actual, again,
            "law violated: step is not deterministic\nturn: {turn:?}"
        );
        let predicted = oracle::<M>(&sut.decider, &before, &turn, expected);
        assert_eq!(
            actual, predicted,
            "law violated: step differs from decide → caps → world check → evolve\nstate: {before:?}\nturn: {turn:?}"
        );
        let outcome = outcome_of(&actual);
        assert_eq!(
            outcome, expected.outcome,
            "kernel outcome differs from the model's expectation\nstate: {before:?}\nturn: {turn:?}"
        );
        coverage::hit(outcome.label());

        match actual {
            Ok(accepted) => {
                for event in &accepted.events {
                    coverage::hit_prefixed("event", M::event_label(event));
                }
                for effect in &accepted.effects {
                    coverage::hit_prefixed("effect", effect_label(effect));
                }
                sut.state = accepted.state;
                sut.ledger.extend(accepted.events);
            }
            Err(_) => {
                // A refused turn has no events or effects by construction
                // (`StepError` carries none); the state must be untouched.
                assert_eq!(
                    sut.state, before,
                    "law violated: a refused turn changed state"
                );
            }
        }
        sut
    }

    fn check_invariants(sut: &Self::SystemUnderTest, reference: &ReferenceState<M::Model>) {
        let replayed = replay(&sut.decider, &sut.ledger);
        assert_eq!(
            replayed, sut.state,
            "law violated: replay(ledger) != materialized state"
        );
        // Folding is a monoid action of the ledger on states: resuming a
        // replay from any split point reaches the same state.
        for split in 0..=sut.ledger.len() {
            let (prefix, suffix) = sut.ledger.split_at(split);
            let resumed = fold(&sut.decider, replay(&sut.decider, prefix), suffix);
            assert_eq!(
                resumed, sut.state,
                "law violated: fold(replay(prefix), suffix) != replay(ledger) at split {split}"
            );
        }
        assert_eq!(
            M::project(&sut.state),
            reference.model,
            "decider state does not refine the model"
        );
    }
}
