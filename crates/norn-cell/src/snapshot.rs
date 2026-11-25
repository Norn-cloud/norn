//! P5 (#1698): pinned, world-typed snapshot reads.
//!
//! [`CellStore::snapshot`] opens one transaction, pins it ([`SnapshotPin`]:
//! cell, typed [`WorldId`], revision, definitions), and hands the reader a
//! [`Snapshot`] typed by its world: [`Main`] or [`Fork`]. The world is
//! checked against the store's own fork pin, never a caller's string, so a
//! fork cannot open a `Main` snapshot and main cannot open a `Fork` one.
//! A cell with no portable identity, or whose installed definitions differ
//! from the model it was opened with, is refused rather than read partly.
//!
//! A snapshot has no public constructor and lives only inside the closure,
//! inside its transaction: every read through it sees the pinned revision.
//! Main-only operations exist only on `Snapshot<Main>`:
//!
//! ```compile_fail
//! # use norn_cell::snapshot::{Fork, Snapshot};
//! # use norn_cell::engine::NativeEngine;
//! fn fork_head(snapshot: &Snapshot<'_, Fork, NativeEngine>) {
//!     let _ = snapshot.head_command();
//! }
//! ```
//!
//! ```
//! # use norn_cell::snapshot::{Main, Snapshot};
//! # use norn_cell::engine::NativeEngine;
//! fn main_head(snapshot: &Snapshot<'_, Main, NativeEngine>) {
//!     let _ = snapshot.head_command();
//! }
//! ```

use std::marker::PhantomData;

use norn_kernel::read_envelope::DefinitionPin;
use norn_kernel::world::pin::{ForkId, LedgerAnchor, SnapshotPin, WorldId};

use crate::engine::SqlEngine;
use crate::error::CellError;
use crate::model::Model;
use crate::read::{LedgerHead, ObjectRecord, PendingCommand};
use crate::store::{CellStore, MAIN_WORLD};

/// World marker: the tenant's authoritative world. Uninhabited.
#[derive(Debug)]
pub enum Main {}

/// World marker: a scenario fork. Uninhabited.
#[derive(Debug)]
pub enum Fork {}

mod sealed {
    pub trait Sealed {}
    impl Sealed for super::Main {}
    impl Sealed for super::Fork {}
}

/// A world a [`Snapshot`] is typed by. Sealed: only [`Main`] and [`Fork`].
pub trait WorldKind: sealed::Sealed {
    /// Whether this is the main world.
    const MAIN: bool;
}

impl WorldKind for Main {
    const MAIN: bool = true;
}

impl WorldKind for Fork {
    const MAIN: bool = false;
}

/// One pinned read of a store, typed by its world.
#[derive(Debug)]
pub struct Snapshot<'tx, W: WorldKind, E: SqlEngine> {
    store: &'tx CellStore<E>,
    pin: SnapshotPin,
    head: LedgerHead,
    world: PhantomData<fn() -> W>,
}

impl<W: WorldKind, E: SqlEngine> Snapshot<'_, W, E> {
    /// What every read through this snapshot is pinned to.
    #[must_use]
    pub const fn pin(&self) -> &SnapshotPin {
        &self.pin
    }

    /// The verified ledger head read inside the snapshot's transaction; its
    /// commit count is the pin's revision.
    #[must_use]
    pub const fn head(&self) -> &LedgerHead {
        &self.head
    }

    /// The model at the pin's definitions: the snapshot checked, inside its
    /// transaction, that the installed definitions are this model's.
    #[must_use]
    pub const fn model(&self) -> &Model {
        self.store.model()
    }

    /// The object `id` of `object_type` (or a subtype), as of the pin.
    ///
    /// Rows keep the label of the timeline the store inherited, so a fork's
    /// rows are read under the main label; the pin, not the label, names
    /// the world (FORK-PIN-1).
    ///
    /// # Errors
    /// Unknown type; SQL engine or decode errors.
    pub fn object(&self, object_type: &str, id: &str) -> Result<Option<ObjectRecord>, CellError> {
        self.store.read().object(MAIN_WORLD, object_type, id)
    }
}

impl<E: SqlEngine> Snapshot<'_, Fork, E> {
    /// The parent ledger entry this fork was taken at: its history before
    /// this anchor is its parent's.
    #[must_use]
    pub fn horizon(&self) -> &LedgerAnchor {
        match &self.pin.world {
            WorldId::Fork(fork) => fork.base(),
            // A `Fork` snapshot is only constructed over a fork world.
            WorldId::Main => &FORK_HORIZON_UNSET,
        }
    }
}

/// Never returned: [`CellStore::snapshot`] builds a `Fork` snapshot only
/// over a fork world. It keeps [`Snapshot::horizon`] total without a panic.
static FORK_HORIZON_UNSET: LedgerAnchor = LedgerAnchor {
    seq: 0,
    entry_hash: String::new(),
};

impl<E: SqlEngine> Snapshot<'_, Main, E> {
    /// The FIFO outbox head, the next command to claim. Main only: a fork
    /// never claims or performs (WORLD-ISOLATE-1).
    ///
    /// # Errors
    /// SQL engine or decode errors.
    pub fn head_command(&self) -> Result<Option<PendingCommand>, CellError> {
        self.store.read().head_command()
    }

    /// The export record of `commit_seq`, as evidence. Main only: a fork's
    /// minted rows are never evidence (FORK-EVIDENCE-1).
    ///
    /// # Errors
    /// As [`CellStore::export_record`].
    pub fn export_record(
        &self,
        commit_seq: u64,
    ) -> Result<Option<crate::export::ExportRecord>, CellError> {
        self.store.export_record(commit_seq)
    }
}

impl<E: SqlEngine> CellStore<E> {
    /// The typed world this store serves, from its own fork pin: `Main`, or
    /// the fork's name bound to the parent ledger entry it was taken at.
    ///
    /// # Errors
    /// [`CellError::Encoding`] for a stored fork pin that names no valid
    /// fork world.
    pub fn world_id(&self) -> Result<WorldId, CellError> {
        let Some(pin) = self.fork_pin() else {
            return Ok(WorldId::Main);
        };
        LedgerAnchor::new(pin.base.seq, pin.base.hash.clone())
            .and_then(|base| ForkId::new(pin.world.clone(), base))
            .map(WorldId::Fork)
            .map_err(|error| CellError::Encoding(format!("fork pin: {error}")))
    }

    /// Run `read` against one pinned snapshot of world `W`, inside one
    /// transaction.
    ///
    /// # Errors
    /// [`CellError::SnapshotWorldMismatch`] when the store does not serve
    /// world `W`; [`CellError::Encoding`] for a cell with no portable
    /// identity; [`CellError::SchemaMismatch`] when the installed
    /// definitions are not this store's model; ledger verification, engine
    /// or `read`'s own errors otherwise.
    pub fn snapshot<W: WorldKind, T>(
        &self,
        read: impl FnOnce(&Snapshot<'_, W, E>) -> Result<T, CellError>,
    ) -> Result<T, CellError> {
        let world = self.world_id()?;
        if world.is_main() != W::MAIN {
            return Err(CellError::SnapshotWorldMismatch {
                expected: if W::MAIN { "main" } else { "a fork" },
                found: world.encode(),
            });
        }
        self.conn.transaction(|| {
            let cell = self.cell_id()?.ok_or_else(|| {
                CellError::Encoding("unqualified history cannot pin a snapshot".to_owned())
            })?;
            let head = self.verified_head()?;
            let installed = self.read().installed_definitions(None)?;
            if let Some(definitions) = &installed
                && definitions.model_hash != self.model().model_hash()
            {
                return Err(CellError::SchemaMismatch {
                    expected: self.model().model_hash().to_owned(),
                    found: definitions.model_hash.clone(),
                });
            }
            let definitions = DefinitionPin {
                version: installed
                    .as_ref()
                    .map_or(0, |definitions| definitions.version),
                model_hash: self.model().model_hash().to_owned(),
                admission_receipt: installed.map(|definitions| definitions.receipt_sha256),
            };
            let snapshot = Snapshot {
                store: self,
                pin: SnapshotPin {
                    cell,
                    world,
                    revision: head.commits,
                    definitions,
                },
                head,
                world: PhantomData,
            };
            read(&snapshot)
        })
    }
}

#[cfg(all(test, feature = "native"))]
#[allow(clippy::unwrap_used, reason = "snapshot fixture setup")]
mod tests {
    use std::collections::BTreeMap;

    use norn_ir::cell_id::CellId;

    use super::*;
    use crate::engine::NativeEngine;
    use crate::store::{CommitBatch, FactWriteRow};
    use crate::test_door::TestCommit as _;

    const SCOPE: &str = "org_demo/dev";
    const THING: &str = "law.Thing";

    fn model() -> Model {
        Model::from_definitions(&norn_testkit::cell_host::law_object_types(), &[]).unwrap()
    }

    fn store() -> CellStore<NativeEngine> {
        let cell = CellId::try_from("000000000000000000000000000000aa".to_owned()).unwrap();
        let mut store =
            CellStore::from_engine_bound(NativeEngine::open_in_memory().unwrap(), model(), &cell)
                .unwrap();
        store.bind("scope", SCOPE).unwrap();
        store
    }

    fn put(store: &mut CellStore<NativeEngine>, id: &str, time: u64) {
        store
            .commit(&CommitBatch {
                world: MAIN_WORLD.to_owned(),
                scope: SCOPE.to_owned(),
                logical_time: time,
                writes: vec![FactWriteRow::Put {
                    object_type: THING.to_owned(),
                    key: id.to_owned(),
                    values: BTreeMap::new(),
                }],
                ..CommitBatch::default()
            })
            .unwrap();
    }

    #[test]
    fn a_main_snapshot_pins_the_head_it_reads_and_a_fork_snapshot_is_refused_on_main() {
        let mut main = store();
        put(&mut main, "a", 1);
        let pin = main
            .snapshot::<Main, _>(|snapshot| {
                assert_eq!(snapshot.pin().revision, snapshot.head().commits);
                assert_eq!(
                    snapshot.pin().revision.checked_add(1),
                    Some(snapshot.store.read().next_commit_seq()?)
                );
                assert!(snapshot.head_command()?.is_none());
                assert!(snapshot.object(THING, "a")?.is_some());
                Ok(snapshot.pin().clone())
            })
            .unwrap();
        assert_eq!(pin.world, WorldId::Main);
        assert_eq!(pin.revision, 1);
        assert_eq!(pin.cell, main.cell_id().unwrap().unwrap());
        assert_eq!(pin.definitions.model_hash, main.model().model_hash());
        let refused = main.snapshot::<Fork, _>(|_| Ok(())).unwrap_err();
        assert!(
            matches!(refused, CellError::SnapshotWorldMismatch { expected: "a fork", ref found } if found == "main"),
            "{refused}"
        );
    }

    #[test]
    fn a_fork_snapshot_is_bound_to_its_base_and_never_moves_main() {
        let mut main = store();
        put(&mut main, "a", 1);
        let base = main.verified_head().unwrap();
        let mut fork = main.fork("what-if", &base).unwrap();
        put(&mut fork, "b", 2);
        let world = fork.world_id().unwrap();
        let WorldId::Fork(id) = &world else {
            panic!("a fork store serves a fork world, got {world}");
        };
        assert_eq!(id.name(), "what-if");
        assert_eq!(
            id.base(),
            &LedgerAnchor::new(base.seq, base.hash.clone()).unwrap()
        );
        let pin = fork
            .snapshot::<Fork, _>(|snapshot| {
                assert_eq!(snapshot.horizon(), id.base());
                assert!(snapshot.object(THING, "a")?.is_some());
                assert!(snapshot.object(THING, "b")?.is_some());
                Ok(snapshot.pin().clone())
            })
            .unwrap();
        assert_eq!(pin.world, world);
        assert_eq!(Some(pin.revision), base.commits.checked_add(1));
        assert!(matches!(
            fork.snapshot::<Main, _>(|_| Ok(())),
            Err(CellError::SnapshotWorldMismatch {
                expected: "main",
                ..
            })
        ));
        // Main is unchanged by the fork's commit.
        let after = main
            .snapshot::<Main, _>(|snapshot| {
                assert!(snapshot.object(THING, "b")?.is_none());
                Ok(snapshot.pin().revision)
            })
            .unwrap();
        assert_eq!(after, base.commits);
        assert_eq!(main.verified_head().unwrap(), base);
        // Reopening keeps the fork's typed world.
        assert_eq!(fork.world_id().unwrap(), world);
    }

    #[test]
    fn an_unqualified_cell_cannot_pin_a_snapshot() {
        let legacy = CellStore::open_in_memory(model()).unwrap();
        if legacy.cell_id().unwrap().is_none() {
            assert!(matches!(
                legacy.snapshot::<Main, _>(|_| Ok(())),
                Err(CellError::Encoding(_))
            ));
        } else {
            assert!(legacy.snapshot::<Main, _>(|_| Ok(())).is_ok());
        }
    }
}
