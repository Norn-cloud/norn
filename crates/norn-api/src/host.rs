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
        self.0
            .as_ref()
            .unwrap_or_else(|| unreachable!("a cell guard always holds the cell"))
    }
}

impl<E: SqlEngine> DerefMut for CellGuard<'_, E> {
    fn deref_mut(&mut self) -> &mut ActionCell<E> {
        self.0
            .as_mut()
            .unwrap_or_else(|| unreachable!("a cell guard always holds the cell"))
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

    /// Capture source rows and map them through the cell's own mapping path,
    /// under the definition epoch, in OBS-PAGE-1 pages: consecutive rows with
    /// the same [`MappingInput`](norn_cell::mapping_bundle::MappingInput),
    /// split at [`PAGE_ROWS`] and before a repeated source record, are
    /// captured by `inject_mapping_page` and committed by
    /// `process_mapping_page`. The trusted host's sync calls this; no caller
    /// supplies facts, and a refused row stays in the inbox as quarantined
    /// evidence with its reasons. Rows already captured are captured once.
    /// Returns one outcome per row.
    /// # Errors
    /// A page the cell cannot capture (framing or pins it refuses), or a
    /// store failure; pages before it stay captured and processed.
    pub fn ingest(
        &self,
        rows: &[(
            Observed<RuntimeValue>,
            norn_cell::mapping_bundle::MappingInput,
        )],
    ) -> Result<Vec<Ingested>, Refusal> {
        let _epoch = self.definition_read()?;
        let mut slot = self.cell.lock().map_err(Refusal::host)?;
        let cell = slot
            .take()
            .ok_or_else(|| Refusal::host("the cell is unavailable"))?;
        let mut observing: norn_cell::SqliteCell<
            ObservationEvent,
            RuntimeObject,
            RuntimeObject,
            E,
        > = cell.into_profile();
        let mut out = Vec::with_capacity(rows.len());
        let mut result = Ok(());
        for (input, page) in pages(rows) {
            result = ingest_page(&mut observing, &page, input, &mut out);
            if result.is_err() {
                break;
            }
        }
        *slot = Some(observing.into_profile());
        result.map(|()| out).map_err(Refusal::host)
    }

    /// One artifact of the active definitions, as its JSON envelope. For the
    /// trusted host process (a source sync reading its connector); no route
    /// reaches it.
    /// # Errors
    /// Poisoned synchronization or a store failure refuses closed.
    pub fn installed_artifact(
        &self,
        artifact_id: &str,
    ) -> Result<Option<serde_json::Value>, Refusal> {
        let Some(active) = self.active_definitions()? else {
            return Ok(None);
        };
        let set: Vec<serde_json::Value> =
            serde_json::from_str(&active.canonical_set).map_err(Refusal::host)?;
        Ok(set.into_iter().find(|artifact| {
            artifact
                .get("artifactId")
                .and_then(serde_json::Value::as_str)
                == Some(artifact_id)
        }))
    }

    /// One object's current values in the main world. For the trusted host
    /// process (a source sync reading its own connection state); no route
    /// reaches it, and it grants no read to anyone.
    /// # Errors
    /// Poisoned synchronization or a store failure refuses closed.
    pub fn host_object(
        &self,
        object_type: &str,
        key: &str,
    ) -> Result<Option<RuntimeObject>, Refusal> {
        let _epoch = self.definition_read()?;
        let cell = self.cell()?;
        cell.store()
            .read()
            .object(norn_cell::MAIN_WORLD, object_type, key)
            .map(|row| row.map(|row| row.values))
            .map_err(Refusal::host)
    }

    /// The cell model's object type names, in model order: the types a catalog
    /// (an agent's tool schema) may offer. Naming a type grants nothing; every
    /// read of it is still authorized.
    /// # Errors
    /// Poisoned synchronization refuses closed.
    pub fn object_type_names(&self) -> Result<Vec<String>, Refusal> {
        let cell = self.cell()?;
        Ok(cell
            .store()
            .model()
            .object_types()
            .map(|object_type| object_type.name.as_str().to_owned())
            .collect())
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

/// The most rows one OBS-PAGE-1 page carries; the cell's own bound is higher.
pub const PAGE_ROWS: usize = 500;

/// OBS-PAGE-1 framing: consecutive rows with the same mapping input, split at
/// [`PAGE_ROWS`] and before a source record the page already carries.
fn pages(
    rows: &[(
        Observed<RuntimeValue>,
        norn_cell::mapping_bundle::MappingInput,
    )],
) -> Vec<(
    &norn_cell::mapping_bundle::MappingInput,
    Vec<Observed<RuntimeValue>>,
)> {
    let mut pages: Vec<(
        &norn_cell::mapping_bundle::MappingInput,
        Vec<Observed<RuntimeValue>>,
    )> = Vec::new();
    let mut seen = std::collections::BTreeSet::new();
    for (raw, input) in rows {
        let identity = raw.provenance.source_id.clone();
        let split = pages.last().is_none_or(|(current, page)| {
            *current != input || page.len() >= PAGE_ROWS || !seen.insert(identity.clone())
        });
        if split {
            seen = std::collections::BTreeSet::from([identity]);
            pages.push((input, Vec::new()));
        }
        if let Some((_, page)) = pages.last_mut() {
            page.push(raw.clone());
        }
    }
    pages
}

/// Capture and commit one page, one outcome per row into `out`. A page over
/// the cell's byte bound is halved. A row the page did not report (captured
/// as a finding, or a page an earlier sync consumed) is processed on its own,
/// as one-by-one delivery would.
fn ingest_page<E: SqlEngine>(
    cell: &mut norn_cell::SqliteCell<ObservationEvent, RuntimeObject, RuntimeObject, E>,
    page: &[Observed<RuntimeValue>],
    input: &norn_cell::mapping_bundle::MappingInput,
    out: &mut Vec<Ingested>,
) -> Result<(), CellError> {
    let captured = match cell.inject_mapping_page(page, input) {
        Err(CellError::OutOfRange("mapping page bytes")) if page.len() > 1 => {
            let (first, second) = page.split_at(page.len() / 2);
            ingest_page(cell, first, input, out)?;
            return ingest_page(cell, second, input, out);
        }
        other => other?,
    };
    let start = out.len();
    for (id, _new) in &captured {
        for (member, outcome) in cell.process_mapping_page(id)?.unwrap_or_default() {
            out.push(Ingested {
                observation_id: member,
                outcome,
            });
        }
    }
    if out.len().saturating_sub(start) < page.len() {
        let reported: std::collections::BTreeSet<String> = out
            .iter()
            .skip(start)
            .map(|i| i.observation_id.clone())
            .collect();
        for raw in page {
            let (id, _new) = cell.inject_mapping_bundle(raw, input)?;
            if !reported.contains(&id) {
                let outcome = cell.process_mapping_outcome(&id)?;
                out.push(Ingested {
                    observation_id: id,
                    outcome,
                });
            }
        }
    }
    Ok(())
}

#[cfg(test)]
#[allow(
    clippy::expect_used,
    clippy::indexing_slicing,
    reason = "page framing scenarios"
)]
mod page_tests {
    use super::*;
    use crate::pull::{PullProfile, Row, observation};
    use norn_cell::mapping_bundle::MappingInput;

    fn row(id: &str) -> Observed<RuntimeValue> {
        let profile = PullProfile {
            source: "erpnext".into(),
            pipeline: "norn-pull/1".into(),
            feeds: Vec::new(),
            token: None,
        };
        let row = Row {
            dataset: "items".into(),
            source_id: id.into(),
            version: "1".into(),
            payload: RuntimeValue::Object(std::collections::BTreeMap::new()),
        };
        observation(&profile, &row, 0).expect("sealed")
    }

    fn input(dataset: &str) -> MappingInput {
        MappingInput {
            mapping: "erpnext.sandbox.mapping".into(),
            dataset: dataset.into(),
            connector_instance: "sandbox".into(),
            partition: dataset.into(),
            recorded_at: norn_ir::provenance::UtcTimestamp("2026-11-12T00:00:00.000Z".into()),
        }
    }

    /// OBS-PAGE-1 framing: a page is one mapping input, at most `PAGE_ROWS`
    /// rows, and never carries one source record twice.
    #[test]
    fn pages_split_at_the_bound_a_new_input_and_a_repeated_record() {
        let (items, bins) = (input("items"), input("bins"));
        let many: Vec<_> = (0..1_200)
            .map(|n| (row(&format!("r{n}")), items.clone()))
            .collect();
        let sizes: Vec<usize> = pages(&many).iter().map(|(_, page)| page.len()).collect();
        assert_eq!(sizes, [500, 500, 200]);

        let mixed = vec![
            (row("x"), items.clone()),
            (row("y"), items.clone()),
            (row("x"), items.clone()),
            (row("z"), bins.clone()),
            (row("z"), items.clone()),
        ];
        let framed: Vec<(String, Vec<String>)> = pages(&mixed)
            .into_iter()
            .map(|(input, page)| {
                (
                    input.dataset.clone(),
                    page.iter()
                        .map(|raw| raw.provenance.source_id.clone())
                        .collect(),
                )
            })
            .collect();
        assert_eq!(
            framed,
            [
                ("items".to_owned(), vec!["x".to_owned(), "y".to_owned()]),
                ("items".to_owned(), vec!["x".to_owned()]),
                ("bins".to_owned(), vec!["z".to_owned()]),
                ("items".to_owned(), vec!["z".to_owned()]),
            ]
        );
    }
}
