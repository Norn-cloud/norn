//! Self-tests for the harness: a correct toy decider passes with full kernel
//! coverage and a reproducible seed, and three deliberate mutants are each
//! caught and shrunk to their minimal counterexample.

use std::cell::Cell;
use std::fmt;

use norn_kernel::{
    Command, CommandId, Decider, Decision, Effect, FreshnessPolicy, Generation, IdempotencyKey,
    Observations, Timer, TimerId,
};
use norn_testkit::proptest::prelude::*;
use norn_testkit::{
    DeciderModel, DecisionShape, KERNEL_OUTCOMES, Prediction, Settings, StateOf, check, run,
};

#[derive(Debug, Clone, PartialEq, Eq)]
enum Input {
    Inc,
    Dec,
    Ping,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Event {
    Incremented,
    Decremented,
    Pinged,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Pong;

#[derive(Debug, Clone, PartialEq, Eq)]
struct BelowZero;

impl fmt::Display for BelowZero {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("counter is already zero")
    }
}

impl std::error::Error for BelowZero {}

#[derive(Debug, Clone, PartialEq, Eq)]
struct State {
    count: u32,
    pings: u32,
}

/// Which deliberate bug the counter carries.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mutant {
    None,
    /// `evolve` ignores `Decremented` once the count reaches 3.
    EvolveDropsDecrementAtThree,
    /// `decide` alternates between one and two events for `Ping`.
    NondeterministicPing,
    /// `decide` accepts `Dec` at zero with an empty decision.
    AcceptsDecrementAtZero,
    /// `decide` alternates its answer, but only for historical observations
    /// (which `step` refuses before consulting `decide`).
    NondeterministicWhenHistorical,
}

struct Counter {
    mutant: Mutant,
    flip: Cell<bool>,
}

impl Decider for Counter {
    type State = State;
    type Input = Input;
    type Event = Event;
    type Command = Pong;
    type Rejection = BelowZero;

    fn initial_state(&self) -> State {
        State { count: 0, pings: 0 }
    }

    fn decide(
        &self,
        state: &State,
        input: &Input,
        obs: &Observations,
    ) -> Result<Decision<Event, Pong, Input>, BelowZero> {
        if self.mutant == Mutant::NondeterministicWhenHistorical
            && !obs.world.is_writable()
            && self.flip.replace(!self.flip.get())
        {
            return Ok(Decision::events(vec![Event::Pinged, Event::Pinged]));
        }
        match input {
            Input::Inc => Ok(Decision::events(vec![Event::Incremented])),
            Input::Dec if state.count == 0 => {
                if self.mutant == Mutant::AcceptsDecrementAtZero {
                    Ok(Decision::none())
                } else {
                    Err(BelowZero)
                }
            }
            Input::Dec => Ok(Decision::events(vec![Event::Decremented])),
            Input::Ping => {
                let id = format!("ping-{}", state.pings);
                let mut decision = Decision::events(vec![Event::Pinged])
                    .with_effect(Effect::Perform(Command {
                        id: CommandId(id.clone()),
                        generation: Generation::default(),
                        idempotency_key: IdempotencyKey(id.clone()),
                        freshness: FreshnessPolicy::AppendOnly,
                        deadline: None,
                        payload: Pong,
                    }))
                    .with_effect(Effect::Schedule(Timer {
                        id: TimerId(id),
                        due: obs.now,
                        payload: Input::Ping,
                    }));
                if self.mutant == Mutant::NondeterministicPing
                    && self.flip.replace(!self.flip.get())
                {
                    decision = decision.with_event(Event::Pinged);
                }
                Ok(decision)
            }
        }
    }

    fn evolve(&self, state: &State, event: &Event) -> State {
        let mut next = state.clone();
        match event {
            Event::Incremented => next.count = next.count.saturating_add(1),
            Event::Decremented
                if self.mutant == Mutant::EvolveDropsDecrementAtThree && state.count >= 3 => {}
            Event::Decremented => next.count = next.count.saturating_sub(1),
            Event::Pinged => next.pings = next.pings.saturating_add(1),
        }
        next
    }
}

/// The reference model: just the count. `M` selects the mutant.
struct CounterModel<const M: u8>;

const fn mutant(tag: u8) -> Mutant {
    match tag {
        1 => Mutant::EvolveDropsDecrementAtThree,
        2 => Mutant::NondeterministicPing,
        3 => Mutant::AcceptsDecrementAtZero,
        4 => Mutant::NondeterministicWhenHistorical,
        _ => Mutant::None,
    }
}

const ONE_EVENT: DecisionShape = DecisionShape {
    events: 1,
    performs: 0,
    timers: 0,
};

impl<const M: u8> DeciderModel for CounterModel<M> {
    type Decider = Counter;
    type Model = u32;

    fn decider() -> Counter {
        Counter {
            mutant: mutant(M),
            flip: Cell::new(false),
        }
    }

    fn initial_model() -> u32 {
        0
    }

    fn inputs(_: &u32) -> BoxedStrategy<Input> {
        prop_oneof![Just(Input::Dec), Just(Input::Inc), Just(Input::Ping)].boxed()
    }

    fn predict(count: &u32, input: &Input, _: &Observations) -> Prediction<u32> {
        match input {
            Input::Inc => Prediction::Accept {
                next: count.saturating_add(1),
                shape: ONE_EVENT,
            },
            Input::Dec if *count == 0 => Prediction::Reject,
            Input::Dec => Prediction::Accept {
                next: count.saturating_sub(1),
                shape: ONE_EVENT,
            },
            Input::Ping => Prediction::Accept {
                next: *count,
                shape: DecisionShape {
                    events: 1,
                    performs: 1,
                    timers: 1,
                },
            },
        }
    }

    fn project(state: &StateOf<Self>) -> u32 {
        state.count
    }

    fn input_label(input: &Input) -> &'static str {
        match input {
            Input::Inc => "inc",
            Input::Dec => "dec",
            Input::Ping => "ping",
        }
    }

    fn event_label(event: &Event) -> &'static str {
        match event {
            Event::Incremented => "incremented",
            Event::Decremented => "decremented",
            Event::Pinged => "pinged",
        }
    }
}

#[test]
fn correct_decider_passes_with_full_kernel_coverage() {
    let coverage = check::<CounterModel<0>>(&Settings::new(file!()));
    coverage.assert_covered(KERNEL_OUTCOMES);
    coverage.assert_covered(&[
        "input:inc",
        "input:dec",
        "input:ping",
        "event:incremented",
        "event:decremented",
        "event:pinged",
        "effect:perform",
        "effect:schedule",
        "world:main",
        "world:fork",
        "world:historical",
    ]);
}

#[test]
fn fixed_seed_is_reproducible() {
    let settings = Settings::new(file!()).cases(8);
    let first = check::<CounterModel<0>>(&settings);
    let second = check::<CounterModel<0>>(&settings);
    assert_eq!(first, second);
    // Under a `PROPTEST_RNG_SEED` override every run uses the env seed, so
    // the in-code seed cannot change the exploration.
    if std::env::var_os("PROPTEST_RNG_SEED").is_none() {
        let other = check::<CounterModel<0>>(&settings.seed(7));
        assert_ne!(first, other, "a different seed should explore differently");
    }
}

fn inputs_of(failure: &norn_testkit::Failure<norn_testkit::TurnOf<CounterModel<1>>>) -> Vec<Input> {
    failure
        .transitions
        .iter()
        .map(|turn| turn.input.clone())
        .collect()
}

#[test]
fn mutant_evolve_is_caught_and_shrunk_to_minimal_sequence() {
    let failure = run::<CounterModel<1>>(&Settings::new(file!()).without_persistence())
        .expect_err("mutant 1 must be caught");
    assert_eq!(
        inputs_of(&failure),
        [Input::Inc, Input::Inc, Input::Inc, Input::Dec],
        "{failure}"
    );
    assert!(
        failure.reason.contains("refine the model"),
        "{}",
        failure.reason
    );
}

#[test]
fn mutant_nondeterministic_decide_is_caught() {
    let failure = run::<CounterModel<2>>(&Settings::new(file!()).without_persistence())
        .expect_err("mutant 2 must be caught");
    assert_eq!(failure.transitions.len(), 1, "{failure}");
    assert_eq!(failure.transitions[0].input, Input::Ping, "{failure}");
    assert!(
        failure.reason.contains("not deterministic"),
        "{}",
        failure.reason
    );
}

#[test]
fn mutant_accepting_a_rejection_is_caught() {
    let failure = run::<CounterModel<3>>(&Settings::new(file!()).without_persistence())
        .expect_err("mutant 3 must be caught");
    assert_eq!(failure.transitions.len(), 1, "{failure}");
    assert_eq!(failure.transitions[0].input, Input::Dec, "{failure}");
    assert!(
        failure.reason.contains("disagree on the verdict"),
        "{}",
        failure.reason
    );
}

#[test]
fn mutant_nondeterministic_only_in_historical_world_is_caught() {
    let failure = run::<CounterModel<4>>(&Settings::new(file!()).without_persistence())
        .expect_err("mutant 4 must be caught");
    assert_eq!(failure.transitions.len(), 1, "{failure}");
    assert!(
        matches!(
            failure.transitions[0].observations.world,
            norn_kernel::World::Historical { .. }
        ),
        "{failure}"
    );
    assert!(
        failure.reason.contains("decide is not deterministic"),
        "{}",
        failure.reason
    );
}
