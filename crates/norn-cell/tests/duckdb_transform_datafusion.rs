//! The `duckdb_transform` VIEW-ENGINE-1 parity suite on `datafusion` (#1765).

#[path = "duckdb_transform.rs"]
mod suite;
#[path = "backends/datafusion_transform.rs"]
mod transform_backend;
