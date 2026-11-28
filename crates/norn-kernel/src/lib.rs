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
mod action_aggregate;
pub mod action_commit;
pub mod authz;
mod authz_action;
pub mod cell_host;
mod commit;
mod commit_check;
mod commit_order;
mod commit_versions;
mod creator_stamp;
mod decider;
pub mod decision_commit;
mod decision_links;
mod effect;
mod envelope;
pub mod flow_identity;
pub mod flow_turn;
pub mod governing_laws;
mod guard_diagnostic;
mod independence;
mod lifecycle_init;
pub mod limits;
pub mod naming_series;
mod partition_slice;
pub mod predicate_read;
pub use partition_slice::{PartitionSlice, load_partition_slice};
mod principal_inputs;
mod synchronous_rule;
pub use norn_decisions::DecisionBinding;
pub use principal_inputs::{
    ActionPrincipalInputs, PrincipalInputsRequest, UnresolvedPrincipalInput,
};
pub use synchronous_rule::{SynchronousRule, SynchronousRuleError};
mod observations;
pub mod quantity;
pub mod receipt_binding;
mod receipt_decision;
pub mod refusal;
mod replay_decision;
pub mod result_reconciliation;
pub mod snapshot_observe;
pub mod timer_schedule;
mod version_check;
// Hosts and the dependency-bounded reference harness share the input codec
// contract used to bind exact observation continuation payloads.
pub use commit_check::{
    CommitAudit, CommitLawAudit, InvariantEvaluationError, InvariantFailure,
    check_commit_identifiers, check_commit_observed, check_commit_owned_rows,
    check_commit_with_audit, check_fact_key_identity, evaluate_invariants,
    invariant_evaluation_budget,
};
#[doc(hidden)]
pub mod pack_mbt_support {
    pub use norn_authz::cedar_projection;
    pub use norn_authz::{PdpExplanation, ValidatedPolicySet};
    pub use norn_checker::law_examples::{
        Verdict as LawExampleVerdict, evaluate_runtime as evaluate_law_example_runtime,
    };
    pub use norn_expr::{EvaluationBudget, Expression, Value as ExprValue, expression_read_paths};
    pub use norn_ir::UpsertWriteMode;
    pub use norn_ir::access::{
        AccessCondition, AccessIr, AccessLiteral, AccessRule, PolicyRoot, ResourceCompareOp,
        ResourceCondition, ResourceOperand, ScopeDimension,
    };
    pub use norn_ir::mapping::MappingIr;
    pub use norn_ir::{
        ActionEffectProgram, ActionRoot, ActionUiEffect, ArtifactEnvelope, ArtifactPayload,
        DataQualityOutcome, DataQualityRuleRef, Decimal, DecisionOutput, EntityRoot, FactChange,
        InterfaceDef, JsonValue as IrJsonValue, LawIR, LawModality, LawResponse, Lifecycle,
        ObjectTypeDef, ObjectTypeName, Property, PropertyConstraints, PropertyId, PropertyType,
        PropertyTypeDef, RecordMember, ResourceStatus, SliceKey, Transition, TransitionId,
        WriteChange, ZenExpression, lifecycle_codes,
    };

    /// ACTION-ACTOR-1 / ACTION-CLOCK-1 / ORIGIN-DATAFLOW-1: every input field
    /// classified by the kernel as server-derived (actor-derived, clock-derived,
    /// or purely server-derived from non-caller roots) that callers must omit
    /// from raw action input.
    #[must_use]
    pub fn server_derived_input_fields(action: &ActionRoot) -> std::collections::BTreeSet<String> {
        let mut fields = crate::action_commit::actor_derived_fields(action);
        fields.extend(crate::action_commit::clock_derived_fields(action));
        fields.extend(norn_expr::schema::trusted_server_derived_fields(action));
        fields
    }

    /// Computes the canonical `sha256:...` content pin for a retained [`LawIR`],
    /// matching [`crate::governing_laws`]'s `DataQualityRuleRef::Law::version`.
    ///
    /// # Errors
    ///
    /// Returns an error if canonical JSON encoding fails.
    pub fn law_content_pin(law: &LawIR) -> Result<String, String> {
        let bytes = norn_ir::canonical::canonical_json_of(law).map_err(|err| err.to_string())?;
        Ok(format!(
            "sha256:{}",
            norn_ir::canonical::HashProfile::Norn1.hash_canonical_bytes(&bytes)
        ))
    }
}
/// The one monotonic fuel tank every invariant, law and action turn spends from (P1).
pub use norn_expr::TurnFuel;
pub use norn_ir::{MAX_INTENT_REF_BYTES, MAX_TRACE_ID_BYTES, TraceContext, TraceContextError};
pub use serde;
pub use snapshot_observe::{ObservationAuthorization, ObservationEvent, PreparedObservation};

pub use action_commit::{
    ACTION_CALL_TYPE, ACTION_EFFECT_MUTATE_PARENT_WITH_ROWS, ActionAuthorizer, ActionAuthzRequest,
    ActionCall, ActionCallRecord, ActionDenial, ActionDerivedValue, ActionInputPreparation,
    ActionOutcome, ActionParameterPin, ActionPreconditionOutcome, ActionReadAuthzRequest,
    ActionReadTypeAuthzRequest, ActionRefusal, AutomationInputViolation,
    NamingSeriesInspectionRequest, OWNED_ROWS_TYPE, Prepared, PreparedIntent, action_reads_clock,
    actor_derived_fields, clock_derived_fields, commit_action, commit_action_with_limits,
    commit_action_with_meter, commit_action_with_preparation, independent_accountable_people,
    owned_rows_manifest_key, prepare_action_input, prepare_action_input_for_call,
    prepare_action_intent, retained_person_matches, validate_accountable_person,
    validate_automation_static_input,
};
pub use cell_host::{Authorized, Checked, CommitSeal, Committed, Proposed};
pub use cell_host::{
    BoundedFact, CellCommit, CellHost, CommitError, CommitModel, CommitReceipt, CommitVersions,
    CommitView, Consumed, DueTimer, FactExpectation, FactKey, FactMutation, FactState, FactVersion,
    FactWrite, LedgerSeq, LinkKey, LinkMutation, LinkReadExpectation, LinkReadTarget, LinkState,
    LinkWrite, ObservationId, Observed, OwnedRowsCommitProof, Performed, PredicateActionRead,
    PredicateObject, PredicateQuery, PredicateReadExpectation, PredicateReadMember, Provenance,
    SerializationLane, WrittenLinkVersion, WrittenVersion, check_commit,
};
pub use creator_stamp::{CreatorStampProof, check_commit_stamped};
pub use decider::{
    Decider, Decision, DecisionOf, EnvelopeKind, Step, StepError, StepOf, StepResult, fold, replay,
    step,
};
pub use decision_commit::{
    DataQualityRuleClassification, DecisionRefusal, DecisionTurn, DefinitionWork,
    EffectiveValuesError, FactEvent, FactSlice, PreparedDecision, TypeCatalog, commit_decision,
    commit_decision_with_limits, commit_decision_with_meter, decision_cell_commit, load_slice,
    prepare_decision, prepare_decision_with_limits, prepare_decision_with_source_owned_targets,
};
pub use decision_links::{
    HostCellCommit, decision_cell_commit_with_host, decision_cell_commit_with_host_and_meter,
};
pub use effect::{
    Command, CommandId, DependencyToken, Effect, FreshnessPolicy, Generation, IdempotencyKey,
    InputIdentity, OwnerVersion, Timer, TimerId,
};
pub use envelope::{TurnEnvelopeLimits, TurnEnvelopeMeter, TurnEnvelopeUsage};
pub use independence::{TransitionGuard, transition_guards};
pub use lifecycle_init::LifecycleInitRefusal;
pub use limits::{
    EnvelopeLimits, IngressLimit, IngressLimitExceeded, LimitProfileRef, ResourceLimitProfile,
    SliceLimits, SliceMeter, check_envelope_bytes,
};
pub use naming_series::{
    NAMING_SERIES_COUNTER_TYPE, NamingCounterSeed, install_naming_counter_seeds,
};
pub use observations::{
    CommitId, Entropy, LogicalTime, Observations, ScenarioClock, ScenarioObservations, World,
    WorldId,
};
pub use refusal::{INTERNAL_MESSAGE, NOT_FOUND_MESSAGE, Refusable, Refusal, RefusalCategory};

mod action_quantity;

/// Conservative ownership boundary for cell-local atomic execution.
pub mod execution_profile;

mod profile_ownership;

/// Versioned single-cell read evidence and context validation.
pub mod read_envelope;
mod read_token_check;

/// D03 outbox lifecycle (#1699) and typed world pins (#1698).
pub mod world;
