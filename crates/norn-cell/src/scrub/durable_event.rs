//! Resumable, row-budget-bounded durable fact-event proof verifier for
//! `Phase::History` (S10). Replaces unbounded 4,097-row emission scans and
//! unbounded packed export reads with keyset-paginated cursors under `RowBudget`.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::sync::Arc;

use norn_ir::RuntimeValue as Value;

use super::RowBudget;
use crate::engine::{SqlEngine, SqlRow, params};
use crate::history::{FactSnapshot, materialize_historical_values, unproven_event};
use crate::payload_storage::ExportDecodeCursor;
use crate::read::{StoreReader, to_i64, to_u64};
use crate::store::InstalledDefinitionSet;
use crate::system::FACT_VALUES;
use crate::{CellError, Model, Receipt};

const MAX_EMISSIONS: usize = 4096;
const EMISSION_PAGE_CAP: usize = 64;

#[derive(Clone)]
struct EmissionScan {
    world: String,
    commit_seq: u64,
    key: norn_kernel::FactKey,
    receipt: Receipt,
    after_version: u64,
    total_seen: usize,
    pages_done: bool,
    pending_versions: VecDeque<u64>,
}

impl EmissionScan {
    fn new(world: &str, commit_seq: u64, key: norn_kernel::FactKey, receipt: Receipt) -> Self {
        Self {
            world: world.to_owned(),
            commit_seq,
            key,
            receipt,
            after_version: 0,
            total_seen: 0,
            pages_done: false,
            pending_versions: VecDeque::new(),
        }
    }

    fn next_snapshot<E: SqlEngine>(
        &mut self,
        reader: &StoreReader<'_, E>,
        budget: &mut RowBudget,
    ) -> Result<EmissionStep, CellError> {
        if self.pending_versions.is_empty() && !self.pages_done {
            if budget.exhausted() {
                return Ok(EmissionStep::Yield);
            }
            let root = reader.root_type_or_self(self.key.object_type.as_str())?;
            let limit = budget.remaining().min(EMISSION_PAGE_CAP);
            let rows = reader.conn.query(
                &format!(
                    "SELECT \"version\" FROM \"{FACT_VALUES}\" WHERE \"world\" = ?1 AND \"commit_seq\" = ?2 AND \"root_type\" = ?3 AND \"id\" = ?4 AND \"version\" > ?5 ORDER BY \"version\" LIMIT ?6"
                ),
                params![
                    &self.world,
                    to_i64(self.commit_seq, "commit seq")?,
                    root,
                    &self.key.key,
                    to_i64(self.after_version, "fact version")?,
                    to_i64(u64::try_from(limit).unwrap_or(u64::MAX), "emission limit")?,
                ],
            )?;
            let count = rows.len();
            budget.charge(count);
            for raw_row in rows {
                let version = to_u64(SqlRow(raw_row).get(0)?, "fact version")?;
                self.total_seen = self.total_seen.saturating_add(1);
                if self.total_seen > MAX_EMISSIONS {
                    return Err(unproven_event());
                }
                self.after_version = version;
                self.pending_versions.push_back(version);
            }
            if count < limit {
                self.pages_done = true;
            }
        }
        let Some(version) = self.pending_versions.front().copied() else {
            return Ok(EmissionStep::Done);
        };
        if budget.remaining() < 2 {
            return Ok(EmissionStep::Yield);
        }
        let _ = self.pending_versions.pop_front();
        let snapshot = reader.proven_snapshot(
            &self.world,
            self.key.object_type.as_str(),
            &self.key.key,
            version,
        )?;
        budget.charge(2);
        if self.receipt.body.world != self.world
            || !self.receipt.body.written.iter().any(|row| {
                row.key == snapshot.key
                    && row.version == snapshot.version
                    && snapshot.lineage.contains(&row.object_type)
            })
        {
            return Err(unproven_event());
        }
        Ok(EmissionStep::Snapshot(snapshot))
    }
}

enum EmissionStep {
    Yield,
    Snapshot(FactSnapshot),
    Done,
}

#[derive(Clone)]
struct SchemaEvolutionState {
    object_type: norn_ir::ObjectTypeName,
    materialized: BTreeMap<String, Value>,
    previous_hash: String,
    after_model_hash: String,
    next_version: u64,
    last_version: u64,
    export_cursor: Option<ExportDecodeCursor>,
}

#[derive(Clone)]
struct ModifiedState {
    key: norn_kernel::FactKey,
    before_version: u64,
    after_version: u64,
    before: BTreeMap<String, Value>,
    after: BTreeMap<String, Value>,
    after_checked: bool,
    before_checked: bool,
    evolution: Option<Box<SchemaEvolutionState>>,
    scan: EmissionScan,
    matched_after: bool,
}

#[derive(Clone)]
struct CreatedState {
    values: BTreeMap<String, Value>,
    scan: EmissionScan,
    unique_match: Option<FactSnapshot>,
    pending_snapshot: Option<FactSnapshot>,
    export_cursor: Option<ExportDecodeCursor>,
    cached_writer_model: Option<(String, Arc<Model>)>,
    prior_checked: bool,
}

#[derive(Clone)]
struct SetState {
    property: norn_ir::PropertyId,
    value: Value,
    scan: EmissionScan,
    matched_count: usize,
}

#[derive(Clone)]
enum DurableKind {
    Modified(Box<ModifiedState>),
    Created(Box<CreatedState>),
    Set(Box<SetState>),
}

/// Resumable verifier for one durable fact event (`created`, `set`, `modified`).
#[derive(Clone)]
pub(super) struct DurableEventCursor {
    kind: DurableKind,
}

impl DurableEventCursor {
    /// Prepare a resumable cursor if `value` is a durable fact event; returns
    /// `Ok(None)` for ordinary non-fact events.
    pub(super) fn new(
        world: &str,
        commit_seq: u64,
        receipt: &Receipt,
        value: &Value,
    ) -> Result<Option<Self>, CellError> {
        let event = if value.get("op").and_then(Value::as_str) == Some("observed_fact") {
            value.get("event").ok_or_else(unproven_event)?
        } else {
            value
        };
        if !matches!(
            event.get("op").and_then(Value::as_str),
            Some("created" | "set" | "modified")
        ) {
            return Ok(None);
        }
        if [value, event]
            .into_iter()
            .any(|v| v.get("world").is_some_and(|v| v.as_str() != Some(world)))
        {
            return Err(unproven_event());
        }
        let mut canonical = event.clone();
        if let Some(object) = canonical.as_object_mut() {
            object.remove("causality");
            object.remove("world");
        }
        let decoded: norn_kernel::FactEvent =
            norn_ir::runtime::decode(&canonical).map_err(|_invalid| unproven_event())?;
        let kind = match decoded {
            norn_kernel::FactEvent::Modified {
                key,
                before_version,
                after_version,
                before,
                after,
                changed_properties,
            } => {
                let changed = before
                    .keys()
                    .chain(after.keys())
                    .collect::<BTreeSet<_>>()
                    .into_iter()
                    .filter(|property| before.get(*property) != after.get(*property))
                    .map(|property| norn_ir::PropertyId(property.clone()))
                    .collect::<Vec<_>>();
                if changed_properties != changed
                    || before_version.checked_next() != Some(after_version)
                {
                    return Err(unproven_event());
                }
                let scan = EmissionScan::new(world, commit_seq, key.clone(), receipt.clone());
                DurableKind::Modified(Box::new(ModifiedState {
                    key,
                    before_version: before_version.0,
                    after_version: after_version.0,
                    before,
                    after,
                    after_checked: false,
                    before_checked: false,
                    evolution: None,
                    scan,
                    matched_after: false,
                }))
            }
            norn_kernel::FactEvent::Created { key, values } => {
                let scan = EmissionScan::new(world, commit_seq, key, receipt.clone());
                DurableKind::Created(Box::new(CreatedState {
                    values,
                    scan,
                    unique_match: None,
                    pending_snapshot: None,
                    export_cursor: None,
                    cached_writer_model: None,
                    prior_checked: false,
                }))
            }
            norn_kernel::FactEvent::Set {
                key,
                property,
                value,
            } => {
                let scan = EmissionScan::new(world, commit_seq, key, receipt.clone());
                DurableKind::Set(Box::new(SetState {
                    property,
                    value,
                    scan,
                    matched_count: 0,
                }))
            }
            norn_kernel::FactEvent::CommandEnqueued { .. }
            | norn_kernel::FactEvent::NamingSeriesAllocated { .. }
            | norn_kernel::FactEvent::NamingSeriesCounterSeeded { .. }
            | norn_kernel::FactEvent::Deleted { .. }
            | norn_kernel::FactEvent::Transitioned { .. }
            | norn_kernel::FactEvent::InvariantRefused { .. } => return Err(unproven_event()),
        };
        Ok(Some(Self { kind }))
    }

    /// Advance durable event verification within `budget`; returns `Ok(true)`
    /// once proven, or `Ok(false)` to resume on the next wake.
    pub(super) fn step<E: SqlEngine>(
        &mut self,
        reader: &StoreReader<'_, E>,
        versions: &[InstalledDefinitionSet],
        budget: &mut RowBudget,
    ) -> Result<bool, CellError> {
        match &mut self.kind {
            DurableKind::Modified(state) => step_modified(reader, versions, state, budget),
            DurableKind::Created(state) => step_created(reader, versions, state, budget),
            DurableKind::Set(state) => step_set(reader, state, budget),
        }
    }
}

fn step_modified<E: SqlEngine>(
    reader: &StoreReader<'_, E>,
    versions: &[InstalledDefinitionSet],
    state: &mut ModifiedState,
    budget: &mut RowBudget,
) -> Result<bool, CellError> {
    if !state.after_checked {
        if budget.remaining() < 2 {
            return Ok(false);
        }
        reader.verify_fact_snapshot(
            &state.scan.world,
            state.key.object_type.as_str(),
            &state.key.key,
            state.after_version,
            &state.after,
        )?;
        budget.charge(2);
        state.after_checked = true;
    }
    if !state.before_checked && !step_modified_before(reader, versions, state, budget)? {
        return Ok(false);
    }
    loop {
        match state.scan.next_snapshot(reader, budget)? {
            EmissionStep::Yield => return Ok(false),
            EmissionStep::Snapshot(snapshot) => {
                if snapshot.version == state.after_version {
                    state.matched_after = true;
                }
            }
            EmissionStep::Done => {
                if !state.matched_after {
                    return Err(unproven_event());
                }
                return Ok(true);
            }
        }
    }
}

fn step_modified_before<E: SqlEngine>(
    reader: &StoreReader<'_, E>,
    versions: &[InstalledDefinitionSet],
    state: &mut ModifiedState,
    budget: &mut RowBudget,
) -> Result<bool, CellError> {
    if state.evolution.is_none() && !init_modified_evolution(reader, versions, state, budget)? {
        return Ok(false);
    }
    if state.before_checked {
        return Ok(true);
    }
    step_modified_evolution(reader, versions, state, budget)
}

fn init_modified_evolution<E: SqlEngine>(
    reader: &StoreReader<'_, E>,
    versions: &[InstalledDefinitionSet],
    state: &mut ModifiedState,
    budget: &mut RowBudget,
) -> Result<bool, CellError> {
    if state.before_version == 0 {
        if budget.remaining() < 2 {
            return Ok(false);
        }
        reader.verify_fact_snapshot(
            &state.scan.world,
            state.key.object_type.as_str(),
            &state.key.key,
            0,
            &state.before,
        )?;
        budget.charge(2);
        state.before_checked = true;
        return Ok(true);
    }
    if budget.remaining() < 6 {
        return Ok(false);
    }
    let before_snap = reader.proven_snapshot(
        &state.scan.world,
        state.key.object_type.as_str(),
        &state.key.key,
        state.before_version,
    )?;
    budget.charge(2);
    if before_snap.values.as_ref() == Some(&state.before) {
        state.before_checked = true;
        return Ok(true);
    }
    let after_snap = reader.proven_snapshot(
        &state.scan.world,
        state.key.object_type.as_str(),
        &state.key.key,
        state.after_version,
    )?;
    budget.charge(2);
    if before_snap.object_type != after_snap.object_type {
        return Err(unproven_event());
    }
    let first = reader
        .receipt_for_fact_version(
            &state.scan.world,
            state.key.object_type.as_str(),
            &state.key.key,
            state.before_version,
        )?
        .body
        .definitions_version;
    budget.charge(2);
    let last = state.scan.receipt.body.definitions_version;
    if first >= last
        || (first != 0
            && find_version(versions, first)
                .is_none_or(|installed| installed.model_hash != before_snap.model_hash))
    {
        return Err(unproven_event());
    }
    state.evolution = Some(Box::new(SchemaEvolutionState {
        object_type: norn_ir::ObjectTypeName(after_snap.object_type),
        materialized: before_snap.values.ok_or_else(unproven_event)?,
        previous_hash: before_snap.model_hash,
        after_model_hash: after_snap.model_hash,
        next_version: first.saturating_add(1),
        last_version: last,
        export_cursor: None,
    }));
    Ok(true)
}

fn step_modified_evolution<E: SqlEngine>(
    reader: &StoreReader<'_, E>,
    versions: &[InstalledDefinitionSet],
    state: &mut ModifiedState,
    budget: &mut RowBudget,
) -> Result<bool, CellError> {
    let Some(evo) = state.evolution.as_deref_mut() else {
        return Err(unproven_event());
    };
    while evo.next_version <= evo.last_version {
        let installed = find_version(versions, evo.next_version).ok_or_else(unproven_event)?;
        if evo.next_version == evo.last_version && installed.model_hash != evo.after_model_hash {
            return Err(unproven_event());
        }
        if installed.model_hash != evo.previous_hash {
            let historical;
            let model = if reader.model().model_hash() == installed.model_hash {
                reader.model()
            } else {
                let Some(record) = step_export_cursor(
                    reader.conn,
                    installed.commit_seq,
                    &mut evo.export_cursor,
                    budget,
                )?
                else {
                    return Ok(false);
                };
                let installation = record.definition_install.ok_or_else(unproven_event)?;
                historical = Model::from_retained_snapshot(&installation.model)?;
                if installation.installed != *installed
                    || historical.model_hash() != installed.model_hash
                    || record.model_hash != installed.model_hash
                {
                    return Err(unproven_event());
                }
                &historical
            };
            evo.materialized =
                materialize_historical_values(model, &evo.object_type, &evo.materialized)?;
            evo.previous_hash = installed.model_hash.clone();
        }
        evo.next_version = evo.next_version.saturating_add(1);
    }
    if evo.materialized != state.before {
        return Err(unproven_event());
    }
    state.evolution = None;
    state.before_checked = true;
    Ok(true)
}

fn step_created<E: SqlEngine>(
    reader: &StoreReader<'_, E>,
    versions: &[InstalledDefinitionSet],
    state: &mut CreatedState,
    budget: &mut RowBudget,
) -> Result<bool, CellError> {
    loop {
        if let Some(snapshot) = state.pending_snapshot.clone() {
            let Some(matches) = check_created_snapshot(reader, versions, state, &snapshot, budget)?
            else {
                return Ok(false);
            };
            state.pending_snapshot = None;
            if matches && state.unique_match.replace(snapshot).is_some() {
                return Err(unproven_event());
            }
            continue;
        }
        if state.prior_checked {
            return Ok(true);
        }
        if state.scan.pages_done && state.scan.pending_versions.is_empty() {
            let snapshot = state.unique_match.as_ref().ok_or_else(unproven_event)?;
            if snapshot.version > 1 {
                if budget.remaining() < 2 {
                    return Ok(false);
                }
                reader.verify_prior_absence(snapshot)?;
                budget.charge(2);
            }
            state.prior_checked = true;
            return Ok(true);
        }
        match state.scan.next_snapshot(reader, budget)? {
            EmissionStep::Yield => return Ok(false),
            EmissionStep::Snapshot(snapshot) => state.pending_snapshot = Some(snapshot),
            EmissionStep::Done => {}
        }
    }
}

fn check_created_snapshot<E: SqlEngine>(
    reader: &StoreReader<'_, E>,
    versions: &[InstalledDefinitionSet],
    state: &mut CreatedState,
    snapshot: &FactSnapshot,
    budget: &mut RowBudget,
) -> Result<Option<bool>, CellError> {
    let Some(stored) = &snapshot.values else {
        return Ok(Some(false));
    };
    if stored == &state.values {
        return Ok(Some(true));
    }
    let model = if snapshot.model_hash == reader.model().model_hash() {
        reader.model()
    } else {
        if state
            .cached_writer_model
            .as_ref()
            .is_none_or(|(hash, _)| hash != &snapshot.model_hash)
        {
            let def_ver = state.scan.receipt.body.definitions_version;
            let Some(installed) = find_version(versions, def_ver)
                .filter(|installed| installed.model_hash == snapshot.model_hash)
            else {
                return Ok(Some(false));
            };
            let Some(record) = step_export_cursor(
                reader.conn,
                installed.commit_seq,
                &mut state.export_cursor,
                budget,
            )?
            else {
                return Ok(None);
            };
            if budget.exhausted() {
                state.export_cursor = Some(ExportDecodeCursor::Ready(Some(Box::new(record))));
                return Ok(None);
            }
            let installation = record.definition_install.ok_or_else(unproven_event)?;
            let install_receipt = reader.receipt_at(installed.commit_seq)?;
            budget.charge(1);
            if installation.installed != *installed
                || install_receipt.as_ref() != Some(&record.receipt)
            {
                return Err(unproven_event());
            }
            let retained = Arc::new(StoreReader::<E>::retained_model_from(
                installed,
                &installation,
            )?);
            state.cached_writer_model = Some((snapshot.model_hash.clone(), retained));
        }
        match &state.cached_writer_model {
            Some((_, model)) => model.as_ref(),
            None => return Ok(Some(false)),
        }
    };
    Ok(Some(created_values_match(
        model,
        snapshot,
        stored,
        &state.values,
    )))
}

fn created_values_match(
    model: &Model,
    snapshot: &FactSnapshot,
    stored: &BTreeMap<String, Value>,
    supplied: &BTreeMap<String, Value>,
) -> bool {
    let definitions = model.object_types().cloned().collect::<Vec<_>>();
    let catalog = norn_kernel::TypeCatalog::new(&definitions)
        .with_interfaces(model.interfaces())
        .with_property_types(model.property_types());
    let Ok(effective) = catalog.effective_object_values(
        &norn_ir::ObjectTypeName(snapshot.object_type.clone()),
        supplied,
    ) else {
        return false;
    };
    if stored.len() != effective.len() {
        return false;
    }
    effective.iter().all(|(property, value)| {
        stored.get(property).is_some_and(|actual| {
            actual == value
                || value.as_str().is_some_and(|state| {
                    model
                        .lifecycle(&snapshot.object_type, property)
                        .is_ok_and(|declaration| {
                            declaration.is_some_and(|declaration| {
                                declaration.initial.iter().any(|initial| initial == state)
                            })
                        })
                })
        })
    })
}

fn step_set<E: SqlEngine>(
    reader: &StoreReader<'_, E>,
    state: &mut SetState,
    budget: &mut RowBudget,
) -> Result<bool, CellError> {
    loop {
        match state.scan.next_snapshot(reader, budget)? {
            EmissionStep::Yield => return Ok(false),
            EmissionStep::Snapshot(snapshot) => {
                if snapshot
                    .values
                    .as_ref()
                    .and_then(|values| values.get(state.property.as_str()))
                    == Some(&state.value)
                {
                    state.matched_count = state.matched_count.saturating_add(1);
                    if state.matched_count > 1 {
                        return Err(unproven_event());
                    }
                }
            }
            EmissionStep::Done => {
                if state.matched_count != 1 {
                    return Err(unproven_event());
                }
                return Ok(true);
            }
        }
    }
}

fn find_version(
    versions: &[InstalledDefinitionSet],
    version: u64,
) -> Option<&InstalledDefinitionSet> {
    versions.iter().find(|item| item.version == version)
}

fn step_export_cursor(
    engine: &impl SqlEngine,
    commit_seq: u64,
    slot: &mut Option<ExportDecodeCursor>,
    budget: &mut RowBudget,
) -> Result<Option<crate::export::ExportRecord>, CellError> {
    if slot.is_none() {
        if budget.exhausted() {
            return Ok(None);
        }
        *slot =
            Some(ExportDecodeCursor::open(engine, commit_seq, budget)?.ok_or_else(unproven_event)?);
    }
    let Some(cursor) = slot.as_mut() else {
        return Err(unproven_event());
    };
    let Some(record) = cursor.step(engine, budget)? else {
        return Ok(None);
    };
    *slot = None;
    Ok(Some(record))
}
