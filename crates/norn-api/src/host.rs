//! One already-opened tenant cell shared by authorized transport adapters.
use crate::{
    authz::{Authorizer, DenyAll, ReadCapability, ReadRequest, read_denied},
    refusal::Refusal,
    scope::{CellScope, ScopeQuery},
};
use norn_cell::{CellStore, engine::SqlEngine};
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
}
