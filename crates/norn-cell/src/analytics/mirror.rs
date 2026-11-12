//! ANALYTICS-FRONTIER-1: the governed-fact delta the cell pushes to the
//! tenant's mirror. Every committed object version is read from the cell's
//! own value commitments (`_norn_fact_values`), checked against its property
//! and sent typed; the engine never reads the cell.

use norn_checker::entity_instance::EffectiveProperty;
use norn_checker::metric::compute::contribution::instant_key;
use norn_ir::RuntimeValue;

use super::lower::table_schema;
use super::{Column, ColumnKind, FrontierRequest, MirrorRow, SyncRequest, TableSchema, TenantId, Value};
use crate::engine::{SqlEngine, SqlRow, params};
use crate::read::{to_i64, to_u64};
use crate::system::FACT_VALUES;
use crate::{CellError, CellStore};

/// Committed versions after a frontier, in whole commits.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MirrorBatch {
    /// The sync to send.
    pub request: SyncRequest,
    /// The batch reaches the cell's verified head.
    pub complete: bool,
}

/// The stored value of one column, or `None` when it fails its property's
/// check (the row is then marked bad and any question reading it refuses).
fn stored(column: &Column, property: &EffectiveProperty, value: Option<&RuntimeValue>) -> Option<Value> {
    let value = value?;
    property.check_runtime_value(value).ok()?;
    Some(match (column.kind, value) {
        (_, RuntimeValue::Null) => Value::Null,
        (ColumnKind::Text, RuntimeValue::String(text)) => Value::Text(text.clone()),
        (ColumnKind::Integer, RuntimeValue::Number(number)) if number.is_integer() => {
            Value::Number(number.canonical())
        }
        (ColumnKind::Decimal { scale }, RuntimeValue::Number(number)) if number.scale() <= scale => {
            Value::Number(number.canonical())
        }
        (ColumnKind::Boolean, RuntimeValue::Bool(flag)) => Value::Bool(*flag),
        (ColumnKind::Timestamp | ColumnKind::Date, RuntimeValue::String(text)) => {
            let (secs, nanos) = instant_key(text, column.kind == ColumnKind::Date)?;
            Value::Instant {
                text: text.clone(),
                secs,
                nanos,
            }
        }
        (ColumnKind::Opaque, value) => Value::Json(String::from_utf8(value.canonical_json().ok()?).ok()?),
        _ => return None,
    })
}

impl<E: SqlEngine> CellStore<E> {
    /// The analytical table of every root type in the active model, with its
    /// stored properties.
    ///
    /// # Errors
    /// The model cannot be read.
    pub fn analytics_tables(&self) -> Result<Vec<TableSchema>, CellError> {
        let model = self.model();
        model
            .object_types()
            .filter(|def| model.chain(def.name.as_str()).is_ok_and(|chain| chain.len() == 1))
            .map(|def| {
                let contract = model.query_contract(def.name.as_str())?;
                table_schema(def.name.as_str(), &contract.properties)
            })
            .collect()
    }

    /// The frontier question for `world`, carrying the current schemas.
    ///
    /// # Errors
    /// The model cannot be read.
    pub fn analytics_frontier(&self, tenant: &TenantId, world: &str) -> Result<FrontierRequest, CellError> {
        Ok(FrontierRequest {
            tenant: tenant.clone(),
            world: world.to_owned(),
            tables: self.analytics_tables()?,
        })
    }

    /// ANALYTICS-FRONTIER-1: the versions committed after `after`, in commit
    /// order, up to about `limit` rows in whole commits (a larger commit is
    /// sent whole), through the verified head when nothing more remains.
    ///
    /// # Errors
    /// A value commitment that does not verify, or a store error.
    pub fn analytics_delta(
        &self,
        tenant: &TenantId,
        world: &str,
        after: u64,
        limit: usize,
    ) -> Result<MirrorBatch, CellError> {
        let head = self.verified_head()?.commits;
        let tables = self.analytics_tables()?;
        let model = self.model();
        let reader = self.read();
        self.conn.transaction(|| {
            let bound = to_i64(u64::try_from(limit.saturating_add(1)).unwrap_or(u64::MAX), "limit")?;
            let sql = format!(
                "SELECT root_type,object_type,id,version,commit_seq,snapshot,hash FROM \"{FACT_VALUES}\" WHERE world=?1 AND commit_seq>?2 AND commit_seq<=?3 ORDER BY commit_seq,root_type,id,version LIMIT ?4"
            );
            let mut rows = self.conn.query(&sql, params![world, to_i64(after, "after")?, to_i64(head, "head")?, bound])?;
            let mut through = head;
            let mut complete = true;
            if rows.len() > limit {
                // Cut at the last whole commit; a single larger commit goes whole.
                let seq = |row: Option<&Vec<crate::engine::SqlValue>>| -> Result<u64, CellError> {
                    let row = row.ok_or(CellError::OutOfRange("delta row"))?;
                    to_u64(SqlRow(row.clone()).get(4)?, "commit")
                };
                let last = seq(rows.get(limit))?;
                let first = seq(rows.first())?;
                complete = false;
                if last == first {
                    rows = self.conn.query(
                        &format!("SELECT root_type,object_type,id,version,commit_seq,snapshot,hash FROM \"{FACT_VALUES}\" WHERE world=?1 AND commit_seq=?2 ORDER BY root_type,id,version"),
                        params![world, to_i64(first, "commit")?],
                    )?;
                    through = first;
                } else {
                    rows.retain(|row| SqlRow(row.clone()).get::<i64>(4).is_ok_and(|seq| u64::try_from(seq).is_ok_and(|seq| seq < last)));
                    through = last.saturating_sub(1);
                }
            }
            let mut out = Vec::with_capacity(rows.len());
            for row in rows {
                let row = SqlRow(row);
                let root: String = row.get(0)?;
                let object_type: String = row.get(1)?;
                let id: String = row.get(2)?;
                let version = to_u64(row.get(3)?, "fact version")?;
                let commit_seq = to_u64(row.get(4)?, "fact commit")?;
                let encoded: String = row.get(5)?;
                let hash: String = row.get(6)?;
                let Some(table) = tables.iter().find(|t| t.name == root) else {
                    continue;
                };
                let snapshot = crate::history::decode_snapshot(world, &root, &id, version, &encoded, &hash)?;
                // A value commitment is canonical JSON, whose text is NFC; the
                // exact engine reads current rows as written. A current version
                // is sent as written; a superseded one (never read by an answer
                // at the verified head) from its commitment.
                let values = match reader.object(world, &object_type, &id)? {
                    Some(record) if record.version == version => snapshot.values.map(|_| record.values),
                    Some(_) | None => snapshot.values,
                };
                let contract = model.query_contract(&root)?;
                let mut bad = false;
                let values = values.map(|values| {
                    table
                        .columns
                        .iter()
                        .map(|column| {
                            let stored = contract
                                .properties
                                .iter()
                                .find(|p| p.id.as_str() == column.id)
                                .and_then(|property| stored(column, property, values.get(&column.id)));
                            stored.unwrap_or_else(|| {
                                bad = true;
                                Value::Null
                            })
                        })
                        .collect()
                });
                out.push(MirrorRow {
                    table: root,
                    object_type,
                    id,
                    version,
                    commit_seq,
                    scope: snapshot.scope,
                    values,
                    bad,
                });
            }
            Ok(MirrorBatch {
                request: SyncRequest {
                    tenant: tenant.clone(),
                    world: world.to_owned(),
                    after,
                    through,
                    rows: out,
                },
                complete,
            })
        })
    }
}
