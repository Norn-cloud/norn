//! AUTOMATION-OUTBOX-1 (Core 04): transactional automation trigger outbox and
//! indexed trigger projection.
//!
//! Matching automation trigger candidates are recorded inside the same `SQLite`
//! transaction as the triggering write (direct action calls, bound flow calls,
//! approved proposals, or raw fact writes). Active, enabled, authority-bound
//! write and action triggers are projected transactionally into
//! `_norn_retained_automation_trigger` inside `SqlEngine::definition_writes`.
//! The commit path probes only touched root hierarchies and committed
//! `norn.ActionCall` ids via indexed lookups, skips snapshot reads and JSON
//! serialization when no rule matches, and bounds fan-out by
//! [`MAX_CANDIDATES_PER_COMMIT`] (refusing and rolling back on overflow).
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
//! ## Retirement & U2 authority obligations (`Core 05 AUTH-ATTENUATE-1`)
//! Retiring an automation deletes its trigger-index row and marks its pending
//! outbox candidates void (`voided_commit_seq = commit_seq`) in the same
//! transaction. No trigger actor grant is stored on candidates; U2 must require
//! the candidate's revision to remain the current active revision and
//! reauthorize the author's live grant (`parent_grant_ids` + `author`) and
//! concrete target row under the dispatch hold.

use std::collections::{BTreeMap, BTreeSet};

use norn_ir::{FactChange, RuntimeValue as Value};
use serde::{Deserialize, Serialize};

use crate::Model;
use crate::codec::canonical_json;
use crate::engine::{SqlEngine, SqlRow, params};
use crate::error::CellError;
use crate::read::{ObjectRecord, StoreReader, to_i64, to_u64};
use crate::retained::{RetainedDefinition, RetainedLifecycle};
use crate::store::{CommitBatch, FactWriteRow, WrittenRow};

/// The transactional trigger outbox table.
pub(crate) const AUTOMATION_OUTBOX: &str = "_norn_automation_outbox";

/// Indexed projection of active, enabled, authority-bound write and action
/// automation triggers, maintained transactionally on install, revision, and
/// retire. Prefixed `_norn_retained_` so `NativeEngine` S1 write-protects it
/// outside `SqlEngine::definition_writes`.
pub(crate) const AUTOMATION_TRIGGER_INDEX: &str = "_norn_retained_automation_trigger";

/// Maximum automation trigger candidates one commit may enqueue. Exceeding
/// this bound refuses and rolls back the entire commit rather than dropping
/// candidates.
pub const MAX_CANDIDATES_PER_COMMIT: usize = 256;

/// Trigger kind strings persisted in the outbox and trigger index.
const KIND_CREATED: &str = "event:created";
const KIND_UPDATED: &str = "event:updated";
const KIND_ACTION: &str = "event:action";
const KIND_LIFECYCLE_ENTERED: &str = "lifecycle_entered";

/// The outbox and trigger-projection DDL, applied with the other system tables
/// (purely additive).
pub(crate) const DDL: [&str; 7] = [
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
    r#"CREATE INDEX IF NOT EXISTS "_norn_retained_automation_trigger/type" ON "_norn_retained_automation_trigger" ("object_type", "trigger_kind", "automation_id")"#,
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

/// Write- or action-triggered matcher projected from an active `automation`.
#[derive(Debug, Clone, PartialEq, Eq)]
enum TriggerMatcher {
    Created,
    Updated,
    Action { action_id: String },
    LifecycleEntered { property: String, state: String },
}

impl TriggerMatcher {
    const fn kind_label(&self) -> &'static str {
        match self {
            Self::Created => KIND_CREATED,
            Self::Updated => KIND_UPDATED,
            Self::Action { .. } => KIND_ACTION,
            Self::LifecycleEntered { .. } => KIND_LIFECYCLE_ENTERED,
        }
    }
}

#[derive(Debug, Clone)]
struct WatchedRule {
    automation_id: String,
    revision: u64,
    object_type: String,
    matcher: TriggerMatcher,
}

/// Pre-commit trigger probe results and pre-write object snapshots captured
/// before `Writer::apply_writes` mutates any rows.
#[derive(Default)]
pub(crate) struct AutomationWatch {
    watched_row_roots: BTreeSet<String>,
    action_rules: BTreeMap<String, Vec<WatchedRule>>,
    before: BTreeMap<(String, String), Option<ObjectRecord>>,
}

impl AutomationWatch {
    fn is_empty(&self) -> bool {
        self.watched_row_roots.is_empty() && self.action_rules.is_empty()
    }
}

fn parse_watched_rule(automation_id: &str, revision: u64, payload: &str) -> Option<WatchedRule> {
    let value: serde_json::Value = serde_json::from_str(payload).ok()?;
    if value.get("enabled").and_then(serde_json::Value::as_bool) != Some(true) {
        return None;
    }
    let object_type = value.get("objectType")?.as_str()?.to_owned();
    let when = value.get("when")?.as_object()?;
    let field = |k: &str| when.get(k).and_then(serde_json::Value::as_str);
    let matcher = match field("kind")? {
        "event" => match field("event")? {
            "created" => TriggerMatcher::Created,
            "updated" => TriggerMatcher::Updated,
            "action" => TriggerMatcher::Action {
                action_id: field("actionId")?.to_owned(),
            },
            _ => return None,
        },
        "lifecycle_entered" => TriggerMatcher::LifecycleEntered {
            property: field("property")?.to_owned(),
            state: field("state")?.to_owned(),
        },
        // `stale` triggers are timer-driven (U4), never enqueued on write commits.
        _ => return None,
    };
    Some(WatchedRule {
        automation_id: automation_id.to_owned(),
        revision,
        object_type,
        matcher,
    })
}

/// Maintain `_norn_retained_automation_trigger` transactionally on install,
/// revision, and retire. Must be called inside `SqlEngine::definition_writes`.
pub(crate) fn sync_trigger_index<E: SqlEngine>(
    conn: &E,
    model: &Model,
    definition: &RetainedDefinition,
) -> Result<(), CellError> {
    conn.execute(
        &format!("DELETE FROM \"{AUTOMATION_TRIGGER_INDEX}\" WHERE \"automation_id\" = ?1"),
        params![definition.id],
    )?;
    // Core 05 AUTH-ATTENUATE-1: index only active automations with a server-bound revocable grant.
    let authorized = definition.lifecycle == RetainedLifecycle::Active
        && definition
            .authority
            .as_ref()
            .is_some_and(|a| a.revocable && !a.parent_grant_ids.is_empty());
    let Some(rule) = authorized
        .then(|| parse_watched_rule(&definition.id, definition.revision, &definition.payload))
        .flatten()
    else {
        return Ok(());
    };
    let root_type = model
        .chain(&rule.object_type)
        .ok()
        .and_then(|chain| chain.first().map(String::as_str))
        .unwrap_or(&rule.object_type);
    let (action_id, lifecycle_property, lifecycle_state) = match &rule.matcher {
        TriggerMatcher::Created | TriggerMatcher::Updated => (None, None, None),
        TriggerMatcher::Action { action_id } => (Some(action_id.as_str()), None, None),
        TriggerMatcher::LifecycleEntered { property, state } => {
            (None, Some(property.as_str()), Some(state.as_str()))
        }
    };
    conn.execute(
        &format!(
            "INSERT INTO \"{AUTOMATION_TRIGGER_INDEX}\" (\
              \"automation_id\", \"revision\", \"object_type\", \"root_type\", \
              \"trigger_kind\", \"action_id\", \"lifecycle_property\", \"lifecycle_state\"\
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)"
        ),
        params![
            rule.automation_id,
            to_i64(rule.revision, "automation revision")?,
            rule.object_type,
            root_type,
            rule.matcher.kind_label(),
            action_id,
            lifecycle_property,
            lifecycle_state,
        ],
    )
}

/// Mark every pending outbox candidate for any retired automation in `changes`
/// as void in the same transaction as `:retire`.
pub(crate) fn void_retired<'a, E: SqlEngine>(
    conn: &E,
    changes: impl IntoIterator<Item = &'a crate::retained::RetainedDefinitionChange>,
    commit_seq: u64,
) -> Result<(), CellError> {
    let sql = format!(
        "UPDATE \"{AUTOMATION_OUTBOX}\" SET \"voided_commit_seq\" = ?1 \
         WHERE \"automation_id\" = ?2 AND \"revision\" <= ?3 \
         AND \"consumed_commit_seq\" IS NULL AND \"voided_commit_seq\" IS NULL"
    );
    for change in changes {
        if change.definition.kind == "automation"
            && change.definition.lifecycle == RetainedLifecycle::Retired
        {
            conn.execute(
                &sql,
                params![
                    to_i64(commit_seq, "voided commit seq")?,
                    change.definition.id,
                    to_i64(change.definition.revision, "retiring revision")?,
                ],
            )?;
        }
    }
    Ok(())
}

const INDEX_COLUMNS: &str = "\"automation_id\", \"revision\", \"object_type\", \"trigger_kind\", \"action_id\", \"lifecycle_property\", \"lifecycle_state\"";

fn decode_indexed_rule(values: Vec<crate::engine::SqlValue>) -> Result<WatchedRule, CellError> {
    let row = SqlRow(values);
    let automation_id: String = row.get(0)?;
    let revision = to_u64(row.get(1)?, "automation revision")?;
    let object_type: String = row.get(2)?;
    let trigger_kind: String = row.get(3)?;
    let (action_id, prop, state): (Option<String>, Option<String>, Option<String>) =
        (row.get(4)?, row.get(5)?, row.get(6)?);
    let missing = |col: &str| CellError::Engine(format!("indexed trigger missing {col}"));
    let matcher = match trigger_kind.as_str() {
        KIND_CREATED => TriggerMatcher::Created,
        KIND_UPDATED => TriggerMatcher::Updated,
        KIND_ACTION => TriggerMatcher::Action {
            action_id: action_id.ok_or_else(|| missing("action_id"))?,
        },
        KIND_LIFECYCLE_ENTERED => TriggerMatcher::LifecycleEntered {
            property: prop.ok_or_else(|| missing("lifecycle_property"))?,
            state: state.ok_or_else(|| missing("lifecycle_state"))?,
        },
        other => {
            return Err(CellError::Engine(format!(
                "indexed automation trigger has unknown kind {other:?}"
            )));
        }
    };
    Ok(WatchedRule {
        automation_id,
        revision,
        object_type,
        matcher,
    })
}

fn has_row_rules_for_root<E: SqlEngine>(conn: &E, root_type: &str) -> Result<bool, CellError> {
    let found: Option<i64> = conn.query_optional(
        &format!(
            "SELECT 1 FROM \"{AUTOMATION_TRIGGER_INDEX}\" \
             WHERE \"root_type\" = ?1 AND \"trigger_kind\" != '{KIND_ACTION}' LIMIT 1"
        ),
        params![root_type],
        |row| row.get(0),
    )?;
    Ok(found.is_some())
}

fn query_action_rules<E: SqlEngine>(
    conn: &E,
    action_id: &str,
) -> Result<Vec<WatchedRule>, CellError> {
    let rows = conn.query(
        &format!(
            "SELECT {INDEX_COLUMNS} FROM \"{AUTOMATION_TRIGGER_INDEX}\" \
             WHERE \"trigger_kind\" = '{KIND_ACTION}' AND \"action_id\" = ?1 \
             ORDER BY \"automation_id\""
        ),
        params![action_id],
    )?;
    rows.into_iter().map(decode_indexed_rule).collect()
}

fn query_row_rules_for_lineage<E: SqlEngine>(
    conn: &E,
    model: &Model,
    concrete_type: &str,
) -> Result<Vec<WatchedRule>, CellError> {
    let mut rules = Vec::new();
    for ancestor in model.chain(concrete_type)? {
        let rows = conn.query(
            &format!(
                "SELECT {INDEX_COLUMNS} FROM \"{AUTOMATION_TRIGGER_INDEX}\" \
                 WHERE \"object_type\" = ?1 AND \"trigger_kind\" != '{KIND_ACTION}' \
                 ORDER BY \"automation_id\""
            ),
            params![ancestor],
        )?;
        for values in rows {
            rules.push(decode_indexed_rule(values)?);
        }
    }
    rules.sort_by(|left, right| left.automation_id.cmp(&right.automation_id));
    Ok(rules)
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
) -> Vec<ActionCallMeta> {
    let mut calls = Vec::new();
    for write in &batch.writes {
        if let FactWriteRow::Put {
            object_type,
            values,
            ..
        } = write
            && object_type == norn_kernel::ACTION_CALL_TYPE
            && let Some(meta) = extract_action_call(reader, values)
        {
            calls.push(meta);
        }
    }
    for fact in &batch.facts {
        match fact {
            FactChange::Create {
                object_type,
                values,
                ..
            }
            | FactChange::Set {
                object_type,
                values,
                ..
            } if object_type.as_str() == norn_kernel::ACTION_CALL_TYPE => {
                if let Some(meta) = extract_action_call(reader, values) {
                    calls.push(meta);
                }
            }
            FactChange::Create { .. }
            | FactChange::Set { .. }
            | FactChange::Delete { .. }
            | FactChange::Transition { .. } => {}
        }
    }
    calls
}

/// Capture pre-commit state for objects touched by `batch` via indexed probes.
/// Returns immediately with zero object reads when no rule matches.
pub(crate) fn capture_before<E: SqlEngine>(
    reader: &StoreReader<'_, E>,
    batch: &CommitBatch,
) -> Result<AutomationWatch, CellError> {
    if batch.writes.is_empty() && batch.facts.is_empty() {
        return Ok(AutomationWatch::default());
    }
    let model = reader.model();
    let action_calls = action_calls_in_batch(reader, batch);
    let mut action_rules: BTreeMap<String, Vec<WatchedRule>> = BTreeMap::new();
    for call in &action_calls {
        if !action_rules.contains_key(&call.action_id) {
            let rules = query_action_rules(reader.conn, &call.action_id)?;
            if !rules.is_empty() {
                action_rules.insert(call.action_id.clone(), rules);
            }
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
    if watched_row_roots.is_empty() && action_rules.is_empty() {
        return Ok(AutomationWatch::default());
    }
    let mut before = BTreeMap::new();
    for (ty, key) in touched {
        let root = reader.root_type(ty)?;
        if watched_row_roots.contains(root) {
            capture_root_snapshot(reader, &batch.world, root, key, &mut before)?;
        }
    }
    for call in &action_calls {
        if action_rules.contains_key(&call.action_id) && model.has_type(&call.target_type) {
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
        action_rules,
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

fn emit_candidate<E: SqlEngine>(
    reader: &StoreReader<'_, E>,
    rule: &WatchedRule,
    ctx: &CandidateEmit<'_>,
    before_lazy: &mut LazySnapshot<'_>,
    after_lazy: &mut LazySnapshot<'_>,
    emitted: &mut usize,
) -> Result<(), CellError> {
    *emitted = emitted.saturating_add(1);
    if *emitted > MAX_CANDIDATES_PER_COMMIT {
        return Err(CellError::AutomationFanoutExceeded {
            limit: MAX_CANDIDATES_PER_COMMIT,
            actual: *emitted,
        });
    }
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
/// driven from committed `norn.ActionCall` records). Refuses and rolls back
/// with [`CellError::AutomationFanoutExceeded`] past [`MAX_CANDIDATES_PER_COMMIT`].
pub(crate) fn record<E: SqlEngine>(
    reader: &StoreReader<'_, E>,
    batch: &CommitBatch,
    written: &[WrittenRow],
    commit_seq: u64,
    watch: &AutomationWatch,
) -> Result<(), CellError> {
    if watch.is_empty() {
        return Ok(());
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
    let mut emitted: usize = 0;
    if !watch.watched_row_roots.is_empty() {
        record_row_triggers(
            reader,
            batch,
            commit_seq,
            watch,
            &final_rows,
            &action_calls,
            &mut emitted,
        )?;
    }
    if !watch.action_rules.is_empty() {
        record_action_triggers(
            reader,
            batch,
            commit_seq,
            watch,
            &final_rows,
            &action_calls,
            &mut emitted,
        )?;
    }
    Ok(())
}

fn record_row_triggers<E: SqlEngine>(
    reader: &StoreReader<'_, E>,
    batch: &CommitBatch,
    commit_seq: u64,
    watch: &AutomationWatch,
    final_rows: &BTreeMap<(String, String), &WrittenRow>,
    action_calls: &[ActionCallMeta],
    emitted: &mut usize,
) -> Result<(), CellError> {
    let model = reader.model();
    let mut rules_by_concrete: BTreeMap<String, Vec<WatchedRule>> = BTreeMap::new();
    for (target, row) in final_rows {
        let (root, key) = target;
        if !watch.watched_row_roots.contains(root) {
            continue;
        }
        if !rules_by_concrete.contains_key(&row.object_type) {
            let loaded = query_row_rules_for_lineage(reader.conn, model, &row.object_type)?;
            rules_by_concrete.insert(row.object_type.clone(), loaded);
        }
        let Some(rules) = rules_by_concrete
            .get(&row.object_type)
            .filter(|r| !r.is_empty())
        else {
            continue;
        };
        let before_record = watch.before.get(target).and_then(Option::as_ref);
        let after_record = reader.object(&batch.world, root, key)?;
        let primary_action = action_calls
            .iter()
            .find(|call| call.target_root == *root && call.target_key == *key)
            .map(|call| call.action_id.as_str());
        let mut before_lazy = LazySnapshot::new(before_record);
        let mut after_lazy = LazySnapshot::new(after_record.as_ref());
        let ctx = CandidateEmit {
            world: &batch.world,
            commit_seq,
            recorded_at_ms: batch.logical_time,
            object_type: &row.object_type,
            object_id: &row.key,
            object_version: row.version,
            action_id: primary_action,
        };
        for rule in rules {
            let rule_before =
                before_record.filter(|rec| model.is_a(&rec.object_type, &rule.object_type));
            let rule_after = after_record
                .as_ref()
                .filter(|rec| model.is_a(&rec.object_type, &rule.object_type));
            if row_rule_matches(&rule.matcher, rule_before, rule_after) {
                emit_candidate(
                    reader,
                    rule,
                    &ctx,
                    &mut before_lazy,
                    &mut after_lazy,
                    emitted,
                )?;
            }
        }
    }
    Ok(())
}

fn record_action_triggers<E: SqlEngine>(
    reader: &StoreReader<'_, E>,
    batch: &CommitBatch,
    commit_seq: u64,
    watch: &AutomationWatch,
    final_rows: &BTreeMap<(String, String), &WrittenRow>,
    action_calls: &[ActionCallMeta],
    emitted: &mut usize,
) -> Result<(), CellError> {
    let model = reader.model();
    for call in action_calls {
        let Some(rules) = watch.action_rules.get(&call.action_id) else {
            continue;
        };
        let target_key = (call.target_root.clone(), call.target_key.clone());
        let before_record = watch.before.get(&target_key).and_then(Option::as_ref);
        let after_record = if model.has_type(&call.target_type) {
            reader.object(&batch.world, &call.target_root, &call.target_key)?
        } else {
            None
        };
        let written_row = final_rows.get(&target_key).copied();
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
            reader.version(&batch.world, effective_object_type, &call.target_key)?
        } else {
            0
        };
        let mut before_lazy = LazySnapshot::new(before_record);
        let mut after_lazy = LazySnapshot::new(after_record.as_ref());
        let ctx = CandidateEmit {
            world: &batch.world,
            commit_seq,
            recorded_at_ms: batch.logical_time,
            object_type: effective_object_type,
            object_id: &call.target_key,
            object_version,
            action_id: Some(call.action_id.as_str()),
        };
        for rule in rules {
            if model.is_a(effective_object_type, &rule.object_type) {
                emit_candidate(
                    reader,
                    rule,
                    &ctx,
                    &mut before_lazy,
                    &mut after_lazy,
                    emitted,
                )?;
            }
        }
    }
    Ok(())
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

const SELECT_COLUMNS: &str = "\"world\", \"automation_id\", \"revision\", \"trigger_commit_seq\", \"object_type\", \"object_id\", \"object_version\", \"trigger_kind\", \"action_id\", \"before_values\", \"after_values\", \"recorded_at_ms\", \"consumed_commit_seq\", \"voided_commit_seq\"";

fn decode_snapshot(text: Option<String>) -> Result<Option<BTreeMap<String, Value>>, CellError> {
    text.map(|raw| {
        match Value::from_json_text(&raw)
            .map_err(|e| CellError::Encoding(format!("automation outbox snapshot: {e}")))?
        {
            Value::Object(map) => Ok(map),
            Value::Null
            | Value::Bool(_)
            | Value::Number(_)
            | Value::String(_)
            | Value::Array(_) => Err(CellError::Encoding(
                "automation outbox snapshot must be a JSON object".to_owned(),
            )),
        }
    })
    .transpose()
}

fn decode_candidate(
    values: Vec<crate::engine::SqlValue>,
) -> Result<AutomationTriggerCandidate, CellError> {
    let row = SqlRow(values);
    let opt_seq = |idx: usize, label: &'static str| -> Result<Option<u64>, CellError> {
        row.get::<Option<i64>>(idx)?
            .map(|seq| to_u64(seq, label))
            .transpose()
    };
    Ok(AutomationTriggerCandidate {
        world: row.get(0)?,
        key: AutomationTriggerKey {
            automation_id: row.get(1)?,
            revision: to_u64(row.get(2)?, "automation revision")?,
            trigger_commit_seq: to_u64(row.get(3)?, "trigger commit seq")?,
            object_type: row.get(4)?,
            object_id: row.get(5)?,
            object_version: to_u64(row.get(6)?, "object version")?,
        },
        trigger_kind: row.get(7)?,
        action_id: row.get(8)?,
        before_values: decode_snapshot(row.get(9)?)?,
        after_values: decode_snapshot(row.get(10)?)?,
        recorded_at_ms: to_u64(row.get(11)?, "trigger recorded_at_ms")?,
        consumed_commit_seq: opt_seq(12, "consumed commit seq")?,
        voided_commit_seq: opt_seq(13, "voided commit seq")?,
    })
}

impl<E: SqlEngine> StoreReader<'_, E> {
    /// Pending (unconsumed, unvoided) automation trigger candidates in `world`,
    /// ordered deterministically by `(trigger_commit_seq, automation_id,
    /// revision, object_type, object_id, object_version)`, up to `limit`.
    ///
    /// # Errors
    /// SQL engine or decode errors.
    pub fn pending_automation_triggers(
        &self,
        world: &str,
        limit: usize,
    ) -> Result<Vec<AutomationTriggerCandidate>, CellError> {
        let rows = self.conn.query(
            &format!(
                "SELECT {SELECT_COLUMNS} FROM \"{AUTOMATION_OUTBOX}\" \
                 WHERE \"world\" = ?1 AND \"consumed_commit_seq\" IS NULL AND \"voided_commit_seq\" IS NULL \
                 ORDER BY \"trigger_commit_seq\", \"automation_id\", \"revision\", \"object_type\", \"object_id\", \"object_version\" \
                 LIMIT ?2"
            ),
            params![world, i64::try_from(limit).unwrap_or(i64::MAX)],
        )?;
        rows.into_iter().map(decode_candidate).collect()
    }

    /// All automation trigger candidates recorded by commit `commit_seq` in
    /// `world` (including consumed and voided rows for audit inspection).
    ///
    /// # Errors
    /// SQL engine or decode errors.
    pub fn automation_triggers_for_commit(
        &self,
        world: &str,
        commit_seq: u64,
    ) -> Result<Vec<AutomationTriggerCandidate>, CellError> {
        let rows = self.conn.query(
            &format!(
                "SELECT {SELECT_COLUMNS} FROM \"{AUTOMATION_OUTBOX}\" \
                 WHERE \"world\" = ?1 AND \"trigger_commit_seq\" = ?2 \
                 ORDER BY \"automation_id\", \"revision\", \"object_type\", \"object_id\", \"object_version\""
            ),
            params![world, to_i64(commit_seq, "trigger commit seq")?],
        )?;
        rows.into_iter().map(decode_candidate).collect()
    }

    /// Total number of pending (unconsumed, unvoided) automation trigger
    /// candidates in `world`.
    ///
    /// # Errors
    /// SQL engine or count range errors.
    pub fn pending_automation_trigger_count(&self, world: &str) -> Result<u64, CellError> {
        let count: i64 = self.conn.query_row(
            &format!(
                "SELECT COUNT(*) FROM \"{AUTOMATION_OUTBOX}\" \
                 WHERE \"world\" = ?1 AND \"consumed_commit_seq\" IS NULL AND \"voided_commit_seq\" IS NULL"
            ),
            params![world],
            |row| row.get(0),
        )?;
        to_u64(count, "automation trigger count")
    }
}

#[cfg(all(test, feature = "native"))]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    reason = "transactional trigger outbox unit tests"
)]
mod tests {
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
    use norn_ir::ObjectTypeDef;
    use serde_json::json;

    const SCOPE: &str = "org_demo/dev";

    fn sample_model() -> (Model, String) {
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
                { "id": "aTag", "type": { "kind": "text" }, "default": "a" }
            ]
        });
        let ticket_b = json!({
            "name": "demo.TicketB",
            "extends": "demo.Ticket",
            "properties": [
                { "id": "bTag", "type": { "kind": "text" }, "default": "b" }
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
            .map(|raw| serde_json::from_value(raw.clone()).expect("type"))
            .collect();
        let model = Model::from_definitions(&defs, &[]).expect("model");
        let artifacts = json!([
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
        ]);
        let canonical = String::from_utf8(
            Value::from_json(artifacts)
                .expect("exact")
                .canonical_json()
                .expect("canonical"),
        )
        .expect("utf8");
        (model, canonical)
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
        when: serde_json::Value,
    ) {
        let payload = serde_json::to_string(&json!({
            "title": id,
            "enabled": true,
            "objectType": object_type,
            "when": when,
            "then": {
                "actionId": "demo.ticket.escalate",
                "mode": "auto"
            }
        }))
        .expect("payload");
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
        store
            .install_retained_definition(
                model.clone(),
                &install_input(canonical_set, 20),
                Some(expected_defs_version),
                SCOPE,
                &change,
            )
            .expect("install automation");
    }

    fn retire_automation(
        store: &mut CellStore,
        model: &Model,
        canonical_set: &str,
        expected_defs_version: u64,
        id: &str,
        current_revision: u64,
    ) {
        let current = store
            .read()
            .retained_definition(id, Some(current_revision))
            .expect("read")
            .expect("exists");
        let mut definition = current.definition;
        definition.revision = current_revision.saturating_add(1);
        definition.lifecycle = RetainedLifecycle::Retired;
        definition.signoff = None;
        definition.authority = None;
        let change = RetainedDefinitionChange {
            definition,
            expected_revision: Some(current_revision),
        };
        store
            .install_retained_definition(
                model.clone(),
                &install_input(canonical_set, 30),
                Some(expected_defs_version),
                SCOPE,
                &change,
            )
            .expect("retire automation");
    }

    fn open_with_ticket_automations(path: &std::path::Path) -> (CellStore, Model, String) {
        let (model, canonical) = sample_model();
        let mut store =
            CellStore::open(path, model.clone(), StoreOptions::default()).expect("open");
        store.bind("scope", SCOPE).expect("bind");
        store
            .install_definitions(model.clone(), install_input(&canonical, 10), Some(0), SCOPE)
            .expect("kernel defs");
        install_automation_for(
            &mut store,
            &model,
            &canonical,
            1,
            "ops.on_create",
            "demo.Ticket",
            json!({ "kind": "event", "event": "created" }),
        );
        install_automation_for(
            &mut store,
            &model,
            &canonical,
            2,
            "ops.on_stalled",
            "demo.Ticket",
            json!({ "kind": "lifecycle_entered", "property": "status", "state": "stalled" }),
        );
        (store, model, canonical)
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
    fn rolls_back_before_commit_and_survives_crash_after_commit() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("cell.sqlite");
        let (mut store, model, _canonical) = open_with_ticket_automations(&path);

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
            store
                .read()
                .pending_automation_trigger_count(MAIN_WORLD)
                .expect("count"),
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

        let reopened = CellStore::open(&path, model, StoreOptions::default()).expect("reopen");
        let pending = reopened
            .read()
            .pending_automation_triggers(MAIN_WORLD, 10)
            .expect("pending");
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
    }

    #[test]
    fn replay_dedupes_retire_voids_pending_and_lifecycle_captures_before_and_after() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("cell.sqlite");
        let (mut store, model, canonical) = open_with_ticket_automations(&path);

        let create_batch = ticket_batch("create-t1", 100, "open");
        let CommitOutcome::Committed(create_receipt) = store.commit(&create_batch).expect("create")
        else {
            panic!("expected committed receipt");
        };
        let create_seq = create_receipt.body.commit_seq;

        let replayed = store.commit(&create_batch).expect("replay");
        assert!(matches!(replayed, CommitOutcome::Replayed(_)));
        assert_eq!(
            store
                .read()
                .pending_automation_trigger_count(MAIN_WORLD)
                .expect("count"),
            1
        );

        let pending = store
            .read()
            .pending_automation_triggers(MAIN_WORLD, 10)
            .expect("pending");
        let dup_key = &pending[0].key;
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
        )
        .expect("duplicate insert ignored");
        assert_eq!(
            store
                .read()
                .pending_automation_trigger_count(MAIN_WORLD)
                .expect("count"),
            1
        );

        // Retiring `ops.on_create` atomically voids its pending candidates and removes its index row.
        retire_automation(&mut store, &model, &canonical, 3, "ops.on_create", 1);
        assert_eq!(
            store
                .read()
                .pending_automation_trigger_count(MAIN_WORLD)
                .expect("count"),
            0,
            "retire must void pending candidates in the same transaction"
        );
        let audited = store
            .read()
            .automation_triggers_for_commit(MAIN_WORLD, create_seq)
            .expect("audit");
        assert_eq!(audited.len(), 1);
        assert!(audited[0].voided_commit_seq.is_some());

        let stall_batch = ticket_batch("stall-t1", 200, "stalled");
        let CommitOutcome::Committed(receipt) = store.commit(&stall_batch).expect("stall") else {
            panic!("expected committed receipt");
        };
        let for_commit = store
            .read()
            .automation_triggers_for_commit(MAIN_WORLD, receipt.body.commit_seq)
            .expect("commit triggers");
        assert_eq!(for_commit.len(), 1);
        assert_eq!(for_commit[0].key.automation_id, "ops.on_stalled");
        assert_eq!(for_commit[0].key.object_version, 2);
        assert_eq!(for_commit[0].trigger_kind, KIND_LIFECYCLE_ENTERED);
        assert_eq!(
            for_commit[0]
                .before_values
                .as_ref()
                .and_then(|values| values.get("status")),
            Some(&Value::String("open".into()))
        );
        assert_eq!(
            for_commit[0]
                .after_values
                .as_ref()
                .and_then(|values| values.get("status")),
            Some(&Value::String("stalled".into()))
        );
    }

    /// S3: `event:action` is driven from the committed `norn.ActionCall` record:
    /// covers receipt-only calls with no row write, unchanged targets (`before == after`),
    /// and actions that delete their target (`after_values == None`, `before_values == Some(...)`).
    #[test]
    fn action_triggers_cover_receipt_only_unchanged_and_deleted_targets() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("action-triggers.sqlite");
        let (model, canonical) = sample_model();
        let mut store =
            CellStore::open(&path, model.clone(), StoreOptions::default()).expect("open");
        store.bind("scope", SCOPE).expect("bind");
        store
            .install_definitions(model.clone(), install_input(&canonical, 10), Some(0), SCOPE)
            .expect("defs");
        install_automation_for(
            &mut store,
            &model,
            &canonical,
            1,
            "ops.on_receipt_only",
            "demo.Ticket",
            json!({ "kind": "event", "event": "action", "actionId": "demo.ticket.ping" }),
        );
        install_automation_for(
            &mut store,
            &model,
            &canonical,
            2,
            "ops.on_delete",
            "demo.Ticket",
            json!({ "kind": "event", "event": "action", "actionId": "demo.ticket.remove" }),
        );

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
        let CommitOutcome::Committed(absent_rcpt) = store.commit(&absent_call).expect("absent")
        else {
            panic!("expected commit");
        };
        let absent_triggers = store
            .read()
            .automation_triggers_for_commit(MAIN_WORLD, absent_rcpt.body.commit_seq)
            .expect("triggers");
        assert_eq!(absent_triggers.len(), 1);
        assert_eq!(absent_triggers[0].trigger_kind, KIND_ACTION);
        assert_eq!(absent_triggers[0].key.object_id, "t-ghost");
        assert_eq!(absent_triggers[0].key.object_version, 0);
        assert_eq!(absent_triggers[0].before_values, None);
        assert_eq!(absent_triggers[0].after_values, None);

        // Seed a live ticket `t-live`.
        store
            .commit(&ticket_batch("seed-live", 110, "open"))
            .expect("seed");

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
        let CommitOutcome::Committed(unchanged_rcpt) =
            store.commit(&unchanged_call).expect("unchanged")
        else {
            panic!("expected commit");
        };
        let unchanged_triggers = store
            .read()
            .automation_triggers_for_commit(MAIN_WORLD, unchanged_rcpt.body.commit_seq)
            .expect("triggers");
        assert_eq!(unchanged_triggers.len(), 1);
        assert_eq!(unchanged_triggers[0].key.object_id, "t-1");
        assert_eq!(unchanged_triggers[0].key.object_version, 1);
        assert!(unchanged_triggers[0].before_values.is_some());
        assert_eq!(
            unchanged_triggers[0].before_values, unchanged_triggers[0].after_values,
            "unchanged target must have before equal to after"
        );

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
        let CommitOutcome::Committed(delete_rcpt) = store.commit(&delete_call).expect("delete")
        else {
            panic!("expected commit");
        };
        let delete_triggers = store
            .read()
            .automation_triggers_for_commit(MAIN_WORLD, delete_rcpt.body.commit_seq)
            .expect("triggers");
        assert_eq!(delete_triggers.len(), 1);
        assert_eq!(delete_triggers[0].key.automation_id, "ops.on_delete");
        assert_eq!(delete_triggers[0].key.object_id, "t-1");
        assert_eq!(delete_triggers[0].key.object_version, 2);
        assert_eq!(
            delete_triggers[0]
                .before_values
                .as_ref()
                .and_then(|values| values.get("status")),
            Some(&Value::String("open".into()))
        );
        assert_eq!(
            delete_triggers[0].after_values, None,
            "deleted target must have after_values = None"
        );
    }

    /// S4: unrelated writes skip all object reads and automation work, and
    /// candidate fan-out past `MAX_CANDIDATES_PER_COMMIT` refuses and rolls back.
    #[test]
    fn unrelated_write_does_no_automation_work_and_fanout_refuses() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("hotpath.sqlite");
        let (mut store, _model, _canonical) = open_with_ticket_automations(&path);

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
        let watch = capture_before(&store.read(), &unrelated).expect("probe");
        assert!(
            watch.is_empty(),
            "unrelated write must match zero indexed triggers"
        );
        assert!(
            watch.before.is_empty(),
            "unrelated write must perform zero pre-commit object reads"
        );
        store.commit(&unrelated).expect("commit unrelated");
        assert_eq!(
            store
                .read()
                .pending_automation_trigger_count(MAIN_WORLD)
                .expect("count"),
            0
        );

        // Fan-out exceeding MAX_CANDIDATES_PER_COMMIT refuses and rolls back completely.
        let overflow_count = MAX_CANDIDATES_PER_COMMIT.saturating_add(1);
        let overflow_writes: Vec<FactWriteRow> = (0..overflow_count)
            .map(|idx| FactWriteRow::Put {
                object_type: "demo.Ticket".to_owned(),
                key: format!("t-bulk-{idx}"),
                values: BTreeMap::from([
                    ("status".to_owned(), Value::String("open".into())),
                    ("score".to_owned(), Value::from(10_i64)),
                ]),
            })
            .collect();
        let overflow_batch = CommitBatch {
            world: MAIN_WORLD.to_owned(),
            scope: SCOPE.to_owned(),
            logical_time: 200,
            idempotency_key: Some("bulk-overflow".to_owned()),
            writes: overflow_writes,
            ..CommitBatch::default()
        };
        let refused = store.commit(&overflow_batch);
        assert!(
            matches!(
                refused,
                Err(CellError::AutomationFanoutExceeded { limit, actual })
                    if limit == MAX_CANDIDATES_PER_COMMIT && actual == overflow_count
            ),
            "{refused:?}"
        );
        assert_eq!(
            store
                .read()
                .pending_automation_trigger_count(MAIN_WORLD)
                .expect("count"),
            0,
            "refused fan-out must roll back every outbox candidate"
        );
        assert_eq!(
            store
                .read()
                .object(MAIN_WORLD, "demo.Ticket", "t-bulk-0")
                .expect("read"),
            None,
            "refused fan-out must roll back every business row write"
        );
    }

    /// S5: replacing a live object with a sibling subtype in one commit fires
    /// as `event:created` (with `before_values = None`) for a rule watching the
    /// new subtype, and as `event:updated` for a rule watching their common ancestor.
    #[test]
    fn sibling_replacement_fires_create_with_no_before_state_for_new_subtype() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("sibling.sqlite");
        let (model, canonical) = sample_model();
        let mut store =
            CellStore::open(&path, model.clone(), StoreOptions::default()).expect("open");
        store.bind("scope", SCOPE).expect("bind");
        store
            .install_definitions(model.clone(), install_input(&canonical, 10), Some(0), SCOPE)
            .expect("defs");
        install_automation_for(
            &mut store,
            &model,
            &canonical,
            1,
            "ops.b_created",
            "demo.TicketB",
            json!({ "kind": "event", "event": "created" }),
        );
        install_automation_for(
            &mut store,
            &model,
            &canonical,
            2,
            "ops.b_updated",
            "demo.TicketB",
            json!({ "kind": "event", "event": "updated" }),
        );
        install_automation_for(
            &mut store,
            &model,
            &canonical,
            3,
            "ops.base_updated",
            "demo.Ticket",
            json!({ "kind": "event", "event": "updated" }),
        );

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
                    ("aTag".to_owned(), Value::String("from-a".into())),
                ]),
            }],
            ..CommitBatch::default()
        };
        store.commit(&create_a).expect("create TicketA");
        assert_eq!(
            store
                .read()
                .pending_automation_trigger_count(MAIN_WORLD)
                .expect("count"),
            0
        );

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
                        ("bTag".to_owned(), Value::String("from-b".into())),
                    ]),
                },
            ],
            ..CommitBatch::default()
        };
        let CommitOutcome::Committed(receipt) = store.commit(&replace_with_b).expect("replace")
        else {
            panic!("expected commit");
        };
        let triggers = store
            .read()
            .automation_triggers_for_commit(MAIN_WORLD, receipt.body.commit_seq)
            .expect("triggers");
        assert_eq!(triggers.len(), 2, "{triggers:?}");

        // Common ancestor `demo.Ticket` sees an update with TicketA before and TicketB after.
        assert_eq!(triggers[0].key.automation_id, "ops.base_updated");
        assert_eq!(triggers[0].trigger_kind, KIND_UPDATED);
        assert_eq!(
            triggers[0]
                .before_values
                .as_ref()
                .and_then(|values| values.get("aTag")),
            Some(&Value::String("from-a".into()))
        );
        assert_eq!(
            triggers[0]
                .after_values
                .as_ref()
                .and_then(|values| values.get("bTag")),
            Some(&Value::String("from-b".into()))
        );

        // New sibling `demo.TicketB` fires `event:created` (NOT `event:updated`) with no before-state.
        assert_eq!(triggers[1].key.automation_id, "ops.b_created");
        assert_eq!(triggers[1].trigger_kind, KIND_CREATED);
        assert_eq!(triggers[1].before_values, None);
        assert_eq!(
            triggers[1]
                .after_values
                .as_ref()
                .and_then(|values| values.get("bTag")),
            Some(&Value::String("from-b".into()))
        );
    }
}
