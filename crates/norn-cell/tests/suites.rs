//! `norn-cell`'s integration tests, built as one test binary.
//!
//! Each `tests/<name>.rs` is the module `<name>` here, so nextest names a test
//! `binary_id(norn-cell::suites)` and `test(=<name>::<test>)`, and
//! `cargo test -p norn-cell --test suites <name>::` runs one former binary.
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

#[cfg(all(feature = "duckdb", feature = "native"))]
#[path = "analytics_backend.rs"]
mod analytics_backend;
#[path = "asof_reader.rs"]
mod asof_reader;
#[path = "bench_clauses.rs"]
mod bench_clauses;
#[path = "bench_import.rs"]
mod bench_import;
#[cfg(all(feature = "duckdb", feature = "native"))]
#[path = "bench_manifest.rs"]
mod bench_manifest;
#[path = "bench_mapping.rs"]
mod bench_mapping;
#[path = "bench_rules.rs"]
mod bench_rules;
#[path = "cell_host.rs"]
mod cell_host;
#[path = "cell_host_dst.rs"]
mod cell_host_dst;
#[path = "commit_path.rs"]
mod commit_path;
#[cfg(all(feature = "duckdb", feature = "native"))]
#[path = "dataset_laws.rs"]
mod dataset_laws;
#[cfg(all(feature = "datafusion", feature = "duckdb"))]
#[path = "dataset_laws_datafusion.rs"]
mod dataset_laws_datafusion;
#[path = "definition_scope.rs"]
mod definition_scope;
#[path = "dirty_data_wedge.rs"]
mod dirty_data_wedge;
#[path = "fork_questions.rs"]
mod fork_questions;
#[path = "governed_create.rs"]
mod governed_create;
#[path = "jewelry_pricing.rs"]
mod jewelry_pricing;
#[path = "label_identity.rs"]
mod label_identity;
#[path = "lifecycle_initialization.rs"]
mod lifecycle_initialization;
#[path = "lifecycle_updates.rs"]
mod lifecycle_updates;
#[path = "mapping_links.rs"]
mod mapping_links;
#[cfg(not(target_arch = "wasm32"))]
#[path = "mcp_kernel_parity.rs"]
mod mcp_kernel_parity;
#[path = "model_interfaces.rs"]
mod model_interfaces;
#[path = "ontology_lending.rs"]
mod ontology_lending;
#[cfg(feature = "native")]
#[path = "outbox_generations.rs"]
mod outbox_generations;
#[cfg(feature = "native")]
#[path = "outbox_generations_restore.rs"]
mod outbox_generations_restore;
#[path = "parent_rows.rs"]
mod parent_rows;
#[path = "principal_inputs.rs"]
mod principal_inputs;
#[path = "query_envelope.rs"]
mod query_envelope;
#[path = "query_lineage_sources.rs"]
mod query_lineage_sources;
#[path = "rea_inventory.rs"]
mod rea_inventory;
#[path = "read_envelope.rs"]
mod read_envelope;
#[path = "receipt_consume.rs"]
mod receipt_consume;
#[path = "retail_truth_slice.rs"]
mod retail_truth_slice;
#[path = "retained_definitions.rs"]
mod retained_definitions;
#[path = "source_agreement.rs"]
mod source_agreement;
#[path = "stress_purchase.rs"]
mod stress_purchase;
#[path = "supply_lab_stress.rs"]
mod supply_lab_stress;
#[path = "synchronous_action.rs"]
mod synchronous_action;
