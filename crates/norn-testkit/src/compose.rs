//! Decider composition for the monoid law (T2-KER-3).
//!
//! Deciders form a monoid under [`Product`]: two independent deciders run
//! side by side over the product of their states, with inputs, events,
//! commands, and rejections tagged [`Either`] side. [`Neutral`] (no inputs,
//! unit state) is the identity. [`product_is_interleaving`] states the law
//! that makes this a monoid: driving a product over an interleaved turn
//! sequence is exactly driving each component over its own turns. Identity
//! and associativity (up to the canonical re-tagging) follow from it, and the
//! kernel test suite checks both directly.
//!
//! Timer and command ids are namespaced per side (`l/`, `r/`) so the two
//! components cannot replace or cancel each other's timers in the cell's
//! timer table.
//!
//! This is test support: the kernel has no composition API. It exists to
//! state the composition law against real deciders.

use std::convert::Infallible;
use std::fmt;

use norn_kernel::{
    Command, CommandId, Decider, Decision, DecisionOf, Effect, Observations, Timer, TimerId, step,
};
use proptest::prelude::*;
use proptest::test_runner::TestCaseError;

use crate::laws::{Checkable, EffectOf, Trace, drive};
use crate::model::{DeciderModel, EventOf, InputOf, Prediction, StateOf, Turn};

/// A value from the left or the right component.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Either<L, R> {
    /// The left component's.
    Left(L),
    /// The right component's.
    Right(R),
}

impl<L, R> Either<L, R> {
    /// The left value, if any.
    pub fn left(self) -> Option<L> {
        match self {
            Self::Left(left) => Some(left),
            Self::Right(_) => None,
        }
    }

    /// The right value, if any.
    pub fn right(self) -> Option<R> {
        match self {
            Self::Left(_) => None,
            Self::Right(right) => Some(right),
        }
    }
}

impl<L: fmt::Display, R: fmt::Display> fmt::Display for Either<L, R> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Left(left) => write!(f, "left: {left}"),
            Self::Right(right) => write!(f, "right: {right}"),
        }
    }
}

impl<L: std::error::Error, R: std::error::Error> std::error::Error for Either<L, R> {}

/// Which side of a [`Product`] a value belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Side {
    /// The left component.
    Left,
    /// The right component.
    Right,
}

impl Side {
    const fn prefix(self) -> &'static str {
        match self {
            Self::Left => "l/",
            Self::Right => "r/",
        }
    }
}

/// Re-tags one component effect as a product effect: payloads are mapped
/// with `command` / `input`, and command and timer ids are namespaced by
/// `side`.
pub fn lift_effect<C, I, PC, PI>(
    side: Side,
    effect: Effect<C, I>,
    command: impl FnOnce(C) -> PC,
    input: impl FnOnce(I) -> PI,
) -> Effect<PC, PI> {
    let prefix = side.prefix();
    match effect {
        Effect::Perform(cmd) => Effect::Perform(Command {
            id: CommandId(format!("{prefix}{}", cmd.id.0)),
            generation: cmd.generation,
            idempotency_key: cmd.idempotency_key,
            freshness: cmd.freshness,
            deadline: cmd.deadline,
            payload: command(cmd.payload),
        }),
        Effect::Schedule(timer) => Effect::Schedule(Timer {
            id: TimerId(format!("{prefix}{}", timer.id.0)),
            due: timer.due,
            payload: input(timer.payload),
        }),
        Effect::CancelTimer(id) => Effect::CancelTimer(TimerId(format!("{prefix}{}", id.0))),
    }
}

/// A left-component effect as a product effect.
pub fn lift_left<A: Decider, B: Decider>(effect: EffectOf<A>) -> EffectOf<Product<A, B>> {
    lift_effect(Side::Left, effect, Either::Left, Either::Left)
}

/// A right-component effect as a product effect.
pub fn lift_right<A: Decider, B: Decider>(effect: EffectOf<B>) -> EffectOf<Product<A, B>> {
    lift_effect(Side::Right, effect, Either::Right, Either::Right)
}

/// Two independent deciders side by side.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Product<A, B>(pub A, pub B);

impl<A: Decider, B: Decider> Decider for Product<A, B> {
    type State = (A::State, B::State);
    type Input = Either<A::Input, B::Input>;
    type Event = Either<A::Event, B::Event>;
    type Command = Either<A::Command, B::Command>;
    type Rejection = Either<A::Rejection, B::Rejection>;

    fn initial_state(&self) -> Self::State {
        (self.0.initial_state(), self.1.initial_state())
    }

    fn decide(
        &self,
        state: &Self::State,
        input: &Self::Input,
        observations: &Observations,
    ) -> Result<DecisionOf<Self>, Self::Rejection> {
        match input {
            Either::Left(input) => {
                let Decision { events, effects } = self
                    .0
                    .decide(&state.0, input, observations)
                    .map_err(Either::Left)?;
                Ok(Decision {
                    events: events.into_iter().map(Either::Left).collect(),
                    effects: effects.into_iter().map(lift_left::<A, B>).collect(),
                })
            }
            Either::Right(input) => {
                let Decision { events, effects } = self
                    .1
                    .decide(&state.1, input, observations)
                    .map_err(Either::Right)?;
                Ok(Decision {
                    events: events.into_iter().map(Either::Right).collect(),
                    effects: effects.into_iter().map(lift_right::<A, B>).collect(),
                })
            }
        }
    }

    fn evolve(&self, state: &Self::State, event: &Self::Event) -> Self::State {
        match event {
            Either::Left(event) => (self.0.evolve(&state.0, event), state.1.clone()),
            Either::Right(event) => (state.0.clone(), self.1.evolve(&state.1, event)),
        }
    }
}

/// The identity of [`Product`]: no inputs, no events, unit state.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Neutral;

impl Decider for Neutral {
    type State = ();
    type Input = Infallible;
    type Event = Infallible;
    type Command = Infallible;
    type Rejection = Infallible;

    fn initial_state(&self) {}

    fn decide(
        &self,
        (): &(),
        input: &Infallible,
        _: &Observations,
    ) -> Result<DecisionOf<Self>, Infallible> {
        match *input {}
    }

    fn evolve(&self, (): &(), event: &Infallible) {
        match *event {}
    }
}

/// The reference model of `Product<MA::Decider, MB::Decider>`: the pair of
/// component models, each predicting its own side.
pub struct ProductModel<MA, MB>(std::marker::PhantomData<fn() -> (MA, MB)>);

impl<MA: DeciderModel, MB: DeciderModel> DeciderModel for ProductModel<MA, MB>
where
    MA::Decider: Checkable,
    MB::Decider: Checkable,
    StateOf<MA>: Clone,
    StateOf<MB>: Clone,
{
    type Decider = Product<MA::Decider, MB::Decider>;
    type Model = (MA::Model, MB::Model);

    fn decider() -> Self::Decider {
        Product(MA::decider(), MB::decider())
    }

    fn initial_model() -> Self::Model {
        (MA::initial_model(), MB::initial_model())
    }

    fn inputs(model: &Self::Model) -> BoxedStrategy<InputOf<Self>> {
        prop_oneof![
            MA::inputs(&model.0).prop_map(Either::Left),
            MB::inputs(&model.1).prop_map(Either::Right),
        ]
        .boxed()
    }

    fn predict(
        model: &Self::Model,
        input: &InputOf<Self>,
        observations: &Observations,
    ) -> Prediction<Self::Model> {
        match input {
            Either::Left(input) => match MA::predict(&model.0, input, observations) {
                Prediction::Accept { next, shape } => Prediction::Accept {
                    next: (next, model.1.clone()),
                    shape,
                },
                Prediction::Reject => Prediction::Reject,
            },
            Either::Right(input) => match MB::predict(&model.1, input, observations) {
                Prediction::Accept { next, shape } => Prediction::Accept {
                    next: (model.0.clone(), next),
                    shape,
                },
                Prediction::Reject => Prediction::Reject,
            },
        }
    }

    fn project(state: &StateOf<Self>) -> Self::Model {
        (MA::project(&state.0), MB::project(&state.1))
    }

    fn input_label(input: &InputOf<Self>) -> &'static str {
        match input {
            Either::Left(input) => MA::input_label(input),
            Either::Right(input) => MB::input_label(input),
        }
    }

    fn event_label(event: &EventOf<Self>) -> &'static str {
        match event {
            Either::Left(event) => MA::event_label(event),
            Either::Right(event) => MB::event_label(event),
        }
    }
}

fn component_turns<I: Clone, T>(turns: &[Turn<I>], pick: impl Fn(&I) -> Option<T>) -> Vec<Turn<T>> {
    turns
        .iter()
        .filter_map(|turn| {
            pick(&turn.input).map(|input| Turn {
                input,
                observations: turn.observations.clone(),
                limits: turn.limits,
            })
        })
        .collect()
}

/// A component trace after its first `done` turns: its state and ledger.
fn component_prefix<D: Decider>(trace: &Trace<D>, done: usize) -> (&D::State, &[D::Event]) {
    let Some(last) = done.checked_sub(1) else {
        return (&trace.initial, &[]);
    };
    let state = trace.states.get(last).unwrap_or(&trace.initial);
    let len = trace.turns.get(last).map_or(0, |record| record.ledger_len);
    (state, trace.ledger.get(..len).unwrap_or_default())
}

/// Per-turn committed effects of a component trace, in turn order.
fn effects_per_turn<D: Decider>(trace: &Trace<D>) -> Vec<&[EffectOf<D>]> {
    let mut start = 0;
    trace
        .turns
        .iter()
        .map(|record| {
            let slice = trace
                .effects
                .get(start..record.effects_len)
                .unwrap_or_default();
            start = record.effects_len;
            slice
        })
        .collect()
}

/// **Composition (monoid) law**: `product` behaves as `a` and `b` run
/// independently.
///
/// Drives `product` over `turns` and each component over the turns tagged
/// for it, all through [`norn_kernel::step`]. Then, after **every** turn:
/// the product accepted exactly when the component did; its state is the
/// pair of the components' states; its ledger so far is the interleaving of
/// the components' ledgers so far; and its effect queue so far is the
/// interleaving of their re-tagged ([`lift_effect`]) effects.
///
/// # Errors
///
/// The first difference.
pub fn product_is_interleaving<A, B, P>(
    product: &P,
    a: &A,
    b: &B,
    initial: &(A::State, B::State),
    turns: &[Turn<Either<A::Input, B::Input>>],
) -> Result<(), TestCaseError>
where
    A: Checkable,
    B: Checkable,
    P: Checkable
        + Decider<
            State = (A::State, B::State),
            Input = Either<A::Input, B::Input>,
            Event = Either<A::Event, B::Event>,
            Command = Either<A::Command, B::Command>,
        >,
    A::Command: Clone,
    B::Command: Clone,
{
    let whole = drive(step::<P>, product, initial.clone(), turns);
    let left = drive(
        step::<A>,
        a,
        initial.0.clone(),
        &component_turns(turns, |input| input.clone().left()),
    );
    let right = drive(
        step::<B>,
        b,
        initial.1.clone(),
        &component_turns(turns, |input| input.clone().right()),
    );

    let mut left_turns = left.turns.iter().zip(effects_per_turn(&left));
    let mut right_turns = right.turns.iter().zip(effects_per_turn(&right));
    let (mut left_done, mut right_done) = (0_usize, 0_usize);
    let mut expected_effects = Vec::new();
    for (ix, ((turn, record), state)) in turns
        .iter()
        .zip(&whole.turns)
        .zip(&whole.states)
        .enumerate()
    {
        let component = match &turn.input {
            Either::Left(_) => left_turns.next().map(|(component, effects)| {
                left_done = left_done.saturating_add(1);
                expected_effects.extend(effects.iter().cloned().map(lift_left::<A, B>));
                component.accepted
            }),
            Either::Right(_) => right_turns.next().map(|(component, effects)| {
                right_done = right_done.saturating_add(1);
                expected_effects.extend(effects.iter().cloned().map(lift_right::<A, B>));
                component.accepted
            }),
        };
        prop_assert_eq!(
            Some(record.accepted),
            component,
            "law violated (composition): the product and its component disagree on turn {} ({:?})",
            ix,
            turn
        );

        // After every turn — not only at the end — the product's state,
        // ledger prefix, and effect prefix equal the components'. A product
        // that disturbs the other side and later restores it is caught here.
        let (left_state, left_ledger) = component_prefix(&left, left_done);
        let (right_state, right_ledger) = component_prefix(&right, right_done);
        prop_assert_eq!(
            state,
            &(left_state.clone(), right_state.clone()),
            "law violated (composition): after turn {} the product's state is not the pair of component states ({:?})",
            ix,
            turn
        );
        let ledger = whole.ledger.get(..record.ledger_len).unwrap_or_default();
        let ledger_left: Vec<_> = ledger.iter().cloned().filter_map(Either::left).collect();
        let ledger_right: Vec<_> = ledger.iter().cloned().filter_map(Either::right).collect();
        prop_assert_eq!(
            ledger_left.as_slice(),
            left_ledger,
            "law violated (composition): after turn {} the product's left ledger differs from the left component's",
            ix
        );
        prop_assert_eq!(
            ledger_right.as_slice(),
            right_ledger,
            "law violated (composition): after turn {} the product's right ledger differs from the right component's",
            ix
        );
        prop_assert_eq!(
            whole.effects.get(..record.effects_len).unwrap_or_default(),
            expected_effects.as_slice(),
            "law violated (composition): after turn {} the product's effects are not the components' re-tagged effects",
            ix
        );
    }
    Ok(())
}
