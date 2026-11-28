//! `norn-formats`'s integration tests, built as one test binary.
//!
//! Each `tests/<name>.rs` is the module `<name>` here, so nextest names a test
//! `binary_id(norn-formats::suites)` and `test(=<name>::<test>)`, and
//! `cargo test -p norn-formats --test suites <name>::` runs one former binary.
//! A `#[path]` module owns its directory as a crate root does, so each suite's
//! own `mod` and `#[path]` helpers resolve as they did when it was a binary.
//! A new suite is a new module here: `scripts/codemods/fold_integration_tests.py`
//! keeps this list and every reference to it in step.
#![allow(
    dead_code,
    unreachable_pub,
    reason = "each suite keeps the visibility it had as its own test binary"
)]

// Shared test support, compiled once for every suite in this binary.
#[allow(
    clippy::expect_used,
    clippy::panic,
    reason = "shared importer conformance harness uses test-only expect/panic"
)]
mod support;

#[path = "owl.rs"]
mod owl;
#[path = "owl_align.rs"]
mod owl_align;
#[path = "owl_scale.rs"]
mod owl_scale;
#[path = "pack.rs"]
mod pack;
#[path = "xsd.rs"]
mod xsd;
