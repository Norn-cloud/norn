//! One already-opened tenant cell shared by authorized transport adapters.
use crate::{
    authz::{Authorizer, DenyAll},
    refusal::Refusal,
    scope::CellScope,
};
use norn_cell::{CellStore, engine::SqlEngine};
use std::sync::{Mutex, MutexGuard, RwLock, RwLockReadGuard};

pub(crate) type ActionCell<E> = norn_cell::SqliteCell<
    norn_kernel::FactEvent,
    norn_ir::RuntimeObject,
    norn_ir::RuntimeObject,
    E,
>;
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
        responder: impl FnMut(&norn_kernel::Command<norn_ir::RuntimeObject>) -> norn_ir::RuntimeObject
        + Send
        + 'static,
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
    pub(crate) fn cell(&self) -> Result<MutexGuard<'_, ActionCell<E>>, Refusal> {
        self.cell.lock().map_err(Refusal::host)
    }
}
