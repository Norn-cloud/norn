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
use serde_json::value::RawValue;
use transparency_sha2::{Digest as _, Sha256};

pub use super::backend::StoreBody;
use super::backend::{AnalyticalEngineId, ReadSnapshot, SnapshotProvenance};
use super::predicate::{
    FIRST_ZEN_SECOND, LAST_ZEN_SECOND, instant_nanos, zen_date_key, zen_timestamp_key,
};
use super::store_body::{legacy_land_response, legacy_store_request};
use super::{
    BucketTotals, Column, ColumnKind, ColumnProfile, EngineRefusal, EvaluateRequest,
    EvaluateResponse, ExportFormat, ExportRequest, FileFormat, Filter, FrontierRequest,
    FrontierResponse, GroupKey, LandRequest, LandResponse, LandedFile, LineageRequest,
    LineageResponse, Literal, LoadRequest, LoadResponse, Lowered, MAX_PREDICATE_SCALE, MemberRow,
    MembersRequest, MembersResponse, MirrorRow, PageClause, PageOp, PageOperand, PageRequest,
    PageResponse, PageRow, PredicateKind, ProfileRequest, ProfileResponse, ResidueRow, RowFinding,
    RowPredicate, RuleCheck, RuleCount, SettleRequest, Source, StoreRequest, SyncRequest,
    TIME_LINK_COLUMN, TableSchema, TenantId, Test, TimeLink, ValidRange, Value, dataset_cache_id,
    hex,
};
use super::{MAX_RESIDUE_PAGES, RESIDUE_EXCEEDED, RESIDUE_PAGE_BYTES, RESIDUE_PAGE_ROWS, row_fuel};

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
/// This engine's identity, as its answers' provenance names it.
pub const ENGINE_ID: AnalyticalEngineId = AnalyticalEngineId::DuckDb;
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
    /// The directory holding one `<tenant>.duckdb` per tenant and, under
    /// `tmp/`, one scratch directory per tenant.
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
    /// OLAP-02: the landing attempts this engine minted and still holds, by
    /// tenant and token. An attempt exists from the moment its directory is
    /// created until its one owner removes it; while an operation holds it
    /// (`leased`) no other operation may touch it.
    attempts: Mutex<BTreeMap<(TenantId, String), Held>>,
}

/// OLAP-02: one held landing attempt.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Held {
    /// An operation (the landing's download and load, a settle or a store)
    /// holds it.
    leased: bool,
    /// The landing contract it landed and the step it is at, once landed;
    /// only a settle (residue left) or a store (files written) of exactly
    /// that contract may take it.
    landed: Option<Landed>,
}

/// OLAP-02 (Sol r4): the landing contract of an attempt, recorded by the
/// engine at land time: the request it landed (by [`contract`]: landing
/// identity, load identity, table, file, binding, pins and rules) and its
/// latest answer. A later step must name exactly this request, and a store
/// exactly this answer; nothing a caller echoes replaces what the engine
/// recorded.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Landed {
    request: LandRequest,
    answer: LandResponse,
}

impl Landed {
    fn phase(&self) -> Phase {
        Phase::of(&self.answer)
    }
}

/// The immutable part of a landing request: all of it, with the attempt the
/// engine minted, but the presigned GET URL, which is minted per call.
fn contract(request: &LandRequest) -> LandRequest {
    let mut request = request.clone();
    request.load.file.url.clear();
    request
}

/// The immutable part of a store request: its landing's [`contract`], the
/// answer it stores and its objects' URIs, but the presigned PUT URLs,
/// which are minted per call.
fn store_contract(request: &StoreRequest) -> StoreRequest {
    let mut request = request.clone();
    request.land = contract(&request.land);
    request.data.url.clear();
    request.held.url.clear();
    request
}

/// OLAP-02 (Sol r4): the receipt of a store, kept by tenant and attempt: the
/// immutable request it was made by ([`store_contract`]) and the answer it
/// gave. A replay answers this answer, and only for this request.
#[derive(serde::Serialize)]
struct StoreReceipt {
    version: u8,
    request: StoreRequest,
    response: LandResponse,
}

const STORE_RECEIPT_VERSION: u8 = 1;

#[derive(Default, serde::Deserialize)]
#[serde(try_from = "u8")]
enum StoreReceiptVersion {
    #[default]
    Unversioned,
    Current,
}

impl TryFrom<u8> for StoreReceiptVersion {
    type Error = &'static str;

    fn try_from(version: u8) -> Result<Self, Self::Error> {
        if version == STORE_RECEIPT_VERSION {
            Ok(Self::Current)
        } else {
            Err("unknown store receipt version")
        }
    }
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct RecordedStore {
    // Unversioned receipts are from before the introduction of row laws.
    #[serde(default)]
    version: StoreReceiptVersion,
    request: Box<RawValue>,
    response: Box<RawValue>,
}

struct StoreAnswer {
    answer: LandResponse,
    recorded: Option<Box<RawValue>>,
}

fn json_text(value: &impl serde::Serialize) -> Outcome<String> {
    serde_json::to_string(value).map_err(|error| refusal("analytics.engine", error.to_string()))
}

fn receipt_key(attempt: &str) -> String {
    format!("store:{attempt}")
}

/// How far a landed attempt is: which next step may take it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase {
    /// Residue rows remain for the per-row evaluator: a settle's.
    Staged,
    /// The data and held-set files are written: a store's.
    Written,
}

impl Phase {
    fn of(response: &LandResponse) -> Self {
        if response.data.is_some() && response.held.is_some() {
            Self::Written
        } else {
            Self::Staged
        }
    }
}

/// A refusal of a landing attempt this engine does not hold (never minted,
/// or already removed by its owner).
pub const ATTEMPT_UNKNOWN: &str = "bulk.attempt_unknown";
/// A refusal of a landing attempt another operation holds: its download or
/// load is still running, or another settle or store of it is.
pub const ATTEMPT_BUSY: &str = "bulk.attempt_busy";

/// OLAP-02: exclusive use of one landing attempt, minted by the engine
/// ([`Engine::land`]) or taken by a settle or a store of its landing. The lease is the
/// attempt's one cleanup owner: dropped, it removes the attempt's directory
/// and forgets the attempt, unless it was released as landed. Nothing else
/// removes an attempt, and nothing names one but its engine-minted token.
struct Lease<'e> {
    engine: &'e Engine,
    tenant: TenantId,
    token: String,
    dir: PathBuf,
    /// Set when the attempt landed (or settled): the lease then releases
    /// it, kept, for the next step of exactly this contract.
    landed: Option<Landed>,
}

impl Lease<'_> {
    fn files(&self) -> (PathBuf, PathBuf) {
        (self.dir.join("data.parquet"), self.dir.join("held.parquet"))
    }
}

impl Drop for Lease<'_> {
    fn drop(&mut self) {
        let key = (self.tenant.clone(), std::mem::take(&mut self.token));
        if let Some(landed) = self.landed.take() {
            let mut attempts = self.engine.attempts();
            attempts.insert(
                key,
                Held {
                    leased: false,
                    landed: Some(landed),
                },
            );
            return;
        }
        // Held by this lease alone, so no other operation can be using the
        // directory, and its token is never minted again.
        let _removed = std::fs::remove_dir_all(&self.dir);
        self.engine.attempts().remove(&key);
    }
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
            attempts: Mutex::new(BTreeMap::new()),
        })
    }

    /// The scratch directory of `tenant` alone (`tmp/<tenant>/`), created
    /// if absent: its loads' downloads, its exports, and its database's
    /// `DuckDB` spill (`temp_directory`). It is also the only directory that
    /// tenant's database may read or write (`allowed_directories`), so no
    /// tenant's file is ever within reach of another tenant's database.
    ///
    /// # Errors
    /// The directory cannot be created.
    pub fn scratch(&self, tenant: &TenantId) -> std::io::Result<PathBuf> {
        let path = self.settings.root.join("tmp").join(tenant.as_str());
        std::fs::create_dir_all(&path)?;
        Ok(path)
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
        let scratch = self
            .scratch(tenant)
            .map_err(|error| refusal("analytics.engine", error.to_string()))?;
        let conn = db(Connection::open(&path))?;
        let scratch = scratch.to_string_lossy().replace('\'', "''");
        // MANIFEST-02: the tenant's private directory is its own; no other
        // tenant's connection names it.
        let private = self.private(tenant)?;
        let private = private.to_string_lossy().replace('\'', "''");
        db(conn.execute_batch(&format!(
            "SET autoinstall_known_extensions=false; SET autoload_known_extensions=false; \
             SET memory_limit='{}'; SET threads={}; SET temp_directory='{private}/spill'; \
             SET preserve_insertion_order=true; SET allowed_directories=['{scratch}/', '{private}/']; \
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

    fn attempts(&self) -> std::sync::MutexGuard<'_, BTreeMap<(TenantId, String), Held>> {
        self.attempts.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// OLAP-02 / MANIFEST-02: mint a landing attempt of `tenant`, held by
    /// the returned lease: a fresh token no caller chose, its directory
    /// created exclusively under the tenant's private scratch, and its
    /// source file, created empty for the download.
    fn begin_attempt(&self, tenant: &TenantId) -> Outcome<(Lease<'_>, std::fs::File)> {
        let failed = |error: std::io::Error| refusal("analytics.engine", error.to_string());
        let scratch = self.private(tenant)?.join("scratch");
        // Created exclusively under a random name (retried on a clash), so
        // the token names no directory that existed before, even one a
        // previous process left.
        let dir = tempfile::Builder::new()
            .prefix("a")
            .rand_bytes(31)
            .tempdir_in(&scratch)
            .map_err(failed)?
            .keep();
        let token = dir
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or_default()
            .to_owned();
        let key = (tenant.clone(), token);
        let mut attempts = self.attempts();
        if attempts.contains_key(&key) {
            // The directory is new, so this cannot happen; if it did, the
            // entry is another lease's and only the new directory goes.
            drop(attempts);
            let _removed = std::fs::remove_dir_all(&dir);
            return Err(refusal("analytics.engine", "a landing token was reused"));
        }
        attempts.insert(
            key.clone(),
            Held {
                leased: true,
                landed: None,
            },
        );
        drop(attempts);
        let lease = Lease {
            engine: self,
            tenant: key.0,
            token: key.1,
            dir,
            landed: None,
        };
        private_dir(&lease.dir)?;
        let file = create_new(&lease.dir.join("source"))?;
        Ok((lease, file))
    }

    /// OLAP-02: take the landed attempt `request.attempt` of its tenant for
    /// the step `phase` of `request`, with the answer its last step gave.
    /// An attempt this engine does not hold is [`ATTEMPT_UNKNOWN`]; one
    /// another operation holds (its download, its load, or another settle
    /// or store is still running) is [`ATTEMPT_BUSY`]; one landed by any
    /// other request (Sol r4: another landing, load, table, file, binding,
    /// pins or rules), or not at that step, or (for a store, `answer`)
    /// whose last answer is not `answer`, is stale; so is (for a settle,
    /// `through`) one whose last answer's residue page does not end at
    /// `through` (S5: a settle covers exactly the page the engine answered,
    /// never rows it has not shown). Every check is made before the attempt
    /// is taken, so a refusal touches nothing: the attempt, its staging and
    /// its files stay as they were.
    fn lease_attempt(
        &self,
        request: &LandRequest,
        phase: Phase,
        answer: Option<&LandResponse>,
        through: Option<u64>,
    ) -> Outcome<(Lease<'_>, LandResponse)> {
        let tenant = &request.load.tenant;
        let token = &request.attempt;
        let dir = self.private(tenant)?.join("scratch").join(token);
        let mut attempts = self.attempts();
        let Some(held) = attempts.get_mut(&(tenant.clone(), token.clone())) else {
            return Err(refusal(ATTEMPT_UNKNOWN, "the landing attempt is not held"));
        };
        if held.leased {
            return Err(refusal(ATTEMPT_BUSY, "the landing attempt is in use"));
        }
        let Some(landed) = held.landed.as_ref().filter(|landed| {
            landed.request == contract(request)
                && landed.phase() == phase
                && answer.is_none_or(|answer| *answer == landed.answer)
        }) else {
            return Err(refusal(
                STALE,
                "the landing attempt is another landing's or another step's",
            ));
        };
        if let Some(through) = through
            && landed.answer.residue.last().map(|row| row.row) != Some(through)
        {
            return Err(refusal(
                STALE,
                "the settle does not end at the residue page the engine answered",
            ));
        }
        let answer = landed.answer.clone();
        held.leased = true;
        drop(attempts);
        Ok((
            Lease {
                engine: self,
                tenant: tenant.clone(),
                token: token.clone(),
                dir,
                landed: None,
            },
            answer,
        ))
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
            stale |= meta(conn, "model_pin")?.as_deref() != Some(&request.model_pin);
            for table in &request.tables {
                stale |= meta(conn, &format!("pin:{}", table.name))?.as_deref() != Some(&table.pin);
            }
            if stale {
                reset_mirror(conn, &request.tables, &request.links, &request.model_pin)?;
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
    /// mirror's frontier, a batch encoded under another model or table schema
    /// than the mirror holds (`core.compute.stale_input`), or an unknown table.
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
            let mut same = meta(conn, "model_pin")?.as_deref() == Some(request.model_pin.as_str())
                && request
                    .rows
                    .iter()
                    .all(|row| request.schema_pins.contains_key(&row.table));
            for (table, pin) in &request.schema_pins {
                same &= meta(conn, &format!("pin:{table}"))?.as_deref() == Some(pin.as_str());
            }
            if !same {
                return Err(refusal(STALE, "the batch is of another mirror generation"));
            }
            in_transaction(conn, |conn| {
                apply_sync(conn, request)?;
                set_meta(conn, &key, &request.through.to_string())
            })?;
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
            let scope = Scope::new(conn, &request.source, Some(&request.model_pin))?;
            let population = match scope.verify_mirror_rows(conn, &request.source, true)? {
                Some(population) => population,
                None => query_u64(
                    conn,
                    &Frag::fmt(
                        &format!("SELECT count(*) FROM {} WHERE {{}}", scope.table),
                        &[&scope.all],
                    ),
                )?,
            };
            let excluded = scope.excluded_present(conn)?;
            if let Some(link) = &request.query.time_link {
                check_link(conn, &scope, link)?;
            }
            let projection = Projection::build(&request.query, &scope)?;
            if let Some(untimed) = &projection.untimed
                && query_u64(conn, untimed)? > 0
            {
                return Err(refusal(
                    UNSUPPORTED,
                    "a snapshot version without its valid time",
                ));
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
                provenance: scope.provenance(&request.tenant),
            })
        })
    }

    /// Page one bucket's members in identity order.
    ///
    /// # Errors
    /// As [`Self::evaluate`].
    pub fn members(&self, request: &MembersRequest) -> Outcome<MembersResponse> {
        self.with(&request.tenant, |conn| {
            let pin = pinned(&request.source, request.model_pin.as_deref())?;
            let scope = Scope::new(conn, &request.source, pin)?;
            let _population = scope.verify_mirror_rows(conn, &request.source, true)?;
            if let Some(link) = &request.query.time_link {
                check_link(conn, &scope, link)?;
            }
            let projection = Projection::build(&request.query, &scope)?;
            if let Some(untimed) = &projection.untimed
                && query_u64(conn, untimed)? > 0
            {
                return Err(refusal(
                    UNSUPPORTED,
                    "a snapshot version without its valid time",
                ));
            }
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
            load_locked(conn, request, file, bytes, started, Lineage::Write)
        })
    }

    /// MANIFEST-LAND-1 / MANIFEST-LAWS-1: land `request` in a fresh attempt.
    /// The engine mints the attempt (OLAP-02: a token no caller chooses, so
    /// `request.attempt` must be empty) and `fetch` writes the source object
    /// into the attempt's own source file. The file is loaded under staging
    /// tables of the attempt alone (`<type>#<attempt>`), then every rule is
    /// checked set-based over the typed rows. A rule the engine cannot
    /// decide exactly for a row is left as that row's residue for the
    /// per-row evaluator ([`Self::settle`]); every other decision stands
    /// (LAW-04). With no residue the attempt's two Parquet files are written
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
    /// The answer names the attempt; it stays held, for a settle of its
    /// residue or a store of its files. Until then the landing holds it, so
    /// no settle or store, replayed or not, can remove it while `fetch` is
    /// still writing; a failed landing removes it, and nothing else does.
    ///
    /// # Errors
    /// `analytics.invalid` for a caller-named attempt, what `fetch` refuses,
    /// as [`Self::load`], `bulk.unsupported_law` for a rule the engine does
    /// not type, `bulk.unsupported_encoding` for a column the file cannot
    /// carry exactly, [`RESIDUE_EXCEEDED`] for residue past
    /// [`MAX_RESIDUE_PAGES`] pages (S1: refused before any answer, its
    /// staging dropped), or the files cannot be written or read back.
    pub fn land(
        &self,
        request: &LandRequest,
        fetch: impl FnOnce(std::fs::File) -> Outcome<()>,
    ) -> Outcome<LandResponse> {
        if !request.attempt.is_empty() {
            return Err(refusal(
                "analytics.invalid",
                "the engine names a landing's attempt; send none",
            ));
        }
        let (mut lease, source) = self.begin_attempt(&request.load.tenant)?;
        fetch(source)?;
        let request = &LandRequest {
            attempt: lease.token.clone(),
            ..request.clone()
        };
        let started = Instant::now();
        let load_request = &request.load;
        let file = lease.dir.join("source");
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
            let landed = (|| {
                let load = load_locked(conn, &staged_load, &file, bytes, started, Lineage::Skip)?;
                in_transaction(conn, |conn| check_rules(conn, &staged_table, request))?;
                residue_fits(conn, &staged_table, request.residue_page)?;
                set_meta(
                    conn,
                    &format!("counts:{}", staged_table.name),
                    &counts_text(&load)?,
                )?;
                Self::landing(conn, &lease.dir, request, load)
            })();
            if landed.is_err() {
                drop_staged(conn, &staged_table)?;
            }
            landed
        });
        let _source = std::fs::remove_file(&file);
        let landed = landed?;
        lease.landed = Some(Landed {
            request: contract(request),
            answer: landed.clone(),
        });
        Ok(landed)
    }

    /// MANIFEST-LAWS-1: record the per-row evaluator's findings for the
    /// residue rows up to `through`, then answer as [`Self::land`] does:
    /// the next residue rows, or the landed files once none remains. A
    /// finding of a rule the engine did not leave to that row refuses, and
    /// so does a step of an attempt that is not staged (another attempt's
    /// retry never resets this one's findings, LAW-01).
    ///
    /// OLAP-02: the settle takes the attempt by its engine-minted token,
    /// only while it has residue, for exactly the request it landed (Sol r4:
    /// the landing contract recorded at land time, so a settle that echoes
    /// the landing but changes the load is refused before the attempt is
    /// taken), and while nothing else holds it; a refusal to take it touches
    /// nothing. Once taken, a refused settle drops the attempt's staging and
    /// removes it; it is the one owner.
    ///
    /// # Errors
    /// [`ATTEMPT_UNKNOWN`], [`ATTEMPT_BUSY`], `core.compute.stale_input`
    /// for an attempt that is not staged or a `through` that is not the
    /// last row of the page the engine answered (S5, before the attempt is
    /// taken), `bulk.invalid_finding`, or an engine failure.
    pub fn settle(&self, request: &SettleRequest) -> Outcome<LandResponse> {
        let land = &request.land;
        let (mut lease, _answer) =
            self.lease_attempt(land, Phase::Staged, None, Some(request.through))?;
        let staged_table = staged(&land.load.table, &land.attempt);
        let settled = self.with(&land.load.tenant, |conn| {
            // Taken: from here every refusal drops the staging with the
            // attempt, so nothing of it is left behind.
            let settled = (|| {
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
                in_transaction(conn, |conn| settle_rows(conn, &staged_table, request))?;
                Self::landing(conn, &lease.dir, land, load)
            })();
            if settled.is_err() {
                drop_staged(conn, &staged_table)?;
            }
            settled
        })?;
        lease.landed = Some(Landed {
            request: contract(land),
            answer: settled.clone(),
        });
        Ok(settled)
    }

    /// The answer to a landing step: the next residue rows, or (none left)
    /// the attempt's written files (in `dir`, the attempt's directory) and
    /// the findings by rule, after which the attempt's staging is dropped.
    ///
    /// LAW-02: the data file holds exactly the typed rows no rule holds
    /// back, and its valid time spans only them; the held-set file holds
    /// every row that could not be typed and every finding.
    fn landing(
        conn: &Connection,
        dir: &Path,
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
            response.residue = residue_page(conn, &table, 0, request.residue_page)?;
            return Ok(response);
        }
        let (data, held) = (dir.join("data.parquet"), dir.join("held.parquet"));
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
    /// [`dataset_cache_id`] names. Each file must still be the digest its
    /// landing answered; it moves into the tenant's object directory under
    /// that digest (an object already there must be the same digest).
    /// `send` sends the two files (data, held) to their objects first.
    ///
    /// OLAP-02: the store takes the attempt by its engine-minted token, and
    /// only once its files are written, for exactly the request it landed
    /// and the answer it gave (Sol r4: the answer stored is the engine's own
    /// record, never the caller's), and while nothing else holds it; it is
    /// then the attempt's one owner and removes it, stored or not. A store
    /// of an attempt still landing or settling is [`ATTEMPT_BUSY`] and
    /// touches nothing. A store keeps its receipt by tenant and attempt; a
    /// replay of an attempt already stored must be that receipt's request
    /// (only the presigned URLs may differ) and answers the receipt's
    /// answer, removing nothing; any other body is stale.
    ///
    /// # Errors
    /// [`ATTEMPT_UNKNOWN`], [`ATTEMPT_BUSY`], `core.compute.stale_input`,
    /// `bulk.digest_mismatch`, what `send` refuses, or an I/O or engine
    /// failure.
    pub fn store(
        &self,
        request: &StoreRequest,
        send: impl FnOnce(&Path, &Path) -> Outcome<()>,
    ) -> Outcome<LandResponse> {
        self.store_answer(request, send).map(|result| result.answer)
    }

    /// Store a call's landed files or return the exact recorded JSON body.
    ///
    /// # Errors
    /// The same refusals as [`Self::store`], a legacy body with no matching
    /// legacy receipt, or a response serialization failure.
    pub fn store_json(
        &self,
        body: &StoreBody,
        send: impl FnOnce(&Path, &Path) -> Outcome<()>,
    ) -> Outcome<Box<RawValue>> {
        let result = if body.is_legacy() {
            self.replayed(body.request(), true)?
                .ok_or_else(|| refusal(ATTEMPT_UNKNOWN, "the legacy receipt does not exist"))?
        } else {
            self.store_answer(body.request(), send)?
        };
        result.recorded.map_or_else(
            || {
                serde_json::value::to_raw_value(&result.answer)
                    .map_err(|error| refusal("analytics.engine", error.to_string()))
            },
            Ok,
        )
    }

    fn store_answer(
        &self,
        request: &StoreRequest,
        send: impl FnOnce(&Path, &Path) -> Outcome<()>,
    ) -> Outcome<StoreAnswer> {
        let land = &request.land;
        let tenant = &land.load.tenant;
        let table = &land.load.table;
        if request.landed.landing != land.landing || request.landed.attempt != land.attempt {
            return Err(refusal(STALE, "the answer is not this landing's"));
        }
        if request.landed.data.is_none() || request.landed.held.is_none() {
            return Err(refusal(
                "analytics.invalid",
                "the landing has residue to settle",
            ));
        }
        let leased = self.lease_attempt(land, Phase::Written, Some(&request.landed), None);
        let (lease, answer) = match leased {
            Ok(leased) => leased,
            Err(unknown) if unknown.code == ATTEMPT_UNKNOWN => {
                return self.replayed(request, false)?.ok_or(unknown);
            }
            Err(refused) => return Err(refused),
        };
        let (Some(data_file), Some(held_file)) = (&answer.data, &answer.held) else {
            return Err(refusal(
                "analytics.engine",
                "a written attempt names its files",
            ));
        };
        let (data, held) = lease.files();
        send(&data, &held)?;
        let objects = self.private(tenant)?.join("objects");
        let mut kept = Vec::new();
        for (path, file) in [(&data, data_file), (&held, held_file)] {
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
        let record = json_text(&record)?;
        let response = LandResponse {
            stored: Some(cache.clone()),
            ..answer
        };
        let receipt = json_text(&StoreReceipt {
            version: STORE_RECEIPT_VERSION,
            request: store_contract(request),
            response: response.clone(),
        })?;
        // The kept record, the receipt and the load's lineage commit
        // together, before the lease is released: a replay that finds the
        // attempt gone finds its receipt. Sol r3 S8: lineage is written only
        // here, once residue has settled and both files are sent, so a
        // landing refused before its store leaves none behind.
        self.with(tenant, |conn| {
            in_transaction(conn, |conn| {
                set_meta(conn, &format!("landing:{cache}"), &record)?;
                set_meta(conn, &receipt_key(&land.attempt), &receipt)?;
                set_lineage(conn, &land.load)
            })
        })?;
        drop(lease);
        Ok(StoreAnswer {
            answer: response,
            recorded: None,
        })
    }

    /// MANIFEST-LINEAGE-1: what the landing holds for one source row:
    /// whether it was admitted, why it was refused, and every rule it fails.
    /// Held-back rows are answered too: lineage reaches every source row.
    ///
    /// # Errors
    /// `analytics.dataset_cold` if the kept files are absent,
    /// `core.compute.stale_input` for another landing, or an engine failure.
    pub fn lineage(&self, request: &LineageRequest) -> Outcome<LineageResponse> {
        self.with(&request.tenant, |conn| {
            let table = &request.table;
            if meta(conn, &format!("load:{table}"))?.as_deref() != Some(request.load.as_str()) {
                // MANIFEST-CACHE-1: a cache miss rebuilds the dataset
                // from the files of exactly its cache identity.
                hydrate(conn, table, &request.load)?;
            }
            let admitted = query_u64(
                conn,
                &Frag::fmt(
                    &format!(
                        "SELECT count(*) FROM {} WHERE \":row\" = {{}}",
                        dataset_table(table)
                    ),
                    &[&Frag::bind("?", Sql::UBigInt(request.row))],
                ),
            )? == 1;
            let mut statement = db(conn.prepare(&format!(
                "SELECT \":reason\", \":rule\", \":holds\" FROM {} WHERE \":row\" = ? \
                 ORDER BY \":rule\" NULLS FIRST, \":reason\"",
                quarantine_table(table)
            )))?;
            let held: Vec<(String, Option<String>, bool)> = db(db(statement
                .query_map(params_from_iter([Sql::UBigInt(request.row)]), |row| {
                    Ok((row.get(0)?, row.get(1)?, row.get(2)?))
                }))?
            .collect::<Result<_, _>>())?;
            let mut response = LineageResponse {
                admitted,
                refused: None,
                findings: Vec::new(),
            };
            for (reason, rule, holds) in held {
                match rule {
                    Some(rule) => response.findings.push(RowFinding {
                        row: request.row,
                        rule,
                        label: reason,
                        holds,
                    }),
                    None => response.refused = Some(reason),
                }
            }
            Ok(response)
        })
    }

    /// OLAP-02 (Sol r4): the answer of a store of `request` that already
    /// happened, from the receipt kept for its tenant and attempt. Reads
    /// only. `None` when no store of the attempt happened; stale when one
    /// did, of any other request than this one ([`store_contract`]).
    fn replayed(&self, request: &StoreRequest, legacy: bool) -> Outcome<Option<StoreAnswer>> {
        let land = &request.land;
        let Some(text) = self.with(&land.load.tenant, |conn| {
            meta(conn, &receipt_key(&land.attempt))
        })?
        else {
            return Ok(None);
        };
        let decode_error =
            |error: serde_json::Error| refusal("analytics.engine", error.to_string());
        let receipt: RecordedStore = serde_json::from_str(&text).map_err(decode_error)?;
        let recorded_request: StoreRequest = match receipt.version {
            StoreReceiptVersion::Unversioned => {
                legacy_store_request(&receipt.request).map_err(decode_error)?
            }
            StoreReceiptVersion::Current if !legacy => {
                serde_json::from_str(receipt.request.get()).map_err(decode_error)?
            }
            StoreReceiptVersion::Current => {
                return Err(refusal(STALE, "the store is another request's"));
            }
        };
        if recorded_request != store_contract(request) {
            return Err(refusal(STALE, "the store is another request's"));
        }
        let answer = if matches!(receipt.version, StoreReceiptVersion::Unversioned) {
            legacy_land_response(&receipt.response).map_err(decode_error)?
        } else {
            serde_json::from_str(receipt.response.get()).map_err(decode_error)?
        };
        Ok(Some(StoreAnswer {
            answer,
            recorded: Some(receipt.response),
        }))
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
            let pin = pinned(&request.source, request.model_pin.as_deref())?;
            let scope = Scope::new(conn, &request.source, pin)?;
            let _population = scope.verify_mirror_rows(conn, &request.source, false)?;
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
            let scope = Scope::new(conn, &request.source, Some(&request.model_pin))?;
            let _population = scope.verify_mirror_rows(conn, &request.source, false)?;
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
                provenance: scope.provenance(&request.tenant),
            })
        })
    }

    /// Export columns of every row of a source into a scratch file the
    /// caller streams and then deletes.
    ///
    /// # Errors
    /// An unknown column or a stale source.
    pub fn export(&self, request: &ExportRequest) -> Outcome<PathBuf> {
        let scratch = self
            .scratch(&request.tenant)
            .map_err(|error| refusal("analytics.engine", error.to_string()))?;
        let suffix = match request.format {
            ExportFormat::Parquet => ".parquet",
            ExportFormat::Jsonl => ".jsonl",
        };
        let staged = tempfile::Builder::new()
            .prefix("export-")
            .suffix(suffix)
            .tempfile_in(&scratch)
            .map_err(|error| refusal("analytics.engine", error.to_string()))?
            .into_temp_path();
        self.with(&request.tenant, |conn| {
            let pin = pinned(&request.source, request.model_pin.as_deref())?;
            let scope = Scope::new(conn, &request.source, pin)?;
            let _population = scope.verify_mirror_rows(conn, &request.source, false)?;
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
            let target = staged.to_string_lossy().replace('\'', "''");
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
            let copied = db(conn.execute_batch(&format!(
                "COPY \":export\" TO '{target}' ({format});"
            )));
            let _dropped = conn.execute_batch("DROP TABLE IF EXISTS \":export\";");
            copied
        })?;
        staged
            .keep()
            .map_err(|error| refusal("analytics.engine", error.to_string()))
    }
}

/// N1: a follow-up read of a mirror names the generation it is bound to.
fn pinned<'p>(source: &Source, pin: Option<&'p str>) -> Outcome<Option<&'p str>> {
    match (source, pin) {
        (Source::Mirror { .. }, None) => Err(refusal(STALE, "a mirror read names no model")),
        (Source::Mirror { .. } | Source::Dataset { .. }, pin) => Ok(pin),
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

fn in_transaction<T>(
    conn: &Connection,
    body: impl FnOnce(&Connection) -> Outcome<T>,
) -> Outcome<T> {
    db(conn.execute_batch("BEGIN TRANSACTION"))?;
    match body(conn) {
        Ok(value) => {
            db(conn.execute_batch("COMMIT"))?;
            Ok(value)
        }
        Err(error) => {
            let _rolled_back = conn.execute_batch("ROLLBACK");
            Err(error)
        }
    }
}

fn reset_mirror(
    conn: &Connection,
    tables: &[TableSchema],
    links: &[String],
    model_pin: &str,
) -> Outcome<()> {
    in_transaction(conn, |conn| {
        let mut statement = db(conn.prepare(
            "SELECT table_name FROM information_schema.tables WHERE table_name LIKE 'm:%' OR table_name LIKE 'k:%'",
        ))?;
        let names: Vec<String> =
            db(db(statement.query_map([], |row| row.get(0)))?.collect::<Result<_, _>>())?;
        for name in names {
            db(conn.execute_batch(&format!("DROP TABLE {}", ident(&name))))?;
        }
        db(conn.execute_batch(
            "DELETE FROM \":meta\" WHERE key LIKE 'frontier:%' OR key LIKE 'pin:%' OR key = 'model_pin'",
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
        set_meta(conn, "model_pin", model_pin)?;
        Ok(())
    })
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
    /// The snapshot this scope reads, as an answer names it.
    read: ReadSnapshot,
}

impl Scope {
    fn new(conn: &Connection, source: &Source, expected_model_pin: Option<&str>) -> Outcome<Self> {
        match source {
            Source::Mirror {
                world,
                table,
                frontier,
                scope,
                excluded,
                model,
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
                let model_pin = meta(conn, "model_pin")?
                    .filter(|pin| expected_model_pin.is_none_or(|expected| pin == expected))
                    .ok_or_else(|| refusal(STALE, "the mirror is of another model"))?;
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
                    all,
                    population,
                    versions: Some(versions),
                    excluded: excluded.clone(),
                    at: Some((world.clone(), *frontier)),
                    model: model.clone(),
                    read: ReadSnapshot::Mirror {
                        world: world.clone(),
                        table: table.clone(),
                        frontier: *frontier,
                        scope: scope.clone(),
                        model_pin,
                        schema_pin: schema.pin,
                    },
                    columns: schema.columns,
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
                    read: ReadSnapshot::Dataset {
                        table: table.clone(),
                        load: load.clone(),
                    },
                })
            }
        }
    }

    /// Enforce mirror row scope and model pin (and `:bad` when `check_bad` is
    /// set), returning the mirror population count (`None` for dataset sources).
    fn verify_mirror_rows(
        &self,
        conn: &Connection,
        source: &Source,
        check_bad: bool,
    ) -> Outcome<Option<u64>> {
        let Source::Mirror { scope, .. } = source else {
            return Ok(None);
        };
        let checks = Frag::fmt(
            &format!(
                "SELECT count(*), count(*) FILTER (WHERE \":scope\" <> {{}}), count(*) FILTER (WHERE {}) FROM {} WHERE {{}}",
                ident(":bad"),
                self.table
            ),
            &[&Frag::bind("?", Sql::Text(scope.clone())), &self.all],
        );
        let mut statement = db(conn.prepare(&checks.text))?;
        let (population, other_scope, bad): (u64, u64, u64) = db(statement
            .query_row(params_from_iter(checks.params), |row| {
                Ok((row.get(0)?, row.get(1)?, row.get(2)?))
            }))?;
        if other_scope > 0 {
            return Err(refusal(STALE, "a row of another scope"));
        }
        if check_bad && bad > 0 {
            return Err(refusal(UNSUPPORTED, "a stored value failed its check"));
        }
        // ASOF-KNOWLEDGE-1: a version committed under another model is not
        // read as this one (the exact engine refuses it the same way).
        if let Some(model) = &self.model {
            let other_model = Frag::fmt(
                &format!(
                    "SELECT count(*) FROM {} WHERE {{}} AND \":model\" <> {{}}",
                    self.table
                ),
                &[&self.all, &Frag::bind("?", Sql::Text(model.clone()))],
            );
            if query_u64(conn, &other_model)? > 0 {
                return Err(refusal(UNSUPPORTED, "a version of another model"));
            }
        }
        Ok(Some(population))
    }

    fn provenance(&self, tenant: &TenantId) -> SnapshotProvenance {
        SnapshotProvenance {
            engine: AnalyticalEngineId::DuckDb,
            tenant: tenant.clone(),
            snapshot: self.read.clone(),
            freshness: crate::compute::Freshness::Exact,
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
        if arity == 0 {
            return Err(refusal(UNSUPPORTED, "a query has no operands"));
        }
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
/// MANIFEST-LAWS-1 (S4): a number the per-row decoder cannot hold exactly
/// (an Integer: a signed 64-bit integer; a Decimal: `norn_ir::Decimal`, a
/// 96-bit mantissa and at most 28 fractional digits once canonical) is not
/// typed, whichever rules read it.
const OUT_OF_RANGE: &str = "bulk.out_of_range";

/// Whether the typed number `v` (a `DECIMAL`) is in the per-row decoder's
/// exact range: its canonical digits (trailing fractional zeros and the
/// sign dropped) are at most [`EXACT_MANTISSA`], with at most
/// [`MAX_PREDICATE_SCALE`] of them fractional.
fn exact_sql(v: &str) -> String {
    let unsigned = format!("ltrim(CAST({v} AS VARCHAR), '-')");
    let canonical = format!(
        "(CASE WHEN strpos({unsigned}, '.') > 0 THEN rtrim(rtrim({unsigned}, '0'), '.') ELSE {unsigned} END)"
    );
    let fraction = format!(
        "(CASE WHEN strpos({canonical}, '.') > 0 THEN strlen({canonical}) - strpos({canonical}, '.') ELSE 0 END)"
    );
    let digits = format!("ltrim(replace({canonical}, '.', ''), '0')");
    let width = EXACT_MANTISSA.len();
    format!(
        "({fraction} <= {MAX_PREDICATE_SCALE} AND (strlen({digits}) < {width} OR (strlen({digits}) = {width} AND {digits} <= '{EXACT_MANTISSA}')))"
    )
}

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
            // S4: an Integer is a signed 64-bit integer to the per-row
            // property codec, not a decimal, so its range is i64's.
            reason: format!(
                "CASE WHEN {v} IS NULL THEN 'bulk.not_integer' WHEN NOT ({v} BETWEEN {} AND {}) THEN '{OUT_OF_RANGE}' END",
                i64::MIN,
                i64::MAX
            ),
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
                "CASE WHEN NOT s{i} THEN 'bulk.not_decimal' WHEN NOT f{i} THEN 'bulk.scale_exceeded' WHEN {v} IS NULL THEN 'bulk.not_decimal' \
                 WHEN NOT {} THEN '{OUT_OF_RANGE}' END",
                exact_sql(&v)
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
            super::PointLaw::OneOf(values) => {
                if values.is_empty() {
                    "'bulk.not_in_enum'".to_owned()
                } else {
                    format!(
                        "CASE WHEN {v} IN ({}) THEN NULL ELSE 'bulk.not_in_enum' END",
                        values
                            .iter()
                            .map(|value| format!("'{}'", value.replace('\'', "''")))
                            .collect::<Vec<_>>()
                            .join(", ")
                    )
                }
            }
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
    Ok((rows, reasons))
}

/// The lineage record of load `request`: its source file's digest and
/// object. A direct load writes it in its load transaction; a landing
/// writes it in its store transaction (Sol r3 S8/S9).
fn set_lineage(conn: &Connection, request: &LoadRequest) -> Outcome<()> {
    set_meta(
        conn,
        &format!("lineage:{}", request.load),
        &format!("{} {}", request.file.sha256, request.file.uri),
    )
}

/// Whether a load step writes its load's lineage in its own transaction:
/// a direct load does; a landing's staged load does not (its store does).
#[derive(Clone, Copy, PartialEq, Eq)]
enum Lineage {
    Write,
    Skip,
}

/// The load step under the tenant's lock, in one transaction (Sol r3 S9:
/// with a direct load's lineage, so neither commits without the other).
fn load_locked(
    conn: &Connection,
    request: &LoadRequest,
    file: &Path,
    bytes: u64,
    started: Instant,
    lineage: Lineage,
) -> Outcome<LoadResponse> {
    let (rows, reasons) = in_transaction(conn, |conn| {
        let loaded = load_file(conn, request, file)?;
        if lineage == Lineage::Write {
            set_lineage(conn, request)?;
        }
        Ok(loaded)
    })?;
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

/// MANIFEST-LAWS-1 (S3): the held-set value columns of a typed staged row
/// (`d`), as the text the per-row path holds: each value's text, a number
/// as its canonical decimal (`1.500` is `1.5`).
fn held_values(table: &TableSchema) -> Vec<String> {
    table
        .columns
        .iter()
        .map(|column| {
            let text = format!("CAST(d.{} AS VARCHAR)", ident(&column.id));
            match column.kind {
                ColumnKind::Integer | ColumnKind::Decimal { .. } => format!(
                    "CASE WHEN strpos({text}, '.') > 0 THEN rtrim(rtrim({text}, '0'), '.') ELSE {text} END"
                ),
                ColumnKind::Text
                | ColumnKind::Boolean
                | ColumnKind::Timestamp
                | ColumnKind::Date
                | ColumnKind::Opaque => text,
            }
        })
        .collect()
}

/// The quarantine's column list for a rule finding: the metadata and every
/// value column.
fn finding_columns(table: &TableSchema) -> String {
    let mut columns = vec![
        ident(":row"),
        ident(":load"),
        ident(":reason"),
        ident(":rule"),
        ident(":holds"),
    ];
    columns.extend(table.columns.iter().map(|c| ident(&c.id)));
    columns.join(", ")
}

/// The values of [`finding_columns`]: the row (`row`), the load, reason,
/// rule and hold flag bound, and the staged row's values.
fn finding_values(table: &TableSchema, row: &str) -> String {
    let mut values = vec![row.to_owned()];
    values.extend(["?"; 4].map(str::to_owned));
    values.extend(held_values(table));
    values.join(", ")
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

/// A literal or column read: its SQL and the guard a number or text needs.
fn lower_leaf(node: &RowPredicate, table: &TableSchema) -> Outcome<LoweredRule> {
    use PredicateKind as K;
    use RowPredicate as P;
    Ok(match node {
        P::Null => LoweredRule {
            sql: Frag::raw("NULL"),
            kind: K::Null,
            guards: Vec::new(),
        },
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
                K::Null | K::Bool | K::DateText | K::Date | K::Timestamp => Vec::new(),
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
        | P::Coalesce(..)
        | P::Conditional(..)
        | P::Date(_)
        | P::Timestamp(_) => return Err(refusal(LAW, "not a literal or a column")),
    })
}

fn merge_kinds_sql(left: PredicateKind, right: PredicateKind) -> Option<PredicateKind> {
    use PredicateKind as K;
    match (left, right) {
        (K::Null, other) | (other, K::Null) => Some(other),
        (K::Number(a), K::Number(b)) => Some(K::Number(a.max(b))),
        (K::DateText, K::Text) | (K::Text, K::DateText) => Some(K::Text),
        (a, b) if a == b => Some(a),
        _ => None,
    }
}

fn lower_temporal_leaf(
    node: &RowPredicate,
    table: &TableSchema,
    is_date: bool,
) -> Outcome<LoweredRule> {
    use PredicateKind as K;
    use RowPredicate as P;
    let target = if is_date { K::Date } else { K::Timestamp };
    Ok(match node {
        P::Null => LoweredRule {
            sql: Frag::raw("NULL"),
            kind: K::Null,
            guards: Vec::new(),
        },
        P::Text(lit) => {
            let sql = if is_date {
                let secs = zen_date_key(lit)
                    .ok_or_else(|| refusal(LAW, "an invalid calendar date literal"))?;
                Frag::bind("CAST(? AS BIGINT)", Sql::BigInt(secs))
            } else {
                let (secs, nanos) = zen_timestamp_key(lit)
                    .ok_or_else(|| refusal(LAW, "an invalid timestamp literal"))?;
                Frag::bind(
                    "CAST(? AS HUGEINT)",
                    Sql::Text(instant_nanos(secs, nanos).to_string()),
                )
            };
            LoweredRule {
                sql,
                kind: target,
                guards: Vec::new(),
            }
        }
        P::Column(id) => {
            let expected = if is_date {
                ColumnKind::Date
            } else {
                ColumnKind::Timestamp
            };
            let matched = table
                .columns
                .iter()
                .find(|c| c.id == *id)
                .is_some_and(|c| c.kind == expected);
            if !matched {
                return Err(refusal(
                    LAW,
                    format!("column {id} is not a temporal column"),
                ));
            }
            let col = ident(id);
            let t = derived(id, "t");
            let n = derived(id, "n");
            let guard = Frag::raw(format!(
                "({col} >= '0001' AND {t} >= {FIRST_ZEN_SECOND} AND {t} <= {LAST_ZEN_SECOND})"
            ));
            let sql = if is_date {
                Frag::raw(t)
            } else {
                Frag::raw(format!(
                    "(CAST({t} AS HUGEINT) * 1000000000 + CAST({n} AS HUGEINT))"
                ))
            };
            LoweredRule {
                sql,
                kind: target,
                guards: vec![guard],
            }
        }
        P::Bool(_)
        | P::Number(_)
        | P::Not(_)
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
        | P::Coalesce(..)
        | P::Conditional(..)
        | P::Date(_)
        | P::Timestamp(_) => return Err(refusal(LAW, "unsupported temporal argument")),
    })
}

/// Lower the argument tree of `date(...)` (`is_date = true`, `BIGINT` UTC
/// midnight seconds) or `timestamp(...)` (`is_date = false`, `HUGEINT`
/// order-isomorphic UTC nanoseconds) with exact ZEN `0001..=9999` guards.
fn lower_temporal(node: &RowPredicate, table: &TableSchema, is_date: bool) -> Outcome<LoweredRule> {
    use PredicateKind as K;
    use RowPredicate as P;
    Ok(match node {
        P::Null | P::Text(_) | P::Column(_) => lower_temporal_leaf(node, table, is_date)?,
        P::Coalesce(a, b) => {
            let (a, b) = (
                lower_temporal(a, table, is_date)?,
                lower_temporal(b, table, is_date)?,
            );
            let kind = merge_kinds_sql(a.kind, b.kind)
                .ok_or_else(|| refusal(LAW, "a temporal coalesce the engine does not type"))?;
            let mut guards = a.guards;
            guards.extend(b.guards);
            let sql = if a.kind == K::Null && b.kind == K::Null {
                Frag::raw("NULL")
            } else {
                Frag::fmt("COALESCE({}, {})", &[&a.sql, &b.sql])
            };
            LoweredRule { sql, kind, guards }
        }
        P::Conditional(c, a, b) => {
            let c = lower_node(c, table)?;
            if c.kind != K::Bool {
                return Err(refusal(LAW, "a temporal conditional with non-Boolean test"));
            }
            let (a, b) = (
                lower_temporal(a, table, is_date)?,
                lower_temporal(b, table, is_date)?,
            );
            let kind = merge_kinds_sql(a.kind, b.kind)
                .ok_or_else(|| refusal(LAW, "a temporal conditional the engine does not type"))?;
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
        P::Bool(_)
        | P::Number(_)
        | P::Not(_)
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
        | P::Date(_)
        | P::Timestamp(_) => return Err(refusal(LAW, "unsupported temporal argument")),
    })
}

/// Cast mixed-scale decimal operands explicitly to `DECIMAL(38, max(x, y))` so
/// `DuckDB` never rounds a higher-scale branch/operand down to a lower-scale
/// sibling's type and cast overflow routes cleanly to residue.
fn widen_decimal_pair(
    a_sql: Frag,
    a_kind: PredicateKind,
    b_sql: Frag,
    b_kind: PredicateKind,
) -> (Frag, Frag) {
    if let (PredicateKind::Number(x), PredicateKind::Number(y)) = (a_kind, b_kind)
        && x != y
    {
        let to = x.max(y);
        let cast = |sql: Frag, scale: u32| {
            if scale == to {
                sql
            } else {
                Frag::fmt(&format!("CAST({{}} AS DECIMAL(38,{to}))"), &[&sql])
            }
        };
        return (cast(a_sql, x), cast(b_sql, y));
    }
    (a_sql, b_sql)
}

/// `a == b` or `a != b`; uses SQL null-safe equality (`IS [NOT] DISTINCT FROM`)
/// with explicit mixed-scale decimal widening so dynamically nullable branches
/// match ZEN's `null == null` / `null == x` without rounding.
fn lower_equality(
    equal: bool,
    a: &RowPredicate,
    b: &RowPredicate,
    table: &TableSchema,
) -> Outcome<LoweredRule> {
    use PredicateKind as K;
    let (a, b) = (lower_node(a, table)?, lower_node(b, table)?);
    let mut guards = a.guards;
    guards.extend(b.guards);
    let sql = match (a.kind, b.kind) {
        (K::Null, K::Null) => Frag::raw(if equal { "TRUE" } else { "FALSE" }),
        (K::Null, _) => {
            let test = if equal { "IS NULL" } else { "IS NOT NULL" };
            Frag::fmt(&format!("({{}} {test})"), &[&b.sql])
        }
        (_, K::Null) => {
            let test = if equal { "IS NULL" } else { "IS NOT NULL" };
            Frag::fmt(&format!("({{}} {test})"), &[&a.sql])
        }
        _ => {
            let (a_sql, b_sql) = widen_decimal_pair(a.sql, a.kind, b.sql, b.kind);
            let op = if equal {
                "IS NOT DISTINCT FROM"
            } else {
                "IS DISTINCT FROM"
            };
            Frag::fmt(&format!("({{}} {op} {{}})"), &[&a_sql, &b_sql])
        }
    };
    Ok(LoweredRule {
        sql,
        kind: K::Bool,
        guards,
    })
}

fn lower_coalesce(a: &RowPredicate, b: &RowPredicate, table: &TableSchema) -> Outcome<LoweredRule> {
    let (a, b) = (lower_node(a, table)?, lower_node(b, table)?);
    let kind = merge_kinds_sql(a.kind, b.kind)
        .ok_or_else(|| refusal(LAW, "a coalesce the engine does not type"))?;
    let mut guards = a.guards;
    guards.extend(b.guards);
    let sql = if a.kind == PredicateKind::Null && b.kind == PredicateKind::Null {
        Frag::raw("NULL")
    } else {
        let (a_sql, b_sql) = widen_decimal_pair(a.sql, a.kind, b.sql, b.kind);
        Frag::fmt("COALESCE({}, {})", &[&a_sql, &b_sql])
    };
    Ok(LoweredRule { sql, kind, guards })
}

fn lower_conditional(
    c: &RowPredicate,
    a: &RowPredicate,
    b: &RowPredicate,
    table: &TableSchema,
) -> Outcome<LoweredRule> {
    let (c, a, b) = (
        lower_node(c, table)?,
        lower_node(a, table)?,
        lower_node(b, table)?,
    );
    let kind = if c.kind == PredicateKind::Bool {
        merge_kinds_sql(a.kind, b.kind)
    } else {
        None
    }
    .ok_or_else(|| refusal(LAW, "a conditional the engine does not type"))?;
    let mut guards = c.guards;
    guards.extend(a.guards);
    guards.extend(b.guards);
    let (a_sql, b_sql) = widen_decimal_pair(a.sql, a.kind, b.sql, b.kind);
    Ok(LoweredRule {
        sql: Frag::fmt(
            "(CASE WHEN {} THEN {} ELSE {} END)",
            &[&c.sql, &a_sql, &b_sql],
        ),
        kind,
        guards,
    })
}

fn lower_node(node: &RowPredicate, table: &TableSchema) -> Outcome<LoweredRule> {
    use PredicateKind as K;
    use RowPredicate as P;
    let combine = |op: &str, a: LoweredRule, b: LoweredRule, kind: K| -> LoweredRule {
        let mut guards = a.guards;
        guards.extend(b.guards);
        LoweredRule {
            sql: Frag::fmt(&format!("({{}} {op} {{}})"), &[&a.sql, &b.sql]),
            kind,
            guards,
        }
    };
    let binary = |op: &str, a: &RowPredicate, b: &RowPredicate, kind: K| -> Outcome<LoweredRule> {
        Ok(combine(
            op,
            lower_node(a, table)?,
            lower_node(b, table)?,
            kind,
        ))
    };
    let compare = |op: &str, a: &RowPredicate, b: &RowPredicate| -> Outcome<LoweredRule> {
        let (a, b) = (lower_node(a, table)?, lower_node(b, table)?);
        let mut guards = a.guards;
        guards.extend(b.guards);
        let (a_sql, b_sql) = widen_decimal_pair(a.sql, a.kind, b.sql, b.kind);
        Ok(LoweredRule {
            sql: Frag::fmt(&format!("({{}} {op} {{}})"), &[&a_sql, &b_sql]),
            kind: K::Bool,
            guards,
        })
    };
    let arithmetic = |op: &str,
                      a: &RowPredicate,
                      b: &RowPredicate,
                      scale: fn(u32, u32) -> Outcome<u32>|
     -> Outcome<LoweredRule> {
        let (a, b) = (lower_node(a, table)?, lower_node(b, table)?);
        let (K::Number(x), K::Number(y)) = (a.kind, b.kind) else {
            return Err(refusal(LAW, "arithmetic on a value that is not a number"));
        };
        let scale = scale(x, y)?;
        let mut lowered = combine(op, a, b, K::Number(scale));
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
        P::Add(a, b) => arithmetic("+", a, b, |x, y| Ok(x.max(y)))?,
        P::Subtract(a, b) => arithmetic("-", a, b, |x, y| Ok(x.max(y)))?,
        P::Multiply(a, b) => arithmetic("*", a, b, |x, y| {
            x.checked_add(y)
                .filter(|s| *s <= MAX_PREDICATE_SCALE)
                .ok_or_else(|| refusal(LAW, "a scale past the exact range"))
        })?,
        P::Equal(a, b) | P::NotEqual(a, b) => {
            lower_equality(matches!(node, P::Equal(..)), a, b, table)?
        }
        P::Less(a, b) => compare("<", a, b)?,
        P::LessOrEqual(a, b) => compare("<=", a, b)?,
        P::Greater(a, b) => compare(">", a, b)?,
        P::GreaterOrEqual(a, b) => compare(">=", a, b)?,
        P::And(a, b) => binary("AND", a, b, K::Bool)?,
        P::Or(a, b) => binary("OR", a, b, K::Bool)?,
        P::Coalesce(a, b) => lower_coalesce(a, b, table)?,
        P::Conditional(c, a, b) => lower_conditional(c, a, b, table)?,
        P::Date(inner) => lower_temporal(inner, table, true)?,
        P::Timestamp(inner) => lower_temporal(inner, table, false)?,
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
    residue.extend(
        guards
            .iter()
            .map(|guard| Frag::fmt("TRY({}) IS NOT TRUE", &[guard])),
    );
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

/// MANIFEST-LAWS-1 (S2): a bound, in SQL over the row, on the fuel the
/// per-row evaluator charges for `check`: its static part
/// ([`RuleCheck::fuel`]) and the data its operations read, as ZEN's meter
/// charges it (an equality, `in` or text `+` its operands' sizes; every
/// operator is charged so here). A value's size is a text's UTF-8 bytes
/// (`octet_length`) and 1 for anything else. `None` for a rule outside the
/// lowered subset.
fn fuel_sql(check: &RuleCheck, table: &TableSchema) -> Option<String> {
    let mut parts = vec![format!("CAST({} AS HUGEINT)", check.fuel)];
    parts.push(format!(
        "CAST({} AS HUGEINT)",
        data_fuel(check.predicate.as_ref()?, table)
    ));
    if let Some(condition) = &check.condition {
        parts.push(format!("CAST({} AS HUGEINT)", data_fuel(condition, table)));
    }
    Some(format!("({})", parts.join(" + ")))
}

/// The data-proportional fuel of `node` and everything under it, in SQL.
fn data_fuel(node: &RowPredicate, table: &TableSchema) -> String {
    use RowPredicate as P;
    match node {
        P::Null | P::Bool(_) | P::Number(_) | P::Text(_) | P::Column(_) => "0".to_owned(),
        P::Not(inner) | P::Negate(inner) => data_fuel(inner, table),
        P::Date(inner) | P::Timestamp(inner) => {
            format!("{} + {}", data_fuel(inner, table), value_size(inner, table))
        }
        P::Coalesce(a, b) => format!("{} + {}", data_fuel(a, table), data_fuel(b, table)),
        P::Add(a, b)
        | P::Subtract(a, b)
        | P::Multiply(a, b)
        | P::Equal(a, b)
        | P::NotEqual(a, b)
        | P::Less(a, b)
        | P::LessOrEqual(a, b)
        | P::Greater(a, b)
        | P::GreaterOrEqual(a, b)
        | P::And(a, b)
        | P::Or(a, b) => format!(
            "{} + {} + {} + {}",
            data_fuel(a, table),
            data_fuel(b, table),
            value_size(a, table),
            value_size(b, table)
        ),
        P::Conditional(c, a, b) => format!(
            "{} + {} + {}",
            data_fuel(c, table),
            data_fuel(a, table),
            data_fuel(b, table)
        ),
    }
}

/// ZEN's shallow size of `node`'s value, in SQL: a text's UTF-8 bytes
/// (`strlen` in `DuckDB`, which counts UTF-8 bytes while `length` counts
/// Unicode code points), else 1.
fn value_size(node: &RowPredicate, table: &TableSchema) -> String {
    use RowPredicate as P;
    match node {
        P::Text(text) => text.len().max(1).to_string(),
        P::Column(id)
            if table.columns.iter().find(|c| c.id == *id).is_some_and(|c| {
                matches!(
                    c.kind,
                    ColumnKind::Text | ColumnKind::Date | ColumnKind::Timestamp
                )
            }) =>
        {
            format!("coalesce(strlen({}), 0)", ident(id))
        }
        P::Coalesce(a, b) | P::Conditional(_, a, b) => format!(
            "greatest({}, {})",
            value_size(a, table),
            value_size(b, table)
        ),
        P::Null
        | P::Bool(_)
        | P::Number(_)
        | P::Column(_)
        | P::Not(_)
        | P::Negate(_)
        | P::Date(_)
        | P::Timestamp(_)
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
        | P::Or(..) => "1".to_owned(),
    }
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
    // S2: a row whose rules may cost the per-row evaluator more fuel than
    // it gives a row is left to it for every rule, so the engine never
    // decides a row the per-row path refuses. A rule outside the lowered
    // subset already leaves every row to it.
    let over = request
        .checks
        .iter()
        .map(|check| fuel_sql(check, table))
        .collect::<Option<Vec<_>>>()
        .map(|fuel| format!("(({}) > {})", fuel.join(" + "), row_fuel()));
    for (i, check) in request.checks.iter().enumerate() {
        let (residue, fails) = check_sql(check, table)?;
        let residue = match &over {
            Some(over) => Frag::fmt(&format!("({{}} OR {over})"), &[&residue]),
            None => residue,
        };
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
    let held_into = finding_columns(table);
    let held = finding_values(table, "d.\":row\"");
    for (i, check) in request.checks.iter().enumerate() {
        db(conn.execute(
            &format!("INSERT INTO {residue} SELECT \":row\", ? FROM \":outcome\" WHERE \"r{i}\""),
            params_from_iter([Sql::Text(check.rule.clone())]),
        ))?;
        // S3: a finding carries the row's values, as the per-row path's does.
        db(conn.execute(
            &format!(
                "INSERT INTO {quarantine} ({held_into}) SELECT {held} FROM \":outcome\" AS o \
                 JOIN {dataset} AS d ON d.\":row\" = o.\":row\" WHERE NOT o.\"r{i}\" AND o.\"f{i}\""
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
/// must name a rule the engine left to that row, once, and match the
/// metadata the landing pinned for that rule.
fn settle_rows(conn: &Connection, table: &TableSchema, request: &SettleRequest) -> Outcome<()> {
    let quarantine = quarantine_table(&table.name);
    let residue = residue_table(&table.name);
    let dataset = dataset_table(&table.name);
    let columns = finding_columns(table);
    let values = finding_values(table, "?");
    let mut statement = db(conn.prepare(&format!(
        "SELECT count(*) FROM {residue} WHERE \":row\" = ? AND \":rule\" = ? AND \":row\" <= ?"
    )))?;
    let checks: BTreeMap<_, _> = request
        .land
        .checks
        .iter()
        .map(|check| (check.rule.as_str(), check))
        .collect();
    let mut seen = std::collections::BTreeSet::new();
    for finding in &request.findings {
        let Some(check) = checks
            .get(finding.rule.as_str())
            .filter(|check| check.label == finding.label && check.holds == finding.holds)
        else {
            return Err(refusal(
                "bulk.invalid_finding",
                "the finding does not match the landed rule's label and hold",
            ));
        };
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
        // S3: from the staged row, so the finding carries its values.
        db(conn.execute(
            &format!(
                "INSERT INTO {quarantine} ({columns}) SELECT {values} FROM {dataset} AS d WHERE d.\":row\" = ?"
            ),
            params_from_iter([
                Sql::UBigInt(finding.row),
                Sql::Text(request.land.load.load.clone()),
                Sql::Text(check.label.clone()),
                Sql::Text(check.rule.clone()),
                Sql::Boolean(check.holds),
                Sql::UBigInt(finding.row),
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

/// MANIFEST-LAWS-1 (S6): the residue page after row `after`: the next
/// residue rows in row order, at most `limit` of them (clamped to
/// `1..=RESIDUE_PAGE_ROWS`), with their present values, ending before the
/// row that would take the page's serialized rows past
/// [`RESIDUE_PAGE_BYTES`]. A row that alone passes the bound is
/// [`RESIDUE_EXCEEDED`].
///
/// Sol r2 S6: `DuckDB` materializes a query's whole result before its first
/// row is read, so the wide values are never in the candidate query. It
/// reads the next row ids, their rules and each row's value bytes (a lower
/// bound on its serialized size: JSON only adds keys, quotes and escapes).
/// Only the rows whose value bytes fit the page together are then read with
/// their values, so a page's read holds at most [`RESIDUE_PAGE_BYTES`] of
/// values. Their exact serialized size then ends the page.
fn residue_page(
    conn: &Connection,
    table: &TableSchema,
    after: u64,
    limit: u32,
) -> Outcome<Vec<ResidueRow>> {
    let mut statement = db(conn.prepare(&residue_candidates_sql(table)))?;
    let candidates = db(db(statement.query_map(
        params_from_iter([
            Sql::UBigInt(after),
            Sql::UInt(limit.clamp(1, RESIDUE_PAGE_ROWS)),
        ]),
        |row| {
            Ok((
                row.get::<_, u64>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, u64>(2)?,
            ))
        },
    ))?
    .collect::<Result<Vec<_>, _>>())?;
    // S7: a page's JSON is its rows, a comma between each two, and its two
    // brackets: the count starts at one bracket and each row adds itself
    // and one separator (the last one's is the closing bracket).
    let mut lower = 1usize;
    let mut fits = 0usize;
    for (_, _, bytes) in &candidates {
        lower = lower.saturating_add(
            usize::try_from(*bytes)
                .unwrap_or(usize::MAX)
                .saturating_add(1),
        );
        if lower > RESIDUE_PAGE_BYTES {
            break;
        }
        fits = fits.saturating_add(1);
    }
    let mut page = Vec::new();
    let mut bytes = 1usize;
    if let (true, Some((first, ..)), Some((last, ..))) = (
        fits > 0,
        candidates.first(),
        candidates.get(fits.saturating_sub(1)),
    ) {
        let mut statement = db(conn.prepare(&residue_values_sql(table)))?;
        let mut rows = db(statement.query(params_from_iter([
            Sql::UBigInt(*first),
            Sql::UBigInt(*last),
        ])))?;
        let mut taken = candidates.iter();
        while let Some(row) = db(rows.next())? {
            let id: u64 = db(row.get(0))?;
            let Some((candidate, rules, _)) = taken.next().filter(|(at, ..)| *at == id) else {
                return Err(refusal(
                    "analytics.engine",
                    "a residue page read other rows than its candidates",
                ));
            };
            let mut values = BTreeMap::new();
            for (i, column) in table.columns.iter().enumerate() {
                let at = i.saturating_mul(3).saturating_add(1);
                if let Some(value) = present(
                    column.kind,
                    db(row.get(at))?,
                    db(row.get(at.saturating_add(1)))?,
                    db(row.get(at.saturating_add(2)))?,
                ) {
                    values.insert(column.id.clone(), value);
                }
            }
            let residue = ResidueRow {
                row: *candidate,
                values,
                rules: rules.split('\n').map(str::to_owned).collect(),
            };
            // The row's JSON and the separator after it.
            let size = serde_json::to_string(&residue)
                .map_err(|e| refusal("analytics.engine", e.to_string()))?
                .len()
                .saturating_add(1);
            if size.saturating_add(1) > RESIDUE_PAGE_BYTES {
                return Err(too_wide(residue.row));
            }
            bytes = bytes.saturating_add(size);
            if bytes > RESIDUE_PAGE_BYTES {
                return Ok(page);
            }
            page.push(residue);
        }
    }
    // Every candidate read fits: the first one left unread starts the next
    // page, unless its value bytes alone already pass a page.
    match candidates.get(fits) {
        Some((row, _, value_bytes))
            if usize::try_from(*value_bytes)
                .unwrap_or(usize::MAX)
                .saturating_add(2)
                > RESIDUE_PAGE_BYTES =>
        {
            Err(too_wide(*row))
        }
        _ => Ok(page),
    }
}

/// S6: the residue page's candidates after `?`, at most `?` rows: each
/// row's id, its rules and its text values' bytes. It reads no value
/// itself, so the result `DuckDB` materializes holds no wide value.
fn residue_candidates_sql(table: &TableSchema) -> String {
    // Only text is wide, and its JSON holds every byte of it; a number's or
    // a Boolean's text is short, and its canonical form may be shorter.
    let mut value_bytes = vec!["0".to_owned()];
    for column in &table.columns {
        if matches!(
            column.kind,
            ColumnKind::Text | ColumnKind::Opaque | ColumnKind::Timestamp | ColumnKind::Date
        ) {
            value_bytes.push(format!(
                "coalesce(strlen(CAST(d.{} AS VARCHAR)), 0)",
                ident(&column.id)
            ));
        }
    }
    let residue = residue_table(&table.name);
    format!(
        "WITH ids AS (SELECT DISTINCT \":row\" FROM {residue} WHERE \":row\" > ? ORDER BY \":row\" LIMIT ?), \
         r AS (SELECT \":row\", string_agg(\":rule\", chr(10) ORDER BY \":rule\") AS \":rules\" FROM {residue} \
         WHERE \":row\" IN (SELECT \":row\" FROM ids) GROUP BY \":row\") \
         SELECT r.\":row\", r.\":rules\", CAST({} AS UBIGINT) FROM {} AS d JOIN r ON d.\":row\" = r.\":row\" \
         ORDER BY r.\":row\"",
        value_bytes.join(" + "),
        dataset_table(&table.name),
    )
}

/// S6: the values of the residue rows from `?` through `?`, the candidates
/// whose value bytes fit one page together: the id, then for each column
/// its text and, for an instant, its ordering key.
fn residue_values_sql(table: &TableSchema) -> String {
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
    format!(
        "SELECT {} FROM {} AS d WHERE d.\":row\" BETWEEN ? AND ? \
         AND d.\":row\" IN (SELECT \":row\" FROM {}) ORDER BY d.\":row\"",
        select.join(", "),
        dataset_table(&table.name),
        residue_table(&table.name),
    )
}

/// [`RESIDUE_EXCEEDED`] for residue row `row`, larger than a page alone.
fn too_wide(row: u64) -> EngineRefusal {
    refusal(
        RESIDUE_EXCEEDED,
        format!("residue row {row} is larger than a residue page ({RESIDUE_PAGE_BYTES} bytes)"),
    )
}

/// MANIFEST-LAWS-1 (S1): refuse a landing whose residue takes more than
/// [`MAX_RESIDUE_PAGES`] pages of `limit` rows (or holds a row larger than a
/// page), so every host settles it within [`super::MAX_ENGINE_STEPS`] calls.
/// Pages are counted as [`residue_page`] answers them, each settle ending
/// at its page's last row (S5), so the count is the settles the landing
/// takes. Checked before the first answer; the caller drops the staging.
fn residue_fits(conn: &Connection, table: &TableSchema, limit: u32) -> Outcome<()> {
    let mut after = 0;
    for _ in 0..MAX_RESIDUE_PAGES {
        match residue_page(conn, table, after, limit)?.last() {
            Some(last) => after = last.row,
            None => return Ok(()),
        }
    }
    if residue_page(conn, table, after, 1)?.is_empty() {
        return Ok(());
    }
    Err(refusal(
        RESIDUE_EXCEEDED,
        format!(
            "the landing leaves more residue than {MAX_RESIDUE_PAGES} pages of {} rows",
            limit.clamp(1, RESIDUE_PAGE_ROWS)
        ),
    ))
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
    in_transaction(conn, |conn| rebuild(conn, &kept, load))
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

#[cfg(test)]
#[allow(clippy::expect_used, reason = "a test of the residue page's queries")]
mod tests {
    use super::*;

    #[test]
    fn an_unversioned_store_receipt_replays_its_original_bytes() {
        const TENANT: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
        const COUNTS: &str =
            r#"{"load":"load-1","rows":1,"rejected":0,"reasons":{},"bytes":40,"millis":1}"#;
        const FILES: &str = concat!(
            r#""data":{"sha256":"data-digest","rows":1,"bytes":60},"#,
            r#""held":{"sha256":"held-digest","rows":0,"bytes":20},"validTime":[]"#,
        );
        let landed = format!(r#""landing":"landing-1","attempt":"aOld","load":{COUNTS},{FILES}"#);
        let answer = format!(r#"{{{landed},"stored":"old-cache"}}"#);
        let original = format!(
            r#"{{
                "land":{{"load":{{"tenant":"{TENANT}",
                    "table":{{"name":"t.Row","columns":[],"pin":"table-pin"}},"load":"load-1",
                    "file":{{"url":"","uri":"s3://synthetic/source","sha256":"source-digest","format":"csv"}},
                    "headers":{{}}}},
                    "pins":{{"definitionsVersion":1,"modelHash":"model","binding":"binding",
                        "mapping":"mapping","laws":"laws"}},
                    "landing":"landing-1","attempt":"aOld"}},
                "landed":{{{landed}}},
                "data":{{"uri":"s3://synthetic/data","url":""}},
                "held":{{"uri":"s3://synthetic/held","url":""}}
            }}"#
        );
        let receipt = format!(r#"{{"request":{original},"response":{answer}}}"#);
        let dir = tempfile::tempdir().expect("engine directory");
        let settings = || Settings {
            root: dir.path().to_path_buf(),
            memory_limit: "128MB".into(),
            threads: 1,
        };
        let tenant = TenantId::try_from(TENANT.to_owned()).expect("tenant");
        let key = receipt_key("aOld");
        Engine::new(settings())
            .expect("old engine")
            .with(&tenant, |conn| set_meta(conn, &key, &receipt))
            .expect("stored receipt");
        let reopened = Engine::new(settings()).expect("reopened engine");
        let refreshed = original.replace(r#""url":"""#, r#""url":"https://synthetic/refreshed""#);
        let request: StoreBody = serde_json::from_str(&refreshed).expect("old store request");
        let replayed = reopened
            .store_json(&request, |_, _| panic!("a replay sends nothing"))
            .expect("replayed");
        assert_eq!(
            serde_json::to_string(&replayed).expect("wire answer"),
            answer
        );
        assert_eq!(
            reopened
                .with(&tenant, |conn| meta(conn, &key))
                .expect("receipt")
                .as_deref(),
            Some(receipt.as_str())
        );
        let changed = refreshed.replace("s3://synthetic/data", "s3://synthetic/other");
        let mismatch: StoreBody = serde_json::from_str(&changed).expect("changed object");
        assert_eq!(
            reopened
                .store_json(&mismatch, |_, _| panic!("a refusal sends nothing"))
                .expect_err("another store body")
                .code,
            STALE
        );
        for (attempt, version) in [("aZero", "0"), ("aFuture", "2"), ("aNull", "null")] {
            let key = receipt_key(attempt);
            let unsupported = receipt
                .replacen('{', &format!("{{\"version\":{version},"), 1)
                .replace("aOld", attempt);
            reopened
                .with(&tenant, |conn| set_meta(conn, &key, &unsupported))
                .expect("stored unsupported receipt");
            let body: StoreBody =
                serde_json::from_str(&original.replace("aOld", attempt)).expect("old store body");
            assert_eq!(
                reopened
                    .store_json(&body, |_, _| panic!("a refusal sends nothing"))
                    .expect_err("explicit unsupported version")
                    .code,
                "analytics.engine"
            );
        }
    }

    /// MANIFEST-LAWS-1 (Sol r2 S6): `DuckDB` materializes a query's whole
    /// result before its first row is read, so the candidate query of a
    /// residue page carries no value: only each row's id, rules and value
    /// bytes. Three residue rows of 3 MiB text each: the candidates read
    /// three narrow rows, and the page reads and answers the one that fits.
    #[test]
    fn a_residue_page_materializes_no_value_before_it_fits_the_page() {
        let table = TableSchema {
            name: "t.Wide".into(),
            columns: vec![Column {
                id: "wide".into(),
                kind: ColumnKind::Text,
                nullable: true,
                laws: Vec::new(),
            }],
            pin: String::new(),
        };
        let wide = 3usize << 20;
        let conn = Connection::open_in_memory().expect("duckdb");
        conn.execute_batch(&format!(
            "CREATE TABLE {dataset} ({}); CREATE TABLE {residue} (\":row\" UBIGINT, \":rule\" VARCHAR); \
             INSERT INTO {dataset} (\":row\", \"wide\") SELECT i, repeat('w', {wide}) FROM range(1, 4) AS t(i); \
             INSERT INTO {residue} SELECT i, 'r' FROM range(1, 4) AS t(i);",
            dataset_ddl(&table),
            dataset = dataset_table(&table.name),
            residue = residue_table(&table.name),
        ))
        .expect("tables");
        let mut statement = conn
            .prepare(&residue_candidates_sql(&table))
            .expect("candidates");
        let candidates = statement
            .query_map(params_from_iter([Sql::UBigInt(0), Sql::UInt(100)]), |row| {
                Ok((
                    row.get::<_, u64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, u64>(2)?,
                ))
            })
            .expect("candidates read")
            .collect::<Result<Vec<_>, _>>()
            .expect("candidate rows");
        assert_eq!(statement.column_count(), 3, "no value column is read");
        let wide_bytes = u64::try_from(wide).expect("bytes");
        assert_eq!(
            candidates,
            (1..=3)
                .map(|row| (row, "r".to_owned(), wide_bytes))
                .collect::<Vec<_>>()
        );
        let page = residue_page(&conn, &table, 0, 100).expect("page");
        assert_eq!(page.iter().map(|row| row.row).collect::<Vec<_>>(), vec![1]);
        assert_eq!(
            page.first().and_then(|row| row.values.get("wide")),
            Some(&Value::Text("w".repeat(wide)))
        );
    }
}
