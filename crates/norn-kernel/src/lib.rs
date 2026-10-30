//! `norn-kernel`: sans-IO decider + `CellHost` trait/effects.
//!
//! Ring 0 — pure, portable semantic core (see `AGENTS.md`). This crate is
//! synchronous, performs no IO, and reads no clock or entropy source: time and
//! randomness enter only as [`Observations`] supplied by the host for one turn.
//!
//! This slice (T2-KER-1) provides the Decider form required by ruling
//! 2026-09-25 §2/§3/§5:
//!
//! - [`Decider`]: `initial_state` / `decide` / `evolve`. `decide` is total and
//!   pure; it returns a [`Decision`] (ledger events plus closed [`Effect`]s) or
//!   a typed rejection. Materialized state is `fold(evolve)` over events.
//! - [`Effect`]: the closed, serializable effect ADT — `perform` a [`Command`],
//!   `schedule` a [`Timer`], or cancel one. No closures, no trait objects.
//! - [`step`]: one tick-shaped turn — `decide`, envelope checks, world checks,
//!   then `evolve` over the new events — with all-or-nothing semantics.
//!
//! The action transition (T2-KER-2a) lives in [`action`]: scope fail-closed
//! command admission, the authorization stage (the verdict is supplied by the
//! host), and [`action::VersionedAction`], the owner-version CAS +
//! idempotency decider. `norn-engine` is its shell.
//!
//! Resource limits (T2-LIM-1) live in [`limits`]: the versioned
//! [`ResourceLimitProfile`] (shipped default `norn.limits/1`), whose per-
//! decision counts [`step`] and the decision commit path enforce, plus pure
//! slice and envelope-byte checks. Hosts can share a configurable
//! [`TurnEnvelopeMeter`] across commits for aggregate event/command/timer
//! limits; profile calibration, fuel metering, and the full aggregate turn
//! budget remain T3-BUDGET-1.
//!
//! The host-facing four-verb [`CellHost`] trait (T4-CELL-1) lives in
//! [`cell_host`]: `commit` (facts, ledger events, command and timer enqueue,
//! and delivery consumption in one transaction), `perform`, `observe`, and
//! `clock`, plus `serialize_by` and `publish`. It replaces the 2026-09-09
//! `DurableContext`. [`check_commit`] is the shared, pure admission check
//! every host runs before applying a commit.
//!
//! The decision-result commit path (T2-KER-2b) lives in [`decision_commit`]:
//! [`prepare_decision`] resolves a `norn_ir::DecisionOutput` (fact changes,
//! command records, timer records) against the slice the decision read —
//! verifying lifecycle `from` states and evaluating effective guards with
//! `norn-expr` — into one [`CellCommit`], and [`commit_decision`] commits it
//! through a [`CellHost`] or returns a typed [`DecisionRefusal`] with nothing
//! committed.

#![forbid(unsafe_code)]
// Deterministic core policy (#3299); host adapters retain their own arithmetic/collections.
#![deny(
    clippy::float_arithmetic,
    clippy::integer_division,
    clippy::modulo_arithmetic,
    clippy::disallowed_types
)]

pub mod action;
pub mod action_commit;
pub mod authz;
mod authz_action;
pub mod cell_host;
mod commit_check;
mod decider;
pub mod decision_commit;
mod effect;
mod envelope;
pub mod limits;
pub mod naming_series;
mod observations;
pub mod quantity;
pub mod refusal;
pub mod result_reconciliation;
pub mod snapshot_observe;
mod version_check;
// Hosts and the dependency-bounded reference harness share the input codec
// contract used to bind exact observation continuation payloads.
pub use commit_check::{
    InvariantEvaluationError, InvariantFailure, check_commit_identifiers, check_commit_observed,
    check_commit_owned_rows, check_fact_key_identity, evaluate_invariants,
    invariant_evaluation_budget,
};
pub use serde;
pub use snapshot_observe::{ObservationAuthorization, ObservationEvent, PreparedObservation};

pub use action_commit::{
    ACTION_CALL_TYPE, ACTION_EFFECT_MUTATE_PARENT_WITH_ROWS, ActionAuthorizer, ActionAuthzRequest,
    ActionCall, ActionCallRecord, ActionDenial, ActionDerivedValue, ActionInputPreparation,
    ActionOutcome, ActionPreconditionOutcome, ActionReadAuthzRequest, ActionReadTypeAuthzRequest,
    ActionRefusal, NamingSeriesInspectionRequest, OWNED_ROWS_TYPE, commit_action,
    commit_action_with_limits, commit_action_with_meter, commit_action_with_preparation,
    owned_rows_manifest_key, prepare_action_input, prepare_action_input_for_call,
};
pub use cell_host::{
    BoundedFact, CellCommit, CellHost, CommitError, CommitModel, CommitReceipt, CommitVersions,
    CommitView, Consumed, DueTimer, FactExpectation, FactKey, FactMutation, FactState, FactVersion,
    FactWrite, LedgerSeq, LinkKey, LinkMutation, LinkReadExpectation, LinkReadTarget, LinkState,
    LinkWrite, ObservationId, Observed, OwnedRowsCommitProof, Performed, Provenance,
    SerializationLane, WrittenLinkVersion, WrittenVersion, check_commit,
};
pub use decider::{
    Decider, Decision, DecisionOf, EnvelopeKind, Step, StepError, StepOf, StepResult, fold, replay,
    step,
};
pub use decision_commit::{
    DataQualityRuleClassification, DecisionRefusal, DecisionTurn, EffectiveValuesError, FactEvent,
    FactSlice, PreparedDecision, TypeCatalog, commit_decision, commit_decision_with_limits,
    commit_decision_with_meter, load_slice, prepare_decision, prepare_decision_with_limits,
    prepare_decision_with_source_owned_targets,
};
pub use effect::{
    Command, CommandId, DependencyToken, Effect, FreshnessPolicy, Generation, IdempotencyKey,
    InputIdentity, OwnerVersion, Timer, TimerId,
};
pub use envelope::{TurnEnvelopeLimits, TurnEnvelopeMeter, TurnEnvelopeUsage};
pub use limits::{
    EnvelopeLimits, IngressLimit, IngressLimitExceeded, LimitProfileRef, ResourceLimitProfile,
    SliceLimits, SliceMeter, check_envelope_bytes,
};
pub use naming_series::{
    NAMING_SERIES_COUNTER_TYPE, NamingCounterSeed, install_naming_counter_seeds,
};
pub use observations::{CommitId, Entropy, LogicalTime, Observations, World, WorldId};
pub use refusal::{INTERNAL_MESSAGE, NOT_FOUND_MESSAGE, Refusable, Refusal, RefusalCategory};

mod action_quantity;
