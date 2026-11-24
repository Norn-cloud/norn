//! AUTOMATION-OUTBOX-1 (Core 04): transactional automation trigger outbox.
//!
//! Every triggering commit — direct action calls, bound calls from flows,
//! approved proposals, or raw fact writes — records matching automation trigger
//! candidates **inside the same `SQLite` transaction** as the triggering write,
//! never from a post-commit hook.
//!
//! Each candidate is pinned to:
//! - the automation definition id and revision;
//! - the triggering commit sequence (`trigger_commit_seq`);
//! - the object type, object id, and object version written by that commit;
//! - the exact pre-commit and post-commit values (`before_values` /
//!   `after_values`) and, when the commit carried an `ActionCall`, its
//!   triggering `action_id`.
//!
//! Dedupe key (enforced by the table's primary key per world):
//! `(automation_id, revision, trigger_commit_seq, object_type, object_id, object_version)`.
//!
//! No trigger actor principal or grant is recorded on a candidate: execution
//! (U2) always resolves the automation's server-bound author grant from
//! `_norn_retained_authority` (`AUTH-ATTENUATE-1`).

use std::collections::{BTreeMap, BTreeSet};

use norn_ir::{FactChange, RuntimeValue as Value};
use serde::{Deserialize, Serialize};

use crate::codec::canonical_json;
use crate::engine::{SqlEngine, SqlRow, params};
use crate::error::CellError;
use crate::read::{ObjectRecord, StoreReader, to_i64, to_u64};
use crate::retained::RetainedLifecycle;
use crate::store::{CommitBatch, FactWriteRow, WrittenRow};

/// The transactional trigger outbox table.
pub(crate) const AUTOMATION_OUTBOX: &str = "_norn_automation_outbox";

/// The outbox DDL, applied with the other system tables.
pub(crate) const DDL: [&str; 2] = [
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
  PRIMARY KEY ("world", "automation_id", "revision", "trigger_commit_seq", "object_type", "object_id", "object_version")
)"#,
    r#"CREATE INDEX IF NOT EXISTS "_norn_automation_outbox/pending" ON "_norn_automation_outbox" ("world", "trigger_commit_seq", "automation_id", "revision", "object_type", "object_id", "object_version") WHERE "consumed_commit_seq" IS NULL"#,
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
    /// Concrete object type written by the triggering commit.
    pub object_type: String,
    /// Written object id.
    pub object_id: String,
    /// Written object version in the triggering commit.
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
    /// Exact pre-commit property values (`None` when the object was created).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub before_values: Option<BTreeMap<String, Value>>,
    /// Exact post-commit property values (`None` when the object was deleted).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub after_values: Option<BTreeMap<String, Value>>,
    /// Logical time (ms) of the triggering commit.
    pub recorded_at_ms: u64,
    /// Commit sequence that drained this candidate, or `None` while pending.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub consumed_commit_seq: Option<u64>,
}

/// Write-triggered matcher extracted from an active `automation` definition.
enum TriggerMatcher {
    Created,
    Updated,
    Action { action_id: String },
    LifecycleEntered { property: String, state: String },
}

impl TriggerMatcher {
    const fn kind_label(&self) -> &'static str {
        match self {
            Self::Created => "event:created",
            Self::Updated => "event:updated",
            Self::Action { .. } => "event:action",
            Self::LifecycleEntered { .. } => "lifecycle_entered",
        }
    }
}

struct WatchedRule {
    automation_id: String,
    revision: u64,
    object_type: String,
    matcher: TriggerMatcher,
}

/// Pre-commit snapshot of active write-triggered automations and pre-write
/// object values captured inside the triggering commit transaction before
/// `Writer::apply_writes` mutates any rows.
pub(crate) struct AutomationWatch {
    rules: Vec<WatchedRule>,
    /// Pre-commit records keyed by `(root_type, object_id)`.
    before: BTreeMap<(String, String), Option<ObjectRecord>>,
}

impl AutomationWatch {
    const fn empty() -> Self {
        Self {
            rules: Vec::new(),
            before: BTreeMap::new(),
        }
    }
}

fn parse_watched_rule(automation_id: &str, revision: u64, payload: &str) -> Option<WatchedRule> {
    let value: serde_json::Value = serde_json::from_str(payload).ok()?;
    if value.get("enabled").and_then(serde_json::Value::as_bool) != Some(true) {
        return None;
    }
    let object_type = value
        .get("objectType")
        .and_then(serde_json::Value::as_str)?
        .to_owned();
    let when = value.get("when")?.as_object()?;
    let matcher = match when.get("kind").and_then(serde_json::Value::as_str)? {
        "event" => match when.get("event").and_then(serde_json::Value::as_str)? {
            "created" => TriggerMatcher::Created,
            "updated" => TriggerMatcher::Updated,
            "action" => TriggerMatcher::Action {
                action_id: when
                    .get("actionId")
                    .and_then(serde_json::Value::as_str)?
                    .to_owned(),
            },
            _ => return None,
        },
        "lifecycle_entered" => TriggerMatcher::LifecycleEntered {
            property: when
                .get("property")
                .and_then(serde_json::Value::as_str)?
                .to_owned(),
            state: when
                .get("state")
                .and_then(serde_json::Value::as_str)?
                .to_owned(),
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

fn load_active_write_rules<E: SqlEngine>(
    reader: &StoreReader<'_, E>,
) -> Result<Vec<WatchedRule>, CellError> {
    const PAGE_SIZE: u32 = 100;
    let mut rules = Vec::new();
    let mut after: Option<String> = None;
    loop {
        let page = reader.retained_definitions("automation", false, after.as_deref(), PAGE_SIZE)?;
        let full = page.len() >= usize::try_from(PAGE_SIZE).unwrap_or(usize::MAX);
        after = page.last().map(|record| record.definition.id.clone());
        for record in page {
            if record.definition.lifecycle != RetainedLifecycle::Active {
                continue;
            }
            // Core 05 AUTH-ATTENUATE-1: never enqueue candidates for an
            // automation that carries no server-bound revocable author grant.
            if !record
                .definition
                .authority
                .as_ref()
                .is_some_and(|authority| authority.revocable)
            {
                continue;
            }
            if let Some(rule) = parse_watched_rule(
                &record.definition.id,
                record.definition.revision,
                &record.definition.payload,
            ) {
                rules.push(rule);
            }
        }
        if !full {
            return Ok(rules);
        }
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

/// Capture pre-commit state for objects touched by `batch` when active
/// write-triggered automations exist. Called inside `CellStore`'s commit
/// transaction right before `Writer::apply_writes`.
///
/// Fast path: when `batch` has no object writes or no active write-triggered
/// automations exist, returns immediately without reading any object rows.
pub(crate) fn capture_before<E: SqlEngine>(
    reader: &StoreReader<'_, E>,
    batch: &CommitBatch,
) -> Result<AutomationWatch, CellError> {
    if batch.writes.is_empty() && batch.facts.is_empty() {
        return Ok(AutomationWatch::empty());
    }
    let rules = load_active_write_rules(reader)?;
    if rules.is_empty() {
        return Ok(AutomationWatch::empty());
    }

    let model = reader.model();
    let watched_roots: BTreeSet<&str> = rules
        .iter()
        .filter_map(|rule| reader.root_type(&rule.object_type).ok())
        .collect();

    let mut before = BTreeMap::new();
    for (object_type, key) in batch
        .writes
        .iter()
        .map(FactWriteRow::target)
        .chain(batch.facts.iter().map(fact_change_target))
    {
        if !model.has_type(object_type) {
            continue;
        }
        let root = reader.root_type(object_type)?;
        if !watched_roots.contains(root) {
            continue;
        }
        let lookup_key = (root.to_owned(), key.to_owned());
        if before.contains_key(&lookup_key) {
            continue;
        }
        // Query through `root` so a write addressing a supertype or subtype
        // always resolves the live concrete record before mutation.
        let prior = reader.object(&batch.world, root, key)?;
        before.insert(lookup_key, prior);
    }

    Ok(AutomationWatch { rules, before })
}

struct ActionCallMeta {
    action_id: String,
    target_root: String,
    target_key: String,
}

fn action_calls_in_batch<E: SqlEngine>(
    reader: &StoreReader<'_, E>,
    batch: &CommitBatch,
) -> Vec<ActionCallMeta> {
    let mut calls = Vec::new();
    for write in &batch.writes {
        let FactWriteRow::Put {
            object_type,
            values,
            ..
        } = write
        else {
            continue;
        };
        if object_type != norn_kernel::ACTION_CALL_TYPE {
            continue;
        }
        let Some(action_id) = values.get("actionId").and_then(Value::as_str) else {
            continue;
        };
        let Some(object) = values.get("object").and_then(Value::as_object) else {
            continue;
        };
        let Some(target_type) = object.get("objectType").and_then(Value::as_str) else {
            continue;
        };
        let Some(target_key) = object.get("key").and_then(Value::as_str) else {
            continue;
        };
        let target_root = reader
            .root_type(target_type)
            .unwrap_or(target_type)
            .to_owned();
        calls.push(ActionCallMeta {
            action_id: action_id.to_owned(),
            target_root,
            target_key: target_key.to_owned(),
        });
    }
    calls
}

fn rule_matches(
    matcher: &TriggerMatcher,
    before_record: Option<&ObjectRecord>,
    after_record: Option<&ObjectRecord>,
    matching_actions: &[&str],
) -> bool {
    match matcher {
        TriggerMatcher::Created => before_record.is_none() && after_record.is_some(),
        TriggerMatcher::Updated => before_record.is_some() && after_record.is_some(),
        TriggerMatcher::Action { action_id } => {
            after_record.is_some()
                && matching_actions
                    .iter()
                    .any(|called| *called == action_id.as_str())
        }
        TriggerMatcher::LifecycleEntered { property, state } => {
            let entered = after_record
                .as_ref()
                .and_then(|record| record.values.get(property))
                .and_then(Value::as_str)
                == Some(state.as_str());
            let was_in = before_record
                .and_then(|record| record.values.get(property))
                .and_then(Value::as_str)
                == Some(state.as_str());
            entered && !was_in
        }
    }
}

/// Record matching automation trigger candidates inside the triggering commit's
/// `SQLite` transaction, right after `Writer::apply_writes`.
///
/// Dedupe is enforced by the primary key `(world, automation_id, revision,
/// trigger_commit_seq, object_type, object_id, object_version)` via `INSERT OR IGNORE`.
pub(crate) fn record<E: SqlEngine>(
    reader: &StoreReader<'_, E>,
    batch: &CommitBatch,
    written: &[WrittenRow],
    commit_seq: u64,
    watch: AutomationWatch,
) -> Result<(), CellError> {
    if watch.rules.is_empty() || written.is_empty() {
        return Ok(());
    }

    let model = reader.model();
    let action_calls = action_calls_in_batch(reader, batch);

    // Collapse multiple writes to the same object within one commit (e.g.,
    // several `FactChange::Set` steps) to its final committed row/version.
    let mut final_rows: BTreeMap<(String, String), &WrittenRow> = BTreeMap::new();
    for row in written {
        if !model.has_type(&row.object_type) {
            continue;
        }
        let root = reader.root_type(&row.object_type)?.to_owned();
        final_rows.insert((root, row.key.clone()), row);
    }

    for ((root, key), row) in final_rows {
        let before_record = watch
            .before
            .get(&(root.clone(), key.clone()))
            .and_then(Option::as_ref);
        let after_record = reader.object(&batch.world, &row.object_type, &row.key)?;
        let matching_actions: Vec<&str> = action_calls
            .iter()
            .filter(|call| call.target_root == root && call.target_key == key)
            .map(|call| call.action_id.as_str())
            .collect();
        let primary_action = matching_actions.first().copied();
        let before_json = before_record
            .map(|record| canonical_json(&Value::Object(record.values.clone())))
            .transpose()?;
        let after_json = after_record
            .as_ref()
            .map(|record| canonical_json(&Value::Object(record.values.clone())))
            .transpose()?;

        for rule in &watch.rules {
            if !model.is_a(&row.object_type, &rule.object_type)
                || !rule_matches(
                    &rule.matcher,
                    before_record,
                    after_record.as_ref(),
                    &matching_actions,
                )
            {
                continue;
            }
            let action_id_col = match &rule.matcher {
                TriggerMatcher::Action { action_id } => Some(action_id.as_str()),
                TriggerMatcher::Created
                | TriggerMatcher::Updated
                | TriggerMatcher::LifecycleEntered { .. } => primary_action,
            };
            insert_candidate_row(
                reader.conn,
                &batch.world,
                &AutomationTriggerKey {
                    automation_id: rule.automation_id.clone(),
                    revision: rule.revision,
                    trigger_commit_seq: commit_seq,
                    object_type: row.object_type.clone(),
                    object_id: row.key.clone(),
                    object_version: row.version,
                },
                rule.matcher.kind_label(),
                action_id_col,
                before_json.as_deref(),
                after_json.as_deref(),
                batch.logical_time,
            )?;
        }
    }
    Ok(())
}

#[allow(
    clippy::too_many_arguments,
    reason = "maps the fixed columns of one transactional outbox row"
)]
pub(crate) fn insert_candidate_row<E: SqlEngine>(
    conn: &E,
    world: &str,
    key: &AutomationTriggerKey,
    trigger_kind: &str,
    action_id: Option<&str>,
    before_json: Option<&str>,
    after_json: Option<&str>,
    recorded_at_ms: u64,
) -> Result<(), CellError> {
    conn.execute(
        &format!(
            "INSERT OR IGNORE INTO \"{AUTOMATION_OUTBOX}\" (\
              \"world\", \"automation_id\", \"revision\", \"trigger_commit_seq\", \
              \"object_type\", \"object_id\", \"object_version\", \"trigger_kind\", \
              \"action_id\", \"before_values\", \"after_values\", \"recorded_at_ms\", \
              \"consumed_commit_seq\"\
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, NULL)"
        ),
        params![
            world,
            key.automation_id,
            to_i64(key.revision, "automation revision")?,
            to_i64(key.trigger_commit_seq, "trigger commit seq")?,
            key.object_type,
            key.object_id,
            to_i64(key.object_version, "object version")?,
            trigger_kind,
            action_id,
            before_json,
            after_json,
            to_i64(recorded_at_ms, "trigger recorded_at_ms")?,
        ],
    )
}

const SELECT_COLUMNS: &str = "\"world\", \"automation_id\", \"revision\", \"trigger_commit_seq\", \"object_type\", \"object_id\", \"object_version\", \"trigger_kind\", \"action_id\", \"before_values\", \"after_values\", \"recorded_at_ms\", \"consumed_commit_seq\"";

fn decode_candidate(
    values: Vec<crate::engine::SqlValue>,
) -> Result<AutomationTriggerCandidate, CellError> {
    let row = SqlRow(values);
    let world: String = row.get(0)?;
    let automation_id: String = row.get(1)?;
    let revision = to_u64(row.get(2)?, "automation revision")?;
    let trigger_commit_seq = to_u64(row.get(3)?, "trigger commit seq")?;
    let object_type: String = row.get(4)?;
    let object_id: String = row.get(5)?;
    let object_version = to_u64(row.get(6)?, "object version")?;
    let trigger_kind: String = row.get(7)?;
    let action_id: Option<String> = row.get(8)?;
    let before_text: Option<String> = row.get(9)?;
    let after_text: Option<String> = row.get(10)?;
    let recorded_at_ms = to_u64(row.get(11)?, "trigger recorded_at_ms")?;
    let consumed_raw: Option<i64> = row.get(12)?;
    let consumed_commit_seq = consumed_raw
        .map(|seq| to_u64(seq, "consumed commit seq"))
        .transpose()?;

    let decode_object =
        |text: Option<String>| -> Result<Option<BTreeMap<String, Value>>, CellError> {
            text.map(|raw| {
                let parsed = Value::from_json_text(&raw).map_err(|error| {
                    CellError::Encoding(format!("automation outbox snapshot: {error}"))
                })?;
                match parsed {
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
        };

    Ok(AutomationTriggerCandidate {
        world,
        key: AutomationTriggerKey {
            automation_id,
            revision,
            trigger_commit_seq,
            object_type,
            object_id,
            object_version,
        },
        trigger_kind,
        action_id,
        before_values: decode_object(before_text)?,
        after_values: decode_object(after_text)?,
        recorded_at_ms,
        consumed_commit_seq,
    })
}

impl<E: SqlEngine> StoreReader<'_, E> {
    /// Pending (unconsumed) automation trigger candidates in `world`, ordered
    /// deterministically by `(trigger_commit_seq, automation_id, revision,
    /// object_type, object_id, object_version)`, up to `limit`.
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
                 WHERE \"world\" = ?1 AND \"consumed_commit_seq\" IS NULL \
                 ORDER BY \"trigger_commit_seq\", \"automation_id\", \"revision\", \"object_type\", \"object_id\", \"object_version\" \
                 LIMIT ?2"
            ),
            params![world, i64::try_from(limit).unwrap_or(i64::MAX)],
        )?;
        rows.into_iter().map(decode_candidate).collect()
    }

    /// All automation trigger candidates recorded by commit `commit_seq` in `world`.
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

    /// Total number of pending (unconsumed) automation trigger candidates in `world`.
    ///
    /// # Errors
    /// SQL engine or count range errors.
    pub fn pending_automation_trigger_count(&self, world: &str) -> Result<u64, CellError> {
        let count: i64 = self.conn.query_row(
            &format!(
                "SELECT COUNT(*) FROM \"{AUTOMATION_OUTBOX}\" \
                 WHERE \"world\" = ?1 AND \"consumed_commit_seq\" IS NULL"
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
        let object_type = json!({
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
        let typed: ObjectTypeDef = serde_json::from_value(object_type.clone()).expect("type");
        let model = Model::from_definitions(&[typed], &[]).expect("model");
        let artifacts = json!([{
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
            "payload": { "entityVersion": "2.0", "objectType": object_type }
        }]);
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

    fn install_automation(
        store: &mut CellStore,
        model: &Model,
        canonical_set: &str,
        expected_defs_version: u64,
        id: &str,
        when: serde_json::Value,
    ) {
        let payload = serde_json::to_string(&json!({
            "title": id,
            "enabled": true,
            "objectType": "demo.Ticket",
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
                    object_type: "demo.Ticket".to_owned(),
                    action_digest: "d".repeat(64),
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

    fn open_with_ticket_automations(path: &std::path::Path) -> (CellStore, Model) {
        let (model, canonical) = sample_model();
        let mut store =
            CellStore::open(path, model.clone(), StoreOptions::default()).expect("open");
        store.bind("scope", SCOPE).expect("bind");
        store
            .install_definitions(model.clone(), install_input(&canonical, 10), Some(0), SCOPE)
            .expect("kernel defs");
        install_automation(
            &mut store,
            &model,
            &canonical,
            1,
            "ops.on_create",
            json!({ "kind": "event", "event": "created" }),
        );
        install_automation(
            &mut store,
            &model,
            &canonical,
            2,
            "ops.on_stalled",
            json!({ "kind": "lifecycle_entered", "property": "status", "state": "stalled" }),
        );
        (store, model)
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

    #[test]
    fn rolls_back_before_commit_and_survives_crash_after_commit() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("cell.sqlite");
        let (mut store, model) = open_with_ticket_automations(&path);

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
        assert_eq!(
            pending[0]
                .after_values
                .as_ref()
                .and_then(|values| values.get("status")),
            Some(&Value::String("open".into()))
        );
    }

    #[test]
    fn replay_dedupes_and_lifecycle_transition_captures_before_and_after() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("cell.sqlite");
        let (mut store, _model) = open_with_ticket_automations(&path);

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
        insert_candidate_row(
            store.read().conn,
            MAIN_WORLD,
            &pending[0].key,
            "event:created",
            None,
            None,
            None,
            100,
        )
        .expect("duplicate insert ignored");
        assert_eq!(
            store
                .read()
                .pending_automation_trigger_count(MAIN_WORLD)
                .expect("count"),
            1
        );

        let stall_batch = ticket_batch("stall-t1", 200, "stalled");
        let CommitOutcome::Committed(receipt) = store.commit(&stall_batch).expect("stall") else {
            panic!("expected committed receipt");
        };
        assert_eq!(receipt.body.commit_seq, create_seq.saturating_add(1));
        let for_commit = store
            .read()
            .automation_triggers_for_commit(MAIN_WORLD, receipt.body.commit_seq)
            .expect("commit triggers");
        assert_eq!(for_commit.len(), 1);
        assert_eq!(for_commit[0].key.automation_id, "ops.on_stalled");
        assert_eq!(for_commit[0].key.object_version, 2);
        assert_eq!(for_commit[0].trigger_kind, "lifecycle_entered");
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
}
