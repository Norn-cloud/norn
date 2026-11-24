//! AUTOMATION-OUTBOX-1 (Core 04): transactional automation trigger outbox,
//! lossless overflow continuation, and indexed trigger projection.
//!
//! Matching automation trigger candidates are recorded inside the same `SQLite`
//! transaction as the triggering write (direct action calls, bound flow calls,
//! approved proposals, or raw fact writes). Active, enabled, authority-bound
//! write and action triggers are projected transactionally into
//! `_norn_retained_automation_trigger` inside `SqlEngine::definition_writes`.
//! The commit path probes only touched root hierarchies and committed
//! `norn.ActionCall` ids via indexed `LIMIT 1` lookups, skips snapshot reads
//! and JSON serialization when no rule matches, pages rules with keyset
//! cursors (`LIMIT` [`RULE_PAGE_LIMIT`]) so no unbounded rule list is ever
//! materialized in a commit, records up to [`MAX_CANDIDATES_PER_COMMIT`]
//! candidates inline, and persists one idempotent continuation row in
//! `_norn_automation_outbox_continuation` in the same transaction when
//! matching crosses the inline bound. A business write is never refused or
//! rolled back because of hidden automation rules.
//!
//! ## Snapshot semantics (filtered per rule against `rule.object_type`)
//! - **Created / sibling replacement** (`event:created`): `before_values = None`,
//!   `after_values = Some(after)`. Replacing subtype A with sibling B in one
//!   commit fires `event:created` with `before_values = None` for rules on B
//!   and `event:updated` for rules on their common ancestor.
//! - **Updated / lifecycle transition** (`event:updated`, `lifecycle_entered`):
//!   `before_values = Some(before)`, `after_values = Some(after)`.
//! - **Action triggers** (`event:action`): driven from committed `norn.ActionCall`
//!   records in the same transaction: deleted target -> `Some(before)` / `None`;
//!   unchanged target -> `Some(before)` / `Some(after)` (`before == after`);
//!   absent receipt-only target -> `None` / `None`.
//!
//! ## Retirement & U2 obligations (`Core 04` / `Core 05 AUTH-ATTENUATE-1`)
//! - **Retirement & live authority:** retiring an automation deletes its
//!   trigger-index row and marks its pending outbox candidates void
//!   (`voided_commit_seq = commit_seq`) in the same transaction. No trigger
//!   actor grant is stored on candidates; U2 must require the candidate's
//!   revision to remain the current active revision and reauthorize the
//!   author's live grant (`parent_grant_ids` + `author`) and concrete target
//!   row under the dispatch hold.
//! - **Lossless continuation expansion:** select each rule's latest retained
//!   revision at or below the pinned definitions version, then filter to
//!   active, enabled, bound rules. Resume from receipt writes, retained action
//!   calls, and historical versions (including tombstones) with the recorded
//!   matcher version and cursor. Insert candidates idempotently before
//!   draining and apply the dispatch checks above. During a #1551 scrub,
//!   retry gated as-of reads without advancing the cursor. Retain the required
//!   history until expansion completes.

mod continuation;
mod trigger_index;

use std::collections::{BTreeMap, BTreeSet};

use norn_ir::{FactChange, RuntimeValue as Value};
use serde::{Deserialize, Serialize};

use crate::Model;
use crate::codec::canonical_json;
use crate::engine::{SqlEngine, params};
use crate::error::CellError;
use crate::read::{ObjectRecord, StoreReader, to_i64};
use crate::store::{CommitBatch, FactWriteRow, WrittenRow};

pub use continuation::{
    AUTOMATION_MATCHER_VERSION, AutomationContinuationCursor, AutomationOutboxContinuation,
    current_engine_version,
};
use trigger_index::{
    Page1Cache, RulePage, TriggerMatcher, WatchedRule, has_action_rules, has_row_rules_for_root,
    query_action_rules_page, query_row_rules_page,
};
pub(crate) use trigger_index::{sync_trigger_index, void_retired};

/// The transactional trigger outbox table.
pub(crate) const AUTOMATION_OUTBOX: &str = "_norn_automation_outbox";

/// Maximum automation trigger candidates recorded inline inside one triggering
/// commit. Additional matches are never dropped and never refuse the commit:
/// one idempotent continuation row is written in the same transaction for U2
/// to expand from committed history.
pub const MAX_CANDIDATES_PER_COMMIT: usize = 256;

/// Maximum indexed automation rules fetched per keyset-paged `SQLite` query
/// during a commit.
pub const RULE_PAGE_LIMIT: usize = 64;

/// Maximum `(written object × rule)` evaluations performed inline during one
/// commit before yielding to a transactional continuation row.
pub const MAX_RULE_SCANS_PER_COMMIT: usize = 4_096;

/// The outbox, continuation, and trigger-projection DDL, applied with the
/// other system tables (purely additive).
pub(crate) const DDL: [&str; 9] = [
    r#"CREATE TABLE IF NOT EXISTS "_norn_automation_outbox" (
  "world" TEXT NOT NULL,
  "automation_id" TEXT NOT NULL,
  "revision" INTEGER NOT NULL,
  "trigger_commit_seq" INTEGER NOT NULL,
  "object_type" TEXT NOT NULL,
  "object_id" TEXT NOT NULL,
  "object_version" INTEGER NOT NULL,
  "trigger_kind" TEXT NOT NULL,
  "action_id" TEXT,
  "before_values" TEXT,
  "after_values" TEXT,
  "recorded_at_ms" INTEGER NOT NULL,
  "consumed_commit_seq" INTEGER,
  "voided_commit_seq" INTEGER,
  PRIMARY KEY ("world", "automation_id", "revision", "trigger_commit_seq", "object_type", "object_id", "object_version")
)"#,
    r#"CREATE INDEX IF NOT EXISTS "_norn_automation_outbox/pending" ON "_norn_automation_outbox" ("world", "trigger_commit_seq", "automation_id", "revision", "object_type", "object_id", "object_version") WHERE "consumed_commit_seq" IS NULL AND "voided_commit_seq" IS NULL"#,
    r#"CREATE INDEX IF NOT EXISTS "_norn_automation_outbox/automation" ON "_norn_automation_outbox" ("automation_id", "revision") WHERE "consumed_commit_seq" IS NULL AND "voided_commit_seq" IS NULL"#,
    r#"CREATE TABLE IF NOT EXISTS "_norn_automation_outbox_continuation" (
  "trigger_commit_seq" INTEGER NOT NULL,
  "world" TEXT NOT NULL,
  "cursor" TEXT NOT NULL,
  "storage_version" INTEGER NOT NULL,
  "engine_version" TEXT NOT NULL,
  "definitions_version" INTEGER NOT NULL,
  "matcher_version" INTEGER NOT NULL,
  "recorded_at_ms" INTEGER NOT NULL,
  "completed_commit_seq" INTEGER,
  PRIMARY KEY ("trigger_commit_seq")
)"#,
    r#"CREATE INDEX IF NOT EXISTS "_norn_automation_outbox_continuation/pending" ON "_norn_automation_outbox_continuation" ("world", "trigger_commit_seq") WHERE "completed_commit_seq" IS NULL"#,
    r#"CREATE TABLE IF NOT EXISTS "_norn_retained_automation_trigger" (
  "automation_id" TEXT NOT NULL,
  "revision" INTEGER NOT NULL,
  "object_type" TEXT NOT NULL,
  "root_type" TEXT NOT NULL,
  "trigger_kind" TEXT NOT NULL,
  "action_id" TEXT,
  "lifecycle_property" TEXT,
  "lifecycle_state" TEXT,
  PRIMARY KEY ("automation_id")
)"#,
    r#"CREATE INDEX IF NOT EXISTS "_norn_retained_automation_trigger/type" ON "_norn_retained_automation_trigger" ("object_type", "automation_id") WHERE "trigger_kind" != 'event:action'"#,
    r#"CREATE INDEX IF NOT EXISTS "_norn_retained_automation_trigger/root" ON "_norn_retained_automation_trigger" ("root_type", "trigger_kind")"#,
    r#"CREATE INDEX IF NOT EXISTS "_norn_retained_automation_trigger/action" ON "_norn_retained_automation_trigger" ("action_id", "automation_id") WHERE "trigger_kind" = 'event:action'"#,
];

/// Dedupe key of one automation trigger candidate.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AutomationTriggerKey {
    /// Admitted automation definition id.
    pub automation_id: String,
    /// Installed revision of the automation definition at trigger time.
    pub revision: u64,
    /// Commit sequence of the triggering write.
    pub trigger_commit_seq: u64,
    /// Concrete object type written or targeted by the triggering commit.
    pub object_type: String,
    /// Target object id.
    pub object_id: String,
    /// Target object version in the triggering commit (or existing/0 for an
    /// unchanged or absent receipt-only action call).
    pub object_version: u64,
}

/// One durable automation trigger candidate recorded inside the triggering
/// commit transaction.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AutomationTriggerCandidate {
    /// World in which the triggering write committed.
    pub world: String,
    /// Dedupe key pinning automation revision, commit sequence, and object version.
    pub key: AutomationTriggerKey,
    /// Matched trigger kind (`event:created`, `event:updated`, `event:action`,
    /// or `lifecycle_entered`).
    pub trigger_kind: String,
    /// Triggering action id when the commit carried an `ActionCall` for this target.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub action_id: Option<String>,
    /// Exact pre-commit stored property values filtered against the rule's
    /// watched type (`None` on create, sibling replacement, or absent target).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub before_values: Option<BTreeMap<String, Value>>,
    /// Exact post-commit stored property values filtered against the rule's
    /// watched type (`None` when the target was deleted or absent).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub after_values: Option<BTreeMap<String, Value>>,
    /// Logical time (ms) of the triggering commit.
    pub recorded_at_ms: u64,
    /// Commit sequence that drained this candidate, or `None` while pending.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub consumed_commit_seq: Option<u64>,
    /// Commit sequence of the `:retire` commit that voided this candidate
    /// before drain, or `None` while pending or consumed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub voided_commit_seq: Option<u64>,
}

/// Pre-commit trigger probe results and pre-write object snapshots captured
/// before `Writer::apply_writes` mutates any rows.
#[derive(Default)]
pub(crate) struct AutomationWatch {
    watched_row_roots: BTreeSet<String>,
    watched_actions: BTreeSet<String>,
    before: BTreeMap<(String, String), Option<ObjectRecord>>,
}

impl AutomationWatch {
    fn is_empty(&self) -> bool {
        self.watched_row_roots.is_empty() && self.watched_actions.is_empty()
    }
}

fn fact_change_target(fact: &FactChange) -> (&str, &str) {
    match fact {
        FactChange::Create {
            object_type, key, ..
        }
        | FactChange::Set {
            object_type, key, ..
        }
        | FactChange::Delete { object_type, key }
        | FactChange::Transition {
            object_type, key, ..
        } => (object_type.as_str(), key.as_str()),
    }
}

struct ActionCallMeta {
    action_id: String,
    target_type: String,
    target_root: String,
    target_key: String,
}

fn extract_action_call<E: SqlEngine>(
    reader: &StoreReader<'_, E>,
    values: &BTreeMap<String, Value>,
) -> Option<ActionCallMeta> {
    let action_id = values.get("actionId").and_then(Value::as_str)?;
    let object = values.get("object").and_then(Value::as_object)?;
    let target_type = object.get("objectType").and_then(Value::as_str)?;
    let target_key = object.get("key").and_then(Value::as_str)?;
    let target_root = reader
        .root_type(target_type)
        .unwrap_or(target_type)
        .to_owned();
    Some(ActionCallMeta {
        action_id: action_id.to_owned(),
        target_type: target_type.to_owned(),
        target_root,
        target_key: target_key.to_owned(),
    })
}

fn action_calls_in_batch<E: SqlEngine>(
    reader: &StoreReader<'_, E>,
    batch: &CommitBatch,
) -> BTreeMap<String, ActionCallMeta> {
    let mut calls = BTreeMap::new();
    for write in &batch.writes {
        if let FactWriteRow::Put {
            object_type,
            key,
            values,
        } = write
            && object_type == norn_kernel::ACTION_CALL_TYPE
            && let Some(meta) = extract_action_call(reader, values)
        {
            calls.insert(key.clone(), meta);
        }
    }
    for fact in &batch.facts {
        if let FactChange::Create {
            object_type,
            key,
            values,
        } = fact
            && object_type.as_str() == norn_kernel::ACTION_CALL_TYPE
            && let Some(meta) = extract_action_call(reader, values)
        {
            calls.insert(key.clone(), meta);
        }
    }
    calls
}

/// Capture pre-commit state for objects touched by `batch` via indexed
/// `LIMIT 1` probes. Returns immediately with zero object reads when no rule
/// matches.
pub(crate) fn capture_before<E: SqlEngine>(
    reader: &StoreReader<'_, E>,
    batch: &CommitBatch,
) -> Result<AutomationWatch, CellError> {
    if batch.writes.is_empty() && batch.facts.is_empty() {
        return Ok(AutomationWatch::default());
    }
    let model = reader.model();
    let action_calls = action_calls_in_batch(reader, batch);
    let mut watched_actions = BTreeSet::new();
    let mut checked_actions = BTreeSet::new();
    for call in action_calls.values() {
        if checked_actions.insert(call.action_id.as_str())
            && has_action_rules(reader.conn, &call.action_id)?
        {
            watched_actions.insert(call.action_id.clone());
        }
    }
    let touched: Vec<(&str, &str)> = batch
        .writes
        .iter()
        .map(FactWriteRow::target)
        .chain(batch.facts.iter().map(fact_change_target))
        .filter(|(ty, _)| model.has_type(ty))
        .collect();
    let mut watched_row_roots = BTreeSet::new();
    let mut checked_roots = BTreeSet::new();
    for (ty, _) in &touched {
        let root = reader.root_type(ty)?;
        if checked_roots.insert(root) && has_row_rules_for_root(reader.conn, root)? {
            watched_row_roots.insert(root.to_owned());
        }
    }
    if watched_row_roots.is_empty() && watched_actions.is_empty() {
        return Ok(AutomationWatch::default());
    }
    let mut before = BTreeMap::new();
    for (ty, key) in touched {
        let root = reader.root_type(ty)?;
        if watched_row_roots.contains(root) {
            capture_root_snapshot(reader, &batch.world, root, key, &mut before)?;
        }
    }
    for call in action_calls.values() {
        if watched_actions.contains(&call.action_id) && model.has_type(&call.target_type) {
            capture_root_snapshot(
                reader,
                &batch.world,
                &call.target_root,
                &call.target_key,
                &mut before,
            )?;
        }
    }
    Ok(AutomationWatch {
        watched_row_roots,
        watched_actions,
        before,
    })
}

fn capture_root_snapshot<E: SqlEngine>(
    reader: &StoreReader<'_, E>,
    world: &str,
    root: &str,
    key: &str,
    before: &mut BTreeMap<(String, String), Option<ObjectRecord>>,
) -> Result<(), CellError> {
    let lookup_key = (root.to_owned(), key.to_owned());
    if before.contains_key(&lookup_key) {
        return Ok(());
    }
    // Query through `root` so a write addressing a supertype or subtype always
    // resolves the live concrete record before mutation.
    let prior = reader.object(world, root, key)?;
    before.insert(lookup_key, prior);
    Ok(())
}

fn row_rule_matches(
    matcher: &TriggerMatcher,
    before_record: Option<&ObjectRecord>,
    after_record: Option<&ObjectRecord>,
) -> bool {
    match matcher {
        TriggerMatcher::Created => before_record.is_none() && after_record.is_some(),
        TriggerMatcher::Updated => before_record.is_some() && after_record.is_some(),
        TriggerMatcher::LifecycleEntered { property, state } => {
            let entered = after_record
                .and_then(|record| record.values.get(property))
                .and_then(Value::as_str)
                == Some(state.as_str());
            let was_in = before_record
                .and_then(|record| record.values.get(property))
                .and_then(Value::as_str)
                == Some(state.as_str());
            entered && !was_in
        }
        TriggerMatcher::Action { .. } => false,
    }
}

/// Lazily serializes one `ObjectRecord`'s stored values to canonical JSON only
/// when a rule actually matches and retains that snapshot after type filtering.
struct LazySnapshot<'a> {
    record: Option<&'a ObjectRecord>,
    cached: Option<String>,
}

impl<'a> LazySnapshot<'a> {
    const fn new(record: Option<&'a ObjectRecord>) -> Self {
        Self {
            record,
            cached: None,
        }
    }

    fn json_for(&mut self, model: &Model, watched_type: &str) -> Result<Option<&str>, CellError> {
        let Some(record) = self
            .record
            .filter(|rec| model.is_a(&rec.object_type, watched_type))
        else {
            return Ok(None);
        };
        if self.cached.is_none() {
            self.cached = Some(canonical_json(&Value::Object(record.values.clone()))?);
        }
        Ok(self.cached.as_deref())
    }
}

struct CandidateEmit<'a> {
    world: &'a str,
    commit_seq: u64,
    recorded_at_ms: u64,
    object_type: &'a str,
    object_id: &'a str,
    object_version: u64,
    action_id: Option<&'a str>,
}

#[derive(Default)]
struct MatchBudget {
    emitted: usize,
    scanned: usize,
}

impl MatchBudget {
    const fn candidates_exhausted(&self) -> bool {
        self.emitted >= MAX_CANDIDATES_PER_COMMIT
    }

    const fn scans_exhausted(&self) -> bool {
        self.scanned >= MAX_RULE_SCANS_PER_COMMIT
    }

    const fn record_candidate(&mut self) {
        self.emitted = self.emitted.saturating_add(1);
    }

    const fn record_scan(&mut self) {
        self.scanned = self.scanned.saturating_add(1);
    }
}

struct RecordContext<'a> {
    batch: &'a CommitBatch,
    commit_seq: u64,
    watch: &'a AutomationWatch,
    final_rows: &'a BTreeMap<(String, String), &'a WrittenRow>,
    action_calls: &'a BTreeMap<String, ActionCallMeta>,
}

struct RowTargetCtx<'a> {
    root: &'a str,
    key: &'a str,
    before_record: Option<&'a ObjectRecord>,
    after_record: Option<&'a ObjectRecord>,
    before_lazy: LazySnapshot<'a>,
    after_lazy: LazySnapshot<'a>,
    emit: CandidateEmit<'a>,
}

struct ActionTargetCtx<'a> {
    call_key: &'a str,
    action_id: &'a str,
    effective_object_type: &'a str,
    before_lazy: LazySnapshot<'a>,
    after_lazy: LazySnapshot<'a>,
    emit: CandidateEmit<'a>,
}

fn emit_candidate<E: SqlEngine>(
    reader: &StoreReader<'_, E>,
    rule: &WatchedRule,
    ctx: &CandidateEmit<'_>,
    before_lazy: &mut LazySnapshot<'_>,
    after_lazy: &mut LazySnapshot<'_>,
) -> Result<(), CellError> {
    let model = reader.model();
    let before_json = before_lazy.json_for(model, &rule.object_type)?;
    let after_json = after_lazy.json_for(model, &rule.object_type)?;
    insert_candidate(
        reader.conn,
        ctx,
        &rule.automation_id,
        rule.revision,
        rule.matcher.kind_label(),
        before_json,
        after_json,
    )
}

/// Record matching automation trigger candidates inside the triggering commit's
/// `SQLite` transaction (Pass 1: row-write triggers; Pass 2: `event:action`
/// driven from committed `norn.ActionCall` records).
///
/// Never refuses or rolls back a business write on automation fan-out: rule
/// lookups are keyset-paged with `LIMIT`, at most [`MAX_CANDIDATES_PER_COMMIT`]
/// candidates are written inline, and any remaining work persists one
/// idempotent continuation row in `_norn_automation_outbox_continuation` in the
/// same transaction for U2 to expand from committed history.
pub(crate) fn record<E: SqlEngine>(
    reader: &StoreReader<'_, E>,
    batch: &CommitBatch,
    written: &[WrittenRow],
    commit_seq: u64,
    definitions_version: u64,
    watch: &AutomationWatch,
    retained: Option<&crate::export::ExportRecord>,
) -> Result<Option<AutomationOutboxContinuation>, CellError> {
    let original = retained.and_then(|record| record.automation_continuation.as_ref());
    if let Some(original) = original
        && original.matcher_version != AUTOMATION_MATCHER_VERSION
    {
        return Err(CellError::EngineUnknownVersion {
            found: format!("automation matcher version {}", original.matcher_version),
        });
    }
    let paused = record_inline(reader, batch, written, commit_seq, watch)?;
    let continuation = if let Some(export) = retained {
        match (paused.as_ref(), original) {
            (None, None) => None,
            (Some(cursor), Some(row))
                if row.cursor == *cursor
                    && row.trigger_commit_seq == commit_seq
                    && row.world == batch.world
                    && Some(row.storage_version) == export.storage_version
                    && row.definitions_version == definitions_version
                    && row.recorded_at_ms == batch.logical_time
                    && row.completed_commit_seq.is_none() =>
            {
                Some(row.clone())
            }
            _ => {
                return Err(CellError::LedgerCorrupt {
                    seq: commit_seq,
                    reason: "automation continuation does not match replay".to_owned(),
                });
            }
        }
    } else {
        paused.map(|cursor| AutomationOutboxContinuation {
            trigger_commit_seq: commit_seq,
            world: batch.world.clone(),
            cursor,
            storage_version: crate::engine_release::STORAGE_VERSION,
            engine_version: current_engine_version(),
            definitions_version,
            matcher_version: AUTOMATION_MATCHER_VERSION,
            recorded_at_ms: batch.logical_time,
            completed_commit_seq: None,
        })
    };
    if let Some(row) = &continuation {
        continuation::insert_continuation(reader.conn, row)?;
    }
    Ok(continuation)
}

fn record_inline<E: SqlEngine>(
    reader: &StoreReader<'_, E>,
    batch: &CommitBatch,
    written: &[WrittenRow],
    commit_seq: u64,
    watch: &AutomationWatch,
) -> Result<Option<AutomationContinuationCursor>, CellError> {
    if watch.is_empty() {
        return Ok(None);
    }
    let model = reader.model();
    let action_calls = action_calls_in_batch(reader, batch);
    let mut final_rows: BTreeMap<(String, String), &WrittenRow> = BTreeMap::new();
    for row in written {
        if model.has_type(&row.object_type) {
            let root = reader.root_type(&row.object_type)?.to_owned();
            final_rows.insert((root, row.key.clone()), row);
        }
    }
    let ctx = RecordContext {
        batch,
        commit_seq,
        watch,
        final_rows: &final_rows,
        action_calls: &action_calls,
    };
    let mut budget = MatchBudget::default();
    let mut paused = if watch.watched_row_roots.is_empty() {
        None
    } else {
        record_row_triggers(reader, &ctx, &mut budget)?
    };
    if paused.is_none() && !watch.watched_actions.is_empty() {
        paused = record_action_triggers(reader, &ctx, &mut budget)?;
    }
    Ok(paused)
}

fn record_row_triggers<E: SqlEngine>(
    reader: &StoreReader<'_, E>,
    ctx: &RecordContext<'_>,
    budget: &mut MatchBudget,
) -> Result<Option<AutomationContinuationCursor>, CellError> {
    let model = reader.model();
    let mut row_cache = Page1Cache::default();
    for (target_key, row) in ctx.final_rows {
        let (root, key) = target_key;
        if !ctx.watch.watched_row_roots.contains(root) {
            continue;
        }
        let chain = model.chain(&row.object_type)?;
        let mut first_pages = Vec::with_capacity(chain.len());
        for ancestor in chain {
            let page = row_cache.get_or_load(reader.conn, ancestor, query_row_rules_page)?;
            first_pages.push((ancestor.as_str(), page));
        }
        if first_pages.iter().all(|(_, page)| page.rules.is_empty()) {
            continue;
        }
        let before_record = ctx.watch.before.get(target_key).and_then(Option::as_ref);
        let after_record = reader.object(&ctx.batch.world, root, key)?;
        let primary_action = ctx
            .action_calls
            .values()
            .find(|call| call.target_root == *root && call.target_key == *key)
            .map(|call| call.action_id.as_str());
        let mut target = RowTargetCtx {
            root,
            key,
            before_record,
            after_record: after_record.as_ref(),
            before_lazy: LazySnapshot::new(before_record),
            after_lazy: LazySnapshot::new(after_record.as_ref()),
            emit: CandidateEmit {
                world: &ctx.batch.world,
                commit_seq: ctx.commit_seq,
                recorded_at_ms: ctx.batch.logical_time,
                object_type: &row.object_type,
                object_id: &row.key,
                object_version: row.version,
                action_id: primary_action,
            },
        };
        for (ancestor, first_page) in first_pages {
            if first_page.rules.is_empty() {
                continue;
            }
            if let Some(cursor) =
                match_row_rules_for_ancestor(reader, &mut target, ancestor, first_page, budget)?
            {
                return Ok(Some(cursor));
            }
        }
    }
    Ok(None)
}

fn match_row_rules_for_ancestor<E: SqlEngine>(
    reader: &StoreReader<'_, E>,
    target: &mut RowTargetCtx<'_>,
    ancestor: &str,
    first_page: RulePage,
    budget: &mut MatchBudget,
) -> Result<Option<AutomationContinuationCursor>, CellError> {
    let model = reader.model();
    let mut page = first_page;
    let mut after_id: Option<String> = None;
    loop {
        let mut prev_id = after_id;
        for rule in &page.rules {
            if budget.scans_exhausted() {
                return Ok(Some(AutomationContinuationCursor::Row {
                    root_type: target.root.to_owned(),
                    object_id: target.key.to_owned(),
                    ancestor_type: ancestor.to_owned(),
                    after_automation_id: prev_id,
                }));
            }
            budget.record_scan();
            let rule_before = target
                .before_record
                .filter(|rec| model.is_a(&rec.object_type, &rule.object_type));
            let rule_after = target
                .after_record
                .filter(|rec| model.is_a(&rec.object_type, &rule.object_type));
            if row_rule_matches(&rule.matcher, rule_before, rule_after) {
                if budget.candidates_exhausted() {
                    return Ok(Some(AutomationContinuationCursor::Row {
                        root_type: target.root.to_owned(),
                        object_id: target.key.to_owned(),
                        ancestor_type: ancestor.to_owned(),
                        after_automation_id: prev_id,
                    }));
                }
                emit_candidate(
                    reader,
                    rule,
                    &target.emit,
                    &mut target.before_lazy,
                    &mut target.after_lazy,
                )?;
                budget.record_candidate();
            }
            prev_id = Some(rule.automation_id.clone());
        }
        if !page.has_more {
            return Ok(None);
        }
        after_id = prev_id;
        page = query_row_rules_page(reader.conn, ancestor, after_id.as_deref(), RULE_PAGE_LIMIT)?;
        if page.rules.is_empty() {
            return Ok(None);
        }
    }
}

fn record_action_triggers<E: SqlEngine>(
    reader: &StoreReader<'_, E>,
    ctx: &RecordContext<'_>,
    budget: &mut MatchBudget,
) -> Result<Option<AutomationContinuationCursor>, CellError> {
    let model = reader.model();
    let mut action_cache = Page1Cache::default();
    for (call_key, call) in ctx.action_calls {
        if !ctx.watch.watched_actions.contains(&call.action_id) {
            continue;
        }
        let first_page =
            action_cache.get_or_load(reader.conn, &call.action_id, query_action_rules_page)?;
        if first_page.rules.is_empty() {
            continue;
        }
        let target_key = (call.target_root.clone(), call.target_key.clone());
        let before_record = ctx.watch.before.get(&target_key).and_then(Option::as_ref);
        let after_record = if model.has_type(&call.target_type) {
            reader.object(&ctx.batch.world, &call.target_root, &call.target_key)?
        } else {
            None
        };
        let written_row = ctx.final_rows.get(&target_key).copied();
        let effective_object_type = after_record
            .as_ref()
            .map(|rec| rec.object_type.as_str())
            .or_else(|| before_record.map(|rec| rec.object_type.as_str()))
            .or_else(|| written_row.map(|row| row.object_type.as_str()))
            .unwrap_or(&call.target_type);
        let object_version = if let Some(row) = written_row {
            row.version
        } else if let Some(after) = &after_record {
            after.version
        } else if let Some(before) = before_record {
            before.version
        } else if model.has_type(effective_object_type) {
            reader.version(&ctx.batch.world, effective_object_type, &call.target_key)?
        } else {
            0
        };
        let mut target = ActionTargetCtx {
            call_key,
            action_id: &call.action_id,
            effective_object_type,
            before_lazy: LazySnapshot::new(before_record),
            after_lazy: LazySnapshot::new(after_record.as_ref()),
            emit: CandidateEmit {
                world: &ctx.batch.world,
                commit_seq: ctx.commit_seq,
                recorded_at_ms: ctx.batch.logical_time,
                object_type: effective_object_type,
                object_id: &call.target_key,
                object_version,
                action_id: Some(call.action_id.as_str()),
            },
        };
        if let Some(cursor) = match_action_rules_for_call(reader, &mut target, first_page, budget)?
        {
            return Ok(Some(cursor));
        }
    }
    Ok(None)
}

fn match_action_rules_for_call<E: SqlEngine>(
    reader: &StoreReader<'_, E>,
    target: &mut ActionTargetCtx<'_>,
    first_page: RulePage,
    budget: &mut MatchBudget,
) -> Result<Option<AutomationContinuationCursor>, CellError> {
    let model = reader.model();
    let mut page = first_page;
    let mut after_id: Option<String> = None;
    loop {
        let mut prev_id = after_id;
        for rule in &page.rules {
            if budget.scans_exhausted() {
                return Ok(Some(AutomationContinuationCursor::Action {
                    action_call_key: target.call_key.to_owned(),
                    action_id: target.action_id.to_owned(),
                    after_automation_id: prev_id,
                }));
            }
            budget.record_scan();
            if model.is_a(target.effective_object_type, &rule.object_type) {
                if budget.candidates_exhausted() {
                    return Ok(Some(AutomationContinuationCursor::Action {
                        action_call_key: target.call_key.to_owned(),
                        action_id: target.action_id.to_owned(),
                        after_automation_id: prev_id,
                    }));
                }
                emit_candidate(
                    reader,
                    rule,
                    &target.emit,
                    &mut target.before_lazy,
                    &mut target.after_lazy,
                )?;
                budget.record_candidate();
            }
            prev_id = Some(rule.automation_id.clone());
        }
        if !page.has_more {
            return Ok(None);
        }
        after_id = prev_id;
        page = query_action_rules_page(
            reader.conn,
            target.action_id,
            after_id.as_deref(),
            RULE_PAGE_LIMIT,
        )?;
        if page.rules.is_empty() {
            return Ok(None);
        }
    }
}

fn insert_candidate<E: SqlEngine>(
    conn: &E,
    ctx: &CandidateEmit<'_>,
    automation_id: &str,
    revision: u64,
    trigger_kind: &str,
    before_json: Option<&str>,
    after_json: Option<&str>,
) -> Result<(), CellError> {
    conn.execute(
        &format!(
            "INSERT OR IGNORE INTO \"{AUTOMATION_OUTBOX}\" (\
              \"world\", \"automation_id\", \"revision\", \"trigger_commit_seq\", \
              \"object_type\", \"object_id\", \"object_version\", \"trigger_kind\", \
              \"action_id\", \"before_values\", \"after_values\", \"recorded_at_ms\", \
              \"consumed_commit_seq\", \"voided_commit_seq\"\
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, NULL, NULL)"
        ),
        params![
            ctx.world,
            automation_id,
            to_i64(revision, "automation revision")?,
            to_i64(ctx.commit_seq, "trigger commit seq")?,
            ctx.object_type,
            ctx.object_id,
            to_i64(ctx.object_version, "object version")?,
            trigger_kind,
            ctx.action_id,
            before_json,
            after_json,
            to_i64(ctx.recorded_at_ms, "trigger recorded_at_ms")?,
        ],
    )
}

#[cfg(all(test, feature = "native"))]
mod tests {
    use super::trigger_index::{KIND_ACTION, KIND_CREATED, KIND_LIFECYCLE_ENTERED, KIND_UPDATED};
    use super::*;
    use crate::Model;
    use crate::flow_run::RunStarter;
    use crate::retained::{
        RetainedAuthority, RetainedDefinition, RetainedDefinitionChange, RetainedLifecycle,
        payload_sha256,
    };
    use crate::store::{
        CellStore, CommitOutcome, CommitPoint, InstalledDefinitionInput, MAIN_WORLD, StoreOptions,
    };
    use crate::test_door::TestCommit as _;
    use norn_ir::{ObjectTypeDef, ObjectTypeName};
    use serde_json::json;

    type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

    const SCOPE: &str = "org_demo/dev";

    fn sample_model() -> TestResult<(Model, String)> {
        let ticket = json!({
            "name": "demo.Ticket",
            "properties": [
                {
                    "id": "status",
                    "type": {
                        "kind": "enum",
                        "values": [
                            { "value": "open" },
                            { "value": "stalled" },
                            { "value": "closed" }
                        ]
                    }
                },
                { "id": "score", "type": { "kind": "integer" } }
            ],
            "lifecycles": [
                {
                    "property": "status",
                    "initial": ["open"],
                    "terminal": ["closed"],
                    "transitions": [
                        { "id": "stall", "from": ["open"], "to": "stalled" },
                        { "id": "close", "from": ["open", "stalled"], "to": "closed" }
                    ]
                }
            ]
        });
        let ticket_a = json!({
            "name": "demo.TicketA",
            "extends": "demo.Ticket",
            "properties": [
                { "id": "a_tag", "type": { "kind": "text" }, "default": "a" }
            ]
        });
        let ticket_b = json!({
            "name": "demo.TicketB",
            "extends": "demo.Ticket",
            "properties": [
                { "id": "b_tag", "type": { "kind": "text" }, "default": "b" }
            ]
        });
        let note = json!({
            "name": "demo.Note",
            "properties": [
                { "id": "body", "type": { "kind": "text" } }
            ]
        });
        let action_call = json!({
            "name": norn_kernel::ACTION_CALL_TYPE,
            "properties": [
                { "id": "actionId", "type": { "kind": "text" } },
                { "id": "object", "type": { "kind": "json-ext" } }
            ]
        });
        let defs: Vec<ObjectTypeDef> = [&ticket, &ticket_a, &ticket_b, &note, &action_call]
            .into_iter()
            .map(|raw| serde_json::from_value(raw.clone()))
            .collect::<Result<_, _>>()?;
        let model = Model::from_definitions(&defs, &[])?;
        let artifacts = sample_artifacts(&ticket, &ticket_a, &ticket_b, &note);
        let canonical = String::from_utf8(Value::from_json(artifacts)?.canonical_json()?)?;
        Ok((model, canonical))
    }

    fn sample_artifacts(
        ticket: &serde_json::Value,
        ticket_a: &serde_json::Value,
        ticket_b: &serde_json::Value,
        note: &serde_json::Value,
    ) -> serde_json::Value {
        json!([
            {
                "irVersion": "0.1",
                "artifactId": "demo.Ticket.entity",
                "kind": "entity",
                "namespace": "demo",
                "name": "Ticket",
                "version": "0.1.0",
                "lifecycle": "draft",
                "scope": { "orgId": "org_demo", "environmentId": "dev" },
                "dependencies": [],
                "provenance": { "source": "authoring_sdk", "sourcePath": "test" },
                "payload": { "entityVersion": "2.0", "objectType": ticket }
            },
            {
                "irVersion": "0.1",
                "artifactId": "demo.TicketA.entity",
                "kind": "entity",
                "namespace": "demo",
                "name": "TicketA",
                "version": "0.1.0",
                "lifecycle": "draft",
                "scope": { "orgId": "org_demo", "environmentId": "dev" },
                "dependencies": [],
                "provenance": { "source": "authoring_sdk", "sourcePath": "test" },
                "payload": { "entityVersion": "2.0", "objectType": ticket_a }
            },
            {
                "irVersion": "0.1",
                "artifactId": "demo.TicketB.entity",
                "kind": "entity",
                "namespace": "demo",
                "name": "TicketB",
                "version": "0.1.0",
                "lifecycle": "draft",
                "scope": { "orgId": "org_demo", "environmentId": "dev" },
                "dependencies": [],
                "provenance": { "source": "authoring_sdk", "sourcePath": "test" },
                "payload": { "entityVersion": "2.0", "objectType": ticket_b }
            },
            {
                "irVersion": "0.1",
                "artifactId": "demo.Note.entity",
                "kind": "entity",
                "namespace": "demo",
                "name": "Note",
                "version": "0.1.0",
                "lifecycle": "draft",
                "scope": { "orgId": "org_demo", "environmentId": "dev" },
                "dependencies": [],
                "provenance": { "source": "authoring_sdk", "sourcePath": "test" },
                "payload": { "entityVersion": "2.0", "objectType": note }
            }
        ])
    }

    fn install_input(canonical_set: &str, installed_at: u64) -> InstalledDefinitionInput {
        InstalledDefinitionInput {
            canonical_set: canonical_set.to_owned(),
            registry: Vec::new(),
            receipt_sha256: "a".repeat(64),
            admission_receipt: None,
            readmission: None,
            installed_at,
            diff: json!({ "added": [], "removed": [], "changed": [], "narrowing": [] }),
            layout: None,
        }
    }

    fn install_automation_for(
        store: &mut CellStore,
        model: &Model,
        canonical_set: &str,
        expected_defs_version: u64,
        id: &str,
        object_type: &str,
        when: &serde_json::Value,
    ) -> TestResult {
        let payload = serde_json::to_string(&json!({
            "title": id,
            "enabled": true,
            "objectType": object_type,
            "when": when,
            "then": {
                "actionId": "demo.ticket.escalate",
                "mode": "auto"
            }
        }))?;
        let change = RetainedDefinitionChange {
            definition: RetainedDefinition {
                kind: "automation".to_owned(),
                id: id.to_owned(),
                revision: 1,
                lifecycle: RetainedLifecycle::Active,
                title: id.to_owned(),
                payload_sha256: payload_sha256(&payload),
                payload,
                signoff: None,
                authority: Some(RetainedAuthority {
                    delegation_id: format!("norn.automation.grant/{id}"),
                    author: RunStarter {
                        subject: "owner@demo".to_owned(),
                        actor_kind: "user".to_owned(),
                        roles: vec!["owner".to_owned()],
                        role_table: "demo".to_owned(),
                    },
                    scope: SCOPE.to_owned(),
                    action_id: "demo.ticket.escalate".to_owned(),
                    object_type: object_type.to_owned(),
                    action_digest: "d".repeat(64),
                    parent_grant_ids: vec!["tenant.actions/owner_permit".to_owned()],
                    bound_at: 20,
                    definitions_version: expected_defs_version,
                    revocable: true,
                }),
            },
            expected_revision: None,
        };
        store.install_retained_definition(
            model.clone(),
            &install_input(canonical_set, 20),
            Some(expected_defs_version),
            SCOPE,
            &change,
        )?;
        Ok(())
    }

    fn retire_automation(
        store: &mut CellStore,
        model: &Model,
        canonical_set: &str,
        expected_defs_version: u64,
        id: &str,
        current_revision: u64,
        retired_at: u64,
    ) -> TestResult {
        let current = store
            .read()
            .retained_definition(id, Some(current_revision))?
            .ok_or("missing exists")?;
        let mut definition = current.definition;
        definition.revision = current_revision.saturating_add(1);
        definition.lifecycle = RetainedLifecycle::Retired;
        definition.signoff = None;
        definition.authority = None;
        let change = RetainedDefinitionChange {
            definition,
            expected_revision: Some(current_revision),
        };
        store.install_retained_definition(
            model.clone(),
            &install_input(canonical_set, retired_at),
            Some(expected_defs_version),
            SCOPE,
            &change,
        )?;
        Ok(())
    }

    fn open_with_ticket_automations(
        path: &std::path::Path,
    ) -> TestResult<(CellStore, Model, String)> {
        let (model, canonical) = sample_model()?;
        let mut store = CellStore::open(path, model.clone(), StoreOptions::default())?;
        store.bind("scope", SCOPE)?;
        store.install_definitions(model.clone(), install_input(&canonical, 10), Some(0), SCOPE)?;
        install_automation_for(
            &mut store,
            &model,
            &canonical,
            1,
            "ops.on_create",
            "demo.Ticket",
            &json!({ "kind": "event", "event": "created" }),
        )?;
        install_automation_for(
            &mut store,
            &model,
            &canonical,
            2,
            "ops.on_stalled",
            "demo.Ticket",
            &json!({ "kind": "lifecycle_entered", "property": "status", "state": "stalled" }),
        )?;
        Ok((store, model, canonical))
    }

    fn ticket_batch(idempotency_key: &str, logical_time: u64, status: &str) -> CommitBatch {
        CommitBatch {
            world: MAIN_WORLD.to_owned(),
            scope: SCOPE.to_owned(),
            logical_time,
            idempotency_key: Some(idempotency_key.to_owned()),
            writes: vec![FactWriteRow::Put {
                object_type: "demo.Ticket".to_owned(),
                key: "t-1".to_owned(),
                values: BTreeMap::from([
                    ("status".to_owned(), Value::String(status.into())),
                    ("score".to_owned(), Value::from(40_i64)),
                ]),
            }],
            ..CommitBatch::default()
        }
    }

    fn action_call_write(
        action_id: &str,
        target_type: &str,
        target_key: &str,
        key: &str,
    ) -> FactWriteRow {
        FactWriteRow::Put {
            object_type: norn_kernel::ACTION_CALL_TYPE.to_owned(),
            key: format!("{action_id}/{key}"),
            values: BTreeMap::from([
                ("actionId".to_owned(), Value::String(action_id.into())),
                (
                    "object".to_owned(),
                    Value::Object(BTreeMap::from([
                        ("objectType".to_owned(), Value::String(target_type.into())),
                        ("key".to_owned(), Value::String(target_key.into())),
                    ])),
                ),
            ]),
        }
    }

    #[test]
    fn rolls_back_before_commit_and_survives_crash_after_commit() -> TestResult {
        let dir = tempfile::tempdir()?;
        let path = dir.path().join("cell.sqlite");
        let (mut store, model, _canonical) = open_with_ticket_automations(&path)?;

        let create_batch = ticket_batch("create-t1", 100, "open");
        let aborted = store.commit_probed(&create_batch, &mut |point| {
            if point == CommitPoint::FactsApplied {
                return Err(CellError::Engine(
                    "simulated crash before commit".to_owned(),
                ));
            }
            Ok(())
        });
        assert!(aborted.is_err());
        assert_eq!(
            store.read().pending_automation_trigger_count(MAIN_WORLD)?,
            0,
            "aborted commit must record no trigger candidates"
        );

        let post_crash = store.commit_probed(&create_batch, &mut |point| {
            if point == CommitPoint::Committed {
                return Err(CellError::Engine("simulated crash after commit".to_owned()));
            }
            Ok(())
        });
        assert!(post_crash.is_err());
        drop(store);

        let reopened = CellStore::open(&path, model, StoreOptions::default())?;
        let pending = reopened
            .read()
            .pending_automation_triggers(MAIN_WORLD, 10)?;
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].key.automation_id, "ops.on_create");
        assert_eq!(pending[0].key.revision, 1);
        assert_eq!(pending[0].key.object_type, "demo.Ticket");
        assert_eq!(pending[0].key.object_id, "t-1");
        assert_eq!(pending[0].key.object_version, 1);
        assert_eq!(pending[0].before_values, None);
        assert_eq!(pending[0].voided_commit_seq, None);
        assert_eq!(
            pending[0]
                .after_values
                .as_ref()
                .and_then(|values| values.get("status")),
            Some(&Value::String("open".into()))
        );
        Ok(())
    }

    #[test]
    fn replay_dedupes_retire_voids_pending_and_lifecycle_captures_before_and_after() -> TestResult {
        let dir = tempfile::tempdir()?;
        let path = dir.path().join("cell.sqlite");
        let (mut store, model, canonical) = open_with_ticket_automations(&path)?;

        let create_batch = ticket_batch("create-t1", 100, "open");
        let CommitOutcome::Committed(create_receipt) = store.commit(&create_batch)? else {
            return Err("expected committed receipt".into());
        };
        let create_seq = create_receipt.body.commit_seq;

        let replayed = store.commit(&create_batch)?;
        assert!(matches!(replayed, CommitOutcome::Replayed(_)));
        assert_eq!(
            store.read().pending_automation_trigger_count(MAIN_WORLD)?,
            1
        );

        let pending = store.read().pending_automation_triggers(MAIN_WORLD, 10)?;
        let dup_key = &pending.first().ok_or("missing pending trigger")?.key;
        insert_candidate(
            store.read().conn,
            &CandidateEmit {
                world: MAIN_WORLD,
                commit_seq: dup_key.trigger_commit_seq,
                recorded_at_ms: 100,
                object_type: &dup_key.object_type,
                object_id: &dup_key.object_id,
                object_version: dup_key.object_version,
                action_id: None,
            },
            &dup_key.automation_id,
            dup_key.revision,
            KIND_CREATED,
            None,
            None,
        )?;
        assert_eq!(
            store.read().pending_automation_trigger_count(MAIN_WORLD)?,
            1
        );

        // Retiring `ops.on_create` atomically voids its pending candidates and removes its index row.
        retire_automation(&mut store, &model, &canonical, 3, "ops.on_create", 1, 150)?;
        assert_eq!(
            store.read().pending_automation_trigger_count(MAIN_WORLD)?,
            0,
            "retire must void pending candidates in the same transaction"
        );
        let audited = store
            .read()
            .automation_triggers_for_commit(MAIN_WORLD, create_seq)?;
        let [audited] = audited.as_slice() else {
            return Err("expected one automation trigger".into());
        };
        assert!(audited.voided_commit_seq.is_some());

        let stall_batch = ticket_batch("stall-t1", 200, "stalled");
        let CommitOutcome::Committed(receipt) = store.commit(&stall_batch)? else {
            return Err("expected committed receipt".into());
        };
        let for_commit = store
            .read()
            .automation_triggers_for_commit(MAIN_WORLD, receipt.body.commit_seq)?;
        let [for_commit] = for_commit.as_slice() else {
            return Err("expected one automation trigger".into());
        };
        assert_eq!(for_commit.key.automation_id, "ops.on_stalled");
        assert_eq!(for_commit.key.object_version, 2);
        assert_eq!(for_commit.trigger_kind, KIND_LIFECYCLE_ENTERED);
        assert_eq!(
            for_commit
                .before_values
                .as_ref()
                .and_then(|values| values.get("status")),
            Some(&Value::String("open".into()))
        );
        assert_eq!(
            for_commit
                .after_values
                .as_ref()
                .and_then(|values| values.get("status")),
            Some(&Value::String("stalled".into()))
        );
        Ok(())
    }

    /// S3: `event:action` is driven from the committed `norn.ActionCall` record:
    /// covers receipt-only calls with no row write, unchanged targets (`before == after`),
    /// and actions that delete their target (`after_values == None`, `before_values == Some(...)`).
    #[test]
    fn action_triggers_cover_receipt_only_unchanged_and_deleted_targets() -> TestResult {
        let dir = tempfile::tempdir()?;
        let path = dir.path().join("action-triggers.sqlite");
        let (model, canonical) = sample_model()?;
        let mut store = CellStore::open(&path, model.clone(), StoreOptions::default())?;
        store.bind("scope", SCOPE)?;
        store.install_definitions(model.clone(), install_input(&canonical, 10), Some(0), SCOPE)?;
        install_automation_for(
            &mut store,
            &model,
            &canonical,
            1,
            "ops.on_receipt_only",
            "demo.Ticket",
            &json!({ "kind": "event", "event": "action", "actionId": "demo.ticket.ping" }),
        )?;
        install_automation_for(
            &mut store,
            &model,
            &canonical,
            2,
            "ops.on_delete",
            "demo.Ticket",
            &json!({ "kind": "event", "event": "action", "actionId": "demo.ticket.remove" }),
        )?;

        // 1. Receipt-only call targeting an absent object (no business row write).
        let absent_call = CommitBatch {
            world: MAIN_WORLD.to_owned(),
            scope: SCOPE.to_owned(),
            logical_time: 100,
            idempotency_key: Some("ping-absent".to_owned()),
            writes: vec![action_call_write(
                "demo.ticket.ping",
                "demo.Ticket",
                "t-ghost",
                "k1",
            )],
            ..CommitBatch::default()
        };
        let CommitOutcome::Committed(absent_rcpt) = store.commit(&absent_call)? else {
            return Err("expected commit".into());
        };
        let absent_triggers = store
            .read()
            .automation_triggers_for_commit(MAIN_WORLD, absent_rcpt.body.commit_seq)?;
        let [absent_triggers] = absent_triggers.as_slice() else {
            return Err("expected one automation trigger".into());
        };
        assert_eq!(absent_triggers.trigger_kind, KIND_ACTION);
        assert_eq!(absent_triggers.key.object_id, "t-ghost");
        assert_eq!(absent_triggers.key.object_version, 0);
        assert_eq!(absent_triggers.before_values, None);
        assert_eq!(absent_triggers.after_values, None);

        // Seed a live ticket `t-live`.
        store.commit(&ticket_batch("seed-live", 110, "open"))?;

        // 2. Receipt-only call on existing unchanged target `t-1`: before == after.
        let unchanged_call = CommitBatch {
            world: MAIN_WORLD.to_owned(),
            scope: SCOPE.to_owned(),
            logical_time: 120,
            idempotency_key: Some("ping-unchanged".to_owned()),
            writes: vec![action_call_write(
                "demo.ticket.ping",
                "demo.Ticket",
                "t-1",
                "k2",
            )],
            ..CommitBatch::default()
        };
        let CommitOutcome::Committed(unchanged_rcpt) = store.commit(&unchanged_call)? else {
            return Err("expected commit".into());
        };
        let unchanged_triggers = store
            .read()
            .automation_triggers_for_commit(MAIN_WORLD, unchanged_rcpt.body.commit_seq)?;
        let [unchanged_triggers] = unchanged_triggers.as_slice() else {
            return Err("expected one automation trigger".into());
        };
        assert_eq!(unchanged_triggers.key.object_id, "t-1");
        assert_eq!(unchanged_triggers.key.object_version, 1);
        assert!(unchanged_triggers.before_values.is_some());
        assert_eq!(
            unchanged_triggers.before_values, unchanged_triggers.after_values,
            "unchanged target must have before equal to after"
        );

        assert_delete_and_fact_create_triggers(&mut store)?;
        Ok(())
    }

    fn assert_delete_and_fact_create_triggers(store: &mut CellStore) -> TestResult {
        // 3. Action that deletes its target (for example owned-row parent delete):
        //    `before_values = Some(prior)`, `after_values = None`.
        let delete_call = CommitBatch {
            world: MAIN_WORLD.to_owned(),
            scope: SCOPE.to_owned(),
            logical_time: 130,
            idempotency_key: Some("remove-t1".to_owned()),
            writes: vec![
                FactWriteRow::Delete {
                    object_type: "demo.Ticket".to_owned(),
                    key: "t-1".to_owned(),
                },
                action_call_write("demo.ticket.remove", "demo.Ticket", "t-1", "k3"),
            ],
            ..CommitBatch::default()
        };
        let CommitOutcome::Committed(delete_rcpt) = store.commit(&delete_call)? else {
            return Err("expected commit".into());
        };
        let delete_triggers = store
            .read()
            .automation_triggers_for_commit(MAIN_WORLD, delete_rcpt.body.commit_seq)?;
        let [delete_triggers] = delete_triggers.as_slice() else {
            return Err("expected one automation trigger".into());
        };
        assert_eq!(delete_triggers.key.automation_id, "ops.on_delete");
        assert_eq!(delete_triggers.key.object_id, "t-1");
        assert_eq!(delete_triggers.key.object_version, 2);
        assert_eq!(
            delete_triggers
                .before_values
                .as_ref()
                .and_then(|values| values.get("status")),
            Some(&Value::String("open".into()))
        );
        assert_eq!(
            delete_triggers.after_values, None,
            "deleted target must have after_values = None"
        );

        // 4. Canonical `FactChange::Create` for `norn.ActionCall` in `batch.facts`
        //    is also collected and triggers `event:action`.
        let fact_create_call = CommitBatch {
            world: MAIN_WORLD.to_owned(),
            scope: SCOPE.to_owned(),
            logical_time: 140,
            idempotency_key: Some("ping-fact-create".to_owned()),
            facts: vec![FactChange::Create {
                object_type: ObjectTypeName(norn_kernel::ACTION_CALL_TYPE.to_owned()),
                key: "demo.ticket.ping/k4".to_owned(),
                values: BTreeMap::from([
                    (
                        "actionId".to_owned(),
                        Value::String("demo.ticket.ping".into()),
                    ),
                    (
                        "object".to_owned(),
                        Value::Object(BTreeMap::from([
                            ("objectType".to_owned(), Value::String("demo.Ticket".into())),
                            ("key".to_owned(), Value::String("t-ghost-fact".into())),
                        ])),
                    ),
                ]),
            }],
            ..CommitBatch::default()
        };
        let CommitOutcome::Committed(fact_rcpt) = store.commit(&fact_create_call)? else {
            return Err("expected commit".into());
        };
        let fact_triggers = store
            .read()
            .automation_triggers_for_commit(MAIN_WORLD, fact_rcpt.body.commit_seq)?;
        let [fact_triggers] = fact_triggers.as_slice() else {
            return Err("expected one automation trigger".into());
        };
        assert_eq!(fact_triggers.key.automation_id, "ops.on_receipt_only");
        assert_eq!(fact_triggers.key.object_id, "t-ghost-fact");
        assert_eq!(fact_triggers.before_values, None);
        assert_eq!(fact_triggers.after_values, None);
        Ok(())
    }

    fn assert_unrelated_write_skips_outbox(store: &mut CellStore) -> TestResult {
        let unrelated = CommitBatch {
            world: MAIN_WORLD.to_owned(),
            scope: SCOPE.to_owned(),
            logical_time: 100,
            idempotency_key: Some("note-1".to_owned()),
            writes: vec![FactWriteRow::Put {
                object_type: "demo.Note".to_owned(),
                key: "n-1".to_owned(),
                values: BTreeMap::from([("body".to_owned(), Value::String("hello".into()))]),
            }],
            ..CommitBatch::default()
        };
        let watch = capture_before(&store.read(), &unrelated)?;
        assert!(
            watch.is_empty(),
            "unrelated write must match zero indexed triggers"
        );
        assert!(
            watch.before.is_empty(),
            "unrelated write must perform zero pre-commit object reads"
        );
        let CommitOutcome::Committed(note_rcpt) = store.commit(&unrelated)? else {
            return Err("expected commit".into());
        };
        assert_eq!(
            store.read().pending_automation_trigger_count(MAIN_WORLD)?,
            0
        );
        assert_eq!(
            store
                .read()
                .pending_automation_continuation_count(MAIN_WORLD)?,
            0
        );
        assert_eq!(
            store
                .read()
                .automation_outbox_continuation(note_rcpt.body.commit_seq)?,
            None
        );
        Ok(())
    }

    fn overflow_batch(overflow_count: usize) -> CommitBatch {
        let overflow_writes: Vec<FactWriteRow> = (0..overflow_count)
            .map(|idx| FactWriteRow::Put {
                object_type: "demo.Ticket".to_owned(),
                key: format!("t-bulk-{idx:04}"),
                values: BTreeMap::from([
                    ("status".to_owned(), Value::String("open".into())),
                    ("score".to_owned(), Value::from(10_i64)),
                ]),
            })
            .collect();
        CommitBatch {
            world: MAIN_WORLD.to_owned(),
            scope: SCOPE.to_owned(),
            logical_time: 200,
            idempotency_key: Some("bulk-overflow".to_owned()),
            writes: overflow_writes,
            ..CommitBatch::default()
        }
    }

    fn assert_overflow_rolls_back(
        store: &mut CellStore,
        overflow_batch: &CommitBatch,
    ) -> TestResult {
        // 1. A rolled-back transaction leaves neither candidates nor a continuation row.
        let aborted = store.commit_probed(overflow_batch, &mut |point| {
            if point == CommitPoint::FactsApplied {
                return Err(CellError::Engine(
                    "simulated crash after outbox and continuation write".to_owned(),
                ));
            }
            Ok(())
        });
        assert!(aborted.is_err());
        assert_eq!(
            store.read().pending_automation_trigger_count(MAIN_WORLD)?,
            0,
            "rolled-back transaction must leave zero candidates"
        );
        assert_eq!(
            store
                .read()
                .pending_automation_continuation_count(MAIN_WORLD)?,
            0,
            "rolled-back transaction must leave zero continuation rows"
        );
        assert_eq!(
            store
                .read()
                .object(MAIN_WORLD, "demo.Ticket", "t-bulk-0000")?,
            None
        );
        Ok(())
    }

    /// S4: unrelated writes skip all object reads and automation work; a
    /// single-rule, large-batch write over `MAX_CANDIDATES_PER_COMMIT` commits
    /// cleanly with exactly `MAX` inline candidates and one idempotent
    /// continuation row in the same transaction; a rolled-back transaction
    /// leaves neither candidates nor a continuation row.
    #[tokio::test]
    async fn unrelated_write_skips_work_and_large_batch_records_transactional_continuation()
    -> TestResult {
        let dir = tempfile::tempdir()?;
        let path = dir.path().join("hotpath.sqlite");
        let (model, canonical) = sample_model()?;
        let mut store = CellStore::open(&path, model.clone(), StoreOptions::default())?;
        store.bind("scope", SCOPE)?;
        store.install_definitions(model.clone(), install_input(&canonical, 10), Some(0), SCOPE)?;
        // Single active rule watching `demo.Ticket` creates.
        install_automation_for(
            &mut store,
            &model,
            &canonical,
            1,
            "ops.on_create",
            "demo.Ticket",
            &json!({ "kind": "event", "event": "created" }),
        )?;

        assert_unrelated_write_skips_outbox(&mut store)?;

        // Single-rule large-batch write exceeding MAX_CANDIDATES_PER_COMMIT.
        let overflow_count = MAX_CANDIDATES_PER_COMMIT.saturating_add(5);
        let overflow_batch = overflow_batch(overflow_count);
        assert_overflow_rolls_back(&mut store, &overflow_batch)?;

        // 2. The real commit succeeds without refusal: every business row lands,
        //    exactly MAX_CANDIDATES_PER_COMMIT inline candidates are persisted,
        //    and one continuation row captures the exact resume cursor and versions.
        let CommitOutcome::Committed(receipt) = store.commit(&overflow_batch)? else {
            return Err("expected committed receipt".into());
        };
        let commit_seq = receipt.body.commit_seq;
        assert!(
            store
                .read()
                .object(MAIN_WORLD, "demo.Ticket", "t-bulk-0000")?
                .is_some()
        );
        assert!(
            store
                .read()
                .object(
                    MAIN_WORLD,
                    "demo.Ticket",
                    &format!("t-bulk-{:04}", overflow_count.saturating_sub(1)),
                )?
                .is_some(),
            "rows past the inline outbox bound must still commit"
        );
        let inline = store
            .read()
            .automation_triggers_for_commit(MAIN_WORLD, commit_seq)?;
        assert_eq!(inline.len(), MAX_CANDIDATES_PER_COMMIT);
        assert_eq!(
            store
                .read()
                .pending_automation_continuation_count(MAIN_WORLD)?,
            1
        );
        let expected_cursor = AutomationContinuationCursor::Row {
            root_type: "demo.Ticket".to_owned(),
            object_id: format!("t-bulk-{MAX_CANDIDATES_PER_COMMIT:04}"),
            ancestor_type: "demo.Ticket".to_owned(),
            after_automation_id: None,
        };
        let cont = store
            .read()
            .automation_outbox_continuation(commit_seq)?
            .ok_or("missing continuation row exists")?;
        assert_eq!(cont.trigger_commit_seq, commit_seq);
        assert_eq!(cont.world, MAIN_WORLD);
        assert_eq!(cont.cursor, expected_cursor);
        assert_eq!(cont.storage_version, crate::engine_release::STORAGE_VERSION);
        assert_eq!(cont.engine_version, current_engine_version());
        assert_eq!(cont.definitions_version, receipt.body.definitions_version);
        assert_eq!(cont.matcher_version, AUTOMATION_MATCHER_VERSION);
        assert_eq!(cont.recorded_at_ms, 200);
        assert_eq!(cont.completed_commit_seq, None);

        assert_overflow_replays(&mut store, &overflow_batch, &cont)?;
        drop(store);
        assert_overflow_survives_reopen_and_restore(&path, &model, commit_seq, cont).await?;
        Ok(())
    }

    fn assert_overflow_replays(
        store: &mut CellStore,
        overflow_batch: &CommitBatch,
        cont: &AutomationOutboxContinuation,
    ) -> TestResult {
        // 3. Idempotent on trigger_commit_seq: replay and duplicate insert keep the original row.
        assert!(matches!(
            store.commit(overflow_batch)?,
            CommitOutcome::Replayed(_)
        ));
        continuation::insert_continuation(
            store.read().conn,
            &AutomationOutboxContinuation {
                recorded_at_ms: 999,
                ..cont.clone()
            },
        )?;
        let pending_conts = store
            .read()
            .pending_automation_continuations(MAIN_WORLD, 10)?;
        assert_eq!(pending_conts.as_slice(), std::slice::from_ref(cont));
        Ok(())
    }

    async fn assert_overflow_survives_reopen_and_restore(
        path: &std::path::Path,
        model: &Model,
        commit_seq: u64,
        cont: AutomationOutboxContinuation,
    ) -> TestResult {
        // 4. Reopen durability: both inline candidates and continuation row survive restart.
        let reopened = CellStore::open(path, model.clone(), StoreOptions::default())?;
        assert_eq!(
            reopened
                .read()
                .pending_automation_trigger_count(MAIN_WORLD)?,
            u64::try_from(MAX_CANDIDATES_PER_COMMIT)?
        );
        assert_eq!(
            reopened.read().automation_outbox_continuation(commit_seq)?,
            Some(cont.clone())
        );

        let mut record = reopened
            .export_record(commit_seq)?
            .ok_or("missing overflow record")?;
        assert_eq!(record.automation_continuation.as_ref(), Some(&cont));

        // Represent an export written by a different build without needing a
        // second binary. The last record can be republished under its new hash.
        let mut original = cont;
        original.engine_version.push_str("-other-build");
        for matcher_version in [AUTOMATION_MATCHER_VERSION, u32::MAX] {
            original.matcher_version = matcher_version;
            record.automation_continuation = Some(original.clone());
            let mut exporter = crate::export::Exporter::new(std::sync::Arc::new(
                object_store::memory::InMemory::new(),
            ));
            for seq in 1..commit_seq {
                let prior = reopened.export_record(seq)?.ok_or("missing prior record")?;
                exporter.append(&prior).await?;
            }
            exporter.append(&record).await?;
            let target = path.with_file_name(format!("restored-{matcher_version}.sqlite"));
            let result = CellStore::restore_from(
                &mut exporter,
                &target,
                model.clone(),
                SCOPE,
                &record.bindings,
                StoreOptions::default(),
            )
            .await;
            if matcher_version == AUTOMATION_MATCHER_VERSION {
                result?;
                let restored = CellStore::open(&target, model.clone(), StoreOptions::default())?;
                assert_eq!(
                    restored
                        .read()
                        .automation_outbox_continuation(commit_seq)?
                        .as_ref(),
                    Some(&original)
                );
            } else {
                assert!(matches!(
                    result,
                    Err(crate::RestoreError::Cell(CellError::EngineUnknownVersion { found }))
                        if found == format!("automation matcher version {matcher_version}")
                ));
                assert!(!target.exists());
            }
        }
        Ok(())
    }

    fn assert_rule_pages(conn: &impl SqlEngine) -> TestResult {
        // Keyset paging with LIMIT 2 walks all 3 row rules cleanly without duplicates.
        let p1 = query_row_rules_page(conn, "demo.Ticket", None, 2)?;
        assert!(p1.has_more);
        let p1_ids: Vec<&str> = p1.rules.iter().map(|r| r.automation_id.as_str()).collect();
        assert_eq!(p1_ids, ["ops.r1", "ops.r2"]);
        let p2 = query_row_rules_page(conn, "demo.Ticket", Some("ops.r2"), 2)?;
        assert!(!p2.has_more);
        let p2_ids: Vec<&str> = p2.rules.iter().map(|r| r.automation_id.as_str()).collect();
        assert_eq!(p2_ids, ["ops.r3"]);

        // Keyset paging with LIMIT 1 walks both action rules cleanly.
        let a1 = query_action_rules_page(conn, "demo.ticket.ping", None, 1)?;
        assert!(a1.has_more);
        assert_eq!(a1.rules.len(), 1);
        assert_eq!(
            a1.rules.first().ok_or("missing action rule")?.automation_id,
            "ops.a1"
        );
        let a2 = query_action_rules_page(conn, "demo.ticket.ping", Some("ops.a1"), 1)?;
        assert!(!a2.has_more);
        assert_eq!(a2.rules.len(), 1);
        assert_eq!(
            a2.rules.first().ok_or("missing action rule")?.automation_id,
            "ops.a2"
        );
        Ok(())
    }

    /// S4: indexed rule lookups use `LIMIT` with keyset cursors, and a pause in
    /// the middle of a type's rule list records the exact `after_automation_id`
    /// keyset cursor for deterministic U2 resumption.
    #[test]
    fn keyset_rule_paging_and_mid_rule_list_continuation_cursor() -> TestResult {
        const { assert!(MAX_CANDIDATES_PER_COMMIT == 256) };
        let dir = tempfile::tempdir()?;
        let path = dir.path().join("keyset.sqlite");
        let (model, canonical) = sample_model()?;
        let mut store = CellStore::open(&path, model.clone(), StoreOptions::default())?;
        store.bind("scope", SCOPE)?;
        store.install_definitions(model.clone(), install_input(&canonical, 10), Some(0), SCOPE)?;
        for (idx, id) in ["ops.r1", "ops.r2", "ops.r3"].into_iter().enumerate() {
            install_automation_for(
                &mut store,
                &model,
                &canonical,
                1_u64.saturating_add(u64::try_from(idx)?),
                id,
                "demo.Ticket",
                &json!({ "kind": "event", "event": "created" }),
            )?;
        }
        install_automation_for(
            &mut store,
            &model,
            &canonical,
            4,
            "ops.a1",
            "demo.Ticket",
            &json!({ "kind": "event", "event": "action", "actionId": "demo.ticket.ping" }),
        )?;
        install_automation_for(
            &mut store,
            &model,
            &canonical,
            5,
            "ops.a2",
            "demo.Ticket",
            &json!({ "kind": "event", "event": "action", "actionId": "demo.ticket.ping" }),
        )?;

        assert_rule_pages(store.read().conn)?;

        // 86 rows × 3 rules = 258 matches: 85*3 = 255 on t-0000..t-0084 plus
        // ops.r1 on t-0085 hits 256 inline; ops.r2 on t-0085 pauses mid-rule-list.
        let writes: Vec<FactWriteRow> = (0..86_usize)
            .map(|idx| FactWriteRow::Put {
                object_type: "demo.Ticket".to_owned(),
                key: format!("t-{idx:04}"),
                values: BTreeMap::from([
                    ("status".to_owned(), Value::String("open".into())),
                    ("score".to_owned(), Value::from(1_i64)),
                ]),
            })
            .collect();
        let batch = CommitBatch {
            world: MAIN_WORLD.to_owned(),
            scope: SCOPE.to_owned(),
            logical_time: 300,
            idempotency_key: Some("mid-rule-pause".to_owned()),
            writes,
            ..CommitBatch::default()
        };
        let CommitOutcome::Committed(receipt) = store.commit(&batch)? else {
            return Err("expected commit".into());
        };
        assert_eq!(
            store
                .read()
                .automation_triggers_for_commit(MAIN_WORLD, receipt.body.commit_seq)?
                .len(),
            MAX_CANDIDATES_PER_COMMIT
        );
        let cont = store
            .read()
            .automation_outbox_continuation(receipt.body.commit_seq)?
            .ok_or("missing continuation exists")?;
        assert_eq!(
            cont.cursor,
            AutomationContinuationCursor::Row {
                root_type: "demo.Ticket".to_owned(),
                object_id: "t-0085".to_owned(),
                ancestor_type: "demo.Ticket".to_owned(),
                after_automation_id: Some("ops.r1".to_owned()),
            }
        );
        Ok(())
    }

    fn seed_ticket_a(store: &mut CellStore) -> TestResult {
        // Seed live `demo.TicketA` under key `s-1`.
        let create_a = CommitBatch {
            world: MAIN_WORLD.to_owned(),
            scope: SCOPE.to_owned(),
            logical_time: 100,
            idempotency_key: Some("create-a".to_owned()),
            writes: vec![FactWriteRow::Put {
                object_type: "demo.TicketA".to_owned(),
                key: "s-1".to_owned(),
                values: BTreeMap::from([
                    ("status".to_owned(), Value::String("open".into())),
                    ("score".to_owned(), Value::from(10_i64)),
                    ("a_tag".to_owned(), Value::String("from-a".into())),
                ]),
            }],
            ..CommitBatch::default()
        };
        store.commit(&create_a)?;
        assert_eq!(
            store.read().pending_automation_trigger_count(MAIN_WORLD)?,
            0
        );
        Ok(())
    }

    /// S5: replacing a live object with a sibling subtype in one commit fires
    /// as `event:created` (with `before_values = None`) for a rule watching the
    /// new subtype, and as `event:updated` for a rule watching their common ancestor.
    #[test]
    fn sibling_replacement_fires_create_with_no_before_state_for_new_subtype() -> TestResult {
        let dir = tempfile::tempdir()?;
        let path = dir.path().join("sibling.sqlite");
        let (model, canonical) = sample_model()?;
        let mut store = CellStore::open(&path, model.clone(), StoreOptions::default())?;
        store.bind("scope", SCOPE)?;
        store.install_definitions(model.clone(), install_input(&canonical, 10), Some(0), SCOPE)?;
        install_automation_for(
            &mut store,
            &model,
            &canonical,
            1,
            "ops.b_created",
            "demo.TicketB",
            &json!({ "kind": "event", "event": "created" }),
        )?;
        install_automation_for(
            &mut store,
            &model,
            &canonical,
            2,
            "ops.b_updated",
            "demo.TicketB",
            &json!({ "kind": "event", "event": "updated" }),
        )?;
        install_automation_for(
            &mut store,
            &model,
            &canonical,
            3,
            "ops.base_updated",
            "demo.Ticket",
            &json!({ "kind": "event", "event": "updated" }),
        )?;

        seed_ticket_a(&mut store)?;

        // Replace `demo.TicketA` with sibling `demo.TicketB` in one commit.
        let replace_with_b = CommitBatch {
            world: MAIN_WORLD.to_owned(),
            scope: SCOPE.to_owned(),
            logical_time: 200,
            idempotency_key: Some("replace-b".to_owned()),
            writes: vec![
                FactWriteRow::Delete {
                    object_type: "demo.TicketA".to_owned(),
                    key: "s-1".to_owned(),
                },
                FactWriteRow::Put {
                    object_type: "demo.TicketB".to_owned(),
                    key: "s-1".to_owned(),
                    values: BTreeMap::from([
                        ("status".to_owned(), Value::String("open".into())),
                        ("score".to_owned(), Value::from(25_i64)),
                        ("b_tag".to_owned(), Value::String("from-b".into())),
                    ]),
                },
            ],
            ..CommitBatch::default()
        };
        let CommitOutcome::Committed(receipt) = store.commit(&replace_with_b)? else {
            return Err("expected commit".into());
        };
        let triggers = store
            .read()
            .automation_triggers_for_commit(MAIN_WORLD, receipt.body.commit_seq)?;
        assert_eq!(triggers.len(), 2, "{triggers:?}");

        // Common ancestor `demo.Ticket` sees an update with TicketA before and TicketB after.
        let base_update = triggers
            .iter()
            .find(|trigger| trigger.key.automation_id == "ops.base_updated")
            .ok_or("missing ancestor update")?;
        assert_eq!(base_update.trigger_kind, KIND_UPDATED);
        assert_eq!(
            base_update
                .before_values
                .as_ref()
                .and_then(|values| values.get("a_tag")),
            Some(&Value::String("from-a".into()))
        );
        assert_eq!(
            base_update
                .after_values
                .as_ref()
                .and_then(|values| values.get("b_tag")),
            Some(&Value::String("from-b".into()))
        );

        // New sibling `demo.TicketB` fires `event:created` (NOT `event:updated`) with no before-state.
        let sibling_create = triggers
            .iter()
            .find(|trigger| trigger.key.automation_id == "ops.b_created")
            .ok_or("missing sibling creation")?;
        assert_eq!(sibling_create.trigger_kind, KIND_CREATED);
        assert_eq!(sibling_create.before_values, None);
        assert_eq!(
            sibling_create
                .after_values
                .as_ref()
                .and_then(|values| values.get("b_tag")),
            Some(&Value::String("from-b".into()))
        );
        Ok(())
    }
}
