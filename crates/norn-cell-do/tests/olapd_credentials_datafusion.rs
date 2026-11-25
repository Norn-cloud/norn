//! The `olapd_credentials` suite with the daemon running `datafusion` (D136 / D137).

#[path = "backends/datafusion.rs"]
mod analytics_backend;
#[path = "olapd_credentials.rs"]
mod suite;
