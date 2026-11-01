//! OBS-EVIDENCE-1: a trusted adapter supplies data, never business mutation authority.
use crate::{host::DevHost, refusal::Refusal, scope::CellScope};
use norn_cell::engine::SqlEngine;
use norn_ir::RuntimeValue;
use norn_kernel::Observed;

impl<E: SqlEngine> DevHost<E> {
    /// Run the trusted adapter's checked submission and enqueue its complete evidence.
    /// A callback keeps transport dependencies outside the cell/API contracts.
    /// Scope comes from trusted connector configuration; this is not a network route.
    /// Mapping and observation consumption happen later through the cell driver.
    ///
    /// # Errors
    /// Scope mismatch, adapter framing/authentication failure, or durable inbox failure.
    pub fn observe_submission(
        &self,
        scope: &CellScope,
        checked: impl FnOnce() -> Result<Observed<RuntimeValue>, Refusal>,
    ) -> Result<bool, Refusal> {
        self.scope()
            .admit(Some(&scope.org_id), Some(&scope.environment_id))?;
        let _epoch = self.definition_read()?;
        let observed = checked()?;
        let value = norn_ir::runtime::encode(&observed).map_err(Refusal::host)?;
        self.cell()?
            .inject(&observed.id.0, &value)
            .map_err(Refusal::host)
    }
}
