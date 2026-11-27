//! `norn-kernel`'s integration tests, built as one test binary.
//!
//! Each `tests/<name>.rs` is the module `<name>` here, so nextest names a test
//! `binary_id(norn-kernel::suites)` and `test(=<name>::<test>)`, and
//! `cargo test -p norn-kernel --test suites <name>::` runs one former binary.
//! A `#[path]` module owns its directory as a crate root does, so each suite's
//! own `mod` and `#[path]` helpers resolve as they did when it was a binary.
//! A new suite is a new module here: `scripts/codemods/fold_integration_tests.py`
//! keeps this list and every reference to it in step.
#![allow(
    dead_code,
    unreachable_pub,
    clippy::duplicate_mod,
    reason = "each suite keeps the visibility and the shared helpers it had as its own test binary"
)]

#[path = "accounting_commit.rs"]
mod accounting_commit;
#[path = "action_aggregates.rs"]
mod action_aggregates;
#[path = "action_commit.rs"]
mod action_commit;
#[path = "action_parameters.rs"]
mod action_parameters;
#[path = "action_preconditions.rs"]
mod action_preconditions;
#[path = "action_preparation.rs"]
mod action_preparation;
#[path = "action_series_context.rs"]
mod action_series_context;
#[path = "authz_action.rs"]
mod authz_action;
#[path = "cell_host.rs"]
mod cell_host;
#[path = "decider.rs"]
mod decider;
#[path = "decision_commit.rs"]
mod decision_commit;
#[path = "execution_profile.rs"]
mod execution_profile;
#[path = "fact_event.rs"]
mod fact_event;
#[path = "flow_primitives.rs"]
mod flow_primitives;
#[path = "invariant_commit.rs"]
mod invariant_commit;
#[path = "limits.rs"]
mod limits;
#[path = "location_hierarchy.rs"]
mod location_hierarchy;
#[path = "outbox_ledger.rs"]
mod outbox_ledger;
#[path = "predicate_slice_and_links.rs"]
mod predicate_slice_and_links;
#[path = "psm_action_laws.rs"]
mod psm_action_laws;
#[path = "psm_action_transition.rs"]
mod psm_action_transition;
#[path = "psm_decider_laws.rs"]
mod psm_decider_laws;
#[path = "psm_order_lifecycle.rs"]
mod psm_order_lifecycle;
#[path = "quantity_reservations.rs"]
mod quantity_reservations;
#[path = "receipt_effect.rs"]
mod receipt_effect;
#[path = "refusal_taxonomy.rs"]
mod refusal_taxonomy;
#[path = "registry_ref_invariants.rs"]
mod registry_ref_invariants;
#[path = "result_reconciliation.rs"]
mod result_reconciliation;
#[path = "snapshot_observe.rs"]
mod snapshot_observe;
#[path = "temporal_intervals.rs"]
mod temporal_intervals;
