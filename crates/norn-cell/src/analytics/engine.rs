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
    FrontierResponse, GroupKey, LandRequest, LandResponse, LandedFile, Literal, LoadRequest,
    LoadResponse, Lowered, MAX_PREDICATE_SCALE, MemberRow, MembersRequest, MembersResponse,
    MirrorRow, PageClause, PageOp, PageOperand, PageRequest, PageResponse, PageRow, PredicateKind,
    ProfileRequest, ProfileResponse, ResidueRow, RowPredicate, RuleCheck, RuleCount, SettleRequest,
    Source, StoreRequest, SyncRequest, TIME_LINK_COLUMN, TableSchema, TenantId, Test, TimeLink,
    ValidRange, Value, dataset_cache_id, hex,
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
/// The most rows one records page returns.
pub const MAX_PAGE_ROWS: u32 = 200;
/// The columns every exported row carries itself; no property is exported
/// under these names.
pub const EXPORT_RESERVED: [&str; 2] = ["id", "version"];
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

/// The residue rows of a landing: left to the per-row evaluator.
fn residue_table(name: &str) -> String {
    ident(&format!("r:{name}"))
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
        // MANIFEST-02: the tenant's private directory is its own; no other
        // tenant's connection names it.
        let private = self.private(tenant)?;
        let private = private.to_string_lossy().replace('\'', "''");
        db(conn.execute_batch(&format!(
            "SET autoinstall_known_extensions=false; SET autoload_known_extensions=false; \
             SET memory_limit='{}'; SET threads={}; SET temp_directory='{private}/spill'; \
             SET preserve_insertion_order=true; SET allowed_directories=['{scratch}', '{private}']; \
             SET enable_external_access=false; \
             CREATE TABLE IF NOT EXISTS \":meta\" (key VARCHAR PRIMARY KEY, value VARCHAR NOT NULL);",
            self.settings.memory_limit.replace('\'', ""),
            self.settings.threads.max(1),
        )))?;
        let conn = Arc::new(Mutex::new(conn));
        tenants.insert(tenant.clone(), Arc::clone(&conn));
        Ok(conn)
    }

    /// MANIFEST-02: the tenant's private directory, `<root>/t/<tenant>`,
    /// readable and writable by this process alone. It holds one directory
    /// per landing attempt (`scratch/<attempt>/`) and the landed files by
    /// digest (`objects/`).
    fn private(&self, tenant: &TenantId) -> Outcome<PathBuf> {
        let dir = self.settings.root.join("t").join(tenant.as_str());
        for sub in ["scratch", "objects", "spill"] {
            private_dir(&dir.join(sub))?;
        }
        private_dir(&dir)?;
        Ok(dir)
    }

    /// MANIFEST-02: the directory of one landing attempt of `tenant`.
    fn attempt_dir(&self, tenant: &TenantId, attempt: &str) -> Outcome<PathBuf> {
        let valid = !attempt.is_empty()
            && attempt.len() <= 80
            && attempt.bytes().all(|b| b.is_ascii_alphanumeric());
        if !valid {
            return Err(refusal(
                "analytics.invalid",
                "an attempt is 1 to 80 letters and digits",
            ));
        }
        Ok(self.private(tenant)?.join("scratch").join(attempt))
    }

    /// MANIFEST-02: begin a landing attempt of `tenant`: its directory,
    /// created here and only here (an attempt that exists already refuses),
    /// and its source file, created empty for the caller to write the
    /// downloaded object into. The landing reads that file and nothing else.
    ///
    /// # Errors
    /// `bulk.attempt_exists`, an invalid attempt, or an I/O failure.
    pub fn begin_attempt(
        &self,
        tenant: &TenantId,
        attempt: &str,
    ) -> Outcome<(PathBuf, std::fs::File)> {
        let dir = self.attempt_dir(tenant, attempt)?;
        match std::fs::create_dir(&dir) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                return Err(refusal("bulk.attempt_exists", "the landing attempt exists"));
            }
            Err(error) => return Err(refusal("analytics.engine", error.to_string())),
        }
        private_dir(&dir)?;
        let path = dir.join("source");
        let file = create_new(&path)?;
        Ok((path, file))
    }

    /// Remove a landing attempt's directory and everything in it.
    pub fn end_attempt(&self, tenant: &TenantId, attempt: &str) {
        if let Ok(dir) = self.attempt_dir(tenant, attempt) {
            let _removed = std::fs::remove_dir_all(dir);
        }
    }

    /// The landed data and held-set files of an attempt, for the caller to
    /// send to object storage.
    ///
    /// # Errors
    /// An invalid attempt.
    pub fn landed_files(&self, tenant: &TenantId, attempt: &str) -> Outcome<(PathBuf, PathBuf)> {
        let dir = self.attempt_dir(tenant, attempt)?;
        Ok((dir.join("data.parquet"), dir.join("held.parquet")))
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
            // ASOF-KNOWLEDGE-1: a version committed under another model is not
            // read as this one (the exact engine refuses it the same way).
            if let Some(model) = &scope.model {
                let other = Frag::fmt(
                    &format!(
                        "SELECT count(*) FROM {} WHERE {{}} AND \":model\" <> {{}}",
                        scope.table
                    ),
                    &[&scope.all, &Frag::bind("?", Sql::Text(model.clone()))],
                );
                if query_u64(conn, &other)? > 0 {
                    return Err(refusal(UNSUPPORTED, "a version of another model"));
                }
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
            load_locked(conn, request, file, bytes, started)
        })
    }

    /// MANIFEST-LAND-1 / MANIFEST-LAWS-1: load the attempt's source file
    /// (written into the file [`Self::begin_attempt`] created) under staging
    /// tables of the attempt alone (`<type>#<attempt>`), then check every
    /// rule set-based over the typed rows. A rule the engine cannot decide
    /// exactly for a row is left as that row's residue for the per-row
    /// evaluator ([`Self::settle`]); every other decision stands (LAW-04).
    /// With no residue the attempt's two Parquet files are written
    /// (`Self::landing`).
    ///
    /// LAW-01: nothing a reader sees changes. Staging is the attempt's own,
    /// a retry is another attempt, and a dataset is read only at a stored
    /// cache identity a committed manifest names. A refused step drops the
    /// attempt's staging.
    ///
    /// MANIFEST-02: every file is made read-only before it is hashed, in a
    /// directory only this process writes, so the bytes hashed are the bytes
    /// read and sent.
    ///
    /// # Errors
    /// As [`Self::load`], `bulk.unsupported_law` for a rule the engine does
    /// not type, `bulk.unsupported_encoding` for a column the file cannot
    /// carry exactly, or the files cannot be written or read back.
    pub fn land(&self, request: &LandRequest) -> Outcome<LandResponse> {
        let started = Instant::now();
        let load_request = &request.load;
        let dir = self.attempt_dir(&load_request.tenant, &request.attempt)?;
        let file = dir.join("source");
        seal(&file)?;
        let (digest, bytes) =
            sha256_file(&file).map_err(|error| refusal("bulk.unreadable", error.to_string()))?;
        if digest != load_request.file.sha256 {
            return Err(refusal(
                "bulk.digest_mismatch",
                "the file is not the recorded upload",
            ));
        }
        let staged_table = staged(&load_request.table, &request.attempt);
        let staged_load = LoadRequest {
            table: staged_table.clone(),
            ..load_request.clone()
        };
        let landed = self.with(&load_request.tenant, |conn| {
            if meta(conn, &format!("counts:{}", staged_table.name))?.is_some() {
                return Err(refusal("bulk.attempt_exists", "the attempt has landed"));
            }
            let landed = (|| {
                let load = load_locked(conn, &staged_load, &file, bytes, started)?;
                db(conn.execute_batch("BEGIN TRANSACTION"))?;
                let checked = check_rules(conn, &staged_table, request);
                if checked.is_err() {
                    let _rolled_back = conn.execute_batch("ROLLBACK");
                }
                checked?;
                db(conn.execute_batch("COMMIT"))?;
                set_meta(
                    conn,
                    &format!("counts:{}", staged_table.name),
                    &counts_text(&load)?,
                )?;
                self.landing(conn, request, load)
            })();
            if landed.is_err() {
                drop_staged(conn, &staged_table)?;
            }
            landed
        });
        let _source = std::fs::remove_file(&file);
        landed
    }

    /// MANIFEST-LAWS-1: record the per-row evaluator's findings for the
    /// residue rows up to `through`, then answer as [`Self::land`] does:
    /// the next residue rows, or the landed files once none remains. A
    /// finding of a rule the engine did not leave to that row refuses, and
    /// so does a step of an attempt that is not staged (another attempt's
    /// retry never resets this one's findings, LAW-01). A refused settle
    /// drops the attempt.
    ///
    /// # Errors
    /// `core.compute.stale_input` for an attempt that is not staged,
    /// `bulk.invalid_finding`, or an engine failure.
    pub fn settle(&self, request: &SettleRequest) -> Outcome<LandResponse> {
        let land = &request.land;
        let staged_table = staged(&land.load.table, &land.attempt);
        self.with(&land.load.tenant, |conn| {
            if meta(conn, &format!("load:{}", staged_table.name))?.as_deref()
                != Some(land.load.load.as_str())
            {
                return Err(refusal(STALE, "the landing attempt is not staged"));
            }
            let load: LoadResponse = serde_json::from_str(
                &meta(conn, &format!("counts:{}", staged_table.name))?
                    .ok_or_else(|| refusal(STALE, "the landing attempt is unknown"))?,
            )
            .map_err(|e| refusal("analytics.engine", e.to_string()))?;
            let settled = (|| {
                db(conn.execute_batch("BEGIN TRANSACTION"))?;
                let settled = settle_rows(conn, &staged_table, request);
                if settled.is_err() {
                    let _rolled_back = conn.execute_batch("ROLLBACK");
                }
                settled?;
                db(conn.execute_batch("COMMIT"))?;
                self.landing(conn, land, load)
            })();
            if settled.is_err() {
                drop_staged(conn, &staged_table)?;
            }
            settled
        })
    }

    /// The answer to a landing step: the next residue rows, or (none left)
    /// the attempt's written files and the findings by rule, after which
    /// the attempt's staging is dropped.
    ///
    /// LAW-02: the data file holds exactly the typed rows no rule holds
    /// back, and its valid time spans only them; the held-set file holds
    /// every row that could not be typed and every finding.
    fn landing(
        &self,
        conn: &Connection,
        request: &LandRequest,
        load: LoadResponse,
    ) -> Outcome<LandResponse> {
        let table = staged(&request.load.table, &request.attempt);
        let residue_total = query_u64(
            conn,
            &Frag::raw(format!(
                "SELECT count(DISTINCT \":row\") FROM {}",
                residue_table(&table.name)
            )),
        )?;
        let (findings, held_by_rules) = rule_counts(conn, &table)?;
        let mut response = LandResponse {
            landing: request.landing.clone(),
            attempt: request.attempt.clone(),
            load,
            data: None,
            held: None,
            valid_time: Vec::new(),
            findings,
            held_by_rules,
            residue_total,
            residue: Vec::new(),
            stored: None,
        };
        if residue_total > 0 {
            response.residue = residue_rows(conn, &table, request.residue_page)?;
            return Ok(response);
        }
        let (data, held) = self.landed_files(&request.load.tenant, &request.attempt)?;
        if data.exists() || held.exists() {
            return Err(refusal("bulk.attempt_exists", "the attempt has landed"));
        }
        let trusted = trusted_rows(&table);
        copy_parquet(
            conn,
            &format!(
                "SELECT {} FROM {} WHERE {trusted} ORDER BY \":row\"",
                data_columns(&table).join(", "),
                dataset_table(&table.name)
            ),
            &data,
        )?;
        copy_parquet(
            conn,
            &format!(
                "SELECT {} FROM {} ORDER BY \":row\", \":rule\" NULLS FIRST, \":reason\"",
                held_columns(&table).join(", "),
                quarantine_table(&table.name)
            ),
            &held,
        )?;
        let data_rows = query_u64(
            conn,
            &Frag::raw(format!(
                "SELECT count(*) FROM {} WHERE {trusted}",
                dataset_table(&table.name)
            )),
        )?;
        let held_rows = query_u64(
            conn,
            &Frag::raw(format!(
                "SELECT count(*) FROM {}",
                quarantine_table(&table.name)
            )),
        )?;
        response.valid_time = valid_ranges(conn, &table, &trusted)?;
        drop_staged(conn, &table)?;
        let landed = |path: &Path, rows: u64| -> Outcome<LandedFile> {
            seal(path)?;
            let (sha256, bytes) = sha256_file(path)
                .map_err(|error| refusal("analytics.engine", error.to_string()))?;
            Ok(LandedFile {
                sha256,
                rows,
                bytes,
            })
        };
        response.data = Some(landed(&data, data_rows)?);
        response.held = Some(landed(&held, held_rows)?);
        Ok(response)
    }

    /// MANIFEST-CACHE-1: keep an attempt's files, which the caller has sent
    /// to their objects, as the files of the cache identity
    /// [`dataset_cache_id`] names. Each file must still be the digest the
    /// request names; it moves into the tenant's object directory under that
    /// digest (an object already there must be the same digest). The
    /// attempt's directory is removed.
    ///
    /// # Errors
    /// `bulk.digest_mismatch`, or an I/O or engine failure.
    pub fn store(&self, request: &StoreRequest) -> Outcome<LandResponse> {
        let land = &request.land;
        let landed = &request.landed;
        let tenant = &land.load.tenant;
        let table = &land.load.table;
        if landed.landing != land.landing || landed.attempt != land.attempt {
            return Err(refusal(STALE, "the answer is not this landing's"));
        }
        let (Some(data_file), Some(held_file)) = (&landed.data, &landed.held) else {
            return Err(refusal(
                "analytics.invalid",
                "the landing has residue to settle",
            ));
        };
        let (data, held) = self.landed_files(tenant, &land.attempt)?;
        let objects = self.private(tenant)?.join("objects");
        let mut kept = Vec::new();
        for (path, file) in [(&data, data_file), (&held, held_file)] {
            if !is_hex64(&file.sha256) {
                return Err(refusal("analytics.invalid", "a digest is 64 hex digits"));
            }
            let target = objects.join(format!("{}.parquet", file.sha256));
            verify_object(path, &file.sha256)?;
            if target.exists() {
                verify_object(&target, &file.sha256)?;
            } else {
                std::fs::rename(path, &target)
                    .map_err(|error| refusal("analytics.engine", error.to_string()))?;
            }
            kept.push(target);
        }
        let cache = dataset_cache_id(table, &data_file.sha256, &held_file.sha256);
        let record = KeptLanding {
            table: table.clone(),
            data: KeptFile {
                path: kept.first().cloned().unwrap_or_default(),
                sha256: data_file.sha256.clone(),
                uri: request.data.uri.clone(),
            },
            held: KeptFile {
                path: kept.get(1).cloned().unwrap_or_default(),
                sha256: held_file.sha256.clone(),
                uri: request.held.uri.clone(),
            },
        };
        let text = serde_json::to_string(&record)
            .map_err(|error| refusal("analytics.engine", error.to_string()))?;
        self.with(tenant, |conn| {
            set_meta(conn, &format!("landing:{cache}"), &text)
        })?;
        self.end_attempt(tenant, &land.attempt);
        Ok(LandResponse {
            stored: Some(cache),
            ..landed.clone()
        })
    }

    /// One records page of a source (HOST-READ-3 on the engine): the rows
    /// every clause holds for, ordered by the keys (absent values last in
    /// either direction) then identity, from `offset`; and how many match.
    /// Text compares by bytes, numbers exactly, instants by their ordering key.
    ///
    /// # Errors
    /// An unknown column, a comparison its kind does not take, or a stale source.
    pub fn page(&self, request: &PageRequest) -> Outcome<PageResponse> {
        self.with(&request.tenant, |conn| {
            let scope = Scope::new(conn, &request.source)?;
            let mut conditions = vec![scope.population.clone()];
            if !request.ids.is_empty() {
                conditions.push(Frag::fmt("\":id\" IN ({})", &[&id_list(&request.ids)]));
            }
            for clause in &request.filters {
                conditions.push(page_clause(scope.column(&clause.column)?, clause)?);
            }
            let matches = Frag::join(&conditions, " AND ");
            let mut order = Vec::new();
            for key in &request.sort {
                let column = scope.column(&key.column)?;
                let direction = if key.descending { "DESC" } else { "ASC" };
                let keys = match column.kind {
                    ColumnKind::Timestamp | ColumnKind::Date => {
                        vec![derived(&column.id, "t"), derived(&column.id, "n")]
                    }
                    ColumnKind::Text
                    | ColumnKind::Integer
                    | ColumnKind::Decimal { .. }
                    | ColumnKind::Boolean => vec![ident(&column.id)],
                    ColumnKind::Opaque => {
                        return Err(refusal(UNSUPPORTED, format!("{} has no order", column.id)));
                    }
                };
                order.extend(
                    keys.into_iter()
                        .map(|key| format!("{key} {direction} NULLS LAST")),
                );
            }
            order.push(format!("{} ASC", ident(":id")));
            let count = Frag::fmt(
                &format!("SELECT count(*) FROM {} WHERE {{}}", scope.table),
                &[&matches],
            );
            let total: u64 = db(db(conn.prepare(&count.text))?
                .query_row(params_from_iter(count.params), |row| row.get(0)))?;
            let mut select = vec![ident(":id"), ident(":v")];
            for column in &scope.columns {
                select.push(format!("CAST({} AS VARCHAR)", ident(&column.id)));
                if matches!(column.kind, ColumnKind::Timestamp | ColumnKind::Date) {
                    select.push(derived(&column.id, "t"));
                    select.push(derived(&column.id, "n"));
                } else {
                    select.push("CAST(NULL AS BIGINT)".to_owned());
                    select.push("CAST(NULL AS INTEGER)".to_owned());
                }
            }
            let page = Frag::fmt(
                &format!(
                    "SELECT {} FROM {} WHERE {{}} ORDER BY {} LIMIT {{}} OFFSET {{}}",
                    select.join(", "),
                    scope.table,
                    order.join(", ")
                ),
                &[
                    &matches,
                    &Frag::bind("?", Sql::UInt(request.limit.min(MAX_PAGE_ROWS))),
                    &Frag::bind("?", Sql::UBigInt(request.offset)),
                ],
            );
            let mut statement = db(conn.prepare(&page.text))?;
            let rows = db(statement.query_map(params_from_iter(page.params), |row| {
                let mut values = BTreeMap::new();
                for (i, column) in scope.columns.iter().enumerate() {
                    let at = i.saturating_mul(3).saturating_add(2);
                    let value = present(
                        column.kind,
                        row.get(at)?,
                        row.get(at.saturating_add(1))?,
                        row.get(at.saturating_add(2))?,
                    );
                    if let Some(value) = value {
                        values.insert(column.id.clone(), value);
                    }
                }
                Ok(PageRow {
                    id: row.get(0)?,
                    version: row.get(1)?,
                    values,
                })
            }))?;
            Ok(PageResponse {
                total,
                rows: db(rows.collect())?,
            })
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
                if EXPORT_RESERVED.contains(&id.as_str()) {
                    return Err(refusal(
                        "analytics.invalid",
                        format!("{id} names the row's own column"),
                    ));
                }
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
                ":model VARCHAR",
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

#[allow(
    clippy::too_many_lines,
    reason = "one batch: stage, close superseded versions, insert current ones"
)]
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
            format!("{} VARCHAR", ident(":model")),
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
                    Sql::Text(row.model.clone()),
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
                "INSERT INTO {target} (\":w\", \":id\", \":v\", \":from\", \":to\", \":scope\", \":type\", \":bad\", \":model\"{separator}{}) \
                 SELECT ?, \":id\", \":v\", \":from\", lead(\":from\") OVER (PARTITION BY \":id\" ORDER BY \":v\", \":ord\"), \":scope\", \":type\", \":bad\", \":model\"{separator}{} \
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
    /// At a knowledge cutoff, the model every version read must be of.
    model: Option<String>,
}

impl Scope {
    fn new(conn: &Connection, source: &Source) -> Outcome<Self> {
        match source {
            Source::Mirror {
                world,
                table,
                frontier,
                excluded,
                model,
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
                    model: model.clone(),
                })
            }
            Source::Dataset { table, load } => {
                if meta(conn, &format!("load:{table}"))?.as_deref() != Some(load.as_str()) {
                    // MANIFEST-CACHE-1: a cache miss rebuilds the dataset
                    // from the files of exactly its cache identity.
                    hydrate(conn, table, load)?;
                }
                let schema = schema_of(conn, &format!("dataset:{table}"))?;
                // DATA-QUARANTINE / MANIFEST-LAWS-1: a row a rule holds back
                // is in no answer, as a held-back observation is in none.
                Ok(Self {
                    table: dataset_table(table),
                    columns: schema.columns,
                    all: Frag::raw("TRUE"),
                    population: Frag::raw(format!(
                        "\":row\" NOT IN (SELECT \":row\" FROM {} WHERE \":holds\" AND \":rule\" IS NOT NULL)",
                        quarantine_table(table)
                    )),
                    versions: None,
                    excluded: Vec::new(),
                    at: None,
                    model: None,
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

/// One records clause over `column`: every comparison but `empty` needs a
/// present value; a timestamp or date is present when it has its key.
/// One operand compared with `op` (`=`, `<>`, `<`, `<=`, `>`, `>=`) as
/// `column` compares: text by bytes, numbers exactly, instants by key.
fn page_compare(column: &Column, operand: &PageOperand, op: &str) -> Outcome<Frag> {
    let invalid = || {
        refusal(
            "analytics.invalid",
            format!("{} does not compare as asked", column.id),
        )
    };
    let c = ident(&column.id);
    match (column.kind, operand) {
        (ColumnKind::Text | ColumnKind::Opaque, PageOperand::Text(text)) => {
            Ok(Frag::bind(&format!("{c} {op} ?"), Sql::Text(text.clone())))
        }
        (ColumnKind::Integer | ColumnKind::Decimal { .. }, PageOperand::Number(text)) => {
            let decimal = norn_ir::Decimal::parse(text).map_err(|_unparsed| invalid())?;
            let canonical = decimal.canonical();
            let scale = canonical.split_once('.').map_or(0, |(_, f)| f.len());
            let digits = canonical.bytes().filter(u8::is_ascii_digit).count();
            if digits > 38 {
                return Err(invalid());
            }
            Ok(Frag::bind(
                &format!("{c} {op} CAST(? AS DECIMAL(38,{scale}))"),
                Sql::Text(canonical),
            ))
        }
        (ColumnKind::Boolean, PageOperand::Bool(flag)) => {
            Ok(Frag::bind(&format!("{c} {op} ?"), Sql::Boolean(*flag)))
        }
        (ColumnKind::Timestamp | ColumnKind::Date, PageOperand::Instant { secs, nanos }) => {
            let t = derived(&column.id, "t");
            let n = derived(&column.id, "n");
            let secs = Frag::bind("?", Sql::BigInt(*secs));
            let nanos = Frag::bind("?", Sql::BigInt(i64::from(*nanos)));
            let (strict, last) = match op {
                "=" => {
                    return Ok(Frag::fmt(
                        &format!("({t} = {{}} AND {n} = {{}})"),
                        &[&secs, &nanos],
                    ));
                }
                "<>" => {
                    return Ok(Frag::fmt(
                        &format!("NOT ({t} = {{}} AND {n} = {{}})"),
                        &[&secs, &nanos],
                    ));
                }
                "<" | "<=" => ("<", op),
                _ => (">", op),
            };
            Ok(Frag::fmt(
                &format!("({t} {strict} {{}} OR ({t} = {{}} AND {n} {last} {{}}))"),
                &[&secs, &secs, &nanos],
            ))
        }
        _ => Err(invalid()),
    }
}

fn page_clause(column: &Column, clause: &PageClause) -> Outcome<Frag> {
    let invalid = || {
        refusal(
            "analytics.invalid",
            format!("{} does not compare as asked", column.id),
        )
    };
    let temporal = matches!(column.kind, ColumnKind::Timestamp | ColumnKind::Date);
    let c = ident(&column.id);
    let held = if temporal {
        format!("{} IS NOT NULL", derived(&column.id, "t"))
    } else {
        format!("{c} IS NOT NULL")
    };
    let compare = |operand: &PageOperand, op: &str| page_compare(column, operand, op);
    let one = || match clause.operands.as_slice() {
        [operand] => Ok(operand),
        _ => Err(invalid()),
    };
    let test = match clause.op {
        PageOp::Empty => return Ok(Frag::raw(format!("NOT ({held})"))),
        PageOp::Present => return Ok(Frag::raw(held)),
        PageOp::Eq => compare(one()?, "=")?,
        PageOp::Ne => compare(one()?, "<>")?,
        PageOp::Lt => compare(one()?, "<")?,
        PageOp::Lte => compare(one()?, "<=")?,
        PageOp::Gt => compare(one()?, ">")?,
        PageOp::Gte => compare(one()?, ">=")?,
        PageOp::Between => match clause.operands.as_slice() {
            [low, high] => Frag::fmt(
                "({} AND {})",
                &[&compare(low, ">=")?, &compare(high, "<=")?],
            ),
            _ => return Err(invalid()),
        },
        PageOp::In => {
            if clause.operands.is_empty() {
                return Err(invalid());
            }
            let each = clause
                .operands
                .iter()
                .map(|operand| compare(operand, "="))
                .collect::<Outcome<Vec<_>>>()?;
            Frag::fmt("({})", &[&Frag::join(&each, " OR ")])
        }
        PageOp::Contains | PageOp::Prefix => match (column.kind, one()?) {
            (ColumnKind::Text, PageOperand::Text(text)) => {
                let function = if clause.op == PageOp::Contains {
                    "contains"
                } else {
                    "starts_with"
                };
                Frag::bind(&format!("{function}({c}, ?)"), Sql::Text(text.clone()))
            }
            _ => return Err(invalid()),
        },
    };
    Ok(Frag::fmt(&format!("({held} AND {{}})"), &[&test]))
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
                        // At a knowledge cutoff the exact engine reads values from
                        // their commitments, whose text is NFC; now, as written.
                        ColumnKind::Text | ColumnKind::Timestamp | ColumnKind::Date => {
                            let normal = format!("nfc_normalize({c})");
                            let shown = if scope.model.is_some() {
                                normal.clone()
                            } else {
                                c
                            };
                            (normal, shown, GroupKind::Text)
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
        // Without a grouping the aggregate still answers one row; an empty
        // contribution set is no bucket, as the exact engine answers it.
        let group_by = if dims == 0 {
            " HAVING count(*) > 0".to_owned()
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
            // RFC 3339 as the cell reads it (`norn_expr::temporal::
            // canonical_timestamp`): `YYYY-MM-DDTHH:MM:SS`, then an optional
            // `.` and 1–9 digits, then `Z` or `±HH:MM` other than `-00:00`
            // (an unknown local offset), and a UTC instant whose year is
            // 0000–9999.
            let zone = format!(
                "CASE WHEN right({c}, 1) = 'Z' THEN 'Z' WHEN substr(right({c}, 6), 1, 1) IN ('+', '-') AND translate(right({c}, 6), '0123456789', '') IN ('+:', '-:') \
                 AND substr(right({c}, 6), 4, 1) = ':' AND substr(right({c}, 6), 2, 2) <= '23' AND substr(right({c}, 6), 5, 2) <= '59' \
                 AND right({c}, 6) <> '-00:00' THEN right({c}, 6) END"
            );
            let fraction = format!("substr({c}, 20, strlen({c}) - 19 - strlen({zone}))");
            let shape = format!(
                "strlen({c}) BETWEEN 20 AND 35 AND translate(substr({c}, 1, 19), '0123456789', '') = '--T::' \
                 AND substr({c}, 5, 1) = '-' AND substr({c}, 8, 1) = '-' AND substr({c}, 11, 1) = 'T' AND substr({c}, 14, 1) = ':' \
                 AND substr({c}, 17, 1) = ':' AND substr({c}, 12, 2) <= '23' AND substr({c}, 15, 2) <= '59' AND substr({c}, 18, 2) <= '59' AND {zone} IS NOT NULL \
                 AND ({fraction} = '' OR (substr({fraction}, 1, 1) = '.' AND strlen({fraction}) BETWEEN 2 AND 10 AND translate(substr({fraction}, 2), '0123456789', '') = ''))"
            );
            let offset = format!(
                "CASE WHEN {zone} = 'Z' THEN 0 ELSE (CASE WHEN substr({zone}, 1, 1) = '-' THEN -1 ELSE 1 END) * \
                 (CAST(substr({zone}, 2, 2) AS BIGINT) * 3600 + CAST(substr({zone}, 5, 2) AS BIGINT) * 60) END"
            );
            let utc =
                format!("(epoch_ms(TRY_CAST(substr({c}, 1, 19) AS TIMESTAMP)) // 1000 - {offset})");
            Parsed {
                columns: vec![
                    format!("{c} AS {v}"),
                    // The UTC second must lie between 0000-01-01T00:00:00Z
                    // and 9999-12-31T23:59:59Z.
                    format!(
                        "CASE WHEN {shape} THEN CASE WHEN {utc} BETWEEN -62167219200 AND 253402300799 THEN {utc} END END AS t{i}"
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
/// header or Parquet field name), its parsed value, and `":reason"`, the
/// first refusal in column order. A CSV field has no null, so an empty or
/// blank one is null; a Parquet value is null only when the file says so,
/// and present text, empty or blank, is kept as it is (LAW-02/05: as the
/// per-row path reads it).
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
    let nanos = match request.file.format {
        FileFormat::Parquet => nanosecond_fields(conn, &path)?,
        FileFormat::Csv => std::collections::BTreeSet::new(),
    };
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
        // LAW-03: an instant is imported exactly or not at all: its every
        // nanosecond and its instant (never the session's zone), so a rule
        // decides the value the file holds.
        let lossy = || {
            refusal(
                "bulk.unsupported_encoding",
                format!("{name} holds instants this load cannot import exactly"),
            )
        };
        let text = match (column.kind, data_type.as_str()) {
            (ColumnKind::Timestamp, "TIMESTAMP_NS") => {
                // The second is taken in HUGEINT: near the lower bound of
                // TIMESTAMP_NS, the nanoseconds less their fraction are
                // below BIGINT's range though the microseconds are not.
                let fraction =
                    format!("(((epoch_ns({f}) % 1000000000) + 1000000000) % 1000000000)");
                format!(
                    "strftime(make_timestamp(CAST((CAST(epoch_ns({f}) AS HUGEINT) - {fraction}) // 1000 AS BIGINT)), '%Y-%m-%dT%H:%M:%S') \
                     || '.' || lpad(CAST({fraction} AS VARCHAR), 9, '0') || 'Z'"
                )
            }
            (ColumnKind::Timestamp, _) if nanos.contains(name) => return Err(lossy()),
            (ColumnKind::Timestamp, "TIMESTAMP WITH TIME ZONE") => {
                format!("strftime(make_timestamp(epoch_us({f})), '%Y-%m-%dT%H:%M:%S.%fZ')")
            }
            (ColumnKind::Timestamp, "TIMESTAMP" | "TIMESTAMP_MS" | "TIMESTAMP_S") => {
                format!("strftime(CAST({f} AS TIMESTAMP), '%Y-%m-%dT%H:%M:%S.%fZ')")
            }
            (ColumnKind::Timestamp, kind) if kind.starts_with("TIMESTAMP") => return Err(lossy()),
            (ColumnKind::Date, "DATE") => format!("strftime({f}, '%Y-%m-%d')"),
            _ => format!("CAST({f} AS VARCHAR)"),
        };
        texts.push(match request.file.format {
            FileFormat::Csv => {
                format!("CASE WHEN trim({text}) = '' THEN NULL ELSE {text} END AS c{i}")
            }
            FileFormat::Parquet => format!("{text} AS c{i}"),
        });
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

/// LAW-03: the Parquet fields that hold nanosecond instants (a `NANOS`
/// timestamp or a legacy `INT96` one). The engine imports such a field only
/// as `TIMESTAMP_NS`; read as anything narrower, it is refused.
fn nanosecond_fields(conn: &Connection, path: &str) -> Outcome<std::collections::BTreeSet<String>> {
    let mut statement = db(conn.prepare(&format!(
        "SELECT name, CAST(type AS VARCHAR), coalesce(CAST(logical_type AS VARCHAR), '') FROM parquet_schema('{path}')"
    )))?;
    let fields = db(db(statement.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, Option<String>>(1)?.unwrap_or_default(),
            row.get::<_, String>(2)?,
        ))
    }))?
    .collect::<Result<Vec<_>, _>>())?;
    Ok(fields
        .into_iter()
        .filter(|(_, physical, logical)| {
            physical == "INT96" || logical.contains("NANOS=NanoSeconds")
        })
        .map(|(name, _, _)| name)
        .collect())
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
         CREATE TABLE {quarantine} ({});",
        dataset_ddl(table),
        quarantine_ddl(table),
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
            "INSERT INTO {quarantine} SELECT \":row\", ?, \":reason\", NULL, TRUE{} FROM \":checked\" WHERE \":reason\" IS NOT NULL ORDER BY \":row\"",
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

/// The load step under the tenant's lock, in one transaction.
fn load_locked(
    conn: &Connection,
    request: &LoadRequest,
    file: &Path,
    bytes: u64,
    started: Instant,
) -> Outcome<LoadResponse> {
    db(conn.execute_batch("BEGIN TRANSACTION"))?;
    match load_file(conn, request, file) {
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
}

/// The quarantine (held-set) table: each held-back source row with its
/// reason, the rule that holds it (none for a row that could not be typed),
/// whether it holds the row back, and the row's text values.
fn quarantine_ddl(table: &TableSchema) -> String {
    let mut columns = vec![
        format!("{} UBIGINT", ident(":row")),
        format!("{} VARCHAR", ident(":load")),
        format!("{} VARCHAR", ident(":reason")),
        format!("{} VARCHAR", ident(":rule")),
        format!("{} BOOLEAN", ident(":holds")),
    ];
    columns.extend(
        table
            .columns
            .iter()
            .map(|c| format!("{} VARCHAR", ident(&c.id))),
    );
    columns.join(", ")
}

/// The data file's columns: the cache table's, without the landing identity.
fn data_columns(table: &TableSchema) -> Vec<String> {
    let mut columns = vec![ident(":id"), ident(":v"), ident(":row")];
    for column in &table.columns {
        columns.push(ident(&column.id));
        if matches!(column.kind, ColumnKind::Timestamp | ColumnKind::Date) {
            columns.push(derived(&column.id, "t"));
            columns.push(derived(&column.id, "n"));
        }
    }
    columns
}

/// The held-set file's columns: the quarantine table's, without the landing identity.
fn held_columns(table: &TableSchema) -> Vec<String> {
    let mut columns = vec![
        ident(":row"),
        ident(":reason"),
        ident(":rule"),
        ident(":holds"),
    ];
    columns.extend(table.columns.iter().map(|c| ident(&c.id)));
    columns
}

fn quoted_path(path: &Path) -> String {
    format!("'{}'", path.to_string_lossy().replace('\'', "''"))
}

/// Write one query's rows as a zstd Parquet file, in the query's order.
fn copy_parquet(conn: &Connection, query: &str, path: &Path) -> Outcome<()> {
    db(conn.execute_batch(&format!(
        "COPY ({query}) TO {} (FORMAT parquet, COMPRESSION zstd, ROW_GROUP_SIZE 122880)",
        quoted_path(path)
    )))
}

/// Each temporal column's earliest and latest admitted value, by instant.
fn valid_ranges(conn: &Connection, table: &TableSchema, rows: &str) -> Outcome<Vec<ValidRange>> {
    let dataset = dataset_table(&table.name);
    let mut ranges = Vec::new();
    for column in &table.columns {
        if !matches!(column.kind, ColumnKind::Timestamp | ColumnKind::Date) {
            continue;
        }
        let (c, t, n) = (
            ident(&column.id),
            derived(&column.id, "t"),
            derived(&column.id, "n"),
        );
        let end = |direction: &str| -> Outcome<Option<String>> {
            let mut statement = db(conn.prepare(&format!(
                "SELECT {c} FROM {dataset} WHERE {c} IS NOT NULL AND {rows} ORDER BY {t} {direction}, {n} {direction} LIMIT 1"
            )))?;
            let mut found = db(statement.query([]))?;
            match db(found.next())? {
                Some(row) => Ok(Some(db(row.get(0))?)),
                None => Ok(None),
            }
        };
        if let (Some(from), Some(to)) = (end("ASC")?, end("DESC")?) {
            ranges.push(ValidRange {
                column: column.id.clone(),
                from,
                to,
            });
        }
    }
    Ok(ranges)
}

/// LAW-02: the typed rows of a staged table that no rule holds back.
fn trusted_rows(table: &TableSchema) -> String {
    format!(
        "\":row\" NOT IN (SELECT \":row\" FROM {} WHERE \":holds\" AND \":rule\" IS NOT NULL)",
        quarantine_table(&table.name)
    )
}

fn counts_text(load: &LoadResponse) -> Outcome<String> {
    serde_json::to_string(load).map_err(|e| refusal("analytics.engine", e.to_string()))
}

/// The largest mantissa the per-row evaluator's decimal holds exactly
/// (`2^96 - 1`): a value whose scaled magnitude passes it is not computed
/// set-based.
const EXACT_MANTISSA: &str = "79228162514264337593543950335";

/// `|x| <= EXACT_MANTISSA / 10^scale`, and false when it cannot be computed.
fn in_range(value: &Frag, scale: u32) -> Outcome<Frag> {
    let digits = EXACT_MANTISSA.len();
    let scale_digits = usize::try_from(scale).map_err(|_e| refusal(LAW, "a scale"))?;
    let bound = if scale_digits == 0 {
        EXACT_MANTISSA.to_owned()
    } else {
        let split = digits
            .checked_sub(scale_digits)
            .ok_or_else(|| refusal(LAW, "a scale past the exact range"))?;
        format!(
            "{}.{}",
            EXACT_MANTISSA.get(..split).unwrap_or("0"),
            EXACT_MANTISSA.get(split..).unwrap_or_default()
        )
    };
    Ok(Frag::fmt(
        &format!("coalesce(TRY(abs({{}}) <= CAST('{bound}' AS DECIMAL(38,{scale}))), FALSE)"),
        &[value],
    ))
}

const LAW: &str = "bulk.unsupported_law";

/// LAW-05: the per-row evaluator compares text by its NFC form, the engine
/// by its bytes. They agree exactly where the text is already NFC, so text
/// that is not is left to the per-row evaluator as residue.
fn nfc_guard(text: &Frag) -> Frag {
    Frag::fmt("(nfc_normalize({}) = {})", &[text, text])
}

/// One lowered predicate: its SQL, null-free over rows whose reads are all
/// present, and the guards that must all hold for it to be exact.
struct LoweredRule {
    sql: Frag,
    kind: PredicateKind,
    guards: Vec<Frag>,
}

/// MANIFEST-LAWS-1: the SQL of `predicate` over `table`, with every value
/// bound. The predicate is typed again here, by the engine's own schema.
fn predicate_sql(predicate: &RowPredicate, table: &TableSchema) -> Outcome<LoweredRule> {
    if predicate.kind(table).is_none() {
        return Err(refusal(LAW, "a rule the engine does not type"));
    }
    lower_node(predicate, table)
}

/// A literal or column read: its SQL and the guard a number needs.
fn lower_leaf(node: &RowPredicate, table: &TableSchema) -> Outcome<LoweredRule> {
    use PredicateKind as K;
    use RowPredicate as P;
    Ok(match node {
        P::Null => return Err(refusal(LAW, "null outside an equality")),
        P::Bool(flag) => LoweredRule {
            sql: Frag::raw(if *flag { "TRUE" } else { "FALSE" }),
            kind: K::Bool,
            guards: Vec::new(),
        },
        P::Number(text) => {
            let Some(K::Number(scale)) = node.kind(table) else {
                return Err(refusal(LAW, "a number literal"));
            };
            let sql = Frag::bind(
                &format!("CAST(? AS DECIMAL(38,{scale}))"),
                Sql::Text(text.clone()),
            );
            let guard = in_range(&sql, scale)?;
            LoweredRule {
                sql,
                kind: K::Number(scale),
                guards: vec![guard],
            }
        }
        P::Text(text) => {
            let sql = Frag::bind("CAST(? AS VARCHAR)", Sql::Text(text.clone()));
            let guards = vec![nfc_guard(&sql)];
            LoweredRule {
                sql,
                kind: K::Text,
                guards,
            }
        }
        P::Column(id) => {
            let kind = node
                .kind(table)
                .ok_or_else(|| refusal(LAW, format!("no typed column {id}")))?;
            let sql = Frag::raw(ident(id));
            let guards = match kind {
                K::Number(scale) => vec![in_range(&sql, scale)?],
                K::Text => vec![nfc_guard(&sql)],
                K::Null | K::Bool => Vec::new(),
            };
            LoweredRule { sql, kind, guards }
        }
        P::Not(_)
        | P::Negate(_)
        | P::Add(..)
        | P::Subtract(..)
        | P::Multiply(..)
        | P::Equal(..)
        | P::NotEqual(..)
        | P::Less(..)
        | P::LessOrEqual(..)
        | P::Greater(..)
        | P::GreaterOrEqual(..)
        | P::And(..)
        | P::Or(..)
        | P::Conditional(..) => return Err(refusal(LAW, "not a literal or a column")),
    })
}

/// `a == b` or `a != b`; against `null` it is a null test.
fn lower_equality(
    equal: bool,
    a: &RowPredicate,
    b: &RowPredicate,
    table: &TableSchema,
) -> Outcome<LoweredRule> {
    use PredicateKind as K;
    use RowPredicate as P;
    Ok(match (a, b) {
        (P::Null, P::Null) => LoweredRule {
            sql: Frag::raw(if equal { "TRUE" } else { "FALSE" }),
            kind: K::Bool,
            guards: Vec::new(),
        },
        (P::Null, value) | (value, P::Null) => {
            let value = lower_node(value, table)?;
            let test = if equal { "IS NULL" } else { "IS NOT NULL" };
            LoweredRule {
                sql: Frag::fmt(&format!("({{}} {test})"), &[&value.sql]),
                kind: K::Bool,
                guards: value.guards,
            }
        }
        (a, b) => {
            let (a, b) = (lower_node(a, table)?, lower_node(b, table)?);
            let mut guards = a.guards;
            guards.extend(b.guards);
            let op = if equal { "=" } else { "<>" };
            LoweredRule {
                sql: Frag::fmt(&format!("({{}} {op} {{}})"), &[&a.sql, &b.sql]),
                kind: K::Bool,
                guards,
            }
        }
    })
}

fn lower_node(node: &RowPredicate, table: &TableSchema) -> Outcome<LoweredRule> {
    use PredicateKind as K;
    use RowPredicate as P;
    let binary = |op: &str, a: &RowPredicate, b: &RowPredicate, kind: K| -> Outcome<LoweredRule> {
        let (a, b) = (lower_node(a, table)?, lower_node(b, table)?);
        let mut guards = a.guards;
        guards.extend(b.guards);
        Ok(LoweredRule {
            sql: Frag::fmt(&format!("({{}} {op} {{}})"), &[&a.sql, &b.sql]),
            kind,
            guards,
        })
    };
    let number = |a: &RowPredicate, b: &RowPredicate| -> Outcome<(u32, u32)> {
        match (a.kind(table), b.kind(table)) {
            (Some(K::Number(x)), Some(K::Number(y))) => Ok((x, y)),
            _ => Err(refusal(LAW, "arithmetic on a value that is not a number")),
        }
    };
    let arithmetic =
        |op: &str, a: &RowPredicate, b: &RowPredicate, scale: u32| -> Outcome<LoweredRule> {
            let mut lowered = binary(op, a, b, K::Number(scale))?;
            lowered.guards.push(in_range(&lowered.sql, scale)?);
            Ok(lowered)
        };
    Ok(match node {
        P::Null | P::Bool(_) | P::Number(_) | P::Text(_) | P::Column(_) => lower_leaf(node, table)?,
        P::Not(inner) => {
            let inner = lower_node(inner, table)?;
            LoweredRule {
                sql: Frag::fmt("(NOT {})", &[&inner.sql]),
                kind: K::Bool,
                guards: inner.guards,
            }
        }
        P::Negate(inner) => {
            let inner = lower_node(inner, table)?;
            LoweredRule {
                sql: Frag::fmt("(- {})", &[&inner.sql]),
                kind: inner.kind,
                guards: inner.guards,
            }
        }
        P::Add(a, b) => {
            let (x, y) = number(a, b)?;
            arithmetic("+", a, b, x.max(y))?
        }
        P::Subtract(a, b) => {
            let (x, y) = number(a, b)?;
            arithmetic("-", a, b, x.max(y))?
        }
        P::Multiply(a, b) => {
            let (x, y) = number(a, b)?;
            arithmetic(
                "*",
                a,
                b,
                x.checked_add(y)
                    .filter(|s| *s <= MAX_PREDICATE_SCALE)
                    .ok_or_else(|| refusal(LAW, "a scale past the exact range"))?,
            )?
        }
        P::Equal(a, b) | P::NotEqual(a, b) => {
            lower_equality(matches!(node, P::Equal(..)), a, b, table)?
        }
        P::Less(a, b) => binary("<", a, b, K::Bool)?,
        P::LessOrEqual(a, b) => binary("<=", a, b, K::Bool)?,
        P::Greater(a, b) => binary(">", a, b, K::Bool)?,
        P::GreaterOrEqual(a, b) => binary(">=", a, b, K::Bool)?,
        P::And(a, b) => binary("AND", a, b, K::Bool)?,
        P::Or(a, b) => binary("OR", a, b, K::Bool)?,
        P::Conditional(c, a, b) => {
            let kind = node
                .kind(table)
                .ok_or_else(|| refusal(LAW, "a conditional the engine does not type"))?;
            let (c, a, b) = (
                lower_node(c, table)?,
                lower_node(a, table)?,
                lower_node(b, table)?,
            );
            let mut guards = c.guards;
            guards.extend(a.guards);
            guards.extend(b.guards);
            LoweredRule {
                sql: Frag::fmt(
                    "(CASE WHEN {} THEN {} ELSE {} END)",
                    &[&c.sql, &a.sql, &b.sql],
                ),
                kind,
                guards,
            }
        }
    })
}

/// MANIFEST-LAWS-1: one check's SQL over the cache table: whether the row is
/// residue (a read is absent, a guard fails, or the engine cannot compute
/// the predicate) and, if it is not, whether it fails the rule.
fn check_sql(check: &RuleCheck, table: &TableSchema) -> Outcome<(Frag, Frag)> {
    let Some(predicate) = &check.predicate else {
        // Outside the lowered subset: every row goes to the per-row evaluator.
        return Ok((Frag::raw("TRUE"), Frag::raw("FALSE")));
    };
    let mut reads = predicate.reads();
    if let Some(condition) = &check.condition {
        for read in condition.reads() {
            if !reads.contains(&read) {
                reads.push(read);
            }
        }
    }
    let predicate = predicate_sql(predicate, table)?;
    if predicate.kind != PredicateKind::Bool {
        return Err(refusal(LAW, "a rule whose predicate is not Boolean"));
    }
    let condition = match &check.condition {
        Some(condition) => {
            let lowered = predicate_sql(condition, table)?;
            if lowered.kind != PredicateKind::Bool {
                return Err(refusal(LAW, "a rule whose condition is not Boolean"));
            }
            Some(lowered)
        }
        None => None,
    };
    let mut residue: Vec<Frag> = reads
        .iter()
        .map(|read| Frag::raw(format!("{} IS NULL", ident(read))))
        .collect();
    let mut guards = predicate.guards.clone();
    if let Some(condition) = &condition {
        guards.extend(condition.guards.clone());
        residue.push(Frag::fmt("TRY({}) IS NULL", &[&condition.sql]));
    }
    residue.extend(guards.iter().map(|guard| Frag::fmt("NOT {}", &[guard])));
    residue.push(Frag::fmt("TRY({}) IS NULL", &[&predicate.sql]));
    let residue = Frag::fmt("({})", &[&Frag::join(&residue, " OR ")]);
    let expect = if check.expect { "TRUE" } else { "FALSE" };
    let fails = match &condition {
        Some(condition) => Frag::fmt(
            &format!("(TRY({{}}) AND TRY({{}}) <> {expect})"),
            &[&condition.sql, &predicate.sql],
        ),
        None => Frag::fmt(&format!("(TRY({{}}) <> {expect})"), &[&predicate.sql]),
    };
    Ok((residue, fails))
}

/// MANIFEST-LAWS-1 / LAW-04: check every rule over the staged typed rows
/// in one pass. For each rule, a row it cannot decide exactly is residue
/// of that rule alone (`(row, rule)`); every row it decides and fails is a
/// finding, whatever the row's other rules leave.
fn check_rules(conn: &Connection, table: &TableSchema, request: &LandRequest) -> Outcome<()> {
    let dataset = dataset_table(&table.name);
    let quarantine = quarantine_table(&table.name);
    let residue = residue_table(&table.name);
    db(conn.execute_batch(&format!(
        "DROP TABLE IF EXISTS {residue}; CREATE TABLE {residue} (\":row\" UBIGINT, \":rule\" VARCHAR, PRIMARY KEY (\":row\", \":rule\"));"
    )))?;
    if request.checks.is_empty() {
        return Ok(());
    }
    let mut select = vec![Frag::raw(ident(":row"))];
    let mut columns = vec![format!("{} UBIGINT", ident(":row"))];
    for (i, check) in request.checks.iter().enumerate() {
        let (residue, fails) = check_sql(check, table)?;
        select.push(Frag::fmt(&format!("{{}} AS \"r{i}\""), &[&residue]));
        select.push(Frag::fmt(&format!("{{}} AS \"f{i}\""), &[&fails]));
        columns.push(format!("\"r{i}\" BOOLEAN"));
        columns.push(format!("\"f{i}\" BOOLEAN"));
    }
    // Values are bound, so the outcome is inserted rather than created from a query.
    db(conn.execute_batch(&format!(
        "CREATE OR REPLACE TEMP TABLE \":outcome\" ({});",
        columns.join(", ")
    )))?;
    let outcome = Frag::fmt(
        &format!("INSERT INTO \":outcome\" SELECT {{}} FROM {dataset}"),
        &[&Frag::join(&select, ", ")],
    );
    db(conn.execute(&outcome.text, params_from_iter(outcome.params)))?;
    for (i, check) in request.checks.iter().enumerate() {
        db(conn.execute(
            &format!("INSERT INTO {residue} SELECT \":row\", ? FROM \":outcome\" WHERE \"r{i}\""),
            params_from_iter([Sql::Text(check.rule.clone())]),
        ))?;
        db(conn.execute(
            &format!(
                "INSERT INTO {quarantine} (\":row\", \":load\", \":reason\", \":rule\", \":holds\") \
                 SELECT \":row\", ?, ?, ?, ? FROM \":outcome\" WHERE NOT \"r{i}\" AND \"f{i}\""
            ),
            params_from_iter([
                Sql::Text(request.load.load.clone()),
                Sql::Text(check.label.clone()),
                Sql::Text(check.rule.clone()),
                Sql::Boolean(check.holds),
            ]),
        ))?;
    }
    db(conn.execute_batch("DROP TABLE \":outcome\";"))
}

/// MANIFEST-LAWS-1 / LAW-04: record the per-row findings of the residue
/// rows up to `through` and remove those rows from the residue. A finding
/// must name a rule the engine left to that row, once.
fn settle_rows(conn: &Connection, table: &TableSchema, request: &SettleRequest) -> Outcome<()> {
    let quarantine = quarantine_table(&table.name);
    let residue = residue_table(&table.name);
    let mut statement = db(conn.prepare(&format!(
        "SELECT count(*) FROM {residue} WHERE \":row\" = ? AND \":rule\" = ? AND \":row\" <= ?"
    )))?;
    let mut seen = std::collections::BTreeSet::new();
    for finding in &request.findings {
        let present: u64 = db(statement.query_row(
            params_from_iter([
                Sql::UBigInt(finding.row),
                Sql::Text(finding.rule.clone()),
                Sql::UBigInt(request.through),
            ]),
            |row| row.get(0),
        ))?;
        if present != 1 || !seen.insert((finding.row, finding.rule.clone())) {
            return Err(refusal(
                "bulk.invalid_finding",
                format!(
                    "row {} is not residue of that rule up to {}",
                    finding.row, request.through
                ),
            ));
        }
        db(conn.execute(
            &format!(
                "INSERT INTO {quarantine} (\":row\", \":load\", \":reason\", \":rule\", \":holds\") VALUES (?, ?, ?, ?, ?)"
            ),
            params_from_iter([
                Sql::UBigInt(finding.row),
                Sql::Text(request.land.load.load.clone()),
                Sql::Text(finding.label.clone()),
                Sql::Text(finding.rule.clone()),
                Sql::Boolean(finding.holds),
            ]),
        ))?;
    }
    db(conn.execute(
        &format!("DELETE FROM {residue} WHERE \":row\" <= ?"),
        params_from_iter([Sql::UBigInt(request.through)]),
    ))?;
    Ok(())
}

/// Findings by rule, and how many admitted rows a rule holds back.
fn rule_counts(
    conn: &Connection,
    table: &TableSchema,
) -> Outcome<(BTreeMap<String, RuleCount>, u64)> {
    let quarantine = quarantine_table(&table.name);
    let mut statement = db(conn.prepare(&format!(
        "SELECT \":rule\", min(\":reason\"), count(*), bool_or(\":holds\") FROM {quarantine} \
         WHERE \":rule\" IS NOT NULL GROUP BY \":rule\" ORDER BY \":rule\""
    )))?;
    let counts = db(db(statement.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            RuleCount {
                label: row.get(1)?,
                rows: row.get(2)?,
                holds: row.get(3)?,
            },
        ))
    }))?
    .collect::<Result<BTreeMap<_, _>, _>>())?;
    let held = query_u64(
        conn,
        &Frag::raw(format!(
            "SELECT count(DISTINCT \":row\") FROM {quarantine} WHERE \":rule\" IS NOT NULL AND \":holds\""
        )),
    )?;
    Ok((counts, held))
}

/// The first `limit` residue rows, in row order, with their present values.
fn residue_rows(conn: &Connection, table: &TableSchema, limit: u32) -> Outcome<Vec<ResidueRow>> {
    let mut select = vec![format!("d.{}", ident(":row"))];
    for column in &table.columns {
        select.push(format!("CAST(d.{} AS VARCHAR)", ident(&column.id)));
        if matches!(column.kind, ColumnKind::Timestamp | ColumnKind::Date) {
            select.push(format!("d.{}", derived(&column.id, "t")));
            select.push(format!("d.{}", derived(&column.id, "n")));
        } else {
            select.push("CAST(NULL AS BIGINT)".to_owned());
            select.push("CAST(NULL AS INTEGER)".to_owned());
        }
    }
    select.push("r.\":rules\"".to_owned());
    let mut statement = db(conn.prepare(&format!(
        "SELECT {} FROM {} AS d JOIN (SELECT \":row\", string_agg(\":rule\", chr(10) ORDER BY \":rule\") AS \":rules\" \
         FROM {} GROUP BY \":row\") AS r ON d.\":row\" = r.\":row\" ORDER BY d.\":row\" LIMIT ?",
        select.join(", "),
        dataset_table(&table.name),
        residue_table(&table.name),
    )))?;
    let rules_at = table.columns.len().saturating_mul(3).saturating_add(1);
    let rows = db(
        statement.query_map(params_from_iter([Sql::UInt(limit.max(1))]), |row| {
            let mut values = BTreeMap::new();
            for (i, column) in table.columns.iter().enumerate() {
                let at = i.saturating_mul(3).saturating_add(1);
                if let Some(value) = present(
                    column.kind,
                    row.get(at)?,
                    row.get(at.saturating_add(1))?,
                    row.get(at.saturating_add(2))?,
                ) {
                    values.insert(column.id.clone(), value);
                }
            }
            let rules: String = row.get(rules_at)?;
            Ok(ResidueRow {
                row: row.get(0)?,
                values,
                rules: rules.split('\n').map(str::to_owned).collect(),
            })
        }),
    )?;
    db(rows.collect())
}

/// MANIFEST-CACHE-1: the files a stored landing is kept as, by cache identity.
#[derive(Debug, serde::Serialize, serde::Deserialize)]
struct KeptLanding {
    table: TableSchema,
    data: KeptFile,
    held: KeptFile,
}

#[derive(Debug, serde::Serialize, serde::Deserialize)]
struct KeptFile {
    path: PathBuf,
    sha256: String,
    uri: String,
}

/// MANIFEST-CACHE-1: rebuild the dataset `table` at the cache identity
/// `load` from the files kept for it. Each file must hash to the digest the
/// identity was made of before any of it is read. The table is replaced in
/// one transaction, so a reader sees the old dataset or the new one whole.
fn hydrate(conn: &Connection, table: &str, load: &str) -> Outcome<()> {
    let cold = || {
        refusal(
            DATASET_COLD,
            "the dataset's files are not kept by this engine",
        )
    };
    let text = meta(conn, &format!("landing:{load}"))?.ok_or_else(cold)?;
    let kept: KeptLanding = serde_json::from_str(&text)
        .map_err(|error| refusal("analytics.engine", error.to_string()))?;
    if kept.table.name != table
        || dataset_cache_id(&kept.table, &kept.data.sha256, &kept.held.sha256) != load
    {
        return Err(refusal(STALE, "the kept landing is not this dataset"));
    }
    verify_object(&kept.data.path, &kept.data.sha256)?;
    verify_object(&kept.held.path, &kept.held.sha256)?;
    db(conn.execute_batch("BEGIN TRANSACTION"))?;
    let rebuilt = rebuild(conn, &kept, load);
    if rebuilt.is_err() {
        let _rolled_back = conn.execute_batch("ROLLBACK");
        return rebuilt;
    }
    db(conn.execute_batch("COMMIT"))
}

/// The cache table and quarantine of a kept landing, from its files.
fn rebuild(conn: &Connection, kept: &KeptLanding, load: &str) -> Outcome<()> {
    let table = &kept.table;
    let dataset = dataset_table(&table.name);
    let quarantine = quarantine_table(&table.name);
    db(conn.execute_batch(&format!(
        "DROP TABLE IF EXISTS {dataset}; DROP TABLE IF EXISTS {quarantine}; \
         CREATE TABLE {dataset} ({}); CREATE TABLE {quarantine} ({});",
        dataset_ddl(table),
        quarantine_ddl(table),
    )))?;
    let insert = |target: &str, columns: &[String], path: &Path| -> Outcome<()> {
        let mut names = columns.to_vec();
        names.push(ident(":load"));
        let mut values = columns.to_vec();
        values.push("?".to_owned());
        db(conn.execute(
            &format!(
                "INSERT INTO {target} ({}) SELECT {} FROM read_parquet({})",
                names.join(", "),
                values.join(", "),
                quoted_path(path)
            ),
            [load],
        ))?;
        Ok(())
    };
    insert(&dataset, &data_columns(table), &kept.data.path)?;
    insert(&quarantine, &held_columns(table), &kept.held.path)?;
    set_meta(conn, &format!("load:{}", table.name), load)?;
    set_meta(
        conn,
        &format!("dataset:{}", table.name),
        &schema_text(table)?,
    )?;
    set_meta(
        conn,
        &format!("lineage:{load}"),
        &format!("{} {}", kept.data.sha256, kept.data.uri),
    )
}

/// A refusal of a read whose dataset this engine cannot rebuild: the files
/// of its committed manifest are not kept here.
pub const DATASET_COLD: &str = "analytics.dataset_cold";

/// The staging schema of one landing attempt: the type's columns under a
/// name of the attempt alone (`<type>#<attempt>`), so a reader of the type,
/// or another attempt, never sees it.
fn staged(table: &TableSchema, attempt: &str) -> TableSchema {
    TableSchema {
        name: format!("{}#{attempt}", table.name),
        ..table.clone()
    }
}

/// Drop an attempt's staging tables and their records.
fn drop_staged(conn: &Connection, staged: &TableSchema) -> Outcome<()> {
    db(conn.execute_batch(&format!(
        "DROP TABLE IF EXISTS {}; DROP TABLE IF EXISTS {}; DROP TABLE IF EXISTS {};",
        dataset_table(&staged.name),
        quarantine_table(&staged.name),
        residue_table(&staged.name),
    )))?;
    for key in ["load", "dataset", "counts"] {
        db(conn.execute(
            "DELETE FROM \":meta\" WHERE key = ?",
            [format!("{key}:{}", staged.name)],
        ))?;
    }
    Ok(())
}

/// MANIFEST-02: create `dir` if it is missing and make it this process's alone.
fn private_dir(dir: &Path) -> Outcome<()> {
    let failed = |error: std::io::Error| refusal("analytics.engine", error.to_string());
    std::fs::create_dir_all(dir).map_err(failed)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700)).map_err(failed)?;
    }
    Ok(())
}

/// MANIFEST-02: create `path`, which must not exist, for this process alone.
fn create_new(path: &Path) -> Outcome<std::fs::File> {
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600);
    }
    options
        .open(path)
        .map_err(|error| refusal("analytics.engine", error.to_string()))
}

/// MANIFEST-02: make a written file read-only before it is hashed.
fn seal(path: &Path) -> Outcome<()> {
    let failed = |error: std::io::Error| refusal("bulk.unreadable", error.to_string());
    let mut permissions = std::fs::metadata(path).map_err(failed)?.permissions();
    permissions.set_readonly(true);
    std::fs::set_permissions(path, permissions).map_err(failed)
}

fn is_hex64(text: &str) -> bool {
    text.len() == 64
        && text
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// A kept object must hash to its digest before it is read.
fn verify_object(path: &Path, sha256: &str) -> Outcome<()> {
    let (digest, _bytes) =
        sha256_file(path).map_err(|error| refusal("bulk.unreadable", error.to_string()))?;
    if digest == sha256 {
        Ok(())
    } else {
        Err(refusal(
            "bulk.digest_mismatch",
            "a kept dataset file is not the one its manifest records",
        ))
    }
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
                // A reserved alias: a property may itself be named `label`.
                "SELECT left({c}, {width}) AS \":label\", count(*) FROM {} WHERE {{}} AND {c} IS NOT NULL GROUP BY \":label\" ORDER BY \":label\"",
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
