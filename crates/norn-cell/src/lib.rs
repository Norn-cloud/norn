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
//!   system tables ([`system`]). One commit writes the
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

pub mod admitted;
pub mod analytics;
pub mod automation_outbox;
pub mod catalog;
pub mod cell_identity;
pub mod classification;
mod codec;
pub mod compute;
pub mod coverage_observer;
pub mod dataset_laws;
pub mod dataset_manifest;
pub mod decision_ledger;
mod definition_scope;
pub mod due_retry;
pub mod engine;
pub mod engine_release;
pub mod error;
pub mod export;
mod export_journal;
mod export_journal_residue;
pub mod flow_run;
pub mod fork;
pub mod fork_evidence;
pub mod fork_merge;
pub mod fork_scenario;
pub mod function_receipt;
mod history;
mod history_selection;
pub mod host;
mod link_guard;
mod link_laws;
pub mod mapping_bundle;
pub mod mapping_conflict;
mod mapping_identity;
mod mapping_links;
pub use mapping_links::LINK_HELD_RULE;
#[doc(hidden)]
pub use mapping_links::check_link_plans_against_wide_scope;
pub mod mapping_page;
mod mapping_process;
mod migration;
pub mod mining;
mod observation_quality;
mod open_checkpoint;
pub use open_checkpoint::CheckpointKey;
mod outbox_generations;
pub mod outbox_ledger;
mod outbox_ledger_migrate;
pub mod provision;
pub use observation_quality::AuthoritativePage;
mod identity;
pub mod model;
mod payload_storage;
mod predicate_scan;
pub mod quantity;
pub mod reactive;
mod reactive_read;
pub mod read;
mod receipt_commitment;
#[cfg(feature = "native")]
pub mod restore;
pub mod retained;
mod scrub;
pub use scrub::ScrubStatus;
pub mod snapshot;
pub mod source_agreement;
pub mod source_cursor;
pub mod source_provenance;
pub mod store;
pub mod system;
// #1485 S6: the raw commits exist only in this crate's own tests, and the
// suites under `tests/` that need them are compiled here (`suites`), naming
// the crate `norn_cell` as an integration test does.
#[cfg(test)]
extern crate self as norn_cell;
#[cfg(test)]
#[allow(
    dead_code,
    unreachable_pub,
    clippy::duplicate_mod,
    reason = "integration suites compiled in the crate keep their own visibility and \
              each loads its shared helpers, as its own test binary did"
)]
mod suites;
#[cfg(test)]
mod test_door;
pub mod transparency;
mod write;

pub use admitted::{AdmittedAction, AdmittedDefinitions};
pub use coverage_observer::{
    CedarRuleKey, DomainCoverageRecorder, DomainCoverageSnapshot, ImmutableRefusalKey,
    ObservedSqliteCell, TransitionKey,
};
pub use definition_scope::{
    PRINCIPAL_INPUTS_PROPERTY, function_command_record_type, is_reserved_kernel_name,
    kernel_record_types, kernel_record_types_recording_principal_inputs,
};
pub use error::CellError;

pub use fork::{ForkEngine, ForkPin, ForkState, StoreImage};
pub use fork_evidence::{
    ComparisonBaseline, ForkCommitEvidence, SimulatedDecisionCardEvidence,
    verify_fork_commit_evidence, verify_main_export_record, verify_main_receipt,
};
pub use fork_merge::{
    ForkActionCall, ForkMergeRefusalReason, ForkPromotionReport, PromotedCommand,
    execute_action_in_fork, promote_decision_fork,
};
pub use fork_scenario::{ForkScenario, RecordedEntropy, WorldContext, fixture_key};
pub use function_receipt::ResultAuthority;
pub use history::{FactSnapshot, FactVersionEvidence, HistoricalEvidence, KnowledgeCutoff};
pub use host::{ClaimOutcome, CommitRecord, Responder, Settlement, SqliteCell};
pub use mapping_process::{MappingOutcome, QuarantineReason};
pub use model::{BindingIndexSpec, Model, ModelDefinitionSnapshot};
pub use read::{
    BindingSortValue, BindingSortedCandidate, BindingSortedPosition, FindingLedgerPage,
    LedgerEntry, LedgerHead, LinkRecord, MarkedQuestionPath, ObjectRecord, ParkedPage,
    PendingCommand, PendingObservation, PendingTimer, RunRecord, StoreReader,
};
#[cfg(feature = "native")]
pub use restore::{HistoryDiagnosis, PrefixRestore, RestoreError, RestoreReport, UnprovenEntry};
pub use store::{
    CellStore, CommitBatch, CommitOutcome, CommitPoint, Consumption, DefinitionInstallRecord,
    DefinitionReadmission, EffectRow, FactWriteRow, GuardedError, InstalledDefinitionInput,
    InstalledDefinitionSet, JournalMode, LinkChange, MAIN_WORLD, MarkingDeclassification,
    MarkingReveal, OutboxCommand, Receipt, ReceiptBody, StoreOptions, TimerEntry, WrittenRow,
};

/// Whether `function_id@version` is a compiled-in built-in
/// (`norn_builtins::resolve`); a host registers it so a flow step may
/// perform only built-ins (#1252).
#[must_use]
pub fn builtin_registered(function_id: &str, version: u32) -> bool {
    matches!(norn_builtins::resolve(function_id, version), Ok(Some(_)))
}

/// Workers/Durable Objects SQLite adapter (workerd).
#[cfg(target_arch = "wasm32")]
pub mod workers;

mod read_envelope;
