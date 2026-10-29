//! `norn-testkit`: shared property-test harness for Norn Deciders (T6-PSM-1).
//!
//! Test support only — take it as a `[dev-dependencies]` entry; the skeleton
//! guard rejects it as a normal or build dependency (see `AGENTS.md`).
//!
//! The harness is a thin adapter over [`proptest_state_machine`]: a
//! [`DeciderModel`] (an independent reference model of one
//! [`norn_kernel::Decider`]) is turned into a
//! [`ReferenceStateMachine`](proptest_state_machine::ReferenceStateMachine) /
//! [`StateMachineTest`](proptest_state_machine::StateMachineTest) pair by
//! [`DeciderMachine`]. Every generated turn is run through the kernel's
//! [`step`](norn_kernel::step) and checked against the Decider laws the kernel
//! documents:
//!
//! 1. **Determinism.** `decide` and `step` return equal results when called
//!    twice with the same state, input, and [`Observations`](norn_kernel::Observations).
//! 2. **Step = decide, check, evolve.** `step` equals an independent oracle:
//!    historical worlds are refused before `decide`; a rejection becomes
//!    `StepError::Rejected`; envelope caps are checked in order
//!    (events, commands, timers); a non-main world may not `perform`; and an
//!    accepted turn's state is `fold(evolve)` over its events. A refused turn
//!    carries zero events and zero effects and leaves state untouched.
//! 3. **Replay.** After every turn, `replay(ledger) == state`, and folding is
//!    a monoid action: `fold(replay(prefix), suffix) == replay(ledger)` at
//!    every split point.
//! 4. **Refinement.** The decider agrees with the model: same accept/reject
//!    verdict, same decision shape (event/perform/timer counts), and
//!    [`DeciderModel::project`] of the state equals the model state.
//!
//! Beyond the model-based machine, [`laws`] states the Decider laws that need
//! no model — the step law, replay over every ledger split, purity across
//! fresh instances, and rejection transparency — for any decider and any
//! initial state, and [`compose`] states the composition (monoid) law over
//! [`compose::Product`]. [`run_property`] / [`check_property`] run them with
//! the same seed and regression conventions. Each law takes the `step`
//! function as a parameter so a mutant self-test can prove it fails.
//!
//! [`cell_host`] holds the in-memory reference
//! [`CellHost`](norn_kernel::CellHost) ([`cell_host::MemoryCell`]) and the
//! host-agnostic `CellHost` laws ([`cell_host::check_cell_host`]) every host
//! driver runs (T4-CELL-1).
//!
//! [`run`] / [`check`] drive the machine with a deterministic seed, persist
//! failures to a committed `*.proptest-regressions` file beside the test,
//! shrink failures to a minimal transition sequence, and return
//! [`Coverage`] counters so a test can assert every transition, outcome, and
//! effect kind was actually exercised.

pub mod cell_host;
pub mod compose;
mod coverage;
pub mod laws;
mod machine;
mod model;
mod runner;
pub mod strategy;

pub use coverage::{Coverage, KERNEL_OUTCOMES};
pub use machine::{DeciderMachine, ReferenceState, SystemUnderTest};
pub use model::{
    DeciderModel, DecisionShape, EventOf, InputOf, Outcome, Prediction, StateOf, Turn, TurnOf,
};
pub use runner::{
    DEFAULT_CASES, DEFAULT_SEED, Failure, REGRESSION_EXTENSION, Settings, check, check_property,
    run, run_property,
};

/// Re-exported so dependents need only `norn-testkit` in `[dev-dependencies]`.
pub use proptest;
/// Re-exported so dependents need only `norn-testkit` in `[dev-dependencies]`.
pub use proptest_state_machine;
