//! The `olapd_credentials` suite with the daemon running `duckdb` (D136 / D137).

/// The engine the daemon is started with.
const ENGINE: norn_cell::analytics::AnalyticalEngineId =
    norn_cell::analytics::AnalyticalEngineId::DuckDb;

#[path = "olapd_credentials.rs"]
mod suite;
