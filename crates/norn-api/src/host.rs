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
use norn_ir::{RuntimeObject, RuntimeValue};
use norn_kernel::{Command, FactEvent, ObservationEvent, Observed};
use std::ops::{Deref, DerefMut};
use std::sync::{Mutex, MutexGuard, RwLock, RwLockReadGuard};

pub(crate) type ActionCell<E> = norn_cell::SqliteCell<FactEvent, RuntimeObject, RuntimeObject, E>;
/// A single tenant's cell and definition epoch. Unconfigured authorization denies.
pub struct DevHost<E: SqlEngine> {
    scope: CellScope,
    pub(crate) authorizer: Box<dyn Authorizer>,
    definitions: RwLock<()>,
    /// Always `Some` outside [`DevHost::ingest`], which holds the lock while
    /// it maps source rows under the observation profile.
    cell: Mutex<Option<ActionCell<E>>>,
}

/// The locked cell, present whenever a guard exists.
pub(crate) struct CellGuard<'a, E: SqlEngine>(MutexGuard<'a, Option<ActionCell<E>>>);

impl<E: SqlEngine> Deref for CellGuard<'_, E> {
    type Target = ActionCell<E>;
    fn deref(&self) -> &ActionCell<E> {
        // `cell()` admits only a present cell, and nothing takes it while the guard lives.
        self.0.as_ref().unwrap_or_else(|| unreachable!("a cell guard always holds the cell"))
    }
}

impl<E: SqlEngine> DerefMut for CellGuard<'_, E> {
    fn deref_mut(&mut self) -> &mut ActionCell<E> {
        self.0.as_mut().unwrap_or_else(|| unreachable!("a cell guard always holds the cell"))
    }
}

/// What one captured source row came to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ingested {
    /// The inbox id the cell captured the row under.
    pub observation_id: String,
    /// What processing did.
    pub outcome: norn_cell::MappingOutcome,
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
            cell: Mutex::new(Some(cell)),
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
    pub(crate) fn cell(&self) -> Result<CellGuard<'_, E>, Refusal> {
        let guard = self.cell.lock().map_err(Refusal::host)?;
        if guard.is_none() {
            return Err(Refusal::host("the cell is unavailable"));
        }
        Ok(CellGuard(guard))
    }

    /// Capture source rows and map them through the cell's own mapping path
    /// (`inject_mapping_bundle`, then `process_mapping_bundle`), under the
    /// definition epoch. The trusted host's sync calls this; no caller
    /// supplies facts, and a refused row stays in the inbox as quarantined
    /// evidence with its reasons. Rows already captured are captured once.
    /// # Errors
    /// A row the cell cannot capture (framing or pins it refuses), or a store
    /// failure; rows before it stay captured and processed.
    pub fn ingest(
        &self,
        rows: &[(Observed<RuntimeValue>, norn_cell::mapping_bundle::MappingInput)],
    ) -> Result<Vec<Ingested>, Refusal> {
        let _epoch = self.definition_read()?;
        let mut slot = self.cell.lock().map_err(Refusal::host)?;
        let cell = slot.take().ok_or_else(|| Refusal::host("the cell is unavailable"))?;
        let mut observing: norn_cell::SqliteCell<ObservationEvent, RuntimeObject, RuntimeObject, E> =
            cell.into_profile();
        let mut out = Vec::with_capacity(rows.len());
        let mut failure = None;
        for (raw, input) in rows {
            let step = observing.inject_mapping_bundle(raw, input).and_then(|(id, _new)| {
                let outcome = observing.process_mapping_outcome(&id)?;
                Ok(Ingested { observation_id: id, outcome })
            });
            match step {
                Ok(ingested) => out.push(ingested),
                Err(error) => {
                    failure = Some(error);
                    break;
                }
            }
        }
        *slot = Some(observing.into_profile());
        match failure {
            Some(error) => Err(Refusal::host(error)),
            None => Ok(out),
        }
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
        cell.install_definitions(candidate_model, input, expected_version)
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
            })
    }
}
