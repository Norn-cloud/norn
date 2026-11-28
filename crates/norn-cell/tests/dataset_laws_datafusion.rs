//! The `dataset_laws` parity suite on `datafusion` (D136 / D137).

use super::dataset_cell;
use super::retail_truth_slice;
#[path = "backends/datafusion.rs"]
mod analytics_backend;
#[path = "dataset_laws.rs"]
mod suite;
