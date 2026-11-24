//! Pre-commit capture for `AUTOMATION-OUTBOX-1`: which roots and actions
//! have rules, the batch's committed action calls, and the bounded preimage
//! snapshots inline matching reads, taken in replay order.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use norn_ir::{FactChange, RuntimeValue as Value};

use super::MAX_PREIMAGES_PER_COMMIT;
use super::trigger_index::{has_action_rules, has_row_rules_for_root, has_row_rules_for_type};
use crate::engine::SqlEngine;
use crate::error::CellError;
use crate::read::{ObjectRecord, StoreReader};
use crate::store::{CommitBatch, FactWriteRow};

/// Pre-commit trigger probe results and pre-write object snapshots captured
/// before `Writer::apply_writes` mutates any rows.
#[derive(Default)]
pub(crate) struct AutomationWatch {
    pub(super) watched_row_roots: BTreeSet<String>,
    pub(super) watched_actions: BTreeSet<String>,
    pub(super) before: BTreeMap<(String, String), Option<ObjectRecord>>,
    /// The first written `(root, key)` target, in replay order, whose preimage
    /// fell outside [`MAX_PREIMAGES_PER_COMMIT`]: Pass 1 defers it and every
    /// later target to the continuation.
    pub(super) row_cutoff: Option<(String, String)>,
    /// The first watched `norn.ActionCall` key, in replay order, whose target
    /// preimage fell outside the budget: Pass 2 defers it and every later call.
    pub(super) action_cutoff: Option<String>,
}

impl AutomationWatch {
    pub(super) fn is_empty(&self) -> bool {
        self.watched_row_roots.is_empty() && self.watched_actions.is_empty()
    }
}

/// Replay-stable order identity of one `(root type, id)` target (matcher
/// version 2): the NFC identity an export carries first, then the raw bytes
/// as the tie-break between identities that collide under NFC.
pub(super) type ReplayKey<'a> = (String, String, &'a str, &'a str);

pub(super) fn replay_key<'a>(root: &'a str, id: &'a str) -> ReplayKey<'a> {
    (
        norn_ir::canonical::nfc(root),
        norn_ir::canonical::nfc(id),
        root,
        id,
    )
}

/// Whether `(root, id)` sorts at or after `cutoff` in replay order.
pub(super) fn at_or_after(root: &str, id: &str, cutoff: Option<(&str, &str)>) -> bool {
    cutoff.is_some_and(|(cut_root, cut_id)| replay_key(root, id) >= replay_key(cut_root, cut_id))
}

/// Pre-commit snapshot reads bounded by `limit` ([`preimage_limit`]).
struct PreimageBudget<'r, 's, E: SqlEngine> {
    reader: &'r StoreReader<'s, E>,
    world: &'r str,
    limit: usize,
    before: BTreeMap<(String, String), Option<ObjectRecord>>,
}

impl<E: SqlEngine> PreimageBudget<'_, '_, E> {
    /// Capture `(root, key)`'s preimage unless it is already held. Returns
    /// `false`, reading nothing, when the budget is spent.
    fn capture(&mut self, root: &str, key: &str) -> Result<bool, CellError> {
        let lookup_key = (root.to_owned(), key.to_owned());
        if self.before.contains_key(&lookup_key) {
            return Ok(true);
        }
        if self.before.len() >= self.limit {
            return Ok(false);
        }
        // Query through `root` so a write addressing a supertype or subtype
        // always resolves the live concrete record before mutation.
        let prior = self.reader.object(self.world, root, key)?;
        self.before.insert(lookup_key, prior);
        Ok(true)
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

pub(super) struct ActionCallMeta {
    pub(super) action_id: String,
    pub(super) target_type: String,
    pub(super) target_root: String,
    pub(super) target_key: String,
}

pub(super) struct ActionCallFields<'a> {
    pub(super) action_id: Option<&'a Value>,
    pub(super) object: Option<&'a Value>,
    pub(super) metadata_set: bool,
}

impl<'a> ActionCallFields<'a> {
    pub(super) fn from_values(values: &'a BTreeMap<String, Value>) -> Self {
        Self {
            action_id: values.get("actionId"),
            object: values.get("object"),
            metadata_set: false,
        }
    }
}

pub(super) fn extract_action_call<E: SqlEngine>(
    reader: &StoreReader<'_, E>,
    fields: &ActionCallFields<'_>,
) -> Option<ActionCallMeta> {
    let action_id = fields.action_id?.as_str()?;
    let object = fields.object?.as_object()?;
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

pub(super) fn action_calls_in_batch(batch: &CommitBatch) -> BTreeMap<&str, ActionCallFields<'_>> {
    // The writer applies legacy rows before canonical facts. Fold both in
    // that order so the same final metadata drives pre-commit snapshots and
    // candidate recording. A Set alone does not introduce a new action call.
    let mut calls = BTreeMap::new();
    for write in &batch.writes {
        let (object_type, key) = write.target();
        if object_type != norn_kernel::ACTION_CALL_TYPE {
            continue;
        }
        match write {
            FactWriteRow::Put { values, .. } => {
                calls.insert(key, ActionCallFields::from_values(values));
            }
            FactWriteRow::Delete { .. } => {
                calls.remove(key);
            }
        }
    }
    for fact in &batch.facts {
        let (object_type, key) = fact_change_target(fact);
        if object_type != norn_kernel::ACTION_CALL_TYPE {
            continue;
        }
        match fact {
            FactChange::Create { values, .. } => {
                calls.insert(key, ActionCallFields::from_values(values));
            }
            FactChange::Delete { .. } => {
                calls.remove(key);
            }
            FactChange::Set {
                property, value, ..
            } => {
                if let Some(fields) = calls.get_mut(key) {
                    match property.as_str() {
                        "actionId" => fields.action_id = Some(value),
                        "object" => fields.object = Some(value),
                        _ => continue,
                    }
                    fields.metadata_set = true;
                }
            }
            FactChange::Transition { .. } => {}
        }
    }
    calls
}

/// How many preimages [`capture_before`] may hold for a commit: the
/// [`MAX_PREIMAGES_PER_COMMIT`] budget for a live commit and for the replay of
/// a continuation written under it (matcher version 2 or later). Replaying a
/// record with no continuation, or with a matcher version 1 one, captures
/// without the cap: the live commit then never paused at a preimage cutoff
/// (version 1 had none; under version 2 no rule-bearing target lay past it),
/// so the uncapped replay reproduces it exactly, and pre-#1704 v7 exports keep
/// restoring.
pub(crate) fn preimage_limit(retained: Option<&crate::export::ExportRecord>) -> usize {
    match retained.map(|record| record.automation_continuation.as_ref()) {
        None => MAX_PREIMAGES_PER_COMMIT,
        Some(Some(row)) if row.matcher_version != 1 => MAX_PREIMAGES_PER_COMMIT,
        Some(_) => usize::MAX,
    }
}

/// Capture pre-commit state for objects touched by `batch` via indexed
/// `LIMIT 1` probes, holding at most `limit` preimages (see
/// [`preimage_limit`]). Returns immediately with zero object reads when no
/// rule matches.
pub(crate) fn capture_before<E: SqlEngine>(
    reader: &StoreReader<'_, E>,
    batch: &CommitBatch,
    limit: usize,
) -> Result<AutomationWatch, CellError> {
    if batch.writes.is_empty() && batch.facts.is_empty() {
        return Ok(AutomationWatch::default());
    }
    let model = reader.model();
    let action_calls: Vec<(&str, ActionCallMeta)> = action_calls_in_batch(batch)
        .into_iter()
        .filter_map(|(key, fields)| extract_action_call(reader, &fields).map(|call| (key, call)))
        .collect();
    let mut watched_actions = BTreeSet::new();
    let mut checked_actions = BTreeSet::new();
    for (_, call) in &action_calls {
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
    // Targets whose preimage inline matching may read, deduplicated by
    // `(root, key)`, then captured in replay order up to the budget.
    let mut row_targets: BTreeMap<(&str, &str), bool> = BTreeMap::new();
    let mut type_has_row_rules: HashMap<&str, bool> = HashMap::new();
    let mut target_matches_row_rules: HashMap<&str, bool> = HashMap::new();
    for (ty, key) in touched {
        let root = reader.root_type(ty)?;
        if !watched_row_roots.contains(root) {
            continue;
        }
        let may_match = target_type_may_match_row_rules(
            reader,
            ty,
            &mut type_has_row_rules,
            &mut target_matches_row_rules,
        )?;
        *row_targets.entry((root, key)).or_default() |= may_match;
    }
    let mut row_order: Vec<(&str, &str)> = row_targets
        .into_iter()
        .filter_map(|(target, may_match)| may_match.then_some(target))
        .collect();
    row_order.sort_by_cached_key(|&(root, key)| replay_key(root, key));
    let mut budget = PreimageBudget {
        reader,
        world: &batch.world,
        limit,
        before: BTreeMap::new(),
    };
    let mut row_cutoff = None;
    for (root, key) in row_order {
        if !budget.capture(root, key)? {
            row_cutoff = Some((root.to_owned(), key.to_owned()));
            break;
        }
    }
    let mut action_order: Vec<(&str, &ActionCallMeta)> = action_calls
        .iter()
        .filter(|(_, call)| watched_actions.contains(&call.action_id))
        .map(|(key, call)| (*key, call))
        .collect();
    action_order.sort_by_cached_key(|&(key, _)| replay_key(norn_kernel::ACTION_CALL_TYPE, key));
    let mut action_cutoff = None;
    for (call_key, call) in action_order {
        // Pass 2 runs only after Pass 1 finishes, so a deferred row target
        // defers every action call too.
        let captured = row_cutoff.is_none()
            && (!model.has_type(&call.target_type)
                || budget.capture(&call.target_root, &call.target_key)?);
        if !captured {
            action_cutoff = Some(call_key.to_owned());
            break;
        }
    }
    Ok(AutomationWatch {
        watched_row_roots,
        watched_actions,
        before: budget.before,
        row_cutoff,
        action_cutoff,
    })
}

fn target_type_may_match_row_rules<'a, E: SqlEngine>(
    reader: &'a StoreReader<'_, E>,
    target_type: &'a str,
    type_has_row_rules: &mut HashMap<&'a str, bool>,
    target_matches_row_rules: &mut HashMap<&'a str, bool>,
) -> Result<bool, CellError> {
    if let Some(&cached) = target_matches_row_rules.get(target_type) {
        return Ok(cached);
    }
    let model = reader.model();
    let mut matched = false;
    // Check ancestors (including `target_type`) so sibling replacements still
    // capture preimages for common-ancestor rules, and descendants so a
    // supertype write (e.g. `FactChange::Set`) still captures subtype preimages.
    let candidates = model.chain(target_type)?.iter().map(String::as_str).chain(
        model.object_types().filter_map(|def| {
            let name = def.name.as_str();
            (name != target_type && model.is_a(name, target_type)).then_some(name)
        }),
    );
    for candidate in candidates {
        let has_rule = if let Some(&cached) = type_has_row_rules.get(candidate) {
            cached
        } else {
            let found = has_row_rules_for_type(reader.conn, candidate)?;
            type_has_row_rules.insert(candidate, found);
            found
        };
        if has_rule {
            matched = true;
            break;
        }
    }
    target_matches_row_rules.insert(target_type, matched);
    Ok(matched)
}
