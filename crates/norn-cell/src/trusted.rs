//! The trusted embedding door to the raw store commit (feature
//! `trusted-commit`).
//!
//! `CellStore::commit_unchecked`, `commit_probed_unchecked` and
//! `commit_guarded_unchecked` run no law
//! check, so they are private to this crate (#1375 S4, #1444 fix 5): an
//! authored write reaches storage only through the kernel path, whose
//! `Checked` stage (`norn_kernel::cell_host::Checked`) proves the shared
//! admission ran on that exact commit.
//!
//! [`TrustedCommit`] re-exposes them for the two callers that write without
//! an actor: a host's own provisioning (the trusted base's rows, a demo
//! seed) and tests. The skeleton guard (`trusted-commit-feature`) admits the
//! feature only as a dev-dependency or from the hosting crate; any other
//! crate that enables it fails CI.

use crate::engine::SqlEngine;
use crate::error::CellError;
use crate::read::StoreReader;
use crate::store::{CellStore, CommitBatch, CommitOutcome, CommitPoint, GuardedError};

/// Raw, unchecked commits for a trusted embedder. See the [module](self).
pub trait TrustedCommit<E: SqlEngine> {
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

impl<E: SqlEngine> TrustedCommit<E> for CellStore<E> {
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
