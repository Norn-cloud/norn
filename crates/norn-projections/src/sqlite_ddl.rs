//! `EntityIR` v2 → SQLite DDL (T1-PROJ-2, #2529): the normalized object and
//! link tables of a tenant cell's store (ruling 2026-09-25 §6, §7).
//!
//! A pure, deterministic, one-way projection of a set of object types (and
//! the shared property types their `ref`s name) into `CREATE TABLE` and
//! `CREATE INDEX` statements in the **admitted SQLite subset**
//! ([`ADMITTED_SQLITE_SUBSET`]). `norn-cell` applies them to its local
//! SQLite file; the TypeScript cell shim will apply the same statements to
//! Durable Objects SQLite.
//!
//! # Shape
//!
//! - **One table per object type**, `"o:<type>"`. A root type's table holds
//!   the system columns [`WORLD_COLUMN`], [`ID_COLUMN`], [`TYPE_COLUMN`] (the
//!   concrete type of the object), [`SCOPE_COLUMN`], and [`VERSION_COLUMN`]
//!   (the owner version), then one column per stored property. Primary key
//!   `(_world, _id)`.
//! - **Inheritance is table-per-type with a parent foreign key.** A subtype's
//!   table holds `(_world, _id, _scope)` (the scope repeated for its scoped
//!   keys) and only the properties it adds; a narrowing override of an
//!   inherited property stays in the ancestor's column. The key references
//!   the supertype's table `ON DELETE CASCADE`, so deleting the root row
//!   deletes the object.
//! - **Keys.** Each declared [`KeySpec`] is a `UNIQUE` index over
//!   `(_scope, _world, <components in tuple order>)` on the declaring table.
//!   A key whose components are not all columns of the declaring table is
//!   rejected ([`SqliteDdlError::KeyNotLocal`]): uniqueness across tables is
//!   not expressible in the subset. Normalized matching (identity rules) is
//!   T8-ID-1's, not the index's.
//! - **Computed properties are excluded**: they are derived, never stored.
//! - **One table per link**, `"l:<source type>.<link id>"`: `(_world,
//!   _source, _target)` plus one column per link property, primary key
//!   `(_world, _source, _target)`. Both ends are indexed and reference their
//!   object tables `ON DELETE CASCADE` (the target only when it is an object
//!   type of the input; an interface target has no table). A `to: one` end is
//!   a `UNIQUE (_world, _source)` index, a `from: one` end a `UNIQUE (_world,
//!   _target)` index. `required` is a commit-time rule, not DDL.
//! - **Wide types** (ruling §6: ≤100 columns per table) keep their first
//!   properties as columns and the rest in one JSON object column,
//!   [`OVERFLOW_COLUMN`]. Key components must be columns.
//! - **Property columns never carry a raw property id.** The physical name
//!   of a stored property is [`property_column_name`]: the id behind the
//!   fixed [`PROPERTY_COLUMN_PREFIX`]. No system column and no SQLite rowid
//!   alias (`rowid`, `oid`, `_rowid_`) contains `:`, so a property can never
//!   shadow one, whatever its id. Overflow JSON keys stay the raw ids.
//!   [`project_logical_sqlite_ddl`] renders the same schema with property
//!   columns named by their raw ids; that text is the schema's persisted
//!   identity (DDL hash, export `ddlSql`), so physical naming changes no hash.
//!
//! Every value is stored in one of three SQLite storage classes, chosen by
//! [`ValueEncoding`]: exact decimals, dates, timestamps, durations, enums,
//! and text are `TEXT` (never `REAL`: there is no binary floating point),
//! integers and booleans are `INTEGER`, and `json-ext` is `TEXT` JSON.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::{self, Write as _};

use norn_ir::{
    KeySpec, LinkCardinality, LinkId, Multiplicity, ObjectTypeDef, ObjectTypeName, Property,
    PropertyId, PropertyKind, PropertyType, PropertyTypeDef, PropertyTypeName,
};

/// The admitted SQLite subset the projection emits (and the cell store's
/// own system tables use), as a human-readable contract.
///
/// Only: `CREATE TABLE IF NOT EXISTS` with `TEXT`/`INTEGER` columns,
/// `NOT NULL`, a composite `PRIMARY KEY`, and `FOREIGN KEY … ON DELETE
/// CASCADE`; `CREATE [UNIQUE] INDEX IF NOT EXISTS` over plain columns; quoted
/// identifiers. No `STRICT`, `WITHOUT ROWID`, `AUTOINCREMENT`, generated
/// columns, `CHECK`, triggers, views, virtual tables, extensions, or JSON
/// functions in DDL. Host limits (ruling §6): at most
/// [`MAX_COLUMNS_PER_TABLE`] columns per table and
/// [`MAX_STATEMENT_BYTES`] bytes per statement.
pub const ADMITTED_SQLITE_SUBSET: &str = "CREATE TABLE IF NOT EXISTS (TEXT/INTEGER, NOT NULL, \
composite PRIMARY KEY, FOREIGN KEY ON DELETE CASCADE); CREATE [UNIQUE] INDEX IF NOT EXISTS; \
quoted identifiers; <=100 columns per table; <=100 KB per statement";

/// Host limit: columns per table (ruling 2026-09-25 §6).
pub const MAX_COLUMNS_PER_TABLE: usize = 100;

/// Host limit: bytes per SQL statement (ruling 2026-09-25 §6).
pub const MAX_STATEMENT_BYTES: usize = 100_000;

/// Bound on `ref` chains and supertype chains (cycles are checker errors;
/// the projection fails closed instead of looping).
pub const MAX_CHAIN_DEPTH: usize = 32;

/// The world column (`main` or a fork id).
pub const WORLD_COLUMN: &str = "_world";
/// The object id column (the object's identity key value).
pub const ID_COLUMN: &str = "_id";
/// The concrete object type column (root tables only).
pub const TYPE_COLUMN: &str = "_type";
/// The tenant scope column (every object table; subtype tables repeat the
/// root row's scope for their scoped key indexes).
pub const SCOPE_COLUMN: &str = "_scope";
/// The owner version column (root tables only).
pub const VERSION_COLUMN: &str = "_version";
/// The JSON object column holding the properties of a wide type that do not
/// fit as columns.
pub const OVERFLOW_COLUMN: &str = "_overflow";
/// The source object id column of a link table.
pub const SOURCE_COLUMN: &str = "_source";
/// The target object id column of a link table.
pub const TARGET_COLUMN: &str = "_target";
/// The prefix of every property column ([`property_column_name`]).
pub const PROPERTY_COLUMN_PREFIX: &str = "p:";

/// The projection input: object types and the shared property types their
/// `ref`s resolve against.
#[derive(Debug, Clone, Copy)]
pub struct SqliteDdlInput<'a> {
    /// Every object type to project. Supertypes must be included.
    pub object_types: &'a [ObjectTypeDef],
    /// Shared property types (local and registry) `ref`s resolve against.
    pub property_types: &'a [PropertyTypeDef],
}

/// How one property value is stored.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ValueEncoding {
    /// A JSON string stored as `TEXT` (`text`, `enum`, `date`, `timestamp`,
    /// `duration`).
    Text,
    /// An exact decimal string stored verbatim as `TEXT`.
    Decimal,
    /// A signed 64-bit integer stored as `INTEGER`.
    Integer,
    /// `false`/`true` stored as `INTEGER` `0`/`1`.
    Boolean,
    /// Any JSON stored as `TEXT` JSON.
    Json,
}

impl ValueEncoding {
    /// The encoding of a resolved (non-`ref`) property kind.
    #[must_use]
    pub const fn of_kind(kind: PropertyKind) -> Option<Self> {
        match kind {
            PropertyKind::Text
            | PropertyKind::Enum
            | PropertyKind::Date
            | PropertyKind::Timestamp
            | PropertyKind::Duration => Some(Self::Text),
            PropertyKind::Decimal => Some(Self::Decimal),
            PropertyKind::Integer => Some(Self::Integer),
            PropertyKind::Boolean => Some(Self::Boolean),
            PropertyKind::Record | PropertyKind::JsonExt => Some(Self::Json),
            PropertyKind::Ref => None,
        }
    }

    /// The declared SQLite column type.
    #[must_use]
    pub const fn sql_type(self) -> &'static str {
        match self {
            Self::Integer | Self::Boolean => "INTEGER",
            Self::Text | Self::Decimal | Self::Json => "TEXT",
        }
    }
}

/// One stored property of a table.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PropertyColumn {
    /// The property id.
    pub property: PropertyId,
    /// The column name ([`property_column_name`]), or `None` when the
    /// property lives in the table's [`OVERFLOW_COLUMN`].
    pub column: Option<String>,
    /// The resolved kind (never `ref`).
    pub kind: PropertyKind,
    /// How values are stored.
    pub encoding: ValueEncoding,
    /// Whether the value may be `NULL`.
    pub nullable: bool,
}

/// The table of one object type.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObjectTable {
    /// The object type.
    pub object_type: ObjectTypeName,
    /// The table name (unquoted).
    pub table: String,
    /// The supertype, whose table this one's key references.
    pub parent: Option<ObjectTypeName>,
    /// The properties stored here, in declaration order.
    pub columns: Vec<PropertyColumn>,
    /// Whether the table has an [`OVERFLOW_COLUMN`].
    pub has_overflow: bool,
    /// `CREATE TABLE` then its `CREATE INDEX` statements.
    pub statements: Vec<String>,
}

impl ObjectTable {
    /// Whether this is a root table (no supertype): it carries the type and
    /// version columns.
    #[must_use]
    pub const fn is_root(&self) -> bool {
        self.parent.is_none()
    }
}

/// The table of one link.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinkTable {
    /// The declaring (source) object type.
    pub source: ObjectTypeName,
    /// The link id.
    pub link: LinkId,
    /// The target object type or interface.
    pub target: ObjectTypeName,
    /// The cardinality of both ends.
    pub cardinality: LinkCardinality,
    /// The table name (unquoted).
    pub table: String,
    /// The link properties, in declaration order.
    pub columns: Vec<PropertyColumn>,
    /// Whether the table has an [`OVERFLOW_COLUMN`].
    pub has_overflow: bool,
    /// `CREATE TABLE` then its `CREATE INDEX` statements.
    pub statements: Vec<String>,
}

/// The projected schema: object tables (supertypes before subtypes, then by
/// name) and link tables (by source type, then link id).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SqliteSchema {
    /// One per object type.
    pub object_tables: Vec<ObjectTable>,
    /// One per declared (not narrowing-override) link.
    pub link_tables: Vec<LinkTable>,
}

impl SqliteSchema {
    /// Every statement, in application order.
    #[must_use = "the statements are the schema"]
    pub fn statements(&self) -> impl Iterator<Item = &str> {
        self.object_tables
            .iter()
            .flat_map(|table| table.statements.iter())
            .chain(
                self.link_tables
                    .iter()
                    .flat_map(|table| table.statements.iter()),
            )
            .map(String::as_str)
    }

    /// The whole schema as one SQL script (`;\n`-terminated statements).
    #[must_use]
    pub fn to_sql(&self) -> String {
        let mut out = String::new();
        for statement in self.statements() {
            out.push_str(statement);
            out.push_str(";\n");
        }
        out
    }

    /// The table of `object_type`.
    #[must_use]
    pub fn object_table(&self, object_type: &str) -> Option<&ObjectTable> {
        self.object_tables
            .iter()
            .find(|table| table.object_type.as_str() == object_type)
    }

    /// The table of link `link` declared on `source`.
    #[must_use]
    pub fn link_table(&self, source: &str, link: &str) -> Option<&LinkTable> {
        self.link_tables
            .iter()
            .find(|table| table.source.as_str() == source && table.link.as_str() == link)
    }
}

/// Why a set of object types does not project.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SqliteDdlError {
    /// Two object types share a name.
    DuplicateObjectType(String),
    /// A supertype is not in the input.
    UnknownSupertype {
        /// The subtype.
        object_type: String,
        /// The missing supertype.
        supertype: String,
    },
    /// A supertype chain is cyclic or deeper than [`MAX_CHAIN_DEPTH`].
    SupertypeChain(String),
    /// A `ref` names no shared property type of the input.
    UnresolvedPropertyType {
        /// The owner (object type, or `<type>.<link>`).
        owner: String,
        /// The property.
        property: String,
        /// The unresolved name.
        name: String,
    },
    /// A `ref` chain is cyclic or deeper than [`MAX_CHAIN_DEPTH`].
    PropertyTypeChain(String),
    /// A key component is not a stored column of the declaring table.
    KeyNotLocal {
        /// The object type.
        object_type: String,
        /// The key.
        key: String,
        /// The component.
        property: String,
    },
    /// A table needs more system columns than the host limit allows.
    TooManyColumns(String),
    /// A statement exceeds [`MAX_STATEMENT_BYTES`].
    StatementTooLarge(String),
}

impl fmt::Display for SqliteDdlError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::DuplicateObjectType(name) => write!(f, "object type `{name}` is declared twice"),
            Self::UnknownSupertype {
                object_type,
                supertype,
            } => write!(
                f,
                "object type `{object_type}` extends `{supertype}`, which is not in the input"
            ),
            Self::SupertypeChain(name) => {
                write!(f, "supertype chain of `{name}` is cyclic or too deep")
            }
            Self::UnresolvedPropertyType {
                owner,
                property,
                name,
            } => write!(
                f,
                "property `{owner}.{property}` refs `{name}`, which is not in the input"
            ),
            Self::PropertyTypeChain(name) => {
                write!(f, "property type chain of `{name}` is cyclic or too deep")
            }
            Self::KeyNotLocal {
                object_type,
                key,
                property,
            } => write!(
                f,
                "key `{object_type}.{key}` component `{property}` is not a column of the declaring table"
            ),
            Self::TooManyColumns(table) => {
                write!(f, "table `{table}` exceeds {MAX_COLUMNS_PER_TABLE} columns")
            }
            Self::StatementTooLarge(table) => write!(
                f,
                "a statement of table `{table}` exceeds {MAX_STATEMENT_BYTES} bytes"
            ),
        }
    }
}

impl std::error::Error for SqliteDdlError {}

/// The table name of `object_type`.
#[must_use]
pub fn object_table_name(object_type: &str) -> String {
    format!("o:{object_type}")
}

/// The table name of link `link` declared on `source`.
#[must_use]
pub fn link_table_name(source: &str, link: &str) -> String {
    format!("l:{source}.{link}")
}

/// The physical column of property `property`: `p:<property id>`.
///
/// This is the only mapping from property ids to SQL column names. It is
/// injective, stable across store versions, and disjoint from every system
/// column and SQLite rowid alias, none of which contains `:`.
#[must_use]
pub fn property_column_name(property: &str) -> String {
    format!("{PROPERTY_COLUMN_PREFIX}{property}")
}

/// Quote an SQL identifier (`"` doubled).
#[must_use]
pub fn quote_identifier(name: &str) -> String {
    format!("\"{}\"", name.replace('"', "\"\""))
}

/// Project `input` into the SQLite DDL of its object and link tables.
///
/// # Errors
///
/// Returns the first [`SqliteDdlError`] in object-table order.
pub fn project_sqlite_ddl(input: &SqliteDdlInput<'_>) -> Result<SqliteSchema, SqliteDdlError> {
    project(input, property_column_name)
}

/// The identity rendering of [`project_sqlite_ddl`]: the same tables and
/// indexes with every property column named by its raw property id. Its
/// text is hashed and exported as the schema's identity. Never execute it.
///
/// # Errors
///
/// As [`project_sqlite_ddl`].
pub fn project_logical_sqlite_ddl(
    input: &SqliteDdlInput<'_>,
) -> Result<SqliteSchema, SqliteDdlError> {
    project(input, str::to_owned)
}

/// The column name of a property id: physical or logical.
type ColumnNaming = fn(&str) -> String;

fn project(
    input: &SqliteDdlInput<'_>,
    naming: ColumnNaming,
) -> Result<SqliteSchema, SqliteDdlError> {
    let mut by_name: BTreeMap<&str, &ObjectTypeDef> = BTreeMap::new();
    for def in input.object_types {
        if by_name.insert(def.name.as_str(), def).is_some() {
            return Err(SqliteDdlError::DuplicateObjectType(
                def.name.as_str().to_owned(),
            ));
        }
    }
    let property_types: BTreeMap<&str, &PropertyTypeDef> = input
        .property_types
        .iter()
        .map(|def| (def.name.as_str(), def))
        .collect();

    // Ancestors (nearest first) of every type; validates every chain.
    let mut ancestors: BTreeMap<&str, Vec<&ObjectTypeDef>> = BTreeMap::new();
    for def in by_name.values() {
        ancestors.insert(def.name.as_str(), ancestor_chain(def, &by_name)?);
    }

    // Supertypes first: order by depth, then by name.
    let mut order: Vec<(usize, &str)> = ancestors
        .iter()
        .map(|(name, chain)| (chain.len(), *name))
        .collect();
    order.sort_unstable();

    let mut schema = SqliteSchema::default();
    for (_, name) in &order {
        let Some(def) = by_name.get(name) else {
            continue;
        };
        let chain = ancestors.get(name).map_or(&[][..], Vec::as_slice);
        schema
            .object_tables
            .push(object_table(def, chain, &property_types, naming)?);
    }

    let mut links = Vec::new();
    for def in by_name.values() {
        let chain = ancestors
            .get(def.name.as_str())
            .map_or(&[][..], Vec::as_slice);
        let inherited: BTreeSet<&str> = chain
            .iter()
            .flat_map(|ancestor| ancestor.links.iter().map(|link| link.id.as_str()))
            .collect();
        for link in &def.links {
            if inherited.contains(link.id.as_str()) {
                continue;
            }
            links.push(link_table(def, link, &by_name, &property_types, naming)?);
        }
    }
    links.sort_by(|a, b| (&a.source, &a.link).cmp(&(&b.source, &b.link)));
    schema.link_tables = links;
    Ok(schema)
}

/// The supertypes of `def`, nearest first.
fn ancestor_chain<'a>(
    def: &'a ObjectTypeDef,
    by_name: &BTreeMap<&str, &'a ObjectTypeDef>,
) -> Result<Vec<&'a ObjectTypeDef>, SqliteDdlError> {
    let mut chain = Vec::new();
    let mut current = def;
    while let Some(parent) = &current.extends {
        let Some(parent_def) = by_name.get(parent.as_str()) else {
            return Err(SqliteDdlError::UnknownSupertype {
                object_type: current.name.as_str().to_owned(),
                supertype: parent.as_str().to_owned(),
            });
        };
        if chain.len() >= MAX_CHAIN_DEPTH || parent_def.name == def.name {
            return Err(SqliteDdlError::SupertypeChain(def.name.as_str().to_owned()));
        }
        chain.push(*parent_def);
        current = parent_def;
    }
    Ok(chain)
}

/// Resolve `property`'s kind and nullability through `ref` chains.
fn resolve(
    owner: &str,
    property: &Property,
    property_types: &BTreeMap<&str, &PropertyTypeDef>,
) -> Result<(PropertyKind, bool), SqliteDdlError> {
    let mut nullable = property.constraints.nullable;
    let mut current: &PropertyType = &property.property_type;
    let mut depth = 0_usize;
    loop {
        let PropertyType::Ref { name } = current else {
            return Ok((current.kind(), nullable));
        };
        let def = lookup(owner, property, name, property_types)?;
        nullable = nullable || def.constraints.nullable;
        current = &def.property_type;
        depth = depth.saturating_add(1);
        if depth > MAX_CHAIN_DEPTH {
            return Err(SqliteDdlError::PropertyTypeChain(name.as_str().to_owned()));
        }
    }
}

fn lookup<'a>(
    owner: &str,
    property: &Property,
    name: &PropertyTypeName,
    property_types: &BTreeMap<&str, &'a PropertyTypeDef>,
) -> Result<&'a PropertyTypeDef, SqliteDdlError> {
    property_types.get(name.as_str()).copied().ok_or_else(|| {
        SqliteDdlError::UnresolvedPropertyType {
            owner: owner.to_owned(),
            property: property.id.as_str().to_owned(),
            name: name.as_str().to_owned(),
        }
    })
}

/// The stored (non-computed) properties of `properties` not in `skip`, as
/// columns, overflowing past `budget`.
fn property_columns(
    owner: &str,
    properties: &[Property],
    skip: &BTreeSet<&str>,
    budget: usize,
    property_types: &BTreeMap<&str, &PropertyTypeDef>,
    naming: ColumnNaming,
) -> Result<(Vec<PropertyColumn>, bool), SqliteDdlError> {
    let stored: Vec<&Property> = properties
        .iter()
        .filter(|property| property.computed.is_none() && !skip.contains(property.id.as_str()))
        .collect();
    // With overflow, one column of the budget is the overflow column itself.
    let overflow = stored.len() > budget;
    let fits = if overflow {
        budget.saturating_sub(1)
    } else {
        budget
    };
    let mut columns = Vec::with_capacity(stored.len());
    for (index, property) in stored.into_iter().enumerate() {
        let (kind, nullable) = resolve(owner, property, property_types)?;
        let encoding = ValueEncoding::of_kind(kind)
            .ok_or_else(|| SqliteDdlError::PropertyTypeChain(property.id.as_str().to_owned()))?;
        columns.push(PropertyColumn {
            property: property.id.clone(),
            column: (index < fits).then(|| naming(property.id.as_str())),
            kind,
            encoding,
            nullable,
        });
    }
    Ok((columns, overflow))
}

fn column_definitions(out: &mut Vec<String>, columns: &[PropertyColumn], has_overflow: bool) {
    for column in columns {
        if let Some(name) = &column.column {
            let null = if column.nullable { "" } else { " NOT NULL" };
            out.push(format!(
                "{} {}{null}",
                quote_identifier(name),
                column.encoding.sql_type()
            ));
        }
    }
    if has_overflow {
        out.push(format!("{} TEXT", quote_identifier(OVERFLOW_COLUMN)));
    }
}

fn create_table(table: &str, definitions: &[String]) -> String {
    let mut out = format!("CREATE TABLE IF NOT EXISTS {} (", quote_identifier(table));
    for (index, definition) in definitions.iter().enumerate() {
        let separator = if index == 0 { "\n  " } else { ",\n  " };
        // Infallible: writing to a String.
        #[expect(
            clippy::let_underscore_must_use,
            reason = "fmt::Write for String is infallible; its trait signature still returns Result"
        )]
        let _ = write!(out, "{separator}{definition}");
    }
    out.push_str("\n)");
    out
}

fn create_index(unique: bool, index: &str, table: &str, columns: &[&str]) -> String {
    let unique = if unique { "UNIQUE " } else { "" };
    let columns: Vec<String> = columns
        .iter()
        .map(|column| quote_identifier(column))
        .collect();
    format!(
        "CREATE {unique}INDEX IF NOT EXISTS {} ON {} ({})",
        quote_identifier(index),
        quote_identifier(table),
        columns.join(", ")
    )
}

fn key_list(columns: &[&str]) -> String {
    columns
        .iter()
        .map(|column| quote_identifier(column))
        .collect::<Vec<_>>()
        .join(", ")
}

fn check_statements(table: &str, statements: &[String]) -> Result<(), SqliteDdlError> {
    if statements
        .iter()
        .any(|statement| statement.len() > MAX_STATEMENT_BYTES)
    {
        return Err(SqliteDdlError::StatementTooLarge(table.to_owned()));
    }
    Ok(())
}

fn object_table(
    def: &ObjectTypeDef,
    ancestors: &[&ObjectTypeDef],
    property_types: &BTreeMap<&str, &PropertyTypeDef>,
    naming: ColumnNaming,
) -> Result<ObjectTable, SqliteDdlError> {
    let name = def.name.as_str();
    let table = object_table_name(name);
    let parent = def.extends.clone();
    let root = parent.is_none();
    let system: &[&str] = if root {
        &[
            WORLD_COLUMN,
            ID_COLUMN,
            TYPE_COLUMN,
            SCOPE_COLUMN,
            VERSION_COLUMN,
        ]
    } else {
        &[WORLD_COLUMN, ID_COLUMN, SCOPE_COLUMN]
    };
    let budget = MAX_COLUMNS_PER_TABLE
        .checked_sub(system.len())
        .ok_or_else(|| SqliteDdlError::TooManyColumns(table.clone()))?;
    let inherited: BTreeSet<&str> = ancestors
        .iter()
        .flat_map(|ancestor| {
            ancestor
                .properties
                .iter()
                .map(|property| property.id.as_str())
        })
        .collect();
    let (columns, has_overflow) = property_columns(
        name,
        &def.properties,
        &inherited,
        budget,
        property_types,
        naming,
    )?;

    let mut definitions = Vec::new();
    for column in system {
        let sql_type = if *column == VERSION_COLUMN {
            "INTEGER"
        } else {
            "TEXT"
        };
        definitions.push(format!("{} {sql_type} NOT NULL", quote_identifier(column)));
    }
    column_definitions(&mut definitions, &columns, has_overflow);
    let pk = key_list(&[WORLD_COLUMN, ID_COLUMN]);
    definitions.push(format!("PRIMARY KEY ({pk})"));
    if let Some(parent) = &parent {
        definitions.push(format!(
            "FOREIGN KEY ({pk}) REFERENCES {} ({pk}) ON DELETE CASCADE",
            quote_identifier(&object_table_name(parent.as_str()))
        ));
    }
    let mut statements = vec![create_table(&table, &definitions)];
    for key in &def.identity {
        statements.push(key_index(name, &table, key, &columns)?);
    }
    check_statements(&table, &statements)?;
    Ok(ObjectTable {
        object_type: def.name.clone(),
        table,
        parent,
        columns,
        has_overflow,
        statements,
    })
}

fn key_index(
    object_type: &str,
    table: &str,
    key: &KeySpec,
    columns: &[PropertyColumn],
) -> Result<String, SqliteDdlError> {
    let mut index_columns: Vec<&str> = vec![SCOPE_COLUMN, WORLD_COLUMN];
    for component in &key.properties {
        let column = columns
            .iter()
            .find(|column| column.property == *component)
            .and_then(|column| column.column.as_deref())
            .ok_or_else(|| SqliteDdlError::KeyNotLocal {
                object_type: object_type.to_owned(),
                key: key.name.as_str().to_owned(),
                property: component.as_str().to_owned(),
            })?;
        index_columns.push(column);
    }
    Ok(create_index(
        true,
        &format!("{table}/key:{}", key.name.as_str()),
        table,
        &index_columns,
    ))
}

fn link_table(
    def: &ObjectTypeDef,
    link: &norn_ir::Link,
    by_name: &BTreeMap<&str, &ObjectTypeDef>,
    property_types: &BTreeMap<&str, &PropertyTypeDef>,
    naming: ColumnNaming,
) -> Result<LinkTable, SqliteDdlError> {
    let source = def.name.as_str();
    let table = link_table_name(source, link.id.as_str());
    let system = [WORLD_COLUMN, SOURCE_COLUMN, TARGET_COLUMN];
    let budget = MAX_COLUMNS_PER_TABLE
        .checked_sub(system.len())
        .ok_or_else(|| SqliteDdlError::TooManyColumns(table.clone()))?;
    let owner = format!("{source}.{}", link.id.as_str());
    let (columns, has_overflow) = property_columns(
        &owner,
        &link.properties,
        &BTreeSet::new(),
        budget,
        property_types,
        naming,
    )?;

    let mut definitions: Vec<String> = system
        .iter()
        .map(|column| format!("{} TEXT NOT NULL", quote_identifier(column)))
        .collect();
    column_definitions(&mut definitions, &columns, has_overflow);
    definitions.push(format!(
        "PRIMARY KEY ({})",
        key_list(&[WORLD_COLUMN, SOURCE_COLUMN, TARGET_COLUMN])
    ));
    let object_pk = key_list(&[WORLD_COLUMN, ID_COLUMN]);
    definitions.push(format!(
        "FOREIGN KEY ({}) REFERENCES {} ({object_pk}) ON DELETE CASCADE",
        key_list(&[WORLD_COLUMN, SOURCE_COLUMN]),
        quote_identifier(&object_table_name(source))
    ));
    if by_name.contains_key(link.target.as_str()) {
        definitions.push(format!(
            "FOREIGN KEY ({}) REFERENCES {} ({object_pk}) ON DELETE CASCADE",
            key_list(&[WORLD_COLUMN, TARGET_COLUMN]),
            quote_identifier(&object_table_name(link.target.as_str()))
        ));
    }
    let mut statements = vec![create_table(&table, &definitions)];
    statements.push(create_index(
        link.cardinality.from == Multiplicity::One,
        &format!("{table}/target"),
        &table,
        &[WORLD_COLUMN, TARGET_COLUMN],
    ));
    if link.cardinality.to == Multiplicity::One {
        statements.push(create_index(
            true,
            &format!("{table}/source"),
            &table,
            &[WORLD_COLUMN, SOURCE_COLUMN],
        ));
    }
    check_statements(&table, &statements)?;
    Ok(LinkTable {
        source: def.name.clone(),
        link: link.id.clone(),
        target: link.target.clone(),
        cardinality: link.cardinality,
        table,
        columns,
        has_overflow,
        statements,
    })
}
