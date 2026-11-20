//! Governed fixture writes (#1485 S6): the kernel path, for fixtures outside
//! `norn-cell`'s own tests.
//!
//! The raw store commit runs no law check, so it exists only in `norn-cell`'s
//! own `#[cfg(test)]` build (`test_door`). A fixture anywhere else writes
//! through [`SqliteCell`], whose commit runs the shared kernel admission
//! (`Proposed` → `Checked`) on exactly the commit it stores. Only what that
//! path expresses is accepted: main-world object puts, creates and deletes and
//! link changes, with their ledger events, at one logical time. Anything else
//! a raw batch could carry is refused, never dropped.
#![allow(
    dead_code,
    clippy::wildcard_enum_match_arm,
    reason = "shared test fixture; each user takes what it needs"
)]

use norn_cell::engine::{DefaultEngine, SqlEngine};
use norn_cell::{
    CellError, CellStore, CommitBatch, CommitOutcome, FactWriteRow, LinkChange, MAIN_WORLD,
    SqliteCell,
};
use norn_ir::{FactChange, RuntimeValue};
use norn_kernel::cell_host::JsonObject;
use norn_kernel::{
    CellCommit, CellHost, FactKey, FactMutation, FactVersion, FactWrite, LinkKey, LinkMutation,
    LinkWrite, LogicalTime,
};

/// The kernel path over a store: events are the batch's own runtime values.
pub(crate) type Governed<S = DefaultEngine> = SqliteCell<RuntimeValue, JsonObject, JsonObject, S>;

/// What a raw batch carried that the kernel path does not express.
fn unsupported(what: &str) -> CellError {
    CellError::Engine(format!("a governed fixture commit carries no {what}"))
}

/// The kernel's writes for `rows` on the store `cell` holds: each put or
/// delete expects the object's current version.
pub(crate) fn fact_writes<E, C, I, S: SqlEngine>(
    cell: &SqliteCell<E, C, I, S>,
    rows: &[FactWriteRow],
) -> Result<Vec<FactWrite>, CellError> {
    let reader = cell.store().read();
    rows.iter()
        .map(|row| {
            let (object_type, key) = row.target();
            let expected = FactVersion(reader.version(MAIN_WORLD, object_type, key)?);
            let mutation = match row {
                FactWriteRow::Put { values, .. } => FactMutation::Put {
                    values: values.clone(),
                },
                FactWriteRow::Delete { .. } => FactMutation::Delete,
            };
            Ok(FactWrite {
                key: FactKey::new(object_type, key),
                expected,
                mutation,
            })
        })
        .collect()
}

/// The kernel's writes for canonical creates: each expects the object absent.
fn creates(facts: &[FactChange]) -> Result<Vec<FactWrite>, CellError> {
    facts
        .iter()
        .map(|fact| match fact {
            FactChange::Create {
                object_type,
                key,
                values,
            } => Ok(FactWrite {
                key: FactKey::new(object_type.as_str(), key.as_str()),
                expected: FactVersion::ABSENT,
                mutation: FactMutation::Put {
                    values: values.clone(),
                },
            }),
            _ => Err(unsupported("fact change but a create")),
        })
        .collect()
}

/// The kernel's link writes: each names its target's type and expects the
/// link's current version.
fn link_writes<E, C, I, S: SqlEngine>(
    cell: &SqliteCell<E, C, I, S>,
    links: &[LinkChange],
) -> Result<Vec<LinkWrite>, CellError> {
    let reader = cell.store().read();
    links
        .iter()
        .map(|change| {
            let (object_type, link, source, target, target_type, mutation) = match change {
                LinkChange::Add {
                    object_type,
                    link,
                    source,
                    target,
                    values,
                    target_type,
                } => (
                    object_type,
                    link,
                    source,
                    target,
                    target_type,
                    LinkMutation::Add {
                        values: values.clone(),
                    },
                ),
                LinkChange::Remove {
                    object_type,
                    link,
                    source,
                    target,
                    target_type,
                } => (
                    object_type,
                    link,
                    source,
                    target,
                    target_type,
                    LinkMutation::Remove,
                ),
            };
            let target_type = target_type
                .as_deref()
                .ok_or_else(|| unsupported("link without its target's type"))?;
            let expected = FactVersion(reader.link_version(
                MAIN_WORLD,
                object_type,
                link,
                source,
                target_type,
                target,
            )?);
            Ok(LinkWrite {
                key: LinkKey::new(
                    FactKey::new(object_type.as_str(), source.as_str()),
                    link.as_str(),
                    FactKey::new(target_type, target.as_str()),
                ),
                expected,
                mutation,
            })
        })
        .collect()
}

/// Commit what `batch` carries on `cell` through the kernel path, at the
/// batch's logical time.
///
/// # Errors
///
/// A field the kernel path does not express, or the kernel's refusal.
pub(crate) fn commit_on<S: SqlEngine>(
    cell: &mut Governed<S>,
    batch: &CommitBatch,
) -> Result<(), CellError> {
    if batch.world != MAIN_WORLD {
        return Err(unsupported("world but main"));
    }
    if batch.consumes.is_some() || batch.quantity.is_some() || batch.idempotency_key.is_some() {
        return Err(unsupported("delivery, quantity or idempotency key"));
    }
    if !batch.effects.is_empty() {
        return Err(unsupported("effects"));
    }
    let mut writes = fact_writes(cell, &batch.writes)?;
    writes.extend(creates(&batch.facts)?);
    let links = link_writes(cell, &batch.links)?;
    cell.advance_to(LogicalTime::from_millis(batch.logical_time));
    cell.commit(CellCommit {
        writes,
        links,
        events: batch.events.clone(),
        ..CellCommit::empty()
    })
    .map(|_receipt| ())
    .map_err(|refusal| CellError::Engine(format!("the kernel refused the fixture: {refusal:?}")))
}

/// `store.commit(batch)` through the kernel path, for a store of any engine:
/// it is served by a [`Governed`] cell bound to the batch's scope for this
/// one commit, and comes back with the receipt it recorded.
///
/// # Errors
///
/// As [`commit_on`], or a store of another scope.
pub(crate) fn commit_owned<S: SqlEngine>(
    store: CellStore<S>,
    batch: &CommitBatch,
) -> Result<(CellStore<S>, CommitOutcome), CellError> {
    let mut cell = Governed::new(store, batch.scope.clone(), |command| {
        command.payload.clone()
    })?;
    commit_on(&mut cell, batch)?;
    let store = cell.into_store();
    let receipt = store
        .read()
        .receipts()?
        .pop()
        .ok_or_else(|| CellError::Engine("a governed commit records a receipt".to_owned()))?;
    Ok((store, CommitOutcome::Committed(receipt)))
}

/// [`commit_owned`] in place, for a store of the default engine.
///
/// # Errors
///
/// As [`commit_owned`]; the store is then an empty one of the same model.
pub(crate) fn commit(
    store: &mut CellStore,
    batch: &CommitBatch,
) -> Result<CommitOutcome, CellError> {
    let placeholder = CellStore::open_in_memory(store.model().clone())?;
    let (owned, outcome) = commit_owned(std::mem::replace(store, placeholder), batch)?;
    *store = owned;
    Ok(outcome)
}

/// `store.commit(&batch)` for fixtures outside `norn-cell`: the same call
/// shape as the raw door, through the kernel path ([`commit`]).
pub(crate) trait GovernedCommit {
    /// [`commit`] on this store.
    ///
    /// # Errors
    ///
    /// As [`commit`].
    fn commit(&mut self, batch: &CommitBatch) -> Result<CommitOutcome, CellError>;
}

impl GovernedCommit for CellStore {
    fn commit(&mut self, batch: &CommitBatch) -> Result<CommitOutcome, CellError> {
        commit(self, batch)
    }
}
