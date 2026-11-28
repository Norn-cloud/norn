//! `norn-testkit`'s integration tests, built as one test binary.
//!
//! Each `tests/<name>.rs` is the module `<name>` here, so nextest names a test
//! `binary_id(norn-testkit::suites)` and `test(=<name>::<test>)`, and
//! `cargo test -p norn-testkit --test suites <name>::` runs one former binary.
//! A `#[path]` module owns its directory as a crate root does, so each suite's
//! own `mod` and `#[path]` helpers resolve as they did when it was a binary.
//! A new suite is a new module here: `scripts/codemods/fold_integration_tests.py`
//! keeps this list and every reference to it in step.

#[path = "decide_commit.rs"]
mod decide_commit;
#[path = "harness.rs"]
mod harness;
#[path = "link_target_identity.rs"]
mod link_target_identity;
#[path = "pack_mbt_adversarial.rs"]
mod pack_mbt_adversarial;
#[path = "pack_mbt_honesty.rs"]
mod pack_mbt_honesty;
#[path = "pack_mbt_sqlite.rs"]
mod pack_mbt_sqlite;
