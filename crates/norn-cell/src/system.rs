//! The cell store's system tables (T2-ST-2, #2541), in the admitted SQLite
//! subset (`norn_projections::sqlite_ddl::ADMITTED_SQLITE_SUBSET`).
//!
//! | table | row | written by |
//! | --- | --- | --- |
//! | `_norn_meta` | store format and the model's DDL hash | open |
//! | `_norn_ledger` | one event, hash-chained to the previous one | commit |
//! | `_norn_receipt` | one commit: ledger span and head, request hash, consumption, written versions, idempotency key | commit |
//! | `_norn_outbox` | one command id: its latest generation, its recorded result, and the commit that consumed it (unconsumed rows are the `perform` queue, in enqueue order) | commit; perform |
//! | `_norn_perform_claim` | one durable claim per command generation, committed before external dispatch | commit |
//! | `_norn_timer` | one pending timer and its incarnation (the scheduling commit); the `clock` queue, whose earliest `due` is the host alarm | commit |
//! | `_norn_run` | one `perform` execution and its outcome, and the commit that consumed it | perform; commit |
//! | `_norn_inbox` | one ingested observation (the `observe` queue) and the commit that consumed it | ingest; commit |
//! | `_norn_version` | the highest version ever assigned to an object id within its type hierarchy (keyed by root type), so versions never repeat across delete, recreate, or a change of subtype | commit |
//! | `_norn_link_version` | one link row `(link table, source, target)` ever written: its link id, its endpoint types as last written, and its version, bumped by every add or remove, so it never repeats across remove and re-add (#3013) | commit |
//! | `_norn_link_creation` | the original commit of a live link incarnation, preserved across property replacement; nullable legacy evidence is never reconstructed from ambiguous mutation receipts | commit |
//! | `_norn_definition_version` | each admitted canonical definition set, checker receipt digest, projected model/DDL identity, install time, and installing commit sequence | definition install |
//! | `_norn_definition_registry` | exact package@version selections for each retained definition version | definition install |
//! | `_norn_definition_admission_receipt` | canonical full checker receipts when available | definition install/readmission |
//! | `_norn_definition_active` | the current retained definition version | definition install |
//! | `_norn_definition_layout` | the editor-only layout snapshot and its independent revision for each retained definition version | definition install |
//!
//! Everything one commit writes is written in one SQLite transaction: the
//! consumed delivery, facts, events, receipt, outbox and timer changes are
//! atomic (ruling 2026-09-25 §5, "commit … in the same transaction as
//! enqueueing the resulting commands and timers").

/// Pre-alpha format /7 binds exact values and durable markings on top of
/// main's pending-link indexes (/4), quantity v2 and cell-wide clock (/6).
/// Stores /2 through /6 must be recreated, never upgraded in place.
pub const STORE_FORMAT: &str = "norn.cell.store/7";
/// Reverse index of pending relation intents, maintained with their records.
pub(crate) const PENDING_LINK_INDEX: &str = "_norn_pending_link";

/// The previous-hash of the first ledger entry.
pub const GENESIS_HASH: &str = "0000000000000000000000000000000000000000000000000000000000000000";

/// Immutable engine-materialized value proofs for exact fact versions.
pub(crate) const FACT_VALUES: &str = "_norn_fact_values";

/// Ledger table.
pub(crate) const LEDGER: &str = "_norn_ledger";
/// Receipt table.
pub(crate) const RECEIPT: &str = "_norn_receipt";
/// Outbox table.
pub(crate) const OUTBOX: &str = "_norn_outbox";
/// Durable perform-claim table.
pub(crate) const PERFORM_CLAIM: &str = "_norn_perform_claim";
/// Timer table.
pub(crate) const TIMER: &str = "_norn_timer";
/// Run table.
pub(crate) const RUN: &str = "_norn_run";
/// Inbox table.
pub(crate) const INBOX: &str = "_norn_inbox";
/// Version high-water table.
pub(crate) const VERSIONS: &str = "_norn_version";
/// Link version table (#3013).
pub(crate) const LINK_VERSIONS: &str = "_norn_link_version";
/// Recorded live-link creation evidence (#3174).
pub(crate) const LINK_CREATION: &str = "_norn_link_creation";
/// Meta table.
pub(crate) const META: &str = "_norn_meta";
/// Retained admitted definition versions.
pub(crate) const DEFINITION_VERSION: &str = "_norn_definition_version";
/// Exact package@version selections for each retained definition version.
pub(crate) const DEFINITION_REGISTRY: &str = "_norn_definition_registry";
/// Canonical full admission receipts, retained by definitions version.
pub(crate) const DEFINITION_ADMISSION_RECEIPT: &str = "_norn_definition_admission_receipt";
/// The active definition version pointer.
pub(crate) const DEFINITION_ACTIVE: &str = "_norn_definition_active";
/// Editor-only layout sidecars keyed by retained definition version.
pub(crate) const DEFINITION_LAYOUT: &str = "_norn_definition_layout";
/// Cumulative current effective markings for each live object property.
pub(crate) const MARKING_CURRENT: &str = "_norn_marking_current";
/// Reserved snapshot slot for fact occurrence/version lineage, independent of
/// property values. Admitted property identifiers are nonempty.
pub(crate) const MARKING_OCCURRENCE: &str = "";
/// Immutable effective-marking snapshots for each object property version.
pub(crate) const MARKING_VERSION: &str = "_norn_marking_version";
/// Completeness markers for current and immutable marking snapshots.
pub(crate) const MARKING_SNAPSHOT: &str = "_norn_marking_snapshot";

/// The system DDL, in application order.
pub const SYSTEM_DDL: &[&str] = &[
    r#"CREATE TABLE IF NOT EXISTS "_norn_clock" ("id" INTEGER NOT NULL PRIMARY KEY CHECK ("id"=1), "logical_time" INTEGER NOT NULL, "commit_seq" INTEGER NOT NULL, "receipt_hash" TEXT NOT NULL)"#,
    r#"INSERT OR IGNORE INTO "_norn_clock" ("id","logical_time","commit_seq","receipt_hash") VALUES (1,0,0,'')"#,
    r#"CREATE TABLE IF NOT EXISTS "_norn_quantity_ledger" (
      "world" TEXT NOT NULL, "ledger" TEXT NOT NULL, "version" INTEGER NOT NULL,
      "pack" TEXT NOT NULL, "unrestricted" TEXT NOT NULL,
      "defined_at" INTEGER NOT NULL,
      PRIMARY KEY ("world", "ledger")
    )"#,
    r#"CREATE TABLE IF NOT EXISTS "_norn_quantity_balance" (
      "world" TEXT NOT NULL, "ledger" TEXT NOT NULL, "account" TEXT NOT NULL,
      "unit" TEXT NOT NULL, "layer" TEXT NOT NULL, "posted" TEXT NOT NULL,
      "held" TEXT NOT NULL, "version" INTEGER NOT NULL,
      PRIMARY KEY ("world", "ledger", "account", "unit", "layer")
    )"#,
    r#"CREATE TABLE IF NOT EXISTS "_norn_quantity_history" (
      "world" TEXT NOT NULL, "ledger" TEXT NOT NULL, "account" TEXT NOT NULL,
      "unit" TEXT NOT NULL, "layer" TEXT NOT NULL, "posted" TEXT NOT NULL,
      "held" TEXT NOT NULL, "version" INTEGER NOT NULL, "commit_seq" INTEGER NOT NULL,
      PRIMARY KEY ("world", "ledger", "account", "unit", "layer", "version")
    )"#,
    r#"CREATE TABLE IF NOT EXISTS "_norn_quantity_reservation" (
      "world" TEXT NOT NULL, "ledger" TEXT NOT NULL, "id" TEXT NOT NULL,
      "record" TEXT NOT NULL, "version" INTEGER NOT NULL, "commit_seq" INTEGER NOT NULL,
      "created_at" INTEGER NOT NULL,
      PRIMARY KEY ("world", "ledger", "id")
    )"#,
    r#"CREATE TABLE IF NOT EXISTS "_norn_quantity_posting" (
      "world" TEXT NOT NULL, "ledger" TEXT NOT NULL, "id" TEXT NOT NULL,
      "record" TEXT NOT NULL, "reverses" TEXT, "version" INTEGER NOT NULL,
      "commit_seq" INTEGER NOT NULL, PRIMARY KEY ("world", "ledger", "id")
    )"#,
    r#"CREATE UNIQUE INDEX IF NOT EXISTS "_norn_quantity_posting/reversal"
      ON "_norn_quantity_posting" ("world", "ledger", "reverses")"#,
    r#"CREATE TABLE IF NOT EXISTS "_norn_meta" (
  "key" TEXT NOT NULL,
  "value" TEXT NOT NULL,
  PRIMARY KEY ("key")
)"#,
    r#"CREATE TABLE IF NOT EXISTS "_norn_pending_link" (
  "world" TEXT NOT NULL,
  "source_type" TEXT NOT NULL,
  "source_key" TEXT NOT NULL,
  "record_key" TEXT NOT NULL,
  "target_type" TEXT NOT NULL,
  "target_key" TEXT NOT NULL,
  PRIMARY KEY ("world", "source_type", "source_key", "record_key")
)"#,
    r#"CREATE INDEX IF NOT EXISTS "_norn_pending_link/target" ON "_norn_pending_link" ("world", "target_type", "target_key", "record_key")"#,
    r#"CREATE INDEX IF NOT EXISTS "_norn_pending_link/record" ON "_norn_pending_link" ("world", "record_key")"#,
    r#"CREATE TABLE IF NOT EXISTS "_norn_ledger" (
  "seq" INTEGER NOT NULL,
  "commit_seq" INTEGER NOT NULL,
  "world" TEXT NOT NULL,
  "event" TEXT NOT NULL,
  "prev_hash" TEXT NOT NULL,
  "hash" TEXT NOT NULL,
  PRIMARY KEY ("seq")
)"#,
    r#"CREATE INDEX IF NOT EXISTS "_norn_ledger/commit" ON "_norn_ledger" ("commit_seq")"#,
    r#"CREATE INDEX IF NOT EXISTS "_norn_ledger/quantity_latest" ON "_norn_ledger"
      ("world", json_extract("event", '$.request.ledgerId'), "seq" DESC)
      WHERE json_extract("event", '$.type')='quantity.transition'"#,
    r#"CREATE INDEX IF NOT EXISTS "_norn_ledger/quantity_reversal" ON "_norn_ledger"
      ("world", json_extract("event", '$.request.ledgerId'), json_extract("event", '$.request.operation.original_id'))
      WHERE json_extract("event", '$.type')='quantity.transition'
        AND json_extract("event", '$.request.operation.kind')='reverse'"#,
    r#"CREATE TABLE IF NOT EXISTS "_norn_receipt" (
  "commit_seq" INTEGER NOT NULL,
  "world" TEXT NOT NULL,
  "idempotency_key" TEXT,
  "request_hash" TEXT NOT NULL,
  "receipt" TEXT NOT NULL,
  "receipt_hash" TEXT NOT NULL,
  PRIMARY KEY ("commit_seq")
)"#,
    r#"CREATE INDEX IF NOT EXISTS "_norn_receipt/quantity_boundary" ON "_norn_receipt"
      (json_extract("receipt", '$.firstEventSeq'))
      WHERE json_extract("receipt", '$.quantity.version')='norn.quantity.commit/2'"#,
    r#"CREATE UNIQUE INDEX IF NOT EXISTS "_norn_receipt/idempotency" ON "_norn_receipt" ("world", "idempotency_key")"#,
    r#"CREATE TABLE IF NOT EXISTS "_norn_outbox" (
  "command_id" TEXT NOT NULL,
  "position" INTEGER NOT NULL,
  "generation" INTEGER NOT NULL,
  "commit_seq" INTEGER NOT NULL,
  "idempotency_key" TEXT NOT NULL,
  "deadline" INTEGER,
  "command" TEXT NOT NULL,
  "result" TEXT,
  "consumed_commit_seq" INTEGER,
  PRIMARY KEY ("command_id")
)"#,
    r#"CREATE UNIQUE INDEX IF NOT EXISTS "_norn_outbox/position" ON "_norn_outbox" ("position")"#,
    r#"CREATE TABLE IF NOT EXISTS "_norn_perform_claim" (
  "command_id" TEXT NOT NULL,
  "generation" INTEGER NOT NULL,
  "commit_seq" INTEGER NOT NULL,
  PRIMARY KEY ("command_id", "generation")
)"#,
    r#"CREATE TABLE IF NOT EXISTS "_norn_timer" (
  "timer_id" TEXT NOT NULL,
  "due" INTEGER NOT NULL,
  "timer" TEXT NOT NULL,
  "commit_seq" INTEGER NOT NULL,
  PRIMARY KEY ("timer_id")
)"#,
    r#"CREATE INDEX IF NOT EXISTS "_norn_timer/due" ON "_norn_timer" ("due", "timer_id")"#,
    r#"CREATE INDEX IF NOT EXISTS "_norn_timer/quantity_due" ON "_norn_timer" ("due", "timer_id")
      WHERE "timer_id">='norn.quantity.expiry/' AND "timer_id"<'norn.quantity.expiry0'"#,
    r#"CREATE INDEX IF NOT EXISTS "_norn_timer/ordinary_due" ON "_norn_timer" ("due", "timer_id")
      WHERE "timer_id"<'norn.quantity.expiry/' OR "timer_id">='norn.quantity.expiry0'"#,
    r#"CREATE TABLE IF NOT EXISTS "_norn_run" (
  "command_id" TEXT NOT NULL,
  "generation" INTEGER NOT NULL,
  "attempt" INTEGER NOT NULL,
  "outcome" TEXT NOT NULL,
  "consumed_commit_seq" INTEGER,
  PRIMARY KEY ("command_id", "generation", "attempt")
)"#,
    r#"CREATE TABLE IF NOT EXISTS "_norn_inbox" (
  "position" INTEGER NOT NULL,
  "observation_id" TEXT NOT NULL,
  "observed" TEXT NOT NULL,
  "consumed_commit_seq" INTEGER,
  PRIMARY KEY ("position")
)"#,
    r#"CREATE UNIQUE INDEX IF NOT EXISTS "_norn_inbox/id" ON "_norn_inbox" ("observation_id")"#,
    r#"CREATE TABLE IF NOT EXISTS "_norn_version" (
  "world" TEXT NOT NULL,
  "root_type" TEXT NOT NULL,
  "id" TEXT NOT NULL,
  "version" INTEGER NOT NULL,
  PRIMARY KEY ("world", "root_type", "id")
)"#,
    r#"CREATE TABLE IF NOT EXISTS "_norn_link_version" (
  "world" TEXT NOT NULL,
  "link_table" TEXT NOT NULL,
  "source" TEXT NOT NULL,
  "target" TEXT NOT NULL,
  "link" TEXT NOT NULL,
  "source_type" TEXT NOT NULL,
  "target_type" TEXT NOT NULL,
  "version" INTEGER NOT NULL,
  PRIMARY KEY ("world", "link_table", "source", "target")
)"#,
    r#"CREATE INDEX IF NOT EXISTS "_norn_link_version/source" ON "_norn_link_version" ("world", "source")"#,
    r#"CREATE INDEX IF NOT EXISTS "_norn_link_version/target" ON "_norn_link_version" ("world", "target")"#,
    r#"CREATE INDEX IF NOT EXISTS "_norn_link_version/source_order" ON "_norn_link_version" ("world", "source", "link_table", "target", "link", "source_type", "target_type")"#,
    r#"CREATE INDEX IF NOT EXISTS "_norn_link_version/target_order" ON "_norn_link_version" ("world", "target", "link_table", "source", "link", "source_type", "target_type")"#,
    r#"CREATE INDEX IF NOT EXISTS "_norn_link_version/source_link_order" ON "_norn_link_version" ("world", "source", "link", "link_table", "target", "source_type", "target_type")"#,
    r#"CREATE INDEX IF NOT EXISTS "_norn_link_version/target_link_order" ON "_norn_link_version" ("world", "target", "link", "link_table", "source", "source_type", "target_type")"#,
    r#"CREATE TABLE IF NOT EXISTS "_norn_link_creation" (
  "world" TEXT NOT NULL,
  "link_table" TEXT NOT NULL,
  "source" TEXT NOT NULL,
  "target" TEXT NOT NULL,
  "created_commit" INTEGER,
  "creation_version" INTEGER,
  PRIMARY KEY ("world", "link_table", "source", "target")
)"#,
    r#"CREATE TABLE IF NOT EXISTS "_norn_definition_version" (
  "version" INTEGER NOT NULL,
  "receipt_sha256" TEXT NOT NULL,
  "canonical_set" TEXT NOT NULL,
  "model_hash" TEXT NOT NULL,
  "ddl_hash" TEXT NOT NULL,
  "installed_at" INTEGER NOT NULL,
  "commit_seq" INTEGER NOT NULL,
  PRIMARY KEY ("version")
)"#,
    r#"CREATE UNIQUE INDEX IF NOT EXISTS "_norn_definition_version/commit" ON "_norn_definition_version" ("commit_seq")"#,
    r#"CREATE TABLE IF NOT EXISTS "_norn_definition_registry" (
  "version" INTEGER NOT NULL,
  "registry" TEXT NOT NULL,
  PRIMARY KEY ("version")
)"#,
    r#"CREATE TABLE IF NOT EXISTS "_norn_definition_admission_receipt" (
  "version" INTEGER NOT NULL,
  "receipt" TEXT NOT NULL,
  PRIMARY KEY ("version")
)"#,
    r#"CREATE TABLE IF NOT EXISTS "_norn_definition_active" (
  "id" INTEGER NOT NULL,
  "version" INTEGER NOT NULL,
  PRIMARY KEY ("id")
)"#,
    r#"CREATE TABLE IF NOT EXISTS "_norn_definition_layout" (
  "version" INTEGER NOT NULL,
  "layout_revision" INTEGER NOT NULL,
  "sidecar" TEXT NOT NULL,
  PRIMARY KEY ("version")
)"#,
    r#"CREATE TABLE IF NOT EXISTS "_norn_fact_values" (
  "world" TEXT NOT NULL,
  "root_type" TEXT NOT NULL,
  "object_type" TEXT NOT NULL,
  "id" TEXT NOT NULL,
  "version" INTEGER NOT NULL,
  "commit_seq" INTEGER NOT NULL,
  "snapshot" TEXT NOT NULL,
  "hash" TEXT NOT NULL,
  PRIMARY KEY ("world", "root_type", "id", "version")
)"#,
    r#"CREATE INDEX IF NOT EXISTS "_norn_fact_values/commit" ON "_norn_fact_values" ("world", "commit_seq", "root_type", "id", "version")"#,
    r#"CREATE TABLE IF NOT EXISTS "_norn_marking_current" (
  "world" TEXT NOT NULL,
  "object_type" TEXT NOT NULL,
  "id" TEXT NOT NULL,
  "property" TEXT NOT NULL,
  "version" INTEGER NOT NULL,
  "markings" TEXT NOT NULL,
  PRIMARY KEY ("world", "object_type", "id", "property")
)"#,
    r#"CREATE TABLE IF NOT EXISTS "_norn_marking_version" (
  "world" TEXT NOT NULL,
  "object_type" TEXT NOT NULL,
  "id" TEXT NOT NULL,
  "property" TEXT NOT NULL,
  "version" INTEGER NOT NULL,
  "markings" TEXT NOT NULL,
  PRIMARY KEY ("world", "object_type", "id", "property", "version")
)"#,
    r#"CREATE TABLE IF NOT EXISTS "_norn_marking_snapshot" (
  "world" TEXT NOT NULL,
  "object_type" TEXT NOT NULL,
  "id" TEXT NOT NULL,
  "version" INTEGER NOT NULL,
  "is_current" INTEGER NOT NULL,
  PRIMARY KEY ("world", "object_type", "id", "version", "is_current")
)"#,
    r#"CREATE INDEX IF NOT EXISTS "_norn_marking_current_nonempty" ON "_norn_marking_current" ("world", "object_type", "property") WHERE "markings" != '[]'"#,
];
