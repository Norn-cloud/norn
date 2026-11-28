//! `norn-authoring`'s integration tests, built as one test binary.
//!
//! Each `tests/<name>.rs` is the module `<name>` here, so nextest names a test
//! `binary_id(norn-authoring::suites)` and `test(=<name>::<test>)`, and
//! `cargo test -p norn-authoring --test suites <name>::` runs one former binary.
//! A `#[path]` module owns its directory as a crate root does, so each suite's
//! own `mod` and `#[path]` helpers resolve as they did when it was a binary.
//! A new suite is a new module here: `scripts/codemods/fold_integration_tests.py`
//! keeps this list and every reference to it in step.

#[path = "compiler_parity.rs"]
mod compiler_parity;
#[path = "corpus_parity.rs"]
mod corpus_parity;
#[path = "fmt_parity.rs"]
mod fmt_parity;
#[path = "program_smoke.rs"]
mod program_smoke;
