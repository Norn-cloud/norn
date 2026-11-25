//! Committed-state reads of a cell store, shared by the public read API and
//! by the commit path (the guard and the writer read through the same
//! functions inside the commit transaction).

use std::collections::BTreeMap;

use norn_ir::RuntimeValue as Value;
use norn_projections::sqlite_ddl::{
    ID_COLUMN, OVERFLOW_COLUMN, SCOPE_COLUMN, SOURCE_COLUMN, TARGET_COLUMN, TYPE_COLUMN,
    VERSION_COLUMN, WORLD_COLUMN, quote_identifier,
};
use norn_projections::{PropertyColumn, ValueEncoding};
use serde::Serialize;

use crate::codec::{DecodeContext, canonicalize_stored_value, decode_stored_value, hash_json};
use crate::engine::{
    DefaultEngine, MAX_SAFE_INTEGER, MAX_STATEMENT_BYTES, SqlEngine, SqlRow, SqlValue, params,
};
use crate::error::CellError;
use crate::model::Model;
use crate::store::{Receipt, ReceiptBody};
use crate::system::RECEIPT;

mod definitions;
mod findings;
mod ledger;
mod links;
mod objects;
mod pagination;
mod queue;

pub(crate) use definitions::{
    active_definition_version, definition_by_version, validate_active_definition_pointer,
};
pub use findings::FindingLedgerPage;
pub use ledger::{LedgerEntry, LedgerHead};
pub use links::{
    LinkDirection, LinkEdgeRecord, LinkPagePosition, LinkReadPage, LinkRecord, LinkRef,
};
pub use objects::{MarkedQuestionPath, ObjectRecord};
pub use pagination::{
    BindingRootCandidate, BindingSortValue, BindingSortedCandidate, BindingSortedPosition,
};
pub use queue::{
    ParkedPage, PendingCommand, PendingObservation, PendingTimer, RunRecord, is_parked,
};

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
        let hash: String = row.get(5)?;
        // A historical read serves only a receipt whose body still hashes to
        // its stored digest, the leaf the transparency root is built from
        // (OPEN-CHECKPOINT-1: an open from a checkpoint never re-reads it).
        if hash_json(&body)? != hash {
            return Err(CellError::LedgerCorrupt {
                seq: commit_seq,
                reason: "receipt hash does not verify".to_owned(),
            });
        }
        out.push(Receipt { body, hash });
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

pub(crate) fn query_projected(
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
    // The same columns `receipt_at` reads, decoded the same way (S4, #1551
    // round 2): the hashed body's own world and key must be the ones looked
    // up, so a changed lookup column never returns another world's receipt.
    let rows = conn.query(
        &format!(
            "SELECT \"commit_seq\", \"world\", \"idempotency_key\", \"request_hash\", \"receipt\", \"receipt_hash\" FROM \"{RECEIPT}\" WHERE \"world\" = ?1 AND \"idempotency_key\" = ?2 LIMIT 2"
        ),
        params![world, key],
    )?;
    let mut receipts = decode_receipts(rows)?.into_iter();
    let receipt = receipts.next();
    if receipts.next().is_some()
        || receipt.as_ref().is_some_and(|receipt| {
            receipt.body.world != world || receipt.body.idempotency_key.as_deref() != Some(key)
        })
    {
        return Err(CellError::LedgerCorrupt {
            seq: receipt.map_or(0, |receipt| receipt.body.commit_seq),
            reason: "receipt columns disagree with the hashed receipt".to_owned(),
        });
    }
    Ok(receipt)
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

pub(crate) type PendingDigests = BTreeMap<(String, String, u64), String>;

/// Committed-state reads over one engine, including its active commit
/// transaction when called during a commit.
#[derive(Debug, Clone, Copy)]
pub struct StoreReader<'a, E: SqlEngine = DefaultEngine> {
    pub(crate) conn: &'a E,
    model: &'a Model,
    quantity_boundary: Option<crate::quantity::FormatBoundary>,
    pub(crate) pending_digests: Option<(u64, &'a PendingDigests)>,
    pub(crate) source_disputes_transaction: bool,
    pub(crate) source_disputes:
        Option<&'a std::cell::RefCell<crate::mapping_conflict::SourceDisputeIndex>>,
    pub(crate) mapping_plans:
        Option<&'a std::cell::RefCell<crate::mapping_bundle::MappingPlanCache>>,
    pub(crate) verified_receipts: Option<&'a std::cell::RefCell<crate::history::VerifiedReceipts>>,
    /// The receipt cache belongs to one read-only pass inside a transaction:
    /// nothing writes while it lives, so it holds without a read revision.
    pub(crate) verified_receipts_transaction: bool,
    /// OPEN-CHECKPOINT-1 (#1551 round 4): which history reads wait for the
    /// cell's scrub. Open on every reader but the store's served one.
    pub(crate) history_gate: crate::scrub::Gate,
    /// S12 (#1551 round 7): live scrub reference so even an already-acquired
    /// served reader observes local and sibling durable fences via `data_version`.
    pub(crate) live_scrub: Option<&'a std::cell::RefCell<crate::scrub::Scrub>>,
}

impl<'a, E: SqlEngine> StoreReader<'a, E> {
    pub(crate) const fn new(conn: &'a E, model: &'a Model) -> Self {
        Self {
            conn,
            model,
            quantity_boundary: None,
            pending_digests: None,
            source_disputes: None,
            mapping_plans: None,
            source_disputes_transaction: false,
            verified_receipts: None,
            verified_receipts_transaction: false,
            history_gate: crate::scrub::Gate::OPEN,
            live_scrub: None,
        }
    }

    /// The same reader, refusing the history reads `gate` holds back.
    #[must_use]
    pub(crate) const fn with_history_gate(mut self, gate: crate::scrub::Gate) -> Self {
        self.history_gate = gate;
        self
    }

    /// Attach the store's live scrub state for dynamic fence checks (S12).
    #[must_use]
    pub(crate) const fn with_live_scrub(
        mut self,
        scrub: &'a std::cell::RefCell<crate::scrub::Scrub>,
    ) -> Self {
        self.live_scrub = Some(scrub);
        self
    }

    /// S12 (#1551 round 7): fail closed on a local or sibling durable fence,
    /// checking `_norn_meta` whenever `PRAGMA data_version` changed since the
    /// last clean check (zero extra SQL queries on the hot path).
    pub(crate) fn check_live(&self) -> Result<(), CellError> {
        self.history_gate.refuse_fenced()?;
        let Some(scrub) = self.live_scrub else {
            return Ok(());
        };
        scrub.borrow().refuse_fenced()?;
        if self.conn.read_isolation() != crate::engine::ReadIsolation::ExclusiveSynchronous {
            match self.conn.external_data_version() {
                Ok(Some(dv)) => {
                    if scrub.borrow().fence_checked_dv() != Some(dv) {
                        if let Some(finding) = crate::open_checkpoint::read_fence_open(self.conn)? {
                            scrub.borrow_mut().record_durable_fence(finding.clone());
                            return Err(CellError::Fenced(finding));
                        }
                        scrub.borrow_mut().set_fence_checked_dv(dv);
                    }
                }
                Ok(None) | Err(_) => {
                    if let Some(finding) = crate::open_checkpoint::read_fence_open(self.conn)? {
                        scrub.borrow_mut().record_durable_fence(finding.clone());
                        return Err(CellError::Fenced(finding));
                    }
                }
            }
        }
        Ok(())
    }

    /// S7/S8/S12: fail closed on a durable fence first, then refuse while
    /// covered history is still unverified.
    pub(crate) fn check_history(&self, what: &'static str) -> Result<(), CellError> {
        self.check_live()?;
        self.history_gate.history(what)
    }

    /// S8/S12: fail closed on a durable fence first, then gate ledger reads
    /// that reach covered history.
    pub(crate) fn check_ledger(&self, after: u64) -> Result<(), CellError> {
        self.check_live()?;
        self.history_gate.ledger(after)
    }

    /// S4/S12: fail closed on a durable fence first, then gate idempotency
    /// lookup misses while covered receipts remain unverified.
    pub(crate) fn check_receipt_miss(&self) -> Result<(), CellError> {
        self.check_live()?;
        self.history_gate.receipt_miss()
    }

    /// A receipt cache for one read-only pass inside a transaction.
    pub(crate) const fn with_transaction_verified_receipts(
        mut self,
        receipts: &'a std::cell::RefCell<crate::history::VerifiedReceipts>,
    ) -> Self {
        self.verified_receipts = Some(receipts);
        self.verified_receipts_transaction = true;
        self
    }

    pub(crate) const fn with_verified_receipts(
        mut self,
        receipts: &'a std::cell::RefCell<crate::history::VerifiedReceipts>,
    ) -> Self {
        self.verified_receipts = Some(receipts);
        self
    }

    /// Writer-owned preimages support internal event validation, never evidence export.
    pub(crate) const fn with_pending_digests(
        mut self,
        commit: u64,
        digests: &'a PendingDigests,
    ) -> Self {
        self.pending_digests = Some((commit, digests));
        self
    }

    pub(crate) const fn with_source_disputes(
        mut self,
        index: &'a std::cell::RefCell<crate::mapping_conflict::SourceDisputeIndex>,
    ) -> Self {
        self.source_disputes = Some(index);
        self
    }

    pub(crate) const fn with_mapping_plans(
        mut self,
        plans: &'a std::cell::RefCell<crate::mapping_bundle::MappingPlanCache>,
    ) -> Self {
        self.mapping_plans = Some(plans);
        self
    }

    pub(crate) const fn with_transaction_source_disputes(
        mut self,
        index: &'a std::cell::RefCell<crate::mapping_conflict::SourceDisputeIndex>,
    ) -> Self {
        self.source_disputes = Some(index);
        self.source_disputes_transaction = true;
        self
    }

    pub(crate) const fn with_quantity_boundary(
        mut self,
        boundary: Option<crate::quantity::FormatBoundary>,
    ) -> Self {
        self.quantity_boundary = boundary;
        self
    }

    /// The model the reader reads.
    #[must_use]
    pub const fn model(&self) -> &'a Model {
        self.model
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
        self.check_history("dump")?;
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
            let integer_columns: Vec<bool> = projection.as_ref().map_or_default(|projection| {
                projection
                    .columns
                    .iter()
                    .map(|(_, is_integer)| *is_integer)
                    .collect()
            });
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
    use super::links::{link_candidate_page_sql, link_locator_endpoint_order};
    use super::objects::marking_property_rows_sql;
    use super::{LinkDirection, LinkPagePosition};
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
    fn marking_property_rows_seek_subjects_and_current_rows_by_key() {
        let engine = NativeEngine::open_in_memory().expect("opens SQLite");
        for statement in SYSTEM_DDL {
            engine
                .execute(statement, &[])
                .expect("creates system schema");
        }
        engine
            .execute(
                r#"CREATE TABLE "o:test.Item" ("_world" TEXT NOT NULL, "_id" TEXT NOT NULL, "_type" TEXT NOT NULL, "_version" INTEGER NOT NULL, PRIMARY KEY ("_world", "_id"))"#,
                &[],
            )
            .expect("creates root table");
        let sql = marking_property_rows_sql("o:test.Item");
        let bindings = [
            SqlValue::Text("main".to_owned()),
            SqlValue::Text("test.Item".to_owned()),
            SqlValue::Text("name".to_owned()),
        ];
        let plan = engine
            .query(&format!("EXPLAIN QUERY PLAN {sql}"), &bindings)
            .expect("explains marking count");
        let details = plan
            .iter()
            .filter_map(|row| match row.get(3) {
                Some(SqlValue::Text(detail)) => Some(detail.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>();
        // A per-object correlated lookup re-scans every matching marking row
        // (about 800x more SQLite work at 1,000 objects).
        assert!(
            details.iter().all(|detail| !detail.contains("CORRELATED")),
            "no correlated subquery: {details:?}"
        );
        // Every table is reached by a keyed search, whichever join order SQLite picks,
        // so the work is linear in the driving rows.
        for table in ["o ", "ms ", "m "] {
            assert!(
                details
                    .iter()
                    .any(|detail| detail.starts_with(&format!("SEARCH {table}"))),
                "{table}is searched by key: {details:?}"
            );
        }
        assert!(
            details.iter().all(|detail| !detail.starts_with("SCAN")),
            "no full scan: {details:?}"
        );
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
            let order = link_locator_endpoint_order(&engine, "main", "m", 1, endpoint)
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
        assert!(link_locator_endpoint_order(&engine, "other", "m", 1, "source").is_err());
    }
}
