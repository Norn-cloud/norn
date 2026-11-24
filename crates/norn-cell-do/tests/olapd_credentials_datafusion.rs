//! The `olapd_credentials` suite with the daemon running `datafusion` (D136 / D137).

/// The engine the daemon is started with.
const ENGINE: norn_cell::analytics::AnalyticalEngineId =
    norn_cell::analytics::AnalyticalEngineId::DataFusion;

#[path = "olapd_credentials.rs"]
mod suite;
