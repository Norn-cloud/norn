//! `norn-cell-do`'s integration tests, built as one test binary.
//!
//! Each `tests/<name>.rs` is the module `<name>` here, so nextest names a test
//! `binary_id(norn-cell-do::suites)` and `test(=<name>::<test>)`, and
//! `cargo test -p norn-cell-do --test suites <name>::` runs one former binary.
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

#[path = "action_steps.rs"]
mod action_steps;
#[path = "actions.rs"]
mod actions;
#[cfg(feature = "duckdb")]
#[path = "analytics_backend.rs"]
mod analytics_backend;
#[cfg(feature = "duckdb")]
#[path = "analytics_profile.rs"]
mod analytics_profile;
#[cfg(all(feature = "datafusion", feature = "duckdb"))]
#[path = "analytics_profile_datafusion.rs"]
mod analytics_profile_datafusion;
#[cfg(feature = "duckdb")]
#[path = "analytics_records.rs"]
mod analytics_records;
#[cfg(all(feature = "datafusion", feature = "duckdb"))]
#[path = "analytics_records_datafusion.rs"]
mod analytics_records_datafusion;
#[cfg(feature = "duckdb")]
#[path = "analytics_sources.rs"]
mod analytics_sources;
#[cfg(all(feature = "datafusion", feature = "duckdb"))]
#[path = "analytics_sources_datafusion.rs"]
mod analytics_sources_datafusion;
#[path = "answers.rs"]
mod answers;
#[path = "approval_fencing_quint.rs"]
mod approval_fencing_quint;
#[path = "approval_recovery.rs"]
mod approval_recovery;
#[path = "approvals.rs"]
mod approvals;
#[path = "automations_u1.rs"]
mod automations_u1;
#[path = "conformance_reference.rs"]
mod conformance_reference;
#[path = "connections.rs"]
mod connections;
#[path = "decisions.rs"]
mod decisions;
#[path = "definitions.rs"]
mod definitions;
#[path = "evaluate.rs"]
mod evaluate;
#[path = "faults.rs"]
mod faults;
#[path = "held_back.rs"]
mod held_back;
#[path = "hosted_install.rs"]
mod hosted_install;
#[path = "hosted_load.rs"]
mod hosted_load;
#[path = "hosted_theme.rs"]
mod hosted_theme;
#[path = "hosted_timers_quint.rs"]
mod hosted_timers_quint;
#[path = "inbox.rs"]
mod inbox;
#[path = "ingest_rate.rs"]
mod ingest_rate;
#[path = "journal_from_memory.rs"]
mod journal_from_memory;
#[path = "marking_lineage.rs"]
mod marking_lineage;
#[path = "mcp.rs"]
mod mcp;
#[path = "ml_approval.rs"]
mod ml_approval;
#[path = "observations_load.rs"]
mod observations_load;
#[cfg(feature = "duckdb")]
#[path = "olapd_credentials.rs"]
mod olapd_credentials;
#[cfg(all(feature = "datafusion", feature = "duckdb"))]
#[path = "olapd_credentials_datafusion.rs"]
mod olapd_credentials_datafusion;
#[path = "provision.rs"]
mod provision;
#[path = "read_policy.rs"]
mod read_policy;
#[path = "refusal_detail.rs"]
mod refusal_detail;
#[path = "reopen_after_migration.rs"]
mod reopen_after_migration;
#[path = "runs.rs"]
mod runs;
#[path = "server.rs"]
mod server;
#[path = "tenant_actions.rs"]
mod tenant_actions;
#[path = "tenant_isolation.rs"]
mod tenant_isolation;
#[path = "timers.rs"]
mod timers;
#[path = "tokens.rs"]
mod tokens;
#[path = "views.rs"]
mod views;
