//! Point object, identity, marking, and quantity reads.

use std::collections::{BTreeMap, BTreeSet};

use norn_ir::RuntimeValue as Value;
use norn_projections::ValueEncoding;
use norn_projections::sqlite_ddl::{
    ID_COLUMN, OVERFLOW_COLUMN, ObjectTable, SCOPE_COLUMN, TYPE_COLUMN, VERSION_COLUMN,
    WORLD_COLUMN, quote_identifier,
};
use serde::{Deserialize, Serialize};

use super::{StoreReader, decode_row, query_projected, to_i64, to_u64};
use crate::codec::{DecodeContext, ValueOwner};
use crate::engine::{SqlEngine, SqlRow, params};
use crate::error::CellError;
use crate::system::{
    MARKING_CURRENT, MARKING_OCCURRENCE, MARKING_SNAPSHOT, MARKING_SUBJECT, MARKING_VERSION,
    VERSIONS, marking_subject,
};

const MAX_MARKING_SUMMARY_VARIANTS: usize = 4_096;

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

/// Count the objects of one concrete type in `root_table` whose current version
/// has a current marking row for property `?3`. Each object resolves its marking
/// subject through the unique `(world, object_type, id)` index, then seeks that
/// subject's current row by the full primary key: no per-row correlated scan.
pub(super) fn marking_property_rows_sql(root_table: &str) -> String {
    format!(
        "SELECT COUNT(*) FROM {root} AS o \
         JOIN \"{MARKING_SUBJECT}\" AS ms ON ms.\"world\" = o.{world} AND ms.\"object_type\" = o.{ty} AND ms.\"id\" = o.{id} \
         JOIN \"{MARKING_CURRENT}\" AS m ON m.\"world\" = o.{world} AND m.\"object_type\" = o.{ty} AND m.\"subject\" = ms.\"subject\" AND m.\"property\" = ?3 AND m.\"version\" = o.{version} \
         WHERE o.{world} = ?1 AND o.{ty} = ?2",
        root = quote_identifier(root_table),
        world = quote_identifier(WORLD_COLUMN),
        ty = quote_identifier(TYPE_COLUMN),
        id = quote_identifier(ID_COLUMN),
        version = quote_identifier(VERSION_COLUMN),
    )
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
        self.check_live()?;
        crate::quantity::balance(self.conn, world, ledger, key)
    }

    /// Read the ledger ordering revision, independent of balance CAS.
    ///
    /// # Errors
    /// SQL or stored-version errors.
    pub fn quantity_version(&self, world: &str, ledger: &str) -> Result<u64, CellError> {
        self.check_live()?;
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
        self.check_live()?;
        crate::quantity::balance_versions(self.conn, world, request)
    }

    /// Verified persisted cell clock, including ordinary and definition commits.
    ///
    /// # Errors
    /// Corruption or storage errors.
    pub fn clock_floor(&self) -> Result<u64, CellError> {
        self.check_live()?;
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
        self.check_live()?;
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
        self.check_live()?;
        crate::quantity::posting(self.conn, world, ledger, id)
    }

    /// The root row of object `id` through `object_type`'s root table:
    /// concrete type, scope, version.
    pub(crate) fn root_row(
        &self,
        world: &str,
        object_type: &str,
        id: &str,
    ) -> Result<Option<(String, String, u64)>, CellError> {
        self.check_live()?;
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
        self.check_live()?;
        self.model
            .chain(object_type)?
            .first()
            .map(String::as_str)
            .ok_or_else(|| CellError::UnknownObjectType(object_type.to_owned()))
    }

    /// Resolve interface identities in the requested world; missing identities
    /// require a conservative lookup across verified indexed roots for that key.
    pub(crate) fn source_dispute_roots(
        &self,
        world: &str,
        object_type: &str,
        key: &str,
    ) -> Result<Option<Vec<String>>, CellError> {
        match self.root_type(object_type) {
            Ok(root) => return Ok(Some(vec![root.to_owned()])),
            Err(CellError::UnknownObjectType(_))
                if self
                    .model
                    .interfaces()
                    .any(|interface| interface.name.as_str() == object_type) => {}
            Err(error) => return Err(error),
        }
        let candidates = self
            .model
            .object_types()
            .filter(|definition| self.model.is_a(definition.name.as_str(), object_type))
            .map(|definition| self.root_type(definition.name.as_str()).map(str::to_owned))
            .collect::<Result<BTreeSet<_>, _>>()?;
        let mut roots = Vec::new();
        for root in candidates {
            if self
                .root_row(world, &root, key)?
                .is_some_and(|(concrete, _scope, _version)| self.model.is_a(&concrete, object_type))
            {
                roots.push(root);
            }
        }
        Ok((!roots.is_empty()).then_some(roots))
    }

    /// The highest version ever assigned to `id` in `object_type`'s
    /// hierarchy (`0` if none).
    pub(crate) fn high_water(
        &self,
        world: &str,
        object_type: &str,
        id: &str,
    ) -> Result<u64, CellError> {
        self.check_live()?;
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
        self.check_live()?;
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
        self.check_live()?;
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
        self.check_live()?;
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
        self.check_live()?;
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
            let property_rows: i64 = self
                .conn
                .query_optional(
                    &marking_property_rows_sql(&root.table),
                    params![world, concrete_type, property],
                    |row| row.get(0),
                )?
                .ok_or_else(|| {
                    CellError::Engine("current marking count query returned no row".to_owned())
                })?;
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

    /// Whether any version, current, past or deleted, of `object_type` or a
    /// subtype in `world` holds a label on `property` (S2, #1547). A question
    /// that reads history groups or filters on retained values, which the
    /// live [`Self::property_marking_summary`] does not see once a row is
    /// deleted or relabelled. One seek of the partial nonempty version index
    /// per concrete type, which `INDEXED BY` holds to: a plan that could not
    /// use it fails, never scans.
    ///
    /// # Errors
    /// Unknown type or a SQL-engine error.
    pub fn property_history_marked(
        &self,
        world: &str,
        object_type: &str,
        property: &str,
    ) -> Result<bool, CellError> {
        self.check_live()?;
        if self.model.tables(object_type)?.is_empty() {
            return Err(CellError::UnknownObjectType(object_type.to_owned()));
        }
        for definition in self.model.object_types() {
            let concrete_type = definition.name.as_str();
            if !self.model.is_a(concrete_type, object_type) {
                continue;
            }
            let held: Option<i64> = self.conn.query_optional(
                &format!(
                    "SELECT 1 FROM \"{MARKING_VERSION}\" INDEXED BY \"{}\" WHERE \"world\" = ?1 AND \"object_type\" = ?2 AND \"property\" = ?3 AND \"markings\" != '[]' LIMIT 1",
                    crate::system::MARKING_VERSION_NONEMPTY
                ),
                params![world, concrete_type, property],
                |row| row.get(0),
            )?;
            if held.is_some() {
                return Ok(true);
            }
        }
        Ok(false)
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

    /// Whether any object version, current or past, holds a marking label.
    /// A cell that holds none serves every field as itself whatever its
    /// catalog; one that holds any projects them, and a label its catalog
    /// does not resolve withholds its field (S2, S3, #1547). A declassified
    /// current label leaves the labels of the versions before it, so both
    /// tables count. One probe of each partial nonempty index, which
    /// `INDEXED BY` holds to: a plan that could not use it fails, never scans.
    ///
    /// # Errors
    /// SQL errors.
    pub fn stores_markings(&self) -> Result<bool, CellError> {
        self.check_live()?;
        for (table, index) in [
            (MARKING_CURRENT, crate::system::MARKING_CURRENT_NONEMPTY),
            (MARKING_VERSION, crate::system::MARKING_VERSION_NONEMPTY),
        ] {
            let held: Option<i64> = self.conn.query_optional(
                &format!(
                    "SELECT 1 FROM \"{table}\" INDEXED BY \"{index}\" WHERE \"markings\" != '[]' LIMIT 1"
                ),
                params![],
                |row| row.get(0),
            )?;
            if held.is_some() {
                return Ok(true);
            }
        }
        Ok(false)
    }
}
