//! `norn-decisions`'s integration tests, built as one test binary.
//!
//! Each `tests/<name>.rs` is the module `<name>` here, so nextest names a test
//! `binary_id(norn-decisions::suites)` and `test(=<name>::<test>)`, and
//! `cargo test -p norn-decisions --test suites <name>::` runs one former binary.
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

#[path = "dmn_tck.rs"]
mod dmn_tck;
#[path = "evaluate.rs"]
mod evaluate;
#[path = "explain.rs"]
mod explain;
#[path = "flow_vertical.rs"]
mod flow_vertical;
#[path = "patch_fact.rs"]
mod patch_fact;
#[path = "principal_inputs.rs"]
mod principal_inputs;
#[path = "zen_corpus.rs"]
mod zen_corpus;
