//! Committed-state reads of a cell store, shared by the public read API and
//! by the commit path (the guard and the writer read through the same
//! functions inside the commit transaction).

use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;

use norn_ir::RuntimeValue as Value;
use norn_projections::sqlite_ddl::{
    ID_COLUMN, OVERFLOW_COLUMN, ObjectTable, SCOPE_COLUMN, SOURCE_COLUMN, TARGET_COLUMN,
    TYPE_COLUMN, VERSION_COLUMN, WORLD_COLUMN, quote_identifier,
};
use norn_projections::{PropertyColumn, ValueEncoding};
use serde::{Deserialize, Serialize};

use crate::codec::{
    DecodeContext, ValueOwner, canonicalize_stored_value, decode_stored_value, hash_json,
};
use crate::engine::{
    DefaultEngine, MAX_PARAMETERS, MAX_SAFE_INTEGER, MAX_STATEMENT_BYTES, SqlEngine, SqlRow,
    SqlValue, params,
};
use crate::error::CellError;
use crate::model::Model;
use crate::reactive::DecodeBudget;
use crate::store::InstalledDefinitionSet;
use crate::store::{Consumption, OutboxCommand, Receipt, ReceiptBody, TimerEntry};
use crate::system::{
    DEFINITION_ACTIVE, DEFINITION_ADMISSION_RECEIPT, DEFINITION_LAYOUT, DEFINITION_REGISTRY,
    DEFINITION_VERSION, GENESIS_HASH, INBOX, LEDGER, LINK_CREATION, LINK_VERSIONS, MARKING_CURRENT,
    MARKING_OCCURRENCE, MARKING_SNAPSHOT, MARKING_SUBJECT, MARKING_VERSION, OUTBOX,
    PENDING_LINK_INDEX, PERFORM_CLAIM, RECEIPT, RUN, TIMER, VERSIONS, marking_subject,
};

const MAX_LINK_PAGE_CANDIDATES: usize = 1_000;
const MAX_LINK_PAGE_BYTES: u64 = 16 * 1024 * 1024;
const LINK_CANDIDATE_ROW_OVERHEAD: u64 = 64;
const MAX_LINK_RECEIPT_LOOKUP_ROWS: usize = 4_096;
const MAX_LINK_RECEIPT_LOOKUP_BYTES: u64 = 8 * 1024 * 1024;
const LINK_RECEIPT_LOOKUP_BATCH: usize = 64;
const MAX_MARKING_SUMMARY_VARIANTS: usize = 4_096;

type LinkReceiptIdentity = (String, String, String, u64);
type LinkTargetVersion = (String, String, u64);

fn link_receipt_identity(link: &LinkEdgeRecord, version: u64) -> LinkReceiptIdentity {
    (
        link.position.link_table.clone(),
        link.edge.source.clone(),
        link.edge.target.clone(),
        version,
    )
}

/// One ledger entry.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LedgerEntry {
    /// 1-based, gapless sequence number.
    pub seq: u64,
    /// The commit that appended it.
    pub commit_seq: u64,
    /// The world.
    pub world: String,
    /// The event.
    pub event: Value,
    /// The previous entry's hash ([`GENESIS_HASH`] for the first).
    pub prev_hash: String,
    /// Norn1 hash of `{seq, commitSeq, world, event, prev}`.
    pub hash: String,
}

/// A bounded ledger scan with only finding producer payloads decoded.
/// Every ledger row consumes the scan budget, including unrelated payloads.
#[derive(Debug, Clone, PartialEq)]
pub struct FindingLedgerPage {
    /// Finding events and their same-commit observation metadata.
    pub entries: Vec<LedgerEntry>,
    /// All examined rows, including same-commit lookup work and repeated rows.
    pub scanned_rows: usize,
    /// Last outer-page position, or the supplied boundary at end of history.
    pub scanned_through: u64,
    /// Commits whose event positions were examined in this scan.
    pub scanned_commits: BTreeSet<u64>,
}

impl LedgerEntry {
    /// The business trigger evaluated by rules, without host causality.
    /// Observations stand for their native fact event at this boundary.
    pub fn rule_input(&self) -> Value {
        let mut event = match self.event.get("op").and_then(Value::as_str) {
            Some("observed_fact") => self.event.get("event").unwrap_or(&self.event).clone(),
            Some("observed_deleted") => norn_ir::runtime_object! {
                "op" => "deleted",
                "key" => self.event.get("key"),
            },
            Some("observed_transition") => norn_ir::runtime_object! {
                "op" => "transitioned",
                "key" => self.event.get("key"),
                "lifecycle" => self.event.get("property"),
                "transition" => self.event.get("transition"),
                "from" => self.event.get("from"),
                "to" => self.event.get("to"),
            },
            _ => self.event.clone(),
        };
        if let Some(object) = event.as_object_mut() {
            object.remove("causality");
        }
        event
    }
}

/// The verified ledger head.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LedgerHead {
    /// The last event sequence number (`0` when empty).
    pub seq: u64,
    /// The last event hash ([`GENESIS_HASH`] when empty).
    pub hash: String,
    /// How many commits.
    pub commits: u64,
}

impl LedgerHead {
    /// The head of an empty ledger: verification from it is a full one.
    #[must_use]
    pub fn genesis() -> Self {
        Self {
            seq: 0,
            hash: GENESIS_HASH.to_owned(),
            commits: 0,
        }
    }
}

/// One object as stored.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ObjectRecord {
    /// The concrete object type.
    pub object_type: String,
    /// The object id.
    pub id: String,
    /// The tenant scope.
    pub scope: String,
    /// The owner version (`+1` per write; a recreate continues after the
    /// highest version the id ever had).
    pub version: u64,
    /// Every stored property (inherited ones included), by property id.
    pub values: BTreeMap<String, Value>,
}

/// One bounded root candidate for a `WidgetIR` key-order scan.
#[derive(Debug)]
pub struct BindingRootCandidate {
    /// Owned identity when it fits the fresh request's candidate budget.
    /// An intrinsically oversized identity is unavailable and omitted.
    pub identity: Option<(String, String)>,
    /// Persistent high-water locator; zero means the scan reached its end.
    pub locator: u64,
    /// Whether another raw candidate follows this position.
    pub has_more: bool,
}

/// A sort value retained in a sealed widget-list continuation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum BindingSortValue {
    /// An integer or boolean column value.
    Integer(i64),
    /// A text, enum, or date column value.
    Text(String),
    /// A nullable sort property with no value.
    Null,
}

/// A stable keyset boundary for a sorted widget list.
///
/// The sort value and identity are copied into the sealed cursor so a deleted
/// boundary row cannot invalidate the next page's seek.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BindingSortedPosition {
    /// The property's SQLite sort value.
    pub value: BindingSortValue,
    /// The identity tie-breaker, always ascending.
    pub identity: String,
}

/// One bounded candidate from an indexed widget sort scan.
#[derive(Debug)]
pub struct BindingSortedCandidate {
    /// Owned identity when both identity and sort position fit the request.
    pub identity: Option<(String, String)>,
    /// Stable position to seal as the continuation boundary.
    pub position: Option<BindingSortedPosition>,
    /// Whether another raw candidate follows this position.
    pub has_more: bool,
}

/// One link as stored.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LinkRecord {
    /// The source object id.
    pub source: String,
    /// The target object id.
    pub target: String,
    /// Link property values.
    pub values: BTreeMap<String, Value>,
}

/// One live link, as [`StoreReader::links_touching`] names it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinkRef {
    /// The link's declaring (source) type.
    pub source_type: String,
    /// The link id.
    pub link: String,
    /// The source object id.
    pub source: String,
    /// The declared target type.
    pub target_type: String,
    /// The target object id.
    pub target: String,
}

/// Which endpoint of a link neighborhood is the requested object.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LinkDirection {
    /// Links whose source is the requested object.
    Out,
    /// Links whose target is the requested object.
    In,
    /// Links with the requested object at either endpoint.
    Both,
}

#[allow(
    clippy::too_many_lines,
    reason = "directional index, cursor and locator bounds stay in one SQL builder"
)]
fn link_candidate_page_sql(
    world: &str,
    id: &str,
    branch_direction: LinkDirection,
    link_type: Option<&str>,
    after: Option<&LinkPagePosition>,
    limit: usize,
    locator_anchor_order: Option<Ordering>,
) -> Result<Vec<(String, Vec<SqlValue>)>, CellError> {
    let mut bindings = vec![SqlValue::from(world), SqlValue::from(id)];
    let link_type_index = link_type.map(|link_type| {
        bindings.push(SqlValue::from(link_type));
        bindings.len()
    });
    let (lookup_column, base_index, filtered_index) = match branch_direction {
        LinkDirection::Out => (
            "source",
            "_norn_link_version/source_order",
            "_norn_link_version/source_link_order",
        ),
        LinkDirection::In => (
            "target",
            "_norn_link_version/target_order",
            "_norn_link_version/target_link_order",
        ),
        LinkDirection::Both => {
            return Err(CellError::Engine(
                "a link candidate branch must have one direction".to_owned(),
            ));
        }
    };
    let index = if link_type_index.is_some() {
        filtered_index
    } else {
        base_index
    };
    let mut base_where = format!("\"world\" = ?1 AND \"{lookup_column}\" = ?2");
    if let Some(link_type_index) = link_type_index {
        #[expect(
            clippy::let_underscore_must_use,
            reason = "fmt::Write for String is infallible; its trait signature still returns Result"
        )]
        let _ = write!(base_where, " AND \"link\" = ?{link_type_index}");
    }
    let mut cursor_clauses = vec![String::new()];
    if let Some(after) = after {
        if let Some(locator) = after.row_locator {
            bindings.push(SqlValue::from(to_i64(locator, "link scan locator")?));
            let cursor = bindings.len();
            let table = format!(
                "(SELECT \"link_table\" FROM \"{LINK_VERSIONS}\" WHERE rowid = ?{cursor} AND \"world\" = ?1)"
            );
            let (column, value) = match branch_direction {
                LinkDirection::Out => ("target", "target"),
                LinkDirection::In => ("source", "source"),
                LinkDirection::Both => {
                    return Err(CellError::Engine(
                        "a link candidate branch must have one direction".to_owned(),
                    ));
                }
            };
            let second = format!(
                "(SELECT \"{value}\" FROM \"{LINK_VERSIONS}\" WHERE rowid = ?{cursor} AND \"world\" = ?1)"
            );
            let table_greater = format!("\"link_table\" > {table}");
            let same_table = format!("\"link_table\" = {table}");
            cursor_clauses = match (branch_direction, locator_anchor_order) {
                (LinkDirection::Out, Some(Ordering::Less)) => vec![table_greater],
                (LinkDirection::Out, Some(Ordering::Greater)) => {
                    vec![format!("\"link_table\" >= {table}")]
                }
                (LinkDirection::In, Some(Ordering::Greater)) => vec![
                    table_greater,
                    format!("{same_table} AND \"{column}\" >= {second}"),
                ],
                (LinkDirection::Out, Some(Ordering::Equal))
                | (LinkDirection::In, Some(Ordering::Equal | Ordering::Less)) => vec![
                    table_greater,
                    format!("{same_table} AND \"{column}\" > {second}"),
                ],
                _ => {
                    return Err(CellError::OutOfRange(
                        "link locator endpoint order is required",
                    ));
                }
            };
        } else {
            let first = bindings
                .len()
                .checked_add(1)
                .ok_or(CellError::OutOfRange("link cursor parameter index"))?;
            bindings.push(SqlValue::from(after.link_table.as_str()));
            let second = first
                .checked_add(1)
                .ok_or(CellError::OutOfRange("link cursor parameter index"))?;
            let (column, endpoint_order, value) = match branch_direction {
                LinkDirection::Out => (
                    "target",
                    id.as_bytes().cmp(after.source.as_bytes()),
                    after.target.as_str(),
                ),
                LinkDirection::In => (
                    "source",
                    id.as_bytes().cmp(after.target.as_bytes()),
                    after.source.as_str(),
                ),
                LinkDirection::Both => {
                    return Err(CellError::Engine(
                        "a link candidate branch must have one direction".to_owned(),
                    ));
                }
            };
            bindings.push(SqlValue::from(value));
            let table_greater = format!("\"link_table\" > ?{first}");
            cursor_clauses = match (branch_direction, endpoint_order) {
                (LinkDirection::Out, Ordering::Less) => vec![table_greater],
                (LinkDirection::Out, Ordering::Greater) => {
                    vec![format!("\"link_table\" >= ?{first}")]
                }
                (LinkDirection::In, Ordering::Greater) => vec![format!(
                    "(\"link_table\", \"{column}\") >= (?{first}, ?{second})"
                )],
                (LinkDirection::Out, Ordering::Equal)
                | (LinkDirection::In, Ordering::Equal | Ordering::Less) => vec![format!(
                    "(\"link_table\", \"{column}\") > (?{first}, ?{second})"
                )],
                (LinkDirection::Both, _) => {
                    return Err(CellError::Engine(
                        "a link candidate branch must have one direction".to_owned(),
                    ));
                }
            };
        }
    }
    let order =
        "\"link_table\" COLLATE BINARY, \"source\" COLLATE BINARY, \"target\" COLLATE BINARY";
    let mut pages = Vec::with_capacity(cursor_clauses.len());
    for cursor_clause in cursor_clauses {
        let limit_index = bindings
            .len()
            .checked_add(1)
            .ok_or(CellError::OutOfRange("link cursor parameter index"))?;
        let mut page_bindings = bindings.clone();
        page_bindings.push(SqlValue::from(
            i64::try_from(limit.saturating_add(1)).unwrap_or(MAX_SAFE_INTEGER),
        ));
        let mut where_clause = base_where.clone();
        if !cursor_clause.is_empty() {
            #[expect(
                clippy::let_underscore_must_use,
                reason = "fmt::Write for String is infallible; its trait signature still returns Result"
            )]
            let _ = write!(where_clause, " AND ({cursor_clause})");
        }
        pages.push((
            format!(
                "SELECT rowid, COALESCE(octet_length(\"link_table\"), 0), COALESCE(octet_length(\"source\"), 0), COALESCE(octet_length(\"target\"), 0), COALESCE(octet_length(\"link\"), 0), COALESCE(octet_length(\"source_type\"), 0), COALESCE(octet_length(\"target_type\"), 0) FROM \"{LINK_VERSIONS}\" INDEXED BY {} WHERE {where_clause} ORDER BY {order} LIMIT ?{limit_index}",
                quote_identifier(index),
            ),
            page_bindings,
        ));
    }
    Ok(pages)
}

fn order_link_candidate_heads<E: SqlEngine>(
    engine: &E,
    locators: &[u64],
) -> Result<Vec<u64>, CellError> {
    if locators.is_empty() {
        return Ok(Vec::new());
    }
    let mut bindings = Vec::with_capacity(locators.len());
    let mut placeholders = Vec::with_capacity(locators.len());
    for locator in locators {
        let index = bindings
            .len()
            .checked_add(1)
            .ok_or(CellError::OutOfRange("link candidate parameter index"))?;
        placeholders.push(format!("?{index}"));
        bindings.push(SqlValue::from(to_i64(*locator, "link scan locator")?));
    }
    engine
        .query(
            &format!(
                "SELECT rowid FROM \"{LINK_VERSIONS}\" WHERE rowid IN ({}) ORDER BY \"link_table\" COLLATE BINARY, \"source\" COLLATE BINARY, \"target\" COLLATE BINARY",
                placeholders.join(", ")
            ),
            &bindings,
        )?
        .into_iter()
        .map(|values| to_u64(SqlRow(values).get(0)?, "link scan locator"))
        .collect()
}

fn link_locator_endpoint_order<E: SqlEngine>(
    engine: &E,
    world: &str,
    id: &str,
    locator: u64,
    endpoint: &str,
) -> Result<Ordering, CellError> {
    if endpoint != "source" && endpoint != "target" {
        return Err(CellError::OutOfRange("link locator endpoint"));
    }
    let ordering: i64 = engine.query_row(
        &format!(
            "SELECT CASE WHEN ?1 COLLATE BINARY < \"{endpoint}\" COLLATE BINARY THEN -1 WHEN ?1 COLLATE BINARY = \"{endpoint}\" COLLATE BINARY THEN 0 ELSE 1 END FROM \"{LINK_VERSIONS}\" WHERE rowid = ?2 AND \"world\" = ?3"
        ),
        params![id, to_i64(locator, "link scan locator")?, world],
        |row| row.get(0),
    )?;
    match ordering {
        -1 => Ok(Ordering::Less),
        0 => Ok(Ordering::Equal),
        1 => Ok(Ordering::Greater),
        _ => Err(CellError::OutOfRange("link candidate order")),
    }
}

fn merge_link_candidate_metadata<E: SqlEngine>(
    engine: &E,
    streams: &[Vec<Vec<SqlValue>>],
    limit: usize,
) -> Result<Vec<Vec<SqlValue>>, CellError> {
    let mut merged = Vec::with_capacity(limit.saturating_add(1));
    let mut stream_indices = vec![0_usize; streams.len()];
    while merged.len() < limit.saturating_add(1) {
        let mut head_locators = Vec::with_capacity(streams.len());
        for (stream, index) in streams.iter().zip(&stream_indices) {
            if let Some(row) = stream.get(*index) {
                head_locators.push(to_u64(SqlRow(row.clone()).get(0)?, "link scan locator")?);
            }
        }
        let ordered_heads = order_link_candidate_heads(engine, &head_locators)?;
        let Some(selected_locator) = ordered_heads.first().copied() else {
            break;
        };
        let mut selected = None;
        for (stream, index) in streams.iter().zip(&mut stream_indices) {
            if let Some(row) = stream.get(*index)
                && to_u64(SqlRow(row.clone()).get(0)?, "link scan locator")? == selected_locator
            {
                if selected.is_none() {
                    selected = Some(row.clone());
                }
                *index = (*index).saturating_add(1);
            }
        }
        if let Some(row) = selected {
            merged.push(row);
        } else {
            return Err(CellError::Engine(
                "link candidate merge lost its selected row".to_owned(),
            ));
        }
    }
    Ok(merged)
}

fn link_candidate_rows_by_locator<E: SqlEngine>(
    engine: &E,
    world: &str,
    locators: &[u64],
) -> Result<BTreeMap<u64, (String, LinkRef, u64)>, CellError> {
    let chunk_size = MAX_PARAMETERS.saturating_sub(1);
    if chunk_size == 0 {
        return Err(CellError::OutOfRange("link candidate parameter limit"));
    }
    let mut result = BTreeMap::new();
    for chunk in locators.chunks(chunk_size) {
        let mut bindings = Vec::with_capacity(chunk.len().saturating_add(1));
        bindings.push(SqlValue::from(world));
        let mut placeholders = Vec::with_capacity(chunk.len());
        for locator in chunk {
            let index = bindings
                .len()
                .checked_add(1)
                .ok_or(CellError::OutOfRange("link candidate parameter index"))?;
            placeholders.push(format!("?{index}"));
            bindings.push(SqlValue::from(to_i64(*locator, "link scan locator")?));
        }
        let rows = engine.query(
            &format!(
                "SELECT rowid, \"link_table\", \"source\", \"target\", \"link\", \"source_type\", \"target_type\", \"version\" FROM \"{LINK_VERSIONS}\" WHERE \"world\" = ?1 AND rowid IN ({})",
                placeholders.join(", ")
            ),
            &bindings,
        )?;
        for values in rows {
            let row = SqlRow(values);
            let locator = to_u64(row.get(0)?, "link scan locator")?;
            result.insert(
                locator,
                (
                    row.get(1)?,
                    LinkRef {
                        source: row.get(2)?,
                        target: row.get(3)?,
                        link: row.get(4)?,
                        source_type: row.get(5)?,
                        target_type: row.get(6)?,
                    },
                    to_u64(row.get(7)?, "link version")?,
                ),
            );
        }
    }
    Ok(result)
}

/// The last scanned link identity for stable keyset pagination.
///
/// The cell orders positions by `(link_table, source, target)` using SQLite's
/// binary text collation. API cursors should authenticate and bind this value
/// to the request identity and filters before passing it back.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LinkPagePosition {
    /// The projected physical link table.
    pub link_table: String,
    /// The source object id.
    pub source: String,
    /// The target object id.
    pub target: String,
    /// Continuation past an intrinsically oversized candidate. An API may
    /// carry this only inside an authenticated, encrypted cursor bound to the
    /// same world, endpoint and filters.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub row_locator: Option<u64>,
}

/// A live link candidate returned by the bounded neighborhood scan.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinkEdgeRecord {
    /// The link edge identity and endpoint types.
    pub edge: LinkRef,
    /// The link compare-and-swap version, including property replacements.
    pub version: u64,
    /// The stable keyset position of this edge for an API cursor boundary.
    pub position: LinkPagePosition,
}

/// One bounded page of live links touching an object.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinkReadPage {
    /// Live, type-compatible links among the scanned candidate rows.
    pub items: Vec<LinkEdgeRecord>,
    /// Raw candidate rows consumed from the version table, including rows
    /// omitted because they are stale or incompatible with the active model.
    pub candidate_count: usize,
    /// UTF-8 bytes of the admitted raw candidates. The metadata-only
    /// lookahead row is not decoded or charged as an examined candidate.
    pub candidate_bytes: u64,
    /// Resume position when additional raw candidates remain.
    pub next_after: Option<LinkPagePosition>,
    /// Whether the candidate query found an additional raw row beyond this page.
    /// A caller applying authorization may need more pages to fill a response.
    pub has_more_candidates: bool,
}

/// A pending outbox row.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PendingCommand {
    /// Position in enqueue order.
    pub position: u64,
    /// The commit that last enqueued it.
    pub commit_seq: u64,
    /// The command as enqueued.
    pub command: OutboxCommand,
    /// The recorded `perform` result, once performed.
    pub result: Option<Value>,
}

/// A pending timer row.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PendingTimer {
    /// The commit that scheduled it: its incarnation.
    pub commit_seq: u64,
    /// The timer.
    pub timer: TimerEntry,
}

/// One `perform` execution.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RunRecord {
    /// The command id.
    pub command_id: String,
    /// The generation.
    pub generation: u64,
    /// 1-based attempt number within the generation.
    pub attempt: u64,
    /// The outcome.
    pub outcome: Value,
    /// The commit that consumed the result, if any.
    pub consumed_commit_seq: Option<u64>,
}

/// A pending inbox row.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PendingObservation {
    /// Position in ingestion order.
    pub position: u64,
    /// The observation id.
    pub observation_id: String,
    /// The observation as ingested.
    pub observed: Value,
}

/// Decode receipt rows, checking each row's lookup columns against its
/// hashed body: they drive idempotent replay and commit numbering.
fn decode_receipts(rows: Vec<Vec<SqlValue>>) -> Result<Vec<Receipt>, CellError> {
    let mut out = Vec::with_capacity(rows.len());
    for values in rows {
        let row = SqlRow(values);
        let commit_seq = to_u64(row.get(0)?, "commit_seq")?;
        let world: String = row.get(1)?;
        let idempotency_key: Option<String> = row.get(2)?;
        let request_hash: String = row.get(3)?;
        let body: ReceiptBody = serde_json::from_str(&row.get::<String>(4)?)?;
        if body.commit_seq != commit_seq
            || body.world != world
            || body.idempotency_key != idempotency_key
            || body.request_hash != request_hash
        {
            return Err(CellError::LedgerCorrupt {
                seq: commit_seq,
                reason: "receipt columns disagree with the hashed receipt".to_owned(),
            });
        }
        out.push(Receipt {
            body,
            hash: row.get(5)?,
        });
    }
    Ok(out)
}

pub(crate) fn to_i64(value: u64, what: &'static str) -> Result<i64, CellError> {
    if value > u64::try_from(MAX_SAFE_INTEGER).unwrap_or(u64::MAX) {
        return Err(CellError::OutOfRange(what));
    }
    i64::try_from(value).map_err(|_overflow| CellError::OutOfRange(what))
}

pub(crate) fn to_u64(value: i64, what: &'static str) -> Result<u64, CellError> {
    if !(0..=MAX_SAFE_INTEGER).contains(&value) {
        return Err(CellError::OutOfRange(what));
    }
    u64::try_from(value).map_err(|_negative| CellError::OutOfRange(what))
}

pub(crate) fn active_definition_version(engine: &impl SqlEngine) -> Result<Option<u64>, CellError> {
    let version: Option<i64> = engine.query_optional(
        &format!("SELECT \"version\" FROM \"{DEFINITION_ACTIVE}\" WHERE \"id\" = 1"),
        params![],
        |row| row.get(0),
    )?;
    version
        .map(|version| to_u64(version, "definitions version"))
        .transpose()
}

pub(crate) fn validate_active_definition_pointer(
    engine: &impl SqlEngine,
) -> Result<Option<u64>, CellError> {
    let active = active_definition_version(engine)?;
    let latest: Option<i64> = engine.query_optional(
        &format!(
            "SELECT \"version\" FROM \"{DEFINITION_VERSION}\" ORDER BY \"version\" DESC LIMIT 1"
        ),
        params![],
        |row| row.get(0),
    )?;
    let latest = latest
        .map(|version| to_u64(version, "definitions version"))
        .transpose()?;
    match (active, latest) {
        (None, None) => Ok(None),
        (Some(active), Some(latest)) if active == latest && active > 0 => Ok(Some(active)),
        (active, latest) => Err(CellError::LedgerCorrupt {
            seq: active.or(latest).unwrap_or(0),
            reason: "active definitions pointer does not name the latest retained version"
                .to_owned(),
        }),
    }
}

pub(crate) fn definition_by_version(
    engine: &impl SqlEngine,
    version: u64,
) -> Result<Option<InstalledDefinitionSet>, CellError> {
    let version_sql = to_i64(version, "definitions version")?;
    let row = engine.query_optional(
        &format!(
            "SELECT \"version\", \"receipt_sha256\", \"canonical_set\", \"model_hash\", \"ddl_hash\", \"installed_at\", \"commit_seq\" FROM \"{DEFINITION_VERSION}\" WHERE \"version\" = ?1"
        ),
        params![version_sql],
        |row| {
            Ok(InstalledDefinitionSet {
                version: to_u64(row.get(0)?, "definitions version")?,
                receipt_sha256: row.get(1)?,
                admission_receipt: None,
                canonical_set: row.get(2)?,
                registry: Vec::new(),
                model_hash: row.get(3)?,
                ddl_hash: row.get(4)?,
                installed_at: to_u64(row.get(5)?, "definitions installed_at")?,
                commit_seq: to_u64(row.get(6)?, "definitions commit_seq")?,
                layout: None,
                layout_revision: 0,
            })
        },
    )?;
    let Some(mut installed) = row else {
        return Ok(None);
    };
    if definition_registry_table_exists(engine)? {
        installed.registry = definition_registry_for_version(engine, version)?;
    }
    if definition_admission_receipt_table_exists(engine)? {
        installed.admission_receipt = definition_admission_receipt_for_version(engine, version)?;
    }
    (installed.layout, installed.layout_revision) = definition_layout_for_version(engine, version)?;
    Ok(Some(installed))
}

fn definition_layout_for_version(
    engine: &impl SqlEngine,
    version: u64,
) -> Result<(Option<String>, u64), CellError> {
    if !definition_layout_table_exists(engine)? {
        return Ok((None, 0));
    }
    let row = engine.query_optional(
        &format!(
            "SELECT \"layout_revision\", \"sidecar\" FROM \"{DEFINITION_LAYOUT}\" WHERE \"version\" = ?1"
        ),
        params![to_i64(version, "definitions version")?],
        |row| {
            Ok((
                to_u64(row.get(0)?, "layout revision")?,
                row.get::<String>(1)?,
            ))
        },
    )?;
    Ok(row.map_or((None, 0), |(revision, sidecar)| (Some(sidecar), revision)))
}

fn definition_layout_table_exists(engine: &impl SqlEngine) -> Result<bool, CellError> {
    let exists = engine
        .query_optional(
            "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?1",
            params![DEFINITION_LAYOUT],
            |row| row.get::<i64>(0),
        )?
        .is_some();
    Ok(exists)
}

fn definition_registry_table_exists(engine: &impl SqlEngine) -> Result<bool, CellError> {
    let exists = engine
        .query_optional(
            "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?1",
            params![DEFINITION_REGISTRY],
            |row| row.get::<i64>(0),
        )?
        .is_some();
    Ok(exists)
}

fn definition_admission_receipt_table_exists(engine: &impl SqlEngine) -> Result<bool, CellError> {
    let exists = engine
        .query_optional(
            "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?1",
            params![DEFINITION_ADMISSION_RECEIPT],
            |row| row.get::<i64>(0),
        )?
        .is_some();
    Ok(exists)
}

fn definition_admission_receipt_for_version(
    engine: &impl SqlEngine,
    version: u64,
) -> Result<Option<String>, CellError> {
    engine.query_optional(
        &format!(
            "SELECT \"receipt\" FROM \"{DEFINITION_ADMISSION_RECEIPT}\" WHERE \"version\" = ?1"
        ),
        params![to_i64(version, "definitions version")?],
        |row| row.get::<String>(0),
    )
}

fn definition_registry_for_version(
    engine: &impl SqlEngine,
    version: u64,
) -> Result<Vec<String>, CellError> {
    let registry = engine.query_optional(
        &format!("SELECT \"registry\" FROM \"{DEFINITION_REGISTRY}\" WHERE \"version\" = ?1"),
        params![to_i64(version, "definitions version")?],
        |row| row.get::<String>(0),
    )?;
    let Some(registry) = registry else {
        return Ok(Vec::new());
    };
    Ok(serde_json::from_str(&registry)?)
}

fn definition_from_row(row: &SqlRow) -> Result<InstalledDefinitionSet, CellError> {
    Ok(InstalledDefinitionSet {
        version: to_u64(row.get(0)?, "definitions version")?,
        receipt_sha256: row.get(1)?,
        admission_receipt: None,
        canonical_set: row.get(2)?,
        registry: Vec::new(),
        model_hash: row.get(3)?,
        ddl_hash: row.get(4)?,
        installed_at: to_u64(row.get(5)?, "definitions installed_at")?,
        commit_seq: to_u64(row.get(6)?, "definitions commit_seq")?,
        layout: None,
        layout_revision: 0,
    })
}

pub(crate) fn next(value: u64, what: &'static str) -> Result<u64, CellError> {
    value.checked_add(1).ok_or(CellError::OutOfRange(what))
}

pub(crate) fn placeholders(count: usize) -> String {
    vec!["?"; count].join(", ")
}

pub(crate) fn column_list(columns: &[&str]) -> String {
    columns
        .iter()
        .map(|column| quote_identifier(column))
        .collect::<Vec<_>>()
        .join(", ")
}

/// Project integer-affinity columns as text only when SQLite stores an
/// integer. This preserves full-range signed integers across the host seam
/// while leaving other stored SQLite value classes intact for codec checks.
pub(crate) fn read_projection(columns: &[(&str, bool)]) -> Vec<String> {
    columns
        .iter()
        .map(|(column, is_integer)| {
            let quoted = quote_identifier(column);
            if *is_integer {
                format!(
                    "(SELECT CASE WHEN typeof(\"_norn_i\") = 'integer' THEN CAST(\"_norn_i\" AS TEXT) ELSE \"_norn_i\" END FROM (SELECT {quoted} AS \"_norn_i\"))"
                )
            } else {
                quoted
            }
        })
        .collect()
}

/// Split a projection into host-bounded statements while preserving columns.
/// `suffix` must select one row or order by a unique key; the synchronous host
/// must keep that committed view stable throughout the reads.
pub(crate) fn query_expressions(
    engine: &impl SqlEngine,
    table: &str,
    expressions: &[String],
    suffix: &str,
    params: &[SqlValue],
) -> Result<Vec<Vec<SqlValue>>, CellError> {
    let sql = format!("SELECT {} {suffix}", expressions.join(", "));
    if sql.len() <= MAX_STATEMENT_BYTES {
        return engine.query(&sql, params);
    }
    if expressions.len() < 2 {
        return Err(CellError::OutOfRange("SQL statement bytes"));
    }

    let (left, right) = expressions.split_at(expressions.len().div_ceil(2));
    let mut rows = query_expressions(engine, table, left, suffix, params)?;
    let mut remaining = query_expressions(engine, table, right, suffix, params)?;
    if rows.len() != remaining.len() {
        return Err(CellError::CorruptValue {
            table: table.to_owned(),
            column: "projected query row count".to_owned(),
        });
    }
    for (row, rest) in rows.iter_mut().zip(&mut remaining) {
        row.append(rest);
    }
    Ok(rows)
}

fn query_projected(
    engine: &impl SqlEngine,
    table: &str,
    columns: &[(&str, bool)],
    suffix: &str,
    params: &[SqlValue],
) -> Result<Vec<Vec<SqlValue>>, CellError> {
    query_expressions(engine, table, &read_projection(columns), suffix, params)
}

struct DumpProjection {
    columns: Vec<(String, bool)>,
    order_by: String,
}

fn dump_projection(model: &Model, table_name: &str) -> Option<DumpProjection> {
    let mut columns = Vec::new();
    let order_by = if let Some(table) = model
        .schema()
        .object_tables
        .iter()
        .find(|table| table.table == table_name)
    {
        if table.is_root() {
            columns.extend([
                (WORLD_COLUMN.to_owned(), false),
                (ID_COLUMN.to_owned(), false),
                (TYPE_COLUMN.to_owned(), false),
                (SCOPE_COLUMN.to_owned(), false),
                (VERSION_COLUMN.to_owned(), false),
            ]);
        } else {
            columns.extend([
                (WORLD_COLUMN.to_owned(), false),
                (ID_COLUMN.to_owned(), false),
                (SCOPE_COLUMN.to_owned(), false),
            ]);
        }
        columns.extend(table.columns.iter().filter_map(|column| {
            column
                .column
                .as_ref()
                .map(|name| (name.clone(), column.encoding == ValueEncoding::Integer))
        }));
        if table.has_overflow {
            columns.push((OVERFLOW_COLUMN.to_owned(), false));
        }
        format!(
            "ORDER BY {}, {}",
            quote_identifier(WORLD_COLUMN),
            quote_identifier(ID_COLUMN)
        )
    } else {
        let table = model
            .schema()
            .link_tables
            .iter()
            .find(|table| table.table == table_name)?;
        columns.extend([
            (WORLD_COLUMN.to_owned(), false),
            (SOURCE_COLUMN.to_owned(), false),
            (TARGET_COLUMN.to_owned(), false),
        ]);
        columns.extend(table.columns.iter().filter_map(|column| {
            column
                .column
                .as_ref()
                .map(|name| (name.clone(), column.encoding == ValueEncoding::Integer))
        }));
        if table.has_overflow {
            columns.push((OVERFLOW_COLUMN.to_owned(), false));
        }
        format!(
            "ORDER BY {}, {}, {}",
            quote_identifier(WORLD_COLUMN),
            quote_identifier(SOURCE_COLUMN),
            quote_identifier(TARGET_COLUMN)
        )
    };
    Some(DumpProjection { columns, order_by })
}

/// Norn1 hash of one ledger entry.
pub(crate) fn entry_hash(
    seq: u64,
    commit_seq: u64,
    world: &str,
    event: &Value,
    prev: &str,
) -> Result<String, CellError> {
    /// The hashed form of one ledger entry.
    #[derive(Serialize)]
    #[serde(rename_all = "camelCase")]
    struct Hashed<'a> {
        seq: u64,
        commit_seq: u64,
        world: &'a str,
        event: &'a Value,
        prev: &'a str,
    }
    hash_json(&Hashed {
        seq,
        commit_seq,
        world,
        event,
        prev,
    })
}

pub(crate) fn receipt_by_key(
    conn: &impl SqlEngine,
    world: &str,
    key: &str,
) -> Result<Option<Receipt>, CellError> {
    let row: Option<(String, String)> = conn.query_optional(
        &format!(
            "SELECT \"receipt\", \"receipt_hash\" FROM \"{RECEIPT}\" WHERE \"world\" = ?1 AND \"idempotency_key\" = ?2"
        ),
        params![world, key],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    row.map(|(body, hash)| {
        Ok(Receipt {
            body: serde_json::from_str(&body)?,
            hash,
        })
    })
    .transpose()
}

/// Decode `columns` from `row` starting at column `offset` (the stored
/// columns in order, then the overflow object if any).
pub(crate) fn decode_row(
    context: DecodeContext<'_, '_>,
    table: &str,
    columns: &[PropertyColumn],
    has_overflow: bool,
    row: &SqlRow,
    offset: usize,
) -> Result<BTreeMap<String, Value>, CellError> {
    let mut values = BTreeMap::new();
    let mut index = offset;
    for column in columns {
        if column.column.is_some() {
            values.insert(
                column.property.as_str().to_owned(),
                decode_stored_value(context, table, column, row.get_ref(index)?)?,
            );
            index = index
                .checked_add(1)
                .ok_or(CellError::OutOfRange("column index"))?;
        }
    }
    if has_overflow {
        let overflow = match row.get_ref(index)? {
            SqlValue::Text(text) => serde_json::from_str::<BTreeMap<String, Value>>(text)?,
            SqlValue::Null | SqlValue::Integer(_) | SqlValue::Real(_) | SqlValue::Blob(_) => {
                BTreeMap::new()
            }
        };
        for column in columns.iter().filter(|column| column.column.is_none()) {
            let name = column.property.as_str();
            values.insert(
                name.to_owned(),
                canonicalize_stored_value(
                    context,
                    column,
                    overflow.get(name).unwrap_or(&Value::Null),
                ),
            );
        }
    }
    Ok(values)
}

/// Committed-state reads over one engine, including its active commit
/// transaction when called during a commit.
#[derive(Debug, Clone, Copy)]
pub struct StoreReader<'a, E: SqlEngine = DefaultEngine> {
    pub(crate) conn: &'a E,
    model: &'a Model,
    quantity_boundary: Option<crate::quantity::FormatBoundary>,
}

impl<'a, E: SqlEngine> StoreReader<'a, E> {
    /// Read one exact current quantity head from this trusted store boundary.
    ///
    /// # Errors
    /// SQL or stored-value errors.
    pub fn quantity_balance(
        &self,
        world: &str,
        ledger: &str,
        key: &norn_kernel::quantity::QuantityKey,
    ) -> Result<norn_kernel::quantity::QuantityBalance, CellError> {
        crate::quantity::balance(self.conn, world, ledger, key)
    }

    /// Read the ledger ordering revision, independent of balance CAS.
    ///
    /// # Errors
    /// SQL or stored-version errors.
    pub fn quantity_version(&self, world: &str, ledger: &str) -> Result<u64, CellError> {
        crate::quantity::version(self.conn, world, ledger)
    }

    /// Read the complete balance CAS set for a quantity intent.
    ///
    /// # Errors
    /// Invalid intent, missing state or storage errors.
    pub fn quantity_balance_versions(
        &self,
        world: &str,
        request: &norn_ir::quantity_ledger::QuantityRequest,
    ) -> Result<Vec<norn_ir::quantity_ledger::QuantityBalanceVersion>, CellError> {
        crate::quantity::balance_versions(self.conn, world, request)
    }

    /// Verified persisted cell clock, including ordinary and definition commits.
    ///
    /// # Errors
    /// Corruption or storage errors.
    pub fn clock_floor(&self) -> Result<u64, CellError> {
        crate::quantity::clock_floor(self.conn)
    }

    /// Read one complete, pinned reservation and its status.
    ///
    /// # Errors
    /// SQL or decoding errors.
    pub fn quantity_reservation(
        &self,
        world: &str,
        ledger: &str,
        id: &str,
    ) -> Result<Option<norn_kernel::quantity::QuantityReservation>, CellError> {
        crate::quantity::reservation(self.conn, world, ledger, id)
    }

    /// Read an append-only posting and its linked reversal identity.
    ///
    /// # Errors
    /// SQL or decoding errors.
    pub fn quantity_posting(
        &self,
        world: &str,
        ledger: &str,
        id: &str,
    ) -> Result<Option<norn_kernel::quantity::QuantityPosting>, CellError> {
        crate::quantity::posting(self.conn, world, ledger, id)
    }

    pub(crate) const fn new(conn: &'a E, model: &'a Model) -> Self {
        Self {
            conn,
            model,
            quantity_boundary: None,
        }
    }

    pub(crate) const fn with_quantity_boundary(
        mut self,
        boundary: Option<crate::quantity::FormatBoundary>,
    ) -> Self {
        self.quantity_boundary = boundary;
        self
    }

    /// The root row of object `id` through `object_type`'s root table:
    /// concrete type, scope, version.
    pub(crate) fn root_row(
        &self,
        world: &str,
        object_type: &str,
        id: &str,
    ) -> Result<Option<(String, String, u64)>, CellError> {
        let tables = self.model.tables(object_type)?;
        let Some(root) = tables.first() else {
            return Err(CellError::UnknownObjectType(object_type.to_owned()));
        };
        let sql = format!(
            "SELECT {}, {}, {} FROM {} WHERE {} = ?1 AND {} = ?2",
            quote_identifier(TYPE_COLUMN),
            quote_identifier(SCOPE_COLUMN),
            quote_identifier(VERSION_COLUMN),
            quote_identifier(&root.table),
            quote_identifier(WORLD_COLUMN),
            quote_identifier(ID_COLUMN),
        );
        let row: Option<(String, String, i64)> =
            self.conn.query_optional(&sql, params![world, id], |row| {
                Ok((row.get(0)?, row.get(1)?, row.get(2)?))
            })?;
        row.map(|(concrete, scope, version)| Ok((concrete, scope, to_u64(version, "version")?)))
            .transpose()
    }

    /// The stored properties of one table's row of object `id`.
    pub(crate) fn read_row(
        &self,
        table: &ObjectTable,
        world: &str,
        id: &str,
        owner: ValueOwner<'_>,
    ) -> Result<BTreeMap<String, Value>, CellError> {
        let mut columns: Vec<(&str, bool)> = table
            .columns
            .iter()
            .filter_map(|column| {
                column
                    .column
                    .as_deref()
                    .map(|name| (name, column.encoding == ValueEncoding::Integer))
            })
            .collect();
        if table.has_overflow {
            columns.push((OVERFLOW_COLUMN, false));
        }
        if columns.is_empty() {
            return Ok(BTreeMap::new());
        }
        let suffix = format!(
            "FROM {} WHERE {} = ?1 AND {} = ?2",
            quote_identifier(&table.table),
            quote_identifier(WORLD_COLUMN),
            quote_identifier(ID_COLUMN),
        );
        match query_projected(
            self.conn,
            &table.table,
            &columns,
            &suffix,
            params![world, id],
        )?
        .into_iter()
        .next()
        {
            Some(values) => {
                let row = SqlRow(values);
                decode_row(
                    DecodeContext {
                        model: self.model,
                        owner,
                    },
                    &table.table,
                    &table.columns,
                    table.has_overflow,
                    &row,
                    0,
                )
            }
            None => Err(CellError::CorruptValue {
                table: table.table.clone(),
                column: ID_COLUMN.to_owned(),
            }),
        }
    }

    /// The root type of `object_type`'s hierarchy.
    pub(crate) fn root_type(&self, object_type: &str) -> Result<&'a str, CellError> {
        self.model
            .chain(object_type)?
            .first()
            .map(String::as_str)
            .ok_or_else(|| CellError::UnknownObjectType(object_type.to_owned()))
    }

    /// The highest version ever assigned to `id` in `object_type`'s
    /// hierarchy (`0` if none).
    pub(crate) fn high_water(
        &self,
        world: &str,
        object_type: &str,
        id: &str,
    ) -> Result<u64, CellError> {
        let version: Option<i64> = self.conn.query_optional(
            &format!(
                "SELECT \"version\" FROM \"{VERSIONS}\" WHERE \"world\" = ?1 AND \"root_type\" = ?2 AND \"id\" = ?3"
            ),
            params![world, self.root_type(object_type)?, id],
            |row| row.get(0),
        )?;
        version.map_or(Ok(0), |version| to_u64(version, "version"))
    }

    /// Locate the retained high-water row for an object identity in `world`.
    ///
    /// The rowid is an opaque, cell-local locator for short-lived references.
    /// Cell commits preserve it across version updates, deletion, and
    /// recreation. Callers should verify the resolved identity before using
    /// a retained locator. An object's high-water row remains after deletion.
    ///
    /// # Errors
    /// Unknown object type; SQL engine errors; rowids outside the portable
    /// integer range.
    pub fn identity_locator(
        &self,
        world: &str,
        object_type: &str,
        id: &str,
    ) -> Result<Option<u64>, CellError> {
        let rowid: Option<i64> = self.conn.query_optional(
            &format!(
                "SELECT rowid FROM \"{VERSIONS}\" WHERE \"world\" = ?1 AND \"root_type\" = ?2 AND \"id\" = ?3"
            ),
            params![world, self.root_type(object_type)?, id],
            |row| row.get(0),
        )?;
        rowid
            .map(|rowid| to_u64(rowid, "identity rowid"))
            .transpose()
    }

    /// Resolve a cell-local high-water row locator to its identity key.
    ///
    /// # Errors
    /// Unknown object type; SQL engine errors; locators outside the portable
    /// integer range.
    pub fn identity_at_locator(
        &self,
        world: &str,
        object_type: &str,
        locator: u64,
    ) -> Result<Option<String>, CellError> {
        let locator = to_i64(locator, "identity rowid")?;
        self.conn.query_optional(
            &format!(
                "SELECT \"id\" FROM \"{VERSIONS}\" WHERE rowid = ?1 AND \"world\" = ?2 AND \"root_type\" = ?3"
            ),
            params![locator, world, self.root_type(object_type)?],
            |row| row.get(0),
        )
    }

    /// The compare-and-swap version of object `id` as `object_type`: the
    /// highest version ever assigned to `id` in the type hierarchy, `0` if
    /// never written. Every write allocates one past it, so it is monotonic
    /// per `(object_type, id)` across deletes, recreates, and subtype changes
    /// (a write elsewhere in the hierarchy may advance it, never lower it).
    ///
    /// # Errors
    ///
    /// Unknown type; SQL engine errors.
    pub fn version(&self, world: &str, object_type: &str, id: &str) -> Result<u64, CellError> {
        self.high_water(world, object_type, id)
    }

    /// The object `id` of `object_type` (or of a subtype) in `world`.
    ///
    /// # Errors
    ///
    /// Unknown type; SQL engine or decode errors.
    pub fn object(
        &self,
        world: &str,
        object_type: &str,
        id: &str,
    ) -> Result<Option<ObjectRecord>, CellError> {
        let Some((concrete, scope, version)) = self.root_row(world, object_type, id)? else {
            return Ok(None);
        };
        if !self.model.is_a(&concrete, object_type) {
            return Ok(None);
        }
        let mut values = BTreeMap::new();
        let owner = ValueOwner::Object {
            object_type: &concrete,
        };
        for table in self.model.tables(&concrete)? {
            values.extend(self.read_row(table, world, id, owner)?);
        }
        Ok(Some(ObjectRecord {
            object_type: concrete,
            id: id.to_owned(),
            scope,
            version,
            values,
        }))
    }

    /// Durable effective markings for one current object (`version=None`)
    /// or one immutable fact-version snapshot (`Some(version)`). A missing
    /// or incomplete snapshot returns `None`; API projections must fail
    /// closed instead of treating it as unmarked.
    ///
    /// # Errors
    /// Unknown object type, malformed stored JSON, or a SQL-engine error.
    pub fn effective_markings(
        &self,
        world: &str,
        object_type: &str,
        id: &str,
        version: Option<u64>,
    ) -> Result<Option<BTreeMap<String, Vec<String>>>, CellError> {
        let current = version.is_none();
        let version = match version {
            Some(version) => version,
            None => self.version(world, object_type, id)?,
        };
        if version == 0 {
            return Ok(None);
        }
        let marked_version: Option<i64> = if current {
            self.conn.query_optional(
                &format!(
                    "SELECT \"version\" FROM \"{MARKING_SNAPSHOT}\" WHERE \"world\" = ?1 AND \"object_type\" = ?2 AND \"id\" = ?3 AND \"is_current\" = 1"
                ),
                params![world, object_type, id],
                |row| row.get(0),
            )?
        } else {
            self.conn.query_optional(
                &format!(
                    "SELECT \"version\" FROM \"{MARKING_SNAPSHOT}\" WHERE \"world\" = ?1 AND \"object_type\" = ?2 AND \"id\" = ?3 AND \"version\" = ?4 AND \"is_current\" = 0"
                ),
                params![world, object_type, id, to_i64(version, "marking snapshot version")?],
                |row| row.get(0),
            )?
        };
        if marked_version != Some(to_i64(version, "marking snapshot version")?) {
            return Ok(None);
        }
        let table = if current {
            MARKING_CURRENT
        } else {
            MARKING_VERSION
        };
        let subject = marking_subject(self.conn, world, object_type, id)?;
        let sql = format!(
            "SELECT \"property\", \"markings\" FROM \"{table}\" WHERE \"world\" = ?1 AND \"object_type\" = ?2 AND \"subject\" = ?3 AND \"version\" = ?4 ORDER BY \"property\""
        );
        let rows = self.conn.query(
            &sql,
            params![
                world,
                object_type,
                subject,
                to_i64(version, "marking snapshot version")?
            ],
        )?;
        let mut result = BTreeMap::new();
        for row in rows {
            let row = SqlRow(row);
            let property: String = row.get(0)?;
            let encoded: String = row.get(1)?;
            let markings: Vec<String> = serde_json::from_str(&encoded).map_err(CellError::Json)?;
            if property != MARKING_OCCURRENCE {
                result.insert(property, markings);
            }
        }
        let expected = self.model.effective_property_markings(object_type)?;
        if expected
            .keys()
            .any(|property| !result.contains_key(property))
        {
            return Ok(None);
        }
        Ok(Some(result))
    }

    /// Durable classifications of the occurrence/version of one fact,
    /// independent of its unchanged property values. Missing property or
    /// occurrence slots return `None` and must fail closed; a public
    /// occurrence has an explicitly persisted empty set.
    ///
    /// # Errors
    /// Unknown type, malformed marking JSON, or a SQL-engine error.
    pub fn fact_occurrence_markings(
        &self,
        world: &str,
        object_type: &str,
        id: &str,
        version: Option<u64>,
    ) -> Result<Option<BTreeSet<String>>, CellError> {
        if self
            .effective_markings(world, object_type, id, version)?
            .is_none()
        {
            return Ok(None);
        }
        let table = if version.is_some() {
            MARKING_VERSION
        } else {
            MARKING_CURRENT
        };
        let version = match version {
            Some(version) => version,
            None => self.version(world, object_type, id)?,
        };
        let subject = marking_subject(self.conn, world, object_type, id)?;
        let encoded: Option<String> = self.conn.query_optional(
            &format!(
                "SELECT \"markings\" FROM \"{table}\" WHERE \"world\" = ?1 AND \"object_type\" = ?2 AND \"subject\" = ?3 AND \"property\" = ?4 AND \"version\" = ?5"
            ),
            params![world, object_type, subject, MARKING_OCCURRENCE, to_i64(version, "marking snapshot version")?],
            |row| row.get(0),
        )?;
        encoded
            .map(|encoded| serde_json::from_str(&encoded).map_err(CellError::Json))
            .transpose()
    }

    /// Union markings over every live row of one property in an object
    /// hierarchy. Returns `None` if any current fact lacks a complete
    /// snapshot or if the summary exceeds its bound, so callers can refuse a
    /// filter/sort that would otherwise inspect raw classified values.
    ///
    /// # Errors
    /// Unknown type, malformed marking JSON, or a SQL-engine error.
    pub fn property_marking_summary(
        &self,
        world: &str,
        object_type: &str,
        property: &str,
    ) -> Result<Option<BTreeSet<String>>, CellError> {
        let tables = self.model.tables(object_type)?;
        let Some(root) = tables.first() else {
            return Err(CellError::UnknownObjectType(object_type.to_owned()));
        };
        let concrete_types = self
            .model
            .object_types()
            .filter(|definition| self.model.is_a(definition.name.as_str(), object_type))
            .map(|definition| definition.name.as_str().to_owned())
            .collect::<Vec<_>>();
        let mut labels = BTreeSet::new();
        for concrete_type in concrete_types {
            let actual: i64 = self
                .conn
                .query_optional(
                    &format!(
                        "SELECT COUNT(*) FROM {} WHERE {} = ?1 AND {} = ?2",
                        quote_identifier(&root.table),
                        quote_identifier(WORLD_COLUMN),
                        quote_identifier(TYPE_COLUMN),
                    ),
                    params![world, concrete_type],
                    |row| row.get(0),
                )?
                .ok_or_else(|| {
                    CellError::Engine("object count query returned no row".to_owned())
                })?;
            let snapshots: i64 = self.conn.query_optional(
                &format!(
                    "SELECT COUNT(*) FROM {} AS o JOIN \"{MARKING_SNAPSHOT}\" AS s ON s.\"world\" = o.{} AND s.\"object_type\" = o.{} AND s.\"id\" = o.{} AND s.\"version\" = o.{} AND s.\"is_current\" = 1 WHERE o.{} = ?1 AND o.{} = ?2",
                    quote_identifier(&root.table),
                    quote_identifier(WORLD_COLUMN),
                    quote_identifier(TYPE_COLUMN),
                    quote_identifier(ID_COLUMN),
                    quote_identifier(VERSION_COLUMN),
                    quote_identifier(WORLD_COLUMN),
                    quote_identifier(TYPE_COLUMN),
                ),
                params![world, concrete_type],
                |row| row.get(0),
            )?.ok_or_else(|| CellError::Engine("marking snapshot count query returned no row".to_owned()))?;
            let property_rows: i64 = self.conn.query_optional(
                &format!(
                    "SELECT COUNT(*) FROM {} AS o JOIN \"{MARKING_CURRENT}\" AS m ON m.\"world\" = o.{} AND m.\"object_type\" = o.{} AND m.\"subject\" = (SELECT \"subject\" FROM \"{MARKING_SUBJECT}\" WHERE \"world\" = m.\"world\" AND \"object_type\" = m.\"object_type\" AND \"id\" = o.{}) AND m.\"version\" = o.{} AND m.\"property\" = ?3 WHERE o.{} = ?1 AND o.{} = ?2",
                    quote_identifier(&root.table),
                    quote_identifier(WORLD_COLUMN),
                    quote_identifier(TYPE_COLUMN),
                    quote_identifier(ID_COLUMN),
                    quote_identifier(VERSION_COLUMN),
                    quote_identifier(WORLD_COLUMN),
                    quote_identifier(TYPE_COLUMN),
                ),
                params![world, concrete_type, property],
                |row| row.get(0),
            )?.ok_or_else(|| CellError::Engine("current marking count query returned no row".to_owned()))?;
            if actual != snapshots || actual != property_rows {
                return Ok(None);
            }
            let variants = self.conn.query(
                &format!(
                    "SELECT DISTINCT \"markings\" FROM \"{MARKING_CURRENT}\" WHERE \"world\" = ?1 AND \"object_type\" = ?2 AND \"property\" = ?3 AND \"markings\" != '[]' LIMIT ?4"
                ),
                params![
                    world,
                    concrete_type,
                    property,
                    i64::try_from(MAX_MARKING_SUMMARY_VARIANTS + 1)
                        .map_err(|_conversion_error| {
                            CellError::OutOfRange("marking summary variant limit")
                        })?,
                ],
            )?;
            if variants.len() > MAX_MARKING_SUMMARY_VARIANTS {
                return Ok(None);
            }
            for variant in variants {
                let encoded: String = SqlRow(variant).get(0)?;
                labels.extend(
                    serde_json::from_str::<Vec<String>>(&encoded).map_err(CellError::Json)?,
                );
            }
        }
        Ok(Some(labels))
    }

    /// Effective identity properties of the requested object type. These
    /// components can be embedded directly in the opaque public key.
    ///
    /// # Errors
    /// The object type is not in the model.
    pub fn identity_properties(&self, object_type: &str) -> Result<BTreeSet<String>, CellError> {
        self.model.identity_properties(object_type)
    }

    /// Names of all effective properties used to fail closed when an older
    /// or incomplete marking snapshot cannot prove their classification.
    ///
    /// # Errors
    /// The object type is not in the model.
    pub fn marking_property_names(&self, object_type: &str) -> Result<BTreeSet<String>, CellError> {
        Ok(self
            .model
            .effective_property_markings(object_type)?
            .into_keys()
            .collect())
    }

    /// Up to `limit` objects of `object_type` (or of a subtype) in `world`,
    /// ordered by id.
    ///
    /// # Errors
    ///
    /// Unknown type; SQL engine or decode errors.
    pub fn list(
        &self,
        world: &str,
        object_type: &str,
        limit: usize,
    ) -> Result<Vec<ObjectRecord>, CellError> {
        self.list_after(world, object_type, None, limit)
    }

    /// Up to `limit` objects after `after` of `object_type` (and its
    /// subtypes) in `world`, ordered by the root identity key. The root
    /// table's `(world, id)` primary key makes each identity unique across
    /// the hierarchy, so the key is also a stable tie-break for concrete
    /// object types.
    ///
    /// # Errors
    ///
    /// Unknown type; SQL engine or decode errors.
    pub fn list_after(
        &self,
        world: &str,
        object_type: &str,
        after: Option<&str>,
        limit: usize,
    ) -> Result<Vec<ObjectRecord>, CellError> {
        let tables = self.model.tables(object_type)?;
        let Some(root) = tables.first() else {
            return Err(CellError::UnknownObjectType(object_type.to_owned()));
        };
        if limit == 0 {
            return Ok(Vec::new());
        }
        let safe_max = usize::try_from(MAX_SAFE_INTEGER).unwrap_or(usize::MAX);
        let cap = limit.min(safe_max);
        let fetch = i64::try_from(cap)
            .unwrap_or(MAX_SAFE_INTEGER)
            .min(MAX_SAFE_INTEGER);
        let type_names: Vec<String> = self
            .model
            .object_types()
            .filter(|definition| self.model.is_a(definition.name.as_str(), object_type))
            .map(|definition| definition.name.as_str().to_owned())
            .collect();
        let after_clause = if after.is_some() {
            format!(" AND {} > ?", quote_identifier(ID_COLUMN))
        } else {
            String::new()
        };
        let fixed_parameters = if after.is_some() { 3 } else { 2 };
        let chunk_size = MAX_PARAMETERS.saturating_sub(fixed_parameters).max(1);
        let mut ids = BTreeSet::new();
        for type_chunk in type_names.chunks(chunk_size) {
            let placeholders = placeholders(type_chunk.len());
            let sql = format!(
                "SELECT {} FROM {} WHERE {} = ? AND {} IN ({placeholders}){after_clause} ORDER BY {} LIMIT ?",
                quote_identifier(ID_COLUMN),
                quote_identifier(&root.table),
                quote_identifier(WORLD_COLUMN),
                quote_identifier(TYPE_COLUMN),
                quote_identifier(ID_COLUMN),
            );
            let mut bindings =
                Vec::with_capacity(type_chunk.len().saturating_add(fixed_parameters));
            bindings.push(SqlValue::from(world));
            bindings.extend(type_chunk.iter().map(|name| SqlValue::from(name.as_str())));
            if let Some(after) = after {
                bindings.push(SqlValue::from(after));
            }
            bindings.push(SqlValue::from(fetch));
            for values in self.conn.query(&sql, &bindings)? {
                let id: String = SqlRow(values).get(0)?;
                ids.insert(id);
                if ids.len() > cap {
                    let _ignored = ids.pop_last();
                }
            }
        }
        let mut out = Vec::with_capacity(ids.len());
        for id in ids {
            if let Some(record) = self.object(world, object_type, &id)? {
                out.push(record);
            }
        }
        Ok(out)
    }

    /// The model the reader reads.
    #[must_use]
    pub const fn model(&self) -> &'a Model {
        self.model
    }

    /// The ids and concrete types of at most `max_rows` rows of the root
    /// table of `object_type`'s hierarchy in `world`, by id, and whether
    /// that was every row. Every examined row counts, whatever its type, so
    /// the work is bounded however many siblings share the table.
    ///
    /// # Errors
    ///
    /// Unknown type; SQL engine errors.
    pub fn root_rows(
        &self,
        world: &str,
        object_type: &str,
        max_rows: usize,
    ) -> Result<(Vec<(String, String)>, bool), CellError> {
        let root = self.root_type(object_type)?;
        let table = self.model.table(root)?;
        let sql = format!(
            "SELECT {}, {} FROM {} WHERE {} = ?1 ORDER BY {} LIMIT ?2",
            quote_identifier(ID_COLUMN),
            quote_identifier(TYPE_COLUMN),
            quote_identifier(&table.table),
            quote_identifier(WORLD_COLUMN),
            quote_identifier(ID_COLUMN),
        );
        let safe_max_rows = usize::try_from(MAX_SAFE_INTEGER)
            .unwrap_or(usize::MAX)
            .saturating_sub(1);
        let max_rows = max_rows.min(safe_max_rows);
        let fetch = i64::try_from(max_rows.saturating_add(1)).unwrap_or(MAX_SAFE_INTEGER);
        let rows = self.conn.query(&sql, params![world, fetch])?;
        let mut out = Vec::new();
        let mut exhausted = true;
        for values in rows {
            let row = SqlRow(values);
            if out.len() >= max_rows {
                exhausted = false;
                break;
            }
            out.push((row.get(0)?, row.get(1)?));
        }
        Ok((out, exhausted))
    }

    /// The links `link` from `source` (of `object_type`) in `world`, by
    /// target id.
    ///
    /// # Errors
    ///
    /// Unknown type or link; SQL engine or decode errors.
    pub fn links(
        &self,
        world: &str,
        object_type: &str,
        link: &str,
        source: &str,
    ) -> Result<Vec<LinkRecord>, CellError> {
        let (_, table) = self.model.link(object_type, link)?;
        let mut columns = vec![(TARGET_COLUMN, false)];
        columns.extend(table.columns.iter().filter_map(|column| {
            column
                .column
                .as_deref()
                .map(|name| (name, column.encoding == ValueEncoding::Integer))
        }));
        if table.has_overflow {
            columns.push((OVERFLOW_COLUMN, false));
        }
        let suffix = format!(
            "FROM {} WHERE {} = ?1 AND {} = ?2 ORDER BY {}",
            quote_identifier(&table.table),
            quote_identifier(WORLD_COLUMN),
            quote_identifier(SOURCE_COLUMN),
            quote_identifier(TARGET_COLUMN),
        );
        let rows = query_projected(
            self.conn,
            &table.table,
            &columns,
            &suffix,
            params![world, source],
        )?;
        let mut out = Vec::new();
        let source_type = self
            .root_row(world, object_type, source)?
            .map_or_else(|| object_type.to_owned(), |(concrete, _, _)| concrete);
        let owner = ValueOwner::Link {
            source_type: &source_type,
            link,
        };
        for values in rows {
            let row = SqlRow(values);
            let target: String = row.get(0)?;
            let values = decode_row(
                DecodeContext {
                    model: self.model,
                    owner,
                },
                &table.table,
                &table.columns,
                table.has_overflow,
                &row,
                1,
            )?;
            out.push(LinkRecord {
                source: source.to_owned(),
                target,
                values,
            });
        }
        Ok(out)
    }

    /// The target ids of at most `limit` links `link` from `source` (of
    /// `object_type`) in `world`, by target id. Their aggregate UTF-8 byte
    /// length is checked against `max_bytes` before any target strings are
    /// fetched; `None` means the keys exceed that budget. Only the target
    /// column is read: a traversal never decodes link properties, and never
    /// more rows than a caller with a record budget can admit (#3079).
    ///
    /// # Errors
    ///
    /// Unknown type or link; SQL engine or decode errors.
    pub fn link_targets(
        &self,
        world: &str,
        object_type: &str,
        link: &str,
        source: &str,
        limit: usize,
        max_bytes: u64,
    ) -> Result<Option<Vec<String>>, CellError> {
        let (_, table) = self.model.link(object_type, link)?;
        let from = format!(
            "FROM {} WHERE {} = ?1 AND {} = ?2 ORDER BY {} LIMIT ?3",
            quote_identifier(&table.table),
            quote_identifier(WORLD_COLUMN),
            quote_identifier(SOURCE_COLUMN),
            quote_identifier(TARGET_COLUMN),
        );
        let fetch = i64::try_from(limit)
            .unwrap_or(MAX_SAFE_INTEGER)
            .min(MAX_SAFE_INTEGER);
        let bindings = [
            SqlValue::from(world),
            SqlValue::from(source),
            SqlValue::from(fetch),
        ];
        // SQLite's octet_length reads the stored UTF-8 byte length without
        // returning the key text. Keep this pass on the exact bounded,
        // ordered link query used below so even oversized keys stay out of
        // host memory until the aggregate fits the caller's remaining budget.
        let preflight = format!(
            "SELECT COALESCE(SUM(target_bytes), 0) FROM (SELECT octet_length({}) AS target_bytes {from})",
            quote_identifier(TARGET_COLUMN),
        );
        let target_bytes = self
            .conn
            .query_row(&preflight, &bindings, |row| row.get(0))?;
        let target_bytes = to_u64(target_bytes, "link target key bytes")?;
        if target_bytes > max_bytes {
            return Ok(None);
        }

        self.conn
            .query(
                &format!("SELECT {} {from}", quote_identifier(TARGET_COLUMN)),
                &bindings,
            )?
            .into_iter()
            .map(|values| SqlRow(values).get(0))
            .collect::<Result<Vec<_>, _>>()
            .map(Some)
    }

    /// The complete bounded live targets of one outgoing link, with each
    /// target's concrete written type and current link version. The target
    /// ids are enumerated by [`Self::link_targets`], then their retained link
    /// version rows provide the concrete identity and CAS version.
    ///
    /// # Errors
    ///
    /// Unknown type or link; SQL or row-decoding errors.
    pub fn link_targets_with_versions(
        &self,
        world: &str,
        object_type: &str,
        link: &str,
        source: &str,
        limit: usize,
        max_bytes: u64,
    ) -> Result<Option<Vec<LinkTargetVersion>>, CellError> {
        let (_, table) = self.model.link(object_type, link)?;
        let Some(targets) =
            self.link_targets(world, object_type, link, source, limit, max_bytes)?
        else {
            return Ok(None);
        };
        let mut reads = Vec::with_capacity(targets.len());
        for target in targets {
            let Some((version, target_type)) =
                self.link_version_row(world, &table.table, source, &target)?
            else {
                // A live row without a retained CAS identity is corrupt or
                // outside this host's supported snapshot representation.
                return Ok(None);
            };
            reads.push((target, target_type, version));
        }
        Ok(Some(reads))
    }

    /// The version and written target type of the link row
    /// `(source, target)` of link table `link_table`, if ever written.
    fn link_version_row(
        &self,
        world: &str,
        link_table: &str,
        source: &str,
        target: &str,
    ) -> Result<Option<(u64, String)>, CellError> {
        let row: Option<(i64, String)> = self.conn.query_optional(
            &format!(
                "SELECT \"version\", \"target_type\" FROM \"{LINK_VERSIONS}\" WHERE \"world\" = ?1 AND \"link_table\" = ?2 AND \"source\" = ?3 AND \"target\" = ?4"
            ),
            params![world, link_table, source, target],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        row.map(|(version, target_type)| Ok((to_u64(version, "link version")?, target_type)))
            .transpose()
    }

    /// The canonical identity type of `name`: its hierarchy root, or the
    /// name itself when it is not an object type (an interface), as the
    /// kernel's `CommitModel::canonical` resolves it.
    pub(crate) fn canonical_type(&self, name: &str) -> String {
        self.model
            .chain(name)
            .ok()
            .and_then(<[String]>::first)
            .map_or_else(|| name.to_owned(), Clone::clone)
    }

    /// The compare-and-swap version of link `link` from `source` (of
    /// `object_type`) to `target` (of `target_type`): bumped by every add or
    /// remove, `0` if never written (#3013). The version is canonical
    /// (#3018), one per source object, link id, and target object, however
    /// they are named: the highest version over every table of `link` in
    /// the source's hierarchy (sibling types may each declare it) whose row
    /// was written to a target of the same canonical identity. It is the one
    /// the latest change of that link wrote.
    ///
    /// # Errors
    ///
    /// Unknown type; no type in the hierarchy declares `link`; SQL engine
    /// errors.
    pub fn link_version(
        &self,
        world: &str,
        object_type: &str,
        link: &str,
        source: &str,
        target_type: &str,
        target: &str,
    ) -> Result<u64, CellError> {
        let root = self.root_type(object_type)?;
        let target_root = self.canonical_type(target_type);
        let mut version = None;
        for table in &self.model.schema().link_tables {
            if table.link.as_str() != link || self.root_type(table.source.as_str())? != root {
                continue;
            }
            let current = match self.link_version_row(world, &table.table, source, target)? {
                Some((current, written)) if self.canonical_type(&written) == target_root => current,
                _ => 0,
            };
            version = Some(version.map_or(current, |known: u64| known.max(current)));
        }
        version.ok_or_else(|| CellError::UnknownLink {
            object_type: object_type.to_owned(),
            link: link.to_owned(),
        })
    }

    /// The link `link` from `source` (of `object_type`) to `target`, if it
    /// exists.
    ///
    /// # Errors
    ///
    /// Unknown type or link; SQL engine or decode errors.
    pub fn link(
        &self,
        world: &str,
        object_type: &str,
        link: &str,
        source: &str,
        target: &str,
    ) -> Result<Option<LinkRecord>, CellError> {
        Ok(self
            .links(world, object_type, link, source)?
            .into_iter()
            .find(|record| record.target == target))
    }

    /// Read one bounded page of live links touching an existing object.
    /// Directional candidates are keyset-seeked through covering indexes;
    /// `Both` merges separately bounded outgoing and incoming streams and
    /// de-duplicates self-links. Callers can apply endpoint authorization and
    /// continue scanning without loading the full neighborhood. Link types and
    /// endpoint types are checked against the active model. Receipt lookup
    /// remains separate so API callers can authorize endpoints first.
    ///
    /// `next_after` advances over scanned candidates, including stale or
    /// model-incompatible historical link-version rows. API cursors must
    /// authenticate and bind the position to the tenant, caller, object, and
    /// filters. Authorization filtering belongs to the API caller, which may
    /// need more than one candidate page to fill its visible page.
    ///
    /// # Errors
    ///
    /// Unknown type, invalid candidate limit, or SQL errors.
    #[allow(
        clippy::too_many_arguments,
        clippy::too_many_lines,
        reason = "one bounded page couples keyset scanning, type checks, and liveness"
    )]
    pub fn links_touching_page(
        &self,
        world: &str,
        object_type: &str,
        id: &str,
        direction: LinkDirection,
        link_type: Option<&str>,
        after: Option<&LinkPagePosition>,
        limit: usize,
    ) -> Result<LinkReadPage, CellError> {
        self.links_touching_page_inner(
            world,
            object_type,
            id,
            direction,
            link_type,
            after,
            limit,
            None,
            MAX_LINK_PAGE_BYTES,
        )?
        .ok_or(CellError::OutOfRange("link read budget"))
    }

    /// Read a link page while charging its raw candidates before fetching
    /// their owned identities. `None` means the shared decode budget cannot
    /// admit the next candidate; the caller must resume from `after`.
    ///
    /// # Errors
    /// Unknown type, invalid candidate limit, or SQL errors.
    #[allow(
        clippy::too_many_arguments,
        reason = "the page query has a shared decode budget"
    )]
    pub fn links_touching_page_with_budget(
        &self,
        world: &str,
        object_type: &str,
        id: &str,
        direction: LinkDirection,
        link_type: Option<&str>,
        after: Option<&LinkPagePosition>,
        limit: usize,
        budget: &mut DecodeBudget,
        max_candidate_bytes: u64,
    ) -> Result<Option<LinkReadPage>, CellError> {
        budget.depend(crate::reactive::Dependency::Object {
            object_type: self.canonical_type(object_type),
            key: id.to_owned(),
        })?;
        budget.depend(crate::reactive::Dependency::Links {
            object_type: object_type.to_owned(),
            key: id.to_owned(),
            link: link_type.map(str::to_owned),
            outgoing: direction != LinkDirection::In,
            incoming: direction != LinkDirection::Out,
        })?;
        self.links_touching_page_inner(
            world,
            object_type,
            id,
            direction,
            link_type,
            after,
            limit,
            Some(budget),
            max_candidate_bytes,
        )
    }

    #[allow(
        clippy::too_many_arguments,
        clippy::too_many_lines,
        reason = "one bounded page couples keyset scanning, type checks, and liveness"
    )]
    fn links_touching_page_inner(
        &self,
        world: &str,
        object_type: &str,
        id: &str,
        direction: LinkDirection,
        link_type: Option<&str>,
        after: Option<&LinkPagePosition>,
        limit: usize,
        mut budget: Option<&mut DecodeBudget>,
        max_candidate_bytes: u64,
    ) -> Result<Option<LinkReadPage>, CellError> {
        if limit == 0 || limit > MAX_LINK_PAGE_CANDIDATES {
            return Err(CellError::OutOfRange("link page candidate limit"));
        }
        if self.model().tables(object_type)?.is_empty() {
            return Err(CellError::UnknownObjectType(object_type.to_owned()));
        }
        let minimum_candidate_bytes = u64::try_from(limit)
            .unwrap_or(u64::MAX)
            .saturating_mul(LINK_CANDIDATE_ROW_OVERHEAD);
        if budget.as_deref().is_some_and(|budget| {
            budget.remaining_rows() < limit || budget.remaining_bytes() < minimum_candidate_bytes
        }) {
            // Refuse before reading neighborhood metadata when the requested
            // page cannot fit its minimum row and byte cost. Empty and
            // non-empty neighborhoods then take the same exhaustion path.
            return Ok(None);
        }
        let Some((concrete, _, _)) = self.root_row(world, object_type, id)? else {
            return Ok(Some(LinkReadPage {
                items: Vec::new(),
                candidate_count: 0,
                candidate_bytes: 0,
                next_after: None,
                has_more_candidates: false,
            }));
        };
        let mut names: Vec<String> = self.model.chain(&concrete)?.to_vec();
        for name in self.model.chain(&concrete)? {
            names.extend(
                self.model
                    .def(name)?
                    .implements
                    .iter()
                    .map(|interface| interface.as_str().to_owned()),
            );
        }

        // The read admits stored interface targets as well as concrete types.
        // Capture those same aliases even for an empty edge range.
        if let Some(budget) = budget.as_deref_mut() {
            for name in &names {
                let outgoing = direction != LinkDirection::In && self.model.is_a(&concrete, name);
                let incoming = direction != LinkDirection::Out;
                if outgoing || incoming {
                    budget.depend(crate::reactive::Dependency::Links {
                        object_type: name.clone(),
                        key: id.to_owned(),
                        link: link_type.map(str::to_owned),
                        outgoing,
                        incoming,
                    })?;
                }
            }
        }

        let first_branch = if direction == LinkDirection::In {
            LinkDirection::In
        } else {
            LinkDirection::Out
        };
        let locator = after.and_then(|position| position.row_locator);
        let first_locator_order = locator
            .map(|locator| {
                link_locator_endpoint_order(
                    self.conn,
                    world,
                    id,
                    locator,
                    if first_branch == LinkDirection::Out {
                        "source"
                    } else {
                        "target"
                    },
                )
            })
            .transpose()?;
        let incoming_locator_order = if direction == LinkDirection::Both {
            locator
                .map(|locator| link_locator_endpoint_order(self.conn, world, id, locator, "target"))
                .transpose()?
        } else {
            None
        };
        let first_queries = link_candidate_page_sql(
            world,
            id,
            first_branch,
            link_type,
            after,
            limit,
            first_locator_order,
        )?;
        let mut metadata_streams = Vec::new();
        for (sql, bindings) in first_queries {
            metadata_streams.push(self.conn.query(&sql, &bindings)?);
        }
        if direction == LinkDirection::Both {
            let incoming_queries = link_candidate_page_sql(
                world,
                id,
                LinkDirection::In,
                link_type,
                after,
                limit,
                incoming_locator_order,
            )?;
            for (sql, bindings) in incoming_queries {
                metadata_streams.push(self.conn.query(&sql, &bindings)?);
            }
        }
        let length_rows = merge_link_candidate_metadata(self.conn, &metadata_streams, limit)?;
        let mut candidate_bytes = 0_u64;
        for values in length_rows.iter().take(limit) {
            let row = SqlRow(values.clone());
            candidate_bytes = candidate_bytes.saturating_add(LINK_CANDIDATE_ROW_OVERHEAD);
            for index in 1..7 {
                candidate_bytes = candidate_bytes.saturating_add(to_u64(
                    row.get::<i64>(index)?,
                    "link candidate field length",
                )?);
            }
        }
        let has_more_candidates = length_rows.len() > limit;
        if candidate_bytes > MAX_LINK_PAGE_BYTES.min(max_candidate_bytes) {
            if limit > 1 {
                return self.links_touching_page_inner(
                    world,
                    object_type,
                    id,
                    direction,
                    link_type,
                    after,
                    limit / 2,
                    budget,
                    max_candidate_bytes,
                );
            }
            // This candidate cannot supply a complete bounded authorization
            // view. Omit it fail-closed, and advance only an internal locator
            // without fetching or exposing its oversized identity.
            let locator = length_rows
                .first()
                .ok_or(CellError::OutOfRange("link scan metadata"))?;
            let locator = to_u64(SqlRow(locator.clone()).get(0)?, "link scan locator")?;
            return Ok(Some(LinkReadPage {
                items: Vec::new(),
                candidate_count: 1,
                candidate_bytes: 0,
                next_after: has_more_candidates.then(|| LinkPagePosition {
                    link_table: String::new(),
                    source: String::new(),
                    target: String::new(),
                    row_locator: Some(locator),
                }),
                has_more_candidates,
            }));
        }
        if let Some(budget) = budget {
            let selected = length_rows.len().min(limit);
            if budget.rows < selected || !budget.charge(candidate_bytes) {
                return Ok(None);
            }
            budget.rows = budget.rows.saturating_sub(selected);
        }
        // The extra metadata row proves raw continuation but its strings are
        // never fetched: an unreadable oversized lookahead cannot fail a page.
        let selected_locators = length_rows
            .iter()
            .take(limit)
            .map(|values| to_u64(SqlRow(values.clone()).get(0)?, "link scan locator"))
            .collect::<Result<Vec<_>, _>>()?;
        let mut fetched = link_candidate_rows_by_locator(self.conn, world, &selected_locators)?;
        let mut candidates = Vec::with_capacity(selected_locators.len());
        for locator in &selected_locators {
            candidates.push(fetched.remove(locator).ok_or_else(|| {
                CellError::Engine("link candidate changed during page read".to_owned())
            })?);
        }
        let candidate_count = candidates.len();
        let next_after = if has_more_candidates {
            candidates.last().map(|(table, edge, _)| LinkPagePosition {
                link_table: table.clone(),
                source: edge.source.clone(),
                target: edge.target.clone(),
                row_locator: None,
            })
        } else {
            None
        };

        let mut live = Vec::new();
        for (table, edge, version) in candidates {
            // The version table can retain rows across definition changes;
            // only query physical tables in the active model.
            if !self
                .model
                .schema()
                .link_tables
                .iter()
                .any(|definition| definition.table == table)
            {
                continue;
            }
            let as_source = edge.source == id && self.model.is_a(&concrete, &edge.source_type);
            let as_target = edge.target == id && names.contains(&edge.target_type);
            let matches_direction = match direction {
                LinkDirection::Out => as_source,
                LinkDirection::In => as_target,
                LinkDirection::Both => as_source || as_target,
            };
            if !matches_direction {
                continue;
            }
            let exists = self
                .conn
                .query_optional(
                    &format!(
                        "SELECT 1 FROM {} WHERE {} = ?1 AND {} = ?2 AND {} = ?3",
                        quote_identifier(&table),
                        quote_identifier(WORLD_COLUMN),
                        quote_identifier(SOURCE_COLUMN),
                        quote_identifier(TARGET_COLUMN),
                    ),
                    params![world, edge.source, edge.target],
                    |_| Ok(()),
                )?
                .is_some();
            if exists {
                let position = LinkPagePosition {
                    link_table: table,
                    source: edge.source.clone(),
                    target: edge.target.clone(),
                    row_locator: None,
                };
                live.push((edge, version, position));
            }
        }

        let items = live
            .into_iter()
            .map(|(edge, version, position)| LinkEdgeRecord {
                edge,
                version,
                position,
            })
            .collect();

        Ok(Some(LinkReadPage {
            items,
            candidate_count,
            candidate_bytes,
            next_after,
            has_more_candidates,
        }))
    }

    /// Resolve original creation commits for supplied live link incarnations.
    ///
    /// Call only after authorizing every endpoint that may be returned. The
    /// New commits record creation durably and property replacements preserve
    /// it. Legacy version-one rows have an unambiguous first-add receipt; its
    /// bounded lookup can refuse on exhaustion. Later legacy versions are
    /// ambiguous and refuse instead of reporting their last write as creation.
    /// These results must not influence whether an unauthorized candidate
    /// exists. Results align with `links`.
    ///
    /// # Errors
    ///
    /// SQL or receipt decode errors, or bounded history exhaustion.
    pub fn link_creation_commits(
        &self,
        world: &str,
        links: &[LinkEdgeRecord],
    ) -> Result<Vec<u64>, CellError> {
        self.link_creation_commits_inner(world, links, None)
    }

    /// Resolve creation evidence under a caller's remaining decode budget.
    /// Length metadata is charged before any legacy receipt body is fetched.
    ///
    /// # Errors
    /// SQL, unavailable creation evidence, or exhausted decode budget.
    pub fn link_creation_commits_with_budget(
        &self,
        world: &str,
        links: &[LinkEdgeRecord],
        budget: &mut DecodeBudget,
    ) -> Result<Vec<u64>, CellError> {
        self.link_creation_commits_inner(world, links, Some(budget))
    }

    fn link_creation_commits_inner(
        &self,
        world: &str,
        links: &[LinkEdgeRecord],
        mut budget: Option<&mut DecodeBudget>,
    ) -> Result<Vec<u64>, CellError> {
        let mut recorded = Vec::with_capacity(links.len());
        let mut versions = Vec::new();
        for link in links {
            if let Some(budget) = budget.as_deref_mut() {
                if budget.rows == 0 || !budget.charge(64) {
                    return Err(CellError::OutOfRange("link read budget"));
                }
                budget.rows = budget.rows.saturating_sub(1);
            }
            let evidence = self.conn.query_optional(
                &format!(
                    "SELECT \"created_commit\", \"creation_version\" FROM \"{LINK_CREATION}\" WHERE \"world\" = ?1 AND \"link_table\" = ?2 AND \"source\" = ?3 AND \"target\" = ?4"
                ),
                params![world, link.position.link_table, link.edge.source, link.edge.target],
                |row| Ok((row.get::<Option<i64>>(0)?, row.get::<Option<i64>>(1)?)),
            )?;
            match evidence {
                Some((Some(commit), _)) => {
                    recorded.push(Some(to_u64(commit, "link created commit")?));
                }
                Some((None, Some(1))) | None => {
                    if evidence.is_none() && link.version != 1 {
                        return Err(CellError::OutOfRange("link creation unavailable"));
                    }
                    recorded.push(None);
                    versions.push((link, 1));
                }
                _ => return Err(CellError::OutOfRange("link creation unavailable")),
            }
        }
        let commits = self.link_creation_commit_map(world, &versions, budget)?;
        links
            .iter()
            .zip(recorded)
            .map(|(link, recorded)| {
                recorded
                    .or_else(|| commits.get(&link_receipt_identity(link, 1)).copied())
                    .ok_or(CellError::OutOfRange("link creation unavailable"))
            })
            .collect()
    }

    fn link_creation_commit_map(
        &self,
        world: &str,
        links: &[(&LinkEdgeRecord, u64)],
        mut budget: Option<&mut DecodeBudget>,
    ) -> Result<BTreeMap<LinkReceiptIdentity, u64>, CellError> {
        let mut pending = links
            .iter()
            .map(|(link, version)| link_receipt_identity(link, *version))
            .collect::<BTreeSet<_>>();
        let mut found = BTreeMap::new();
        let mut scanned_rows = 0_usize;
        let mut scanned_bytes = 0_u64;
        let mut before = None;

        while !pending.is_empty() {
            if scanned_rows >= MAX_LINK_RECEIPT_LOOKUP_ROWS
                || scanned_bytes >= MAX_LINK_RECEIPT_LOOKUP_BYTES
            {
                return Err(CellError::OutOfRange("link read budget"));
            }
            let requested = LINK_RECEIPT_LOOKUP_BATCH
                .min(MAX_LINK_RECEIPT_LOOKUP_ROWS.saturating_sub(scanned_rows));
            let length_rows = self.receipt_lengths_descending(before, requested)?;
            if length_rows.is_empty() {
                return Err(CellError::OutOfRange("link creation unavailable"));
            }

            let mut admitted_rows = 0_usize;
            let mut admitted_bytes = 0_u64;
            for values in &length_rows {
                let row = SqlRow(values.clone());
                let mut row_bytes = 64_u64;
                for index in 1..6 {
                    row_bytes = row_bytes
                        .saturating_add(to_u64(row.get::<i64>(index)?, "receipt field length")?);
                }
                if scanned_bytes
                    .saturating_add(admitted_bytes)
                    .saturating_add(row_bytes)
                    > MAX_LINK_RECEIPT_LOOKUP_BYTES
                    || budget.as_ref().is_some_and(|budget| {
                        admitted_rows >= budget.rows
                            || admitted_bytes.saturating_add(row_bytes) > budget.remaining_bytes()
                    })
                {
                    break;
                }
                admitted_rows = admitted_rows.saturating_add(1);
                admitted_bytes = admitted_bytes.saturating_add(row_bytes);
            }
            if admitted_rows == 0 {
                return Err(CellError::OutOfRange("link read budget"));
            }

            if let Some(budget) = budget.as_deref_mut() {
                if !budget.charge(admitted_bytes) {
                    return Err(CellError::OutOfRange("link read budget"));
                }
                budget.rows = budget.rows.saturating_sub(admitted_rows);
            }

            let receipts = self.receipts_descending(before, admitted_rows)?;
            if receipts.len() != admitted_rows {
                return Err(CellError::Engine(
                    "receipt history changed during link creation lookup".to_owned(),
                ));
            }
            let oldest = receipts.last().map(|receipt| receipt.body.commit_seq);
            for receipt in &receipts {
                if receipt.body.world != world {
                    continue;
                }
                for linked in &receipt.body.linked {
                    let Ok((_, table)) = self.model.link(&linked.object_type, &linked.link) else {
                        continue;
                    };
                    let key = (
                        table.table.clone(),
                        linked.source.clone(),
                        linked.target.clone(),
                        linked.version,
                    );
                    if pending.remove(&key) {
                        found.insert(key, receipt.body.commit_seq);
                    }
                }
            }
            scanned_rows = scanned_rows.saturating_add(admitted_rows);
            scanned_bytes = scanned_bytes.saturating_add(admitted_bytes);
            before = oldest;

            if admitted_rows < length_rows.len() && !pending.is_empty() {
                return Err(CellError::OutOfRange("link read budget"));
            }
            if length_rows.len() < requested && !pending.is_empty() {
                return Err(CellError::OutOfRange("link creation unavailable"));
            }
        }
        Ok(found)
    }

    fn receipt_lengths_descending(
        &self,
        before: Option<u64>,
        limit: usize,
    ) -> Result<Vec<Vec<SqlValue>>, CellError> {
        let limit = i64::try_from(limit).unwrap_or(MAX_SAFE_INTEGER);
        let mut bindings = Vec::with_capacity(2);
        let sql = if let Some(before) = before {
            bindings.extend([
                SqlValue::from(to_i64(before, "commit_seq")?),
                SqlValue::from(limit),
            ]);
            format!(
                "SELECT \"commit_seq\", COALESCE(octet_length(\"world\"), 0), COALESCE(octet_length(\"idempotency_key\"), 0), COALESCE(octet_length(\"request_hash\"), 0), COALESCE(octet_length(\"receipt\"), 0), COALESCE(octet_length(\"receipt_hash\"), 0) FROM \"{RECEIPT}\" WHERE \"commit_seq\" < ?1 ORDER BY \"commit_seq\" DESC LIMIT ?2"
            )
        } else {
            bindings.push(SqlValue::from(limit));
            format!(
                "SELECT \"commit_seq\", COALESCE(octet_length(\"world\"), 0), COALESCE(octet_length(\"idempotency_key\"), 0), COALESCE(octet_length(\"request_hash\"), 0), COALESCE(octet_length(\"receipt\"), 0), COALESCE(octet_length(\"receipt_hash\"), 0) FROM \"{RECEIPT}\" ORDER BY \"commit_seq\" DESC LIMIT ?1"
            )
        };
        self.conn.query(&sql, &bindings)
    }

    fn receipts_descending(
        &self,
        before: Option<u64>,
        limit: usize,
    ) -> Result<Vec<Receipt>, CellError> {
        let limit = i64::try_from(limit).unwrap_or(MAX_SAFE_INTEGER);
        let mut bindings = Vec::with_capacity(2);
        let sql = if let Some(before) = before {
            bindings.extend([
                SqlValue::from(to_i64(before, "commit_seq")?),
                SqlValue::from(limit),
            ]);
            format!(
                "SELECT \"commit_seq\", \"world\", \"idempotency_key\", \"request_hash\", \"receipt\", \"receipt_hash\" FROM \"{RECEIPT}\" WHERE \"commit_seq\" < ?1 ORDER BY \"commit_seq\" DESC LIMIT ?2"
            )
        } else {
            bindings.push(SqlValue::from(limit));
            format!(
                "SELECT \"commit_seq\", \"world\", \"idempotency_key\", \"request_hash\", \"receipt\", \"receipt_hash\" FROM \"{RECEIPT}\" ORDER BY \"commit_seq\" DESC LIMIT ?1"
            )
        };
        decode_receipts(self.conn.query(&sql, &bindings)?)
    }

    /// Every live link with object `id` (addressed as `object_type`) at
    /// either end (#3013), named by the endpoint types it was last written
    /// with. A link counts when the object is its source through the link's
    /// declaring type hierarchy, or its target through the written target
    /// type: the object's type, a supertype, or an interface it implements.
    /// Empty when the object does not exist.
    ///
    /// # Errors
    ///
    /// Unknown type; SQL engine errors.
    pub fn links_touching(
        &self,
        world: &str,
        object_type: &str,
        id: &str,
    ) -> Result<Vec<LinkRef>, CellError> {
        let Some((concrete, _, _)) = self.root_row(world, object_type, id)? else {
            return Ok(Vec::new());
        };
        let mut names: Vec<String> = self.model.chain(&concrete)?.to_vec();
        for name in self.model.chain(&concrete)? {
            names.extend(
                self.model
                    .def(name)?
                    .implements
                    .iter()
                    .map(|interface| interface.as_str().to_owned()),
            );
        }
        let rows = self.conn.query(
            &format!(
                "SELECT \"link_table\", \"source\", \"target\", \"link\", \"source_type\", \"target_type\" FROM \"{LINK_VERSIONS}\" WHERE \"world\" = ?1 AND (\"source\" = ?2 OR \"target\" = ?2) ORDER BY \"link_table\", \"source\", \"target\""
            ),
            params![world, id],
        )?;
        let mut candidates = Vec::new();
        for values in rows {
            let row = SqlRow(values);
            candidates.push((
                row.get::<String>(0)?,
                LinkRef {
                    source: row.get(1)?,
                    target: row.get(2)?,
                    link: row.get(3)?,
                    source_type: row.get(4)?,
                    target_type: row.get(5)?,
                },
            ));
        }
        let mut out = Vec::new();
        for (table, link) in candidates {
            let as_source = link.source == id && self.model.is_a(&concrete, &link.source_type);
            let as_target = link.target == id && names.contains(&link.target_type);
            if !(as_source || as_target) {
                continue;
            }
            let live = self
                .conn
                .query_optional(
                    &format!(
                        "SELECT 1 FROM {} WHERE {} = ?1 AND {} = ?2 AND {} = ?3",
                        quote_identifier(&table),
                        quote_identifier(WORLD_COLUMN),
                        quote_identifier(SOURCE_COLUMN),
                        quote_identifier(TARGET_COLUMN),
                    ),
                    params![world, link.source, link.target],
                    |_| Ok(()),
                )?
                .is_some();
            if live {
                out.push(link);
            }
        }
        Ok(out)
    }

    /// The next commit sequence number.
    pub(crate) fn next_commit_seq(&self) -> Result<u64, CellError> {
        let last: i64 = self.conn.query_row(
            &format!("SELECT COALESCE(MAX(\"commit_seq\"), 0) FROM \"{RECEIPT}\""),
            params![],
            |row| row.get(0),
        )?;
        next(to_u64(last, "commit_seq")?, "commit_seq")
    }

    /// The ledger head: last sequence number and hash.
    pub(crate) fn ledger_tail(&self) -> Result<(u64, String), CellError> {
        let head: Option<(i64, String)> = self.conn.query_optional(
            &format!("SELECT \"seq\", \"hash\" FROM \"{LEDGER}\" ORDER BY \"seq\" DESC LIMIT 1"),
            params![],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        match head {
            Some((seq, hash)) => Ok((to_u64(seq, "seq")?, hash)),
            None => Ok((0, GENESIS_HASH.to_owned())),
        }
    }

    /// Ledger entries with `seq >= from`, in order.
    ///
    /// # Errors
    ///
    /// SQL engine or decode errors.
    pub fn ledger(&self, from: u64) -> Result<Vec<LedgerEntry>, CellError> {
        self.ledger_range(from, None)
    }

    /// At most `limit` ledger entries with `seq > after`, in order: one page,
    /// read with a bounded query (#2970), so a caller never loads the whole
    /// backlog to take a batch.
    ///
    /// # Errors
    ///
    /// SQL engine or decode errors.
    pub fn ledger_page(&self, after: u64, limit: usize) -> Result<Vec<LedgerEntry>, CellError> {
        // No stored sequence exceeds the host's exact-integer bound, so a cursor at or past it
        // has nothing after it.
        match i64::try_from(after) {
            Ok(after) if after < MAX_SAFE_INTEGER => {}
            _ => return Ok(Vec::new()),
        }
        self.ledger_range(after.saturating_add(1), Some(limit))
    }

    /// Read a ledger page for projection, masking every unproven payload.
    /// The stored hashes still refer to the original durability records.
    ///
    /// # Errors
    /// Storage failures or inconsistent value commitments.
    pub fn verified_ledger_page(
        &self,
        after: u64,
        limit: usize,
    ) -> Result<Vec<LedgerEntry>, CellError> {
        self.project_history_entries(self.ledger_page(after, limit)?)
    }

    /// Budgeted counterpart of `verified_ledger_page`; values pass the same accessor.
    ///
    /// # Errors
    /// Storage failures or inconsistent value commitments.
    pub fn verified_ledger_page_with_budget(
        &self,
        after: u64,
        limit: usize,
        budget: &mut crate::reactive::DecodeBudget,
    ) -> Result<Option<Vec<LedgerEntry>>, CellError> {
        self.ledger_page_with_budget(after, limit, budget)?
            .map(|entries| self.project_history_entries(entries))
            .transpose()
    }

    fn project_history_entries(
        &self,
        entries: Vec<LedgerEntry>,
    ) -> Result<Vec<LedgerEntry>, CellError> {
        entries
            .into_iter()
            .map(|mut entry| {
                entry.event = self
                    .verified_history_event(&entry)?
                    .unwrap_or_else(|| norn_ir::runtime_object! { "masked" => true });
                Ok(entry)
            })
            .collect()
    }

    /// Resolve the complete immutable classifications of an event's receipt.
    /// Missing attribution or any incomplete fact snapshot returns `None`.
    /// Current visibility and declassification never rewrite this lineage.
    ///
    /// # Errors
    /// Storage or classification decode errors.
    pub fn ledger_markings(
        &self,
        entry: &LedgerEntry,
    ) -> Result<Option<BTreeSet<String>>, CellError> {
        if self.verified_history_event(entry)?.is_none() {
            return Ok(None);
        }
        let Some(receipt) = self.receipt_at(entry.commit_seq)? else {
            return Ok(None);
        };
        let body = receipt.body;
        if body.world != entry.world
            || entry.seq < body.first_event_seq
            || entry.seq.saturating_sub(body.first_event_seq) >= body.event_count
        {
            return Ok(None);
        }
        // Definition-install metadata is attributed to retained declarative
        // definitions, rather than to a business fact. An ordinary event with
        // the same name cannot borrow an earlier install's public lineage.
        if entry.event.get("type").and_then(Value::as_str) == Some("definitions.installed")
            && body.definitions_version != 0
            && let Some(installed) = self.installed_definitions(Some(body.definitions_version))?
            && installed.commit_seq == entry.commit_seq
            && entry
                .event
                .get("version")
                .and_then(Value::as_i64)
                .and_then(|version| u64::try_from(version).ok())
                == Some(installed.version)
            && entry.event.get("receiptSha256").and_then(Value::as_str)
                == Some(installed.receipt_sha256.as_str())
        {
            return Ok(Some(BTreeSet::new()));
        }
        let mut written = body.written;
        // Visibility-only changes emit no fact version. Their audit payload
        // is still bound to the exact historical subject they authorized.
        if entry.event.get("type").and_then(Value::as_str) == Some("marking.declassified") {
            let Some(object_type) = entry.event.get("objectType").and_then(Value::as_str) else {
                return Ok(None);
            };
            let Some(key) = entry.event.get("key").and_then(Value::as_str) else {
                return Ok(None);
            };
            let Some(version) = entry
                .event
                .get("expectedVersion")
                .and_then(Value::as_i64)
                .and_then(|v| u64::try_from(v).ok())
            else {
                return Ok(None);
            };
            written.push(crate::store::WrittenRow {
                object_type: object_type.to_owned(),
                key: key.to_owned(),
                version,
            });
        }
        if written.is_empty() {
            return Ok(None);
        }
        let mut labels = BTreeSet::new();
        for row in written {
            let Some(snapshot) = self.effective_markings(
                &entry.world,
                &row.object_type,
                &row.key,
                Some(row.version),
            )?
            else {
                return Ok(None);
            };
            labels.extend(snapshot.into_values().flatten());
            let Some(occurrence) = self.fact_occurrence_markings(
                &entry.world,
                &row.object_type,
                &row.key,
                Some(row.version),
            )?
            else {
                return Ok(None);
            };
            labels.extend(occurrence);
        }
        Ok(Some(labels))
    }

    /// Immutable source classifications of a typed fact trigger. Other writes
    /// in the receipt (such as raw staging records) were not evaluated as the
    /// fact event's values and do not classify those values. Unattributed event
    /// kinds fall back to the complete receipt's conservative lineage.
    ///
    /// # Errors
    /// Storage or classification decode errors.
    pub fn fact_event_markings(
        &self,
        entry: &LedgerEntry,
    ) -> Result<Option<BTreeSet<String>>, CellError> {
        let Some(verified) = self.verified_history_event(entry)? else {
            return Ok(None);
        };
        let mut projected = entry.clone();
        projected.event = verified;
        let event = entry.rule_input();
        // A projection can omit unproven envelope text. Raw rule evaluation
        // must not inherit public labels for text absent from that proof.
        if projected.rule_input() != event {
            return Ok(None);
        }
        if !matches!(
            event.get("op").and_then(Value::as_str),
            Some("created" | "set" | "modified" | "deleted" | "transitioned")
        ) {
            return self.ledger_markings(entry);
        }
        let Some(raw_key) = event.get("key") else {
            return Ok(None);
        };
        let Ok(key) = norn_ir::runtime::decode::<norn_kernel::FactKey>(raw_key) else {
            return Ok(None);
        };
        let object_type = key.object_type.as_str();
        let Some(receipt) = self.receipt_at(entry.commit_seq)? else {
            return Ok(None);
        };
        let body = receipt.body;
        if body.world != entry.world
            || entry.seq < body.first_event_seq
            || entry.seq.saturating_sub(body.first_event_seq) >= body.event_count
        {
            return Ok(None);
        }
        let mut labels = BTreeSet::new();
        let mut attributed = false;
        for row in body
            .written
            .iter()
            .filter(|row| row.key == key.key && self.model.is_a(&row.object_type, object_type))
        {
            let Some(snapshot) = self.effective_markings(
                &entry.world,
                &row.object_type,
                &row.key,
                Some(row.version),
            )?
            else {
                return Ok(None);
            };
            attributed = true;
            labels.extend(snapshot.into_values().flatten());
            let Some(occurrence) = self.fact_occurrence_markings(
                &entry.world,
                &row.object_type,
                &row.key,
                Some(row.version),
            )?
            else {
                return Ok(None);
            };
            labels.extend(occurrence);
        }
        Ok(attributed.then_some(labels))
    }

    /// Read one ledger page only when its entries fit the caller's remaining
    /// row and byte budget. Event lengths are checked before the JSON event
    /// bodies are fetched and decoded. `None` means the next page would exceed
    /// the budget; an empty vector means the ledger is exhausted.
    ///
    /// # Errors
    ///
    /// SQL engine, sequence-range, or event decode errors.
    pub fn ledger_page_with_budget(
        &self,
        after: u64,
        limit: usize,
        budget: &mut DecodeBudget,
    ) -> Result<Option<Vec<LedgerEntry>>, CellError> {
        if limit == 0 {
            return Ok(Some(Vec::new()));
        }
        if i64::try_from(after).is_err() || after >= u64::try_from(MAX_SAFE_INTEGER).unwrap_or(0) {
            return Ok(Some(Vec::new()));
        }
        if limit > budget.remaining_rows() {
            return Ok(None);
        }
        let page_limit_sql = i64::try_from(limit).unwrap_or(MAX_SAFE_INTEGER);
        let length_rows = self.conn.query(
            &format!(
                "SELECT \"seq\", COALESCE(octet_length(\"world\"), 0), COALESCE(octet_length(\"event\"), 0), COALESCE(octet_length(\"prev_hash\"), 0), COALESCE(octet_length(\"hash\"), 0) FROM \"{LEDGER}\" WHERE \"seq\" > ?1 ORDER BY \"seq\" LIMIT ?2"
            ),
            params![to_i64(after, "seq")?, page_limit_sql],
        )?;
        let mut bytes = 0_u64;
        for values in &length_rows {
            let row = SqlRow(values.clone());
            bytes = bytes.saturating_add(64);
            for index in 1..5 {
                bytes =
                    bytes.saturating_add(to_u64(row.get::<i64>(index)?, "ledger field length")?);
            }
        }
        if length_rows.len() > budget.remaining_rows() || !budget.charge(bytes) {
            return Ok(None);
        }
        budget.rows = budget.rows.saturating_sub(length_rows.len());
        self.ledger_page(after, length_rows.len()).map(Some)
    }

    /// Scan ledger positions while decoding only finding producer candidates
    /// and their same-commit observation metadata. Unrelated payloads consume
    /// the row budget without consuming the candidate payload byte budget.
    /// Lengths and all scanned rows are admitted before any body is decoded.
    /// Candidates still require verified history access before disclosure.
    ///
    /// # Errors
    /// SQL, sequence-range, durable evidence or event decode errors.
    #[allow(
        clippy::too_many_lines,
        reason = "Keep the row/byte admission and same-commit evidence policy together"
    )]
    pub fn finding_ledger_page_with_budget(
        &self,
        after: u64,
        limit: usize,
        budget: &mut DecodeBudget,
    ) -> Result<Option<FindingLedgerPage>, CellError> {
        let mut page = FindingLedgerPage {
            entries: Vec::new(),
            scanned_rows: 0,
            scanned_through: after,
            scanned_commits: BTreeSet::new(),
        };
        if limit == 0 || after >= u64::try_from(MAX_SAFE_INTEGER).unwrap_or(0) {
            return Ok(Some(page));
        }
        // Check end-of-history even with no rows left: exactly exhausting the
        // admitted scan is valid, but another real ledger row must refuse.
        let lengths = self.conn.query(
            &format!("SELECT \"seq\", \"commit_seq\", json_extract(\"event\", '$.op') IN ('data_quality_finding', 'observed_out_of_band'), json_extract(\"event\", '$.type') = 'observation.recorded', octet_length(\"event\") + octet_length(\"world\") + octet_length(\"prev_hash\") + octet_length(\"hash\") + 64 FROM \"{LEDGER}\" WHERE \"seq\" > ?1 ORDER BY \"seq\" LIMIT ?2"),
            params![to_i64(after, "seq")?, i64::try_from(limit.min(budget.remaining_rows().saturating_add(1))).unwrap_or(MAX_SAFE_INTEGER)],
        )?;
        if lengths.len() > budget.remaining_rows() {
            return Ok(None);
        }
        // Reserve the outer page, then charge every same-commit lookup row to
        // the same request budget. The extra row is a refusal sentinel, never
        // an invitation to parse another commit or all future ledger history.
        let mut work_rows = lengths.len();
        let mut bytes = 0_u64;
        let mut candidates = Vec::new();
        let mut finding_commits = BTreeMap::new();
        for values in lengths {
            let row = SqlRow(values);
            let sequence = to_u64(row.get::<i64>(0)?, "seq")?;
            let commit = to_u64(row.get::<i64>(1)?, "commit_seq")?;
            page.scanned_through = sequence;
            page.scanned_commits.insert(commit);
            let finding = row.get::<Option<i64>>(2)? == Some(1);
            let metadata = row.get::<Option<i64>>(3)? == Some(1);
            let associated = if metadata {
                if let Some(found) = finding_commits.get(&commit) {
                    *found
                } else {
                    let remaining = budget.remaining_rows().saturating_sub(work_rows);
                    let lookup = self.conn.query(
                        &format!("SELECT CASE WHEN json_extract(\"event\", '$.op') IN ('data_quality_finding', 'observed_out_of_band') THEN octet_length(\"event\") + octet_length(\"world\") + octet_length(\"prev_hash\") + octet_length(\"hash\") + 64 ELSE NULL END FROM \"{LEDGER}\" WHERE \"commit_seq\" = ?1 ORDER BY \"seq\" LIMIT ?2"),
                        params![to_i64(commit, "commit_seq")?, i64::try_from(remaining.saturating_add(1)).unwrap_or(MAX_SAFE_INTEGER)],
                    )?;
                    if lookup.len() > remaining {
                        return Ok(None);
                    }
                    work_rows = work_rows.saturating_add(lookup.len());
                    let mut found = false;
                    for values in lookup {
                        if let Some(length) = SqlRow(values).get::<Option<i64>>(0)? {
                            found = true;
                            // Actual finding evidence is subject to the byte
                            // policy even when it lies beyond this outer page.
                            // Unrelated raw observation bodies stay excluded.
                            bytes = bytes.saturating_add(to_u64(length, "finding lookup length")?);
                        }
                    }
                    finding_commits.insert(commit, found);
                    found
                }
            } else {
                false
            };
            if finding || associated {
                bytes =
                    bytes.saturating_add(to_u64(row.get::<i64>(4)?, "ledger candidate length")?);
                candidates.push(sequence);
            }
        }
        page.scanned_rows = work_rows;
        if !budget.charge(bytes) {
            return Ok(None);
        }
        budget.rows = budget.rows.saturating_sub(page.scanned_rows);
        for sequence in candidates {
            page.entries
                .extend(self.ledger_page(sequence.saturating_sub(1), 1)?);
        }
        Ok(Some(page))
    }

    /// The active retained definition version without loading its payload.
    ///
    /// # Errors
    /// SQL errors or a dangling active-version pointer.
    pub fn active_definitions_version(&self) -> Result<Option<u64>, CellError> {
        validate_active_definition_pointer(self.conn)
    }

    /// The retained definition set at `version`, or the active set when
    /// `version` is `None`.
    ///
    /// # Errors
    ///
    /// SQL, decoding, or a dangling active-version pointer.
    pub fn installed_definitions(
        &self,
        version: Option<u64>,
    ) -> Result<Option<InstalledDefinitionSet>, CellError> {
        let version = if let Some(version) = version {
            version
        } else {
            let Some(version) = validate_active_definition_pointer(self.conn)? else {
                return Ok(None);
            };
            version
        };
        let installed = definition_by_version(self.conn, version)?;
        if installed.is_none()
            && version == validate_active_definition_pointer(self.conn)?.unwrap_or(0)
        {
            return Err(CellError::LedgerCorrupt {
                seq: version,
                reason: "active definitions pointer has no retained version".to_owned(),
            });
        }
        Ok(installed)
    }

    /// The active definition version in force at `commit_seq`, derived from
    /// the retained version rows rather than from user-emittable ledger data.
    ///
    /// # Errors
    ///
    /// SQL, decoding, sequence-range, or receipt-integrity errors.
    pub fn definitions_version_at(&self, commit_seq: u64) -> Result<Option<u64>, CellError> {
        if commit_seq == 0 {
            return Ok(None);
        }
        let row: Option<(i64, String, String)> = self.conn.query_optional(
            &format!(
                "SELECT \"commit_seq\", \"receipt\", \"receipt_hash\" FROM \"{RECEIPT}\" WHERE \"commit_seq\" <= ?1 ORDER BY \"commit_seq\" DESC LIMIT 1"
            ),
            params![to_i64(commit_seq, "commit_seq")?],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )?;
        let Some((stored_seq, serialized, stored_hash)) = row else {
            return Ok(None);
        };
        let stored_seq = to_u64(stored_seq, "commit_seq")?;
        let body: ReceiptBody = serde_json::from_str(&serialized)?;
        if body.commit_seq != stored_seq || stored_hash != hash_json(&body)? {
            return Err(CellError::LedgerCorrupt {
                seq: stored_seq,
                reason: "definitions-version receipt does not verify".to_owned(),
            });
        }
        Ok((body.definitions_version != 0).then_some(body.definitions_version))
    }

    /// All retained definition versions in ascending order.
    ///
    /// # Errors
    ///
    /// SQL or decoding errors.
    pub fn installed_definition_versions(&self) -> Result<Vec<InstalledDefinitionSet>, CellError> {
        let rows = self.conn.query(
            &format!(
                "SELECT \"version\", \"receipt_sha256\", \"canonical_set\", \"model_hash\", \"ddl_hash\", \"installed_at\", \"commit_seq\" FROM \"{DEFINITION_VERSION}\" ORDER BY \"version\""
            ),
            params![],
        )?;
        let has_registry_table = definition_registry_table_exists(self.conn)?;
        let has_admission_receipt_table = definition_admission_receipt_table_exists(self.conn)?;
        rows.into_iter()
            .map(|values| {
                let mut installed = definition_from_row(&SqlRow(values))?;
                if has_registry_table {
                    installed.registry =
                        definition_registry_for_version(self.conn, installed.version)?;
                }
                if has_admission_receipt_table {
                    installed.admission_receipt =
                        definition_admission_receipt_for_version(self.conn, installed.version)?;
                }
                (installed.layout, installed.layout_revision) =
                    definition_layout_for_version(self.conn, installed.version)?;
                Ok(installed)
            })
            .collect()
    }

    fn ledger_range(&self, from: u64, limit: Option<usize>) -> Result<Vec<LedgerEntry>, CellError> {
        // SQLite: a negative LIMIT is no limit.
        let limit = match limit {
            Some(limit) => i64::try_from(limit)
                .unwrap_or(MAX_SAFE_INTEGER)
                .min(MAX_SAFE_INTEGER),
            None => -1,
        };
        let rows = self.conn.query(
            &format!(
                "SELECT \"seq\", \"commit_seq\", \"world\", \"event\", \"prev_hash\", \"hash\" FROM \"{LEDGER}\" WHERE \"seq\" >= ?1 ORDER BY \"seq\" LIMIT ?2"
            ),
            params![to_i64(from, "seq")?, limit],
        )?;
        let mut out = Vec::new();
        for values in rows {
            let row = SqlRow(values);
            let event: String = row.get(3)?;
            let entry = LedgerEntry {
                seq: to_u64(row.get(0)?, "seq")?,
                commit_seq: to_u64(row.get(1)?, "commit_seq")?,
                world: row.get(2)?,
                event: serde_json::from_str(&event)?,
                prev_hash: row.get(4)?,
                hash: row.get(5)?,
            };
            self.verify_durable_event(&entry.world, entry.commit_seq, &entry.event)?;
            out.push(entry);
        }
        Ok(out)
    }

    /// Every receipt, in commit order.
    ///
    /// # Errors
    ///
    /// SQL engine or decode errors; [`CellError::LedgerCorrupt`] when a
    /// receipt's lookup columns disagree with its hashed body.
    pub fn receipts(&self) -> Result<Vec<Receipt>, CellError> {
        self.receipts_from(0)
    }

    /// Find one commit receipt by its world-scoped idempotency key.
    ///
    /// # Errors
    ///
    /// SQL engine or receipt decode errors.
    pub fn receipt_by_idempotency_key(
        &self,
        world: &str,
        key: &str,
    ) -> Result<Option<Receipt>, CellError> {
        receipt_by_key(self.conn, world, key)
    }

    /// Receipts with `commit_seq >= from`, in commit order.
    fn receipts_from(&self, from: u64) -> Result<Vec<Receipt>, CellError> {
        // The full scan deliberately has no lower bound: filtering by the
        // host's integer range could hide a corrupt out-of-range receipt.
        let rows = if from == 0 {
            self.conn.query(
                &format!(
                    "SELECT \"commit_seq\", \"world\", \"idempotency_key\", \"request_hash\", \"receipt\", \"receipt_hash\" FROM \"{RECEIPT}\" ORDER BY \"commit_seq\""
                ),
                params![],
            )?
        } else {
            self.conn.query(
                &format!(
                    "SELECT \"commit_seq\", \"world\", \"idempotency_key\", \"request_hash\", \"receipt\", \"receipt_hash\" FROM \"{RECEIPT}\" WHERE \"commit_seq\" >= ?1 ORDER BY \"commit_seq\""
                ),
                params![to_i64(from, "commit_seq")?],
            )?
        };
        decode_receipts(rows)
    }

    /// At most `limit` receipts with `commit_seq > after`, in commit order:
    /// the bounded cursor log that reactive subscriptions publish and resume
    /// from (#2694). Each receipt's lookup columns are checked against its
    /// hashed body, as in [`Self::receipts`].
    ///
    /// # Errors
    ///
    /// SQL engine or decode errors; [`CellError::LedgerCorrupt`] when a
    /// receipt's lookup columns disagree with its hashed body.
    pub fn receipts_after(&self, after: u64, limit: usize) -> Result<Vec<Receipt>, CellError> {
        let limit = i64::try_from(limit)
            .unwrap_or(MAX_SAFE_INTEGER)
            .min(MAX_SAFE_INTEGER);
        let rows = self.conn.query(
            &format!(
                "SELECT \"commit_seq\", \"world\", \"idempotency_key\", \"request_hash\", \"receipt\", \"receipt_hash\" FROM \"{RECEIPT}\" WHERE \"commit_seq\" > ?1 ORDER BY \"commit_seq\" LIMIT ?2"
            ),
            params![to_i64(after, "commit_seq")?, limit],
        )?;
        decode_receipts(rows)
    }

    /// Read a bounded receipt page only when all receipt bodies fit the
    /// caller's remaining candidate byte budget. Length metadata is fetched
    /// first, so an over-budget page is never materialized or decoded.
    /// Returns `None` on exhaustion without charging the budget, leaving it
    /// available for conservative slice re-evaluation.
    ///
    /// # Errors
    /// SQL engine or decode errors; [`CellError::LedgerCorrupt`] when a
    /// receipt's lookup columns disagree with its hashed body.
    pub fn receipts_after_with_budget(
        &self,
        after: u64,
        limit: usize,
        budget: &mut DecodeBudget,
    ) -> Result<Option<Vec<Receipt>>, CellError> {
        let limit = i64::try_from(limit)
            .unwrap_or(MAX_SAFE_INTEGER)
            .min(MAX_SAFE_INTEGER);
        let length_rows = self.conn.query(
            &format!(
                "SELECT COALESCE(octet_length(\"world\"), 0), COALESCE(octet_length(\"idempotency_key\"), 0), COALESCE(octet_length(\"request_hash\"), 0), COALESCE(octet_length(\"receipt\"), 0), COALESCE(octet_length(\"receipt_hash\"), 0) FROM \"{RECEIPT}\" WHERE \"commit_seq\" > ?1 ORDER BY \"commit_seq\" LIMIT ?2"
            ),
            params![to_i64(after, "commit_seq")?, limit],
        )?;
        let mut bytes = 0_u64;
        for values in &length_rows {
            let row = SqlRow(values.clone());
            bytes = bytes.saturating_add(64);
            for index in 0..5 {
                bytes =
                    bytes.saturating_add(to_u64(row.get::<i64>(index)?, "receipt field length")?);
            }
        }
        if !budget.charge(bytes) {
            return Ok(None);
        }
        self.receipts_after(after, usize::try_from(limit).unwrap_or(usize::MAX))
            .map(Some)
    }

    /// Indexed pending-link record identities for one canonical source output.
    /// Row and byte budgets are applied before fetching record keys. `None`
    /// refuses an incomplete result rather than silently dropping intents.
    ///
    /// # Errors
    /// SQL engine or out-of-range length metadata.
    pub fn pending_link_records_from_with_budget(
        &self,
        world: &str,
        source_type: &str,
        source_key: &str,
        budget: &mut DecodeBudget,
    ) -> Result<Option<Vec<String>>, CellError> {
        let limit = i64::try_from(budget.remaining_rows().saturating_add(1))
            .unwrap_or(MAX_SAFE_INTEGER)
            .min(MAX_SAFE_INTEGER);
        let lengths = self.conn.query(
            &format!("SELECT octet_length(record_key) FROM \"{PENDING_LINK_INDEX}\" WHERE world = ?1 AND source_type = ?2 AND source_key = ?3 ORDER BY record_key LIMIT ?4"),
            params![world, source_type, source_key, limit],
        )?;
        let mut bytes = 0_u64;
        for values in &lengths {
            bytes = bytes.saturating_add(64).saturating_add(to_u64(
                SqlRow(values.clone()).get::<i64>(0)?,
                "pending record key length",
            )?);
        }
        if lengths.len() > budget.remaining_rows() || !budget.charge(bytes) {
            return Ok(None);
        }
        if !budget.charge_rows(lengths.len()) {
            return Ok(None);
        }
        self.conn.query(
            &format!("SELECT record_key FROM \"{PENDING_LINK_INDEX}\" WHERE world = ?1 AND source_type = ?2 AND source_key = ?3 ORDER BY record_key LIMIT ?4"),
            params![world, source_type, source_key, limit],
        )?.into_iter().map(|values| SqlRow(values).get::<String>(0)).collect::<Result<Vec<_>, _>>().map(Some)
    }

    /// A bounded prefix of intents indexed by canonical target identity.
    /// Keys are charged from length metadata before retrieval. The boolean
    /// requests continuation; deleting consumed intents advances the prefix.
    ///
    /// # Errors
    /// SQL or invalid length metadata.
    pub fn pending_link_records_to_with_budget(
        &self,
        world: &str,
        target_type: &str,
        target_key: &str,
        limit: usize,
        budget: &mut DecodeBudget,
    ) -> Result<(Vec<String>, bool), CellError> {
        // Charge the lookup itself, including empty target neighborhoods.
        if !budget.charge_rows(1)
            || !budget.charge(
                u64::try_from(target_type.len().saturating_add(target_key.len()))
                    .unwrap_or(u64::MAX)
                    .saturating_add(64),
            )
        {
            return Ok((Vec::new(), true));
        }
        let limit = limit.min(64).min(budget.remaining_rows());
        if limit == 0 {
            return Ok((Vec::new(), true));
        }
        let lengths = self.conn.query(
            &format!("SELECT octet_length(record_key) FROM \"{PENDING_LINK_INDEX}\" WHERE world = ?1 AND target_type = ?2 AND target_key = ?3 ORDER BY record_key LIMIT ?4"),
            params![world, target_type, target_key, i64::try_from(limit.saturating_add(1)).unwrap_or(MAX_SAFE_INTEGER)],
        )?;
        let mut admitted = 0_usize;
        for values in lengths.iter().take(limit) {
            let bytes = to_u64(
                SqlRow(values.clone()).get::<i64>(0)?,
                "pending record key length",
            )?
            .saturating_add(64);
            if !budget.charge(bytes) || !budget.charge_rows(1) {
                break;
            }
            admitted = admitted.saturating_add(1);
        }
        let more = lengths.len() > admitted;
        let keys = self.conn.query(
            &format!("SELECT record_key FROM \"{PENDING_LINK_INDEX}\" WHERE world = ?1 AND target_type = ?2 AND target_key = ?3 ORDER BY record_key LIMIT ?4"),
            params![world, target_type, target_key, i64::try_from(admitted).unwrap_or(MAX_SAFE_INTEGER)],
        )?.into_iter().map(|values| SqlRow(values).get::<String>(0)).collect::<Result<Vec<_>, _>>()?;
        Ok((keys, more))
    }

    /// The receipt of commit `commit_seq`, when it is still in the log.
    ///
    /// # Errors
    ///
    /// As [`Self::receipts_after`].
    pub fn receipt_at(&self, commit_seq: u64) -> Result<Option<Receipt>, CellError> {
        if commit_seq == 0 {
            return Ok(None);
        }
        Ok(self
            .receipts_after(commit_seq.saturating_sub(1), 1)?
            .into_iter()
            .find(|receipt| receipt.body.commit_seq == commit_seq))
    }

    /// The stored hash column of receipt `commit_seq`, without decoding its
    /// body. Subscription cursors need only this digest to bind to the log.
    ///
    /// # Errors
    /// SQL engine errors or an out-of-range commit sequence.
    pub fn receipt_hash_at(&self, commit_seq: u64) -> Result<Option<String>, CellError> {
        if commit_seq == 0 {
            return Ok(None);
        }
        self.conn.query_optional(
            &format!("SELECT \"receipt_hash\" FROM \"{RECEIPT}\" WHERE \"commit_seq\" = ?1"),
            params![to_i64(commit_seq, "commit_seq")?],
            |row| row.get(0),
        )
    }

    /// Recompute the ledger hash chain and every receipt hash, and check that
    /// the receipts tile the ledger (contiguous spans from the first event,
    /// every event in its receipt's commit and world, no orphan events) and
    /// that each receipt's head is the chain's hash at its last event.
    ///
    /// # Errors
    ///
    /// [`CellError::LedgerCorrupt`] at the first mismatch.
    pub fn verify_ledger(&self) -> Result<LedgerHead, CellError> {
        self.verify_ledger_from(&LedgerHead::genesis())
    }

    /// [`Self::verify_ledger`] for the suffix after `checkpoint`, a head an
    /// earlier verification of this file returned (#2975): reads and checks
    /// only the events and receipts after it, so a caller that keeps the
    /// last verified head pays for new commits, not for the whole ledger.
    ///
    /// The prefix through the checkpoint is not re-verified. What is checked
    /// is that the checkpoint still anchors the file: the event at its
    /// sequence number still has its hash, and its last receipt still ends
    /// at that event with that head. Starting from [`LedgerHead::genesis`] is
    /// a full verification.
    ///
    /// # Errors
    ///
    /// [`CellError::LedgerCorrupt`] when the checkpoint no longer anchors
    /// the file or at the first mismatch after it.
    pub fn verify_ledger_from(&self, checkpoint: &LedgerHead) -> Result<LedgerHead, CellError> {
        self.verify_ledger_with_boundary(checkpoint)
            .map(|(head, _)| head)
    }

    #[allow(
        clippy::too_many_lines,
        reason = "one pass: the anchor, the hash chain, receipt tiling and quantity boundary"
    )]
    pub(crate) fn verify_ledger_with_boundary(
        &self,
        checkpoint: &LedgerHead,
    ) -> Result<(LedgerHead, Option<crate::quantity::FormatBoundary>), CellError> {
        let base = checkpoint.seq;
        let anchor_corrupt = || CellError::LedgerCorrupt {
            seq: base,
            reason: "the verified checkpoint no longer anchors the ledger".to_owned(),
        };
        // The anchor event (none for an empty prefix), then the suffix.
        let mut entries = self.ledger(base)?;
        if base == 0 {
            // Commits that appended no events leave the genesis hash; their
            // last receipt is checked as the anchor below.
            if checkpoint.hash != GENESIS_HASH {
                return Err(anchor_corrupt());
            }
        } else {
            match entries.first() {
                Some(anchor) if anchor.seq == base && anchor.hash == checkpoint.hash => {}
                _ => return Err(anchor_corrupt()),
            }
            entries.remove(0);
        }
        let mut heads: Vec<String> = Vec::with_capacity(entries.len());
        let mut prev = checkpoint.hash.clone();
        let mut expected_seq = base;
        for entry in &entries {
            expected_seq = next(expected_seq, "seq")?;
            let corrupt = |reason: &str| CellError::LedgerCorrupt {
                seq: entry.seq,
                reason: reason.to_owned(),
            };
            if entry.seq != expected_seq {
                return Err(corrupt("sequence gap"));
            }
            if entry.prev_hash != prev {
                return Err(corrupt("previous hash does not chain"));
            }
            let hash = entry_hash(
                entry.seq,
                entry.commit_seq,
                &entry.world,
                &entry.event,
                &prev,
            )?;
            if hash != entry.hash {
                return Err(corrupt("entry hash does not verify"));
            }
            prev = hash;
            heads.push(prev.clone());
        }
        // The suffix index of event `seq` (`seq > base`).
        let suffix_index = |seq: u64| {
            seq.checked_sub(base)
                .and_then(|offset| offset.checked_sub(1))
                .and_then(|index| usize::try_from(index).ok())
        };
        // The anchor receipt (none for an empty prefix), then the suffix.
        // Receipts must tile the ledger: contiguous spans from the event
        // after the checkpoint, each event belonging to its receipt's commit
        // and world, no orphans.
        let mut receipts = self.receipts_from(checkpoint.commits)?;
        if checkpoint.commits > 0 {
            match receipts.first() {
                Some(anchor)
                    if anchor.body.commit_seq == checkpoint.commits
                        && anchor.body.ledger_head == checkpoint.hash
                        && anchor
                            .body
                            .first_event_seq
                            .checked_add(anchor.body.event_count)
                            == base.checked_add(1)
                        && hash_json(&anchor.body)? == anchor.hash => {}
                _ => return Err(anchor_corrupt()),
            }
            receipts.remove(0);
        } else if base != 0 {
            return Err(anchor_corrupt());
        }
        let mut expected_commit = checkpoint.commits;
        let mut next_event = next(base, "seq")?;
        for receipt in &receipts {
            expected_commit = next(expected_commit, "commit_seq")?;
            let corrupt = |reason: &str| CellError::LedgerCorrupt {
                seq: receipt.body.commit_seq,
                reason: reason.to_owned(),
            };
            if receipt.body.commit_seq != expected_commit {
                return Err(corrupt("commit sequence gap"));
            }
            if receipt.body.quantity.is_some() != receipt.body.quantity_result.is_some()
                || (receipt.body.quantity_intent.is_some() && receipt.body.quantity.is_none())
            {
                return Err(corrupt("quantity receipt provenance is incomplete"));
            }
            if hash_json(&receipt.body)? != receipt.hash {
                return Err(corrupt("receipt hash does not verify"));
            }
            if receipt.body.first_event_seq != next_event {
                return Err(corrupt("receipt span does not continue the ledger"));
            }
            let end = next_event
                .checked_add(receipt.body.event_count)
                .ok_or_else(|| corrupt("event span out of range"))?;
            for seq in next_event..end {
                let entry = suffix_index(seq)
                    .and_then(|index| entries.get(index))
                    .ok_or_else(|| corrupt("receipt names events past the ledger head"))?;
                if entry.commit_seq != receipt.body.commit_seq || entry.world != receipt.body.world
                {
                    return Err(corrupt("an event does not belong to its receipt"));
                }
            }
            // The head after the commit: the hash at its last event, or the
            // head before it when the commit appended nothing.
            let last = end
                .checked_sub(1)
                .ok_or_else(|| corrupt("event span out of range"))?;
            let head = if last == base {
                Some(checkpoint.hash.as_str())
            } else {
                suffix_index(last)
                    .and_then(|index| heads.get(index))
                    .map(String::as_str)
            }
            .ok_or_else(|| corrupt("receipt names events past the ledger head"))?;
            if head != receipt.body.ledger_head {
                return Err(corrupt("receipt head does not match the ledger"));
            }
            next_event = end;
        }
        if next_event != next(expected_seq, "seq")? {
            return Err(CellError::LedgerCorrupt {
                seq: next_event,
                reason: "ledger events without a receipt".to_owned(),
            });
        }
        let mut boundary = self
            .quantity_boundary
            .filter(|boundary| boundary.seq <= base);
        // A caller may supply a verified checkpoint obtained through another
        // handle, before this handle has cached that handle's upgrade.
        if base > 0 && self.quantity_boundary.is_none() {
            boundary = crate::quantity::format_boundary(self.conn)?
                .filter(|boundary| boundary.seq <= base);
        }
        let typed_receipts = receipts
            .iter()
            .filter(|receipt| receipt.body.quantity.is_some())
            .map(|receipt| (receipt.body.commit_seq, &receipt.body))
            .collect::<std::collections::BTreeMap<_, _>>();
        for entry in &entries {
            let Some(receipt) = typed_receipts.get(&entry.commit_seq) else {
                continue;
            };
            if let Some(marker) = crate::quantity::boundary_marker(entry, receipt)?
                && boundary.replace(marker).is_some()
            {
                return Err(norn_kernel::quantity::QuantityError::Corrupt.into());
            }
        }
        crate::quantity::validate_boundary(self.conn, boundary)?;
        for receipt in typed_receipts.values() {
            let last_seq = receipt
                .first_event_seq
                .checked_add(receipt.event_count)
                .and_then(|end| end.checked_sub(1))
                .ok_or(norn_kernel::quantity::QuantityError::Corrupt)?;
            let last = suffix_index(last_seq)
                .and_then(|index| entries.get(index))
                .ok_or(norn_kernel::quantity::QuantityError::Corrupt)?;
            let request = receipt
                .quantity
                .as_ref()
                .ok_or(norn_kernel::quantity::QuantityError::Corrupt)?;
            if receipt.event_count == 0
                || boundary.is_none_or(|boundary| receipt.first_event_seq < boundary.seq)
                || last.event != crate::quantity::transition_event(request)?
            {
                return Err(norn_kernel::quantity::QuantityError::Corrupt.into());
            }
            for seq in receipt.first_event_seq..last_seq {
                if suffix_index(seq)
                    .and_then(|index| entries.get(index))
                    .and_then(|entry| entry.event.get("type"))
                    .and_then(Value::as_str)
                    == Some("quantity.transition")
                {
                    return Err(norn_kernel::quantity::QuantityError::Corrupt.into());
                }
            }
            if base > 0 {
                crate::quantity::verify_transition_projection(
                    self.conn,
                    &receipt.world,
                    receipt.commit_seq,
                    request,
                )?;
            }
        }
        if base == 0 {
            crate::quantity::verify_projections(self.conn, &receipts)?;
        }
        Ok((
            LedgerHead {
                seq: expected_seq,
                hash: prev,
                commits: expected_commit,
            },
            boundary,
        ))
    }

    /// Pending outbox commands, in enqueue order.
    ///
    /// # Errors
    ///
    /// SQL engine or decode errors.
    pub fn pending_commands(&self) -> Result<Vec<PendingCommand>, CellError> {
        self.outbox_rows("")
    }

    /// The oldest pending command (the next to `perform`), if any.
    ///
    /// # Errors
    ///
    /// SQL engine or decode errors.
    pub fn head_command(&self) -> Result<Option<PendingCommand>, CellError> {
        Ok(self.outbox_rows(" LIMIT 1")?.into_iter().next())
    }

    /// Whether `command_id` and `generation` have a durable perform claim.
    ///
    /// # Errors
    ///
    /// SQLite errors.
    pub fn perform_claimed(&self, command_id: &str, generation: u64) -> Result<bool, CellError> {
        let found: Option<i64> = self
            .conn
            .query_optional(
                &format!(
                    "SELECT 1 FROM \"{PERFORM_CLAIM}\" WHERE \"command_id\" = ?1 AND \"generation\" = ?2"
                ),
                params![command_id, to_i64(generation, "generation")?],
                |row| row.get::<i64>(0),
            )?;
        Ok(found.is_some())
    }

    fn outbox_rows(&self, limit: &str) -> Result<Vec<PendingCommand>, CellError> {
        let rows = self.conn.query(
            &format!(
                "SELECT \"position\", \"commit_seq\", \"command_id\", \"generation\", \"idempotency_key\", \"deadline\", \"command\", \"result\" FROM \"{OUTBOX}\" WHERE \"consumed_commit_seq\" IS NULL ORDER BY \"position\"{limit}"
            ),
            params![],
        )?;
        let mut out = Vec::new();
        for values in rows {
            let row = SqlRow(values);
            let deadline: Option<i64> = row.get(5)?;
            let command: String = row.get(6)?;
            let result: Option<String> = row.get(7)?;
            out.push(PendingCommand {
                position: to_u64(row.get(0)?, "position")?,
                commit_seq: to_u64(row.get(1)?, "commit_seq")?,
                command: OutboxCommand {
                    command_id: row.get(2)?,
                    generation: to_u64(row.get(3)?, "generation")?,
                    idempotency_key: row.get(4)?,
                    deadline: deadline
                        .map(|value| to_u64(value, "deadline"))
                        .transpose()?,
                    command: serde_json::from_str(&command)?,
                },
                result: result
                    .map(|result| serde_json::from_str(&result))
                    .transpose()?,
            });
        }
        Ok(out)
    }

    /// Enumerate all pending timers, earliest due first (ties by id).
    /// Runtime delivery and alarm rearming use bounded selectors instead.
    ///
    /// # Errors
    ///
    /// SQL engine or decode errors.
    pub fn pending_timers(&self) -> Result<Vec<PendingTimer>, CellError> {
        let rows = self.conn.query(
            &format!(
                "SELECT \"commit_seq\", \"timer_id\", \"due\", \"timer\" FROM \"{TIMER}\" ORDER BY \"due\", \"timer_id\""
            ),
            params![],
        )?;
        let mut out = Vec::new();
        for values in rows {
            let row = SqlRow(values);
            let timer: String = row.get(3)?;
            out.push(PendingTimer {
                commit_seq: to_u64(row.get(0)?, "commit_seq")?,
                timer: TimerEntry {
                    timer_id: row.get(1)?,
                    due: to_u64(row.get(2)?, "due")?,
                    timer: serde_json::from_str(&timer)?,
                },
            });
        }
        Ok(out)
    }

    /// One pending timer by its unique primary key, without enumerating others.
    ///
    /// # Errors
    /// SQL engine or decode errors.
    pub fn pending_timer(&self, timer_id: &str) -> Result<Option<PendingTimer>, CellError> {
        self.conn.query_optional(
            r#"SELECT "commit_seq","timer_id","due","timer" FROM "_norn_timer" WHERE "timer_id"=?1 LIMIT 1"#,
            params![timer_id],
            |row| Ok(PendingTimer {
                commit_seq: to_u64(row.get(0)?, "timer incarnation")?,
                timer: TimerEntry { timer_id: row.get(1)?, due: to_u64(row.get(2)?, "timer due")?, timer: serde_json::from_str(&row.get::<String>(3)?)? },
            }),
        )
    }

    /// Whether a known timer is pending, using its unique primary key.
    ///
    /// # Errors
    /// SQL engine or decode errors.
    pub fn has_pending_timer(&self, timer_id: &str) -> Result<bool, CellError> {
        Ok(self
            .conn
            .query_optional(
                r#"SELECT 1 FROM "_norn_timer" WHERE "timer_id"=?1 LIMIT 1"#,
                params![timer_id],
                |row| row.get::<i64>(0),
            )?
            .is_some())
    }

    /// Earliest deadline across ordinary and quantity timers. This covering
    /// index lookup reads at most one row and never decodes a timer payload.
    ///
    /// # Errors
    /// SQL engine or decode errors.
    pub fn next_timer_due(&self) -> Result<Option<u64>, CellError> {
        self.conn.query_optional(
            r#"SELECT "due" FROM "_norn_timer" INDEXED BY "_norn_timer/due" ORDER BY "due","timer_id" LIMIT 1"#,
            params![],
            |row| to_u64(row.get(0)?, "timer due"),
        )
    }

    /// The earliest ordinary timer due at `now` (ties by id), if any.
    /// The partial index excludes host-owned quantity timers before seeking,
    /// so even an overdue quantity backlog cannot amplify this read.
    ///
    /// # Errors
    ///
    /// SQL engine or decode errors.
    pub fn due_timer(&self, now: u64) -> Result<Option<PendingTimer>, CellError> {
        self.conn.query_optional(
            r#"SELECT "commit_seq","timer_id","due","timer" FROM "_norn_timer" INDEXED BY "_norn_timer/ordinary_due" WHERE ("timer_id"<'norn.quantity.expiry/' OR "timer_id">='norn.quantity.expiry0') AND "due"<=?1 ORDER BY "due","timer_id" LIMIT 1"#,
            params![to_i64(now, "timer time")?],
            |row| Ok(PendingTimer {
                commit_seq: to_u64(row.get(0)?, "timer incarnation")?,
                timer: TimerEntry { timer_id: row.get(1)?, due: to_u64(row.get(2)?, "timer due")?, timer: serde_json::from_str(&row.get::<String>(3)?)? },
            }),
        )
    }

    /// Earliest due host-owned quantity deadline, using the timer due index.
    ///
    /// # Errors
    /// Storage or decoding errors.
    pub fn due_quantity_timer(&self, now: u64) -> Result<Option<PendingTimer>, CellError> {
        self.conn.query_optional(
            r#"SELECT "commit_seq","timer_id","due","timer" FROM "_norn_timer" INDEXED BY "_norn_timer/quantity_due" WHERE "timer_id" >= 'norn.quantity.expiry/' AND "timer_id" < 'norn.quantity.expiry0' AND "due"<=?1 ORDER BY "due","timer_id" LIMIT 1"#,
            params![to_i64(now, "quantity expiry time")?], |row| Ok(PendingTimer {
                commit_seq: to_u64(row.get(0)?, "timer incarnation")?,
                timer: TimerEntry { timer_id: row.get(1)?, due: to_u64(row.get(2)?, "timer due")?, timer: serde_json::from_str(&row.get::<String>(3)?)? },
            }),
        )
    }

    /// Pending (unconsumed) observations, in ingestion order.
    ///
    /// # Errors
    ///
    /// SQL engine or decode errors.
    pub fn pending_observations(&self) -> Result<Vec<PendingObservation>, CellError> {
        let rows = self.conn.query(
            &format!(
                "SELECT \"position\", \"observation_id\", \"observed\" FROM \"{INBOX}\" WHERE \"consumed_commit_seq\" IS NULL ORDER BY \"position\""
            ),
            params![],
        )?;
        let mut out = Vec::new();
        for values in rows {
            let row = SqlRow(values);
            let observed: String = row.get(2)?;
            out.push(PendingObservation {
                position: to_u64(row.get(0)?, "position")?,
                observation_id: row.get(1)?,
                observed: serde_json::from_str(&observed)?,
            });
        }
        Ok(out)
    }

    /// The runs of `command_id`, in order.
    ///
    /// # Errors
    ///
    /// SQL engine or decode errors.
    pub fn runs(&self, command_id: &str) -> Result<Vec<RunRecord>, CellError> {
        let rows = self.conn.query(
            &format!(
                "SELECT \"generation\", \"attempt\", \"outcome\", \"consumed_commit_seq\" FROM \"{RUN}\" WHERE \"command_id\" = ?1 ORDER BY \"generation\", \"attempt\""
            ),
            params![command_id],
        )?;
        let mut out = Vec::new();
        for values in rows {
            let row = SqlRow(values);
            let outcome: String = row.get(2)?;
            let consumed: Option<i64> = row.get(3)?;
            out.push(RunRecord {
                command_id: command_id.to_owned(),
                generation: to_u64(row.get(0)?, "generation")?,
                attempt: to_u64(row.get(1)?, "attempt")?,
                outcome: serde_json::from_str(&outcome)?,
                consumed_commit_seq: consumed
                    .map(|value| to_u64(value, "commit_seq"))
                    .transpose()?,
            });
        }
        Ok(out)
    }

    /// How many times any command was executed (`perform` runs).
    ///
    /// # Errors
    ///
    /// SQL engine errors.
    pub fn executions(&self) -> Result<u64, CellError> {
        let count: i64 = self.conn.query_row(
            &format!("SELECT COUNT(*) FROM \"{RUN}\""),
            params![],
            |row| row.get(0),
        )?;
        to_u64(count, "executions")
    }

    /// Whether `consumption` names a pending delivery: a timer with that id
    /// and incarnation; the outbox head with that id and generation and a
    /// recorded result; an unconsumed observation with that id.
    ///
    /// # Errors
    ///
    /// SQL engine errors.
    pub fn is_pending(&self, consumption: &Consumption) -> Result<bool, CellError> {
        match consumption {
            Consumption::Timer {
                timer_id,
                incarnation,
            } => {
                let found: Option<i64> = self.conn.query_optional(
                    &format!(
                        "SELECT 1 FROM \"{TIMER}\" WHERE \"timer_id\" = ?1 AND \"commit_seq\" = ?2 LIMIT 1"
                    ),
                    params![timer_id, to_i64(*incarnation, "incarnation")?],
                    |row| row.get(0),
                )?;
                Ok(found.is_some())
            }
            Consumption::Result {
                command_id,
                generation,
            } => Ok(self.head_command()?.is_some_and(|head| {
                head.command.command_id == *command_id
                    && head.command.generation == *generation
                    && head.result.is_some()
            })),
            Consumption::Observation { observation_id } => {
                let found: Option<i64> = self.conn.query_optional(
                    &format!(
                        "SELECT 1 FROM \"{INBOX}\" WHERE \"observation_id\" = ?1 AND \"consumed_commit_seq\" IS NULL"
                    ),
                    params![observation_id],
                    |row| row.get(0),
                )?;
                Ok(found.is_some())
            }
        }
    }

    /// A deterministic dump of every table: table name → its rows rendered as
    /// JSON arrays, sorted. Two stores with equal dumps hold the same state.
    ///
    /// # Errors
    ///
    /// SQL engine errors.
    pub fn dump(&self) -> Result<BTreeMap<String, Vec<String>>, CellError> {
        self.dump_filtered(|_| true)
    }

    /// A deterministic dump of selected tables, using the same row encoding
    /// as [`Self::dump`]. Hosts can exclude private infrastructure tables before
    /// reading them, including tables whose contents the host forbids accessing.
    ///
    /// # Errors
    ///
    /// SQL engine errors for included tables or schema enumeration.
    pub fn dump_filtered(
        &self,
        include: impl Fn(&str) -> bool,
    ) -> Result<BTreeMap<String, Vec<String>>, CellError> {
        let names: Vec<String> = self
            .conn
            .query(
                "SELECT \"name\" FROM \"sqlite_master\" WHERE \"type\" = 'table' ORDER BY \"name\"",
                params![],
            )?
            .into_iter()
            .map(|values| SqlRow(values).get(0))
            .collect::<Result<_, _>>()?;
        let mut out = BTreeMap::new();
        for name in names.into_iter().filter(|name| include(name)) {
            let projection = dump_projection(self.model, &name);
            let integer_columns: Vec<bool> = projection
                .as_ref()
                .map(|projection| {
                    projection
                        .columns
                        .iter()
                        .map(|(_, is_integer)| *is_integer)
                        .collect()
                })
                .unwrap_or_default();
            let rows = if let Some(projection) = projection {
                let columns: Vec<(&str, bool)> = projection
                    .columns
                    .iter()
                    .map(|(column, is_integer)| (column.as_str(), *is_integer))
                    .collect();
                let suffix = format!("FROM {} {}", quote_identifier(&name), projection.order_by);
                query_projected(self.conn, &name, &columns, &suffix, &[])?
            } else {
                self.conn.query(
                    &format!("SELECT * FROM {}", quote_identifier(&name)),
                    params![],
                )?
            };
            let mut rendered = Vec::new();
            for values in rows {
                let row = SqlRow(values);
                let mut cells = Vec::with_capacity(row.0.len());
                for (index, value) in row.0.iter().enumerate() {
                    let is_integer = integer_columns.get(index).copied().unwrap_or(false);
                    cells.push(match value {
                        SqlValue::Null => serde_json::Value::Null,
                        SqlValue::Integer(number) => serde_json::Value::from(*number),
                        SqlValue::Real(number) => serde_json::Value::from(*number),
                        SqlValue::Text(text) if is_integer => text.parse::<i64>().map_or_else(
                            |_invalid| serde_json::Value::String(text.clone()),
                            serde_json::Value::from,
                        ),
                        SqlValue::Text(text) => serde_json::Value::String(text.clone()),
                        SqlValue::Blob(bytes) => serde_json::Value::from(bytes.clone()),
                    });
                }
                rendered.push(serde_json::Value::Array(cells).to_string());
            }
            rendered.sort();
            out.insert(name, rendered);
        }
        Ok(out)
    }
}

#[cfg(all(test, feature = "native"))]
#[allow(clippy::expect_used, reason = "SQLite index plan regression tests")]
mod link_candidate_index_tests {
    use super::{LinkDirection, LinkPagePosition, link_candidate_page_sql};
    use crate::engine::{NativeEngine, SqlEngine, SqlValue};
    use crate::system::SYSTEM_DDL;
    use std::cmp::Ordering;

    #[allow(
        clippy::too_many_arguments,
        reason = "each input selects one SQL index plan dimension"
    )]
    fn assert_covering_plan(
        engine: &NativeEngine,
        id: &str,
        branch_direction: LinkDirection,
        link_type: Option<&str>,
        after: Option<&LinkPagePosition>,
        expected_index: &str,
        locator_anchor_order: Option<Ordering>,
        expected_ranges: &[&str],
    ) {
        let pages = link_candidate_page_sql(
            "main",
            id,
            branch_direction,
            link_type,
            after,
            8,
            locator_anchor_order,
        )
        .expect("builds candidate query");
        assert_eq!(pages.len(), expected_ranges.len().max(1));
        for (index, (sql, bindings)) in pages.iter().enumerate() {
            let plan = engine
                .query(&format!("EXPLAIN QUERY PLAN {sql}"), bindings)
                .expect("explains candidate query");
            let details = plan
                .iter()
                .filter_map(|row| match row.get(3) {
                    Some(SqlValue::Text(detail)) => Some(detail.as_str()),
                    _ => None,
                })
                .collect::<Vec<_>>();
            assert!(
                details
                    .iter()
                    .any(|detail| detail.contains("USING COVERING INDEX")
                        && detail.contains(expected_index)),
                "expected covering index {expected_index}; plan: {details:?}; SQL: {sql}"
            );
            assert!(
                details
                    .iter()
                    .all(|detail| !detail.contains("USE TEMP B-TREE FOR ORDER BY")),
                "candidate query sorts outside the ordered index: {details:?}; SQL: {sql}"
            );
            if let Some(expected_range) = expected_ranges.get(index) {
                assert!(
                    details.iter().any(|detail| detail.contains(expected_range)),
                    "expected indexed cursor range {expected_range}; plan: {details:?}; SQL: {sql}"
                );
            }
        }
    }

    #[test]
    fn directional_candidate_pages_seek_covering_indexes_with_and_without_link_type() {
        let engine = NativeEngine::open_in_memory().expect("opens SQLite");
        for statement in SYSTEM_DDL {
            engine
                .execute(statement, &[])
                .expect("creates system schema");
        }
        let position = LinkPagePosition {
            link_table: "_link/test.Node/related".to_owned(),
            source: "anchor".to_owned(),
            target: "anchor".to_owned(),
            row_locator: None,
        };
        let both_after_outgoing = LinkPagePosition {
            source: "anchor".to_owned(),
            target: "target".to_owned(),
            ..position.clone()
        };
        let both_after_incoming = LinkPagePosition {
            source: "source".to_owned(),
            target: "anchor".to_owned(),
            ..position.clone()
        };

        assert_covering_plan(
            &engine,
            "anchor",
            LinkDirection::Out,
            None,
            None,
            "_norn_link_version/source_order",
            None,
            &[],
        );
        assert_covering_plan(
            &engine,
            "anchor",
            LinkDirection::In,
            None,
            Some(&position),
            "_norn_link_version/target_order",
            None,
            &["(link_table,source)>(?,?)"],
        );
        assert_covering_plan(
            &engine,
            "anchor",
            LinkDirection::Out,
            Some("related"),
            Some(&both_after_outgoing),
            "_norn_link_version/source_link_order",
            None,
            &["(link_table,target)>(?,?)"],
        );
        assert_covering_plan(
            &engine,
            "anchor",
            LinkDirection::In,
            Some("related"),
            Some(&both_after_incoming),
            "_norn_link_version/target_link_order",
            None,
            &["(link_table,source)>(?,?)"],
        );
    }

    #[test]
    fn both_locator_branch_ranges_follow_global_tuple_order() {
        let engine = NativeEngine::open_in_memory().expect("opens SQLite");
        for statement in SYSTEM_DDL {
            engine
                .execute(statement, &[])
                .expect("creates system schema");
        }
        engine
            .execute(
                "INSERT INTO \"_norn_link_version\" (\"world\", \"link_table\", \"source\", \"target\", \"link\", \"source_type\", \"target_type\", \"version\") VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                &[
                    SqlValue::from("main"),
                    SqlValue::from("links"),
                    SqlValue::from("a"),
                    SqlValue::from("z"),
                    SqlValue::from("related"),
                    SqlValue::from("test.Node"),
                    SqlValue::from("test.Node"),
                    SqlValue::Integer(1),
                ],
            )
            .expect("inserts locator candidate");
        let locator = LinkPagePosition {
            link_table: String::new(),
            source: String::new(),
            target: String::new(),
            row_locator: Some(1),
        };

        for (branch, endpoint, expected) in [
            (LinkDirection::Out, "source", Ordering::Greater),
            (LinkDirection::In, "target", Ordering::Less),
        ] {
            let order = super::link_locator_endpoint_order(&engine, "main", "m", 1, endpoint)
                .expect("compares endpoint to anchor");
            assert_eq!(order, expected);
            assert_covering_plan(
                &engine,
                "m",
                branch,
                Some("related"),
                Some(&locator),
                if branch == LinkDirection::Out {
                    "_norn_link_version/source_link_order"
                } else {
                    "_norn_link_version/target_link_order"
                },
                Some(expected),
                if branch == LinkDirection::Out {
                    &["link_table>?"]
                } else {
                    &["link_table>?", "link_table=? AND source>?"]
                },
            );
        }
        assert!(super::link_locator_endpoint_order(&engine, "other", "m", 1, "source").is_err());
    }
}
