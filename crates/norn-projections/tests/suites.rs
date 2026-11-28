//! `norn-projections`'s integration tests, built as one test binary.
//!
//! Each `tests/<name>.rs` is the module `<name>` here, so nextest names a test
//! `binary_id(norn-projections::suites)` and `test(=<name>::<test>)`, and
//! `cargo test -p norn-projections --test suites <name>::` runs one former binary.
//! A `#[path]` module owns its directory as a crate root does, so each suite's
//! own `mod` and `#[path]` helpers resolve as they did when it was a binary.
//! A new suite is a new module here: `scripts/codemods/fold_integration_tests.py`
//! keeps this list and every reference to it in step.
#![allow(
    dead_code,
    unreachable_pub,
    reason = "each suite keeps the visibility it had as its own test binary"
)]

#[path = "cedar.rs"]
mod cedar;
#[path = "interop_exports.rs"]
mod interop_exports;
#[path = "lifecycle_diagram.rs"]
mod lifecycle_diagram;
#[path = "lineage.rs"]
mod lineage;
#[path = "lineage_world.rs"]
mod lineage_world;
#[path = "mcp_action_tool.rs"]
mod mcp_action_tool;
#[path = "sqlite_ddl.rs"]
mod sqlite_ddl;
#[path = "tool_manifest.rs"]
mod tool_manifest;
#[path = "widget_ux.rs"]
mod widget_ux;
