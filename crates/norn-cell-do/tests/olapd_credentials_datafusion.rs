//! The `olapd_credentials` suite with the daemon running `datafusion` (D136 / D137).

use super::datafusion_backend as analytics_backend;
#[path = "olapd_credentials.rs"]
mod suite;
