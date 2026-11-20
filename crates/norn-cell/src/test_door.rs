//! Raw, unchecked store commits for tests only (#1375 S4, #1444 fix 5).
//!
//! `CellStore::commit_unchecked`, `commit_probed_unchecked` and
//! `commit_guarded_unchecked` run no law check, so they are private to this
//! crate: an authored write reaches storage only through the kernel path,
//! whose `Checked` stage (`norn_kernel::cell_host::Checked`) proves the
//! shared admission ran on that exact commit. A host's own actorless writes
//! are the fixed-input operations in [`crate::provision`].
//!
//! [`TestCommit`] re-exposes the raw commits to test fixtures. It is not a
//! Cargo feature, so no dependency can switch it on: the module exists only
//! in debug builds for a native target
//! (`cfg(all(debug_assertions, not(target_arch = "wasm32")))`), and every
//! shipped build (release, or the Worker's wasm32) is compiled without it.
//! Production code must never import it.

use crate::engine::SqlEngine;
use crate::error::CellError;
use crate::read::StoreReader;
use crate::store::{CellStore, CommitBatch, CommitOutcome, CommitPoint, GuardedError};

/// Raw, unchecked commits for test fixtures. See the [module](self).
pub trait TestCommit<E: SqlEngine> {
    /// `CellStore::commit_unchecked`: commit `batch` atomically, with no law check.
    ///
    /// # Errors
    ///
    /// Any [`CellError`]; on error nothing was written.
    fn commit(&mut self, batch: &CommitBatch) -> Result<CommitOutcome, CellError>;

    /// `CellStore::commit_probed_unchecked`: [`Self::commit`] with a fault probe at
    /// every [`CommitPoint`].
    ///
    /// # Errors
    ///
    /// Any [`CellError`], including the probe's.
    fn commit_probed(
        &mut self,
        batch: &CommitBatch,
        probe: &mut dyn FnMut(CommitPoint) -> Result<(), CellError>,
    ) -> Result<CommitOutcome, CellError>;

    /// `CellStore::commit_guarded_unchecked`: commit `batch` after `guard` admits it,
    /// inside the same transaction.
    ///
    /// # Errors
    ///
    /// [`GuardedError::Guard`] when the guard refuses; otherwise as
    /// [`Self::commit_probed`].
    fn commit_guarded<T: Default, X>(
        &mut self,
        batch: &CommitBatch,
        guard: impl FnOnce(&StoreReader<'_, E>) -> Result<T, X>,
        probe: &mut dyn FnMut(CommitPoint) -> Result<(), CellError>,
    ) -> Result<(T, CommitOutcome), GuardedError<X>>;
}

impl<E: SqlEngine> TestCommit<E> for CellStore<E> {
    fn commit(&mut self, batch: &CommitBatch) -> Result<CommitOutcome, CellError> {
        CellStore::commit_unchecked(self, batch)
    }

    fn commit_probed(
        &mut self,
        batch: &CommitBatch,
        probe: &mut dyn FnMut(CommitPoint) -> Result<(), CellError>,
    ) -> Result<CommitOutcome, CellError> {
        CellStore::commit_probed_unchecked(self, batch, probe)
    }

    fn commit_guarded<T: Default, X>(
        &mut self,
        batch: &CommitBatch,
        guard: impl FnOnce(&StoreReader<'_, E>) -> Result<T, X>,
        probe: &mut dyn FnMut(CommitPoint) -> Result<(), CellError>,
    ) -> Result<(T, CommitOutcome), GuardedError<X>> {
        CellStore::commit_guarded_unchecked(self, batch, guard, probe)
    }
}
