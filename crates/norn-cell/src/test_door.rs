//! Raw, unchecked store commits for tests only (#1375 S4, #1444 fix 5).
//!
//! `CellStore::commit_probed_unchecked` and `commit_guarded_unchecked` run no
//! law check, so they exist only in this crate's `#[cfg(test)]` build: an
//! authored write reaches storage only through the kernel path,
//! whose `Checked` stage (`norn_kernel::cell_host::Checked`) proves the
//! shared admission ran on that exact commit and mints the seal the store's
//! write door demands. The engine's own writes are the closed
//! `SystemWrite` set (`store/system_write.rs`).
//!
//! [`TestCommit`] re-exposes the raw commits to this crate's own tests. It
//! is not a Cargo feature, so no dependency can switch it on, and it is not
//! part of any build but this crate's `#[cfg(test)]` one (#1485 S6): no
//! debug, release or wasm32 build of `norn-cell` that another crate links
//! has it. The suites under `tests/` that use it are compiled inside the
//! crate (`crate::suites`); a fixture in another crate commits through the
//! kernel path (`tests/support/governed.rs`).

use crate::engine::SqlEngine;
use crate::error::CellError;
use crate::read::StoreReader;
use crate::store::{CellStore, CommitBatch, CommitOutcome, CommitPoint, GuardedError};

/// Raw, unchecked commits for test fixtures. See the [module](self).
pub(crate) trait TestCommit<E: SqlEngine> {
    /// Commit `batch` atomically, with no law check.
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
        CellStore::commit_probed_unchecked(self, batch, &mut |_point| Ok(()))
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
