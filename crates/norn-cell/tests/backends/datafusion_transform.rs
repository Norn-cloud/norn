//! VIEW-ENGINE-1 parity on `DataFusion` (#1765): every answer this backend
//! checks must name `datafusion`, so `DuckDB` can never satisfy the suite.

use norn_cell::compute::Engine;
use norn_cell::compute::transform::{RowAccess, TransformResult};
use norn_cell::{CellError, CellStore, ObjectRecord};
use norn_checker::transform::TransformPlan;

/// The engine label every answer of this backend names.
pub(crate) const ENGINE: Engine = Engine::DataFusion;

/// `plan` evaluated on this backend over `store`'s authorized snapshot.
pub(crate) fn evaluate(
    store: &CellStore,
    plan: &TransformPlan,
    (world, scope): (&str, &str),
    access: impl FnMut(&ObjectRecord) -> Result<RowAccess, CellError>,
) -> Result<TransformResult, CellError> {
    store.evaluate_transform_datafusion(plan, world, scope, access)
}
