//! `norn-api`'s integration tests, built as one test binary.
//!
//! Each `tests/<name>.rs` is the module `<name>` here, so nextest names a test
//! `binary_id(norn-api::suites)` and `test(=<name>::<test>)`, and
//! `cargo test -p norn-api --test suites <name>::` runs one former binary.
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

#[path = "answer_contract.rs"]
mod answer_contract;
#[path = "catalog_counts.rs"]
mod catalog_counts;
#[path = "clock.rs"]
mod clock;
#[path = "disclosure_noninterference.rs"]
mod disclosure_noninterference;
#[path = "explorer_objects.rs"]
mod explorer_objects;
#[path = "host_perimeter.rs"]
mod host_perimeter;
#[path = "jcs_rfc8785_corpus.rs"]
mod jcs_rfc8785_corpus;
#[path = "link_held_findings.rs"]
mod link_held_findings;
#[path = "mcp_boundary.rs"]
mod mcp_boundary;
#[path = "object_facets.rs"]
mod object_facets;
#[path = "object_links.rs"]
mod object_links;
#[path = "object_projection.rs"]
mod object_projection;
#[path = "object_query.rs"]
mod object_query;
#[path = "object_routes.rs"]
mod object_routes;
#[path = "privacy.rs"]
mod privacy;
#[path = "pull_rest.rs"]
mod pull_rest;
#[path = "question_routes.rs"]
mod question_routes;
#[path = "read_authority.rs"]
mod read_authority;
#[path = "source_capture_atomicity.rs"]
mod source_capture_atomicity;
#[path = "studio_access.rs"]
mod studio_access;
#[path = "sync_schedule.rs"]
mod sync_schedule;
#[path = "transform_routes.rs"]
mod transform_routes;
#[path = "transform_studio.rs"]
mod transform_studio;
