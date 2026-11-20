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
    /// it maps one page of source rows under the observation profile.
    cell: Mutex<Option<ActionCell<E>>>,
    /// Requests waiting for [`Self::cell`]; a load yields to them between
    /// its commits (INGEST-YIELD-1).
    waiting: std::sync::atomic::AtomicUsize,
    /// INGEST-YIELD-1: a read holds this shared for its whole request, and a
    /// load holds it exclusively for each commit, so a multi-step read sees
    /// one snapshot and a load commits only between reads. Writers are
    /// preferred: a read arriving while a commit waits queues behind that one
    /// commit, so neither starves.
    reads: RwLock<()>,
    /// PRIVACY-1: the key this cell's pseudonyms are computed under; without
    /// one a pseudonym projection redacts.
    pseudonyms: Option<crate::privacy::PseudonymKey>,
    /// The marking catalog of the active definitions, by definitions version.
    markings: Mutex<Option<(Option<u64>, crate::privacy::MarkingCatalog)>>,
}

/// A read in progress ([`DevHost::reading`]): no load commits until it ends.
pub struct Reading<'a> {
    _pinned: Option<std::sync::RwLockReadGuard<'a, ()>>,
}

std::thread_local! {
    /// Reads in progress on this thread. A load the same request starts (a
    /// read-triggered sync) never waits for its own read.
    static READING: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

impl Drop for Reading<'_> {
    fn drop(&mut self) {
        READING.with(|reading| reading.set(reading.get().saturating_sub(1)));
    }
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

/// Whether a cell's model declares the kernel's action types: a provisioned
/// tenant cell (and `norn dev`) serves governed actions and reopens under
/// [`crate::kernel_set::serving_model`]; a cell this build seeds itself
/// reopens under the bare [`crate::kernel_set::kernel_model`].
#[must_use]
pub fn serves_actions(model: &Model) -> bool {
    model
        .object_types()
        .any(|declared| declared.name.as_str() == norn_kernel::ACTION_CALL_TYPE)
}

/// The model a restarted host rebuilds for `canonical_set` (HOST-REOPEN-1):
/// the serving model for a cell that serves actions, else the bare model.
/// # Errors
/// `host.error` when the set cannot be read or projected.
pub fn reopen_model(canonical_set: &str, serving: bool) -> Result<Model, Refusal> {
    let artifacts = crate::kernel_set::retained_artifacts(canonical_set).map_err(Refusal::host)?;
    if serving {
        crate::kernel_set::serving_model(&artifacts).map_err(Refusal::host)
    } else {
        crate::kernel_set::kernel_model(&artifacts)
            .map_err(|refusal| Refusal::host(format!("{refusal:?}")))
    }
}

/// DEFS-REOPEN-1: a revision installs only under the model a restarted host
/// rebuilds from its retained set ([`reopen_model`]); anything else would
/// commit a layout the next open refuses. Every path that installs a model
/// (definitions, flows, uploads, provisioning) checks it here, before
/// anything is written.
/// # Errors
/// `host.error` when the candidate's model or layout differs from the
/// reopen model of `canonical_set`, or the set cannot be read.
pub fn check_reopens(
    candidate_model: &Model,
    canonical_set: &str,
    serving: bool,
) -> Result<(), Refusal> {
    let reopened = reopen_model(canonical_set, serving)?;
    if reopened.model_hash() != candidate_model.model_hash()
        || reopened.ddl_hash() != candidate_model.ddl_hash()
    {
        return Err(Refusal::host(format!(
            "the candidate model is not the one the cell reopens under (model {} for {}, layout {} for {})",
            candidate_model.model_hash(),
            reopened.model_hash(),
            candidate_model.ddl_hash(),
            reopened.ddl_hash()
        )));
    }
    Ok(())
}

/// A load's work budget (INGEST-BATCH-1), in milliseconds of this host's
/// measured cost: a captured row, and a commit the cell attempts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WorkBudget {
    /// The cost of capturing one row.
    pub row_ms: u64,
    /// The cost of one commit attempt.
    pub commit_ms: u64,
    /// The most work one load may take.
    pub total_ms: u64,
}

/// What a load within a budget did ([`DevHost::ingest_within`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IngestReport {
    /// One outcome per reported row (`rows[..next]`, or every row), in row
    /// order.
    pub outcomes: Vec<Ingested>,
    /// Where the caller resumes (`rows[next..]`), when the budget stopped
    /// the load; `None` when every row is reported.
    pub next: Option<usize>,
    /// The OBS-PAGE-1 pages completed.
    pub pages: usize,
    /// The commits the cell attempted.
    pub commits: usize,
    /// The work charged, in the budget's milliseconds.
    pub work_ms: u64,
}

impl IngestReport {
    fn charge(&mut self, commit_ms: u64) {
        self.commits = self.commits.saturating_add(1);
        self.work_ms = self.work_ms.saturating_add(commit_ms);
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
            waiting: std::sync::atomic::AtomicUsize::new(0),
            reads: RwLock::new(()),
            pseudonyms: None,
            markings: Mutex::new(None),
        })
    }
    /// Supply the trusted server policy before sharing this host.
    #[must_use]
    pub fn with_authorizer(mut self, authorizer: impl Authorizer + 'static) -> Self {
        self.authorizer = Box::new(authorizer);
        self
    }
    /// Key this cell's pseudonyms with `secret` (the host's lineage key);
    /// a secret shorter than 32 bytes keys none, and pseudonyms redact.
    #[must_use]
    pub fn with_pseudonym_secret(mut self, secret: Option<&[u8]>) -> Self {
        self.pseudonyms =
            secret.and_then(|secret| crate::privacy::PseudonymKey::derive(secret, &self.scope));
        self
    }
    /// Key this cell's pseudonyms with `secret` in place; see
    /// [`Self::with_pseudonym_secret`].
    pub fn set_pseudonym_secret(&mut self, secret: Option<&[u8]>) {
        self.pseudonyms =
            secret.and_then(|secret| crate::privacy::PseudonymKey::derive(secret, &self.scope));
    }
    /// Whether this cell's pseudonyms are keyed.
    #[must_use]
    pub const fn has_pseudonym_key(&self) -> bool {
        self.pseudonyms.is_some()
    }
    pub(crate) const fn pseudonym_key(&self) -> Option<&crate::privacy::PseudonymKey> {
        self.pseudonyms.as_ref()
    }
    /// PRIVACY-2: the trusted policy's `norn.reveal` decision.
    pub(crate) fn reveals(&self, actor: &str, marking_id: &str) -> bool {
        self.authorizer.reveal(actor, marking_id)
    }
    /// The marking catalog of the active definitions `reader` sees.
    pub(crate) fn marking_catalog(
        &self,
        reader: &norn_cell::StoreReader<'_, E>,
    ) -> Result<crate::privacy::MarkingCatalog, Refusal> {
        let version = reader.active_definitions_version().map_err(Refusal::host)?;
        let mut cached = self.markings.lock().map_err(Refusal::host)?;
        if let Some((at, catalog)) = cached.as_ref()
            && *at == version
        {
            return Ok(std::sync::Arc::clone(catalog));
        }
        let (at, catalog) = crate::privacy::catalog_of(reader)?;
        *cached = Some((at, std::sync::Arc::clone(&catalog)));
        Ok(catalog)
    }
    /// Physical scope, never taken from client claims.
    #[must_use]
    pub const fn scope(&self) -> &CellScope {
        &self.scope
    }
    /// The cell's verified ledger head: what a host conformance run compares
    /// across runtimes. Never a request route; the trusted host asks it.
    /// # Errors
    /// The cell is unavailable or its ledger does not verify.
    pub fn verified_head(&self) -> Result<norn_cell::LedgerHead, Refusal> {
        self.cell()?.store().verified_head().map_err(Refusal::host)
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
        self.waiting
            .fetch_add(1, std::sync::atomic::Ordering::AcqRel);
        let guard = self.cell.lock();
        self.waiting
            .fetch_sub(1, std::sync::atomic::Ordering::AcqRel);
        let guard = guard.map_err(Refusal::host)?;
        if guard.is_none() {
            return Err(Refusal::host("the cell is unavailable"));
        }
        Ok(CellGuard(guard))
    }

    /// Capture source rows and map them through the cell's own mapping path,
    /// under the definition epoch, in OBS-PAGE-1 pages ([`page_count`]): rows with
    /// the same [`MappingInput`](norn_cell::mapping_bundle::MappingInput),
    /// split at [`PAGE_ROWS`] and before a repeated source record, are
    /// captured by `inject_mapping_page` and committed by
    /// `process_mapping_page`. The trusted host's sync calls this; no caller
    /// supplies facts, and a refused row stays in the inbox as quarantined
    /// evidence with its reasons. Rows already captured are captured once.
    /// Returns one outcome per row, in row order.
    ///
    /// INGEST-YIELD-1: the cell is locked one commit at a time (a page's
    /// capture, each run's commit, each per-record fallback commit), never for
    /// a whole page or batch, and each commit waits for reads in flight, so a
    /// read waits for at most one commit and sees one snapshot.
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
        self.ingest_within(rows, None).map(|report| report.outcomes)
    }

    /// [`Self::ingest`] within a work `budget` (INGEST-BATCH-1): every row
    /// captured and every commit the cell actually attempts (a page run, a
    /// per-record fallback, a page's own consumption) is charged. When the
    /// next commit would pass the budget the load stops, after at least one
    /// commit, at the first row of its page that no run it attempted holds
    /// (the page's start when it attempted none there), and reports only the
    /// rows before that point; [`IngestReport::next`] is where the caller
    /// resumes. Resuming with `rows[next..]` frames the rest in batch order,
    /// so runs this load already refused are not attempted again (their
    /// quarantined members stay pending as evidence) and every continuation
    /// makes progress (S3, #1488); captured runs are found again, runs that
    /// committed are consumed and commit nothing, and no row is lost or
    /// reported twice. A run whose own consumption commit does not fit
    /// resumes from its first member, and a row of it that landed before the
    /// stop answers `AlreadyLanded` in the resumed call.
    /// # Errors
    /// As [`Self::ingest`].
    pub fn ingest_within(
        &self,
        rows: &[(
            Observed<RuntimeValue>,
            norn_cell::mapping_bundle::MappingInput,
        )],
        budget: Option<WorkBudget>,
    ) -> Result<IngestReport, Refusal> {
        let _epoch = self.definition_read()?;
        // INGEST-GROUP-1: grouping changes the order rows commit in, so it
        // is used only for inputs proven independent, and never under a
        // budget, whose continuation point needs batch order.
        let independent = budget.is_none() && groupable(rows) && self.independent_inputs(rows)?;
        let framed = pages(rows, independent);
        let mut report = IngestReport {
            outcomes: Vec::with_capacity(rows.len()),
            next: None,
            pages: 0,
            commits: 0,
            work_ms: 0,
        };
        let mut out: Vec<(usize, Ingested)> = Vec::with_capacity(rows.len());
        let mut committed = false;
        let spent = |work: u64, more: u64| {
            budget.is_some_and(|budget| work.saturating_add(more) > budget.total_ms)
        };
        let commit_ms = budget.map_or(0, |budget| budget.commit_ms);
        for (input, page) in framed {
            let first = page.first().map_or(0, |(at, _)| *at);
            let row_ms = budget.map_or(0, |budget| {
                budget
                    .row_ms
                    .saturating_mul(u64::try_from(page.len()).unwrap_or(u64::MAX))
            });
            if committed && spent(report.work_ms, row_ms.saturating_add(commit_ms)) {
                report.next = Some(first);
                break;
            }
            let delivered: Vec<Observed<RuntimeValue>> =
                page.iter().map(|(_, raw)| (*raw).clone()).collect();
            let captured = self.capture_page(&delivered, input)?;
            report.work_ms = report.work_ms.saturating_add(row_ms);
            let records: Vec<String> = captured
                .iter()
                .flat_map(|part| part.records.iter().cloned())
                .collect();
            let position = |id: &str| records.iter().position(|record| record == id);
            // S3 (#1488): page rows before `settled` belong to runs this call
            // attempted (or are no run's), so a stop inside the page resumes
            // there, never at the page's start: a continuation does not
            // re-attempt the runs it already refused, whose quarantined
            // members stay pending as evidence.
            let mut settled = 0_usize;
            let mut stop: Option<usize> = None;
            let mut outcomes = std::collections::BTreeMap::new();
            'runs: for (page_id, _new) in captured.iter().flat_map(|part| &part.pages) {
                if committed && spent(report.work_ms, commit_ms) {
                    stop = Some(settled);
                    break;
                }
                let attempt = self.observing(|cell| cell.try_mapping_page(page_id))?;
                let members = match attempt {
                    norn_cell::mapping_page::PageAttempt::Consumed => continue,
                    norn_cell::mapping_page::PageAttempt::Landed(landed) => {
                        if let Some(at) = landed.iter().filter_map(|(id, _)| position(id)).max() {
                            settled = settled.max(at.saturating_add(1));
                        }
                        outcomes.extend(landed);
                        Vec::new()
                    }
                    norn_cell::mapping_page::PageAttempt::Refused(members) => members,
                };
                report.charge(commit_ms);
                committed = true;
                if members.is_empty() {
                    continue;
                }
                // The per-record path, one commit each, as one by one.
                let mut finishes = true;
                for member in &members {
                    if spent(report.work_ms, commit_ms) {
                        stop = Some(position(member).unwrap_or(settled));
                        break 'runs;
                    }
                    let outcome = self.observing(|cell| cell.process_mapping_outcome(member))?;
                    report.charge(commit_ms);
                    finishes &= !matches!(outcome, norn_cell::MappingOutcome::Quarantined(_));
                    if let Some(at) = position(member) {
                        settled = settled.max(at.saturating_add(1));
                    }
                    outcomes.insert(member.clone(), outcome);
                }
                // The page's own consumption commits only once no member is
                // pending; when it cannot fit, the continuation resumes this
                // pending page from its first member.
                if finishes && spent(report.work_ms, commit_ms) {
                    stop = Some(members.first().and_then(|id| position(id)).unwrap_or(settled));
                    break;
                }
                if self.observing(|cell| cell.finish_mapping_page(page_id))? {
                    report.charge(commit_ms);
                }
            }
            let reported = stop.unwrap_or(page.len()).min(page.len());
            // Each member this page consumed has its outcome; any other record
            // (refused by the mapping, a conflict or identity finding, or a row
            // consumed before) answers as it does one by one.
            for ((at, _), id) in page.iter().zip(records).take(reported) {
                let outcome = if let Some(outcome) = outcomes.remove(&id) {
                    outcome
                } else {
                    let outcome = self.observing(|cell| cell.process_mapping_outcome(&id))?;
                    if outcome == norn_cell::MappingOutcome::Landed {
                        report.charge(commit_ms);
                    }
                    outcome
                };
                out.push((
                    *at,
                    Ingested {
                        observation_id: id,
                        outcome,
                    },
                ));
            }
            if stop.is_some() {
                report.next = Some(page.get(reported).map_or(first, |(at, _)| *at));
                break;
            }
            report.pages = report.pages.saturating_add(1);
        }
        out.sort_by_key(|(at, _)| *at);
        report.outcomes = out.into_iter().map(|(_, ingested)| ingested).collect();
        Ok(report)
    }

    /// Capture one page, halving a page over the cell's byte bound; the parts
    /// in row order.
    fn capture_page(
        &self,
        raws: &[Observed<RuntimeValue>],
        input: &norn_cell::mapping_bundle::MappingInput,
    ) -> Result<Vec<norn_cell::mapping_page::CapturedPage>, Refusal> {
        match self.observing_raw(|cell| cell.inject_mapping_page(raws, input))? {
            Err(CellError::OutOfRange("mapping page bytes")) if raws.len() > 1 => {
                let (first, second) = raws.split_at(raws.len() / 2);
                let mut parts = self.capture_page(first, input)?;
                parts.extend(self.capture_page(second, input)?);
                Ok(parts)
            }
            other => Ok(vec![other.map_err(Refusal::host)?]),
        }
    }

    /// INGEST-GROUP-1: whether every two mapping inputs of the batch are
    /// independent ([`norn_cell::mapping_bundle::MappingFootprint`]); `false`
    /// when any footprint cannot be proven.
    fn independent_inputs(
        &self,
        rows: &[(
            Observed<RuntimeValue>,
            norn_cell::mapping_bundle::MappingInput,
        )],
    ) -> Result<bool, Refusal> {
        let mut inputs: Vec<&norn_cell::mapping_bundle::MappingInput> = Vec::new();
        for (_, input) in rows {
            if !inputs.contains(&input) {
                inputs.push(input);
            }
        }
        if inputs.len() < 2 {
            return Ok(true);
        }
        let footprints = self.observing_raw(|cell| {
            inputs
                .iter()
                .map(|input| cell.store().read().mapping_footprint(input))
                .collect::<Result<Vec<_>, _>>()
        })?;
        let Ok(footprints) = footprints else {
            return Ok(false);
        };
        Ok(footprints.iter().enumerate().all(|(at, footprint)| {
            footprints
                .iter()
                .skip(at.saturating_add(1))
                .all(|other| footprint.independent(other))
        }))
    }

    /// One step of a load under the observation profile, holding the cell
    /// for exactly that step (INGEST-YIELD-1): requests already waiting for
    /// the cell go first (for at most [`YIELD_BOUND`]), and the step waits
    /// for reads in flight.
    fn observing<T>(
        &self,
        step: impl FnOnce(
            &mut norn_cell::SqliteCell<ObservationEvent, RuntimeObject, RuntimeObject, E>,
        ) -> Result<T, CellError>,
    ) -> Result<T, Refusal> {
        self.observing_raw(step)?.map_err(Refusal::host)
    }

    fn observing_raw<T>(
        &self,
        step: impl FnOnce(
            &mut norn_cell::SqliteCell<ObservationEvent, RuntimeObject, RuntimeObject, E>,
        ) -> T,
    ) -> Result<T, Refusal> {
        self.let_waiting_requests_in();
        let _pinned = if READING.with(std::cell::Cell::get) > 0 {
            None
        } else {
            Some(self.reads.write().map_err(Refusal::host)?)
        };
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
        let result = step(&mut observing);
        *slot = Some(observing.into_profile());
        Ok(result)
    }

    /// Pin a read to one snapshot until the guard drops (INGEST-YIELD-1): a
    /// read takes the cell more than once (a page assembled in batches, its
    /// markings, its findings), and no load commit lands between those steps,
    /// so the read never refuses `object.snapshot_moved` because of a load.
    #[must_use]
    pub fn reading(&self) -> Reading<'_> {
        let pinned = if READING.with(std::cell::Cell::get) > 0 {
            // Already pinned by this thread's own read.
            None
        } else {
            self.reads.read().ok()
        };
        READING.with(|reading| reading.set(reading.get().saturating_add(1)));
        Reading { _pinned: pinned }
    }

    /// Yield the cell to requests already waiting for it (INGEST-YIELD-1),
    /// for at most [`YIELD_BOUND`], so a load never starves them and they
    /// never stall it.
    fn let_waiting_requests_in(&self) {
        let started = std::time::Instant::now();
        while self.waiting.load(std::sync::atomic::Ordering::Acquire) > 0
            && started.elapsed() < YIELD_BOUND
        {
            std::thread::yield_now();
        }
    }

    /// Install `added` over the active definitions as the next governed
    /// version: an artifact of the same id is replaced, the whole set is
    /// re-admitted on the cell's own base, and the model is the one the cell
    /// reopens under (HOST-REOPEN-1, checked by [`Self::install_definitions`]).
    /// The trusted host calls this (generated upload types, the cell's own
    /// packs); no route supplies artifacts to it.
    /// # Errors
    /// No active definitions, the checker's refusal (its findings), or the
    /// install's refusal.
    pub fn install_artifacts(
        &self,
        added: &[norn_ir::ArtifactEnvelope],
        installed_at: u64,
    ) -> Result<u64, String> {
        let active = self
            .active_definitions()
            .map_err(|r| r.message)?
            .ok_or("the cell has no definitions to extend")?;
        let admitted = crate::kernel_set::admit_over(
            &active.canonical_set,
            active.admission_receipt.as_deref(),
            added,
        )
        .map_err(|refusal| match refusal {
            crate::kernel_set::KernelRefusal::Refused(findings) => findings
                .iter()
                .map(|f| format!("{}: {}", f.code, f.message))
                .collect::<Vec<_>>()
                .join("; "),
            crate::kernel_set::KernelRefusal::Invalid(detail) => detail,
        })?;
        // The model this cell reopens under (DEFS-REOPEN-1), by its kind.
        let serving = serves_actions(self.cell().map_err(|r| r.explanation)?.store().model());
        let model = reopen_model(&admitted.canonical_set, serving).map_err(|r| r.explanation)?;
        let installed = self
            .install_definitions(
                model,
                InstalledDefinitionInput {
                    canonical_set: admitted.canonical_set,
                    registry: vec![],
                    receipt_sha256: admitted.receipt_sha256,
                    admission_receipt: Some(admitted.receipt),
                    readmission: None,
                    diff: serde_json::json!({}),
                    installed_at,
                    layout: None,
                },
                Some(active.version),
            )
            .map_err(|r| format!("{}: {}", r.code, r.message))?;
        Ok(installed.version)
    }

    /// Whether the cell's active model declares the object type `name`.
    /// # Errors
    /// Poisoned synchronization refuses closed.
    pub fn serves_object_type(&self, name: &str) -> Result<bool, Refusal> {
        let _epoch = self.definition_read()?;
        let cell = self.cell()?;
        Ok(cell
            .store()
            .model()
            .object_types()
            .any(|declared| declared.name.as_str() == name))
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

    /// The current revision of the retained definition `id`, if any, read
    /// under the definitions read epoch. The host's own authority checks
    /// (`/v1/access:decide` reads a binding's `runRole`) read it here; no
    /// network caller does.
    /// # Errors
    /// A store failure.
    pub fn retained_definition(
        &self,
        id: &str,
    ) -> Result<Option<norn_cell::retained::RetainedDefinitionRecord>, Refusal> {
        let _epoch = self.definition_read()?;
        let cell = self.cell()?;
        cell.store()
            .read()
            .retained_definition(id, None)
            .map_err(Refusal::host)
    }

    /// The active definitions version alone (no set is read), or `None` for
    /// a cell with nothing installed: cheap enough to check per request.
    /// # Errors
    /// Poisoned synchronization or a store failure refuses closed.
    pub fn active_definitions_version(&self) -> Result<Option<u64>, Refusal> {
        let _epoch = self.definition_read()?;
        let cell = self.cell()?;
        cell.store()
            .read()
            .active_definitions_version()
            .map_err(Refusal::host)
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
        self.install_definitions_checked(candidate_model, input, expected_version, |_| Ok(()))
    }

    /// [`Self::install_definitions`], with `check` run on the cell's store
    /// under the same exclusive epoch, before anything is written: what it
    /// reads cannot change before the install commits, and its refusal
    /// installs nothing.
    /// # Errors
    /// `check`'s refusal, or as [`Self::install_definitions`].
    pub fn install_definitions_checked(
        &self,
        candidate_model: Model,
        input: InstalledDefinitionInput,
        expected_version: Option<u64>,
        check: impl FnOnce(&CellStore<E>) -> Result<(), Refusal>,
    ) -> Result<InstalledDefinitionSet, Refusal> {
        self.install_definitions_with(candidate_model, input, expected_version, check, &[])
    }

    /// [`Self::install_definitions`], with every `riding` retained revision
    /// (a pack's approvals) appended in the same store transaction: the set
    /// and its approvals are published together or not at all (#1469).
    /// # Errors
    /// As [`Self::install_definitions`]; a retained revision that is not the
    /// next one of its id refuses with `defs.stale`, and installs nothing.
    pub fn install_definitions_riding(
        &self,
        candidate_model: Model,
        input: InstalledDefinitionInput,
        expected_version: Option<u64>,
        riding: &[norn_cell::retained::RetainedDefinitionChange],
    ) -> Result<InstalledDefinitionSet, Refusal> {
        self.install_definitions_with(candidate_model, input, expected_version, |_| Ok(()), riding)
    }

    fn install_definitions_with(
        &self,
        candidate_model: Model,
        input: InstalledDefinitionInput,
        expected_version: Option<u64>,
        check: impl FnOnce(&CellStore<E>) -> Result<(), Refusal>,
        riding: &[norn_cell::retained::RetainedDefinitionChange],
    ) -> Result<InstalledDefinitionSet, Refusal> {
        let _epoch = self.definition_write()?;
        let mut cell = self.cell()?;
        check(cell.store())?;
        check_reopens(
            &candidate_model,
            &input.canonical_set,
            serves_actions(cell.store().model()),
        )?;
        let installed = if riding.is_empty() {
            cell.install_definitions(candidate_model, input, expected_version)
        } else {
            cell.install_definitions_riding(candidate_model, &input, expected_version, riding)
        };
        let installed = installed
            .map_err(|error| {
                // HOST-INSTALL-LIVE-1: every install path refuses live data the
                // candidate's laws break with the same typed 409.
                if let Some(refusal) = crate::cell_definitions::live_law_refusal(&error) {
                    return refusal;
                }
                if let CellError::RetainedDefinitionConflict { id, expected, actual } = &error {
                    return Refusal::new(
                        StatusCode::CONFLICT,
                        "defs.stale",
                        "an approval changed since this revision was planned",
                        format!("{id} was planned at revision {expected:?}, the cell has {actual:?}; re-plan"),
                    );
                }
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
/// A page is one commit, and the cell is held for it (INGEST-YIELD-1), so a
/// read during a load waits for at most one page. A page commit costs about
/// 7 to 9 ms a row on a 4-vCPU host, flat in the page size (the `ERPNext`
/// mapping, 50 to 1,000 rows a page), so 100 rows keep that wait under a
/// second at no cost in rows per second.
pub const PAGE_ROWS: usize = 100;

/// How long a load yields the cell to waiting requests before a commit.
const YIELD_BOUND: std::time::Duration = std::time::Duration::from_millis(250);

/// How many OBS-PAGE-1 pages [`DevHost::ingest`] frames `rows` into in
/// batch order (each at least one commit; the cell splits a page into more
/// at overlapping keys or refused rows).
#[must_use]
pub fn page_count(
    rows: &[(
        Observed<RuntimeValue>,
        norn_cell::mapping_bundle::MappingInput,
    )],
) -> usize {
    pages(rows, false).len()
}

/// One OBS-PAGE-1 page: its mapping input and its rows, each with its index
/// in the batch.
type Page<'a> = (
    &'a norn_cell::mapping_bundle::MappingInput,
    Vec<(usize, &'a Observed<RuntimeValue>)>,
);

/// OBS-PAGE-1 framing: rows with the same mapping input, in batch order,
/// split at [`PAGE_ROWS`] and before a source record the page already
/// carries.
///
/// INGEST-GROUP-1: when the caller has proven the batch's inputs
/// `independent` (no canonical root both write, no link from one into the
/// other's writes or to a target both name), a batch that interleaves them is
/// grouped by mapping input first, keeping each input's rows in batch order,
/// so it is a few pages rather than one per run. Rows of independent inputs
/// commit the same way in either relative order. Grouping also needs that
/// no source record identity appears under two inputs ([`groupable`]).
/// Otherwise the batch is framed in its own order, so overlapping writes
/// apply in batch order and the last row wins, as one by one.
fn pages(
    rows: &[(
        Observed<RuntimeValue>,
        norn_cell::mapping_bundle::MappingInput,
    )],
    independent: bool,
) -> Vec<Page<'_>> {
    let mut order: Vec<usize> = (0..rows.len()).collect();
    if independent && groupable(rows) {
        let mut first: Vec<&norn_cell::mapping_bundle::MappingInput> = Vec::new();
        for (_, input) in rows {
            if !first.contains(&input) {
                first.push(input);
            }
        }
        order.sort_by_key(|&at| {
            rows.get(at)
                .and_then(|(_, input)| first.iter().position(|seen| *seen == input))
                .unwrap_or(usize::MAX)
        });
    }
    let mut pages: Vec<Page<'_>> = Vec::new();
    let mut seen = std::collections::BTreeSet::new();
    for at in order {
        let Some((raw, input)) = rows.get(at) else {
            continue;
        };
        let identity = raw.provenance.source_id.as_str();
        let split = pages.last().is_none_or(|(current, page)| {
            *current != input || page.len() >= PAGE_ROWS || !seen.insert(identity)
        });
        if split {
            seen = std::collections::BTreeSet::from([identity]);
            pages.push((input, Vec::new()));
        }
        if let Some((_, page)) = pages.last_mut() {
            page.push((at, raw));
        }
    }
    pages
}

/// Whether no source record identity appears under two mapping inputs of
/// the batch (INGEST-GROUP-1), so grouping by input keeps every record's
/// rows in batch order.
fn groupable(
    rows: &[(
        Observed<RuntimeValue>,
        norn_cell::mapping_bundle::MappingInput,
    )],
) -> bool {
    let mut inputs: std::collections::HashMap<&str, &norn_cell::mapping_bundle::MappingInput> =
        std::collections::HashMap::new();
    rows.iter().all(|(raw, input)| {
        *inputs
            .entry(raw.provenance.source_id.as_str())
            .or_insert(input)
            == input
    })
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
            mapping: "erpnext.rest.mapping".into(),
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
        let sizes: Vec<usize> = pages(&many, true)
            .iter()
            .map(|(_, page)| page.len())
            .collect();
        assert_eq!(sizes, vec![PAGE_ROWS; 1_200 / PAGE_ROWS]);

        let mixed = vec![
            (row("x"), items.clone()),
            (row("y"), items.clone()),
            (row("x"), items.clone()),
            (row("z"), bins),
            (row("z"), items),
        ];
        let framed: Vec<(String, Vec<String>)> = pages(&mixed, false)
            .into_iter()
            .map(|(input, page)| {
                (
                    input.dataset.clone(),
                    page.iter()
                        .map(|(_, raw)| raw.provenance.source_id.clone())
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

    /// INGEST-GROUP-1: a batch interleaving two datasets (a case, its
    /// events, the next case...) is two pages, one per input, each in batch
    /// order, not one page per run; it was 2 per case before. Each row keeps
    /// its batch index, so outcomes return in row order.
    #[test]
    fn an_interleaved_batch_is_one_page_per_input() {
        let (cases, events) = (input("case"), input("event"));
        let mut batch = Vec::new();
        for case in 0..30 {
            batch.push((row(&format!("c{case}")), cases.clone()));
            for event in 0..3 {
                batch.push((row(&format!("c{case}#{event}")), events.clone()));
            }
        }
        let framed = pages(&batch, true);
        assert_eq!(framed.len(), 2);
        // Inputs not proven independent keep the batch's own order.
        assert_eq!(page_count(&batch), 60);
        let (first, second) = (&framed[0], &framed[1]);
        assert_eq!(first.0.dataset, "case");
        assert_eq!(second.0.dataset, "event");
        assert_eq!(first.1.len(), 30);
        assert_eq!(second.1.len(), 90);
        // Batch order inside each input, and every row once.
        assert!(first.1.windows(2).all(|pair| pair[0].0 < pair[1].0));
        assert!(second.1.windows(2).all(|pair| pair[0].0 < pair[1].0));
        let mut indices: Vec<usize> = framed
            .iter()
            .flat_map(|(_, page)| page.iter().map(|(at, _)| *at))
            .collect();
        indices.sort_unstable();
        assert_eq!(indices, (0..batch.len()).collect::<Vec<_>>());
        // A record's later version still follows its earlier one.
        batch.push((row("c0"), cases));
        let framed = pages(&batch, true);
        assert_eq!(framed.len(), 3, "the repeat of c0 is the next page");
        assert_eq!(framed[1].1.len(), 1);
        assert_eq!(framed[1].0.dataset, "case");
    }

    /// S2: rows A, B, A of two inputs not proven independent (they may write
    /// the same canonical property) frame in batch order, so B never moves
    /// after the second A and the last row wins, as one by one.
    #[test]
    fn overlapping_inputs_keep_batch_order() {
        let (first, second) = (input("first"), input("second"));
        let batch = vec![
            (row("a1"), first.clone()),
            (row("b"), second),
            (row("a2"), first),
        ];
        let framed: Vec<(String, Vec<usize>)> = pages(&batch, false)
            .iter()
            .map(|(input, page)| {
                (
                    input.dataset.clone(),
                    page.iter().map(|(at, _)| *at).collect(),
                )
            })
            .collect();
        assert_eq!(
            framed,
            vec![
                ("first".to_owned(), vec![0]),
                ("second".to_owned(), vec![1]),
                ("first".to_owned(), vec![2]),
            ]
        );
    }
}
