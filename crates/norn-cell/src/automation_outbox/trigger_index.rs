//! Transactional trigger projection (`_norn_retained_automation_trigger`) and
//! bounded keyset-paged rule queries for `AUTOMATION-OUTBOX-1`.
//!
//! Active, enabled, authority-bound write and action triggers are maintained
//! inside `SqlEngine::definition_writes` on install, revision, and retire.
//! Commit-time lookups use indexed `LIMIT 1` probes and keyset-paged `LIMIT`
//! queries so no unbounded rule list is ever materialized inside a write
//! transaction.

use std::collections::BTreeMap;

use crate::Model;
use crate::engine::{SqlEngine, SqlRow, SqlValue, params};
use crate::error::CellError;
use crate::read::{to_i64, to_u64};
use crate::retained::{RetainedDefinition, RetainedDefinitionChange, RetainedLifecycle};

use super::continuation::limit_to_sql;
use super::{AUTOMATION_OUTBOX, RULE_PAGE_LIMIT};

/// Indexed projection of active, enabled, authority-bound write and action
/// automation triggers, maintained transactionally on install, revision, and
/// retire. Prefixed `_norn_retained_` so `NativeEngine` S1 write-protects it
/// outside `SqlEngine::definition_writes`.
const AUTOMATION_TRIGGER_INDEX: &str = "_norn_retained_automation_trigger";

/// Maximum distinct watched types or actions whose first rule page is cached
/// within a single commit (bounds in-memory rule state to
/// `MAX_PAGE1_CACHE_TYPES * RULE_PAGE_LIMIT`).
const MAX_PAGE1_CACHE_TYPES: usize = 16;

/// Trigger kind strings persisted in the outbox and trigger index.
pub(super) const KIND_CREATED: &str = "event:created";
pub(super) const KIND_UPDATED: &str = "event:updated";
pub(super) const KIND_ACTION: &str = "event:action";
pub(super) const KIND_LIFECYCLE_ENTERED: &str = "lifecycle_entered";

/// Write- or action-triggered matcher projected from an active `automation`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum TriggerMatcher {
    Created,
    Updated,
    Action { action_id: String },
    LifecycleEntered { property: String, state: String },
}

impl TriggerMatcher {
    pub(super) const fn kind_label(&self) -> &'static str {
        match self {
            Self::Created => KIND_CREATED,
            Self::Updated => KIND_UPDATED,
            Self::Action { .. } => KIND_ACTION,
            Self::LifecycleEntered { .. } => KIND_LIFECYCLE_ENTERED,
        }
    }
}

#[derive(Debug, Clone)]
pub(super) struct WatchedRule {
    pub(super) automation_id: String,
    pub(super) revision: u64,
    pub(super) object_type: String,
    pub(super) matcher: TriggerMatcher,
}

pub(super) struct RulePage {
    pub(super) rules: Vec<WatchedRule>,
    pub(super) has_more: bool,
}

#[derive(Default)]
pub(super) struct Page1Cache {
    entries: BTreeMap<String, (Vec<WatchedRule>, bool)>,
}

impl Page1Cache {
    pub(super) fn get_or_load<E: SqlEngine>(
        &mut self,
        conn: &E,
        key: &str,
        load: impl FnOnce(&E, &str, Option<&str>, usize) -> Result<RulePage, CellError>,
    ) -> Result<RulePage, CellError> {
        if let Some((rules, has_more)) = self.entries.get(key) {
            return Ok(RulePage {
                rules: rules.clone(),
                has_more: *has_more,
            });
        }
        let page = load(conn, key, None, RULE_PAGE_LIMIT)?;
        if self.entries.len() < MAX_PAGE1_CACHE_TYPES {
            self.entries
                .insert(key.to_owned(), (page.rules.clone(), page.has_more));
        }
        Ok(page)
    }
}

fn parse_watched_rule<'a>(
    model: &'a Model,
    automation_id: &str,
    revision: u64,
    payload: &str,
) -> Result<Option<(WatchedRule, &'a str)>, CellError> {
    let invalid = |field: &str| {
        CellError::DefinitionReceiptInvalid(format!(
            "automation {automation_id} has an invalid {field}"
        ))
    };
    let value: serde_json::Value = serde_json::from_str(payload)?;
    let enabled = value
        .get("enabled")
        .and_then(serde_json::Value::as_bool)
        .ok_or_else(|| invalid("enabled flag"))?;
    let object_type = value
        .get("objectType")
        .and_then(serde_json::Value::as_str)
        .filter(|name| !name.trim().is_empty())
        .ok_or_else(|| invalid("object type"))?;
    let root_type = model
        .chain(object_type)?
        .first()
        .ok_or_else(|| CellError::Engine(format!("missing root type for {object_type}")))?;
    let when = value.get("when").ok_or_else(|| invalid("trigger"))?;
    let field = |key: &str| {
        when.get(key)
            .and_then(serde_json::Value::as_str)
            .filter(|text| !text.trim().is_empty())
            .ok_or_else(|| invalid(key))
    };
    let matcher = match field("kind")? {
        "event" => match field("event")? {
            "created" => TriggerMatcher::Created,
            "updated" => TriggerMatcher::Updated,
            "action" => TriggerMatcher::Action {
                action_id: field("actionId")?.to_owned(),
            },
            _ => return Err(invalid("event trigger")),
        },
        "lifecycle_entered" => TriggerMatcher::LifecycleEntered {
            property: field("property")?.to_owned(),
            state: field("state")?.to_owned(),
        },
        // Stale rules are timer-driven (U4), never enqueued on write commits.
        "stale" => {
            field("property")?;
            when.get("durationMs")
                .and_then(serde_json::Value::as_u64)
                .ok_or_else(|| invalid("durationMs"))?;
            return Ok(None);
        }
        _ => return Err(invalid("trigger kind")),
    };
    Ok(enabled.then(|| {
        (
            WatchedRule {
                automation_id: automation_id.to_owned(),
                revision,
                object_type: object_type.to_owned(),
                matcher,
            },
            root_type.as_str(),
        )
    }))
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
    if !authorized {
        return Ok(());
    }
    let Some((rule, root_type)) = parse_watched_rule(
        model,
        &definition.id,
        definition.revision,
        &definition.payload,
    )?
    else {
        return Ok(());
    };
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
    changes: impl IntoIterator<Item = &'a RetainedDefinitionChange>,
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

fn decode_indexed_rule(values: Vec<SqlValue>) -> Result<WatchedRule, CellError> {
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

fn decode_rule_page(rows: Vec<Vec<SqlValue>>, page_limit: usize) -> Result<RulePage, CellError> {
    let has_more = rows.len() > page_limit;
    let rules = rows
        .into_iter()
        .take(page_limit)
        .map(decode_indexed_rule)
        .collect::<Result<Vec<_>, _>>()?;
    Ok(RulePage { rules, has_more })
}

pub(super) fn has_row_rules_for_root<E: SqlEngine>(
    conn: &E,
    root_type: &str,
) -> Result<bool, CellError> {
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

pub(super) fn has_action_rules<E: SqlEngine>(conn: &E, action_id: &str) -> Result<bool, CellError> {
    let found: Option<i64> = conn.query_optional(
        &format!(
            "SELECT 1 FROM \"{AUTOMATION_TRIGGER_INDEX}\" \
             WHERE \"trigger_kind\" = '{KIND_ACTION}' AND \"action_id\" = ?1 LIMIT 1"
        ),
        params![action_id],
        |row| row.get(0),
    )?;
    Ok(found.is_some())
}

pub(super) fn query_row_rules_page<E: SqlEngine>(
    conn: &E,
    object_type: &str,
    after_automation_id: Option<&str>,
    page_limit: usize,
) -> Result<RulePage, CellError> {
    let fetch_limit = limit_to_sql(page_limit.saturating_add(1));
    let rows = match after_automation_id {
        Some(cursor) => conn.query(
            &format!(
                "SELECT {INDEX_COLUMNS} FROM \"{AUTOMATION_TRIGGER_INDEX}\" \
                 WHERE \"object_type\" = ?1 AND \"trigger_kind\" != '{KIND_ACTION}' \
                 AND \"automation_id\" > ?2 \
                 ORDER BY \"automation_id\" LIMIT ?3"
            ),
            params![object_type, cursor, fetch_limit],
        )?,
        None => conn.query(
            &format!(
                "SELECT {INDEX_COLUMNS} FROM \"{AUTOMATION_TRIGGER_INDEX}\" \
                 WHERE \"object_type\" = ?1 AND \"trigger_kind\" != '{KIND_ACTION}' \
                 ORDER BY \"automation_id\" LIMIT ?2"
            ),
            params![object_type, fetch_limit],
        )?,
    };
    decode_rule_page(rows, page_limit)
}

pub(super) fn query_action_rules_page<E: SqlEngine>(
    conn: &E,
    action_id: &str,
    after_automation_id: Option<&str>,
    page_limit: usize,
) -> Result<RulePage, CellError> {
    let fetch_limit = limit_to_sql(page_limit.saturating_add(1));
    let rows = match after_automation_id {
        Some(cursor) => conn.query(
            &format!(
                "SELECT {INDEX_COLUMNS} FROM \"{AUTOMATION_TRIGGER_INDEX}\" \
                 WHERE \"trigger_kind\" = '{KIND_ACTION}' AND \"action_id\" = ?1 \
                 AND \"automation_id\" > ?2 \
                 ORDER BY \"automation_id\" LIMIT ?3"
            ),
            params![action_id, cursor, fetch_limit],
        )?,
        None => conn.query(
            &format!(
                "SELECT {INDEX_COLUMNS} FROM \"{AUTOMATION_TRIGGER_INDEX}\" \
                 WHERE \"trigger_kind\" = '{KIND_ACTION}' AND \"action_id\" = ?1 \
                 ORDER BY \"automation_id\" LIMIT ?2"
            ),
            params![action_id, fetch_limit],
        )?,
    };
    decode_rule_page(rows, page_limit)
}
