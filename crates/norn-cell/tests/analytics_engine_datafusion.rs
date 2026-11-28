//! The `analytics_engine` parity suite on `datafusion` (D136 / D137).

use super::query_support;
#[path = "backends/datafusion.rs"]
mod analytics_backend;
#[path = "analytics_engine.rs"]
mod suite;
