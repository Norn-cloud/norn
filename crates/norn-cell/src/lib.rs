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

pub mod catalog;
pub mod classification;
mod codec;
pub mod compute;
pub mod engine;
pub mod error;
pub mod export;
mod export_journal;
pub mod flow_run;
mod history;
mod history_selection;
pub mod host;
pub mod mapping_bundle;
pub mod mapping_conflict;
mod mapping_identity;
mod mapping_links;
mod mapping_process;
mod migration;
pub mod mining;
mod observation_quality;
pub use observation_quality::AuthoritativePage;
mod identity;
pub mod model;
mod payload_storage;
pub mod quantity;
pub mod reactive;
mod reactive_read;
pub mod read;
mod receipt_commitment;
#[cfg(feature = "native")]
pub mod restore;
pub mod retained;
pub mod source_provenance;
pub mod store;
pub mod system;
pub mod transparency;
mod write;

pub use error::CellError;
pub use history::{FactSnapshot, FactVersionEvidence, HistoricalEvidence, KnowledgeCutoff};
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

/// Workers/Durable Objects SQLite adapter (celld and workerd).
#[cfg(target_arch = "wasm32")]
pub mod workers;
