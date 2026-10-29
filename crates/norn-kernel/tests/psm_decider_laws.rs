//! Decider laws as property tests over toy deciders (T2-KER-3, #2536).
//!
//! Laws (model-free ones from `norn_testkit::laws`, the composition law from
//! `norn_testkit::compose`), each from arbitrary initial states and
//! arbitrary worlds / envelope caps:
//!
//! | law | property | mutant proving it can fail |
//! | --- | --- | --- |
//! | step | `step` = `decide` → world check → caps → perform-outside-main → `fold(evolve)`; a refusal commits zero events and zero effects and leaves state untouched | `psm_mutant_truncating_step_is_caught`, `psm_mutant_fork_sanitizing_step_is_caught` |
//! | replay | state after every turn = `fold(evolve)` over the ledger so far; resuming from every split gives the same state | `psm_mutant_impure_evolve_is_caught` |
//! | purity | `decide` is equal across repeated calls and fresh instances, historical worlds included | `psm_mutant_ambient_decide_in_historical_world_is_caught` |
//! | rejection | a refused turn is invisible: dropping every refused turn yields the same ledger, effects, and state | `psm_mutant_rejection_with_hidden_memory_is_caught` |
//! | composition (monoid) | a `Product` is the independent interleaving of its components; `Neutral` is its identity; it is associative up to re-tagging | `psm_mutant_cross_talk_product_is_caught`, `psm_mutant_transient_cross_talk_product_is_caught` |
//!
//! The model-based harness also runs on the product of the two toys, so its
//! refinement, determinism, and replay checks apply to a composed decider.
//! The real `VersionedAction` decider's laws are in `psm_action_laws.rs`.

use std::cell::Cell;
use std::fmt;
use std::sync::atomic::{AtomicU64, Ordering};

use norn_kernel::{
    Command, CommandId, Decider, Decision, DecisionOf, Effect, EnvelopeLimits, FreshnessPolicy,
    Generation, IdempotencyKey, LogicalTime, Observations, StepResult, Timer, TimerId, World, fold,
    step,
};
use norn_testkit::compose::{Either, Neutral, Product, ProductModel, product_is_interleaving};
use norn_testkit::laws::{
    Checkable, StepFn, decide_is_pure, drive, rejection_is_transparent, replay_is_fold, step_law,
};
use norn_testkit::proptest::prelude::*;
use norn_testkit::proptest::test_runner::TestError;
use norn_testkit::{
    DeciderModel, DecisionShape, KERNEL_OUTCOMES, Prediction, Settings, StateOf, Turn, check,
    check_property, run_property, strategy,
};

// ----------------------------------------------------------- toy: counter

#[derive(Debug, Clone, PartialEq, Eq)]
enum CounterInput {
    Inc,
    /// Rejected at zero.
    Dec,
    /// One event, one perform, one timer.
    Ping,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum CounterEvent {
    Incremented,
    Decremented,
    Pinged,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Pong(u32);

#[derive(Debug, Clone, PartialEq, Eq)]
struct BelowZero;

impl fmt::Display for BelowZero {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("counter is already zero")
    }
}

impl std::error::Error for BelowZero {}

#[derive(Debug, Clone, PartialEq, Eq)]
struct CounterState {
    count: u32,
    pings: u32,
}

/// Which deliberate bug the counter carries.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mutant {
    None,
    /// `evolve` keeps a hidden call counter and drops every fourth
    /// `Incremented` it is asked to apply.
    ImpureEvolve,
    /// `decide` in a historical world reports a process-global instance
    /// counter (standing in for an ambient clock read at construction):
    /// repeated calls on one instance agree, fresh instances do not.
    AmbientWhenHistorical,
    /// A rejected `Dec` leaves hidden memory; the next `Inc` then emits two
    /// events.
    RejectionMemory,
}

/// Instance counter read by [`Mutant::AmbientWhenHistorical`].
static INSTANCES: AtomicU64 = AtomicU64::new(0);

struct Counter {
    mutant: Mutant,
    instance: u64,
    calls: Cell<u32>,
    strike: Cell<bool>,
}

impl Counter {
    fn new(mutant: Mutant) -> Self {
        Self {
            mutant,
            instance: INSTANCES.fetch_add(1, Ordering::Relaxed),
            calls: Cell::new(0),
            strike: Cell::new(false),
        }
    }
}

impl Decider for Counter {
    type State = CounterState;
    type Input = CounterInput;
    type Event = CounterEvent;
    type Command = Pong;
    type Rejection = BelowZero;

    fn initial_state(&self) -> CounterState {
        CounterState { count: 0, pings: 0 }
    }

    fn decide(
        &self,
        state: &CounterState,
        input: &CounterInput,
        obs: &Observations,
    ) -> Result<DecisionOf<Self>, BelowZero> {
        if self.mutant == Mutant::AmbientWhenHistorical && !obs.world.is_writable() {
            let ambient = u32::try_from(self.instance & u64::from(u32::MAX)).unwrap_or(0);
            return Ok(
                Decision::events(vec![CounterEvent::Pinged]).with_effect(Effect::Perform(
                    Command {
                        id: CommandId("ambient".into()),
                        generation: Generation::default(),
                        idempotency_key: IdempotencyKey("ambient".into()),
                        freshness: FreshnessPolicy::AppendOnly,
                        deadline: None,
                        payload: Pong(ambient),
                    },
                )),
            );
        }
        match input {
            CounterInput::Inc => {
                let mut decision = Decision::events(vec![CounterEvent::Incremented]);
                if self.mutant == Mutant::RejectionMemory && self.strike.get() {
                    decision = decision.with_event(CounterEvent::Incremented);
                }
                Ok(decision)
            }
            CounterInput::Dec if state.count == 0 => {
                if self.mutant == Mutant::RejectionMemory {
                    self.strike.set(true);
                }
                Err(BelowZero)
            }
            CounterInput::Dec => Ok(Decision::events(vec![CounterEvent::Decremented])),
            CounterInput::Ping => {
                let id = format!("ping-{}", state.pings);
                Ok(Decision::events(vec![CounterEvent::Pinged])
                    .with_effect(Effect::Perform(Command {
                        id: CommandId(id.clone()),
                        generation: Generation::default(),
                        idempotency_key: IdempotencyKey(id.clone()),
                        freshness: FreshnessPolicy::AppendOnly,
                        deadline: None,
                        payload: Pong(state.pings),
                    }))
                    .with_effect(Effect::Schedule(Timer {
                        id: TimerId(id),
                        due: obs.now,
                        payload: CounterInput::Ping,
                    })))
            }
        }
    }

    fn evolve(&self, state: &CounterState, event: &CounterEvent) -> CounterState {
        let mut next = state.clone();
        match event {
            CounterEvent::Incremented => {
                let calls = self.calls.get().wrapping_add(1);
                self.calls.set(calls);
                if !(self.mutant == Mutant::ImpureEvolve && calls.is_multiple_of(4)) {
                    next.count = next.count.saturating_add(1);
                }
            }
            CounterEvent::Decremented => next.count = next.count.saturating_sub(1),
            CounterEvent::Pinged => next.pings = next.pings.saturating_add(1),
        }
        next
    }
}

struct CounterModel;

const fn shape(events: usize, performs: usize, timers: usize) -> DecisionShape {
    DecisionShape {
        events,
        performs,
        timers,
    }
}

fn counter_input() -> BoxedStrategy<CounterInput> {
    prop_oneof![
        Just(CounterInput::Inc),
        Just(CounterInput::Dec),
        Just(CounterInput::Ping),
    ]
    .boxed()
}

impl DeciderModel for CounterModel {
    type Decider = Counter;
    type Model = u32;

    fn decider() -> Counter {
        Counter::new(Mutant::None)
    }

    fn initial_model() -> u32 {
        0
    }

    fn inputs(_: &u32) -> BoxedStrategy<CounterInput> {
        counter_input()
    }

    fn predict(count: &u32, input: &CounterInput, _: &Observations) -> Prediction<u32> {
        match input {
            CounterInput::Inc => Prediction::Accept {
                next: count.saturating_add(1),
                shape: shape(1, 0, 0),
            },
            CounterInput::Dec if *count == 0 => Prediction::Reject,
            CounterInput::Dec => Prediction::Accept {
                next: count.saturating_sub(1),
                shape: shape(1, 0, 0),
            },
            CounterInput::Ping => Prediction::Accept {
                next: *count,
                shape: shape(1, 1, 1),
            },
        }
    }

    fn project(state: &StateOf<Self>) -> u32 {
        state.count
    }

    fn input_label(input: &CounterInput) -> &'static str {
        match input {
            CounterInput::Inc => "inc",
            CounterInput::Dec => "dec",
            CounterInput::Ping => "ping",
        }
    }

    fn event_label(event: &CounterEvent) -> &'static str {
        match event {
            CounterEvent::Incremented => "incremented",
            CounterEvent::Decremented => "decremented",
            CounterEvent::Pinged => "pinged",
        }
    }
}

// ---------------------------------------------------------- toy: register

const EXPIRY: &str = "expiry";
const EXPIRY_MS: u64 = 1_000;

#[derive(Debug, Clone, PartialEq, Eq)]
enum RegisterInput {
    Set(u8),
    /// Rejected when empty.
    Clear,
    /// Delivered by `clock`; a no-op once the register is empty.
    Expire,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum RegisterEvent {
    Set(u8),
    Cleared,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Empty;

impl fmt::Display for Empty {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("register is empty")
    }
}

impl std::error::Error for Empty {}

/// A register with an expiry timer: exercises schedule and cancel.
struct Register;

impl Decider for Register {
    type State = Option<u8>;
    type Input = RegisterInput;
    type Event = RegisterEvent;
    type Command = std::convert::Infallible;
    type Rejection = Empty;

    fn initial_state(&self) -> Option<u8> {
        None
    }

    fn decide(
        &self,
        state: &Option<u8>,
        input: &RegisterInput,
        obs: &Observations,
    ) -> Result<DecisionOf<Self>, Empty> {
        match (state, input) {
            (_, RegisterInput::Set(value)) => {
                let due = obs
                    .now
                    .checked_add_millis(EXPIRY_MS)
                    .unwrap_or(LogicalTime::from_millis(u64::MAX));
                Ok(
                    Decision::events(vec![RegisterEvent::Set(*value)]).with_effect(
                        Effect::Schedule(Timer {
                            id: TimerId(EXPIRY.into()),
                            due,
                            payload: RegisterInput::Expire,
                        }),
                    ),
                )
            }
            (None, RegisterInput::Clear) => Err(Empty),
            (Some(_), RegisterInput::Clear) => Ok(Decision::events(vec![RegisterEvent::Cleared])
                .with_effect(Effect::CancelTimer(TimerId(EXPIRY.into())))),
            (None, RegisterInput::Expire) => Ok(Decision::none()),
            (Some(_), RegisterInput::Expire) => Ok(Decision::events(vec![RegisterEvent::Cleared])),
        }
    }

    fn evolve(&self, _: &Option<u8>, event: &RegisterEvent) -> Option<u8> {
        match event {
            RegisterEvent::Set(value) => Some(*value),
            RegisterEvent::Cleared => None,
        }
    }
}

struct RegisterModel;

fn register_input() -> BoxedStrategy<RegisterInput> {
    prop_oneof![
        (0_u8..3).prop_map(RegisterInput::Set),
        Just(RegisterInput::Clear),
        Just(RegisterInput::Expire),
    ]
    .boxed()
}

impl DeciderModel for RegisterModel {
    type Decider = Register;
    type Model = bool;

    fn decider() -> Register {
        Register
    }

    fn initial_model() -> bool {
        false
    }

    fn inputs(_: &bool) -> BoxedStrategy<RegisterInput> {
        register_input()
    }

    fn predict(set: &bool, input: &RegisterInput, _: &Observations) -> Prediction<bool> {
        match (set, input) {
            (_, RegisterInput::Set(_)) => Prediction::Accept {
                next: true,
                shape: shape(1, 0, 1),
            },
            (false, RegisterInput::Clear) => Prediction::Reject,
            (true, RegisterInput::Clear) => Prediction::Accept {
                next: false,
                shape: shape(1, 0, 1),
            },
            (false, RegisterInput::Expire) => Prediction::Accept {
                next: false,
                shape: shape(0, 0, 0),
            },
            (true, RegisterInput::Expire) => Prediction::Accept {
                next: false,
                shape: shape(1, 0, 0),
            },
        }
    }

    fn project(state: &Option<u8>) -> bool {
        state.is_some()
    }

    fn input_label(input: &RegisterInput) -> &'static str {
        match input {
            RegisterInput::Set(_) => "set",
            RegisterInput::Clear => "clear",
            RegisterInput::Expire => "expire",
        }
    }

    fn event_label(event: &RegisterEvent) -> &'static str {
        match event {
            RegisterEvent::Set(_) => "set",
            RegisterEvent::Cleared => "cleared",
        }
    }
}

// -------------------------------------------------------------- strategies

fn turn<I: fmt::Debug + 'static>(input: BoxedStrategy<I>) -> BoxedStrategy<Turn<I>> {
    (input, strategy::observations(), strategy::limits())
        .prop_map(|(input, observations, limits)| Turn {
            input,
            observations,
            limits,
        })
        .boxed()
}

fn turns<I: fmt::Debug + 'static>(input: BoxedStrategy<I>) -> BoxedStrategy<Vec<Turn<I>>> {
    prop::collection::vec(turn(input), 0..24).boxed()
}

fn counter_state() -> BoxedStrategy<CounterState> {
    (0_u32..4, 0_u32..3)
        .prop_map(|(count, pings)| CounterState { count, pings })
        .boxed()
}

fn register_state() -> BoxedStrategy<Option<u8>> {
    prop::option::of(0_u8..3).boxed()
}

type Toys = Product<Counter, Register>;

fn toys() -> Toys {
    Product(Counter::new(Mutant::None), Register)
}

fn toy_input() -> BoxedStrategy<Either<CounterInput, RegisterInput>> {
    prop_oneof![
        counter_input().prop_map(Either::Left),
        register_input().prop_map(Either::Right),
    ]
    .boxed()
}

fn settings() -> Settings {
    Settings::new(file!())
}

/// A mutant run: persistence off (the failure is expected), default seed.
fn mutant_settings() -> Settings {
    Settings::new(file!()).without_persistence()
}

/// The failure message of an expected-to-fail property.
#[track_caller]
fn caught<T: fmt::Debug>(result: Result<(), TestError<T>>) -> String {
    match result {
        Err(TestError::Fail(reason, minimal)) => {
            format!("{}\nminimal: {minimal:?}", reason.message())
        }
        Err(TestError::Abort(reason)) => unreachable!("mutant run aborted: {reason}"),
        Ok(()) => unreachable!("the mutant was not caught"),
    }
}

/// Every model-free law for one decider from one initial state.
fn all_laws<D: Checkable>(
    make: &dyn Fn() -> D,
    initial: &D::State,
    turns: &[Turn<D::Input>],
) -> Result<(), TestCaseError> {
    let decider = make();
    let trace = step_law(step::<D>, &decider, initial.clone(), turns)?;
    replay_is_fold(&decider, &trace)?;
    decide_is_pure(make, initial, turns)?;
    rejection_is_transparent(step::<D>, make, initial, turns)
}

// -------------------------------------------------------- the laws, green

#[test]
fn psm_product_of_toys_obeys_harness_laws() {
    let coverage = check::<ProductModel<CounterModel, RegisterModel>>(&settings());
    coverage.assert_covered(KERNEL_OUTCOMES);
    coverage.assert_covered(&[
        "input:inc",
        "input:dec",
        "input:ping",
        "input:set",
        "input:clear",
        "input:expire",
        "event:incremented",
        "event:decremented",
        "event:pinged",
        "event:set",
        "event:cleared",
        "effect:perform",
        "effect:schedule",
        "effect:cancel_timer",
        "world:fork",
        "world:historical",
    ]);
}

#[test]
fn psm_counter_obeys_model_free_laws_from_any_state() {
    check_property(
        &settings(),
        &(counter_state(), turns(counter_input())),
        |(initial, turns)| all_laws(&|| Counter::new(Mutant::None), &initial, &turns),
    );
}

#[test]
fn psm_register_obeys_model_free_laws_from_any_state() {
    check_property(
        &settings(),
        &(register_state(), turns(register_input())),
        |(initial, turns)| all_laws(&|| Register, &initial, &turns),
    );
}

#[test]
fn psm_product_obeys_model_free_laws_from_any_state() {
    check_property(
        &settings(),
        &((counter_state(), register_state()), turns(toy_input())),
        |(initial, turns)| all_laws(&toys, &initial, &turns),
    );
}

#[test]
fn psm_product_is_interleaving_of_components() {
    check_property(
        &settings(),
        &((counter_state(), register_state()), turns(toy_input())),
        |(initial, turns)| {
            product_is_interleaving(
                &toys(),
                &Counter::new(Mutant::None),
                &Register,
                &initial,
                &turns,
            )
        },
    );
}

#[test]
fn psm_neutral_is_product_identity() {
    check_property(
        &settings(),
        &(counter_state(), turns(counter_input())),
        |(initial, turns)| {
            let counter = || Counter::new(Mutant::None);
            // Left identity: Neutral × D behaves as D.
            let right: Vec<_> = turns
                .iter()
                .map(|turn| retag(turn, Either::<std::convert::Infallible, _>::Right))
                .collect();
            product_is_interleaving(
                &Product(Neutral, counter()),
                &Neutral,
                &counter(),
                &((), initial.clone()),
                &right,
            )?;
            // Right identity: D × Neutral behaves as D.
            let left: Vec<_> = turns
                .iter()
                .map(|turn| retag(turn, Either::<_, std::convert::Infallible>::Left))
                .collect();
            product_is_interleaving(
                &Product(counter(), Neutral),
                &counter(),
                &Neutral,
                &(initial, ()),
                &left,
            )
        },
    );
}

fn retag<I: Clone, J>(turn: &Turn<I>, tag: impl FnOnce(I) -> J) -> Turn<J> {
    Turn {
        input: tag(turn.input.clone()),
        observations: turn.observations.clone(),
        limits: turn.limits,
    }
}

/// Which of three components a turn targets.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Tri<A, B, C> {
    A(A),
    B(B),
    C(C),
}

type LeftNested = Product<Product<Counter, Register>, Counter>;
type RightNested = Product<Counter, Product<Register, Counter>>;

fn left_nested_input(
    input: Tri<CounterInput, RegisterInput, CounterInput>,
) -> Either<Either<CounterInput, RegisterInput>, CounterInput> {
    match input {
        Tri::A(a) => Either::Left(Either::Left(a)),
        Tri::B(b) => Either::Left(Either::Right(b)),
        Tri::C(c) => Either::Right(c),
    }
}

fn right_nested_input(
    input: Tri<CounterInput, RegisterInput, CounterInput>,
) -> Either<CounterInput, Either<RegisterInput, CounterInput>> {
    match input {
        Tri::A(a) => Either::Left(a),
        Tri::B(b) => Either::Right(Either::Left(b)),
        Tri::C(c) => Either::Right(Either::Right(c)),
    }
}

fn left_nested_event(
    event: &Either<Either<CounterEvent, RegisterEvent>, CounterEvent>,
) -> Tri<CounterEvent, RegisterEvent, CounterEvent> {
    match event.clone() {
        Either::Left(Either::Left(a)) => Tri::A(a),
        Either::Left(Either::Right(b)) => Tri::B(b),
        Either::Right(c) => Tri::C(c),
    }
}

fn right_nested_event(
    event: &Either<CounterEvent, Either<RegisterEvent, CounterEvent>>,
) -> Tri<CounterEvent, RegisterEvent, CounterEvent> {
    match event.clone() {
        Either::Left(a) => Tri::A(a),
        Either::Right(Either::Left(b)) => Tri::B(b),
        Either::Right(Either::Right(c)) => Tri::C(c),
    }
}

/// An effect in canonical, nesting-free form: which component, its local
/// timer/command id, and the kind.
fn canonical<C, I>(
    effect: &Effect<C, I>,
    components: &[(&str, char)],
) -> (char, String, &'static str) {
    let (id, kind) = match effect {
        Effect::Perform(command) => (command.id.0.as_str(), "perform"),
        Effect::Schedule(timer) => (timer.id.0.as_str(), "schedule"),
        Effect::CancelTimer(id) => (id.0.as_str(), "cancel_timer"),
    };
    components
        .iter()
        .find_map(|(prefix, name)| {
            id.strip_prefix(prefix)
                .map(|local| (*name, local.to_owned(), kind))
        })
        .unwrap_or_else(|| ('?', id.to_owned(), kind))
}

const LEFT_NESTED_IDS: &[(&str, char)] = &[("l/l/", 'a'), ("l/r/", 'b'), ("r/", 'c')];
const RIGHT_NESTED_IDS: &[(&str, char)] = &[("l/", 'a'), ("r/l/", 'b'), ("r/r/", 'c')];

#[test]
fn psm_product_is_associative() {
    let tri_input = prop_oneof![
        counter_input().prop_map(Tri::A),
        register_input().prop_map(Tri::B),
        counter_input().prop_map(Tri::C),
    ]
    .boxed();
    check_property(
        &settings(),
        &(
            (counter_state(), register_state(), counter_state()),
            turns(tri_input),
        ),
        |((a, b, c), turns)| {
            let left: LeftNested = Product(toys(), Counter::new(Mutant::None));
            let right: RightNested = Product(
                Counter::new(Mutant::None),
                Product(Register, Counter::new(Mutant::None)),
            );
            let left_turns: Vec<_> = turns
                .iter()
                .map(|turn| retag(turn, left_nested_input))
                .collect();
            let right_turns: Vec<_> = turns
                .iter()
                .map(|turn| retag(turn, right_nested_input))
                .collect();
            let l = step_law(
                step::<LeftNested>,
                &left,
                ((a.clone(), b), c.clone()),
                &left_turns,
            )?;
            let r = step_law(step::<RightNested>, &right, (a, (b, c)), &right_turns)?;
            prop_assert_eq!(&l.turns, &r.turns, "associativity: verdicts differ");
            let ((la, lb), lc) = l.last_state().clone();
            let (ra, (rb, rc)) = r.last_state().clone();
            prop_assert_eq!((la, lb, lc), (ra, rb, rc), "associativity: states differ");
            let l_ledger: Vec<_> = l.ledger.iter().map(left_nested_event).collect();
            let r_ledger: Vec<_> = r.ledger.iter().map(right_nested_event).collect();
            prop_assert_eq!(l_ledger, r_ledger, "associativity: ledgers differ");
            let l_effects: Vec<_> = l
                .effects
                .iter()
                .map(|effect| canonical(effect, LEFT_NESTED_IDS))
                .collect();
            let r_effects: Vec<_> = r
                .effects
                .iter()
                .map(|effect| canonical(effect, RIGHT_NESTED_IDS))
                .collect();
            prop_assert_eq!(l_effects, r_effects, "associativity: effects differ");
            Ok(())
        },
    );
}

// ---------------------------------------------------- mutants, each caught

/// Step-law mutant: over the event cap, truncate instead of refusing.
fn truncating_step<D: Decider>(
    decider: &D,
    state: &D::State,
    input: &D::Input,
    observations: &Observations,
    limits: &EnvelopeLimits,
) -> StepResult<D> {
    let loose = EnvelopeLimits {
        max_events: u32::MAX,
        ..*limits
    };
    let mut accepted = step(decider, state, input, observations, &loose)?;
    let cap = usize::try_from(limits.max_events).unwrap_or(usize::MAX);
    if accepted.events.len() > cap {
        accepted.events.truncate(cap);
        accepted.state = fold(decider, state.clone(), &accepted.events);
    }
    Ok(accepted)
}

/// Step-law mutant: in a fork, drop `perform` effects and accept the rest
/// instead of refusing the turn.
fn fork_sanitizing_step<D: Decider>(
    decider: &D,
    state: &D::State,
    input: &D::Input,
    observations: &Observations,
    limits: &EnvelopeLimits,
) -> StepResult<D> {
    let main = Observations {
        world: World::Main,
        ..observations.clone()
    };
    let mut accepted = if matches!(observations.world, World::Fork { .. }) {
        step(decider, state, input, &main, limits)?
    } else {
        step(decider, state, input, observations, limits)?
    };
    if matches!(observations.world, World::Fork { .. }) {
        accepted.effects.retain(|effect| !effect.is_perform());
    }
    Ok(accepted)
}

fn step_law_with(step_fn: StepFn<Counter>) -> String {
    caught(run_property(
        &mutant_settings(),
        &(counter_state(), turns(counter_input())),
        |(initial, turns)| {
            step_law(step_fn, &Counter::new(Mutant::None), initial, &turns).map(|_| ())
        },
    ))
}

#[test]
fn psm_mutant_truncating_step_is_caught() {
    let failure = step_law_with(truncating_step::<Counter>);
    assert!(failure.contains("law violated (step)"), "{failure}");
    assert!(failure.contains("EnvelopeExceeded"), "{failure}");
}

#[test]
fn psm_mutant_fork_sanitizing_step_is_caught() {
    let failure = step_law_with(fork_sanitizing_step::<Counter>);
    assert!(failure.contains("law violated (step)"), "{failure}");
    assert!(failure.contains("PerformOutsideMain"), "{failure}");
}

#[test]
fn psm_mutant_impure_evolve_is_caught() {
    let failure = caught(run_property(
        &mutant_settings(),
        &(counter_state(), turns(counter_input())),
        |(initial, turns)| {
            let decider = Counter::new(Mutant::ImpureEvolve);
            let trace = drive(step::<Counter>, &decider, initial, &turns);
            replay_is_fold(&decider, &trace)
        },
    ));
    assert!(failure.contains("law violated (replay)"), "{failure}");
}

#[test]
fn psm_mutant_ambient_decide_in_historical_world_is_caught() {
    let failure = caught(run_property(
        &mutant_settings(),
        &(counter_state(), turns(counter_input())),
        |(initial, turns)| {
            decide_is_pure(
                &|| Counter::new(Mutant::AmbientWhenHistorical),
                &initial,
                &turns,
            )
        },
    ));
    assert!(
        failure.contains("law violated (purity): decide differs between fresh instances"),
        "{failure}"
    );
    assert!(failure.contains("Historical"), "{failure}");
}

#[test]
fn psm_mutant_rejection_with_hidden_memory_is_caught() {
    let failure = caught(run_property(
        &mutant_settings(),
        &(counter_state(), turns(counter_input())),
        |(initial, turns)| {
            rejection_is_transparent(
                step::<Counter>,
                &|| Counter::new(Mutant::RejectionMemory),
                &initial,
                &turns,
            )
        },
    ));
    assert!(failure.contains("law violated (rejection)"), "{failure}");
}

/// Composition mutant: a product whose left events reset the right state.
struct CrossTalk(Toys);

impl Decider for CrossTalk {
    type State = StateOf<ProductModel<CounterModel, RegisterModel>>;
    type Input = Either<CounterInput, RegisterInput>;
    type Event = Either<CounterEvent, RegisterEvent>;
    type Command = Either<Pong, std::convert::Infallible>;
    type Rejection = Either<BelowZero, Empty>;

    fn initial_state(&self) -> Self::State {
        self.0.initial_state()
    }

    fn decide(
        &self,
        state: &Self::State,
        input: &Self::Input,
        observations: &Observations,
    ) -> Result<DecisionOf<Self>, Self::Rejection> {
        self.0.decide(state, input, observations)
    }

    fn evolve(&self, state: &Self::State, event: &Self::Event) -> Self::State {
        let (left, right) = self.0.evolve(state, event);
        match event {
            Either::Left(_) => (left, self.0.1.initial_state()),
            Either::Right(_) => (left, right),
        }
    }
}

#[test]
fn psm_mutant_cross_talk_product_is_caught() {
    let failure = caught(run_property(
        &mutant_settings(),
        &((counter_state(), register_state()), turns(toy_input())),
        |(initial, turns)| {
            product_is_interleaving(
                &CrossTalk(toys()),
                &Counter::new(Mutant::None),
                &Register,
                &initial,
                &turns,
            )
        },
    ));
    assert!(failure.contains("law violated (composition)"), "{failure}");
}

/// Composition mutant: each left `Pinged` event toggles a reversible
/// corruption of the right state, so an even number of pings restores it.
/// A law that compared states only at the end would miss it.
struct TransientCrossTalk(Toys);

const fn corrupt(register: Option<u8>) -> Option<u8> {
    match register {
        None => Some(200),
        Some(value) if value < 100 => Some(value.wrapping_add(100)),
        Some(200) => None,
        Some(value) => Some(value.wrapping_sub(100)),
    }
}

impl Decider for TransientCrossTalk {
    type State = StateOf<ProductModel<CounterModel, RegisterModel>>;
    type Input = Either<CounterInput, RegisterInput>;
    type Event = Either<CounterEvent, RegisterEvent>;
    type Command = Either<Pong, std::convert::Infallible>;
    type Rejection = Either<BelowZero, Empty>;

    fn initial_state(&self) -> Self::State {
        self.0.initial_state()
    }

    fn decide(
        &self,
        state: &Self::State,
        input: &Self::Input,
        observations: &Observations,
    ) -> Result<DecisionOf<Self>, Self::Rejection> {
        self.0.decide(state, input, observations)
    }

    fn evolve(&self, state: &Self::State, event: &Self::Event) -> Self::State {
        let (left, right) = self.0.evolve(state, event);
        match event {
            Either::Left(CounterEvent::Pinged) => (left, corrupt(right)),
            Either::Left(_) | Either::Right(_) => (left, right),
        }
    }
}

/// Only main-world, V1-capped left pings, in pairs: every sequence ends with
/// the right state restored, so only the per-turn check can fail.
fn ping_pairs() -> BoxedStrategy<Vec<Turn<Either<CounterInput, RegisterInput>>>> {
    (1_usize..4, strategy::logical_time())
        .prop_map(|(pairs, now)| {
            let ping = Turn {
                input: Either::Left(CounterInput::Ping),
                observations: Observations::main(now, norn_kernel::Entropy::from_bytes([0; 32])),
                limits: EnvelopeLimits::V1,
            };
            vec![ping; pairs.saturating_mul(2)]
        })
        .boxed()
}

#[test]
fn psm_mutant_transient_cross_talk_product_is_caught() {
    // Sanity: the corruption really is restored by the end of every
    // generated sequence, so a final-state-only comparison would pass.
    check_property(
        &settings(),
        &((counter_state(), register_state()), ping_pairs()),
        |(initial, turns)| {
            let trace = drive(
                step::<TransientCrossTalk>,
                &TransientCrossTalk(toys()),
                initial.clone(),
                &turns,
            );
            prop_assert_eq!(&trace.last_state().1, &initial.1);
            Ok(())
        },
    );
    let failure = caught(run_property(
        &mutant_settings(),
        &((counter_state(), register_state()), ping_pairs()),
        |(initial, turns)| {
            product_is_interleaving(
                &TransientCrossTalk(toys()),
                &Counter::new(Mutant::None),
                &Register,
                &initial,
                &turns,
            )
        },
    ));
    assert!(
        failure.contains("law violated (composition): after turn 0 the product's state"),
        "{failure}"
    );
}
