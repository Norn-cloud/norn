//! D102: the tenant analytics engine. `DuckDB`, one database file per tenant
//! under the engine's own directory (ANALYTICS-TENANT-1), executing only the
//! typed operations of [`super`]. Statements are built from admitted names,
//! always quoted, and every value is bound; nothing a caller sends is SQL.
//!
//! Each tenant database holds the governed-fact mirror (`m:<root type>`,
//! every version with its commit interval, ANALYTICS-FRONTIER-1), loaded
//! datasets (`d:<type>`, one load at a time, rows numbered from their file,
//! BULK-LINEAGE-1) and their quarantines (`q:<type>`, BULK-LOAD-1).

use std::collections::BTreeMap;
use std::io::Read as _;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Instant;

use duckdb::types::Value as Sql;
use duckdb::{Connection, params_from_iter};
use transparency_sha2::{Digest as _, Sha256};

use super::{
    BucketTotals, Column, ColumnKind, ColumnProfile, EngineRefusal, EvaluateRequest,
    EvaluateResponse, ExportFormat, ExportRequest, FileFormat, Filter, FrontierRequest,
    FrontierResponse, GroupKey, Literal, LoadRequest, LoadResponse, Lowered, MemberRow,
    MembersRequest, MembersResponse, MembershipDigest, MirrorRow, ProfileRequest, ProfileResponse,
    Source, SyncRequest, TableSchema, TenantId, Test, Value, hex,
};

/// An engine outcome: a typed refusal the cell maps to its own answer.
pub type Outcome<T> = Result<T, EngineRefusal>;

fn refusal(code: &str, detail: impl Into<String>) -> EngineRefusal {
    EngineRefusal {
        code: code.to_owned(),
        detail: detail.into(),
    }
}

fn db<T>(value: duckdb::Result<T>) -> Outcome<T> {
    value.map_err(|error| refusal("core.analytics.engine", error.to_string()))
}

const UNSUPPORTED: &str = "core.compute.unsupported_engine";
const STALE: &str = "core.compute.stale_input";
const MAX_VALUE: &str = "9000000000000000000";

/// Engine settings: where tenant files live and what `DuckDB` may use.
#[derive(Debug, Clone)]
pub struct Settings {
    /// The directory holding one `<tenant>.duckdb` per tenant and `tmp/`.
    pub root: PathBuf,
    /// The `DuckDB` `memory_limit`, for example `512MB`.
    pub memory_limit: String,
    /// `DuckDB` worker threads.
    pub threads: u32,
}

/// The engine: lazily opened tenant databases, each used by one operation at a time.
pub struct Engine {
    settings: Settings,
    tenants: Mutex<BTreeMap<TenantId, Arc<Mutex<Connection>>>>,
}

fn ident(name: &str) -> String {
    format!("\"{}\"", name.replace('"', "\"\""))
}

fn derived(column: &str, suffix: &str) -> String {
    ident(&format!("{column}:{suffix}"))
}

fn mirror_table(name: &str) -> String {
    ident(&format!("m:{name}"))
}

fn dataset_table(name: &str) -> String {
    ident(&format!("d:{name}"))
}

fn quarantine_table(name: &str) -> String {
    ident(&format!("q:{name}"))
}

const fn sql_type(kind: ColumnKind) -> &'static str {
    match kind {
        ColumnKind::Integer => "DECIMAL(38,0)",
        ColumnKind::Boolean => "BOOLEAN",
        ColumnKind::Decimal { .. } => "DECIMAL",
        ColumnKind::Text | ColumnKind::Timestamp | ColumnKind::Date | ColumnKind::Opaque => {
            "VARCHAR"
        }
    }
}

fn column_ddl(column: &Column) -> String {
    let base = match column.kind {
        ColumnKind::Decimal { scale } => format!("{} DECIMAL(38,{scale})", ident(&column.id)),
        kind @ (ColumnKind::Text
        | ColumnKind::Integer
        | ColumnKind::Boolean
        | ColumnKind::Timestamp
        | ColumnKind::Date
        | ColumnKind::Opaque) => format!("{} {}", ident(&column.id), sql_type(kind)),
    };
    match column.kind {
        ColumnKind::Timestamp | ColumnKind::Date => format!(
            "{base}, {} BIGINT, {} INTEGER",
            derived(&column.id, "t"),
            derived(&column.id, "n")
        ),
        ColumnKind::Text
        | ColumnKind::Integer
        | ColumnKind::Decimal { .. }
        | ColumnKind::Boolean
        | ColumnKind::Opaque => base,
    }
}

/// The exact cast of a validated text value into its column's storage.
fn cast(kind: ColumnKind, text: &str) -> String {
    match kind {
        ColumnKind::Integer => format!("CAST({text} AS DECIMAL(38,0))"),
        ColumnKind::Decimal { scale } => format!("CAST({text} AS DECIMAL(38,{scale}))"),
        ColumnKind::Boolean => format!("(lower({text}) = 'true')"),
        ColumnKind::Text | ColumnKind::Timestamp | ColumnKind::Date | ColumnKind::Opaque => {
            text.to_owned()
        }
    }
}

impl Engine {
    /// An engine over `settings.root`, creating it and its `tmp/` directory.
    ///
    /// # Errors
    /// The directories cannot be created.
    pub fn new(settings: Settings) -> std::io::Result<Self> {
        std::fs::create_dir_all(settings.root.join("tmp"))?;
        Ok(Self {
            settings,
            tenants: Mutex::new(BTreeMap::new()),
        })
    }

    /// The scratch directory loads and exports use.
    #[must_use]
    pub fn scratch(&self) -> PathBuf {
        self.settings.root.join("tmp")
    }

    fn tenant(&self, tenant: &TenantId) -> Outcome<Arc<Mutex<Connection>>> {
        let mut tenants = self.tenants.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(open) = tenants.get(tenant) {
            return Ok(Arc::clone(open));
        }
        let path = self.settings.root.join(format!("{}.duckdb", tenant.as_str()));
        let conn = db(Connection::open(&path))?;
        let scratch = self.scratch();
        let scratch = scratch.to_string_lossy().replace('\'', "''");
        db(conn.execute_batch(&format!(
            "SET autoinstall_known_extensions=false; SET autoload_known_extensions=false; \
             SET memory_limit='{}'; SET threads={}; SET temp_directory='{scratch}'; \
             SET preserve_insertion_order=true; SET allowed_directories=['{scratch}']; \
             SET enable_external_access=false; \
             CREATE TABLE IF NOT EXISTS \":meta\" (key VARCHAR PRIMARY KEY, value VARCHAR NOT NULL);",
            self.settings.memory_limit.replace('\'', ""),
            self.settings.threads.max(1),
        )))?;
        let conn = Arc::new(Mutex::new(conn));
        tenants.insert(tenant.clone(), Arc::clone(&conn));
        Ok(conn)
    }

    fn with<T>(&self, tenant: &TenantId, f: impl FnOnce(&Connection) -> Outcome<T>) -> Outcome<T> {
        let conn = self.tenant(tenant)?;
        let conn = conn.lock().unwrap_or_else(PoisonError::into_inner);
        f(&conn)
    }

    /// ANALYTICS-FRONTIER-1: the mirror's frontier for a world. A table whose
    /// pin changed (or is new) clears the whole mirror, which then syncs from
    /// the first commit again.
    ///
    /// # Errors
    /// An engine failure.
    pub fn frontier(&self, request: &FrontierRequest) -> Outcome<FrontierResponse> {
        self.with(&request.tenant, |conn| {
            let mut stale = false;
            for table in &request.tables {
                stale |= meta(conn, &format!("pin:{}", table.name))?.as_deref() != Some(&table.pin);
            }
            if stale {
                reset_mirror(conn, &request.tables)?;
            }
            let frontier = meta(conn, &format!("frontier:{}", request.world))?
                .and_then(|v| v.parse().ok())
                .unwrap_or(0);
            Ok(FrontierResponse { frontier })
        })
    }

    /// Apply one sync batch in a single transaction.
    ///
    /// # Errors
    /// `core.analytics.frontier_moved` when the batch does not continue the
    /// mirror's frontier, or an unknown table.
    pub fn sync(&self, request: &SyncRequest) -> Outcome<FrontierResponse> {
        self.with(&request.tenant, |conn| {
            let key = format!("frontier:{}", request.world);
            let current: u64 = meta(conn, &key)?.and_then(|v| v.parse().ok()).unwrap_or(0);
            if current != request.after || request.through < request.after {
                return Err(refusal(
                    "core.analytics.frontier_moved",
                    format!("the mirror is at {current}"),
                ));
            }
            db(conn.execute_batch("BEGIN TRANSACTION"))?;
            let applied = apply_sync(conn, request).and_then(|()| {
                set_meta(conn, &key, &request.through.to_string())
            });
            match applied {
                Ok(()) => db(conn.execute_batch("COMMIT"))?,
                Err(error) => {
                    let _rolled_back = conn.execute_batch("ROLLBACK");
                    return Err(error);
                }
            }
            Ok(FrontierResponse {
                frontier: request.through,
            })
        })
    }

    /// QUERY-ENGINE-1: evaluate a lowered question exactly.
    ///
    /// # Errors
    /// The Core 03 refusals the exact engine would give, a frontier the
    /// mirror has not reached (`core.analytics.frontier_behind`), or a stale load.
    pub fn evaluate(&self, request: &EvaluateRequest) -> Outcome<EvaluateResponse> {
        self.with(&request.tenant, |conn| {
            let scope = Scope::new(conn, &request.source)?;
            let population: u64 = scope.count(conn, &Frag::raw("TRUE"))?;
            if let Source::Mirror { scope: cell, .. } = &request.source {
                let other = Frag::bind("\":scope\" <> ?", Sql::Text(cell.clone()));
                if scope.count(conn, &other)? > 0 {
                    return Err(refusal(STALE, "a row of another scope"));
                }
                if scope.count(conn, &Frag::raw(ident(":bad")))? > 0 {
                    return Err(refusal(UNSUPPORTED, "a stored value failed its check"));
                }
            }
            let excluded = scope.excluded_present(conn)?;
            let projection = Projection::build(&request.query, &scope)?;
            let refused: u64 = projection.count(conn, "refuse")?;
            if refused > 0 {
                return Err(refusal(UNSUPPORTED, "a row is outside the bounded profile"));
            }
            let inline = population <= request.inline_members;
            let buckets = projection.buckets(conn, inline)?;
            Ok(EvaluateResponse {
                population,
                excluded,
                buckets,
            })
        })
    }

    /// Page one bucket's members in identity order.
    ///
    /// # Errors
    /// As [`Self::evaluate`].
    pub fn members(&self, request: &MembersRequest) -> Outcome<MembersResponse> {
        self.with(&request.tenant, |conn| {
            let scope = Scope::new(conn, &request.source)?;
            let projection = Projection::build(&request.query, &scope)?;
            let members = projection.page(conn, &request.group, request.after.as_deref(), request.limit)?;
            Ok(MembersResponse { members })
        })
    }

    /// BULK-LOAD-1: load `file` (already downloaded) as the dataset's only
    /// load. The digest must be the recorded one; every row is typed by its
    /// column, and a row that is not (or misses a required value) is
    /// quarantined with the first reason, in column order.
    ///
    /// # Errors
    /// `core.bulk.digest_mismatch`, an unreadable file, or an engine failure.
    pub fn load(&self, request: &LoadRequest, file: &Path) -> Outcome<LoadResponse> {
        let started = Instant::now();
        let (digest, bytes) = sha256_file(file)
            .map_err(|error| refusal("core.bulk.unreadable", error.to_string()))?;
        if digest != request.file.sha256 {
            return Err(refusal("core.bulk.digest_mismatch", "the file is not the recorded upload"));
        }
        self.with(&request.tenant, |conn| {
            db(conn.execute_batch("BEGIN TRANSACTION"))?;
            let loaded = load_file(conn, request, file);
            match loaded {
                Ok((rows, reasons)) => {
                    db(conn.execute_batch("COMMIT"))?;
                    let rejected = reasons.values().sum();
                    Ok(LoadResponse {
                        load: request.load.clone(),
                        rows,
                        rejected,
                        reasons,
                        bytes,
                        millis: u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
                    })
                }
                Err(error) => {
                    let _rolled_back = conn.execute_batch("ROLLBACK");
                    Err(error)
                }
            }
        })
    }

    /// Profile columns of a source: counts, distinct values, extremes and
    /// nearest-rank quantiles, all exact.
    ///
    /// # Errors
    /// An unknown column or a stale source.
    pub fn profile(&self, request: &ProfileRequest) -> Outcome<ProfileResponse> {
        self.with(&request.tenant, |conn| {
            let scope = Scope::new(conn, &request.source)?;
            let rows = query_u64(
                conn,
                &Frag::fmt(&format!("SELECT count(*) FROM {} WHERE {{}}", scope.table), &[&scope.population]),
            )?;
            let columns = request
                .columns
                .iter()
                .map(|id| profile_column(conn, &scope, id, rows))
                .collect::<Outcome<_>>()?;
            Ok(ProfileResponse { rows, columns })
        })
    }

    /// Export columns of every row of a source into a scratch file the
    /// caller streams and then deletes.
    ///
    /// # Errors
    /// An unknown column or a stale source.
    pub fn export(&self, request: &ExportRequest) -> Outcome<PathBuf> {
        let name = format!(
            "export-{}.{}",
            hex(&Sha256::digest(format!("{:?}{:?}", request, Instant::now()).as_bytes())),
            match request.format {
                ExportFormat::Parquet => "parquet",
                ExportFormat::Jsonl => "jsonl",
            }
        );
        let path = self.scratch().join(name);
        self.with(&request.tenant, |conn| {
            let scope = Scope::new(conn, &request.source)?;
            let mut select = vec![
                format!("{} AS id", ident(":id")),
                format!("{} AS version", ident(":v")),
            ];
            for id in &request.columns {
                let column = scope.column(id)?;
                let value = match (request.format, column.kind) {
                    (ExportFormat::Jsonl, ColumnKind::Integer | ColumnKind::Decimal { .. }) => {
                        format!("CAST({} AS VARCHAR)", ident(id))
                    }
                    _ => ident(id),
                };
                select.push(format!("{value} AS {}", ident(id)));
            }
            let format = match request.format {
                ExportFormat::Parquet => "FORMAT parquet",
                ExportFormat::Jsonl => "FORMAT json",
            };
            let target = path.to_string_lossy().replace('\'', "''");
            let rows = Frag::fmt(
                &format!(
                    "CREATE OR REPLACE TEMP TABLE \":export\" AS SELECT {} FROM {} WHERE {{}} ORDER BY {}",
                    select.join(", "),
                    scope.table,
                    ident(":id"),
                ),
                &[&scope.population],
            );
            db(conn.prepare(&rows.text)).and_then(|mut statement| {
                db(statement.execute(params_from_iter(rows.params)))
            })?;
            db(conn.execute_batch(&format!(
                "COPY \":export\" TO '{target}' ({format}); DROP TABLE \":export\";"
            )))?;
            Ok(())
        })?;
        Ok(path)
    }
}

fn meta(conn: &Connection, key: &str) -> Outcome<Option<String>> {
    let mut statement = db(conn.prepare("SELECT value FROM \":meta\" WHERE key = ?"))?;
    let mut rows = db(statement.query([key]))?;
    match db(rows.next())? {
        Some(row) => Ok(Some(db(row.get(0))?)),
        None => Ok(None),
    }
}

fn set_meta(conn: &Connection, key: &str, value: &str) -> Outcome<()> {
    db(conn.execute(
        "INSERT OR REPLACE INTO \":meta\" VALUES (?, ?)",
        [key, value],
    ))?;
    Ok(())
}

fn reset_mirror(conn: &Connection, tables: &[TableSchema]) -> Outcome<()> {
    db(conn.execute_batch("BEGIN TRANSACTION"))?;
    let reset = (|| {
        let mut statement = db(conn.prepare(
            "SELECT table_name FROM information_schema.tables WHERE table_name LIKE 'm:%'",
        ))?;
        let names: Vec<String> = db(db(statement.query_map([], |row| row.get(0)))?
            .collect::<Result<_, _>>())?;
        for name in names {
            db(conn.execute_batch(&format!("DROP TABLE {}", ident(&name))))?;
        }
        db(conn.execute_batch(
            "DELETE FROM \":meta\" WHERE key LIKE 'frontier:%' OR key LIKE 'pin:%'",
        ))?;
        for table in tables {
            let columns: Vec<String> = table.columns.iter().map(column_ddl).collect();
            let system = [":w VARCHAR", ":id VARCHAR", ":v UBIGINT", ":from UBIGINT", ":to UBIGINT", ":scope VARCHAR", ":type VARCHAR", ":bad BOOLEAN"]
                .iter()
                .map(|c| {
                    let (name, ty) = c.split_once(' ').unwrap_or((c, "VARCHAR"));
                    format!("{} {ty}", ident(name))
                })
                .collect::<Vec<_>>();
            db(conn.execute_batch(&format!(
                "CREATE TABLE {} ({}{}{})",
                mirror_table(&table.name),
                system.join(", "),
                if columns.is_empty() { "" } else { ", " },
                columns.join(", ")
            )))?;
            set_meta(conn, &format!("pin:{}", table.name), &table.pin)?;
            set_meta(conn, &format!("schema:{}", table.name), &schema_text(table)?)?;
        }
        Ok(())
    })();
    match reset {
        Ok(()) => db(conn.execute_batch("COMMIT")),
        Err(error) => {
            let _rolled_back = conn.execute_batch("ROLLBACK");
            Err(error)
        }
    }
}

fn schema_text(table: &TableSchema) -> Outcome<String> {
    serde_json::to_string(table).map_err(|e| refusal("core.analytics.engine", e.to_string()))
}

fn schema_of(conn: &Connection, key: &str) -> Outcome<TableSchema> {
    let text = meta(conn, key)?.ok_or_else(|| refusal(UNSUPPORTED, "no such table"))?;
    serde_json::from_str(&text).map_err(|e| refusal("core.analytics.engine", e.to_string()))
}

/// Encode one stored value for the all-text staging table.
fn stage(value: &Value) -> (Sql, Sql, Sql) {
    match value {
        Value::Null => (Sql::Null, Sql::Null, Sql::Null),
        Value::Bool(flag) => (Sql::Text(flag.to_string()), Sql::Null, Sql::Null),
        Value::Number(text) | Value::Text(text) | Value::Json(text) => {
            (Sql::Text(text.clone()), Sql::Null, Sql::Null)
        }
        Value::Instant { text, secs, nanos } => (
            Sql::Text(text.clone()),
            Sql::BigInt(*secs),
            Sql::Int(i32::try_from(*nanos).unwrap_or(i32::MAX)),
        ),
    }
}

fn apply_sync(conn: &Connection, request: &SyncRequest) -> Outcome<()> {
    let mut by_table: BTreeMap<&str, Vec<&MirrorRow>> = BTreeMap::new();
    for row in &request.rows {
        if row.commit_seq <= request.after || row.commit_seq > request.through {
            return Err(refusal("core.analytics.frontier_moved", "a row outside the batch"));
        }
        by_table.entry(row.table.as_str()).or_default().push(row);
    }
    for (name, rows) in by_table {
        let table = schema_of(conn, &format!("schema:{name}"))?;
        let mut stage_columns = vec![
            format!("{} UBIGINT", ident(":ord")),
            format!("{} VARCHAR", ident(":id")),
            format!("{} UBIGINT", ident(":v")),
            format!("{} UBIGINT", ident(":from")),
            format!("{} VARCHAR", ident(":scope")),
            format!("{} VARCHAR", ident(":type")),
            format!("{} BOOLEAN", ident(":bad")),
            format!("{} BOOLEAN", ident(":tomb")),
        ];
        for column in &table.columns {
            stage_columns.push(format!("{} VARCHAR", ident(&column.id)));
            stage_columns.push(format!("{} BIGINT", derived(&column.id, "t")));
            stage_columns.push(format!("{} INTEGER", derived(&column.id, "n")));
        }
        db(conn.execute_batch(&format!(
            "CREATE OR REPLACE TABLE \":stage\" ({})",
            stage_columns.join(", ")
        )))?;
        {
            let mut appender = db(conn.appender(":stage"))?;
            for (ord, row) in rows.iter().enumerate() {
                let tomb = row.values.is_none();
                if row.values.as_ref().is_some_and(|v| v.len() != table.columns.len()) {
                    return Err(refusal("core.analytics.engine", "values do not match the table"));
                }
                let mut values = vec![
                    Sql::UBigInt(u64::try_from(ord).unwrap_or(u64::MAX)),
                    Sql::Text(row.id.clone()),
                    Sql::UBigInt(row.version),
                    Sql::UBigInt(row.commit_seq),
                    Sql::Text(row.scope.clone()),
                    Sql::Text(row.object_type.clone()),
                    Sql::Boolean(row.bad),
                    Sql::Boolean(tomb),
                ];
                for index in 0..table.columns.len() {
                    let (text, secs, nanos) = row
                        .values
                        .as_ref()
                        .and_then(|v| v.get(index))
                        .map_or((Sql::Null, Sql::Null, Sql::Null), stage);
                    values.extend([text, secs, nanos]);
                }
                db(appender.append_row(duckdb::appender_params_from_iter(values)))?;
            }
            db(appender.flush())?;
        }
        let target = mirror_table(name);
        // Close every open version a staged version supersedes.
        db(conn.execute(
            &format!(
                "UPDATE {target} SET \":to\" = s.first FROM (SELECT \":id\", min(\":from\") AS first FROM \":stage\" GROUP BY \":id\") s \
                 WHERE {target}.\":w\" = ? AND {target}.\":id\" = s.\":id\" AND {target}.\":to\" IS NULL"
            ),
            [&request.world],
        ))?;
        let mut columns = Vec::new();
        let mut values = Vec::new();
        for column in &table.columns {
            columns.push(ident(&column.id));
            values.push(cast(column.kind, &ident(&column.id)));
            if matches!(column.kind, ColumnKind::Timestamp | ColumnKind::Date) {
                for suffix in ["t", "n"] {
                    columns.push(derived(&column.id, suffix));
                    values.push(derived(&column.id, suffix));
                }
            }
        }
        let separator = if columns.is_empty() { "" } else { ", " };
        db(conn.execute(
            &format!(
                "INSERT INTO {target} (\":w\", \":id\", \":v\", \":from\", \":to\", \":scope\", \":type\", \":bad\"{separator}{}) \
                 SELECT ?, \":id\", \":v\", \":from\", lead(\":from\") OVER (PARTITION BY \":id\" ORDER BY \":v\", \":ord\"), \":scope\", \":type\", \":bad\"{separator}{} \
                 FROM \":stage\" QUALIFY NOT \":tomb\"",
                columns.join(", "),
                values.join(", "),
            ),
            [&request.world],
        ))?;
        db(conn.execute_batch("DROP TABLE \":stage\""))?;
    }
    Ok(())
}

/// SQL text with its bound values in textual order, so a fragment used
/// twice binds its values twice and nothing is ever spliced into the text.
#[derive(Clone, Default)]
struct Frag {
    text: String,
    params: Vec<Sql>,
}

impl Frag {
    fn raw(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            params: Vec::new(),
        }
    }

    fn bind(text: &str, value: Sql) -> Self {
        Self {
            text: text.to_owned(),
            params: vec![value],
        }
    }

    /// `template` with each `{}` replaced, in order, by one argument.
    fn fmt(template: &str, args: &[&Self]) -> Self {
        let mut out = Self::default();
        let mut parts = template.split("{}");
        if let Some(first) = parts.next() {
            out.text.push_str(first);
        }
        for (part, arg) in parts.zip(args) {
            out.text.push_str(&arg.text);
            out.params.extend(arg.params.iter().cloned());
            out.text.push_str(part);
        }
        out
    }

    fn join(parts: &[Self], separator: &str) -> Self {
        let mut out = Self::default();
        for (i, part) in parts.iter().enumerate() {
            if i > 0 {
                out.text.push_str(separator);
            }
            out.text.push_str(&part.text);
            out.params.extend(part.params.iter().cloned());
        }
        out
    }
}

/// The rows a question reads: a mirror at a frontier, or a dataset's load.
struct Scope {
    table: String,
    columns: Vec<Column>,
    /// Every row of the source, excluded identities included.
    all: Frag,
    /// The rows a question reads.
    population: Frag,
    excluded: Vec<String>,
}

impl Scope {
    fn new(conn: &Connection, source: &Source) -> Outcome<Self> {
        match source {
            Source::Mirror {
                world,
                table,
                frontier,
                excluded,
                ..
            } => {
                let reached: u64 = meta(conn, &format!("frontier:{world}"))?
                    .and_then(|v| v.parse().ok())
                    .unwrap_or(0);
                if reached < *frontier {
                    return Err(refusal(
                        "core.analytics.frontier_behind",
                        format!("the mirror is at {reached}"),
                    ));
                }
                let schema = schema_of(conn, &format!("schema:{table}"))?;
                let at = Frag::bind("?", Sql::UBigInt(*frontier));
                let all = Frag::fmt(
                    "(\":w\" = {} AND \":from\" <= {} AND (\":to\" IS NULL OR \":to\" > {}))",
                    &[&Frag::bind("?", Sql::Text(world.clone())), &at, &at],
                );
                let population = if excluded.is_empty() {
                    all.clone()
                } else {
                    Frag::fmt("({} AND \":id\" NOT IN ({}))", &[&all, &id_list(excluded)])
                };
                Ok(Self {
                    table: mirror_table(table),
                    columns: schema.columns,
                    all,
                    population,
                    excluded: excluded.clone(),
                })
            }
            Source::Dataset { table, load } => {
                if meta(conn, &format!("load:{table}"))?.as_deref() != Some(load.as_str()) {
                    return Err(refusal(STALE, "the dataset was loaded again"));
                }
                let schema = schema_of(conn, &format!("dataset:{table}"))?;
                Ok(Self {
                    table: dataset_table(table),
                    columns: schema.columns,
                    all: Frag::raw("TRUE"),
                    population: Frag::raw("TRUE"),
                    excluded: Vec::new(),
                })
            }
        }
    }

    fn column(&self, id: &str) -> Outcome<&Column> {
        self.columns
            .iter()
            .find(|c| c.id == id)
            .ok_or_else(|| refusal(UNSUPPORTED, format!("no column {id}")))
    }

    /// Rows of the whole source (exclusions included) matching `condition`.
    fn count(&self, conn: &Connection, condition: &Frag) -> Outcome<u64> {
        let sql = Frag::fmt(
            &format!("SELECT count(*) FROM {} WHERE {{}} AND ({{}})", self.table),
            &[&self.all, condition],
        );
        query_u64(conn, &sql)
    }

    fn excluded_present(&self, conn: &Connection) -> Outcome<Vec<String>> {
        if self.excluded.is_empty() {
            return Ok(Vec::new());
        }
        let sql = Frag::fmt(
            &format!(
                "SELECT \":id\" FROM {} WHERE {{}} AND \":id\" IN ({{}}) ORDER BY \":id\"",
                self.table
            ),
            &[&self.all, &id_list(&self.excluded)],
        );
        let mut statement = db(conn.prepare(&sql.text))?;
        let rows = db(statement.query_map(params_from_iter(sql.params), |row| row.get::<_, String>(0)))?;
        db(rows.collect())
    }
}

fn id_list(ids: &[String]) -> Frag {
    Frag::join(
        &ids.iter()
            .map(|id| Frag::bind("?", Sql::Text(id.clone())))
            .collect::<Vec<_>>(),
        ", ",
    )
}

fn query_u64(conn: &Connection, sql: &Frag) -> Outcome<u64> {
    let mut statement = db(conn.prepare(&sql.text))?;
    db(statement.query_row(params_from_iter(sql.params.clone()), |row| row.get::<_, u64>(0)))
}

/// A lowered question as SQL over its scope: per row the bucket, the
/// operand coefficients, the null flags and whether the exact engine
/// would refuse on reaching it.
struct Projection {
    sql: Frag,
    arity: usize,
    group: GroupKind,
}

#[derive(Clone, Copy)]
enum GroupKind {
    None,
    Week,
    Text,
    Number,
    Bool,
}

/// A filter's truth on a row, and when reaching it refuses the question.
struct Condition {
    holds: Frag,
    refuse: Frag,
}

fn filter_sql(scope: &Scope, filter: &Filter) -> Outcome<Condition> {
    let column = scope.column(&filter.column)?;
    let c = Frag::raw(ident(&filter.column));
    let never = Frag::raw("FALSE");
    let (holds, refuse) = match &filter.test {
        Test::IsNull => (Frag::fmt("({} IS NULL)", &[&c]), never),
        Test::Equals(literal) => {
            let bound = match (literal, column.kind) {
                (Literal::Text(text), ColumnKind::Text | ColumnKind::Date) => {
                    Frag::bind("?", Sql::Text(text.clone()))
                }
                (Literal::Bool(flag), ColumnKind::Boolean) => Frag::bind("?", Sql::Boolean(*flag)),
                (Literal::Number(text), ColumnKind::Integer) => {
                    Frag::bind("CAST(? AS DECIMAL(38,0))", Sql::Text(text.clone()))
                }
                (Literal::Number(text), ColumnKind::Decimal { scale }) => Frag::bind(
                    &format!("CAST(? AS DECIMAL(38,{scale}))"),
                    Sql::Text(text.clone()),
                ),
                _ => return Err(refusal(UNSUPPORTED, "a literal of another kind")),
            };
            (Frag::fmt("({} IS NOT NULL AND {} = {})", &[&c, &c, &bound]), never)
        }
        Test::Instant { secs, nanos } => (
            Frag::fmt(
                &format!(
                    "({{}} IS NOT NULL AND {} = {{}} AND {} = {{}})",
                    derived(&filter.column, "t"),
                    derived(&filter.column, "n")
                ),
                &[
                    &c,
                    &Frag::bind("?", Sql::BigInt(*secs)),
                    &Frag::bind("?", Sql::Int(i32::try_from(*nanos).unwrap_or(i32::MAX))),
                ],
            ),
            never,
        ),
        Test::NeverPresent => (Frag::raw("FALSE"), never),
        Test::RefusePresent => (Frag::raw("FALSE"), Frag::fmt("({} IS NOT NULL)", &[&c])),
    };
    let holds = if filter.negate {
        Frag::fmt("(NOT {})", &[&holds])
    } else {
        holds
    };
    Ok(Condition { holds, refuse })
}

/// Sequential filters: a later filter is reached only when every earlier one
/// held, so its refusal applies only then.
fn filters_sql(scope: &Scope, filters: &[Filter]) -> Outcome<Condition> {
    let mut holds = Frag::raw("TRUE");
    let mut refuse = Frag::raw("FALSE");
    for filter in filters {
        let next = filter_sql(scope, filter)?;
        refuse = Frag::fmt("({} OR ({} AND {}))", &[&refuse, &holds, &next.refuse]);
        holds = Frag::fmt("({} AND {})", &[&holds, &next.holds]);
    }
    Ok(Condition { holds, refuse })
}

fn instant_bound(column: &str, (secs, nanos): (i64, u32), op: &str) -> Frag {
    let t = derived(column, "t");
    let n = derived(column, "n");
    let strict = if op == ">=" { ">" } else { "<" };
    let secs = Frag::bind("?", Sql::BigInt(secs));
    let nanos = Frag::bind("?", Sql::BigInt(i64::from(nanos)));
    Frag::fmt(
        &format!("({t} {strict} {{}} OR ({t} = {{}} AND {n} {op} {{}}))"),
        &[&secs, &secs, &nanos],
    )
}

impl Projection {
    #[allow(
        clippy::too_many_lines,
        reason = "one projection mirrors the shared row projection step by step"
    )]
    fn build(query: &Lowered, scope: &Scope) -> Outcome<Self> {
        let window = &query.window;
        if !matches!(scope.column(&window.column)?.kind, ColumnKind::Timestamp | ColumnKind::Date) {
            return Err(refusal(UNSUPPORTED, "the window is not on an instant"));
        }
        let time = Frag::raw(ident(&window.column));
        let question = filters_sql(scope, &query.filters)?;
        // The base layer evaluates the question's filters once per row.
        let base = Frag::fmt(
            &format!("SELECT *, {{}} AS \":q\", {{}} AS \":qr\" FROM {} WHERE {{}}", scope.table),
            &[&question.holds, &question.refuse, &scope.population],
        );
        let q = Frag::raw("\":q\"");
        let reach = Frag::fmt(
            "({} AND {} IS NOT NULL AND {} AND {})",
            &[
                &q,
                &time,
                &instant_bound(&window.column, window.start, ">="),
                &instant_bound(&window.column, window.end, "<"),
            ],
        );
        let (group_expr, rep_expr, group) = match &query.bucket {
            super::Bucket::None => (Frag::raw("NULL"), Frag::raw("NULL"), GroupKind::None),
            super::Bucket::Week { column } => {
                if !matches!(scope.column(column)?.kind, ColumnKind::Timestamp | ColumnKind::Date) {
                    return Err(refusal(UNSUPPORTED, "a week of a value that is not an instant"));
                }
                let t = derived(column, "t");
                let monday = Frag::raw(format!("({t} - ((({t} - 345600) % 604800) + 604800) % 604800)"));
                (monday.clone(), monday, GroupKind::Week)
            }
            super::Bucket::Column { column } => {
                let c = ident(column);
                match scope.column(column)?.kind {
                    ColumnKind::Text | ColumnKind::Timestamp | ColumnKind::Date => {
                        (Frag::raw(format!("nfc_normalize({c})")), Frag::raw(c), GroupKind::Text)
                    }
                    ColumnKind::Integer | ColumnKind::Decimal { .. } => (
                        Frag::raw(c.clone()),
                        Frag::raw(format!("CAST({c} AS VARCHAR)")),
                        GroupKind::Number,
                    ),
                    ColumnKind::Boolean => (Frag::raw(c.clone()), Frag::raw(c), GroupKind::Bool),
                    ColumnKind::Opaque => return Err(refusal(UNSUPPORTED, "an opaque bucket")),
                }
            }
        };
        let mut select = vec![
            Frag::raw(format!("{} AS id", ident(":id"))),
            Frag::raw(format!("{} AS v", ident(":v"))),
            Frag::fmt("{} AS g", &[&group_expr]),
            Frag::fmt("{} AS r", &[&rep_expr]),
        ];
        let mut refusals = vec![Frag::raw("\":qr\""), Frag::fmt("({} AND {} IS NULL)", &[&q, &time])];
        if let super::Bucket::Week { column } = &query.bucket {
            refusals.push(Frag::fmt("({} AND {} IS NULL)", &[&reach, &Frag::raw(ident(column))]));
        }
        for (k, operand) in query.operands.iter().enumerate() {
            let column = scope.column(&operand.column)?;
            let c = ident(&operand.column);
            let own = filters_sql(scope, &operand.filters)?;
            let (coefficient, valid) = match column.kind {
                ColumnKind::Integer => (
                    format!("CAST({c} * 1000000 AS HUGEINT)"),
                    format!("(abs(CAST({c} AS HUGEINT)) * 1000000 <= {MAX_VALUE})"),
                ),
                ColumnKind::Decimal { scale } if scale <= 6 => (
                    format!("CAST({c} * 1000000 AS HUGEINT)"),
                    format!("(abs(CAST({c} * 1000000 AS HUGEINT)) <= {MAX_VALUE})"),
                ),
                ColumnKind::Decimal { .. } => {
                    let six = format!("TRY_CAST({c} AS DECIMAL(38,6))");
                    (
                        format!("CAST({six} * 1000000 AS HUGEINT)"),
                        format!("({six} IS NOT NULL AND {six} = {c} AND abs(CAST({six} * 1000000 AS HUGEINT)) <= {MAX_VALUE})"),
                    )
                }
                ColumnKind::Text
                | ColumnKind::Boolean
                | ColumnKind::Timestamp
                | ColumnKind::Date
                | ColumnKind::Opaque => {
                    return Err(refusal(UNSUPPORTED, "a summed column that is not numeric"));
                }
            };
            let pass = Frag::fmt("({} AND {})", &[&reach, &own.holds]);
            select.push(Frag::fmt(
                &format!("CASE WHEN {{}} AND {c} IS NOT NULL AND {valid} THEN CAST({coefficient} AS BIGINT) END AS a{k}"),
                &[&pass],
            ));
            select.push(Frag::fmt(
                &format!("CASE WHEN {{}} AND {c} IS NULL THEN 1 ELSE 0 END AS n{k}"),
                &[&pass],
            ));
            refusals.push(Frag::fmt("({} AND {})", &[&reach, &own.refuse]));
            refusals.push(Frag::fmt(
                &format!("({{}} AND {c} IS NOT NULL AND NOT {valid})"),
                &[&pass],
            ));
        }
        let arity = query.operands.len();
        let contributes = (0..arity)
            .map(|k| format!("a{k} IS NOT NULL OR n{k} = 1"))
            .collect::<Vec<_>>()
            .join(" OR ");
        let sql = Frag::fmt(
            &format!("SELECT *, ({contributes}) AS contributes FROM (SELECT {{}}, ({{}}) AS refuse FROM ({{}}) AS base) AS p"),
            &[&Frag::join(&select, ", "), &Frag::join(&refusals, " OR "), &base],
        );
        Ok(Self { sql, arity, group })
    }

    fn count(&self, conn: &Connection, flag: &str) -> Outcome<u64> {
        query_u64(
            conn,
            &Frag::fmt(&format!("SELECT count(*) FROM ({{}}) AS x WHERE {flag}"), &[&self.sql]),
        )
    }

    fn group_key(&self, row: &duckdb::Row<'_>, index: usize) -> duckdb::Result<GroupKey> {
        Ok(match self.group {
            GroupKind::None => GroupKey::Null,
            GroupKind::Week => row
                .get::<_, Option<i64>>(index)?
                .map_or(GroupKey::Null, GroupKey::Week),
            GroupKind::Text => row
                .get::<_, Option<String>>(index)?
                .map_or(GroupKey::Null, GroupKey::Text),
            GroupKind::Number => match row.get::<_, Option<String>>(index)? {
                None => GroupKey::Null,
                Some(text) => GroupKey::Number(
                    norn_ir::Decimal::parse(&text).map_or(text, |d| d.canonical()),
                ),
            },
            GroupKind::Bool => row
                .get::<_, Option<bool>>(index)?
                .map_or(GroupKey::Null, GroupKey::Bool),
        })
    }

    fn amounts(&self) -> String {
        (0..self.arity)
            .map(|k| format!("a{k}"))
            .collect::<Vec<_>>()
            .join(", ")
    }

    fn buckets(&self, conn: &Connection, inline: bool) -> Outcome<Vec<BucketTotals>> {
        let mut aggregates = Vec::new();
        for k in 0..self.arity {
            aggregates.push(format!("CAST(COALESCE(sum(a{k}), 0) AS VARCHAR)"));
            aggregates.push(format!("CAST(COALESCE(sum(abs(CAST(a{k} AS HUGEINT))), 0) AS VARCHAR)"));
            aggregates.push(format!("CAST(sum(n{k}) AS UBIGINT)"));
        }
        let sql = Frag::fmt(
            &format!(
                "SELECT arg_min(r, id) AS rep, count(*) AS c, {} FROM ({{}}) AS x WHERE contributes GROUP BY g ORDER BY g NULLS FIRST",
                aggregates.join(", ")
            ),
            &[&self.sql],
        );
        let mut statement = db(conn.prepare(&sql.text))?;
        let arity = self.arity;
        let rows = db(statement.query_map(params_from_iter(sql.params.clone()), |row| {
            let group = self.group_key(row, 0)?;
            let count: u64 = row.get(1)?;
            let mut totals = Vec::with_capacity(arity);
            let mut magnitude = Vec::with_capacity(arity);
            let mut nulls = Vec::with_capacity(arity);
            let mut column = 2;
            for _ in 0..arity {
                totals.push(row.get::<_, String>(column)?);
                magnitude.push(row.get::<_, String>(column.saturating_add(1))?);
                nulls.push(row.get::<_, u64>(column.saturating_add(2))?);
                column = column.saturating_add(3);
            }
            Ok(BucketTotals {
                group,
                totals,
                magnitude,
                nulls,
                count,
                digest: String::new(),
                members: inline.then(Vec::new),
            })
        }))?;
        let mut buckets: Vec<BucketTotals> = db(rows.collect())?;
        // Members in bucket then identity order: the digests, and inline members.
        let sql = Frag::fmt(
            &format!(
                "SELECT dense_rank() OVER (ORDER BY g NULLS FIRST) AS gk, id, v, {} FROM ({{}}) AS x WHERE contributes ORDER BY gk, id",
                self.amounts()
            ),
            &[&self.sql],
        );
        let mut statement = db(conn.prepare(&sql.text))?;
        let mut rows = db(statement.query(params_from_iter(sql.params.clone())))?;
        let mut digests = vec![MembershipDigest::default(); buckets.len()];
        while let Some(row) = db(rows.next())? {
            let gk: u64 = db(row.get(0))?;
            let index = usize::try_from(gk.saturating_sub(1)).unwrap_or(usize::MAX);
            let member = MemberRow {
                id: db(row.get(1))?,
                version: db(row.get(2))?,
                amounts: db((0..arity)
                    .map(|k| row.get::<_, Option<i64>>(k.saturating_add(3)))
                    .collect::<Result<_, _>>())?,
            };
            digests
                .get_mut(index)
                .ok_or_else(|| refusal("core.analytics.engine", "a member outside its bucket"))?
                .push(&member);
            if let Some(Some(members)) = buckets.get_mut(index).map(|b| b.members.as_mut()) {
                members.push(member);
            }
        }
        for (bucket, digest) in buckets.iter_mut().zip(digests) {
            bucket.digest = digest.finish();
        }
        Ok(buckets)
    }

    fn page(
        &self,
        conn: &Connection,
        group: &GroupKey,
        after: Option<&str>,
        limit: u32,
    ) -> Outcome<Vec<MemberRow>> {
        let matches = match (group, self.group) {
            (GroupKey::Null, _) => Frag::raw("g IS NULL"),
            (GroupKey::Bool(flag), GroupKind::Bool) => Frag::bind("g = ?", Sql::Boolean(*flag)),
            (GroupKey::Number(text), GroupKind::Number) => {
                Frag::bind("CAST(g AS VARCHAR) = ?", Sql::Text(text.clone()))
            }
            (GroupKey::Text(text), GroupKind::Text) => {
                Frag::bind("g = nfc_normalize(?)", Sql::Text(text.clone()))
            }
            (GroupKey::Week(monday), GroupKind::Week) => Frag::bind("g = ?", Sql::BigInt(*monday)),
            _ => return Err(refusal(UNSUPPORTED, "a bucket of another kind")),
        };
        let after = after.map_or_else(
            || Frag::raw("TRUE"),
            |after| Frag::bind("id > ?", Sql::Text(after.to_owned())),
        );
        let limit = Frag::bind("?", Sql::UInt(limit.min(10_000)));
        let sql = Frag::fmt(
            &format!(
                "SELECT id, v, {} FROM ({{}}) AS x WHERE contributes AND {{}} AND {{}} ORDER BY id LIMIT {{}}",
                self.amounts()
            ),
            &[&self.sql, &matches, &after, &limit],
        );
        let mut statement = db(conn.prepare(&sql.text))?;
        let arity = self.arity;
        let rows = db(statement.query_map(params_from_iter(sql.params), |row| {
            Ok(MemberRow {
                id: row.get(0)?,
                version: row.get(1)?,
                amounts: (0..arity)
                    .map(|k| row.get::<_, Option<i64>>(k.saturating_add(2)))
                    .collect::<Result<_, _>>()?,
            })
        }))?;
        db(rows.collect())
    }
}

fn sha256_file(path: &Path) -> std::io::Result<(String, u64)> {
    let mut file = std::fs::File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0u8; 1 << 20];
    let mut bytes = 0u64;
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(buffer.get(..read).unwrap_or_default());
        bytes = bytes.saturating_add(u64::try_from(read).unwrap_or(u64::MAX));
    }
    Ok((hex(&hasher.finalize()), bytes))
}

const TIMESTAMP_SHAPE: &str = r"^[0-9]{4}-[0-9]{2}-[0-9]{2}T[0-9]{2}:[0-9]{2}:[0-9]{2}(\.[0-9]{1,9})?(Z|[+-][0-9]{2}:[0-9]{2})$";
const DATE_SHAPE: &str = r"^[0-9]{4}-[0-9]{2}-[0-9]{2}$";

/// The reason a present text value is not of `kind`, or SQL NULL when it is.
fn reason_sql(kind: ColumnKind, v: &str) -> String {
    match kind {
        ColumnKind::Integer => format!(
            "CASE WHEN regexp_full_match({v}, '[+-]?[0-9]{{1,38}}') THEN NULL ELSE 'core.bulk.not_integer' END"
        ),
        ColumnKind::Decimal { scale } => format!(
            "CASE WHEN NOT regexp_full_match({v}, '[+-]?[0-9]+(\\.[0-9]+)?') THEN 'core.bulk.not_decimal' \
             WHEN length(regexp_extract({v}, '\\.([0-9]+)$', 1)) > {scale} THEN 'core.bulk.scale_exceeded' \
             WHEN TRY_CAST({v} AS DECIMAL(38,{scale})) IS NULL THEN 'core.bulk.not_decimal' ELSE NULL END"
        ),
        ColumnKind::Boolean => format!(
            "CASE WHEN lower({v}) IN ('true', 'false') THEN NULL ELSE 'core.bulk.not_boolean' END"
        ),
        ColumnKind::Timestamp => format!(
            "CASE WHEN regexp_full_match({v}, '{TIMESTAMP_SHAPE}') AND TRY_CAST(substr({v}, 1, 10) AS DATE) IS NOT NULL \
             AND CAST(substr({v}, 12, 2) AS INTEGER) <= 23 AND CAST(substr({v}, 15, 2) AS INTEGER) <= 59 \
             AND CAST(substr({v}, 18, 2) AS INTEGER) <= 59 \
             AND (right({v}, 1) = 'Z' OR (CAST(substr(right({v}, 6), 2, 2) AS INTEGER) <= 23 AND CAST(right({v}, 2) AS INTEGER) <= 59)) \
             THEN NULL ELSE 'core.bulk.not_timestamp' END"
        ),
        ColumnKind::Date => format!(
            "CASE WHEN regexp_full_match({v}, '{DATE_SHAPE}') AND TRY_CAST({v} AS DATE) IS NOT NULL THEN NULL ELSE 'core.bulk.not_date' END"
        ),
        ColumnKind::Text | ColumnKind::Opaque => "NULL".to_owned(),
    }
}

/// The exact UTC ordering key `(seconds, nanoseconds)` of a validated
/// RFC 3339 timestamp (or a date's midnight), in integer arithmetic.
fn instant_sql(kind: ColumnKind, v: &str) -> (String, String) {
    let days = format!("datediff('day', DATE '1970-01-01', CAST(substr({v}, 1, 10) AS DATE))");
    if kind == ColumnKind::Date {
        return (format!("CAST({days} AS BIGINT) * 86400"), "0".to_owned());
    }
    let offset = format!(
        "CASE WHEN right({v}, 1) = 'Z' THEN 0 ELSE (CASE WHEN substr(right({v}, 6), 1, 1) = '-' THEN -1 ELSE 1 END) * \
         (CAST(substr(right({v}, 6), 2, 2) AS BIGINT) * 3600 + CAST(right({v}, 2) AS BIGINT) * 60) END"
    );
    let secs = format!(
        "(CAST({days} AS BIGINT) * 86400 + CAST(substr({v}, 12, 2) AS BIGINT) * 3600 + CAST(substr({v}, 15, 2) AS BIGINT) * 60 + CAST(substr({v}, 18, 2) AS BIGINT) - {offset})"
    );
    let nanos = format!(
        "CAST(rpad(COALESCE(regexp_extract({v}, '^[^.]*\\.([0-9]{{1,9}})', 1), ''), 9, '0') AS INTEGER)"
    );
    (secs, nanos)
}

fn dataset_ddl(table: &TableSchema) -> String {
    let mut columns = vec![
        format!("{} VARCHAR", ident(":id")),
        format!("{} UBIGINT", ident(":v")),
        format!("{} UBIGINT", ident(":row")),
        format!("{} VARCHAR", ident(":load")),
    ];
    columns.extend(table.columns.iter().map(column_ddl));
    columns.join(", ")
}

/// Read the file into `":raw"`: one nullable text column `c<i>` per table
/// column, found by its header (or Parquet field) name, numbered from 1 in
/// file order; an empty or blank field is null.
fn read_raw(conn: &Connection, request: &LoadRequest, file: &Path) -> Outcome<()> {
    let path = file.to_string_lossy().replace('\'', "''");
    let blank = |c: &str| format!("CASE WHEN trim({c}) = '' THEN NULL ELSE {c} END");
    let source = match request.file.format {
        FileFormat::Csv => format!(
            "SELECT * FROM read_csv('{path}', header = true, all_varchar = true, delim = ',', quote = '\"', escape = '\"', strict_mode = true, null_padding = false)"
        ),
        FileFormat::Parquet => format!("SELECT * FROM read_parquet('{path}', file_row_number = true)"),
    };
    // Materialized in file order (insertion order is preserved).
    db(conn.execute_batch(&format!("CREATE OR REPLACE TEMP TABLE \":file\" AS {source}")))?;
    let mut statement = db(conn.prepare(
        "SELECT column_name, data_type FROM information_schema.columns WHERE table_name = ':file'",
    ))?;
    let present: BTreeMap<String, String> = db(db(statement.query_map([], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
    }))?
    .collect::<Result<_, _>>())?;
    let mut select = Vec::new();
    for (i, column) in request.table.columns.iter().enumerate() {
        let name = request.headers.get(&column.id).unwrap_or(&column.id);
        let Some(data_type) = present.get(name) else {
            return Err(refusal("core.bulk.missing_column", format!("the file has no column {name}")));
        };
        let c = ident(name);
        let text = match (column.kind, data_type.as_str()) {
            (ColumnKind::Timestamp, kind) if kind.starts_with("TIMESTAMP") => {
                format!("strftime(CAST({c} AS TIMESTAMP), '%Y-%m-%dT%H:%M:%S.%fZ')")
            }
            (ColumnKind::Date, "DATE") => format!("strftime({c}, '%Y-%m-%d')"),
            _ => format!("CAST({c} AS VARCHAR)"),
        };
        select.push(format!("{} AS c{i}", blank(&text)));
    }
    let row = match request.file.format {
        // Inside the load's transaction row ids start at a local offset; their
        // order is still the file's.
        FileFormat::Csv => "CAST(row_number() OVER (ORDER BY rowid) AS UBIGINT)",
        FileFormat::Parquet => "CAST(file_row_number + 1 AS UBIGINT)",
    };
    let separator = if select.is_empty() { "" } else { ", " };
    db(conn.execute_batch(&format!(
        "CREATE OR REPLACE TEMP TABLE \":raw\" AS SELECT {row} AS \":row\"{separator}{} FROM \":file\" ORDER BY 1",
        select.join(", ")
    )))?;
    Ok(())
}

fn load_file(
    conn: &Connection,
    request: &LoadRequest,
    file: &Path,
) -> Outcome<(u64, BTreeMap<String, u64>)> {
    let table = &request.table;
    read_raw(conn, request, file)?;
    // The first reason per row, in column order.
    let mut reasons = Vec::new();
    for (i, column) in table.columns.iter().enumerate() {
        let v = format!("c{i}");
        let missing = if column.nullable {
            "NULL".to_owned()
        } else {
            "'core.bulk.required'".to_owned()
        };
        reasons.push(format!(
            "CASE WHEN {v} IS NULL THEN {missing} ELSE {} END",
            reason_sql(column.kind, &v)
        ));
    }
    let reason = if reasons.is_empty() {
        "NULL".to_owned()
    } else {
        format!("coalesce({})", reasons.join(", "))
    };
    db(conn.execute_batch(&format!(
        "CREATE OR REPLACE TEMP TABLE \":checked\" AS SELECT *, {reason} AS \":reason\" FROM \":raw\""
    )))?;
    let dataset = dataset_table(&table.name);
    let quarantine = quarantine_table(&table.name);
    db(conn.execute_batch(&format!(
        "DROP TABLE IF EXISTS {dataset}; DROP TABLE IF EXISTS {quarantine}; CREATE TABLE {dataset} ({}); \
         CREATE TABLE {quarantine} (\":row\" UBIGINT, \":load\" VARCHAR, \":reason\" VARCHAR{});",
        dataset_ddl(table),
        table.columns.iter().fold(String::new(), |mut out, c| {
            out.push_str(", ");
            out.push_str(&ident(&c.id));
            out.push_str(" VARCHAR");
            out
        }),
    )))?;
    let mut columns = Vec::new();
    let mut values = Vec::new();
    for (i, column) in table.columns.iter().enumerate() {
        let v = format!("c{i}");
        columns.push(ident(&column.id));
        values.push(format!("CASE WHEN {v} IS NULL THEN NULL ELSE {} END", cast(column.kind, &v)));
        if matches!(column.kind, ColumnKind::Timestamp | ColumnKind::Date) {
            let (secs, nanos) = instant_sql(column.kind, &v);
            columns.push(derived(&column.id, "t"));
            values.push(format!("CASE WHEN {v} IS NULL THEN NULL ELSE {secs} END"));
            columns.push(derived(&column.id, "n"));
            values.push(format!("CASE WHEN {v} IS NULL THEN NULL ELSE {nanos} END"));
        }
    }
    let separator = if columns.is_empty() { "" } else { ", " };
    db(conn.execute(
        &format!(
            "INSERT INTO {dataset} (\":id\", \":v\", \":row\", \":load\"{separator}{}) SELECT CAST(\":row\" AS VARCHAR), 1, \":row\", ?{separator}{} FROM \":checked\" WHERE \":reason\" IS NULL ORDER BY \":row\"",
            columns.join(", "),
            values.join(", ")
        ),
        [&request.load],
    ))?;
    db(conn.execute(
        &format!(
            "INSERT INTO {quarantine} SELECT \":row\", ?, \":reason\"{} FROM \":checked\" WHERE \":reason\" IS NOT NULL ORDER BY \":row\"",
            (0..table.columns.len()).fold(String::new(), |mut out, i| {
                out.push_str(", c");
                out.push_str(&i.to_string());
                out
            })
        ),
        [&request.load],
    ))?;
    let rows: u64 = db(conn.query_row(&format!("SELECT count(*) FROM {dataset}"), [], |row| row.get(0)))?;
    let mut statement = db(conn.prepare(&format!(
        "SELECT \":reason\", count(*) FROM {quarantine} GROUP BY \":reason\" ORDER BY \":reason\""
    )))?;
    let reasons: BTreeMap<String, u64> = db(db(statement.query_map([], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, u64>(1)?))
    }))?
    .collect::<Result<_, _>>())?;
    db(conn.execute_batch("DROP TABLE \":checked\"; DROP TABLE \":raw\"; DROP TABLE IF EXISTS \":file\";"))?;
    set_meta(conn, &format!("load:{}", table.name), &request.load)?;
    set_meta(conn, &format!("dataset:{}", table.name), &schema_text(table)?)?;
    set_meta(
        conn,
        &format!("lineage:{}", request.load),
        &format!("{} {}", request.file.sha256, request.file.uri),
    )?;
    Ok((rows, reasons))
}

fn present(kind: ColumnKind, text: Option<String>, secs: Option<i64>, nanos: Option<i32>) -> Option<Value> {
    let text = text?;
    Some(match kind {
        ColumnKind::Integer | ColumnKind::Decimal { .. } => Value::Number(
            norn_ir::Decimal::parse(&text).map_or(text, |d| d.canonical()),
        ),
        ColumnKind::Timestamp | ColumnKind::Date => Value::Instant {
            text,
            secs: secs?,
            nanos: u32::try_from(nanos?).ok()?,
        },
        ColumnKind::Boolean => Value::Bool(text == "true"),
        ColumnKind::Text => Value::Text(text),
        ColumnKind::Opaque => Value::Json(text),
    })
}

fn profile_column(conn: &Connection, scope: &Scope, id: &str, rows: u64) -> Outcome<ColumnProfile> {
    let column = scope.column(id)?.clone();
    let c = ident(id);
    let temporal = matches!(column.kind, ColumnKind::Timestamp | ColumnKind::Date);
    let (t, n) = if temporal {
        (derived(id, "t"), derived(id, "n"))
    } else {
        ("CAST(NULL AS BIGINT)".to_owned(), "CAST(NULL AS INTEGER)".to_owned())
    };
    let (order, distinct_of) = if temporal {
        (format!("{t}, {n}"), format!("({t}, {n})"))
    } else {
        (c.clone(), c.clone())
    };
    let counts = Frag::fmt(
        &format!("SELECT count({c}), count(DISTINCT {distinct_of}) FROM {} WHERE {{}}", scope.table),
        &[&scope.population],
    );
    let mut statement = db(conn.prepare(&counts.text))?;
    let (count, distinct): (u64, u64) = db(statement.query_row(params_from_iter(counts.params), |row| {
        Ok((row.get(0)?, row.get(1)?))
    }))?;
    let ordered = matches!(
        column.kind,
        ColumnKind::Integer | ColumnKind::Decimal { .. } | ColumnKind::Timestamp | ColumnKind::Date
    );
    let mut quantiles = Vec::new();
    if ordered && count > 0 {
        // Nearest rank: the smallest value with at least p% of values at or below it.
        let ranks: Vec<(u32, u64)> = [0u32, 25, 50, 75, 100]
            .into_iter()
            .map(|p| (p, u64::from(p).saturating_mul(count).div_ceil(100).max(1)))
            .collect();
        let wanted = Frag::join(
            &ranks
                .iter()
                .map(|(_, rank)| Frag::bind("?", Sql::UBigInt(*rank)))
                .collect::<Vec<_>>(),
            ", ",
        );
        let sql = Frag::fmt(
            &format!(
                "SELECT rn, CAST(v AS VARCHAR), t, n FROM (SELECT {c} AS v, {t} AS t, {n} AS n, row_number() OVER (ORDER BY {order}) AS rn FROM {} WHERE {{}} AND {c} IS NOT NULL) AS x WHERE rn IN ({{}})",
                scope.table
            ),
            &[&scope.population, &wanted],
        );
        let mut statement = db(conn.prepare(&sql.text))?;
        let found: BTreeMap<u64, Option<Value>> = db(db(statement.query_map(params_from_iter(sql.params), |row| {
            let rank: u64 = row.get(0)?;
            Ok((rank, present(column.kind, row.get(1)?, row.get(2)?, row.get(3)?)))
        }))?
        .collect::<Result<_, _>>())?;
        for (percent, rank) in ranks {
            if let Some(Some(value)) = found.get(&rank) {
                quantiles.push((percent, value.clone()));
            }
        }
    }
    let min = quantiles.first().map(|(_, v)| v.clone());
    let max = quantiles.last().map(|(_, v)| v.clone());
    Ok(ColumnProfile {
        column: id.to_owned(),
        count,
        nulls: rows.saturating_sub(count),
        distinct,
        min,
        max,
        quantiles,
    })
}
