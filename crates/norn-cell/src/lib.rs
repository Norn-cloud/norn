//! `norn-cell`: shared cell store with host-provided SQL engines.
//!
//! Ring 1 — store and host perimeter (see `AGENTS.md`). The store uses the
//! synchronous [`engine::SqlEngine`] boundary. The default `native` feature
//! selects the bundled SQLite adapter and enables native export and restore
//! support; a host without `native` supplies its own engine. Hosts own
//! durability guarantees.
//!
//! `CellStore<E: SqlEngine = DefaultEngine>` and
//! `SqliteCell<Event, C, I, S: SqlEngine = DefaultEngine>`
//! are generic over the same engine boundary. The default engine is available
//! with the `native` feature; each host's engine determines its durability.
//!
//! - [`Model`]: the object model a store is opened with, projected to
//!   normalized object/link tables by `norn-projections`
//!   (`sqlite_ddl`, T1-PROJ-2 #2529).
//! - [`CellStore`]: the normalized store (T2-ST-1 #2540, T2-ST-2 #2541),
//!   shared by native and host adapters:
//!   object and link tables, and the ledger, receipt, outbox, timer, and run
//!   system tables ([`system`]). One [`CellStore::commit`] writes the
//!   consumed delivery, facts, events, the receipt, enqueued commands, and
//!   timers in one SQLite transaction; reopening verifies the ledger hash
//!   chain ([`StoreReader::verify_ledger`]).
//! - [`reactive`]: reactive queries over declared read slices (T10-RT-1
//!   #2693): coalesced, commit-invalidated, fan-out-capped, profile-bounded.
//! - [`SqliteCell`]: the shared `CellHost` driver (T4-CELL-2 #2676) over
//!   that store: `commit` runs the kernel's `check_commit` inside the store's
//!   commit transaction, `perform` durably claims before dispatch and records
//!   each result before delivering it, and `observe`/`clock` peek until a
//!   commit consumes.

mod codec;
pub mod engine;
pub mod error;
pub mod export;
mod export_journal;
mod history;
pub mod host;
mod migration;
pub mod model;
pub mod quantity;
pub mod reactive;
mod reactive_read;
pub mod read;
#[cfg(feature = "native")]
pub mod restore;
pub mod store;
pub mod system;
mod write;

pub use error::CellError;
pub use host::{CommitRecord, PerformClaim, Responder, SqliteCell};
pub use model::{BindingIndexSpec, Model, ModelDefinitionSnapshot};
pub use read::{
    BindingSortValue, BindingSortedCandidate, BindingSortedPosition, FindingLedgerPage,
    LedgerEntry, LedgerHead, LinkRecord, ObjectRecord, PendingCommand, PendingObservation,
    PendingTimer, RunRecord, StoreReader,
};
#[cfg(feature = "native")]
pub use restore::{RestoreError, RestoreReport};
pub use store::{
    CellStore, CommitBatch, CommitOutcome, CommitPoint, Consumption, DefinitionInstallRecord,
    DefinitionReadmission, EffectRow, FactWriteRow, GuardedError, InstalledDefinitionInput,
    InstalledDefinitionSet, JournalMode, LinkChange, MAIN_WORLD, MarkingDeclassification,
    OutboxCommand, Receipt, ReceiptBody, StoreOptions, TimerEntry, WrittenRow,
};
