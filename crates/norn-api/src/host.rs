//! One already-opened tenant cell shared by authorized transport adapters.
use crate::{
    authz::{Authorizer, DenyAll, ReadCapability, ReadRequest, read_denied},
    refusal::Refusal,
    scope::{CellScope, ScopeQuery},
};
use http::StatusCode;
use norn_cell::{
    CellError, CellStore, InstalledDefinitionInput, InstalledDefinitionSet, Model,
    engine::SqlEngine,
};
use norn_ir::RuntimeObject;
use norn_kernel::{Command, FactEvent};
use std::sync::{Mutex, MutexGuard, RwLock, RwLockReadGuard};

pub(crate) type ActionCell<E> = norn_cell::SqliteCell<FactEvent, RuntimeObject, RuntimeObject, E>;
/// A single tenant's cell and definition epoch. Unconfigured authorization denies.
pub struct DevHost<E: SqlEngine> {
    scope: CellScope,
    pub(crate) authorizer: Box<dyn Authorizer>,
    definitions: RwLock<()>,
    cell: Mutex<ActionCell<E>>,
}
impl<E: SqlEngine> DevHost<E> {
    /// Bind an opened store before publishing any read service.
    /// The outside-world responder belongs to trusted bootstrap, never a read request.
    /// # Errors
    /// Blank scope or a store bound to a different tenant refuses.
    pub fn from_store(
        store: CellStore<E>,
        scope: CellScope,
        responder: impl FnMut(&Command<RuntimeObject>) -> RuntimeObject + Send + 'static,
    ) -> Result<Self, Refusal> {
        scope.admit(Some(&scope.org_id), Some(&scope.environment_id))?;
        let cell = ActionCell::new(store, scope.key(), responder).map_err(Refusal::host)?;
        Ok(Self {
            scope,
            authorizer: Box::new(DenyAll),
            definitions: RwLock::new(()),
            cell: Mutex::new(cell),
        })
    }
    /// Supply the trusted server policy before sharing this host.
    #[must_use]
    pub fn with_authorizer(mut self, authorizer: impl Authorizer + 'static) -> Self {
        self.authorizer = Box::new(authorizer);
        self
    }
    /// Physical scope, never taken from client claims.
    #[must_use]
    pub const fn scope(&self) -> &CellScope {
        &self.scope
    }
    /// Hold the epoch through authorization and projection.
    /// # Errors
    /// Poisoned synchronization refuses closed.
    pub fn definition_read(&self) -> Result<RwLockReadGuard<'_, ()>, Refusal> {
        self.definitions.read().map_err(Refusal::host)
    }
    /// Hold the epoch exclusively while a definition install commits.
    /// # Errors
    /// Poisoned synchronization refuses closed.
    pub fn definition_write(&self) -> Result<std::sync::RwLockWriteGuard<'_, ()>, Refusal> {
        self.definitions.write().map_err(Refusal::host)
    }
    /// HOST-READ-1: authorize and check explicit scope before opening storage.
    /// # Errors
    /// Every policy or scope failure has the same opaque 403 document.
    pub fn read_access(
        &self,
        scope: &ScopeQuery,
        request: &ReadRequest<'_>,
    ) -> Result<RwLockReadGuard<'_, ()>, Refusal> {
        let epoch = self.definition_read();
        let allowed = self.read_permitted(request);
        let scoped = self
            .scope
            .admit(scope.org_id.as_deref(), scope.environment_id.as_deref());
        if !allowed || scoped.is_err() {
            return Err(read_denied());
        }
        epoch
    }
    pub(crate) fn read_permitted(&self, request: &ReadRequest<'_>) -> bool {
        let physical = norn_kernel::authz::ScopeKey {
            org: self.scope.org_id.clone(),
            environment: Some(self.scope.environment_id.clone()),
            tenant: None,
            workspace: None,
            business_unit: None,
        };
        let mut bound = request.access.clone();
        let matches = bound.scope.as_ref() == Some(&physical)
            && bound.resource.scope == physical
            && (request.capability != ReadCapability::Population || bound.action_id == "norn.read");
        bound.scope = Some(physical.clone());
        bound.resource.scope = physical;
        let allowed = self
            .authorizer
            .read(&ReadRequest {
                capability: request.capability,
                access: &bound,
            })
            .is_ok();
        matches && allowed
    }
    pub(crate) fn cell(&self) -> Result<MutexGuard<'_, ActionCell<E>>, Refusal> {
        self.cell.lock().map_err(Refusal::host)
    }

    /// The active definitions revision, read under the definition epoch, or
    /// `None` for a cell with nothing installed. Never writes.
    /// # Errors
    /// Poisoned synchronization or a store failure refuses closed.
    pub fn active_definitions(&self) -> Result<Option<InstalledDefinitionSet>, Refusal> {
        let _epoch = self.definition_read()?;
        let cell = self.cell()?;
        cell.store()
            .read()
            .installed_definitions(None)
            .map_err(Refusal::host)
    }

    /// Install a checker-admitted definition set as the next revision, only
    /// if the active revision is still `expected_version`. The trusted host
    /// process calls this (`norn dev`); no network route reaches it.
    ///
    /// The definition epoch is held exclusively, so no read or action sees a
    /// half-installed revision. A refusal leaves the active revision, its
    /// ledger and every object exactly as they were.
    /// # Errors
    /// `defs.stale` (409) when another revision was installed since the
    /// caller planned; `defs.migration_refused` (409) when the candidate
    /// cannot carry the cell's live data; `host.error` otherwise.
    pub fn install_definitions(
        &self,
        candidate_model: Model,
        input: InstalledDefinitionInput,
        expected_version: Option<u64>,
    ) -> Result<InstalledDefinitionSet, Refusal> {
        let _epoch = self.definition_write()?;
        let mut cell = self.cell()?;
        let installed = cell
            .install_definitions(candidate_model, input, expected_version)
            .map_err(|error| {
                if let CellError::DefinitionVersionConflict { expected, actual } = &error {
                    return Refusal::new(
                        StatusCode::CONFLICT,
                        "defs.stale",
                        "the definitions changed since this revision was planned",
                        format!("planned against {expected:?}, the cell is at {actual:?}; re-plan against the active revision"),
                    );
                }
                if let CellError::DefinitionMigrationRefused(reason) = &error {
                    return Refusal::new(
                        StatusCode::CONFLICT,
                        "defs.migration_refused",
                        "the candidate definitions cannot carry the cell's live data",
                        reason.clone(),
                    );
                }
                Refusal::host(error)
            })?;
        self.model_changed(&cell);
        Ok(installed)
    }

    /// Tell the authorizer the model changed, under the write epoch the
    /// caller holds, with the cell it just installed into.
    pub(crate) fn model_changed(&self, cell: &ActionCell<E>) {
        let types: Vec<norn_ir::ObjectTypeDef> =
            cell.store().model().object_types().cloned().collect();
        self.authorizer.model_changed(&types);
    }
}
