//! Candidate byte-length preflight before fetching and decoding SQLite values.

use std::collections::BTreeMap;
use std::fmt::Write as _;

use crate::codec::{DecodeContext, ValueOwner, decode_stored_value, encode};
use crate::engine::{
    MAX_PARAMETERS, MAX_SAFE_INTEGER, MAX_STATEMENT_BYTES, SqlEngine, SqlRow, SqlValue, params,
};
use crate::error::CellError;
use crate::reactive::{
    BudgetedIdentityRead, BudgetedLinksRead, BudgetedRead, DecodeBudget, Dependency,
};
use crate::read::{
    BindingSortValue, BindingSortedCandidate, BindingSortedPosition, ObjectRecord, StoreReader,
    query_expressions, read_projection, to_u64,
};
use crate::system::{LINK_VERSIONS, VERSIONS};
use norn_kernel::snapshot_observe::OBSERVED_TOMBSTONE_TYPE;
use norn_projections::ValueEncoding;
use norn_projections::sqlite_ddl::{
    ID_COLUMN, OVERFLOW_COLUMN, SCOPE_COLUMN, SOURCE_COLUMN, TARGET_COLUMN, TYPE_COLUMN,
    WORLD_COLUMN, quote_identifier,
};

fn sql_text_literal(value: &str) -> String {
    format!("'{}'", value.replace('\'', "''"))
}

fn hierarchy_type_clause(types: &[String], alias: &str) -> String {
    let values = types
        .iter()
        .map(|object_type| sql_text_literal(object_type))
        .collect::<Vec<_>>()
        .join(", ");
    format!("{alias}.{} IN ({values})", quote_identifier(TYPE_COLUMN))
}

const MAX_BUDGETED_LINK_ENDPOINTS: usize = 1_024;
const IDENTITY_QUERY_BYTES: u64 = 48;
const IDENTITY_ROW_BYTES: u64 = 64;
const LINK_QUERY_BYTES: u64 = 64;
const LINK_ROW_BYTES: u64 = 64;
const SORT_POSITION_ROW_BYTES: u64 = 32;
/// A sealed sorted-list key is capped to keep pagination handles bounded.
const MAX_BINDING_SORT_POSITION_BYTES: u64 = 64 * 1024;

#[derive(Clone, Copy)]
enum BindingSortedRange {
    NonNullStart,
    AscendingAfter,
    DescendingEqualAfter,
    DescendingLowerAfter,
    NullStart,
    NullAfter,
    NullExists,
}

#[derive(Clone, Copy)]
enum RootRowFilter {
    All,
    Active,
    Untombstoned,
}

fn binding_sort_sql_value(value: &BindingSortValue) -> SqlValue {
    match value {
        BindingSortValue::Integer(value) => SqlValue::Integer(*value),
        BindingSortValue::Text(value) => SqlValue::from(value.clone()),
        BindingSortValue::Null => SqlValue::Null,
    }
}

#[allow(
    clippy::too_many_arguments,
    clippy::too_many_lines,
    reason = "the ordered query builder keeps its index, phase and keyset inputs explicit"
)]
fn binding_sorted_metadata<E: SqlEngine>(
    engine: &E,
    world: &str,
    table: &str,
    property: &str,
    index: &str,
    descending: bool,
    range: BindingSortedRange,
    after: Option<&BindingSortedPosition>,
    limit: usize,
) -> Result<Vec<Vec<SqlValue>>, CellError> {
    let property = quote_identifier(property);
    let table = quote_identifier(table);
    let index = quote_identifier(index);
    let id = quote_identifier(ID_COLUMN);
    let world_column = quote_identifier(WORLD_COLUMN);
    let root_column = quote_identifier(TYPE_COLUMN);
    let mut bindings = vec![SqlValue::from(world)];
    let needs_after_id = matches!(
        range,
        BindingSortedRange::AscendingAfter
            | BindingSortedRange::DescendingEqualAfter
            | BindingSortedRange::DescendingLowerAfter
            | BindingSortedRange::NullStart
            | BindingSortedRange::NullAfter
            | BindingSortedRange::NullExists
    );
    let needs_after_value = matches!(
        range,
        BindingSortedRange::AscendingAfter
            | BindingSortedRange::DescendingEqualAfter
            | BindingSortedRange::DescendingLowerAfter
    );
    let after_id = if needs_after_id && let Some(after) = after {
        bindings.push(SqlValue::from(after.identity.clone()));
        Some(format!("?{}", bindings.len()))
    } else {
        None
    };
    let after_value = if needs_after_value && let Some(after) = after {
        bindings.push(binding_sort_sql_value(&after.value));
        Some(format!("?{}", bindings.len()))
    } else {
        None
    };
    let where_clause = match range {
        BindingSortedRange::NonNullStart => format!("r.{property} IS NOT NULL"),
        BindingSortedRange::AscendingAfter => format!(
            "r.{property} IS NOT NULL AND (r.{property}, r.{id}) > ({}, {}) AND r.{id} <> {}",
            after_value.as_deref().unwrap_or("NULL"),
            after_id.as_deref().unwrap_or("NULL"),
            after_id.as_deref().unwrap_or("NULL"),
        ),
        BindingSortedRange::DescendingEqualAfter => format!(
            "r.{property} = {} AND r.{id} > {}",
            after_value.as_deref().unwrap_or("NULL"),
            after_id.as_deref().unwrap_or("NULL"),
        ),
        BindingSortedRange::DescendingLowerAfter => format!(
            "r.{property} IS NOT NULL AND r.{property} < {} AND r.{id} <> {}",
            after_value.as_deref().unwrap_or("NULL"),
            after_id.as_deref().unwrap_or("NULL"),
        ),
        BindingSortedRange::NullStart => after_id.map_or_else(
            || format!("r.{property} IS NULL"),
            |after_id| format!("r.{property} IS NULL AND r.{id} <> {after_id}"),
        ),
        BindingSortedRange::NullAfter => format!(
            "r.{property} IS NULL AND r.{id} > {}",
            after_id.as_deref().unwrap_or("NULL"),
        ),
        BindingSortedRange::NullExists => after_id.map_or_else(
            || format!("r.{property} IS NULL"),
            |after_id| format!("r.{property} IS NULL AND r.{id} <> {after_id}"),
        ),
    };
    if matches!(range, BindingSortedRange::NullExists) {
        return engine.query(
            &format!(
                "SELECT 1 FROM {table} AS r INDEXED BY {index} WHERE r.{world_column} = ?1 AND {where_clause} LIMIT 1"
            ),
            &bindings,
        );
    }
    let order = match range {
        BindingSortedRange::NonNullStart | BindingSortedRange::AscendingAfter => {
            if descending {
                format!("r.{property} DESC, r.{id} ASC")
            } else {
                format!("r.{property} ASC, r.{id} ASC")
            }
        }
        BindingSortedRange::DescendingEqualAfter
        | BindingSortedRange::NullStart
        | BindingSortedRange::NullAfter => format!("r.{id} ASC"),
        BindingSortedRange::DescendingLowerAfter => {
            format!("r.{property} DESC, r.{id} ASC")
        }
        BindingSortedRange::NullExists => String::new(),
    };
    let limit_index = bindings
        .len()
        .checked_add(1)
        .ok_or(CellError::OutOfRange("binding sort parameter index"))?;
    bindings.push(SqlValue::from(
        i64::try_from(limit).unwrap_or(MAX_SAFE_INTEGER),
    ));
    engine.query(
        &format!(
            "SELECT r.rowid, octet_length(r.{id}) + octet_length(r.{root_column}), CASE WHEN r.{property} IS NULL THEN 0 WHEN typeof(r.{property}) = 'text' THEN octet_length(r.{property}) ELSE 8 END FROM {table} AS r INDEXED BY {index} WHERE r.{world_column} = ?1 AND {where_clause} ORDER BY {order} LIMIT ?{limit_index}"
        ),
        &bindings,
    )
}

#[derive(Clone, Copy)]
struct LinkEndpointQuery<'a> {
    table: &'a str,
    lookup_column: &'static str,
    select_column: &'static str,
    query_bytes: u64,
}

fn charge_row(budget: &mut DecodeBudget) -> bool {
    if budget.rows == 0 {
        return false;
    }
    budget.rows = budget.rows.saturating_sub(1);
    true
}

impl<E: SqlEngine> StoreReader<'_, E> {
    /// Read one candidate in declared-property order using its covering
    /// index. Nullable values occupy a final ascending-id phase in both
    /// directions; the returned boundary is a snapshot, so deleting the last
    /// row between pages cannot invalidate the next keyset seek.
    ///
    /// If the candidate or its sealed position cannot fit the request budget,
    /// this returns `None` without advancing the supplied boundary.
    ///
    /// # Errors
    /// Unknown types/properties, missing installed sort indexes, or SQL errors.
    #[allow(
        clippy::too_many_arguments,
        clippy::too_many_lines,
        reason = "candidate seek phases, metadata preflight and request budget share one indexed scan"
    )]
    pub fn binding_sorted_candidate_after_with_budget(
        &self,
        world: &str,
        object_type: &str,
        property_id: &str,
        descending: bool,
        after: Option<&BindingSortedPosition>,
        budget: &mut DecodeBudget,
        candidate_bytes: u64,
    ) -> Result<Option<BindingSortedCandidate>, CellError> {
        let root_type = self.root_type(object_type)?;
        let column = self.model().column(root_type, property_id)?;
        let property = column
            .column
            .column
            .as_deref()
            .ok_or(CellError::OutOfRange(
                "binding sort property is not physical",
            ))?;
        let table = column.table.table.as_str();
        let index = self
            .model()
            .binding_sort_index_name(object_type, property_id, descending)?;

        if budget.rows == 0 || budget.remaining_bytes() < IDENTITY_ROW_BYTES {
            return Ok(None);
        }

        let (mut metadata, selected_range) = match after {
            Some(position) if matches!(&position.value, BindingSortValue::Null) => (
                binding_sorted_metadata(
                    self.conn,
                    world,
                    table,
                    property,
                    &index,
                    descending,
                    BindingSortedRange::NullAfter,
                    Some(position),
                    2,
                )?,
                BindingSortedRange::NullAfter,
            ),
            Some(position) if descending => {
                let equal = binding_sorted_metadata(
                    self.conn,
                    world,
                    table,
                    property,
                    &index,
                    true,
                    BindingSortedRange::DescendingEqualAfter,
                    Some(position),
                    2,
                )?;
                if equal.is_empty() {
                    (
                        binding_sorted_metadata(
                            self.conn,
                            world,
                            table,
                            property,
                            &index,
                            true,
                            BindingSortedRange::DescendingLowerAfter,
                            Some(position),
                            2,
                        )?,
                        BindingSortedRange::DescendingLowerAfter,
                    )
                } else {
                    (equal, BindingSortedRange::DescendingEqualAfter)
                }
            }
            Some(position) => (
                binding_sorted_metadata(
                    self.conn,
                    world,
                    table,
                    property,
                    &index,
                    false,
                    BindingSortedRange::AscendingAfter,
                    Some(position),
                    2,
                )?,
                BindingSortedRange::AscendingAfter,
            ),
            None => (
                binding_sorted_metadata(
                    self.conn,
                    world,
                    table,
                    property,
                    &index,
                    descending,
                    BindingSortedRange::NonNullStart,
                    None,
                    2,
                )?,
                BindingSortedRange::NonNullStart,
            ),
        };

        if metadata.is_empty() && !matches!(selected_range, BindingSortedRange::NullAfter) {
            metadata = binding_sorted_metadata(
                self.conn,
                world,
                table,
                property,
                &index,
                descending,
                BindingSortedRange::NullStart,
                after,
                2,
            )?;
        }
        let Some(values) = metadata.first() else {
            return Ok(Some(BindingSortedCandidate {
                identity: None,
                position: None,
                has_more: false,
            }));
        };

        let row = SqlRow(values.clone());
        let rowid = row.get::<i64>(0)?;
        let identity_bytes = to_u64(row.get(1)?, "binding identity bytes")?;
        let sort_value_bytes = to_u64(row.get(2)?, "binding sort value bytes")?;
        let position_upper_bound = identity_bytes
            .saturating_add(sort_value_bytes)
            .saturating_mul(6)
            .saturating_add(128);
        let candidate_cost = IDENTITY_ROW_BYTES
            .saturating_add(identity_bytes)
            .saturating_add(SORT_POSITION_ROW_BYTES)
            .saturating_add(sort_value_bytes);
        if position_upper_bound > MAX_BINDING_SORT_POSITION_BYTES
            || candidate_cost > candidate_bytes
            || budget.rows == 0
            || !budget.charge(candidate_cost)
        {
            return Ok(None);
        }
        budget.rows = budget.rows.saturating_sub(1);

        // The rowid comes from the covering index's implicit rowid entry.
        // Only after length preflight do we fetch the owned key and identity.
        let fetched = self.conn.query(
            &format!(
                "SELECT {id}, {root_column}, {property} FROM {table} WHERE rowid = ?1 AND {world_column} = ?2",
                id = quote_identifier(ID_COLUMN),
                root_column = quote_identifier(TYPE_COLUMN),
                property = quote_identifier(property),
                table = quote_identifier(table),
                world_column = quote_identifier(WORLD_COLUMN),
            ),
            &[SqlValue::from(rowid), SqlValue::from(world)],
        )?;
        let Some(fetched) = fetched.first() else {
            // A concurrent delete invalidates only this candidate, never the
            // caller's sealed boundary. Retry from the unchanged position.
            return Ok(None);
        };
        let fetched = SqlRow(fetched.clone());
        let (id, concrete_type) = match (fetched.get_ref(0)?, fetched.get_ref(1)?) {
            (SqlValue::Text(id), SqlValue::Text(concrete_type)) => {
                (id.clone(), concrete_type.clone())
            }
            _ => return Ok(None),
        };
        let sort_value = match fetched.get_ref(2)? {
            SqlValue::Null => BindingSortValue::Null,
            SqlValue::Integer(value) => BindingSortValue::Integer(*value),
            SqlValue::Text(value) => BindingSortValue::Text(value.clone()),
            SqlValue::Real(_) | SqlValue::Blob(_) => return Ok(None),
        };
        let is_null = matches!(&sort_value, BindingSortValue::Null);
        let mut has_more = metadata.len() > 1;
        if !is_null && !has_more {
            if matches!(selected_range, BindingSortedRange::DescendingEqualAfter) {
                has_more = !binding_sorted_metadata(
                    self.conn,
                    world,
                    table,
                    property,
                    &index,
                    true,
                    BindingSortedRange::DescendingLowerAfter,
                    after,
                    1,
                )?
                .is_empty();
            }
            if !has_more {
                has_more = !binding_sorted_metadata(
                    self.conn,
                    world,
                    table,
                    property,
                    &index,
                    descending,
                    BindingSortedRange::NullExists,
                    after,
                    1,
                )?
                .is_empty();
            }
        }
        Ok(Some(BindingSortedCandidate {
            identity: Some((id.clone(), concrete_type)),
            position: Some(BindingSortedPosition {
                value: sort_value,
                identity: id,
            }),
            has_more,
        }))
    }

    /// Read one root candidate in indexed key order, preflighting identity
    /// lengths before fetching owned text. Oversized identities advance by
    /// a persistent version-row locator without allocating their text.
    ///
    /// # Errors
    /// Unknown type, stale locator, or SQL/metadata errors.
    #[allow(
        clippy::too_many_arguments,
        clippy::too_many_lines,
        reason = "one indexed candidate query shares position, metadata and request budget"
    )]
    pub fn binding_candidate_after_with_budget(
        &self,
        world: &str,
        object_type: &str,
        after_key: Option<&str>,
        after_locator: Option<u64>,
        budget: &mut DecodeBudget,
        candidate_bytes: u64,
    ) -> Result<Option<crate::read::BindingRootCandidate>, CellError> {
        let root_type = self.root_type(object_type)?;
        let table = quote_identifier(&self.model().table(root_type)?.table);
        let mut bindings = vec![SqlValue::from(world), SqlValue::from(root_type)];
        let after = if let Some(locator) = after_locator {
            let locator = i64::try_from(locator)
                .map_err(|_error| CellError::OutOfRange("binding scan locator"))?;
            let exists: Option<i64> = self.conn.query_optional(
                &format!("SELECT rowid FROM \"{VERSIONS}\" WHERE rowid = ?1 AND \"world\" = ?2 AND \"root_type\" = ?3"),
                params![locator, world, root_type],
                |row| row.get(0),
            )?;
            if exists.is_none() {
                return Err(CellError::OutOfRange("binding scan locator"));
            }
            bindings.push(SqlValue::from(locator));
            format!(
                " AND r.\"_id\" > (SELECT \"id\" FROM \"{VERSIONS}\" WHERE rowid = ?3 AND \"world\" = ?1 AND \"root_type\" = ?2)"
            )
        } else if let Some(key) = after_key {
            bindings.push(SqlValue::from(key));
            " AND r.\"_id\" > ?3".to_owned()
        } else {
            String::new()
        };
        if budget.rows == 0 || budget.remaining_bytes() < IDENTITY_ROW_BYTES {
            return Ok(None);
        }
        // CROSS JOIN fixes the root table as the driving key-ordered stream;
        // dead high-water rows cannot turn this into an unbounded version scan.
        let from = format!(
            "FROM {table} AS r CROSS JOIN \"{VERSIONS}\" AS v ON v.\"world\" = r.\"_world\" AND v.\"root_type\" = ?2 AND v.\"id\" = r.\"_id\" WHERE r.\"_world\" = ?1{after} ORDER BY r.\"_id\" LIMIT 2"
        );
        let metadata = self.conn.query(
            &format!("SELECT v.rowid, octet_length(r.\"_id\") + octet_length(r.\"_type\") {from}"),
            &bindings,
        )?;
        let Some(values) = metadata.first() else {
            return Ok(Some(crate::read::BindingRootCandidate {
                identity: None,
                locator: 0,
                has_more: false,
            }));
        };
        let row = SqlRow(values.clone());
        let locator = to_u64(row.get(0)?, "binding scan locator")?;
        let bytes =
            IDENTITY_ROW_BYTES.saturating_add(to_u64(row.get(1)?, "binding identity bytes")?);
        let has_more = metadata.len() > 1;
        if bytes > candidate_bytes {
            budget.rows = budget.rows.saturating_sub(1);
            if !budget.charge(IDENTITY_ROW_BYTES) {
                return Ok(None);
            }
            return Ok(Some(crate::read::BindingRootCandidate {
                identity: None,
                locator,
                has_more,
            }));
        }
        if !budget.charge(bytes) {
            return Ok(None);
        }
        budget.rows = budget.rows.saturating_sub(1);
        let identity = self.conn.query_optional(
            &format!("SELECT r.\"_id\", r.\"_type\" FROM {table} AS r JOIN \"{VERSIONS}\" AS v ON v.\"world\" = r.\"_world\" AND v.\"root_type\" = ?2 AND v.\"id\" = r.\"_id\" WHERE v.rowid = ?3 AND r.\"_world\" = ?1"),
            params![world, root_type, i64::try_from(locator).map_err(|_error| CellError::OutOfRange("binding scan locator"))?],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        Ok(Some(crate::read::BindingRootCandidate {
            identity,
            locator,
            has_more,
        }))
    }

    fn link_endpoint_ids_with_budget(
        &self,
        world: &str,
        query: LinkEndpointQuery<'_>,
        lookup_value: &str,
        max_rows: usize,
        budget: &mut DecodeBudget,
    ) -> Result<BudgetedLinksRead, CellError> {
        let query_bytes = query
            .query_bytes
            .saturating_add(u64::try_from(lookup_value.len()).unwrap_or(u64::MAX));
        if !budget.charge(query_bytes) {
            return Ok(BudgetedLinksRead::Exhausted);
        }
        let cap = max_rows
            .min(MAX_BUDGETED_LINK_ENDPOINTS)
            .min(budget.rows)
            .min(usize::try_from(budget.remaining_bytes() / LINK_ROW_BYTES).unwrap_or(usize::MAX));
        if cap == 0 {
            // An empty result cannot prove that a complete endpoint set fits
            // when no candidate can be admitted; refuse before inspecting it.
            return Ok(BudgetedLinksRead::Exhausted);
        }
        let fetch = i64::try_from(cap.saturating_add(1)).unwrap_or(MAX_SAFE_INTEGER);
        // The inverse lookup must use the existing target index. Sorting by
        // source lets SQLite choose the source-first primary key and scan all
        // links in a world before LIMIT. Both reads use the same target-index
        // traversal while the cell lock holds the committed view.
        let (table_sql, order_sql) = if query.lookup_column == TARGET_COLUMN {
            let index = format!("{}/target", query.table);
            (
                format!(
                    "{} INDEXED BY {}",
                    quote_identifier(query.table),
                    quote_identifier(&index)
                ),
                String::new(),
            )
        } else {
            (
                quote_identifier(query.table),
                format!(" ORDER BY {}", quote_identifier(query.select_column)),
            )
        };
        let metadata_sql = format!(
            "SELECT octet_length({}) FROM {} WHERE {} = ?1 AND {} = ?2{} LIMIT ?3",
            quote_identifier(query.select_column),
            table_sql,
            quote_identifier(WORLD_COLUMN),
            quote_identifier(query.lookup_column),
            order_sql,
        );
        let rows = self
            .conn
            .query(&metadata_sql, params![world, lookup_value, fetch])?;
        let mut admitted = 0_usize;
        for values in rows {
            if admitted >= cap || !charge_row(budget) {
                return Ok(BudgetedLinksRead::Exhausted);
            }
            let length = to_u64(SqlRow(values).get::<i64>(0)?, "link endpoint length")?;
            let cost = LINK_ROW_BYTES.saturating_add(length);
            if !budget.charge(cost) {
                return Ok(BudgetedLinksRead::Exhausted);
            }
            admitted = admitted.saturating_add(1);
        }
        let sql = format!(
            "SELECT {} FROM {} WHERE {} = ?1 AND {} = ?2{} LIMIT ?3",
            quote_identifier(query.select_column),
            table_sql,
            quote_identifier(WORLD_COLUMN),
            quote_identifier(query.lookup_column),
            order_sql,
        );
        let ids = self
            .conn
            .query(
                &sql,
                params![
                    world,
                    lookup_value,
                    i64::try_from(admitted).unwrap_or(MAX_SAFE_INTEGER)
                ],
            )?
            .into_iter()
            .map(|values| SqlRow(values).get::<String>(0))
            .collect::<Result<Vec<_>, _>>()?;
        Ok(BudgetedLinksRead::Complete(ids))
    }

    /// Read only live target ids of `link` from `source`, without decoding
    /// link properties. The indexed row count and stored target lengths are
    /// checked before owned target ids are fetched.
    ///
    /// # Errors
    /// Unknown type/link or SQL/decode errors.
    pub fn link_targets_with_budget(
        &self,
        world: &str,
        object_type: &str,
        link: &str,
        source: &str,
        budget: &mut DecodeBudget,
    ) -> Result<BudgetedLinksRead, CellError> {
        budget.depend(Dependency::Links {
            object_type: object_type.to_owned(),
            key: source.to_owned(),
            link: Some(link.to_owned()),
            outgoing: true,
            incoming: false,
        })?;
        let (_, table) = self.model().link(object_type, link)?;
        let query_bytes = LINK_QUERY_BYTES
            .saturating_add(u64::try_from(object_type.len()).unwrap_or(u64::MAX))
            .saturating_add(u64::try_from(link.len()).unwrap_or(u64::MAX));
        self.link_endpoint_ids_with_budget(
            world,
            LinkEndpointQuery {
                table: &table.table,
                lookup_column: SOURCE_COLUMN,
                select_column: TARGET_COLUMN,
                query_bytes,
            },
            source,
            MAX_BUDGETED_LINK_ENDPOINTS,
            budget,
        )
    }

    /// Read source ids of live rows of `link` that point at `target`. The
    /// lookup uses the target index and preflights each source id before the
    /// second query allocates it. At most `max_rows` identities are returned.
    ///
    /// # Errors
    /// Unknown type/link or SQL/decode errors.
    pub fn link_sources_to_with_budget(
        &self,
        world: &str,
        object_type: &str,
        link: &str,
        target: &str,
        max_rows: usize,
        budget: &mut DecodeBudget,
    ) -> Result<BudgetedLinksRead, CellError> {
        budget.depend(Dependency::IncomingLinks {
            source_type: object_type.to_owned(),
            target: target.to_owned(),
            link: link.to_owned(),
        })?;
        let (_, table) = self.model().link(object_type, link)?;
        let query_bytes = LINK_QUERY_BYTES
            .saturating_add(u64::try_from(object_type.len()).unwrap_or(u64::MAX))
            .saturating_add(u64::try_from(link.len()).unwrap_or(u64::MAX));
        self.link_endpoint_ids_with_budget(
            world,
            LinkEndpointQuery {
                table: &table.table,
                lookup_column: TARGET_COLUMN,
                select_column: SOURCE_COLUMN,
                query_bytes,
            },
            target,
            max_rows,
            budget,
        )
    }

    /// Read one object's concrete type and key without decoding properties.
    /// Stored identity length is charged before the type string is fetched.
    ///
    /// # Errors
    /// Unknown type, SQL engine, or invalid stored lengths.
    pub fn object_identity_with_budget(
        &self,
        world: &str,
        object_type: &str,
        id: &str,
        budget: &mut DecodeBudget,
    ) -> Result<BudgetedIdentityRead, CellError> {
        budget.depend(Dependency::Object {
            object_type: object_type.to_owned(),
            key: id.to_owned(),
        })?;
        let table = self.model().table(self.root_type(object_type)?)?;
        let query_bytes =
            IDENTITY_QUERY_BYTES.saturating_add(u64::try_from(id.len()).unwrap_or(u64::MAX));
        if !budget.charge(query_bytes) || !charge_row(budget) {
            return Ok(BudgetedIdentityRead::Exhausted);
        }
        let concrete_length: Option<i64> = self.conn.query_optional(
            &format!(
                "SELECT octet_length({}) FROM {} WHERE {} = ?1 AND {} = ?2",
                quote_identifier(TYPE_COLUMN),
                quote_identifier(&table.table),
                quote_identifier(WORLD_COLUMN),
                quote_identifier(ID_COLUMN),
            ),
            params![world, id],
            |row| row.get(0),
        )?;
        let Some(concrete_length) = concrete_length else {
            return Ok(BudgetedIdentityRead::Missing);
        };
        let cost = IDENTITY_ROW_BYTES
            .saturating_add(u64::try_from(id.len()).unwrap_or(u64::MAX))
            .saturating_add(to_u64(concrete_length, "concrete type length")?);
        if !budget.charge(cost) {
            return Ok(BudgetedIdentityRead::Exhausted);
        }
        let concrete: Option<String> = self.conn.query_optional(
            &format!(
                "SELECT {} FROM {} WHERE {} = ?1 AND {} = ?2",
                quote_identifier(TYPE_COLUMN),
                quote_identifier(&table.table),
                quote_identifier(WORLD_COLUMN),
                quote_identifier(ID_COLUMN),
            ),
            params![world, id],
            |row| row.get(0),
        )?;
        let Some(concrete) = concrete else {
            return Ok(BudgetedIdentityRead::Missing);
        };
        if !self.model().is_a(&concrete, object_type) {
            return Ok(BudgetedIdentityRead::Missing);
        }
        Ok(BudgetedIdentityRead::Found {
            object_type: concrete,
            id: id.to_owned(),
        })
    }

    /// Current commit cursor, read without decoding the receipt history.
    ///
    /// # Errors
    /// SQL engine errors or an out-of-range commit sequence.
    pub fn head_commit_seq(&self) -> Result<u64, CellError> {
        Ok(self.next_commit_seq()?.saturating_sub(1))
    }

    /// Scan metadata lengths and charge bytes before fetching owned IDs/types.
    /// Both passes read the same committed cell view; only the admitted prefix
    /// is fetched. Sibling rows consume the same row/byte budget. Exhaustion
    /// produces an incomplete scan, including a single oversized ID or type.
    ///
    /// # Errors
    /// Unknown type, SQL engine, or invalid stored lengths.
    pub fn root_rows_with_budget(
        &self,
        world: &str,
        object_type: &str,
        max_rows: usize,
        budget: &mut DecodeBudget,
    ) -> Result<(Vec<(String, String)>, bool), CellError> {
        self.root_rows_with_budget_filtered(
            world,
            object_type,
            max_rows,
            budget,
            RootRowFilter::All,
            &[],
            None,
        )
    }

    /// Scan a bounded key-ordered root stream, excluding observed tombstones
    /// before identity metadata consumes the decode budget. If the model does
    /// not declare the tombstone type, this is equivalent to
    /// [`Self::root_rows_with_budget`].
    ///
    /// The completion flag describes the filtered stream after tombstone
    /// exclusion: `false` means another visible row exists beyond `max_rows`
    /// or the next visible row could not fit the shared budget.
    ///
    /// # Errors
    /// Unknown type, SQL engine, or invalid stored lengths.
    pub fn untombstoned_root_rows_with_budget(
        &self,
        world: &str,
        object_type: &str,
        max_rows: usize,
        budget: &mut DecodeBudget,
    ) -> Result<(Vec<(String, String)>, bool), CellError> {
        self.root_rows_with_budget_filtered(
            world,
            object_type,
            max_rows,
            budget,
            RootRowFilter::Untombstoned,
            &[],
            None,
        )
    }

    /// The same bounded root scan, excluding tombstones and known non-active
    /// status values before metadata or decode work. Unknown statuses remain visible
    /// so the semantic identity projection can reject them.
    ///
    /// # Errors
    /// Unknown type/property, SQL engine, or invalid stored lengths.
    pub fn active_root_rows_with_budget(
        &self,
        world: &str,
        object_type: &str,
        max_rows: usize,
        budget: &mut DecodeBudget,
    ) -> Result<(Vec<(String, String)>, bool), CellError> {
        self.root_rows_with_budget_filtered(
            world,
            object_type,
            max_rows,
            budget,
            RootRowFilter::Active,
            &[],
            None,
        )
    }

    /// Scan only rows matching all supplied stored text properties. Predicates
    /// are applied before metadata and payloads consume the decode budget.
    ///
    /// # Errors
    /// Unknown type/property, a property without a stored text column, SQL
    /// engine errors, or invalid stored lengths.
    pub fn matching_root_rows_with_budget(
        &self,
        world: &str,
        object_type: &str,
        predicates: &[(&str, &str)],
        max_rows: usize,
        budget: &mut DecodeBudget,
    ) -> Result<(Vec<(String, String)>, bool), CellError> {
        self.matching_root_rows_after_with_budget(
            world,
            object_type,
            predicates,
            None,
            max_rows,
            budget,
        )
    }

    /// Read a bounded page of root rows matching stored text properties after
    /// an exclusive identity boundary. Predicates and the keyset boundary are
    /// applied before metadata or payloads consume the decode budget.
    ///
    /// The returned completion flag describes the matching stream after the
    /// supplied boundary. An incomplete page can resume from its last returned
    /// identity with a fresh budget, including after that identity is deleted.
    ///
    /// # Errors
    /// Unknown type/property, a property without a stored text column, SQL
    /// engine errors, or invalid stored lengths.
    #[allow(
        clippy::too_many_arguments,
        reason = "the filtered keyset page keeps its shared decode budget explicit"
    )]
    pub fn matching_root_rows_after_with_budget(
        &self,
        world: &str,
        object_type: &str,
        predicates: &[(&str, &str)],
        after: Option<&str>,
        max_rows: usize,
        budget: &mut DecodeBudget,
    ) -> Result<(Vec<(String, String)>, bool), CellError> {
        self.root_rows_with_budget_filtered(
            world,
            object_type,
            max_rows,
            budget,
            RootRowFilter::All,
            predicates,
            after,
        )
    }

    /// Read a bounded key-ordered stream of raw root rows for `object_type`'s
    /// hierarchy. Sibling concrete types share the root table and are returned
    /// for the caller to filter with the model after charging their candidate
    /// work. The metadata pass charges identity bytes before owned IDs/types
    /// are fetched.
    ///
    /// A false completion flag means either the stream has a row beyond the
    /// admitted bound or the shared row/byte budget could not admit the next
    /// candidate. The caller can fail closed instead of returning a partial
    /// sorted page.
    ///
    /// # Errors
    /// Unknown type, SQL engine, or invalid stored lengths.
    pub fn binding_rows_after_with_budget(
        &self,
        world: &str,
        object_type: &str,
        after: Option<&str>,
        max_rows: usize,
        budget: &mut DecodeBudget,
    ) -> Result<(Vec<(String, String)>, bool), CellError> {
        let root = self.model().table(self.root_type(object_type)?)?;
        let table = quote_identifier(&root.table);
        let id = format!("r.{}", quote_identifier(ID_COLUMN));
        let concrete = format!("r.{}", quote_identifier(TYPE_COLUMN));
        let after_clause = if after.is_some() {
            format!(" AND {id} > ?2")
        } else {
            String::new()
        };
        let limit_parameter = if after.is_some() { 3 } else { 2 };
        let from = format!(
            "FROM {table} AS r WHERE r.{} = ?1{after_clause} ORDER BY {id} LIMIT ?{limit_parameter}",
            quote_identifier(WORLD_COLUMN),
        );
        let safe_max_rows = usize::try_from(MAX_SAFE_INTEGER)
            .unwrap_or(usize::MAX)
            .saturating_sub(1);
        let cap = max_rows.min(budget.rows).min(safe_max_rows);
        let header_bytes =
            u64::try_from(std::mem::size_of::<(String, String)>()).unwrap_or(u64::MAX);
        if cap == 0 || budget.remaining_bytes() < header_bytes {
            return Ok((Vec::new(), false));
        }
        let fetch = i64::try_from(cap.saturating_add(1)).unwrap_or(MAX_SAFE_INTEGER);
        let mut bindings = vec![SqlValue::from(world)];
        if let Some(after) = after {
            bindings.push(SqlValue::from(after));
        }
        bindings.push(SqlValue::from(fetch));
        let lengths =
            format!("SELECT octet_length({id}) + octet_length({concrete}) + {header_bytes} {from}");
        if lengths.len() > MAX_STATEMENT_BYTES {
            return Ok((Vec::new(), false));
        }
        let metadata = self.conn.query(&lengths, &bindings)?;
        let mut admitted = 0_usize;
        let mut complete = metadata.len() <= cap;
        for values in metadata.iter().take(cap) {
            budget.rows = budget.rows.saturating_sub(1);
            let size = to_u64(SqlRow(values.clone()).get::<i64>(0)?, "scan metadata bytes")?;
            if !budget.charge(size) {
                complete = false;
                break;
            }
            admitted = admitted.saturating_add(1);
        }

        if admitted == 0 {
            return Ok((Vec::new(), complete));
        }
        if let Some(limit) = bindings.last_mut() {
            *limit = SqlValue::from(i64::try_from(admitted).unwrap_or(MAX_SAFE_INTEGER));
        }
        let sql = format!("SELECT {id}, {concrete} {from}");
        if sql.len() > MAX_STATEMENT_BYTES {
            return Ok((Vec::new(), false));
        }
        let rows = self.conn.query(&sql, &bindings)?;
        let out = rows
            .into_iter()
            .map(|values| {
                let row = SqlRow(values);
                Ok((row.get(0)?, row.get(1)?))
            })
            .collect::<Result<Vec<_>, CellError>>()?;
        Ok((out, complete))
    }

    /// Use an existing declared, exact identity index to find a binding's
    /// typed property key. Normalized or policy-backed keys are unsupported
    /// because their equality semantics cannot be represented by a direct
    /// physical-column lookup; in that case this returns an empty incomplete
    /// scan without spending the caller's budget, so it can fall back to the
    /// bounded key-ordered reader.
    ///
    /// # Errors
    /// Unknown object type/property, unencodable key values, SQL engine, or
    /// invalid stored lengths.
    #[allow(
        clippy::too_many_lines,
        reason = "the indexed-key selection, metadata preflight, and fetch share one bounded read"
    )]
    pub fn binding_key_rows_with_budget(
        &self,
        world: &str,
        scope: &str,
        object_type: &str,
        key: &[(String, norn_ir::RuntimeValue)],
        max_rows: usize,
        budget: &mut DecodeBudget,
    ) -> Result<(Vec<(String, String)>, bool), CellError> {
        let tables = self.model().tables(object_type)?;
        let mut provided = BTreeMap::new();
        for (property, value) in key {
            if provided.insert(property.as_str(), value).is_some() {
                return Ok((Vec::new(), false));
            }
        }
        if provided.is_empty() {
            return Ok((Vec::new(), false));
        }

        let mut selected: Option<(&str, &norn_projections::ObjectTable, &norn_ir::KeySpec)> = None;
        'owners: for table in &tables {
            let owner = table.object_type.as_str();
            let definition = self.model().def(owner)?;
            for identity in &definition.identity {
                if identity.rule.is_some() || identity.properties.len() != provided.len() {
                    continue;
                }
                if identity
                    .properties
                    .iter()
                    .any(|property| !provided.contains_key(property.as_str()))
                {
                    continue;
                }
                selected = Some((owner, table, identity));
                break 'owners;
            }
        }
        let Some((owner, key_table, identity)) = selected else {
            return Ok((Vec::new(), false));
        };
        if identity.properties.len().saturating_add(3) > MAX_PARAMETERS {
            return Ok((Vec::new(), false));
        }

        let mut columns = Vec::with_capacity(identity.properties.len());
        let mut property_bindings = Vec::with_capacity(identity.properties.len());
        for property in &identity.properties {
            let name = property.as_str();
            let column = self.model().column(owner, name)?;
            if column.table.table != key_table.table
                || column.column.column.is_none()
                || column.column.nullable
            {
                return Ok((Vec::new(), false));
            }
            let Some(value) = provided.get(name) else {
                return Ok((Vec::new(), false));
            };
            property_bindings.push(encode(owner, column.column, value)?);
            columns.push(quote_identifier(name));
        }

        let cap = max_rows.min(budget.rows).min(
            usize::try_from(MAX_SAFE_INTEGER)
                .unwrap_or(usize::MAX)
                .saturating_sub(1),
        );
        let header_bytes =
            u64::try_from(std::mem::size_of::<(String, String)>()).unwrap_or(u64::MAX);
        if cap == 0 || budget.remaining_bytes() < header_bytes {
            return Ok((Vec::new(), false));
        }
        let root = self.model().table(self.root_type(object_type)?)?;
        let hierarchy_types: Vec<String> = self
            .model()
            .object_types()
            .filter(|definition| self.model().is_a(definition.name.as_str(), object_type))
            .map(|definition| definition.name.as_str().to_owned())
            .collect();
        let hierarchy = hierarchy_type_clause(&hierarchy_types, "r");
        let key_table_name = quote_identifier(&key_table.table);
        let identity_index = quote_identifier(&format!(
            "{}/key:{}",
            key_table.table,
            identity.name.as_str()
        ));
        let root_table_name = quote_identifier(&root.table);
        let id = format!("k.{}", quote_identifier(ID_COLUMN));
        let concrete = format!("r.{}", quote_identifier(TYPE_COLUMN));
        let mut predicates = vec![
            format!("k.{} = ?1", quote_identifier(SCOPE_COLUMN)),
            format!("k.{} = ?2", quote_identifier(WORLD_COLUMN)),
        ];
        for (index, column) in columns.iter().enumerate() {
            let parameter = index.saturating_add(3);
            predicates.push(format!("k.{column} = ?{parameter}"));
        }
        let limit_parameter = property_bindings.len().saturating_add(3);
        let from = format!(
            "FROM {key_table_name} AS k INDEXED BY {identity_index} JOIN {root_table_name} AS r ON r.{} = k.{} AND r.{} = k.{} WHERE {} AND {hierarchy} LIMIT ?{limit_parameter}",
            quote_identifier(WORLD_COLUMN),
            quote_identifier(WORLD_COLUMN),
            quote_identifier(ID_COLUMN),
            quote_identifier(ID_COLUMN),
            predicates.join(" AND "),
        );
        let mut bindings = vec![SqlValue::from(scope), SqlValue::from(world)];
        bindings.extend(property_bindings);
        bindings.push(SqlValue::from(
            i64::try_from(cap.saturating_add(1)).unwrap_or(MAX_SAFE_INTEGER),
        ));
        let metadata_sql =
            format!("SELECT octet_length({id}) + octet_length({concrete}) + {header_bytes} {from}");
        let fetch_sql = format!("SELECT {id}, {concrete} {from}");
        if metadata_sql.len() > MAX_STATEMENT_BYTES || fetch_sql.len() > MAX_STATEMENT_BYTES {
            return Ok((Vec::new(), false));
        }
        let metadata = self.conn.query(&metadata_sql, &bindings)?;
        let mut admitted = 0_usize;
        let mut complete = metadata.len() <= cap;
        for values in metadata.iter().take(cap) {
            budget.rows = budget.rows.saturating_sub(1);
            let size = to_u64(SqlRow(values.clone()).get::<i64>(0)?, "scan metadata bytes")?;
            if !budget.charge(size) {
                complete = false;
                break;
            }
            admitted = admitted.saturating_add(1);
        }
        if admitted == 0 {
            return Ok((Vec::new(), complete));
        }
        if let Some(limit) = bindings.last_mut() {
            *limit = SqlValue::from(i64::try_from(admitted).unwrap_or(MAX_SAFE_INTEGER));
        }
        let rows = self.conn.query(&fetch_sql, &bindings)?;
        let mut out = Vec::with_capacity(rows.len());
        for values in rows {
            let row = SqlRow(values);
            let id: String = row.get(0)?;
            let concrete: String = row.get(1)?;
            if self.model().is_a(&concrete, object_type) {
                out.push((id, concrete));
            }
        }
        Ok((out, complete))
    }

    #[allow(
        clippy::too_many_arguments,
        clippy::too_many_lines,
        reason = "existing filtered root scans and keyset pages share one preflight implementation"
    )]
    fn root_rows_with_budget_filtered(
        &self,
        world: &str,
        object_type: &str,
        max_rows: usize,
        budget: &mut DecodeBudget,
        filter: RootRowFilter,
        predicates: &[(&str, &str)],
        after: Option<&str>,
    ) -> Result<(Vec<(String, String)>, bool), CellError> {
        let table = self.model().table(self.root_type(object_type)?)?;
        let id = format!("r.{}", quote_identifier(ID_COLUMN));
        let concrete = format!("r.{}", quote_identifier(TYPE_COLUMN));
        let headers = std::mem::size_of::<(String, String)>();
        let mut status_filter = if matches!(filter, RootRowFilter::Active) {
            let status = self.model().column(object_type, "status")?;
            let column = status.column.column.as_ref().ok_or_else(|| {
                CellError::Engine(format!("{object_type}.status has no stored column"))
            })?;
            format!(
                " AND COALESCE((SELECT s.{} FROM {} AS s WHERE s.{} = r.{} AND s.{} = r.{}), 'active') NOT IN ('inactive', 'revoked', 'suspended')",
                quote_identifier(column),
                quote_identifier(&status.table.table),
                quote_identifier(WORLD_COLUMN),
                quote_identifier(WORLD_COLUMN),
                quote_identifier(ID_COLUMN),
                quote_identifier(ID_COLUMN),
            )
        } else {
            String::new()
        };
        if matches!(filter, RootRowFilter::Untombstoned | RootRowFilter::Active) {
            let tombstone_types: Vec<String> = self
                .model()
                .object_types()
                .filter(|definition| {
                    self.model()
                        .is_a(definition.name.as_str(), OBSERVED_TOMBSTONE_TYPE)
                })
                .map(|definition| definition.name.as_str().to_owned())
                .collect();
            if !tombstone_types.is_empty() {
                let tombstone_root = self.root_type(OBSERVED_TOMBSTONE_TYPE)?;
                let tombstone_table = self.model().table(tombstone_root)?;
                let type_clause = hierarchy_type_clause(&tombstone_types, "t");
                // The projected primary key is (_world, _id), so each correlated
                // tombstone probe can use its world-scoped anchor index.
                #[expect(
                    clippy::let_underscore_must_use,
                    reason = "fmt::Write for String is infallible; its trait signature still returns Result"
                )]
                let _ = write!(
                    status_filter,
                    " AND NOT EXISTS (SELECT 1 FROM {} AS t WHERE t.{} = r.{} AND t.{} = (r.{} || '/' || r.{}) AND {type_clause})",
                    quote_identifier(&tombstone_table.table),
                    quote_identifier(WORLD_COLUMN),
                    quote_identifier(WORLD_COLUMN),
                    quote_identifier(ID_COLUMN),
                    quote_identifier(TYPE_COLUMN),
                    quote_identifier(ID_COLUMN),
                );
            }
        }
        let mut predicate_bindings = Vec::with_capacity(predicates.len());
        for (index, (property, value)) in predicates.iter().enumerate() {
            let stored = self.model().column(object_type, property)?;
            if stored.column.encoding != ValueEncoding::Text {
                return Err(CellError::Engine(format!(
                    "{object_type}.{property} is not stored text"
                )));
            }
            let column = stored.column.column.as_ref().ok_or_else(|| {
                CellError::Engine(format!("{object_type}.{property} has no stored column"))
            })?;
            let parameter = index.saturating_add(3);
            #[expect(
                clippy::let_underscore_must_use,
                reason = "fmt::Write for String is infallible; its trait signature still returns Result"
            )]
            let _ = write!(
                status_filter,
                " AND (SELECT p.{} FROM {} AS p WHERE p.{} = r.{} AND p.{} = r.{}) = ?{parameter}",
                quote_identifier(column),
                quote_identifier(&stored.table.table),
                quote_identifier(WORLD_COLUMN),
                quote_identifier(WORLD_COLUMN),
                quote_identifier(ID_COLUMN),
                quote_identifier(ID_COLUMN),
            );
            predicate_bindings.push(SqlValue::from(*value));
        }
        if let Some(after) = after {
            let parameter = predicate_bindings.len().saturating_add(3);
            #[expect(
                clippy::let_underscore_must_use,
                reason = "fmt::Write for String is infallible; its trait signature still returns Result"
            )]
            let _ = write!(status_filter, " AND {id} > ?{parameter}");
            predicate_bindings.push(SqlValue::from(after));
        }
        let from = format!(
            "FROM {} AS r WHERE r.{} = ?1{status_filter} ORDER BY {id} LIMIT ?2",
            quote_identifier(&table.table),
            quote_identifier(WORLD_COLUMN)
        );
        // octet_length uses stored length metadata without fetching text/blob
        // contents. This first pass returns integers only, even for huge IDs.
        let lengths =
            format!("SELECT octet_length({id}) + octet_length({concrete}) + {headers} {from}");
        let safe_max_rows = usize::try_from(MAX_SAFE_INTEGER)
            .unwrap_or(usize::MAX)
            .saturating_sub(1);
        let cap = max_rows.min(budget.rows).min(safe_max_rows);
        let minimum_header_bytes = u64::try_from(headers).unwrap_or(u64::MAX);
        if cap == 0 || budget.remaining_bytes() < minimum_header_bytes {
            // Do not let an empty table appear complete when the requested
            // scan cannot admit one row even at its fixed header floor.
            return Ok((Vec::new(), false));
        }
        let fetch = i64::try_from(cap.saturating_add(1)).unwrap_or(MAX_SAFE_INTEGER);
        let mut admitted = 0_usize;
        let mut complete = true;
        let mut bindings = vec![SqlValue::from(world), SqlValue::from(fetch)];
        bindings.extend(predicate_bindings);
        let rows = self.conn.query(&lengths, &bindings)?;
        for values in rows {
            if admitted >= cap {
                complete = false;
                break;
            }
            let row = SqlRow(values);
            budget.rows = budget.rows.saturating_sub(1);
            let size = to_u64(row.get::<i64>(0)?, "scan metadata bytes")?;
            if !budget.charge(size) {
                complete = false;
                break;
            }
            admitted = admitted.saturating_add(1);
        }
        // The cell's single writer holds this view through both passes.
        // Fetch only charged rows, with no uncharged lookahead metadata.
        let sql = format!("SELECT {id}, {concrete} {from}");
        if let Some(limit) = bindings.get_mut(1) {
            *limit = SqlValue::from(i64::try_from(admitted).unwrap_or(MAX_SAFE_INTEGER));
        }
        let rows = self.conn.query(&sql, &bindings)?;
        let mut out = Vec::new();
        for values in rows {
            let row = SqlRow(values);
            out.push((row.get(0)?, row.get(1)?));
        }
        Ok((out, complete))
    }

    /// At most `limit` object ids of `object_type`'s root table in `world`,
    /// in id order, after `after`: bounded paging over one type (#2694).
    ///
    /// # Errors
    /// Unknown type; SQL engine errors.
    pub fn ids_after(
        &self,
        world: &str,
        object_type: &str,
        after: Option<&str>,
        limit: usize,
    ) -> Result<Vec<String>, CellError> {
        let table = self.model().table(self.root_type(object_type)?)?;
        let id = quote_identifier(ID_COLUMN);
        let sql = format!(
            "SELECT {id} FROM {} WHERE {} = ?1 AND {id} > ?2 ORDER BY {id} LIMIT ?3",
            quote_identifier(&table.table),
            quote_identifier(WORLD_COLUMN)
        );
        let limit = i64::try_from(limit)
            .unwrap_or(MAX_SAFE_INTEGER)
            .min(MAX_SAFE_INTEGER);
        self.conn
            .query(&sql, params![world, after.unwrap_or(""), limit])?
            .into_iter()
            .map(|values| SqlRow(values).get::<String>(0))
            .collect()
    }

    /// Read a page of object IDs whose stored byte length is at most
    /// `max_id_bytes`, charging their lengths before fetching the strings.
    /// IDs longer than the caller's legacy cutoff are skipped by SQL. The
    /// page is bounded by both `limit` and the remaining candidate row budget;
    /// it does not consume rows, because the caller charges each object read.
    /// The ID preflight charges at most 8 KiB and at most half the remaining
    /// byte budget, reserving the rest for projected reads.
    /// Returns whether the filtered ID stream is complete after this page.
    /// A byte-truncated page is incomplete so the caller can resume after its
    /// last returned ID on a fresh budget.
    ///
    /// # Errors
    /// Unknown type, SQL engine, or invalid stored ID lengths.
    pub fn ids_after_with_budget(
        &self,
        world: &str,
        object_type: &str,
        after: Option<&str>,
        limit: usize,
        max_id_bytes: usize,
        budget: &mut DecodeBudget,
    ) -> Result<(Vec<String>, bool), CellError> {
        const MAX_ID_PAGE_BYTES: u64 = 16 * 1_024;

        if limit == 0 {
            return Ok((Vec::new(), true));
        }
        let page_rows = limit.min(budget.rows);
        if page_rows == 0 {
            return Ok((Vec::new(), false));
        }
        let page_bytes = budget.remaining_bytes().min(MAX_ID_PAGE_BYTES) / 2;
        if page_bytes == 0 {
            return Ok((Vec::new(), false));
        }

        let table = self.model().table(self.root_type(object_type)?)?;
        let id = quote_identifier(ID_COLUMN);
        let table = quote_identifier(&table.table);
        let world_column = quote_identifier(WORLD_COLUMN);
        let max_id_bytes = i64::try_from(max_id_bytes)
            .unwrap_or(MAX_SAFE_INTEGER)
            .min(MAX_SAFE_INTEGER);
        let page_rows = i64::try_from(page_rows)
            .unwrap_or(MAX_SAFE_INTEGER)
            .min(MAX_SAFE_INTEGER);
        // This pass reads only integer length metadata; legacy IDs over the
        // fixed caller cutoff never become owned strings in this reader.
        let lengths = self.conn.query(
            &format!(
                "SELECT octet_length({id}) FROM {table} WHERE {world_column} = ?1 AND {id} > ?2 AND octet_length({id}) <= ?3 ORDER BY {id} LIMIT ?4"
            ),
            params![world, after.unwrap_or(""), max_id_bytes, page_rows],
        )?;

        let mut admitted = 0_usize;
        let mut admitted_bytes = 0_u64;
        let mut byte_truncated = false;
        for values in &lengths {
            let length = to_u64(SqlRow(values.clone()).get::<i64>(0)?, "object ID length")?;
            if admitted_bytes.saturating_add(length) > page_bytes {
                byte_truncated = true;
                break;
            }
            if !budget.charge(length) {
                byte_truncated = true;
                break;
            }
            admitted_bytes = admitted_bytes.saturating_add(length);
            admitted = admitted.saturating_add(1);
        }

        // Fetch only the IDs whose lengths were charged above, using the same
        // ordered predicate while the caller holds this committed cell view.
        let ids = if admitted == 0 {
            Vec::new()
        } else {
            self.conn
                .query(
                    &format!(
                        "SELECT {id} FROM {table} WHERE {world_column} = ?1 AND {id} > ?2 AND octet_length({id}) <= ?3 ORDER BY {id} LIMIT ?4"
                    ),
                    params![world, after.unwrap_or(""), max_id_bytes, i64::try_from(admitted).unwrap_or(MAX_SAFE_INTEGER)],
                )?
                .into_iter()
                .map(|values| SqlRow(values).get::<String>(0))
                .collect::<Result<Vec<_>, _>>()?
        };
        let complete =
            !byte_truncated && lengths.len() < usize::try_from(page_rows).unwrap_or(usize::MAX);
        Ok((ids, complete))
    }

    /// At most `limit` `(link id, source id)` pairs of every link ever
    /// written to `target` in `world`, through the link-version index (live
    /// or not; the caller checks liveness): the candidates of an inverse
    /// relation, without scanning source tables.
    ///
    /// # Errors
    /// SQL engine errors.
    pub fn link_sources_to(
        &self,
        world: &str,
        target: &str,
        limit: usize,
    ) -> Result<Vec<(String, String)>, CellError> {
        let limit = i64::try_from(limit)
            .unwrap_or(MAX_SAFE_INTEGER)
            .min(MAX_SAFE_INTEGER);
        let rows = self.conn.query(
            &format!(
                "SELECT \"link\", \"source\" FROM \"{LINK_VERSIONS}\" WHERE \"world\" = ?1 AND \"target\" = ?2 ORDER BY \"link_table\", \"source\" LIMIT ?3"
            ),
            params![world, target, limit],
        )?;
        rows.into_iter()
            .map(|values| {
                let row = SqlRow(values);
                Ok((row.get(0)?, row.get(1)?))
            })
            .collect()
    }

    /// Size metadata before `root_row` allocates concrete type and scope.
    fn object_metadata_size(
        &self,
        world: &str,
        object_type: &str,
        id: &str,
    ) -> Result<Option<u64>, CellError> {
        let table = self.model().table(self.root_type(object_type)?)?;
        let sql = format!(
            "SELECT octet_length({}), octet_length({}) FROM {} WHERE {} = ?1 AND {} = ?2",
            quote_identifier(TYPE_COLUMN),
            quote_identifier(SCOPE_COLUMN),
            quote_identifier(&table.table),
            quote_identifier(WORLD_COLUMN),
            quote_identifier(ID_COLUMN),
        );
        let sizes: Option<(i64, i64)> =
            self.conn.query_optional(&sql, params![world, id], |row| {
                Ok((row.get(0)?, row.get(1)?))
            })?;
        sizes
            .map(|(concrete_bytes, scope_bytes)| {
                Ok(u64::try_from(id.len())
                    .unwrap_or(u64::MAX)
                    .saturating_add(to_u64(concrete_bytes, "concrete type bytes")?)
                    .saturating_add(to_u64(scope_bytes, "scope bytes")?)
                    .saturating_add(64))
            })
            .transpose()
    }

    /// Read a candidate only while its stored input fits `budget`.
    /// Text/JSON/overflow bytes and fixed per-property overhead are charged
    /// before decoding, including inherited tables and nonmatching records.
    ///
    /// # Errors
    /// Unknown type, SQL engine, or decode errors.
    pub fn object_with_budget(
        &self,
        world: &str,
        object_type: &str,
        id: &str,
        budget: &mut DecodeBudget,
    ) -> Result<BudgetedRead, CellError> {
        budget.depend(Dependency::Object {
            object_type: object_type.to_owned(),
            key: id.to_owned(),
        })?;
        let Some(metadata) = self.object_metadata_size(world, object_type, id)? else {
            return Ok(BudgetedRead::Missing);
        };
        if !budget.charge(metadata) {
            return Ok(BudgetedRead::Exhausted);
        }
        let Some((concrete, scope, version)) = self.root_row(world, object_type, id)? else {
            return Ok(BudgetedRead::Missing);
        };
        if !self.model().is_a(&concrete, object_type) {
            return Ok(BudgetedRead::Missing);
        }
        let mut values = BTreeMap::new();
        for table in self.model().tables(&concrete)? {
            let mut columns: Vec<&str> = table
                .columns
                .iter()
                .filter_map(|column| column.column.as_deref())
                .collect();
            if table.has_overflow {
                columns.push(OVERFLOW_COLUMN);
            }
            // Charge property keys and a minimum cost per value, including nulls.
            let overhead = table.columns.iter().fold(0_u64, |total, column| {
                total
                    .saturating_add(
                        u64::try_from(column.property.as_str().len()).unwrap_or(u64::MAX),
                    )
                    .saturating_add(32)
            });
            if !budget.charge(overhead) {
                return Ok(BudgetedRead::Exhausted);
            }
            if columns.is_empty() {
                continue;
            }
            // SQLite octet_length(column) uses stored byte-length metadata;
            // it does not load large text/blob contents just to measure them.
            let projection = columns
                .iter()
                .map(|column| format!("COALESCE(octet_length({}), 8)", quote_identifier(column)))
                .collect::<Vec<_>>();
            let suffix = format!(
                "FROM {} WHERE {} = ?1 AND {} = ?2",
                quote_identifier(&table.table),
                quote_identifier(WORLD_COLUMN),
                quote_identifier(ID_COLUMN)
            );
            let row = query_expressions(
                self.conn,
                &table.table,
                &projection,
                &suffix,
                params![world, id],
            )?
            .into_iter()
            .next()
            .map(SqlRow)
            .ok_or_else(|| CellError::Engine("query returned no rows".to_owned()))?;
            let mut size = 0_u64;
            for index in 0..columns.len() {
                size =
                    size.saturating_add(u64::try_from(row.get::<i64>(index)?).unwrap_or(u64::MAX));
            }
            if !budget.charge(size) {
                return Ok(BudgetedRead::Exhausted);
            }
            // The cell lock holds this committed view throughout both reads.
            values.extend(self.read_row(
                table,
                world,
                id,
                ValueOwner::Object {
                    object_type: &concrete,
                },
            )?);
        }
        Ok(BudgetedRead::Found(ObjectRecord {
            object_type: concrete,
            id: id.to_owned(),
            scope,
            version,
            values,
        }))
    }

    /// Read only selected physical properties of one object within the
    /// candidate budget. Large unrelated properties are never fetched or
    /// decoded. This is used for indexes whose authorization evidence is a
    /// small, declared subset of a larger object.
    ///
    /// # Errors
    /// Unknown type/property, SQL engine, or decode errors.
    #[allow(
        clippy::too_many_lines,
        reason = "keeps one selected-field preflight, fetch, and decode path together"
    )]
    pub fn object_properties_with_budget(
        &self,
        world: &str,
        object_type: &str,
        id: &str,
        properties: &[&str],
        budget: &mut DecodeBudget,
    ) -> Result<BudgetedRead, CellError> {
        budget.depend(Dependency::Object {
            object_type: object_type.to_owned(),
            key: id.to_owned(),
        })?;
        let Some(metadata) = self.object_metadata_size(world, object_type, id)? else {
            return Ok(BudgetedRead::Missing);
        };
        if !budget.charge(metadata) || !charge_row(budget) {
            return Ok(BudgetedRead::Exhausted);
        }
        let Some((concrete, scope, version)) = self.root_row(world, object_type, id)? else {
            return Ok(BudgetedRead::Missing);
        };
        if !self.model().is_a(&concrete, object_type) {
            return Ok(BudgetedRead::Missing);
        }

        let mut values = BTreeMap::new();
        for table in self.model().tables(&concrete)? {
            let selected: Vec<_> = table
                .columns
                .iter()
                .filter(|column| properties.contains(&column.property.as_str()))
                .collect();
            if selected.is_empty() {
                continue;
            }
            let overhead = selected.iter().fold(0_u64, |total, column| {
                total
                    .saturating_add(
                        u64::try_from(column.property.as_str().len()).unwrap_or(u64::MAX),
                    )
                    .saturating_add(32)
            });
            if !budget.charge(overhead) {
                return Ok(BudgetedRead::Exhausted);
            }
            let columns: Vec<(&str, bool)> = selected
                .iter()
                .map(|column| {
                    let name = column
                        .column
                        .as_deref()
                        .ok_or_else(|| CellError::CorruptValue {
                            table: table.table.clone(),
                            column: column.property.as_str().to_owned(),
                        })?;
                    Ok((
                        name,
                        column.encoding == norn_projections::ValueEncoding::Integer,
                    ))
                })
                .collect::<Result<_, CellError>>()?;
            let sizes = columns
                .iter()
                .map(|(name, _)| format!("COALESCE(octet_length({}), 8)", quote_identifier(name)))
                .collect::<Vec<_>>();
            let suffix = format!(
                "FROM {} WHERE {} = ?1 AND {} = ?2",
                quote_identifier(&table.table),
                quote_identifier(WORLD_COLUMN),
                quote_identifier(ID_COLUMN),
            );
            let sizes =
                query_expressions(self.conn, &table.table, &sizes, &suffix, params![world, id])?
                    .into_iter()
                    .next()
                    .map(SqlRow)
                    .ok_or_else(|| CellError::Engine("query returned no rows".to_owned()))?;
            let mut size = 0_u64;
            for index in 0..columns.len() {
                size = size.saturating_add(to_u64(
                    sizes.get::<i64>(index)?,
                    "selected property length",
                )?);
            }
            if !budget.charge(size) {
                return Ok(BudgetedRead::Exhausted);
            }

            let row = query_expressions(
                self.conn,
                &table.table,
                &read_projection(&columns),
                &suffix,
                params![world, id],
            )?
            .into_iter()
            .next()
            .map(SqlRow)
            .ok_or_else(|| CellError::Engine("query returned no rows".to_owned()))?;
            for (index, column) in selected.iter().enumerate() {
                values.insert(
                    column.property.as_str().to_owned(),
                    decode_stored_value(
                        DecodeContext {
                            model: self.model(),
                            owner: ValueOwner::Object {
                                object_type: &concrete,
                            },
                        },
                        &table.table,
                        column,
                        row.get_ref(index)?,
                    )?,
                );
            }
        }
        for property in properties {
            if !values.contains_key(*property) {
                return Err(CellError::UnknownProperty {
                    object_type: object_type.to_owned(),
                    property: (*property).to_owned(),
                });
            }
        }
        Ok(BudgetedRead::Found(ObjectRecord {
            object_type: concrete,
            id: id.to_owned(),
            scope,
            version,
            values,
        }))
    }
}

#[cfg(all(test, feature = "native"))]
#[allow(
    clippy::expect_used,
    clippy::indexing_slicing,
    reason = "SQLite index and budget regressions use fixture assertions"
)]
mod binding_read_tests {
    use std::collections::BTreeMap;

    use norn_ir::RuntimeValue as Value;
    use serde_json::json;

    use crate::engine::{SqlEngine, SqlRow, SqlValue};
    use crate::reactive::{DecodeBudget, ReactiveLimits};
    use crate::{BindingIndexSpec, CellStore, CommitBatch, FactWriteRow, MAIN_WORLD, Model};

    fn model() -> Model {
        let types = [
            serde_json::from_value(json!({
                "name": "test.Customer",
                "properties": [{ "id": "email", "type": { "kind": "text" } }],
                "identity": [{ "name": "by_email", "properties": ["email"] }]
            }))
            .expect("customer definition"),
            serde_json::from_value(json!({
                "name": "test.KeyAccount",
                "extends": "test.Customer"
            }))
            .expect("key account definition"),
            serde_json::from_value(json!({
                "name": "test.Prospect",
                "extends": "test.Customer"
            }))
            .expect("prospect definition"),
            serde_json::from_value(json!({
                "name": "test.Order",
                "properties": [{ "id": "email", "type": { "kind": "text" } }]
            }))
            .expect("order definition"),
        ];
        Model::from_definitions_with_binding_indexes(
            &types,
            &[],
            &[],
            &[BindingIndexSpec {
                object_type: "test.Customer".to_owned(),
                property_id: "email".to_owned(),
                sort: true,
                filter: true,
            }],
        )
        .expect("model")
    }

    fn limits() -> ReactiveLimits {
        ReactiveLimits::from_profile(&norn_ir::ResourceLimitProfile::norn_limits_v1())
    }

    fn put(object_type: &str, key: &str, email: &str) -> FactWriteRow {
        FactWriteRow::Put {
            object_type: object_type.to_owned(),
            key: key.to_owned(),
            values: BTreeMap::from([("email".to_owned(), Value::from(email))]),
        }
    }

    fn put_optional_email(object_type: &str, key: &str, email: Option<&str>) -> FactWriteRow {
        FactWriteRow::Put {
            object_type: object_type.to_owned(),
            key: key.to_owned(),
            values: BTreeMap::from([("email".to_owned(), email.map_or(Value::Null, Value::from))]),
        }
    }

    fn seeded_store() -> CellStore {
        let mut store = CellStore::open_in_memory(model()).expect("store");
        store
            .commit(&CommitBatch {
                world: MAIN_WORLD.to_owned(),
                scope: "org_demo/dev".to_owned(),
                writes: vec![
                    put("test.Customer", "C-1", "one@example.test"),
                    put("test.KeyAccount", "C-2", "two@example.test"),
                    put("test.Prospect", "C-3", "three@example.test"),
                    put("test.Order", "O-1", "order@example.test"),
                ],
                ..CommitBatch::default()
            })
            .expect("seeds rows");
        store
    }

    fn sorted_customer_ids(store: &CellStore, descending: bool) -> Vec<String> {
        let mut after = None;
        let mut result = Vec::new();
        loop {
            let mut budget = DecodeBudget::new(&limits());
            let mut page_more = false;
            for _ in 0..2 {
                let candidate_bytes = budget.remaining_bytes();
                let candidate = store
                    .read()
                    .binding_sorted_candidate_after_with_budget(
                        MAIN_WORLD,
                        "test.Customer",
                        "email",
                        descending,
                        after.as_ref(),
                        &mut budget,
                        candidate_bytes,
                    )
                    .expect("sorted candidate read")
                    .expect("candidate fits request budget");
                let Some(position) = candidate.position else {
                    page_more = false;
                    break;
                };
                result.push(
                    candidate
                        .identity
                        .expect("fitting candidate has identity")
                        .0,
                );
                after = Some(position);
                page_more = candidate.has_more;
                if !page_more {
                    break;
                }
            }
            if !page_more {
                break;
            }
        }
        result
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "direction, null, tie and query-plan regressions share one fixture"
    )]
    fn binding_sorted_candidates_keep_direction_ties_and_nulls_last_across_pages() {
        // Equal sort values must not also be a declared unique identity.
        let mut definitions = model().definitions_snapshot();
        let customer = definitions
            .object_types
            .iter_mut()
            .find(|object| object.name.as_str() == "test.Customer")
            .expect("customer definition");
        customer.identity.clear();
        customer
            .properties
            .iter_mut()
            .find(|property| property.id.as_str() == "email")
            .expect("email property")
            .constraints
            .nullable = true;
        let sort_model = Model::from_snapshot(&definitions).expect("nonunique sort model");
        let mut store = CellStore::open_in_memory(sort_model).expect("store");
        store
            .commit(&CommitBatch {
                world: MAIN_WORLD.to_owned(),
                scope: "org_demo/dev".to_owned(),
                writes: vec![
                    put("test.Customer", "C-same-z", "same"),
                    put("test.KeyAccount", "C-same-a", "same"),
                    put("test.Prospect", "C-alpha", "alpha"),
                    put("test.Customer", "C-beta", "beta"),
                    put_optional_email("test.Customer", "C-null-z", None),
                    put_optional_email("test.KeyAccount", "C-null-a", None),
                ],
                ..CommitBatch::default()
            })
            .expect("sort rows seed");

        assert_eq!(
            sorted_customer_ids(&store, false),
            [
                "C-alpha".to_owned(),
                "C-beta".to_owned(),
                "C-same-a".to_owned(),
                "C-same-z".to_owned(),
                "C-null-a".to_owned(),
                "C-null-z".to_owned(),
            ]
        );
        assert_eq!(
            sorted_customer_ids(&store, true),
            [
                "C-same-a".to_owned(),
                "C-same-z".to_owned(),
                "C-beta".to_owned(),
                "C-alpha".to_owned(),
                "C-null-a".to_owned(),
                "C-null-z".to_owned(),
            ]
        );

        let reader = store.read();
        for (sql, bindings, index, range) in [
            (
                "EXPLAIN QUERY PLAN SELECT r.rowid, octet_length(r.\"_id\") + octet_length(r.\"_type\"), octet_length(r.\"email\") FROM \"o:test.Customer\" AS r INDEXED BY \"o:test.Customer/binding-property/email/asc\" WHERE r.\"_world\" = ?1 AND r.\"email\" IS NOT NULL AND (r.\"email\", r.\"_id\") > (?3, ?2) ORDER BY r.\"email\" ASC, r.\"_id\" ASC LIMIT ?4",
                vec![
                    SqlValue::from(MAIN_WORLD),
                    SqlValue::from("C-alpha"),
                    SqlValue::from("alpha"),
                    SqlValue::from(2_i64),
                ],
                "binding-property/email/asc",
                "(email,_id)>",
            ),
            (
                "EXPLAIN QUERY PLAN SELECT r.rowid, octet_length(r.\"_id\") + octet_length(r.\"_type\"), octet_length(r.\"email\") FROM \"o:test.Customer\" AS r INDEXED BY \"o:test.Customer/binding-property/email/desc\" WHERE r.\"_world\" = ?1 AND r.\"email\" = ?3 AND r.\"_id\" > ?2 ORDER BY r.\"_id\" ASC LIMIT ?4",
                vec![
                    SqlValue::from(MAIN_WORLD),
                    SqlValue::from("C-alpha"),
                    SqlValue::from("alpha"),
                    SqlValue::from(2_i64),
                ],
                "binding-property/email/desc",
                "_id>?",
            ),
        ] {
            let details = reader
                .conn
                .query(sql, &bindings)
                .expect("explains sorted query")
                .into_iter()
                .map(|row| SqlRow(row).get::<String>(3))
                .collect::<Result<Vec<_>, _>>()
                .expect("plan details");
            assert!(
                details.iter().any(|detail| {
                    detail.contains("COVERING INDEX")
                        && detail.contains(index)
                        && detail.contains(range)
                }),
                "cursor query must seek through its covering property index: {details:?}"
            );
            assert!(
                details.iter().all(|detail| !detail.contains("TEMP B-TREE")),
                "indexed keyset order must not sort candidates: {details:?}"
            );
        }
    }

    #[test]
    fn binding_sorted_boundary_snapshot_survives_delete_and_update() {
        let mut store = CellStore::open_in_memory(model()).expect("store");
        store
            .commit(&CommitBatch {
                world: MAIN_WORLD.to_owned(),
                scope: "org_demo/dev".to_owned(),
                writes: vec![
                    put("test.Customer", "C-anchor", "alpha"),
                    put("test.Customer", "C-next", "beta"),
                    put("test.Customer", "C-last", "omega"),
                ],
                ..CommitBatch::default()
            })
            .expect("boundary rows seed");
        let mut budget = DecodeBudget::new(&limits());
        let candidate_bytes = budget.remaining_bytes();
        let anchor = store
            .read()
            .binding_sorted_candidate_after_with_budget(
                MAIN_WORLD,
                "test.Customer",
                "email",
                false,
                None,
                &mut budget,
                candidate_bytes,
            )
            .expect("first boundary")
            .expect("candidate fits")
            .position
            .expect("first position");
        assert_eq!(anchor.identity, "C-anchor");

        store
            .commit(&CommitBatch {
                world: MAIN_WORLD.to_owned(),
                scope: "org_demo/dev".to_owned(),
                writes: vec![put("test.Customer", "C-anchor", "zulu")],
                ..CommitBatch::default()
            })
            .expect("moves the boundary row after the snapshot");
        let mut budget = DecodeBudget::new(&limits());
        let candidate_bytes = budget.remaining_bytes();
        let next = store
            .read()
            .binding_sorted_candidate_after_with_budget(
                MAIN_WORLD,
                "test.Customer",
                "email",
                false,
                Some(&anchor),
                &mut budget,
                candidate_bytes,
            )
            .expect("continue after changed boundary")
            .expect("candidate fits");
        assert_eq!(
            next.identity,
            Some(("C-next".to_owned(), "test.Customer".to_owned()))
        );
        let next_position = next.position.expect("next position");
        store
            .commit(&CommitBatch {
                world: MAIN_WORLD.to_owned(),
                scope: "org_demo/dev".to_owned(),
                writes: vec![FactWriteRow::Delete {
                    object_type: "test.Customer".to_owned(),
                    key: "C-next".to_owned(),
                }],
                ..CommitBatch::default()
            })
            .expect("deletes the next boundary row");
        let mut budget = DecodeBudget::new(&limits());
        let candidate_bytes = budget.remaining_bytes();
        let after_delete = store
            .read()
            .binding_sorted_candidate_after_with_budget(
                MAIN_WORLD,
                "test.Customer",
                "email",
                false,
                Some(&next_position),
                &mut budget,
                candidate_bytes,
            )
            .expect("continue after deleted boundary")
            .expect("candidate fits");
        assert_eq!(
            after_delete.identity,
            Some(("C-last".to_owned(), "test.Customer".to_owned()))
        );
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "locator, version and query-plan regressions share one end-to-end fixture"
    )]
    fn oversized_binding_identity_advances_by_a_persistent_indexed_locator() {
        let mut store = seeded_store();
        let initial_version = store
            .read()
            .version(MAIN_WORLD, "test.Customer", "C-1")
            .expect("initial high-water version");
        let mut budget = DecodeBudget::new(&limits());
        let first = store
            .read()
            .binding_candidate_after_with_budget(
                MAIN_WORLD,
                "test.Customer",
                None,
                None,
                &mut budget,
                64,
            )
            .expect("metadata read")
            .expect("candidate fits metadata budget");
        assert!(first.identity.is_none());
        assert!(first.has_more);
        assert_ne!(first.locator, 0);
        store
            .commit(&CommitBatch {
                world: MAIN_WORLD.to_owned(),
                scope: "org_demo/dev".to_owned(),
                writes: vec![put("test.Customer", "C-1", "updated@example.test")],
                ..CommitBatch::default()
            })
            .expect("updates anchor while preserving its high-water row");
        let updated_version = store
            .read()
            .version(MAIN_WORLD, "test.Customer", "C-1")
            .expect("updated high-water version");
        assert_eq!(updated_version, initial_version + 1);
        assert_eq!(
            store
                .read()
                .identity_locator(MAIN_WORLD, "test.Customer", "C-1")
                .expect("updated identity locator")
                .expect("updated high-water row"),
            first.locator,
            "updating the high-water version must preserve its rowid"
        );
        store
            .commit(&CommitBatch {
                world: MAIN_WORLD.to_owned(),
                scope: "org_demo/dev".to_owned(),
                writes: vec![FactWriteRow::Delete {
                    object_type: "test.Customer".to_owned(),
                    key: "C-1".to_owned(),
                }],
                ..CommitBatch::default()
            })
            .expect("deletes anchor while preserving its high-water row");
        let deleted_version = store
            .read()
            .version(MAIN_WORLD, "test.Customer", "C-1")
            .expect("deleted high-water version");
        assert_eq!(deleted_version, updated_version + 1);
        assert_eq!(
            store
                .read()
                .identity_locator(MAIN_WORLD, "test.Customer", "C-1")
                .expect("deleted identity locator")
                .expect("deleted high-water row"),
            first.locator,
            "deleting the anchor must preserve its rowid"
        );
        let bytes = budget.remaining_bytes();
        let next = store
            .read()
            .binding_candidate_after_with_budget(
                MAIN_WORLD,
                "test.Customer",
                None,
                Some(first.locator),
                &mut budget,
                bytes,
            )
            .expect("resume read")
            .expect("candidate fits budget");
        assert_eq!(
            next.identity,
            Some(("C-2".to_owned(), "test.KeyAccount".to_owned()))
        );

        store
            .commit(&CommitBatch {
                world: MAIN_WORLD.to_owned(),
                scope: "org_demo/dev".to_owned(),
                writes: vec![put("test.Customer", "C-1", "recreated@example.test")],
                ..CommitBatch::default()
            })
            .expect("recreates anchor without replacing its high-water row");
        let recreated_version = store
            .read()
            .version(MAIN_WORLD, "test.Customer", "C-1")
            .expect("recreated high-water version");
        assert_eq!(recreated_version, deleted_version + 1);
        assert_eq!(
            store
                .read()
                .identity_locator(MAIN_WORLD, "test.Customer", "C-1")
                .expect("recreated identity locator")
                .expect("recreated high-water row"),
            first.locator,
            "recreation must keep the original high-water rowid"
        );
        let mut recreated_budget = DecodeBudget::new(&limits());
        let recreated_bytes = recreated_budget.remaining_bytes();
        let after_recreate = store
            .read()
            .binding_candidate_after_with_budget(
                MAIN_WORLD,
                "test.Customer",
                None,
                Some(first.locator),
                &mut recreated_budget,
                recreated_bytes,
            )
            .expect("resume scan after recreation")
            .expect("candidate after recreation");
        assert_eq!(
            after_recreate.identity,
            Some(("C-2".to_owned(), "test.KeyAccount".to_owned())),
            "the retained locator must continue after the recreated key"
        );

        let reader = store.read();
        let plan = reader.conn.query(
            "EXPLAIN QUERY PLAN SELECT v.rowid, octet_length(r.\"_id\") + octet_length(r.\"_type\") FROM \"o:test.Customer\" AS r CROSS JOIN \"_norn_version\" AS v ON v.\"world\" = r.\"_world\" AND v.\"root_type\" = ?2 AND v.\"id\" = r.\"_id\" WHERE r.\"_world\" = ?1 AND r.\"_id\" > (SELECT \"id\" FROM \"_norn_version\" WHERE rowid = ?3 AND \"world\" = ?1 AND \"root_type\" = ?2) ORDER BY r.\"_id\" LIMIT 2",
            &[SqlValue::from(MAIN_WORLD), SqlValue::from("test.Customer"), SqlValue::from(i64::try_from(first.locator).expect("portable locator"))],
        ).expect("explains locator candidate read");
        let details = plan
            .into_iter()
            .map(|row| SqlRow(row).get::<String>(3))
            .collect::<Result<Vec<_>, _>>()
            .expect("plan details");
        assert!(
            details
                .iter()
                .any(|detail| detail.contains("SEARCH r") && detail.contains("INDEX")),
            "root must drive an indexed world/key range: {details:?}"
        );
        assert!(
            details
                .iter()
                .any(|detail| detail.contains("SEARCH v") && detail.contains("INDEX")),
            "each root must look up its unique version row: {details:?}"
        );
        assert!(
            details
                .iter()
                .all(|detail| !detail.contains("USE TEMP B-TREE")),
            "the bounded root scan must not sort a full version history: {details:?}"
        );
    }

    #[test]
    fn binding_rows_are_keyset_ordered_and_leave_sibling_filtering_to_caller() {
        let store = seeded_store();
        let mut budget = DecodeBudget::new(&limits());
        let (rows, complete) = store
            .read()
            .binding_rows_after_with_budget(MAIN_WORLD, "test.Customer", None, 10, &mut budget)
            .expect("customer scan");
        assert!(complete);
        assert_eq!(
            rows,
            [
                ("C-1".to_owned(), "test.Customer".to_owned()),
                ("C-2".to_owned(), "test.KeyAccount".to_owned()),
                ("C-3".to_owned(), "test.Prospect".to_owned()),
            ]
        );

        let model = store.read().model().clone();
        let mut subtype_budget = DecodeBudget::new(&limits());
        let (subtype_rows, subtype_complete) = store
            .read()
            .binding_rows_after_with_budget(
                MAIN_WORLD,
                "test.KeyAccount",
                None,
                10,
                &mut subtype_budget,
            )
            .expect("subtype scan");
        assert!(subtype_complete);
        assert_eq!(subtype_rows, rows, "the root stream includes siblings");
        let filtered: Vec<_> = subtype_rows
            .iter()
            .filter(|(_, concrete)| model.is_a(concrete, "test.KeyAccount"))
            .cloned()
            .collect();
        assert_eq!(filtered, [("C-2".to_owned(), "test.KeyAccount".to_owned())]);

        let mut after_budget = DecodeBudget::new(&limits());
        let (after_rows, after_complete) = store
            .read()
            .binding_rows_after_with_budget(
                MAIN_WORLD,
                "test.Customer",
                Some("C-1"),
                10,
                &mut after_budget,
            )
            .expect("continued scan");
        assert!(after_complete);
        assert_eq!(after_rows, rows[1..]);
    }

    #[test]
    fn binding_row_limit_and_decode_exhaustion_are_incomplete() {
        let store = seeded_store();
        let mut row_budget = DecodeBudget::new(&limits());
        let (page, complete) = store
            .read()
            .binding_rows_after_with_budget(MAIN_WORLD, "test.Customer", None, 2, &mut row_budget)
            .expect("bounded page");
        assert_eq!(page.len(), 2);
        assert!(!complete, "lookahead proves another matching row exists");

        let header = u64::try_from(std::mem::size_of::<(String, String)>()).expect("header");
        let bounded = ReactiveLimits {
            max_candidate_bytes_per_commit: header,
            max_scan: 2,
            ..limits()
        };
        let mut byte_budget = DecodeBudget::new(&bounded);
        let (rows, complete) = store
            .read()
            .binding_rows_after_with_budget(MAIN_WORLD, "test.Customer", None, 2, &mut byte_budget)
            .expect("metadata budget");
        assert!(
            rows.is_empty(),
            "identity text is not fetched before charge"
        );
        assert!(!complete);
        assert_eq!(byte_budget.remaining_bytes(), header);
    }

    #[test]
    fn exact_declared_identity_lookup_uses_its_index_and_encodes_typed_values() {
        let store = seeded_store();
        let key = [("email".to_owned(), Value::from("two@example.test"))];
        let mut budget = DecodeBudget::new(&limits());
        let (rows, complete) = store
            .read()
            .binding_key_rows_with_budget(
                MAIN_WORLD,
                "org_demo/dev",
                "test.Customer",
                &key,
                1,
                &mut budget,
            )
            .expect("identity lookup");
        assert!(complete);
        assert_eq!(rows, [("C-2".to_owned(), "test.KeyAccount".to_owned())]);

        let reader = store.read();
        let plan = reader
            .conn
            .query(
                "EXPLAIN QUERY PLAN SELECT k.\"_id\", r.\"_type\" FROM \"o:test.Customer\" AS k INDEXED BY \"o:test.Customer/key:by_email\" JOIN \"o:test.Customer\" AS r ON r.\"_world\" = k.\"_world\" AND r.\"_id\" = k.\"_id\" WHERE k.\"_scope\" = ?1 AND k.\"_world\" = ?2 AND k.\"email\" = ?3 AND r.\"_type\" IN ('test.Customer', 'test.KeyAccount', 'test.Prospect') LIMIT ?4",
                &[
                    SqlValue::from("org_demo/dev"),
                    SqlValue::from(MAIN_WORLD),
                    SqlValue::from("two@example.test"),
                    SqlValue::from(2_i64),
                ],
            )
            .expect("explains key lookup");
        let details: Vec<String> = plan
            .into_iter()
            .map(|row| SqlRow(row).get(3))
            .collect::<Result<_, _>>()
            .expect("plan details");
        assert!(
            details
                .iter()
                .any(|detail| { detail.contains("SEARCH k") && detail.contains("key:by_email") }),
            "identity lookup should search through its declared key index: {details:?}"
        );

        let root_plan = reader
            .conn
            .query(
                "EXPLAIN QUERY PLAN SELECT r.\"_id\", r.\"_type\" FROM \"o:test.Customer\" AS r WHERE r.\"_world\" = ?1 ORDER BY r.\"_id\" LIMIT ?2",
                &[SqlValue::from(MAIN_WORLD), SqlValue::from(2_i64)],
            )
            .expect("explains root scan");
        let root_details: Vec<String> = root_plan
            .into_iter()
            .map(|row| SqlRow(row).get(3))
            .collect::<Result<_, _>>()
            .expect("root plan details");
        assert!(
            root_details
                .iter()
                .any(|detail| detail.contains("INDEX") && detail.contains("autoindex")),
            "root scan should use the world/id primary index: {root_details:?}"
        );
        assert!(
            root_details
                .iter()
                .all(|detail| !detail.contains("USE TEMP B-TREE FOR ORDER BY")),
            "root scan order should come from its index: {root_details:?}"
        );

        let unsupported_key = [("unindexed".to_owned(), Value::from("value"))];
        let before_rows = budget.remaining_rows();
        let before_bytes = budget.remaining_bytes();
        let (fallback, fallback_complete) = store
            .read()
            .binding_key_rows_with_budget(
                MAIN_WORLD,
                "org_demo/dev",
                "test.Customer",
                &unsupported_key,
                1,
                &mut budget,
            )
            .expect("unsupported lookup falls back");
        assert!(fallback.is_empty());
        assert!(!fallback_complete);
        assert_eq!(budget.remaining_rows(), before_rows);
        assert_eq!(budget.remaining_bytes(), before_bytes);
    }
}
