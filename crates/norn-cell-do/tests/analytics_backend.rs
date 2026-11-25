//! D136 parity suites on `DuckDB`: the engine the plain suites run on.

#[path = "backends/duckdb.rs"]
mod hooks;
#[allow(unused_imports, reason = "each suite uses some of these")]
pub(crate) use hooks::*;
