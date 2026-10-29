//! Worked example of the `norn-testkit` proptest-state-machine template
//! (T6-PSM-1): an order lifecycle decider checked against an independent
//! transition-table model.
//!
//! Copy this file to start a new `psm_*` suite. The harness checks, on every
//! generated turn, the Decider laws `norn-kernel` documents: `decide`/`step`
//! determinism under identical `Observations`; `step` = world check →
//! `decide` → envelope caps → perform-outside-main → `evolve`, with zero
//! events/effects and unchanged state on any refusal; historical worlds
//! refused; `replay(ledger) == state` and split-fold composition; and
//! refinement of the model below.
//!
//! Failures shrink to a minimal turn sequence and are persisted to
//! `psm_order_lifecycle.proptest-regressions` beside this file — commit it.
//! Raise the budget with `PROPTEST_CASES=<n>`; explore with
//! `PROPTEST_RNG_SEED=<n>`.

use std::fmt;

use norn_kernel::{
    Command, CommandId, Decider, Decision, Effect, FreshnessPolicy, Generation, IdempotencyKey,
    LogicalTime, Observations, Timer, TimerId,
};
use norn_testkit::proptest::prelude::*;
use norn_testkit::{
    DeciderModel, DecisionShape, KERNEL_OUTCOMES, Prediction, Settings, StateOf, check,
};

const PAYMENT_WINDOW_MS: u64 = 30_000;
const PAYMENT_TIMER: &str = "payment-window";

// ---------------------------------------------------------------- decider

#[derive(Debug, Clone, PartialEq, Eq)]
enum Input {
    Place {
        amount: u32,
    },
    PaymentSucceeded,
    /// Delivered by `clock` when the payment window closes.
    PaymentWindowClosed,
    Ship,
    Cancel,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Event {
    Placed { amount: u32, at: LogicalTime },
    Paid,
    Shipped,
    Cancelled,
    Refunded { amount: u32 },
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Cmd {
    Charge { amount: u32 },
    Dispatch,
    Refund { amount: u32 },
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Rejection {
    ZeroAmount,
    NotAllowed(&'static str),
}

impl fmt::Display for Rejection {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ZeroAmount => f.write_str("order amount must be positive"),
            Self::NotAllowed(status) => write!(f, "input not allowed while {status}"),
        }
    }
}

impl std::error::Error for Rejection {}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Order {
    Empty,
    Placed { amount: u32, at: LogicalTime },
    Paid { amount: u32 },
    Shipped,
    Cancelled,
}

impl Order {
    const fn name(&self) -> &'static str {
        match self {
            Self::Empty => "empty",
            Self::Placed { .. } => "placed",
            Self::Paid { .. } => "paid",
            Self::Shipped => "shipped",
            Self::Cancelled => "cancelled",
        }
    }
}

struct OrderLifecycle;

fn command(kind: &str, obs: &Observations, payload: Cmd) -> Effect<Cmd, Input> {
    // Identity derives from recorded entropy, never an ambient source.
    let mut prefix = [0_u8; 8];
    prefix.copy_from_slice(&obs.entropy.as_bytes()[..8]);
    let id = format!("{kind}-{:016x}", u64::from_le_bytes(prefix));
    Effect::Perform(Command {
        id: CommandId(id.clone()),
        generation: Generation::default(),
        idempotency_key: IdempotencyKey(id),
        freshness: FreshnessPolicy::AppendOnly,
        deadline: None,
        payload,
    })
}

impl Decider for OrderLifecycle {
    type State = Order;
    type Input = Input;
    type Event = Event;
    type Command = Cmd;
    type Rejection = Rejection;

    fn initial_state(&self) -> Order {
        Order::Empty
    }

    fn decide(
        &self,
        state: &Order,
        input: &Input,
        obs: &Observations,
    ) -> Result<Decision<Event, Cmd, Input>, Rejection> {
        let refuse = || Err(Rejection::NotAllowed(state.name()));
        match (state, input) {
            (Order::Empty, Input::Place { amount: 0 }) => Err(Rejection::ZeroAmount),
            (Order::Empty, Input::Place { amount }) => {
                let due = obs
                    .now
                    .checked_add_millis(PAYMENT_WINDOW_MS)
                    .ok_or(Rejection::NotAllowed("placed at the end of time"))?;
                Ok(Decision::events(vec![Event::Placed {
                    amount: *amount,
                    at: obs.now,
                }])
                .with_effect(command("charge", obs, Cmd::Charge { amount: *amount }))
                .with_effect(Effect::Schedule(Timer {
                    id: TimerId(PAYMENT_TIMER.into()),
                    due,
                    payload: Input::PaymentWindowClosed,
                })))
            }
            (Order::Placed { .. }, Input::PaymentSucceeded) => {
                Ok(Decision::events(vec![Event::Paid])
                    .with_effect(Effect::CancelTimer(TimerId(PAYMENT_TIMER.into()))))
            }
            (Order::Placed { .. }, Input::PaymentWindowClosed) => {
                Ok(Decision::events(vec![Event::Cancelled]))
            }
            // A window timer that fires after the order moved on is stale: a
            // no-op acceptance, not a rejection.
            (_, Input::PaymentWindowClosed) => Ok(Decision::none()),
            (Order::Placed { .. }, Input::Cancel) => Ok(Decision::events(vec![Event::Cancelled])
                .with_effect(Effect::CancelTimer(TimerId(PAYMENT_TIMER.into())))),
            (Order::Paid { amount }, Input::Cancel) => {
                Ok(
                    Decision::events(vec![Event::Cancelled, Event::Refunded { amount: *amount }])
                        .with_effect(command("refund", obs, Cmd::Refund { amount: *amount })),
                )
            }
            (Order::Paid { .. }, Input::Ship) => Ok(Decision::events(vec![Event::Shipped])
                .with_effect(command("dispatch", obs, Cmd::Dispatch))),
            _ => refuse(),
        }
    }

    fn evolve(&self, state: &Order, event: &Event) -> Order {
        match (state, event) {
            (_, Event::Placed { amount, at }) => Order::Placed {
                amount: *amount,
                at: *at,
            },
            (Order::Placed { amount, .. }, Event::Paid) => Order::Paid { amount: *amount },
            (_, Event::Shipped) => Order::Shipped,
            (_, Event::Cancelled) => Order::Cancelled,
            // Total: events that do not move the lifecycle leave it as is.
            (state, Event::Paid | Event::Refunded { .. }) => state.clone(),
        }
    }
}

// ------------------------------------------------------------------ model

/// The model deliberately forgets amounts and timestamps: a status table.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Status {
    Empty,
    Placed,
    Paid,
    Shipped,
    Cancelled,
}

struct OrderModel;

const fn shape(events: usize, performs: usize, timers: usize) -> DecisionShape {
    DecisionShape {
        events,
        performs,
        timers,
    }
}

impl DeciderModel for OrderModel {
    type Decider = OrderLifecycle;
    type Model = Status;

    fn decider() -> OrderLifecycle {
        OrderLifecycle
    }

    fn initial_model() -> Status {
        Status::Empty
    }

    fn inputs(_: &Status) -> BoxedStrategy<Input> {
        // Every input from every status: illegal ones must be rejected.
        prop_oneof![
            (0_u32..4).prop_map(|amount| Input::Place { amount }),
            Just(Input::PaymentSucceeded),
            Just(Input::PaymentWindowClosed),
            Just(Input::Ship),
            Just(Input::Cancel),
        ]
        .boxed()
    }

    fn predict(status: &Status, input: &Input, _: &Observations) -> Prediction<Status> {
        let accept = |next, shape| Prediction::Accept { next, shape };
        match (status, input) {
            (Status::Empty, Input::Place { amount: 0 }) => Prediction::Reject,
            (Status::Empty, Input::Place { .. }) => accept(Status::Placed, shape(1, 1, 1)),
            (Status::Placed, Input::PaymentSucceeded) => accept(Status::Paid, shape(1, 0, 1)),
            (Status::Placed, Input::PaymentWindowClosed) => {
                accept(Status::Cancelled, shape(1, 0, 0))
            }
            (_, Input::PaymentWindowClosed) => accept(*status, shape(0, 0, 0)),
            (Status::Placed, Input::Cancel) => accept(Status::Cancelled, shape(1, 0, 1)),
            (Status::Paid, Input::Cancel) => accept(Status::Cancelled, shape(2, 1, 0)),
            (Status::Paid, Input::Ship) => accept(Status::Shipped, shape(1, 1, 0)),
            _ => Prediction::Reject,
        }
    }

    fn project(state: &StateOf<Self>) -> Status {
        match state {
            Order::Empty => Status::Empty,
            Order::Placed { .. } => Status::Placed,
            Order::Paid { .. } => Status::Paid,
            Order::Shipped => Status::Shipped,
            Order::Cancelled => Status::Cancelled,
        }
    }

    fn input_label(input: &Input) -> &'static str {
        match input {
            Input::Place { .. } => "place",
            Input::PaymentSucceeded => "payment_succeeded",
            Input::PaymentWindowClosed => "payment_window_closed",
            Input::Ship => "ship",
            Input::Cancel => "cancel",
        }
    }

    fn event_label(event: &Event) -> &'static str {
        match event {
            Event::Placed { .. } => "placed",
            Event::Paid => "paid",
            Event::Shipped => "shipped",
            Event::Cancelled => "cancelled",
            Event::Refunded { .. } => "refunded",
        }
    }
}

#[test]
fn psm_order_lifecycle_obeys_decider_laws() {
    let coverage = check::<OrderModel>(&Settings::new(file!()));
    coverage.assert_covered(KERNEL_OUTCOMES);
    coverage.assert_covered(&[
        "event:placed",
        "event:paid",
        "event:shipped",
        "event:cancelled",
        "event:refunded",
        "effect:perform",
        "effect:schedule",
        "effect:cancel_timer",
        "world:fork",
        "world:historical",
    ]);
}
