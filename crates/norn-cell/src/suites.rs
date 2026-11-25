//! `norn-cell`'s own suites that commit raw (#1485 S6).
//!
//! The raw, unchecked store commit ([`crate::test_door`]) exists only in this
//! crate's `#[cfg(test)]` build, so the suites that need it are compiled here,
//! inside the crate, from their files under `tests/` (as `suites::<name>`).
//! Every other suite under `tests/` stays an integration test (Cargo.toml
//! lists them; `autotests = false`), and a fixture in any other crate writes
//! through the kernel path (`tests/support/governed.rs`).

#[cfg(all(feature = "duckdb", feature = "native"))]
#[path = "../tests/backends/duckdb.rs"]
mod analytics_backend;
#[cfg(all(feature = "duckdb", feature = "native"))]
#[path = "../tests/analytics_engine.rs"]
mod analytics_engine;
#[cfg(all(feature = "datafusion", not(target_arch = "wasm32")))]
#[path = "../tests/analytics_engine_datafusion.rs"]
mod analytics_engine_datafusion;
#[path = "../tests/answer_documents.rs"]
mod answer_documents;
#[path = "../tests/bench_latency.rs"]
mod bench_latency;
#[path = "../tests/cell_identity.rs"]
mod cell_identity;
#[path = "../tests/classification.rs"]
mod classification;
#[path = "../tests/definitions_export.rs"]
mod definitions_export;
#[path = "../tests/definitions_install.rs"]
mod definitions_install;
#[cfg(all(feature = "duckdb", feature = "native"))]
#[path = "../tests/duckdb_transform.rs"]
mod duckdb_transform;
#[path = "../tests/engine_fixture_record.rs"]
mod engine_fixture_record;
#[path = "../tests/engine_upgrade.rs"]
mod engine_upgrade;
#[path = "../tests/engine_upgrade_mined.rs"]
mod engine_upgrade_mined;
#[path = "../tests/exact_question.rs"]
mod exact_question;
#[path = "../tests/exact_values.rs"]
mod exact_values;
#[path = "../tests/export_journal.rs"]
mod export_journal;
#[path = "../tests/flow_keys.rs"]
mod flow_keys;
#[path = "../tests/flow_run.rs"]
mod flow_run;
#[path = "../tests/historical_delta.rs"]
mod historical_delta;
#[path = "../tests/historical_question.rs"]
mod historical_question;
#[path = "../tests/incremental_question.rs"]
mod incremental_question;
#[path = "../tests/ingress_positions.rs"]
mod ingress_positions;
#[path = "../tests/kernel_quint.rs"]
mod kernel_quint;
#[path = "../tests/ledger_export.rs"]
mod ledger_export;
#[path = "../tests/lifecycle_correspondence.rs"]
mod lifecycle_correspondence;
#[path = "../tests/link_read_page.rs"]
mod link_read_page;
#[path = "../tests/linked_time_question.rs"]
mod linked_time_question;
#[path = "../tests/markings.rs"]
mod markings;
#[path = "../tests/normalized_identity.rs"]
mod normalized_identity;
#[path = "../tests/open_from_checkpoint.rs"]
mod open_from_checkpoint;
#[path = "../tests/parked.rs"]
mod parked;
#[path = "../tests/quantity_refusal_boundary.rs"]
mod quantity_refusal_boundary;
#[path = "../tests/quantity_reservations.rs"]
mod quantity_reservations;
#[path = "../tests/query_fanout.rs"]
mod query_fanout;
#[path = "../tests/query_lineage.rs"]
mod query_lineage;
#[path = "../tests/reactive_query.rs"]
mod reactive_query;
#[path = "../tests/recovery.rs"]
mod recovery;
#[path = "../tests/reopen_after_definitions.rs"]
mod reopen_after_definitions;
#[path = "../tests/restatement_activation.rs"]
mod restatement_activation;
#[path = "../tests/restore.rs"]
mod restore;
#[path = "../tests/scrap_yield_incremental.rs"]
mod scrap_yield_incremental;
#[path = "../tests/snapshot_question.rs"]
mod snapshot_question;
#[path = "../tests/sql_engine.rs"]
mod sql_engine;
#[path = "../tests/startup_memory.rs"]
mod startup_memory;
#[path = "../tests/tables.rs"]
mod tables;
#[path = "../tests/temporal_read_boundary.rs"]
mod temporal_read_boundary;
#[path = "../tests/temporal_source_world.rs"]
mod temporal_source_world;
#[path = "../tests/trace_at_frontier.rs"]
mod trace_at_frontier;
#[path = "../tests/transform.rs"]
mod transform;
#[path = "../tests/transparency.rs"]
mod transparency;
#[path = "../tests/value_commitments.rs"]
mod value_commitments;
