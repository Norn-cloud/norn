//! `norn-builtins`'s integration tests, built as one test binary.
//!
//! Each `tests/<name>.rs` is the module `<name>` here, so nextest names a test
//! `binary_id(norn-builtins::suites)` and `test(=<name>::<test>)`, and
//! `cargo test -p norn-builtins --test suites <name>::` runs one former binary.
//! A `#[path]` module owns its directory as a crate root does, so each suite's
//! own `mod` and `#[path]` helpers resolve as they did when it was a binary.
//! A new suite is a new module here: `scripts/codemods/fold_integration_tests.py`
//! keeps this list and every reference to it in step.

#[path = "decision.rs"]
mod decision;
#[path = "geo.rs"]
mod geo;
#[path = "pin.rs"]
mod pin;
