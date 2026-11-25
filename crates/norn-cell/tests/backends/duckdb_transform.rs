//! VIEW-ENGINE-1 parity on `DuckDB`: the engine `duckdb_transform` lowers to.

use norn_cell::compute::Engine;
use norn_cell::compute::transform::{RowAccess, TransformResult};
use norn_cell::{CellError, CellStore, ObjectRecord};
use norn_checker::transform::TransformPlan;

/// The engine label every answer of this backend names.
pub(crate) const ENGINE: Engine = Engine::Duckdb;

/// `plan` evaluated on this backend over `store`'s authorized snapshot.
pub(crate) fn evaluate(
    store: &CellStore,
    plan: &TransformPlan,
    (world, scope): (&str, &str),
    access: impl FnMut(&ObjectRecord) -> Result<RowAccess, CellError>,
) -> Result<TransformResult, CellError> {
    store.evaluate_transform_duckdb(plan, world, scope, access)
}
