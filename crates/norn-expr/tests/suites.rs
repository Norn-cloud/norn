//! `norn-expr`'s integration tests, built as one test binary.
//!
//! Each `tests/<name>.rs` is the module `<name>` here, so nextest names a test
//! `binary_id(norn-expr::suites)` and `test(=<name>::<test>)`, and
//! `cargo test -p norn-expr --test suites <name>::` runs one former binary.
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

#[path = "build_support.rs"]
mod build_support;
#[path = "check.rs"]
mod check;
#[path = "corpus.rs"]
mod corpus;
#[path = "cost.rs"]
mod cost;
#[path = "decimal_binding_zero.rs"]
mod decimal_binding_zero;
#[path = "fixtures.rs"]
mod fixtures;
#[path = "profile_conformance.rs"]
mod profile_conformance;
#[path = "temporal_intervals.rs"]
mod temporal_intervals;
#[path = "timezone_corpus.rs"]
mod timezone_corpus;
#[path = "type_env.rs"]
mod type_env;
