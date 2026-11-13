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
    MembersRequest, MembersResponse, MirrorRow, ProfileRequest, ProfileResponse, Source,
    SyncRequest, TIME_LINK_COLUMN, TableSchema, TenantId, Test, TimeLink, Value, hex,
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
    value.map_err(|error| refusal("analytics.engine", error.to_string()))
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

/// The mirrored edges of one link table (METRIC-GRAIN-LINK-1).
fn link_table(name: &str) -> String {
    ident(&format!("k:{name}"))
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
        let path = self
            .settings
            .root
            .join(format!("{}.duckdb", tenant.as_str()));
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
            let mut stale = meta(conn, "links")?.unwrap_or_default() != request.links.join("\n");
            for table in &request.tables {
                stale |= meta(conn, &format!("pin:{}", table.name))?.as_deref() != Some(&table.pin);
            }
            if stale {
                reset_mirror(conn, &request.tables, &request.links)?;
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
    /// `analytics.frontier_moved` when the batch does not continue the
    /// mirror's frontier, or an unknown table.
    pub fn sync(&self, request: &SyncRequest) -> Outcome<FrontierResponse> {
        self.with(&request.tenant, |conn| {
            let key = format!("frontier:{}", request.world);
            let current: u64 = meta(conn, &key)?.and_then(|v| v.parse().ok()).unwrap_or(0);
            if current != request.after || request.through < request.after {
                return Err(refusal(
                    "analytics.frontier_moved",
                    format!("the mirror is at {current}"),
                ));
            }
            db(conn.execute_batch("BEGIN TRANSACTION"))?;
            let applied = apply_sync(conn, request)
                .and_then(|()| set_meta(conn, &key, &request.through.to_string()));
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
    /// mirror has not reached (`analytics.frontier_behind`), or a stale load.
    pub fn evaluate(&self, request: &EvaluateRequest) -> Outcome<EvaluateResponse> {
        self.with(&request.tenant, |conn| {
            let scope = Scope::new(conn, &request.source)?;
            let (cell, bad) = match &request.source {
                Source::Mirror { scope, .. } => (
                    Frag::bind("\":scope\" <> ?", Sql::Text(scope.clone())),
                    Frag::raw(ident(":bad")),
                ),
                Source::Dataset { .. } => (Frag::raw("FALSE"), Frag::raw("FALSE")),
            };
            let checks = Frag::fmt(
                &format!(
                    "SELECT count(*), count(*) FILTER (WHERE {{}}), count(*) FILTER (WHERE {{}}) FROM {} WHERE {{}}",
                    scope.table
                ),
                &[&cell, &bad, &scope.all],
            );
            let mut statement = db(conn.prepare(&checks.text))?;
            let (population, other, bad): (u64, u64, u64) = db(statement.query_row(
                params_from_iter(checks.params),
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            ))?;
            if other > 0 {
                return Err(refusal(STALE, "a row of another scope"));
            }
            if bad > 0 {
                return Err(refusal(UNSUPPORTED, "a stored value failed its check"));
            }
            let excluded = scope.excluded_present(conn)?;
            if let Some(link) = &request.query.time_link {
                check_link(conn, &scope, link)?;
            }
            let projection = Projection::build(&request.query, &scope)?;
            if let Some(untimed) = &projection.untimed
                && query_u64(conn, untimed)? > 0
            {
                return Err(refusal(UNSUPPORTED, "a snapshot version without its valid time"));
            }
            let answered = projection.materialize(conn).and_then(|()| {
                if query_u64(conn, &Frag::raw("SELECT count(*) FROM \":p\" WHERE refuse"))? > 0 {
                    return Err(refusal(UNSUPPORTED, "a row is outside the bounded profile"));
                }
                let unlinked = query_u64(conn, &Frag::raw("SELECT count(*) FROM \":p\" WHERE ul"))?;
                Ok((
                    projection.buckets(conn, population <= request.inline_members)?,
                    unlinked,
                ))
            });
            let _dropped = conn.execute_batch("DROP TABLE IF EXISTS \":p\"");
            let (buckets, unlinked) = answered?;
            Ok(EvaluateResponse {
                population,
                excluded,
                buckets,
                unlinked,
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
            let members = projection.page(
                conn,
                &request.group,
                request.after.as_deref(),
                request.limit,
            )?;
            Ok(MembersResponse { members })
        })
    }

    /// BULK-LOAD-1: load `file` (already downloaded) as the dataset's only
    /// load. The digest must be the recorded one; every row is typed by its
    /// column, and a row that is not (or misses a required value) is
    /// quarantined with the first reason, in column order.
    ///
    /// # Errors
    /// `bulk.digest_mismatch`, an unreadable file, or an engine failure.
    pub fn load(&self, request: &LoadRequest, file: &Path) -> Outcome<LoadResponse> {
        let started = Instant::now();
        let (digest, bytes) =
            sha256_file(file).map_err(|error| refusal("bulk.unreadable", error.to_string()))?;
        if digest != request.file.sha256 {
            return Err(refusal(
                "bulk.digest_mismatch",
                "the file is not the recorded upload",
            ));
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
                &Frag::fmt(
                    &format!("SELECT count(*) FROM {} WHERE {{}}", scope.table),
                    &[&scope.population],
                ),
            )?;
            let columns = request
                .columns
                .iter()
                .map(|id| profile_column(conn, &scope, id, rows))
                .collect::<Outcome<_>>()?;
            let quarantined =
                u64::try_from(scope.excluded_present(conn)?.len()).unwrap_or(u64::MAX);
            Ok(ProfileResponse {
                rows,
                quarantined,
                columns,
            })
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
            hex(&Sha256::digest(
                format!("{:?}{:?}", request, Instant::now()).as_bytes()
            )),
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

fn reset_mirror(conn: &Connection, tables: &[TableSchema], links: &[String]) -> Outcome<()> {
    db(conn.execute_batch("BEGIN TRANSACTION"))?;
    let reset = (|| {
        let mut statement = db(conn.prepare(
            "SELECT table_name FROM information_schema.tables WHERE table_name LIKE 'm:%' OR table_name LIKE 'k:%'",
        ))?;
        let names: Vec<String> =
            db(db(statement.query_map([], |row| row.get(0)))?.collect::<Result<_, _>>())?;
        for name in names {
            db(conn.execute_batch(&format!("DROP TABLE {}", ident(&name))))?;
        }
        db(conn.execute_batch(
            "DELETE FROM \":meta\" WHERE key LIKE 'frontier:%' OR key LIKE 'pin:%'",
        ))?;
        for table in tables {
            let columns: Vec<String> = table.columns.iter().map(column_ddl).collect();
            let system = [
                ":w VARCHAR",
                ":id VARCHAR",
                ":v UBIGINT",
                ":from UBIGINT",
                ":to UBIGINT",
                ":scope VARCHAR",
                ":type VARCHAR",
                ":bad BOOLEAN",
            ]
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
            set_meta(
                conn,
                &format!("schema:{}", table.name),
                &schema_text(table)?,
            )?;
        }
        for link in links {
            db(conn.execute_batch(&format!(
                "CREATE TABLE {} (\":w\" VARCHAR, \":s\" VARCHAR, \":t\" VARCHAR, \":live\" BOOLEAN, \":c\" UBIGINT, PRIMARY KEY (\":w\", \":s\", \":t\"))",
                link_table(link)
            )))?;
        }
        set_meta(conn, "links", &links.join("\n"))?;
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
    serde_json::to_string(table).map_err(|e| refusal("analytics.engine", e.to_string()))
}

fn schema_of(conn: &Connection, key: &str) -> Outcome<TableSchema> {
    let text = meta(conn, key)?.ok_or_else(|| refusal(UNSUPPORTED, "no such table"))?;
    serde_json::from_str(&text).map_err(|e| refusal("analytics.engine", e.to_string()))
}

/// METRIC-GRAIN-LINK-1: each changed edge's state after its last change, in
/// commit order.
fn apply_links(conn: &Connection, request: &SyncRequest) -> Outcome<()> {
    if request.links.is_empty() {
        return Ok(());
    }
    let known = meta(conn, "links")?.unwrap_or_default();
    let known: std::collections::BTreeSet<&str> = known.split('\n').collect();
    for edge in &request.links {
        if !known.contains(edge.table.as_str()) {
            return Err(refusal(
                UNSUPPORTED,
                format!("no link table {}", edge.table),
            ));
        }
        // An edge changed again since carries the cell's head, past the batch;
        // none is older than the batch.
        if edge.commit_seq <= request.after {
            return Err(refusal(
                "analytics.frontier_moved",
                "an edge outside the batch",
            ));
        }
        db(conn.execute(
            &format!(
                "INSERT OR REPLACE INTO {} VALUES (?, ?, ?, ?, ?)",
                link_table(&edge.table)
            ),
            duckdb::params![
                request.world,
                edge.source,
                edge.target,
                edge.live,
                edge.commit_seq
            ],
        ))?;
    }
    Ok(())
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
    apply_links(conn, request)?;
    let mut by_table: BTreeMap<&str, Vec<&MirrorRow>> = BTreeMap::new();
    for row in &request.rows {
        if row.commit_seq <= request.after || row.commit_seq > request.through {
            return Err(refusal(
                "analytics.frontier_moved",
                "a row outside the batch",
            ));
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
                if row
                    .values
                    .as_ref()
                    .is_some_and(|v| v.len() != table.columns.len())
                {
                    return Err(refusal("analytics.engine", "values do not match the table"));
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
    /// Every version known at the frontier, superseded ones included (mirror only).
    versions: Option<Frag>,
    excluded: Vec<String>,
    /// The mirror's world and the frontier read (mirror only).
    at: Option<(String, u64)>,
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
                        "analytics.frontier_behind",
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
                let versions = Frag::fmt(
                    "(\":w\" = {} AND \":from\" <= {})",
                    &[&Frag::bind("?", Sql::Text(world.clone())), &at],
                );
                Ok(Self {
                    table: mirror_table(table),
                    columns: schema.columns,
                    all,
                    population,
                    versions: Some(versions),
                    excluded: excluded.clone(),
                    at: Some((world.clone(), *frontier)),
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
                    versions: None,
                    excluded: Vec::new(),
                    at: None,
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
        let rows =
            db(statement.query_map(params_from_iter(sql.params), |row| row.get::<_, String>(0)))?;
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
    db(
        statement.query_row(params_from_iter(sql.params.clone()), |row| {
            row.get::<_, u64>(0)
        }),
    )
}

/// A lowered question as SQL over its scope: per row the bucket, the
/// operand coefficients, the null flags and whether the exact engine
/// would refuse on reaching it.
struct Projection {
    sql: Frag,
    /// A snapshot's versions without their valid time, which refuse the question.
    untimed: Option<Frag>,
    arity: usize,
    /// One per dimension, in the question's order (`g{i}`, `r{i}`).
    groups: Vec<GroupKind>,
}

#[derive(Clone, Copy)]
enum GroupKind {
    Period(super::Grain),
    Text,
    Number,
    Bool,
}

impl Projection {
    /// The grouping columns.
    fn group_by(&self) -> String {
        (0..self.groups.len())
            .map(|i| format!("g{i}"))
            .collect::<Vec<_>>()
            .join(", ")
    }

    /// The bucket order, the same for every pass.
    fn group_order(&self) -> String {
        if self.groups.is_empty() {
            return "1".to_owned();
        }
        (0..self.groups.len())
            .map(|i| format!("g{i} NULLS FIRST"))
            .collect::<Vec<_>>()
            .join(", ")
    }
}

/// METRIC-SNAPSHOT-ASOF-1: per period, each current subject's last version
/// known at the frontier whose valid time is before the period's end (ties
/// to the later version), with the period's label as `":label"`; and the
/// count query of those subjects' versions that have no valid time.
fn as_of_versions(scope: &Scope, as_of: &super::AsOf) -> Outcome<(Frag, Frag)> {
    let versions = scope
        .versions
        .as_ref()
        .ok_or_else(|| refusal(UNSUPPORTED, "a snapshot over a dataset"))?;
    let column = scope.column(&as_of.column)?;
    if !matches!(column.kind, ColumnKind::Timestamp | ColumnKind::Date) {
        return Err(refusal(UNSUPPORTED, "a snapshot's time is not an instant"));
    }
    if as_of.periods.is_empty() {
        return Err(refusal(UNSUPPORTED, "a snapshot without periods"));
    }
    let periods = Frag::join(
        &as_of
            .periods
            .iter()
            .enumerate()
            .map(|(index, (label, (secs, nanos)))| {
                Frag::fmt(
                    &format!("(CAST({{}} AS VARCHAR), {{}}, {{}}, {index})"),
                    &[
                        &label.as_ref().map_or_else(
                            || Frag::raw("NULL"),
                            |label| Frag::bind("?", Sql::Text(label.clone())),
                        ),
                        &Frag::bind("?", Sql::BigInt(*secs)),
                        &Frag::bind("?", Sql::BigInt(i64::from(*nanos))),
                    ],
                )
            })
            .collect::<Vec<_>>(),
        ", ",
    );
    let (t, n) = (derived(&as_of.column, "t"), derived(&as_of.column, "n"));
    let table = &scope.table;
    let subjects = Frag::fmt(
        &format!("\":id\" IN (SELECT \":id\" FROM {table} WHERE {{}})"),
        &[&scope.population],
    );
    let pick = Frag::fmt(
        &format!(
            "SELECT b.\":label\", v.* FROM (VALUES {{}}) AS b(\":label\", \":et\", \":en\", \":pi\") \
             JOIN (SELECT * FROM {table} WHERE {{}} AND {{}}) AS v \
             ON (v.{t} < b.\":et\" OR (v.{t} = b.\":et\" AND v.{n} < b.\":en\")) \
             QUALIFY row_number() OVER (PARTITION BY b.\":pi\", v.\":id\" ORDER BY v.{t} DESC, v.{n} DESC, v.\":v\" DESC) = 1"
        ),
        &[&periods, versions, &subjects],
    );
    let untimed = Frag::fmt(
        &format!(
            "SELECT count(*) FROM {table} WHERE {{}} AND {{}} AND {} IS NULL",
            ident(&as_of.column)
        ),
        &[versions, &subjects],
    );
    Ok((pick, untimed))
}

/// The first day of `grain`'s UTC period holding the instant `t` (seconds),
/// in days since the epoch, in integer and calendar arithmetic only.
fn period_sql(t: &str, grain: super::Grain) -> String {
    let day = format!("(({t} - ((({t} % 86400) + 86400) % 86400)) // 86400)");
    let date = format!("(DATE '1970-01-01' + CAST({day} AS INTEGER))");
    let part = match grain {
        super::Grain::Day => return format!("CAST({day} AS BIGINT)"),
        super::Grain::Week => "week",
        super::Grain::Month => "month",
        super::Grain::Quarter => "quarter",
        super::Grain::Year => "year",
    };
    format!("datediff('day', DATE '1970-01-01', CAST(date_trunc('{part}', {date}) AS DATE))")
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
            (
                Frag::fmt("({} IS NOT NULL AND {} = {})", &[&c, &c, &bound]),
                never,
            )
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

/// METRIC-GRAIN-LINK-1: a linked time reads a mirrored link and an instant of
/// its target, at a frontier no edge of the link changed after (the mirror
/// keeps each edge's latest state).
fn check_link(conn: &Connection, scope: &Scope, link: &TimeLink) -> Outcome<()> {
    let Some((world, frontier)) = &scope.at else {
        return Err(refusal(UNSUPPORTED, "a linked time over a dataset"));
    };
    let known = meta(conn, "links")?.unwrap_or_default();
    if !known.split('\n').any(|name| name == link.link) {
        return Err(refusal(UNSUPPORTED, format!("no link table {}", link.link)));
    }
    let target = schema_of(conn, &format!("schema:{}", link.target))?;
    if !target.columns.iter().any(|column| {
        column.id == link.column && matches!(column.kind, ColumnKind::Timestamp | ColumnKind::Date)
    }) {
        return Err(refusal(UNSUPPORTED, "a linked time that is not an instant"));
    }
    let later = query_u64(
        conn,
        &Frag::fmt(
            &format!(
                "SELECT count(*) FROM {} WHERE \":w\" = {{}} AND \":c\" > {{}}",
                link_table(&link.link)
            ),
            &[
                &Frag::bind("?", Sql::Text(world.clone())),
                &Frag::bind("?", Sql::UBigInt(*frontier)),
            ],
        ),
    )?;
    if later > 0 {
        return Err(refusal(STALE, "a link changed after the answer's frontier"));
    }
    Ok(())
}

/// METRIC-GRAIN-LINK-1: the question's rows with their linked object's time
/// as of the frontier: `":lk"` when exactly one live edge reaches an object
/// current then, `":lm"` when more than one edge does, and the time as
/// `":link"` with its ordering key.
fn linked_rows(scope: &Scope, link: &TimeLink, rows: &Frag) -> Outcome<Frag> {
    let Some((world, frontier)) = &scope.at else {
        return Err(refusal(UNSUPPORTED, "a linked time over a dataset"));
    };
    let c = ident(&link.column);
    let (t, n) = (derived(&link.column, "t"), derived(&link.column, "n"));
    let world = Frag::bind("?", Sql::Text(world.clone()));
    let at = Frag::bind("?", Sql::UBigInt(*frontier));
    Ok(Frag::fmt(
        &format!(
            "SELECT b.*, (e.\":n\" = 1 AND tg.\":id\" IS NOT NULL) AS \":lk\", coalesce(e.\":n\", 0) > 1 AS \":lm\", \
             tg.{c} AS \":link\", tg.{t} AS \":link:t\", tg.{n} AS \":link:n\" \
             FROM ({{}}) AS b \
             LEFT JOIN (SELECT \":s\", count(*) AS \":n\", min(\":t\") AS \":tg\" FROM {} WHERE \":w\" = {{}} AND \":live\" GROUP BY \":s\") AS e ON e.\":s\" = b.\":id\" \
             LEFT JOIN (SELECT \":id\", {c}, {t}, {n} FROM {} WHERE \":w\" = {{}} AND \":from\" <= {{}} AND (\":to\" IS NULL OR \":to\" > {{}})) AS tg ON tg.\":id\" = e.\":tg\"",
            link_table(&link.link),
            mirror_table(&link.target),
        ),
        &[rows, &world, &world, &at, &at],
    ))
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
        let linked = query.time_link.is_some();
        if !(linked && window.column == TIME_LINK_COLUMN
            || matches!(
                scope.column(&window.column)?.kind,
                ColumnKind::Timestamp | ColumnKind::Date
            ))
        {
            return Err(refusal(UNSUPPORTED, "the window is not on an instant"));
        }
        let time = Frag::raw(ident(&window.column));
        let question = filters_sql(scope, &query.filters)?;
        let q = Frag::raw("\":q\"");
        let (base, reach, mut refusals, untimed) = if let Some(as_of) = &query.as_of {
            let (pick, untimed) = as_of_versions(scope, as_of)?;
            // Each picked version is read whatever its own instant: no window.
            let base = Frag::fmt(
                "SELECT *, {} AS \":q\", {} AS \":qr\" FROM ({}) AS pk",
                &[&question.holds, &question.refuse, &pick],
            );
            (base, q, vec![Frag::raw("\":qr\"")], Some(untimed))
        } else {
            // The base layer evaluates the question's filters once per row.
            let rows = Frag::fmt(
                &format!(
                    "SELECT *, {{}} AS \":q\", {{}} AS \":qr\" FROM {} WHERE {{}}",
                    scope.table
                ),
                &[&question.holds, &question.refuse, &scope.population],
            );
            let base = match &query.time_link {
                Some(link) => linked_rows(scope, link, &rows)?,
                None => rows,
            };
            let reach = Frag::fmt(
                "({} AND {} IS NOT NULL AND {} AND {})",
                &[
                    &q,
                    &time,
                    &instant_bound(&window.column, window.start, ">="),
                    &instant_bound(&window.column, window.end, "<"),
                ],
            );
            let refusals = if linked {
                // A linked object without its time, or more than one, refuses
                // as the exact engine does; no link is no time (`unlinked`).
                vec![
                    Frag::raw("\":qr\""),
                    Frag::fmt("({} AND \":lk\" AND {} IS NULL)", &[&q, &time]),
                    Frag::raw("\":lm\""),
                ]
            } else {
                vec![
                    Frag::raw("\":qr\""),
                    Frag::fmt("({} AND {} IS NULL)", &[&q, &time]),
                ]
            };
            (base, reach, refusals, None)
        };
        let mut select = vec![
            Frag::raw(format!("{} AS id", ident(":id"))),
            Frag::raw(format!("{} AS v", ident(":v"))),
        ];
        let mut groups = Vec::new();
        for (i, dimension) in query.dimensions.iter().enumerate() {
            let (group, representative, kind) = match dimension {
                // A snapshot's time grouping is the period it was read as of.
                super::Dimension::Period { .. } if query.as_of.is_some() => (
                    "\":label\"".to_owned(),
                    "\":label\"".to_owned(),
                    GroupKind::Text,
                ),
                super::Dimension::Period { column, grain } => {
                    if !(linked && column == TIME_LINK_COLUMN
                        || matches!(
                            scope.column(column)?.kind,
                            ColumnKind::Timestamp | ColumnKind::Date
                        ))
                    {
                        return Err(refusal(
                            UNSUPPORTED,
                            "a period of a value that is not an instant",
                        ));
                    }
                    // Reaching a row without the instant refuses, as the exact engine does.
                    refusals.push(Frag::fmt(
                        "({} AND {} IS NULL)",
                        &[&reach, &Frag::raw(ident(column))],
                    ));
                    let start = period_sql(&derived(column, "t"), *grain);
                    (start.clone(), start, GroupKind::Period(*grain))
                }
                super::Dimension::Column { column } => {
                    let c = ident(column);
                    match scope.column(column)?.kind {
                        ColumnKind::Text | ColumnKind::Timestamp | ColumnKind::Date => {
                            (format!("nfc_normalize({c})"), c, GroupKind::Text)
                        }
                        ColumnKind::Integer | ColumnKind::Decimal { .. } => (
                            c.clone(),
                            format!("CAST({c} AS VARCHAR)"),
                            GroupKind::Number,
                        ),
                        ColumnKind::Boolean => (c.clone(), c, GroupKind::Bool),
                        ColumnKind::Opaque => return Err(refusal(UNSUPPORTED, "an opaque bucket")),
                    }
                }
            };
            select.push(Frag::raw(format!("{group} AS g{i}")));
            select.push(Frag::raw(format!("{representative} AS r{i}")));
            groups.push(kind);
        }
        let mut counted = Vec::new();
        for (k, operand) in query.operands.iter().enumerate() {
            let column = scope.column(&operand.column)?;
            let c = ident(&operand.column);
            let own = filters_sql(scope, &operand.filters)?;
            if linked {
                // A row with no linked object counts as unlinked when some
                // operand's own filters hold; one that refuses refuses.
                let none = Frag::raw("(\":q\" AND NOT \":lk\" AND NOT \":lm\")");
                refusals.push(Frag::fmt("({} AND {})", &[&none, &own.refuse]));
                counted.push(Frag::fmt("({} AND {})", &[&none, &own.holds]));
            }
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
                        format!(
                            "({six} IS NOT NULL AND {six} = {c} AND abs(CAST({six} * 1000000 AS HUGEINT)) <= {MAX_VALUE})"
                        ),
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
        // METRIC-GRAIN-LINK-1: skipped and counted, never bucketed.
        select.push(if counted.is_empty() {
            Frag::raw("FALSE AS ul")
        } else {
            Frag::fmt("({}) AS ul", &[&Frag::join(&counted, " OR ")])
        });
        let arity = query.operands.len();
        let contributes = (0..arity)
            .map(|k| format!("a{k} IS NOT NULL OR n{k} = 1"))
            .collect::<Vec<_>>()
            .join(" OR ");
        let sql = Frag::fmt(
            &format!(
                "SELECT *, ({contributes}) AS contributes FROM (SELECT {{}}, ({{}}) AS refuse FROM ({{}}) AS base) AS p"
            ),
            &[
                &Frag::join(&select, ", "),
                &Frag::join(&refusals, " OR "),
                &base,
            ],
        );
        Ok(Self {
            sql,
            untimed,
            arity,
            groups,
        })
    }

    /// The bucket's key from its representatives, read from `index` on.
    fn group_key(&self, row: &duckdb::Row<'_>, index: usize) -> duckdb::Result<GroupKey> {
        let mut members = Vec::with_capacity(self.groups.len());
        for (offset, kind) in self.groups.iter().enumerate() {
            let at = index.saturating_add(offset);
            members.push(match kind {
                GroupKind::Period(grain) => {
                    row.get::<_, Option<i64>>(at)?
                        .map_or(GroupKey::Null, |day| GroupKey::Period {
                            grain: *grain,
                            day,
                        })
                }
                GroupKind::Text => row
                    .get::<_, Option<String>>(at)?
                    .map_or(GroupKey::Null, GroupKey::Text),
                GroupKind::Number => match row.get::<_, Option<String>>(at)? {
                    None => GroupKey::Null,
                    Some(text) => GroupKey::Number(
                        norn_ir::Decimal::parse(&text).map_or(text, |d| d.canonical()),
                    ),
                },
                GroupKind::Bool => row
                    .get::<_, Option<bool>>(at)?
                    .map_or(GroupKey::Null, GroupKey::Bool),
            });
        }
        Ok(match <[GroupKey; 1]>::try_from(members) {
            Ok([one]) => one,
            Err(members) if members.is_empty() => GroupKey::Null,
            Err(members) => GroupKey::Tuple(members),
        })
    }

    fn amounts(&self) -> String {
        (0..self.arity)
            .map(|k| format!("a{k}"))
            .collect::<Vec<_>>()
            .join(", ")
    }

    /// Materialize the projection once as `":p"` for the answer's passes.
    fn materialize(&self, conn: &Connection) -> Outcome<()> {
        let sql = Frag::fmt("CREATE OR REPLACE TEMP TABLE \":p\" AS {}", &[&self.sql]);
        let mut statement = db(conn.prepare(&sql.text))?;
        db(statement.execute(params_from_iter(sql.params)))?;
        Ok(())
    }

    /// MEMBERSHIP-DIGEST-1's line for one member, in SQL.
    fn member_line(&self) -> String {
        let amounts = (0..self.arity).fold(String::new(), |mut out, k| {
            out.push_str(" || ',' || COALESCE(CAST(a");
            out.push_str(&k.to_string());
            out.push_str(" AS VARCHAR), '')");
            out
        });
        format!(
            "CAST(strlen(id) AS VARCHAR) || ':' || id || ',' || CAST(v AS VARCHAR){amounts} || ';'"
        )
    }

    /// Per bucket of the materialized projection: totals, magnitudes, null
    /// counts, count and digest, and the members when `inline`.
    fn buckets(&self, conn: &Connection, inline: bool) -> Outcome<Vec<BucketTotals>> {
        let mut aggregates = Vec::new();
        for k in 0..self.arity {
            aggregates.push(format!("CAST(COALESCE(sum(a{k}), 0) AS VARCHAR)"));
            aggregates.push(format!(
                "CAST(COALESCE(sum(abs(CAST(a{k} AS HUGEINT))), 0) AS VARCHAR)"
            ));
            aggregates.push(format!("CAST(sum(n{k}) AS UBIGINT)"));
        }
        // One representative per dimension, all from the bucket's first member.
        let representatives = (0..self.groups.len()).fold(String::new(), |mut out, i| {
            out.push_str("arg_min(r");
            out.push_str(&i.to_string());
            out.push_str(", id), ");
            out
        });
        let dims = self.groups.len();
        let group_by = if dims == 0 {
            String::new()
        } else {
            format!(" GROUP BY {}", self.group_by())
        };
        let sql = format!(
            "SELECT {representatives}count(*) AS c, sha256(string_agg({}, '' ORDER BY id)) AS digest, {} FROM \":p\" WHERE contributes{group_by} ORDER BY {}",
            self.member_line(),
            aggregates.join(", "),
            self.group_order(),
        );
        let mut statement = db(conn.prepare(&sql))?;
        let arity = self.arity;
        let rows = db(statement.query_map([], |row| {
            let group = self.group_key(row, 0)?;
            let count: u64 = row.get(dims)?;
            let digest: String = row.get(dims.saturating_add(1))?;
            let mut totals = Vec::with_capacity(arity);
            let mut magnitude = Vec::with_capacity(arity);
            let mut nulls = Vec::with_capacity(arity);
            let mut column = dims.saturating_add(2);
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
                digest,
                members: inline.then(Vec::new),
            })
        }))?;
        let mut buckets: Vec<BucketTotals> = db(rows.collect())?;
        if !inline {
            return Ok(buckets);
        }
        let sql = format!(
            "SELECT dense_rank() OVER (ORDER BY {}) AS gk, id, v, {} FROM \":p\" WHERE contributes ORDER BY gk, id",
            self.group_order(),
            self.amounts()
        );
        let mut statement = db(conn.prepare(&sql))?;
        let mut rows = db(statement.query([]))?;
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
            buckets
                .get_mut(index)
                .and_then(|b| b.members.as_mut())
                .ok_or_else(|| refusal("analytics.engine", "a member outside its bucket"))?
                .push(member);
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
        let members: Vec<&GroupKey> = match (group, self.groups.len()) {
            (_, 0) => Vec::new(),
            (GroupKey::Tuple(members), n) if n > 1 && members.len() == n => {
                members.iter().collect()
            }
            (one, 1) => vec![one],
            _ => return Err(refusal(UNSUPPORTED, "a bucket of another shape")),
        };
        let mut conditions = vec![Frag::raw("TRUE")];
        for (i, (member, kind)) in members.into_iter().zip(&self.groups).enumerate() {
            let g = format!("g{i}");
            conditions.push(match (member, kind) {
                (GroupKey::Null, _) => Frag::raw(format!("{g} IS NULL")),
                (GroupKey::Bool(flag), GroupKind::Bool) => {
                    Frag::bind(&format!("{g} = ?"), Sql::Boolean(*flag))
                }
                (GroupKey::Number(text), GroupKind::Number) => {
                    Frag::bind(&format!("{g} = ?"), Sql::Text(text.clone()))
                }
                (GroupKey::Text(text), GroupKind::Text) => {
                    Frag::bind(&format!("{g} = nfc_normalize(?)"), Sql::Text(text.clone()))
                }
                (GroupKey::Period { grain, day }, GroupKind::Period(kind)) if grain == kind => {
                    Frag::bind(&format!("{g} = ?"), Sql::BigInt(*day))
                }
                (_, _) => return Err(refusal(UNSUPPORTED, "a bucket of another kind")),
            });
        }
        let matches = Frag::join(&conditions, " AND ");
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

/// One column of the single load pass: the typed value and, for instants,
/// the exact UTC ordering key, each parsed once from the text column `c`, and
/// the flags its reason is decided from. Shapes are checked on characters,
/// never by a pattern engine; casts are exact or yield null, never rounded.
struct Parsed {
    /// Typed value (and `t`/`n` for instants), named `v{i}`, `t{i}`, `n{i}`.
    columns: Vec<String>,
    /// The reason a present value is refused, or SQL NULL, over the columns above.
    reason: String,
}

#[allow(clippy::too_many_lines, reason = "one arm per column kind")]
fn parse_sql(kind: ColumnKind, i: usize) -> Parsed {
    let c = format!("c{i}");
    let v = format!("v{i}");
    match kind {
        ColumnKind::Text | ColumnKind::Opaque => Parsed {
            columns: vec![format!("{c} AS {v}")],
            reason: "NULL".to_owned(),
        },
        ColumnKind::Integer => Parsed {
            columns: vec![format!(
                "CASE WHEN translate({c}, '0123456789', '') IN ('', '-', '+') THEN TRY_CAST({c} AS DECIMAL(38,0)) END AS {v}"
            )],
            reason: format!("CASE WHEN {v} IS NULL THEN 'bulk.not_integer' END"),
        },
        ColumnKind::Decimal { scale } => Parsed {
            columns: vec![
                format!(
                    "translate({c}, '0123456789', '') IN ('', '.', '-', '+', '-.', '+.') AND NOT ltrim({c}, '+-') LIKE '.%' AND NOT {c} LIKE '%.' AS s{i}"
                ),
                format!(
                    "strpos({c}, '.') = 0 OR strlen({c}) - strpos({c}, '.') <= {scale} AS f{i}"
                ),
                format!(
                    "CASE WHEN translate({c}, '0123456789', '') IN ('', '.', '-', '+', '-.', '+.') AND (strpos({c}, '.') = 0 OR strlen({c}) - strpos({c}, '.') <= {scale}) THEN TRY_CAST({c} AS DECIMAL(38,{scale})) END AS {v}"
                ),
            ],
            reason: format!(
                "CASE WHEN NOT s{i} THEN 'bulk.not_decimal' WHEN NOT f{i} THEN 'bulk.scale_exceeded' WHEN {v} IS NULL THEN 'bulk.not_decimal' END"
            ),
        },
        ColumnKind::Boolean => Parsed {
            columns: vec![format!(
                "CASE lower({c}) WHEN 'true' THEN TRUE WHEN 'false' THEN FALSE END AS {v}"
            )],
            reason: format!("CASE WHEN {v} IS NULL THEN 'bulk.not_boolean' END"),
        },
        ColumnKind::Date => Parsed {
            columns: vec![
                format!("{c} AS {v}"),
                format!(
                    "CASE WHEN strlen({c}) = 10 AND translate({c}, '0123456789', '') = '--' AND substr({c}, 5, 1) = '-' AND substr({c}, 8, 1) = '-' \
                     THEN CAST(datediff('day', DATE '1970-01-01', TRY_CAST({c} AS DATE)) AS BIGINT) * 86400 END AS t{i}"
                ),
                format!("0 AS n{i}"),
            ],
            reason: format!("CASE WHEN t{i} IS NULL THEN 'bulk.not_date' END"),
        },
        ColumnKind::Timestamp => {
            // RFC 3339 as the cell reads it: `YYYY-MM-DDTHH:MM:SS`, then an
            // optional `.` and 1–9 digits, then `Z` or `±HH:MM`.
            let zone = format!(
                "CASE WHEN right({c}, 1) = 'Z' THEN 'Z' WHEN translate(right({c}, 6), '0123456789', '') IN ('+:', '-:') \
                 AND substr(right({c}, 6), 4, 1) = ':' AND substr(right({c}, 6), 2, 2) <= '23' AND substr(right({c}, 6), 5, 2) <= '59' THEN right({c}, 6) END"
            );
            let fraction = format!("substr({c}, 20, strlen({c}) - 19 - strlen({zone}))");
            let shape = format!(
                "strlen({c}) BETWEEN 20 AND 35 AND translate(substr({c}, 1, 19), '0123456789', '') = '--T::' \
                 AND substr({c}, 5, 1) = '-' AND substr({c}, 8, 1) = '-' AND substr({c}, 11, 1) = 'T' AND substr({c}, 14, 1) = ':' \
                 AND substr({c}, 17, 1) = ':' AND substr({c}, 12, 2) <= '23' AND substr({c}, 18, 2) <= '59' AND {zone} IS NOT NULL \
                 AND ({fraction} = '' OR (substr({fraction}, 1, 1) = '.' AND strlen({fraction}) BETWEEN 2 AND 10 AND translate(substr({fraction}, 2), '0123456789', '') = ''))"
            );
            let offset = format!(
                "CASE WHEN {zone} = 'Z' THEN 0 ELSE (CASE WHEN substr({zone}, 1, 1) = '-' THEN -1 ELSE 1 END) * \
                 (CAST(substr({zone}, 2, 2) AS BIGINT) * 3600 + CAST(substr({zone}, 5, 2) AS BIGINT) * 60) END"
            );
            Parsed {
                columns: vec![
                    format!("{c} AS {v}"),
                    format!(
                        "CASE WHEN {shape} THEN epoch_ms(TRY_CAST(substr({c}, 1, 19) AS TIMESTAMP)) // 1000 - {offset} END AS t{i}"
                    ),
                    format!(
                        "CASE WHEN {shape} THEN CAST(rpad(substr({fraction}, 2), 9, '0') AS INTEGER) END AS n{i}"
                    ),
                ],
                reason: format!("CASE WHEN t{i} IS NULL THEN 'bulk.not_timestamp' END"),
            }
        }
    }
}

/// The first declared pointwise law the parsed value `v{i}` breaks, in SQL;
/// NULL when it keeps them all. An enum value is quoted and compared exactly,
/// and a bound is a canonical decimal.
fn law_sql(column: &Column, i: usize) -> Outcome<String> {
    let v = format!("v{i}");
    let mut checks = Vec::new();
    for law in &column.laws {
        checks.push(match law {
            super::PointLaw::OneOf(values) => format!(
                "CASE WHEN {v} IN ({}) THEN NULL ELSE 'bulk.not_in_enum' END",
                values
                    .iter()
                    .map(|value| format!("'{}'", value.replace('\'', "''")))
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            super::PointLaw::Min(bound) | super::PointLaw::Max(bound) => {
                let exact = norn_ir::Decimal::parse(bound)
                    .ok()
                    .filter(|d| d.canonical() == *bound)
                    .ok_or_else(|| {
                        refusal(
                            "bulk.unsupported_law",
                            "a bound that is not a canonical decimal",
                        )
                    })?;
                let (op, code) = if matches!(law, super::PointLaw::Min(_)) {
                    ("<", "bulk.below_min")
                } else {
                    (">", "bulk.above_max")
                };
                format!("CASE WHEN {v} {op} {} THEN '{code}' END", exact.canonical())
            }
            super::PointLaw::MinLength(n) => {
                format!("CASE WHEN length({v}) < {n} THEN 'bulk.too_short' END")
            }
            super::PointLaw::MaxLength(n) => {
                format!("CASE WHEN length({v}) > {n} THEN 'bulk.too_long' END")
            }
            super::PointLaw::Unchecked => {
                return Err(refusal(
                    "bulk.unsupported_law",
                    format!(
                        "{} declares a constraint the engine does not check",
                        column.id
                    ),
                ));
            }
        });
    }
    Ok(if checks.is_empty() {
        "NULL".to_owned()
    } else {
        format!("coalesce({})", checks.join(", "))
    })
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

/// Read the file once into `":file"` (insertion order is the file's), then
/// in one pass into `":checked"`: per table column `c{i}` its text (found by
/// header or Parquet field name; an empty or blank field is null), its
/// parsed value, and `":reason"`, the first refusal in column order.
fn check_file(conn: &Connection, request: &LoadRequest, file: &Path) -> Outcome<()> {
    let path = file.to_string_lossy().replace('\'', "''");
    let source = match request.file.format {
        FileFormat::Csv => format!(
            "SELECT * FROM read_csv('{path}', header = true, all_varchar = true, delim = ',', quote = '\"', escape = '\"', strict_mode = true, null_padding = false)"
        ),
        FileFormat::Parquet => {
            format!("SELECT * FROM read_parquet('{path}', file_row_number = true)")
        }
    };
    db(conn.execute_batch(&format!(
        "CREATE OR REPLACE TEMP TABLE \":file\" AS {source}"
    )))?;
    let mut statement = db(conn.prepare(
        "SELECT column_name, data_type FROM information_schema.columns WHERE table_name = ':file'",
    ))?;
    let present: BTreeMap<String, String> = db(db(statement.query_map([], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
    }))?
    .collect::<Result<_, _>>())?;
    let mut texts = Vec::new();
    let mut parsed = Vec::new();
    let mut reasons = Vec::new();
    for (i, column) in request.table.columns.iter().enumerate() {
        let name = request.headers.get(&column.id).unwrap_or(&column.id);
        let Some(data_type) = present.get(name) else {
            return Err(refusal(
                "bulk.missing_column",
                format!("the file has no column {name}"),
            ));
        };
        let f = ident(name);
        let text = match (column.kind, data_type.as_str()) {
            (ColumnKind::Timestamp, kind) if kind.starts_with("TIMESTAMP") => {
                format!("strftime(CAST({f} AS TIMESTAMP), '%Y-%m-%dT%H:%M:%S.%fZ')")
            }
            (ColumnKind::Date, "DATE") => format!("strftime({f}, '%Y-%m-%d')"),
            _ => format!("CAST({f} AS VARCHAR)"),
        };
        texts.push(format!(
            "CASE WHEN trim({text}) = '' THEN NULL ELSE {text} END AS c{i}"
        ));
        let Parsed { columns, reason } = parse_sql(column.kind, i);
        parsed.extend(columns);
        let missing = if column.nullable {
            "NULL"
        } else {
            "'bulk.required'"
        };
        let laws = law_sql(column, i)?;
        reasons.push(format!(
            "CASE WHEN c{i} IS NULL THEN {missing} ELSE coalesce({reason}, {laws}) END"
        ));
    }
    let order = match request.file.format {
        FileFormat::Csv => "rowid",
        FileFormat::Parquet => "file_row_number",
    };
    let reason = if reasons.is_empty() {
        "NULL".to_owned()
    } else {
        format!("coalesce({})", reasons.join(", "))
    };
    let separator = if texts.is_empty() { "" } else { ", " };
    db(conn.execute_batch(&format!(
        "CREATE OR REPLACE TEMP TABLE \":checked\" AS SELECT *, {reason} AS \":reason\" FROM (\
         SELECT *{separator}{} FROM (SELECT CAST(row_number() OVER (ORDER BY {order}) AS UBIGINT) AS \":row\"{separator}{} FROM \":file\") AS t) AS p",
        parsed.join(", "),
        texts.join(", "),
    )))?;
    Ok(())
}

fn load_file(
    conn: &Connection,
    request: &LoadRequest,
    file: &Path,
) -> Outcome<(u64, BTreeMap<String, u64>)> {
    let table = &request.table;
    check_file(conn, request, file)?;
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
        columns.push(ident(&column.id));
        values.push(format!("v{i}"));
        if matches!(column.kind, ColumnKind::Timestamp | ColumnKind::Date) {
            columns.push(derived(&column.id, "t"));
            values.push(format!("t{i}"));
            columns.push(derived(&column.id, "n"));
            values.push(format!("n{i}"));
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
    let rows: u64 = db(
        conn.query_row(&format!("SELECT count(*) FROM {dataset}"), [], |row| {
            row.get(0)
        }),
    )?;
    let mut statement = db(conn.prepare(&format!(
        "SELECT \":reason\", count(*) FROM {quarantine} GROUP BY \":reason\" ORDER BY \":reason\""
    )))?;
    let reasons: BTreeMap<String, u64> = db(db(statement.query_map([], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, u64>(1)?))
    }))?
    .collect::<Result<_, _>>())?;
    db(conn.execute_batch("DROP TABLE \":checked\"; DROP TABLE \":file\";"))?;
    set_meta(conn, &format!("load:{}", table.name), &request.load)?;
    set_meta(
        conn,
        &format!("dataset:{}", table.name),
        &schema_text(table)?,
    )?;
    set_meta(
        conn,
        &format!("lineage:{}", request.load),
        &format!("{} {}", request.file.sha256, request.file.uri),
    )?;
    Ok((rows, reasons))
}

fn present(
    kind: ColumnKind,
    text: Option<String>,
    secs: Option<i64>,
    nanos: Option<i32>,
) -> Option<Value> {
    let text = text?;
    Some(match kind {
        ColumnKind::Integer | ColumnKind::Decimal { .. } => {
            Value::Number(norn_ir::Decimal::parse(&text).map_or(text, |d| d.canonical()))
        }
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
    let numeric = matches!(
        column.kind,
        ColumnKind::Integer | ColumnKind::Decimal { .. }
    );
    let counts = Frag::fmt(
        &format!(
            "SELECT count({c}), count(DISTINCT {c}) FROM {} WHERE {{}}",
            scope.table
        ),
        &[&scope.population],
    );
    let mut statement = db(conn.prepare(&counts.text))?;
    let (count, distinct): (u64, u64) = db(statement
        .query_row(params_from_iter(counts.params), |row| {
            Ok((row.get(0)?, row.get(1)?))
        }))?;
    let mut ranks = BTreeMap::new();
    if (numeric || temporal) && count > 0 {
        let wanted = Frag::join(
            &super::profile_ranks(count)
                .into_iter()
                .map(|rank| Frag::bind("?", Sql::UBigInt(rank)))
                .collect::<Vec<_>>(),
            ", ",
        );
        let (t, n) = if temporal {
            (derived(id, "t"), derived(id, "n"))
        } else {
            (
                "CAST(NULL AS BIGINT)".to_owned(),
                "CAST(NULL AS INTEGER)".to_owned(),
            )
        };
        // Temporal values are ordered by their text, as the exact profile orders them.
        let sql = Frag::fmt(
            &format!(
                "SELECT rn, CAST(v AS VARCHAR), t, n FROM (SELECT {c} AS v, {t} AS t, {n} AS n, row_number() OVER (ORDER BY {c}) AS rn FROM {} WHERE {{}} AND {c} IS NOT NULL) AS x WHERE rn IN ({{}})",
                scope.table
            ),
            &[&scope.population, &wanted],
        );
        let mut statement = db(conn.prepare(&sql.text))?;
        let found: Vec<(u64, Option<Value>)> =
            db(db(statement.query_map(params_from_iter(sql.params), |row| {
                Ok((
                    row.get::<_, u64>(0)?,
                    present(column.kind, row.get(1)?, row.get(2)?, row.get(3)?),
                ))
            }))?
            .collect::<Result<_, _>>())?;
        ranks.extend(
            found
                .into_iter()
                .filter_map(|(rank, value)| value.map(|v| (rank, v))),
        );
    }
    let labels = if temporal && count > 0 {
        Some(temporal_labels(conn, scope, &c)?)
    } else {
        None
    };
    let outliers = match column.kind {
        ColumnKind::Integer if count > 0 => outliers(conn, scope, &c, 0, &ranks, count)?,
        ColumnKind::Decimal { scale } if count > 0 => {
            outliers(conn, scope, &c, scale, &ranks, count)?
        }
        ColumnKind::Integer
        | ColumnKind::Decimal { .. }
        | ColumnKind::Text
        | ColumnKind::Boolean
        | ColumnKind::Timestamp
        | ColumnKind::Date
        | ColumnKind::Opaque => None,
    };
    Ok(ColumnProfile {
        column: id.to_owned(),
        count,
        nulls: rows.saturating_sub(count),
        distinct,
        ranks,
        labels,
        outliers,
    })
}

/// Day labels (the text's first 10 characters) when there are at most
/// [`super::PROFILE_DAY_BINS`], else month labels (the first 7).
fn temporal_labels(
    conn: &Connection,
    scope: &Scope,
    c: &str,
) -> Outcome<(String, Vec<(String, u64)>)> {
    let read = |width: u32| -> Outcome<Vec<(String, u64)>> {
        let sql = Frag::fmt(
            &format!(
                "SELECT left({c}, {width}) AS label, count(*) FROM {} WHERE {{}} AND {c} IS NOT NULL GROUP BY label ORDER BY label",
                scope.table
            ),
            &[&scope.population],
        );
        let mut statement = db(conn.prepare(&sql.text))?;
        let rows = db(statement.query_map(params_from_iter(sql.params), |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, u64>(1)?))
        }))?;
        db(rows.collect())
    };
    let days = read(10)?;
    if days.len() <= super::PROFILE_DAY_BINS {
        Ok(("day".to_owned(), days))
    } else {
        Ok(("month".to_owned(), read(7)?))
    }
}

/// Tukey's far-out values: fences from the nearest-rank quartiles in exact
/// decimal arithmetic; none when a fence cannot be held exactly.
fn outliers(
    conn: &Connection,
    scope: &Scope,
    c: &str,
    scale: u32,
    ranks: &BTreeMap<u64, Value>,
    count: u64,
) -> Outcome<Option<super::OutlierSummary>> {
    let quartile = |percent| match ranks.get(&super::nearest_rank(percent, count)) {
        Some(Value::Number(text)) => norn_ir::Decimal::parse(text).ok(),
        _ => None,
    };
    let (Some(q1), Some(q3)) = (quartile(25), quartile(75)) else {
        return Ok(None);
    };
    let fences = q3.checked_sub(q1).and_then(|iqr| {
        let span =
            (1..super::PROFILE_OUTLIER_K).try_fold(iqr, |total, _| total.checked_add(iqr))?;
        Some((q1.checked_sub(span)?, q3.checked_add(span)?))
    });
    let Some((lower, upper)) = fences else {
        return Ok(None);
    };
    let (lower, upper) = (lower.canonical(), upper.canonical());
    // Fences of declared-scale quartiles keep that scale; wider than the
    // column's storage they cannot be compared exactly, so none is reported.
    if [&lower, &upper]
        .iter()
        .any(|fence| norn_ir::Decimal::parse(fence).map_or(true, |d| d.scale() > scale))
    {
        return Ok(None);
    }
    let typed = format!("CAST(? AS DECIMAL(38,{scale}))");
    let fence = |text: &String| Frag::bind(&typed, Sql::Text(text.clone()));
    let bounds = |sql: &str| Frag::fmt(sql, &[&scope.population, &fence(&lower), &fence(&upper)]);
    let total = query_u64(
        conn,
        &bounds(&format!(
            "SELECT count(*) FROM {} WHERE {{}} AND ({c} < {{}} OR {c} > {{}})",
            scope.table
        )),
    )?;
    let examples_sql = Frag::fmt(
        &format!(
            "SELECT {}, CAST({c} AS VARCHAR) FROM {} WHERE {{}} AND ({c} < {{}} OR {c} > {{}}) ORDER BY CASE WHEN {c} < {{}} THEN {{}} - {c} ELSE {c} - {{}} END DESC, {} LIMIT {}",
            ident(":id"),
            scope.table,
            ident(":id"),
            super::PROFILE_OUTLIER_EXAMPLES
        ),
        &[
            &scope.population,
            &fence(&lower),
            &fence(&upper),
            &fence(&lower),
            &fence(&lower),
            &fence(&upper),
        ],
    );
    let mut statement = db(conn.prepare(&examples_sql.text))?;
    let examples: Vec<(String, String)> = db(db(statement.query_map(
        params_from_iter(examples_sql.params),
        |row| {
            let value: String = row.get(1)?;
            Ok((
                row.get::<_, String>(0)?,
                norn_ir::Decimal::parse(&value).map_or(value, |d| d.canonical()),
            ))
        },
    ))?
    .collect::<Result<_, _>>())?;
    Ok(Some(super::OutlierSummary {
        lower,
        upper,
        count: total,
        examples,
    }))
}
