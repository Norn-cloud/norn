//! INBOX-1: `GET /v1/inbox`, what needs the caller's attention, as one bounded,
//! permission-scoped work queue. Studio's Home, the MCP surface and an in-app
//! agent read the same document.
//!
//! Every section is the summary of an existing read, under that read's own
//! gates, and nothing else:
//!
//! - **Held-back records, grouped by the rule that holds them.** A type is
//!   scanned only when the caller passes its collection gate (as
//!   `GET /v1/objects/{type}:quarantined`), and a record is listed only when
//!   the caller holds Population and Values on it, the quarantined list's row
//!   gate. A type the caller may not read contributes no group and no count.
//!   What the finding says (its words and the values its law compared), the
//!   record's own values and any suggested fix are disclosed only under the
//!   findings read's gate (Population, Values and Sources on the row), and
//!   every value is projected through the cell's markings (PRIVACY-1).
//! - **Decisions waiting on the caller**: the first page of
//!   `GET /v1/decisions?state=pending`.
//! - **Sources with errors**: `source.Connection` records whose last sync
//!   `failed` or was `partial`, through `GET /v1/objects/source.Connection`'s
//!   own query, gates and projection.
//!
//! A suggested fix is computed by the server from the law the finding pins,
//! never per pack: for an equality law `self.p == <rhs>`, the expected value
//! is `<rhs>` over the values the law compared (the finding's DIAG-REASON-1
//! `values`), when `<rhs>` is another value the law read (`self.q`,
//! `self.link.q`) or an exact sum of them (`self.a + self.b`,
//! `sum(map(self.role, #.q))`); any other shape suggests nothing. Setting `p`
//! to it makes the equality hold by construction. It names a governed action the
//! caller may call that upserts the record and takes `p`, and the arguments
//! the server can derive: the record id, `p`, required inputs the record
//! already holds unmarked, and an evidence input (a slice keyed by the input
//! of a `norn.SourceEvidence` type) from the evidence linked to the record for
//! `p`. It is offered only when the caller's own action gate and the action's
//! per-target authorization admit it; the call is still decided again, by the
//! kernel's laws, when it is made.
//!
//! Bounds: at most [`INBOX_SCAN_BOUND`](crate::inbox::INBOX_SCAN_BOUND) rows are scanned for held-back
//! records across all types (`approximate` past it), at most
//! [`MAX_INBOX_GROUPS`](crate::inbox::MAX_INBOX_GROUPS) groups of at most [`MAX_GROUP_RECORDS`](crate::inbox::MAX_GROUP_RECORDS) records, and
//! at most [`MAX_INBOX_DECISIONS`](crate::inbox::MAX_INBOX_DECISIONS) decisions and [`MAX_INBOX_SOURCES`](crate::inbox::MAX_INBOX_SOURCES)
//! sources.

use std::collections::{BTreeMap, BTreeSet};

use norn_cell::read::LinkDirection;
use norn_cell::{MAIN_WORLD, ObjectRecord, StoreReader, engine::SqlEngine};
use norn_ir::{
    ActionEffectProgram, ActionRoot, ArtifactEnvelope, ArtifactKind, DataQualityFinding,
    DataQualityFindingStatus, DataQualityRuleRef, Decimal, LawIR, LawResponse, PropertyType,
    RuntimeValue,
};
use serde::{Deserialize, Serialize, de::IntoDeserializer};
use utoipa::openapi::Ref;
use utoipa::{IntoParams, ToSchema};

use crate::actions::ActionQuery;
use crate::authz::ReadCapability::{Population, Sources, Values};
use crate::decisions::{DecisionDocument, DecisionsQuery, list_decisions};
use crate::host::DevHost;
use crate::object_query::ObjectQueryParams;
use crate::object_review::SOURCE_EVIDENCE;
use crate::object_routes::{
    ObjectAccess, SCAN_BATCH, collection, get_object_query_within, locked, permitted,
};
use crate::privacy::NornMaskedProperty;
use crate::refusal::Refusal;
use crate::scope::ScopeQuery;

/// Rows scanned for held-back records across every type, at most.
pub const INBOX_SCAN_BOUND: usize = 10_000;
/// Held-back groups listed, at most.
pub const MAX_INBOX_GROUPS: usize = 20;
/// Records listed in one held-back group, at most.
pub const MAX_GROUP_RECORDS: usize = 10;
/// Decisions listed, at most.
pub const MAX_INBOX_DECISIONS: u32 = 20;
/// Sources with errors listed, at most.
pub const MAX_INBOX_SOURCES: usize = 20;
/// Visible connections a sources scan reads, at most.
pub const SOURCE_SCAN_BOUND: usize = 500;
/// Links read in each direction to find what a record's law compared.
const NEIGHBOR_BOUND: usize = 64;
/// The connection type sync reports land as (SYNC-REPORT-1).
const CONNECTION_TYPE: &str = crate::sources::CONNECTION_TYPE;

/// `GET /v1/inbox` query: the scope.
#[derive(Debug, Clone, Default, Deserialize, IntoParams)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[into_params(parameter_in = Query, rename_all = "camelCase")]
pub struct InboxQuery {
    /// Organization (required).
    #[param(required = true, nullable = false)]
    pub org_id: Option<String>,
    /// Environment (required).
    #[param(required = true, nullable = false)]
    pub environment_id: Option<String>,
}

/// What one inbox section summarizes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum NornInboxKind {
    /// Records a rule holds out of authoritative reads, by rule.
    HeldBack,
    /// Open decisions the caller may decide.
    Decision,
    /// Connections whose last sync failed or was partial.
    SourceError,
}

/// One section's size.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct NornInboxSection {
    /// The section.
    pub kind: NornInboxKind,
    /// Everything the section found for the caller: held-back records (a
    /// record held by two rules counts once per rule), decisions or sources.
    pub count: u64,
    /// The items of this kind listed in `items`.
    pub shown: u64,
    /// `count` is a lower bound: a scan bound was reached.
    pub approximate: bool,
}

/// The suggested correction of one held-back record, as the server computed it.
#[derive(Debug, Clone, PartialEq, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct NornSuggestedFix {
    /// The governed action to call (`POST /v1/actions/{actionId}`).
    pub action_id: String,
    /// The action's title.
    pub title: String,
    /// What the fix does, in words: `Use 72.00`.
    pub label: String,
    /// The property the fix sets.
    pub property: String,
    /// The value the law expects, at the property's declared scale.
    #[schema(value_type = String)]
    pub value: RuntimeValue,
    /// The call's input as far as the server derives it: the record id, the
    /// property, required inputs the record holds unmarked, and the evidence
    /// the property was read from. Decimals are exact strings.
    #[schema(value_type = BTreeMap<String, serde_json::Value>)]
    pub arguments: BTreeMap<String, RuntimeValue>,
    /// Required inputs the caller still supplies (a reason, for example).
    pub needs: Vec<String>,
}

/// What a finding says about one held-back record.
#[derive(Debug, Clone, PartialEq, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct NornInboxFinding {
    /// The finding id.
    pub id: String,
    /// The finding in plain words: `total 720.00 ≠ lines 72.00`, or the
    /// finding's own message when its compared values are not disclosed.
    pub words: String,
    /// The finding's message.
    pub message: String,
    /// DIAG-REASON-1: the values the law compared, by path, as it read them,
    /// projected through the cell's markings. A path whose values are not
    /// disclosed to the caller is left out.
    #[schema(value_type = BTreeMap<String, serde_json::Value>)]
    pub compared: BTreeMap<String, RuntimeValue>,
}

/// One held-back record.
#[derive(Debug, Clone, PartialEq, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct NornHeldBackRecord {
    /// The concrete object type.
    pub object_type: String,
    /// The object id.
    pub id: String,
    /// The Studio route that reviews it.
    pub target: String,
    /// What the finding says; absent unless the caller may read the record's
    /// findings (Population, Values and Sources on it).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub finding: Option<NornInboxFinding>,
    /// Its stored values, projected through the cell's markings; absent
    /// under the same condition as `finding`.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schema(value_type = Option<BTreeMap<String, serde_json::Value>>)]
    pub values: Option<BTreeMap<String, RuntimeValue>>,
    /// PRIVACY-1: how each marked property was projected; omitted when none is.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub masked: BTreeMap<String, NornMaskedProperty>,
    /// The correction the server derived, when the caller may make it and no
    /// other open law finding holds the record: a fix one law suggests could be
    /// refused by another, so a record two laws hold is offered none.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub suggested_fix: Option<NornSuggestedFix>,
    /// The record's other open findings, under other rules, in the same words
    /// and the same gate as `finding`: when present the laws disagree and the
    /// record needs review, not a one-click fix.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub other_findings: Vec<NornInboxFinding>,
}

/// The records one rule holds back in one type.
#[derive(Debug, Clone, PartialEq, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct NornHeldBackGroup {
    /// The rule the findings pin.
    #[schema(schema_with = rule_ref_schema)]
    pub rule_ref: DataQualityRuleRef,
    /// The rule in words: a law's name, or the rule id.
    pub title: String,
    /// A law's rationale, when it declares one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rationale: Option<String>,
    /// The type whose records it holds.
    pub object_type: String,
    /// How many of its records the caller may review are held by it.
    pub count: u64,
    /// `count` is a lower bound: the scan bound was reached.
    pub approximate: bool,
    /// The Studio route that reviews them: the type's held-back records.
    pub target: String,
    /// At most [`MAX_GROUP_RECORDS`] of them, in id order.
    pub records: Vec<NornHeldBackRecord>,
}

/// A decision waiting on the caller.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct NornInboxDecision {
    /// The Studio route that decides it.
    pub target: String,
    /// The decision, as `GET /v1/decisions` lists it.
    pub decision: DecisionDocument,
}

/// A connection whose last sync failed or was partial.
#[derive(Debug, Clone, PartialEq, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct NornInboxSource {
    /// The connection id.
    pub id: String,
    /// The Studio route that shows it.
    pub target: String,
    /// The connection record's values, as `GET /v1/objects/source.Connection`
    /// serves them (projected): its label, status, last error and counts.
    #[schema(value_type = BTreeMap<String, serde_json::Value>)]
    pub values: BTreeMap<String, RuntimeValue>,
}

/// One inbox item.
#[derive(Debug, Clone, PartialEq, Serialize, ToSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum NornInboxItem {
    /// Records one rule holds back.
    HeldBack(NornHeldBackGroup),
    /// A decision waiting on the caller.
    Decision(Box<NornInboxDecision>),
    /// A connection with sync errors.
    SourceError(NornInboxSource),
}

/// `GET /v1/inbox`: what needs the caller's attention.
#[derive(Debug, Clone, PartialEq, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct NornInbox {
    /// Held-back groups (largest first), then decisions (soonest deadline
    /// first), then sources with errors.
    pub items: Vec<NornInboxItem>,
    /// Each section's size, in that order.
    pub sections: Vec<NornInboxSection>,
    /// The held-back scan bound, in rows.
    pub scan_limit: u64,
}

fn rule_ref_schema() -> Ref {
    Ref::builder()
        .ref_location_from_schema_name("NornIrDataQualityRuleRef")
        .description(Some("The rule the findings pin."))
        .build()
}

/// A path segment of a Studio route.
fn segment(text: &str) -> String {
    let mut escaped = String::with_capacity(text.len());
    for byte in text.bytes() {
        if byte.is_ascii_alphanumeric() || b"-._~:@!$&'()*+,;=".contains(&byte) {
            escaped.push(char::from(byte));
        } else {
            escaped.push('%');
            for nibble in [byte >> 4, byte & 0x0f] {
                let digit = char::from_digit(u32::from(nibble), 16).unwrap_or('0');
                escaped.push(digit.to_ascii_uppercase());
            }
        }
    }
    escaped
}

/// The record page of one object: `/sources/{type}/{id}` (the id is the rest
/// of the path, so it keeps its `/`).
fn record_target(object_type: &str, id: &str) -> String {
    let id: String = id.split('/').map(segment).collect::<Vec<_>>().join("/");
    format!("/sources/{}/{id}", segment(object_type))
}

/// `GET /v1/inbox`: the held-back records, decisions and failing sources the
/// caller may act on, each under its own read's gates. `decisions` is false
/// on a cell that decides nothing (no disclosure authority), which then lists
/// none, as `GET /v1/decisions` refuses there.
///
/// # Errors
/// Missing or foreign scope, or a caller nobody verified (one opaque 403);
/// a commit between the reads (409 `object.snapshot_moved`); a host failure.
#[utoipa::path(get, path = "/v1/inbox", operation_id = "get_inbox", params(InboxQuery), responses((status = 200, description = "What needs the caller's attention: held-back records grouped by the rule that holds them (with the fix the server derives, when the caller may make it), decisions waiting on the caller and sources with sync errors, each filtered by the gates of the read it summarizes and bounded.", body = NornInbox), (status = 400, description = "The request was refused as invalid: an unknown or repeated query parameter.", body = Refusal), (status = 403, description = "The request was refused: scope or authority denied, with the same document whatever the reason.", body = Refusal), (status = 409, description = "The request was refused (`object.snapshot_moved`, retryable): the cell committed between the reads.", body = Refusal), (status = 500, description = "The request was refused: host failure.", body = Refusal)))]
pub fn get_inbox<E: SqlEngine>(
    host: &DevHost<E>,
    query: &InboxQuery,
    actor: Option<&str>,
    access: &dyn ObjectAccess,
    (decisions, now): (bool, norn_kernel::LogicalTime),
) -> Result<NornInbox, Refusal> {
    host.scope()
        .admit(query.org_id.as_deref(), query.environment_id.as_deref())?;
    let actor = actor
        .filter(|actor| !actor.trim().is_empty())
        .ok_or_else(crate::authz::read_denied)?;
    let scope = ScopeQuery {
        org_id: query.org_id.clone(),
        environment_id: query.environment_id.clone(),
    };
    let mut items = Vec::new();
    let mut sections = Vec::new();

    let held = held_back(host, &scope, actor, access)?;
    sections.push(NornInboxSection {
        kind: NornInboxKind::HeldBack,
        count: held.records,
        shown: count_of(held.groups.len()),
        approximate: held.approximate,
    });
    items.extend(held.groups.into_iter().map(NornInboxItem::HeldBack));

    let (waiting, total, capped) = if decisions {
        let page = list_decisions(
            host,
            &DecisionsQuery {
                org_id: scope.org_id.clone(),
                environment_id: scope.environment_id.clone(),
                state: Some("pending".to_owned()),
                limit: Some(MAX_INBOX_DECISIONS),
                ..DecisionsQuery::default()
            },
            Some(actor),
            now,
        )?;
        (page.items, page.total, page.total_capped)
    } else {
        (Vec::new(), 0, false)
    };
    sections.push(NornInboxSection {
        kind: NornInboxKind::Decision,
        count: total,
        shown: count_of(waiting.len()),
        approximate: capped,
    });
    items.extend(waiting.into_iter().map(|decision| {
        NornInboxItem::Decision(Box::new(NornInboxDecision {
            target: format!("/decisions?decision={}", segment(&decision.id)),
            decision,
        }))
    }));

    let (failing, count, approximate) = failing_sources(host, &scope, actor, access);
    sections.push(NornInboxSection {
        kind: NornInboxKind::SourceError,
        count,
        shown: count_of(failing.len()),
        approximate,
    });
    items.extend(failing.into_iter().map(NornInboxItem::SourceError));

    Ok(NornInbox {
        items,
        sections,
        scan_limit: count_of(INBOX_SCAN_BOUND),
    })
}

fn count_of(len: usize) -> u64 {
    u64::try_from(len).unwrap_or(u64::MAX)
}

/// The held-back section.
struct HeldBack {
    groups: Vec<NornHeldBackGroup>,
    records: u64,
    approximate: bool,
}

/// What the definitions say about rules and fixes, read once per request.
struct Definitions {
    laws: BTreeMap<String, (ArtifactEnvelope, LawIR)>,
    /// The actions the caller may call: `Operation::CallAction` per action.
    callable: Vec<ActionRoot>,
}

fn definitions<E: SqlEngine>(
    host: &DevHost<E>,
    scope: &ScopeQuery,
    actor: &str,
) -> Result<Definitions, Refusal> {
    let Some(installed) = host.active_definitions()? else {
        return Ok(Definitions {
            laws: BTreeMap::new(),
            callable: Vec::new(),
        });
    };
    let laws = crate::kernel_set::retained_artifacts(&installed.canonical_set)
        .map_err(Refusal::host)?
        .into_iter()
        .filter(|artifact| artifact.kind() == ArtifactKind::Law)
        .filter_map(|artifact| {
            let decoded: Result<LawIR, serde::de::value::Error> =
                LawIR::deserialize(artifact.payload.to_json_value().into_deserializer());
            decoded
                .ok()
                .map(|law| (artifact.artifact_id.clone(), (artifact, law)))
        })
        .collect();
    // The catalog's own gate; a caller it denies is offered no fix.
    let callable = crate::action_catalog::callable_actions(
        host,
        &ActionQuery {
            org_id: scope.org_id.clone(),
            environment_id: scope.environment_id.clone(),
            version: None,
        },
        Some(actor),
    )
    .map(|callable| {
        callable
            .actions
            .into_iter()
            .map(|(_, action)| action)
            .collect()
    })
    .unwrap_or_default();
    Ok(Definitions { laws, callable })
}

/// One quarantined row and the open findings the caller may see.
struct Held {
    row: ObjectRecord,
    open: Vec<DataQualityFinding>,
}

/// One quarantined row and its open findings, as scanned under the lock.
type Scanned = (ObjectRecord, Vec<crate::object_links::GatedFinding>);

/// Scan `object_type` from its first row, at most `budget` rows; the held
/// rows and whether the type was read to its end.
fn scan_type<E: SqlEngine>(
    host: &DevHost<E>,
    access: &dyn ObjectAccess,
    object_type: &str,
    budget: &mut usize,
    start: &mut Option<(u64, Option<(u64, u64)>)>,
) -> Result<(Vec<Scanned>, bool), Refusal> {
    let mut after: Option<String> = None;
    let mut held = Vec::new();
    loop {
        if *budget == 0 {
            return Ok((held, false));
        }
        let want = SCAN_BATCH.min(*budget);
        let (batch, last, done) = locked(host, access, start, |reader| {
            let rows = reader
                .list_after(MAIN_WORLD, object_type, after.as_deref(), want)
                .map_err(Refusal::host)?;
            let done = rows.len() < want;
            let last = rows.last().map(|row| row.id.clone());
            let scanned = rows.len();
            let mut batch = Vec::new();
            for row in rows {
                // Rows of a subtype are scanned with their own type.
                if row.object_type != object_type
                    || !reader
                        .observation_is_quarantined(MAIN_WORLD, &row)
                        .map_err(Refusal::host)?
                {
                    continue;
                }
                let open: Vec<DataQualityFinding> = reader
                    .observation_findings(MAIN_WORLD, &row.object_type, &row.id)
                    .map_err(Refusal::host)?
                    .into_iter()
                    .filter(|finding| finding.status == DataQualityFindingStatus::Open)
                    .collect();
                if !open.is_empty() {
                    let open =
                        crate::object_links::gate_findings(reader, open).map_err(Refusal::host)?;
                    batch.push((row, open));
                }
            }
            Ok(((batch, scanned), last, done))
        })?;
        let (batch, scanned) = batch;
        *budget = budget.saturating_sub(scanned);
        held.extend(batch);
        if done {
            return Ok((held, true));
        }
        after = last;
    }
}

/// The held-back groups the caller may review, largest first.
fn held_back<E: SqlEngine>(
    host: &DevHost<E>,
    scope: &ScopeQuery,
    actor: &str,
    access: &dyn ObjectAccess,
) -> Result<HeldBack, Refusal> {
    let types: Vec<String> = {
        let cell = host.cell()?;
        let reader = cell.store().read();
        reader
            .model()
            .object_types()
            .map(|object| object.name.as_str().to_owned())
            .collect()
    };
    let definitions = definitions(host, scope, actor)?;
    let mut budget = INBOX_SCAN_BOUND;
    let mut approximate = false;
    let mut groups: Vec<NornHeldBackGroup> = Vec::new();
    let mut records = 0_u64;
    for object_type in types {
        // The collection gate of `:quarantined`: a type the caller may not
        // read is never scanned, so it adds no group and no count.
        let Ok((_epoch, _actor)) = collection(host, scope, Some(actor), &object_type, access)
        else {
            continue;
        };
        if budget == 0 {
            approximate = true;
            break;
        }
        let mut start = None;
        let (held, exhausted) = scan_type(host, access, &object_type, &mut budget, &mut start)?;
        approximate |= !exhausted;
        // The quarantined list's row gate, outside the cell lock; a held link
        // is disclosed as the link would be, and the snapshot is confirmed
        // after the policy whether or not a record below reads evidence
        // (OBS-LINK-DISPUTE-1).
        let readable: Vec<Scanned> = held
            .into_iter()
            .filter(|(row, _)| {
                [Population, Values]
                    .into_iter()
                    .all(|capability| permitted(host, access, actor, row, capability))
            })
            .collect();
        let visible: Vec<Held> =
            crate::object_links::disclosed_findings(host, access, actor, &mut start, readable)?
                .into_iter()
                .map(|(row, open)| Held { row, open })
                .filter(|held| !held.open.is_empty())
                .collect();
        let mut by_rule: BTreeMap<DataQualityRuleRef, Vec<(&Held, &DataQualityFinding)>> =
            BTreeMap::new();
        for held in &visible {
            for finding in &held.open {
                let entry = by_rule.entry(finding.rule_ref.clone()).or_default();
                // One record per rule, whatever its findings under it.
                if entry
                    .last()
                    .is_none_or(|(last, _)| last.row.id != held.row.id)
                {
                    entry.push((held, finding));
                }
            }
        }
        for (rule_ref, members) in by_rule {
            let count = count_of(members.len());
            records = records.saturating_add(count);
            let (title, rationale) = rule_words(&definitions, &rule_ref);
            let mut listed = Vec::new();
            for (held, finding) in members.into_iter().take(MAX_GROUP_RECORDS) {
                listed.push(record(
                    (host, access, actor),
                    &definitions,
                    (&held.row, finding, &held.open),
                    &mut start,
                )?);
            }
            groups.push(NornHeldBackGroup {
                target: format!("/objects/{}?population=held_back", segment(&object_type)),
                rule_ref,
                title,
                rationale,
                object_type: object_type.clone(),
                count,
                approximate: !exhausted,
                records: listed,
            });
        }
    }
    groups.sort_by(|a, b| {
        b.count
            .cmp(&a.count)
            .then_with(|| a.object_type.cmp(&b.object_type))
            .then_with(|| a.rule_ref.cmp(&b.rule_ref))
    });
    if groups.len() > MAX_INBOX_GROUPS {
        groups.truncate(MAX_INBOX_GROUPS);
    }
    Ok(HeldBack {
        groups,
        records,
        approximate,
    })
}

/// A rule's name and rationale: a law's declaration, or the rule's id.
fn rule_words(
    definitions: &Definitions,
    rule_ref: &DataQualityRuleRef,
) -> (String, Option<String>) {
    match rule_ref {
        DataQualityRuleRef::Law {
            artifact_id,
            law_id,
            ..
        } => definitions.laws.get(artifact_id).map_or_else(
            || (law_id.clone(), None),
            |(artifact, law)| (artifact.name.clone(), Some(law.rationale.clone())),
        ),
        DataQualityRuleRef::Invariant { invariant_id, .. } => (invariant_id.0.clone(), None),
        DataQualityRuleRef::Builtin { id, .. } => (id.clone(), None),
    }
}

/// What one record's review read holds, read under one snapshot.
struct Review {
    values: BTreeMap<String, RuntimeValue>,
    masked: BTreeMap<String, NornMaskedProperty>,
    /// Own properties the cell marks (or whose markings are unavailable).
    marked: Vec<String>,
    unavailable: bool,
    /// Rows linked to the record, either way, each with the properties the
    /// cell marks on it; `None` past the bound or with a marking unavailable.
    neighbors: Option<Vec<(ObjectRecord, Vec<String>)>>,
    properties: Vec<(String, Option<PropertyType>)>,
    /// Evidence objects linked to the record: their rows.
    evidence: Vec<ObjectRecord>,
    /// The record's outgoing links by role, each with its target row; `None`
    /// past the bound.
    outbound: Option<Vec<(String, ObjectRecord)>>,
    /// The laws (by artifact id) whose scope takes the record's type.
    applicable: Vec<String>,
}

fn neighbors<E: SqlEngine>(
    reader: &StoreReader<'_, E>,
    row: &ObjectRecord,
) -> Result<Neighborhood, Refusal> {
    let mut rows = Vec::new();
    let mut evidence = Vec::new();
    let mut outbound = Vec::new();
    let mut complete = true;
    for direction in [LinkDirection::In, LinkDirection::Out] {
        let page = reader
            .links_touching_page(
                MAIN_WORLD,
                &row.object_type,
                &row.id,
                direction,
                None,
                None,
                NEIGHBOR_BOUND,
            )
            .map_err(Refusal::host)?;
        complete &= !page.has_more_candidates;
        for edge in page.items {
            let (object_type, id) = if direction == LinkDirection::In {
                (edge.edge.source_type, edge.edge.source)
            } else {
                (edge.edge.target_type, edge.edge.target)
            };
            if let Some(found) = reader
                .object(MAIN_WORLD, &object_type, &id)
                .map_err(Refusal::host)?
            {
                if direction == LinkDirection::In
                    && reader.model().is_a(&found.object_type, SOURCE_EVIDENCE)
                {
                    evidence.push(found.clone());
                }
                if direction == LinkDirection::Out {
                    outbound.push((edge.edge.link.clone(), found.clone()));
                }
                rows.push(found);
            }
        }
    }
    Ok((
        complete.then_some(rows),
        evidence,
        complete.then_some(outbound),
    ))
}

/// The other laws a fix `property := value` would have to keep, tried on the
/// record's values with the fix applied, the finding's compared values and
/// the record's outgoing links by role (`role.property`). Each law it would
/// break is returned in words; one this cannot decide (an unreadable value, a
/// condition other than `self.role != null`, a link past the bound) is
/// returned as its rationale, so no fix is offered either way.
#[allow(
    clippy::too_many_lines,
    reason = "one dry run: the values it tries, then each other law in turn"
)]
fn dry_run(
    definitions: &Definitions,
    review: &Review,
    finding: &DataQualityFinding,
    agreeing: &BTreeSet<String>,
    (property, value): (&str, &RuntimeValue),
) -> Vec<NornInboxFinding> {
    let current = match &finding.rule_ref {
        DataQualityRuleRef::Law { artifact_id, .. } => Some(artifact_id.as_str()),
        DataQualityRuleRef::Invariant { .. } | DataQualityRuleRef::Builtin { .. } => None,
    };
    let Some(outbound) = &review.outbound else {
        // Past the link bound nothing can be decided: no other law is checked, so no fix.
        return vec![undecided(
            finding.id.as_str(),
            "This record has too many links to check a fix against its other laws.",
        )];
    };
    let marked_linked: BTreeSet<(String, String)> = review
        .neighbors
        .iter()
        .flatten()
        .flat_map(|(row, marked)| {
            marked
                .iter()
                .map(|property| (row.id.clone(), property.clone()))
        })
        .collect();
    let mut values = review.values.clone();
    values.insert(property.to_owned(), value.clone());
    for (path, compared) in &finding.values {
        values
            .entry(path.clone())
            .or_insert_with(|| compared.clone());
    }
    let mut linked: BTreeMap<String, Vec<RuntimeValue>> = BTreeMap::new();
    for (role, row) in outbound {
        for (name, held) in &row.values {
            if !marked_linked.contains(&(row.id.clone(), name.clone())) {
                linked
                    .entry(format!("{role}.{name}"))
                    .or_default()
                    .push(held.clone());
            }
        }
    }
    for (path, held) in linked {
        let entry = match <[RuntimeValue; 1]>::try_from(held) {
            Ok([one]) => one,
            Err(many) => RuntimeValue::Array(many),
        };
        values.entry(path).or_insert(entry);
    }
    let mut broken = Vec::new();
    for id in &review.applicable {
        // This finding's own law, and laws whose open findings expect this same fix.
        if Some(id.as_str()) == current || agreeing.contains(id) {
            continue;
        }
        let Some((_, law)) = definitions.laws.get(id) else {
            continue;
        };
        let LawResponse::Predicate { expression } = &law.response else {
            continue;
        };
        let condition = law
            .condition
            .as_ref()
            .map(|condition| condition.as_str().to_owned());
        if !reads(expression.as_str(), property)
            && !condition
                .as_deref()
                .is_some_and(|text| reads(text, property))
        {
            continue;
        }
        let undecidable = || undecided(id, &law.rationale);
        // A condition this can decide: `self.role != null`.
        if let Some(text) = &condition {
            let compact = normalized(text);
            let Some(role) = compact
                .strip_prefix("self.")
                .and_then(|rest| rest.strip_suffix("!=null"))
            else {
                broken.push(undecidable());
                continue;
            };
            let held = values
                .get(role)
                .is_some_and(|held| !matches!(held, RuntimeValue::Null))
                || outbound.iter().any(|(link, _)| link == role);
            if !held {
                continue;
            }
        }
        let Some((left, right)) = split_equality(expression.as_str()) else {
            broken.push(undecidable());
            continue;
        };
        let Some(own) = own_property(left) else {
            broken.push(undecidable());
            continue;
        };
        match (values.get(own), right_value(right, &values)) {
            (Some(actual), Some(want)) if same_number(actual, &want) => {}
            (Some(_), Some(want)) => broken.push(NornInboxFinding {
                id: id.clone(),
                words: words(own, &want, &values, &review.properties),
                message: law.rationale.clone(),
                compared: BTreeMap::new(),
            }),
            _ => broken.push(undecidable()),
        }
    }
    broken
}

/// Two values equal as exact numbers (`72` and `72.00`), or as themselves.
fn same_number(left: &RuntimeValue, right: &RuntimeValue) -> bool {
    match (left, right) {
        (RuntimeValue::Number(a), RuntimeValue::Number(b)) => {
            a.canonical() == b.canonical() || a == b
        }
        _ => left == right,
    }
}

fn undecided(id: &str, words: &str) -> NornInboxFinding {
    NornInboxFinding {
        id: id.to_owned(),
        words: words.to_owned(),
        message: words.to_owned(),
        compared: BTreeMap::new(),
    }
}

/// A record's linked rows (either way, `None` past the bound), the evidence
/// among them, and its outgoing links by role (`None` past the bound).
type Neighborhood = (
    Option<Vec<ObjectRecord>>,
    Vec<ObjectRecord>,
    Option<Vec<(String, ObjectRecord)>>,
);

/// One record of a group: its id and route for every caller who may list it;
/// its finding, values and fix only under the findings read's gate.
#[allow(
    clippy::too_many_lines,
    reason = "one review read: its gate, its snapshot, then each disclosure in order"
)]
fn record<E: SqlEngine>(
    (host, access, actor): (&DevHost<E>, &dyn ObjectAccess, &str),
    definitions: &Definitions,
    (row, finding, open): (&ObjectRecord, &DataQualityFinding, &[DataQualityFinding]),
    start: &mut Option<(u64, Option<(u64, u64)>)>,
) -> Result<NornHeldBackRecord, Refusal> {
    let mut listed = NornHeldBackRecord {
        object_type: row.object_type.clone(),
        id: row.id.clone(),
        target: record_target(&row.object_type, &row.id),
        finding: None,
        values: None,
        masked: BTreeMap::new(),
        suggested_fix: None,
        other_findings: Vec::new(),
    };
    if !permitted(host, access, actor, row, Sources) {
        return Ok(listed);
    }
    let review = locked(host, access, start, |reader| {
        let catalog = host.marking_catalog(reader)?;
        let markings = crate::privacy::row_markings(reader, &catalog, row)?;
        let mut projected = row.clone();
        let outcome =
            crate::privacy::project(host, actor, &catalog, markings.as_ref(), &mut projected);
        let marked = markings
            .as_ref()
            .map(|markings| {
                markings
                    .iter()
                    .filter(|(_, ids)| !ids.is_empty())
                    .map(|(property, _)| property.clone())
                    .collect()
            })
            .unwrap_or_default();
        let (neighbors, evidence, outbound) =
            if finding.values.keys().any(|path| path.contains('.'))
                || !definitions.callable.is_empty()
            {
                neighbors(reader, row)?
            } else {
                (Some(Vec::new()), Vec::new(), Some(Vec::new()))
            };
        // The laws whose scope takes this record's type (its own, a supertype or an interface).
        let applicable = definitions
            .laws
            .iter()
            .filter(|(_, (_, law))| {
                law.scope
                    .object_types
                    .iter()
                    .chain(std::iter::once(&law.subject))
                    .any(|scoped| {
                        scoped == &row.object_type || reader.model().is_a(&row.object_type, scoped)
                    })
            })
            .map(|(id, _)| id.clone())
            .collect();
        // Each linked row with the properties the cell marks on it; a row
        // whose marking snapshot is unavailable discloses no linked value.
        let neighbors = match neighbors {
            Some(rows) => {
                let mut marked_rows = Some(Vec::new());
                for neighbor in rows {
                    let Some(marks) = crate::privacy::row_markings(reader, &catalog, &neighbor)?
                    else {
                        marked_rows = None;
                        break;
                    };
                    let marked: Vec<String> = marks
                        .into_iter()
                        .filter(|(_, ids)| !ids.is_empty())
                        .map(|(property, _)| property)
                        .collect();
                    if let Some(rows) = marked_rows.as_mut() {
                        rows.push((neighbor, marked));
                    }
                }
                marked_rows
            }
            None => None,
        };
        Ok(Review {
            values: projected.values.into_iter().collect(),
            masked: outcome.masked,
            marked,
            unavailable: markings.is_none(),
            neighbors,
            properties: reader
                .model()
                .effective_property_types(&row.object_type)
                .map_err(Refusal::host)?,
            evidence,
            outbound,
            applicable,
        })
    })?;
    if review.unavailable {
        // An unavailable marking snapshot discloses nothing value-derived.
        listed.finding = Some(NornInboxFinding {
            id: finding.id.clone(),
            words: finding.message.clone(),
            message: finding.message.clone(),
            compared: BTreeMap::new(),
        });
        return Ok(listed);
    }
    // Whether the caller may read each linked row.
    let readable: Option<Vec<bool>> = review.neighbors.as_ref().map(|rows| {
        rows.iter()
            .map(|(neighbor, _)| {
                [Population, Values]
                    .into_iter()
                    .all(|capability| permitted(host, access, actor, neighbor, capability))
            })
            .collect()
    });
    // One finding in words: what its law expected, when every value it compared
    // is disclosed as is; otherwise its own message.
    let read = |finding: &DataQualityFinding| {
        let compared = disclosed(&finding.values, &review, readable.as_deref());
        // A fix is computed from raw values: only when every one is disclosed as is.
        let complete = compared.len() == finding.values.len()
            && !compared.keys().any(|path| review.marked.contains(path));
        let law = match &finding.rule_ref {
            DataQualityRuleRef::Law { artifact_id, .. } => {
                definitions.laws.get(artifact_id).map(|(_, law)| law)
            }
            DataQualityRuleRef::Invariant { .. } | DataQualityRuleRef::Builtin { .. } => None,
        };
        let derived = law
            .filter(|_| complete)
            .and_then(|law| expected(law, &finding.values));
        let said = derived.as_ref().map_or_else(
            || finding.message.clone(),
            |(property, value)| words(property, value, &finding.values, &review.properties),
        );
        (compared, derived, said)
    };
    let (compared, derived, words) = read(finding);
    // The record's open findings under other rules. One that expects the same
    // property to take the same value agrees with this fix (two laws, one
    // correction); any other disagrees, and the record needs review.
    let mut agreeing = BTreeSet::new();
    let mut others: Vec<NornInboxFinding> = Vec::new();
    for other in open
        .iter()
        .filter(|other| other.rule_ref != finding.rule_ref)
    {
        let (compared, wants, words) = read(other);
        let agrees = matches!((&derived, &wants), (Some((property, value)), Some((wanted, expected)))
            if property == wanted && same_number(value, expected));
        if agrees {
            if let DataQualityRuleRef::Law { artifact_id, .. } = &other.rule_ref {
                agreeing.insert(artifact_id.clone());
            }
        } else {
            others.push(NornInboxFinding {
                id: other.id.clone(),
                words,
                message: other.message.clone(),
                compared,
            });
        }
    }
    // The fix, tried against every other law that takes the record and reads the
    // property it sets: a law it would break (or one this cannot decide) means
    // the record needs review, not a one-click fix.
    if let Some((property, value)) = &derived
        && others.is_empty()
    {
        others.extend(dry_run(
            definitions,
            &review,
            finding,
            &agreeing,
            (property, value),
        ));
    }
    if let Some((property, value)) = derived
        && others.is_empty()
        && !review.marked.contains(&property)
    {
        listed.suggested_fix = fix(
            (host, access, actor),
            definitions,
            row,
            (&property, &value),
            &review,
        );
    }
    listed.finding = Some(NornInboxFinding {
        id: finding.id.clone(),
        words,
        message: finding.message.clone(),
        compared,
    });
    listed.other_findings = others;
    listed.values = Some(review.values);
    listed.masked = review.masked;
    Ok(listed)
}

/// The compared values the caller may see: the record's own properties
/// projected as its values are, and a linked path (`lines.amount`) only when
/// every linked row that carries the property is readable by the caller and
/// does not mark it. Every linked row is considered, either way, so the rows
/// the law read are among them.
fn disclosed(
    values: &BTreeMap<String, RuntimeValue>,
    review: &Review,
    readable: Option<&[bool]>,
) -> BTreeMap<String, RuntimeValue> {
    values
        .iter()
        .filter_map(|(path, value)| match path.split_once('.') {
            None if review.marked.contains(path) => review
                .values
                .get(path)
                .map(|projected| (path.clone(), projected.clone())),
            None => Some((path.clone(), value.clone())),
            Some((_role, property)) => {
                let rows = review.neighbors.as_ref()?;
                let readable = readable?;
                let mut carriers = rows
                    .iter()
                    .zip(readable)
                    .filter(|((row, _), _)| row.values.contains_key(property))
                    .peekable();
                carriers.peek()?;
                carriers
                    .all(|((_, marked), readable)| {
                        *readable && !marked.iter().any(|marked| marked == property)
                    })
                    .then(|| (path.clone(), value.clone()))
            }
        })
        .collect()
}

/// The top-level `==` of a predicate: its left and right text, when there is
/// exactly one outside brackets and string literals.
fn split_equality(source: &str) -> Option<(&str, &str)> {
    let mut depth = 0_i32;
    let mut quote: Option<char> = None;
    let mut found = None;
    let mut chars = source.char_indices().peekable();
    let mut previous: Option<char> = None;
    while let Some((at, c)) = chars.next() {
        if let Some(open) = quote {
            if c == '\\' {
                chars.next();
            } else if c == open {
                quote = None;
            }
            previous = Some(c);
            continue;
        }
        match c {
            '\'' | '"' | '`' => quote = Some(c),
            '(' | '[' | '{' => depth = depth.saturating_add(1),
            ')' | ']' | '}' => depth = depth.saturating_sub(1),
            '=' if depth == 0
                && chars.peek().is_some_and(|(_, next)| *next == '=')
                && !previous.is_some_and(|before| matches!(before, '!' | '<' | '>' | '=')) =>
            {
                chars.next();
                if chars.peek().is_some_and(|(_, next)| *next == '=') || found.is_some() {
                    return None;
                }
                found = Some(at);
            }
            _ => {}
        }
        previous = Some(c);
    }
    let at = found?;
    let (left, rest) = source.split_at(at);
    Some((left, rest.strip_prefix("==")?))
}

/// `self.<property>` alone.
fn own_property(text: &str) -> Option<&str> {
    let name = text.trim().strip_prefix("self.")?;
    let mut chars = name.chars();
    let first = chars.next()?;
    (first.is_ascii_alphabetic() || first == '_')
        .then_some(())
        .filter(|()| chars.all(|c| c.is_ascii_alphanumeric() || c == '_'))
        .map(|()| name)
}

/// `text` without whitespace and without one pair of enclosing parentheses.
fn normalized(text: &str) -> String {
    let compact: String = text.chars().filter(|c| !c.is_whitespace()).collect();
    match compact
        .strip_prefix('(')
        .and_then(|inner| inner.strip_suffix(')'))
    {
        // Only when the outer pair encloses everything: `(a)+(b)` keeps its own.
        Some(inner) if balanced(inner) => inner.to_owned(),
        Some(_) | None => compact,
    }
}

/// Whether every bracket in `text` closes in order.
fn balanced(text: &str) -> bool {
    let mut depth = 0_usize;
    for c in text.chars() {
        match c {
            '(' => depth = depth.saturating_add(1),
            ')' if depth == 0 => return false,
            ')' => depth = depth.saturating_sub(1),
            _ => {}
        }
    }
    depth == 0
}

/// The exact sum of decimals; `None` for anything else, or an empty list.
fn exact_sum<'v>(values: impl IntoIterator<Item = &'v RuntimeValue>) -> Option<Decimal> {
    let mut total: Option<Decimal> = None;
    for value in values {
        let RuntimeValue::Number(number) = value else {
            return None;
        };
        total = Some(match total {
            Some(sum) => sum.checked_add(*number)?,
            None => *number,
        });
    }
    total
}

/// What the right-hand side of `self.p == <rhs>` is, over the values the law
/// compared, for the forms an equality law takes: another value it read
/// (`self.q`, `self.link.q`), the sum over an inverse role
/// (`sum(map(self.role, #.q))`), or a sum of values (`self.a + self.b`).
/// Nothing is guessed: any other right-hand side has no value here.
fn right_value(rhs: &str, values: &BTreeMap<String, RuntimeValue>) -> Option<RuntimeValue> {
    let rhs = normalized(rhs);
    if let Some(inner) = rhs
        .strip_prefix("sum(map(self.")
        .and_then(|rest| rest.strip_suffix("))"))
    {
        let (role, read) = inner.split_once(",#.")?;
        let RuntimeValue::Array(items) = values.get(&format!("{role}.{read}"))? else {
            return None;
        };
        return exact_sum(items).map(RuntimeValue::Number);
    }
    let terms: Vec<&str> = rhs.split('+').collect();
    let paths: Option<Vec<&RuntimeValue>> = terms
        .iter()
        .map(|term| {
            let path = term.strip_prefix("self.")?;
            path.chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '.')
                .then_some(())?;
            values
                .get(path)
                .filter(|value| !matches!(value, RuntimeValue::Array(_)))
        })
        .collect();
    match paths?.as_slice() {
        [single] => Some((*single).clone()),
        many => exact_sum(many.iter().copied()).map(RuntimeValue::Number),
    }
}

/// Whether `text` reads `self.<property>` (and not a longer name).
fn reads(text: &str, property: &str) -> bool {
    let needle = format!("self.{property}");
    text.match_indices(&needle).any(|(at, _)| {
        text.get(at.saturating_add(needle.len())..)
            .and_then(|rest| rest.chars().next())
            .is_none_or(|next| !(next.is_ascii_alphanumeric() || next == '_'))
    })
}

/// The property a predicate law sets and the value it expects: for
/// `self.p == <rhs>`, `<rhs>` computed from the values the law compared
/// ([`right_value`]). `None` when the law has another shape, its condition
/// reads `p`, or the record already holds that value.
fn expected(
    law: &LawIR,
    values: &BTreeMap<String, RuntimeValue>,
) -> Option<(String, RuntimeValue)> {
    let LawResponse::Predicate { expression } = &law.response else {
        return None;
    };
    let (left, right) = split_equality(expression.as_str())?;
    let property = own_property(left)?.to_owned();
    if reads(right, &property)
        || law
            .condition
            .as_ref()
            .is_some_and(|condition| reads(condition.as_str(), &property))
    {
        return None;
    }
    let actual = values.get(&property)?;
    let value = right_value(right, values)?;
    (value != *actual).then_some((property, value))
}

/// A decimal's text at `scale` fraction digits, when it has no more.
fn at_scale(text: &str, scale: Option<u64>) -> String {
    let Some(scale) = scale.and_then(|scale| usize::try_from(scale).ok()) else {
        return text.to_owned();
    };
    let (whole, fraction) = text.split_once('.').unwrap_or((text, ""));
    if fraction.len() > scale
        || whole.is_empty()
        || !whole
            .trim_start_matches('-')
            .chars()
            .all(|c| c.is_ascii_digit())
        || !fraction.chars().all(|c| c.is_ascii_digit())
    {
        return text.to_owned();
    }
    if scale == 0 {
        return whole.to_owned();
    }
    format!("{whole}.{fraction:0<scale$}")
}

fn decimal_scale(properties: &[(String, Option<PropertyType>)], property: &str) -> Option<u64> {
    properties
        .iter()
        .find(|(name, _)| name == property)
        .and_then(|(_, kind)| match kind {
            Some(PropertyType::Decimal { scale, .. }) => Some(scale.0),
            _ => None,
        })
}

fn shown(value: &RuntimeValue, scale: Option<u64>) -> String {
    match value {
        RuntimeValue::Number(number) => at_scale(&number.canonical(), scale),
        RuntimeValue::String(text) => text.clone(),
        RuntimeValue::Bool(flag) => flag.to_string(),
        RuntimeValue::Null | RuntimeValue::Array(_) | RuntimeValue::Object(_) => String::new(),
    }
}

/// `total 720.00 ≠ lines 72.00`: what the record holds against what its law
/// expects, named by the one other path the law compared when there is one.
fn words(
    property: &str,
    value: &RuntimeValue,
    values: &BTreeMap<String, RuntimeValue>,
    properties: &[(String, Option<PropertyType>)],
) -> String {
    let scale = decimal_scale(properties, property);
    let actual = values
        .get(property)
        .map_or_else(String::new, |actual| shown(actual, scale));
    let others: Vec<&String> = values.keys().filter(|path| *path != property).collect();
    let expected = shown(value, scale);
    match others.as_slice() {
        [path] => {
            let label = match values.get(*path) {
                Some(RuntimeValue::Array(_)) => {
                    path.split_once('.').map_or(path.as_str(), |(role, _)| role)
                }
                _ => path.as_str(),
            };
            format!("{property} {actual} ≠ {label} {expected}")
        }
        _ => format!("{property} {actual} ≠ {expected}"),
    }
}

/// The input field a slice keys by, when it is `{"property": {"from": "input", "property": F}}`.
fn slice_input(key: &norn_ir::SliceKey) -> Option<String> {
    let key = serde_json::to_value(key).ok()?;
    let property = key.get("property")?;
    (property.get("from")?.as_str()? == "input")
        .then(|| property.get("property")?.as_str().map(str::to_owned))
        .flatten()
}

/// The fix: an action the caller may call that upserts this record and takes
/// `property`, with the arguments the server derives, admitted by the
/// action's per-target authorization.
fn fix<E: SqlEngine>(
    (host, access, actor): (&DevHost<E>, &dyn ObjectAccess, &str),
    definitions: &Definitions,
    row: &ObjectRecord,
    (property, value): (&str, &RuntimeValue),
    review: &Review,
) -> Option<NornSuggestedFix> {
    let scale = decimal_scale(&review.properties, property);
    // A decimal travels as its exact text at the property's scale.
    let argument = match value {
        RuntimeValue::Number(number) if scale.is_some() => {
            RuntimeValue::String(at_scale(&number.canonical(), scale))
        }
        RuntimeValue::Number(_) | RuntimeValue::String(_) | RuntimeValue::Bool(_) => value.clone(),
        RuntimeValue::Null | RuntimeValue::Array(_) | RuntimeValue::Object(_) => return None,
    };
    let mut candidates: Vec<&ActionRoot> = definitions
        .callable
        .iter()
        .filter(|action| match &action.effect_program {
            Some(ActionEffectProgram::UpsertInputObject { object_kind, .. }) => {
                object_kind.as_str() == row.object_type
            }
            Some(
                ActionEffectProgram::FireTransition { .. }
                | ActionEffectProgram::MutateParentWithRows { .. },
            )
            | None => false,
        })
        .collect();
    candidates.sort_by(|a, b| a.action_id.cmp(&b.action_id));
    candidates.into_iter().find_map(|action| {
        offer(
            (host, access, actor),
            action,
            row,
            (property, &argument),
            review,
        )
    })
}

#[allow(
    clippy::too_many_lines,
    reason = "each argument the server derives, in order, then the action's own gate"
)]
fn offer<E: SqlEngine>(
    (host, access, actor): (&DevHost<E>, &dyn ObjectAccess, &str),
    action: &ActionRoot,
    row: &ObjectRecord,
    (property, value): (&str, &RuntimeValue),
    review: &Review,
) -> Option<NornSuggestedFix> {
    let Some(ActionEffectProgram::UpsertInputObject {
        object_id_field, ..
    }) = &action.effect_program
    else {
        return None;
    };
    let schema = serde_json::to_value(&action.input_schema).ok()?;
    let fields = schema.get("properties")?.as_object()?;
    let derived = norn_kernel::actor_derived_fields(action);
    if !fields.contains_key(property) || derived.contains(property) {
        return None;
    }
    let required: Vec<String> = schema
        .get("required")
        .and_then(serde_json::Value::as_array)
        .map(|required| {
            required
                .iter()
                .filter_map(serde_json::Value::as_str)
                .filter(|field| !derived.contains(*field))
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default();
    let mut arguments = BTreeMap::from([
        (
            object_id_field.as_str().to_owned(),
            RuntimeValue::String(row.id.clone()),
        ),
        (property.to_owned(), value.clone()),
    ]);
    let mut needs = Vec::new();
    for field in required {
        if arguments.contains_key(&field) {
            continue;
        }
        // An evidence input: the evidence linked to the record for `property`.
        let evidence = action
            .slice
            .iter()
            .filter(|slice| slice_input(&slice.key).as_deref() == Some(field.as_str()))
            .find_map(|slice| {
                review
                    .evidence
                    .iter()
                    .filter(|evidence| {
                        evidence.object_type == slice.object_type.as_str()
                            && evidence
                                .values
                                .get("subject_property")
                                .and_then(RuntimeValue::as_str)
                                == Some(property)
                            && [Population, Values].into_iter().all(|capability| {
                                permitted(host, access, actor, evidence, capability)
                            })
                    })
                    .map(|evidence| evidence.id.clone())
                    .min()
            });
        if let Some(evidence) = evidence {
            arguments.insert(field, RuntimeValue::String(evidence));
            continue;
        }
        // An input the record already holds, unmarked.
        match row.values.get(&field) {
            Some(held)
                if !matches!(held, RuntimeValue::Null) && !review.marked.contains(&field) =>
            {
                arguments.insert(field, held.clone());
            }
            _ => needs.push(field),
        }
    }
    // The action's own per-target authorization, as the kernel asks it.
    let scope = BTreeMap::from([
        ("orgId".to_owned(), host.scope().org_id.clone()),
        (
            "environmentId".to_owned(),
            host.scope().environment_id.clone(),
        ),
    ]);
    let target = norn_kernel::FactKey {
        object_type: norn_ir::ObjectTypeName(row.object_type.clone()),
        key: row.id.clone(),
    };
    let values: norn_ir::RuntimeObject = row.values.clone();
    host.authorizer
        .action(&norn_kernel::ActionAuthzRequest {
            action,
            scope: &scope,
            actor,
            input: &arguments,
            target: &target,
            target_values: Some(&values),
        })
        .ok()?;
    let label = match value {
        RuntimeValue::String(text) => format!("Use {text}"),
        RuntimeValue::Null
        | RuntimeValue::Bool(_)
        | RuntimeValue::Number(_)
        | RuntimeValue::Array(_)
        | RuntimeValue::Object(_) => {
            let text = value
                .canonical_json()
                .map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
                .unwrap_or_default();
            format!("Use {text}")
        }
    };
    Some(NornSuggestedFix {
        action_id: action.action_id.clone(),
        title: action
            .title
            .clone()
            .unwrap_or_else(|| action.action_id.clone()),
        label,
        property: property.to_owned(),
        value: value.clone(),
        arguments,
        needs,
    })
}

/// The error code of a sync whose connector is not installed: that
/// connection is not set up yet, which is not a failure (Studio's Sources
/// page reads it as "Not set up").
const CONNECTOR_MISSING: &str = "source.connector_missing";

/// Connections whose last sync failed or was partial, through the
/// connection type's own grid query; nothing when the caller may not list
/// them or the cell declares no connections. A connection whose connector
/// is not installed is not set up, so it is neither listed nor counted.
fn failing_sources<E: SqlEngine>(
    host: &DevHost<E>,
    scope: &ScopeQuery,
    actor: &str,
    access: &dyn ObjectAccess,
) -> (Vec<NornInboxSource>, u64, bool) {
    let query = |clauses: Vec<serde_json::Value>, limit: usize| {
        let params = ObjectQueryParams {
            limit: Some(limit.to_string()),
            filter: Some(serde_json::json!({ "all": clauses }).to_string()),
            ..ObjectQueryParams::default()
        };
        get_object_query_within(
            host,
            (scope, Some(actor)),
            CONNECTION_TYPE,
            &params,
            access,
            SOURCE_SCAN_BOUND,
        )
    };
    let totals = |page: &crate::objects::NornObjectQueryPage| {
        page.total.as_ref().map_or_else(
            || (count_of(page.records.len()), false),
            |total| (total.count, total.approximate),
        )
    };
    let failing =
        || serde_json::json!({ "property": "status", "op": "in", "value": ["failed", "partial"] });
    // A failed sync always carries its error code and a partial one never
    // does; the filter grammar has no "or", so the not-set-up connections
    // are counted apart and subtracted, and skipped in the listed page.
    let not_set_up = query(
        vec![
            failing(),
            serde_json::json!({ "property": "last_error_code", "op": "eq", "value": CONNECTOR_MISSING }),
        ],
        1,
    );
    let skipped = not_set_up.as_ref().map_or((0, false), totals);
    let limit = usize::try_from(skipped.0)
        .unwrap_or(usize::MAX)
        .saturating_add(MAX_INBOX_SOURCES)
        .min(crate::object_query::MAX_LIMIT);
    let page = match (query(vec![failing()], limit), not_set_up) {
        (Ok(page), Ok(_)) => page,
        // Past the scan bound: there are more connections than one scan.
        (Err(refusal), _) | (_, Err(refusal)) if refusal.code == "object.scan_bound_exceeded" => {
            return (Vec::new(), 0, true);
        }
        _ => return (Vec::new(), 0, false),
    };
    let (count, approximate) = totals(&page);
    let items = page
        .records
        .into_iter()
        .filter_map(|record| {
            let values = record.values?;
            let not_set_up = matches!(
                values.get("last_error_code"),
                Some(RuntimeValue::String(code)) if code == CONNECTOR_MISSING
            );
            (!not_set_up).then(|| NornInboxSource {
                target: record_target(&record.object_type, &record.id),
                id: record.id,
                values,
            })
        })
        .take(MAX_INBOX_SOURCES)
        .collect();
    (
        items,
        count.saturating_sub(skipped.0),
        approximate || skipped.1,
    )
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    reason = "unit correspondence"
)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    #[test]
    fn a_predicate_splits_at_its_one_top_level_equality() {
        assert_eq!(
            split_equality("self.total == sum(map(self.lines, #.amount))"),
            Some(("self.total ", " sum(map(self.lines, #.amount))"))
        );
        assert_eq!(split_equality("self.a != self.b"), None);
        assert_eq!(split_equality("self.a >= self.b"), None);
        assert_eq!(split_equality("self.a == self.b and self.c == 1"), None);
        assert_eq!(split_equality("f(self.a == 1)"), None);
        assert_eq!(split_equality("self.a == '=='"), Some(("self.a ", " '=='")));
        assert_eq!(own_property(" self.total "), Some("total"));
        assert_eq!(own_property("self.lines.amount"), None);
    }

    fn law(expression: &str) -> LawIR {
        serde_json::from_value(serde_json::json!({
            "condition": "len(self.lines) > 0",
            "id": "canon.ItemizedDocument.total_matches_lines",
            "lawVersion": "0.1",
            "modality": { "kind": "always" },
            "owner": "canon",
            "provenance": { "source": "authoring_sdk", "sourcePath": "x.ts" },
            "rationale": "r",
            "response": { "expression": expression, "kind": "predicate" },
            "scope": { "objectTypes": ["canon.ItemizedDocument"], "root": { "id": "canon.ItemizedDocument", "kind": "entity" } },
            "severity": "error",
            "subject": "canon.ItemizedDocument"
        }))
        .unwrap()
    }

    #[test]
    fn only_an_equality_on_an_own_property_with_a_computable_side_suggests_a_value() {
        let number = |text: &str| RuntimeValue::from_json_text(text).unwrap();
        let values = BTreeMap::from([
            ("total".to_owned(), number("720")),
            ("ledger_entry.amount".to_owned(), number("72")),
        ]);
        assert_eq!(
            expected(&law("self.total == self.ledger_entry.amount"), &values),
            Some(("total".to_owned(), number("72")))
        );
        assert_eq!(
            expected(&law("self.total <= self.ledger_entry.amount"), &values),
            None
        );
        assert_eq!(
            expected(&law("self.total == self.ledger_entry.amount * 2"), &values),
            None
        );
        assert_eq!(expected(&law("self.total == self.total"), &values), None);
        assert_eq!(
            expected(&law("self.missing == self.ledger_entry.amount"), &values),
            None
        );
    }

    proptest! {
        /// A total held to the sum of its lines is fixed to that exact sum,
        /// at the property's scale; a total that already matches offers nothing.
        #[test]
        fn the_expected_total_is_the_exact_sum_of_the_lines(
            cents in proptest::collection::vec(0_u32..1_000_000, 1..8),
            off in 1_u32..1000,
        ) {
            let law = law("self.total == sum(map(self.lines, #.amount))");
            let text = |cents: u64| format!("{}.{:02}", cents / 100, cents % 100);
            let sum: u64 = cents.iter().map(|c| u64::from(*c)).sum();
            let lines = RuntimeValue::from_json_text(format!(
                "[{}]",
                cents.iter().map(|c| text(u64::from(*c))).collect::<Vec<_>>().join(",")
            )).unwrap();
            let wrong = RuntimeValue::from_json_text(text(sum + u64::from(off))).unwrap();
            let values = BTreeMap::from([
                ("total".to_owned(), wrong),
                ("lines.amount".to_owned(), lines.clone()),
            ]);
            let (property, value) = expected(&law, &values).unwrap();
            prop_assert_eq!(property.as_str(), "total");
            prop_assert_eq!(shown(&value, Some(2)), text(sum));
            let right = BTreeMap::from([
                ("total".to_owned(), RuntimeValue::from_json_text(text(sum)).unwrap()),
                ("lines.amount".to_owned(), lines),
            ]);
            prop_assert_eq!(expected(&law, &right), None);
        }

        #[test]
        fn a_decimal_is_shown_at_its_scale(whole in 0_u64..1_000_000, fraction in 0_u8..100) {
            prop_assert_eq!(at_scale(&whole.to_string(), Some(2)), format!("{whole}.00"));
            let text = format!("{whole}.{fraction:02}");
            prop_assert_eq!(&at_scale(&text, Some(2)), &text);
            prop_assert_eq!(&at_scale(&text, Some(1)), &text);
        }
    }
}
