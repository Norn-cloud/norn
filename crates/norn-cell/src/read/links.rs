//! Point link reads, bounded neighborhood pagination, and creation commit lookups.

use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;

use norn_ir::RuntimeValue as Value;
use norn_projections::ValueEncoding;
use norn_projections::sqlite_ddl::{
    OVERFLOW_COLUMN, SOURCE_COLUMN, TARGET_COLUMN, WORLD_COLUMN, quote_identifier,
};
use serde::{Deserialize, Serialize};

use super::{StoreReader, decode_receipts, decode_row, query_projected, to_i64, to_u64};
use crate::codec::{DecodeContext, ValueOwner};
use crate::engine::{MAX_PARAMETERS, MAX_SAFE_INTEGER, SqlEngine, SqlRow, SqlValue, params};
use crate::error::CellError;
use crate::reactive::DecodeBudget;
use crate::store::Receipt;
use crate::system::{LINK_CREATION, LINK_VERSIONS, PENDING_LINK_INDEX, RECEIPT};

const MAX_LINK_PAGE_CANDIDATES: usize = 1_000;
const MAX_LINK_PAGE_BYTES: u64 = 16 * 1024 * 1024;
const LINK_CANDIDATE_ROW_OVERHEAD: u64 = 64;
const MAX_LINK_RECEIPT_LOOKUP_ROWS: usize = 4_096;
const MAX_LINK_RECEIPT_LOOKUP_BYTES: u64 = 8 * 1024 * 1024;
const LINK_RECEIPT_LOOKUP_BATCH: usize = 64;

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
pub(super) fn link_candidate_page_sql(
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

pub(super) fn link_locator_endpoint_order<E: SqlEngine>(
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

impl<E: SqlEngine> StoreReader<'_, E> {
    /// The links `link` from `source` (of `object_type`) in `world`, by target id.
    /// # Errors
    /// Unknown type or link; SQL engine or decode errors.
    pub fn links(
        &self,
        world: &str,
        object_type: &str,
        link: &str,
        source: &str,
    ) -> Result<Vec<LinkRecord>, CellError> {
        self.check_live()?;
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
    /// `object_type`) in `world`, by target id, bounded by `max_bytes` (#3079).
    /// # Errors
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
        self.check_live()?;
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
        self.check_live()?;
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
        self.check_live()?;
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
        names.extend(
            self.model
                .interface_aliases(&concrete)
                .into_iter()
                .map(str::to_owned),
        );

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
        self.check_live()?;
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
        names.extend(
            self.model
                .interface_aliases(&concrete)
                .into_iter()
                .map(str::to_owned),
        );
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

    /// Indexed pending-link record identities for one canonical source output.
    /// Row and byte budgets apply before fetching record keys.
    /// # Errors
    /// SQL engine or out-of-range length metadata.
    pub fn pending_link_records_from_with_budget(
        &self,
        world: &str,
        source_type: &str,
        source_key: &str,
        budget: &mut DecodeBudget,
    ) -> Result<Option<Vec<String>>, CellError> {
        self.check_live()?;
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

    /// Bounded prefix of intents indexed by canonical target identity.
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
        self.check_live()?;
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
}
