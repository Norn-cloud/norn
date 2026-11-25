//! D137 (#1533, #1661): the tenant analytics engine on `DataFusion`. It answers
//! the typed operations of [`super::AnalyticalEngine`] exactly as the `DuckDB`
//! engine does, over one directory per tenant (ANALYTICS-TENANT-1): a SQLite
//! catalog (`catalog.sqlite3`), the only publication point, and immutable
//! Parquet objects named by their digest (`objects/`).
//!
//! `DataFusion` scans exactly the objects a catalog snapshot names (an explicit
//! file list, each confined to the tenant's object directory, never a link)
//! on one shared, bounded runtime: a fair spilling memory pool, a capped
//! per-tenant spill directory and a cap on decoded bytes per scan. Every
//! answer is then computed in exact arithmetic over the decoded cells; no
//! caller text is ever a statement, an expression or a path.

mod aggregate;
mod catalog;
mod columnar;
mod dataset;
mod files;
mod landing;
mod laws;
mod mirror;
mod page;
mod profile;
mod query;
mod residue;
mod values;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use datafusion::arrow::record_batch::RecordBatch;
use datafusion::execution::disk_manager::{DiskManagerBuilder, DiskManagerMode};
use datafusion::execution::memory_pool::{FairSpillPool, MemoryPool};
use datafusion::execution::runtime_env::{RuntimeEnv, RuntimeEnvBuilder};
use datafusion::logical_expr::Expr;
use datafusion::prelude::{ParquetReadOptions, SessionConfig, SessionContext};
use serde_json::value::RawValue;

use super::backend::{AnalyticalEngineId, EngineCapabilities, StoreBody};
use super::{
    EngineRefusal, EvaluateRequest, EvaluateResponse, ExportRequest, FrontierRequest,
    FrontierResponse, LandRequest, LandResponse, LineageRequest, LineageResponse, LoadRequest,
    LoadResponse, MembersRequest, MembersResponse, PageRequest, PageResponse, ProfileRequest,
    ProfileResponse, SettleRequest, StoreRequest, SyncRequest, TenantId,
};
use catalog::Catalog;

pub use landing::{ATTEMPT_BUSY, ATTEMPT_UNKNOWN};

/// An engine outcome: a typed refusal the cell maps to its own answer.
pub type Outcome<T> = Result<T, EngineRefusal>;

/// This engine's identity, as its answers' provenance names it.
pub const ENGINE_ID: AnalyticalEngineId = AnalyticalEngineId::DataFusion;
/// The most rows one records page returns.
pub const MAX_PAGE_ROWS: u32 = 200;
/// The columns every exported row carries itself.
pub const EXPORT_RESERVED: [&str; 2] = ["id", "version"];
/// A refusal of a read whose dataset this engine cannot rebuild.
pub const DATASET_COLD: &str = "analytics.dataset_cold";

pub(super) const STALE: &str = "core.compute.stale_input";
pub(super) const UNSUPPORTED: &str = "core.compute.unsupported_engine";
/// A scan that would decode more than its share of memory.
const EXHAUSTED: &str = "analytics.resource_exhausted";

pub(super) fn refusal(code: &str, detail: impl Into<String>) -> EngineRefusal {
    EngineRefusal {
        code: code.to_owned(),
        detail: detail.into(),
    }
}

pub(super) fn engine_error(detail: impl Into<String>) -> EngineRefusal {
    refusal("analytics.engine", detail)
}

fn df<T>(value: datafusion::error::Result<T>) -> Outcome<T> {
    value.map_err(|error| {
        let exhausted = matches!(
            error,
            datafusion::error::DataFusionError::ResourcesExhausted(_)
        );
        if exhausted {
            refusal(EXHAUSTED, error.to_string())
        } else {
            engine_error(error.to_string())
        }
    })
}

/// Engine settings: where tenant files live and what the engine may use.
#[derive(Debug, Clone)]
pub struct Settings {
    /// The directory holding one directory per tenant (`t/<tenant>/`) and,
    /// under `tmp/`, one scratch directory per tenant.
    pub root: PathBuf,
    /// The memory bound, for example `512MB`: the memory pool every scan
    /// shares, and each scan's decoded-bytes cap.
    pub memory_limit: String,
    /// Worker threads.
    pub threads: u32,
}

/// `512MB`, `1GiB`, `65536KB` or a bare byte count, in bytes.
fn parse_memory(limit: &str) -> std::io::Result<usize> {
    let limit = limit.trim();
    let split = limit
        .find(|c: char| !c.is_ascii_digit())
        .unwrap_or(limit.len());
    let (digits, unit) = limit.split_at(split);
    let shift = match unit.trim().to_ascii_uppercase().as_str() {
        "" | "B" => 0_u32,
        "KB" | "KIB" | "K" => 10,
        "MB" | "MIB" | "M" => 20,
        "GB" | "GIB" | "G" => 30,
        _ => return Err(std::io::Error::other("an unreadable memory limit")),
    };
    digits
        .parse::<usize>()
        .ok()
        .and_then(|n| n.checked_mul(1_usize.checked_shl(shift)?))
        .filter(|bytes| *bytes > 0)
        .ok_or_else(|| std::io::Error::other("an unreadable memory limit"))
}

/// One tenant's directory, catalog and bounded `DataFusion` runtime.
pub(super) struct Tenant {
    /// `t/<tenant>/`.
    pub dir: PathBuf,
    /// The catalog, the one publication point.
    pub catalog: Catalog,
    runtime: Arc<RuntimeEnv>,
}

impl Tenant {
    /// The tenant's immutable objects.
    pub(super) fn objects(&self) -> PathBuf {
        self.dir.join("objects")
    }

    /// The tenant's private staging directory for files not yet installed.
    pub(super) fn staging(&self) -> PathBuf {
        self.dir.join("staging")
    }
}

/// The engine: lazily opened tenants, each used by one operation at a time.
pub struct Engine {
    settings: Settings,
    memory: usize,
    pool: Arc<dyn MemoryPool>,
    tenants: Mutex<BTreeMap<TenantId, Arc<Mutex<Tenant>>>>,
    attempts: Mutex<BTreeMap<(TenantId, String), landing::Held>>,
}

impl Engine {
    /// An engine over `settings.root`, creating it and its `tmp/` directory.
    ///
    /// # Errors
    /// The directories or the runtime cannot be created, or the memory
    /// limit is unreadable.
    pub fn new(settings: Settings) -> std::io::Result<Self> {
        std::fs::create_dir_all(settings.root.join("tmp"))?;
        let memory = parse_memory(&settings.memory_limit)?;
        Ok(Self {
            pool: Arc::new(FairSpillPool::new(memory)),
            memory,
            settings,
            tenants: Mutex::new(BTreeMap::new()),
            attempts: Mutex::new(BTreeMap::new()),
        })
    }

    /// The scratch directory of `tenant` alone (`tmp/<tenant>/`), created
    /// if absent: its loads' downloads and its exports.
    ///
    /// # Errors
    /// The directory cannot be created.
    pub fn scratch(&self, tenant: &TenantId) -> std::io::Result<PathBuf> {
        let path = self.settings.root.join("tmp").join(tenant.as_str());
        std::fs::create_dir_all(&path)?;
        Ok(path)
    }

    /// MANIFEST-02: the tenant's private directory, `<root>/t/<tenant>`.
    fn private(&self, tenant: &TenantId) -> Outcome<PathBuf> {
        let dir = self.settings.root.join("t").join(tenant.as_str());
        for sub in ["scratch", "objects", "spill", "staging"] {
            catalog::private_dir(&dir.join(sub))?;
        }
        catalog::private_dir(&dir)?;
        Ok(dir)
    }

    fn tenant(&self, tenant: &TenantId) -> Outcome<Arc<Mutex<Tenant>>> {
        let mut tenants = self.tenants.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(open) = tenants.get(tenant) {
            return Ok(Arc::clone(open));
        }
        let dir = self.private(tenant)?;
        let catalog = Catalog::open(&dir.join("catalog.sqlite3"))?;
        // Spill is the tenant's own, capped at the memory bound.
        let disk = DiskManagerBuilder::default()
            .with_mode(DiskManagerMode::Directories(vec![dir.join("spill")]))
            .with_max_temp_directory_size(u64::try_from(self.memory).unwrap_or(u64::MAX));
        let runtime = df(RuntimeEnvBuilder::new()
            .with_memory_pool(Arc::clone(&self.pool))
            .with_disk_manager_builder(disk)
            .build_arc())?;
        let open = Arc::new(Mutex::new(Tenant {
            dir,
            catalog,
            runtime,
        }));
        tenants.insert(tenant.clone(), Arc::clone(&open));
        Ok(open)
    }

    /// Run `f` with `tenant` held: one operation at a time per tenant.
    fn with<T>(&self, tenant: &TenantId, f: impl FnOnce(&mut Tenant) -> Outcome<T>) -> Outcome<T> {
        let open = self.tenant(tenant)?;
        let mut held = open.lock().unwrap_or_else(PoisonError::into_inner);
        f(&mut held)
    }

    fn attempts(&self) -> MutexGuard<'_, BTreeMap<(TenantId, String), landing::Held>> {
        self.attempts.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Scan exactly `files` (each confined to the tenant's objects) with an
    /// optional typed filter, on the shared bounded runtime. A scan decoding
    /// more than the memory bound refuses rather than grow.
    pub(super) fn scan(
        &self,
        tenant: &Tenant,
        files: &[PathBuf],
        filter: Option<Expr>,
    ) -> Outcome<Vec<RecordBatch>> {
        if files.is_empty() {
            return Ok(Vec::new());
        }
        let objects = tenant.objects();
        let paths = files
            .iter()
            .map(|file| {
                catalog::confined(file, &objects).map(|path| path.to_string_lossy().into_owned())
            })
            .collect::<Outcome<Vec<_>>>()?;
        let mut config = SessionConfig::new()
            .with_target_partitions(usize::try_from(self.settings.threads.max(1)).unwrap_or(1))
            .with_batch_size(8192)
            .with_information_schema(false);
        config
            .options_mut()
            .execution
            .parquet
            .schema_force_view_types = false;
        let ctx = SessionContext::new_with_config_rt(config, Arc::clone(&tenant.runtime));
        let batches = self.block_on(async move {
            let frame = ctx
                .read_parquet(paths, ParquetReadOptions::default())
                .await?;
            let frame = match filter {
                Some(filter) => frame.filter(filter)?,
                None => frame,
            };
            frame.collect().await
        })?;
        let batches = df(batches)?;
        let decoded = batches.iter().fold(0_usize, |total, batch| {
            total.saturating_add(batch.get_array_memory_size())
        });
        if decoded > self.memory {
            return Err(refusal(
                EXHAUSTED,
                "a scan decodes more than the memory bound",
            ));
        }
        Ok(batches)
    }

    /// Drive `future` to completion on a current-thread runtime of its own,
    /// on a thread of its own, so a caller inside another runtime never nests
    /// one; memory and spill stay bounded by the tenant's shared `RuntimeEnv`.
    fn block_on<F>(&self, future: F) -> Outcome<F::Output>
    where
        F: std::future::Future + Send,
        F::Output: Send,
    {
        let _settings = &self.settings;
        std::thread::scope(|scope| {
            scope
                .spawn(|| {
                    tokio::runtime::Builder::new_current_thread()
                        .build()
                        .map(|runtime| runtime.block_on(future))
                        .map_err(|error| engine_error(error.to_string()))
                })
                .join()
                .map_err(|_panicked| engine_error("a scan failed"))?
        })
    }

    /// ANALYTICS-FRONTIER-1: the mirror's frontier for a world; a changed
    /// model, schema or link set resets the mirror first.
    ///
    /// # Errors
    /// An engine failure.
    pub fn frontier(&self, request: &FrontierRequest) -> Outcome<FrontierResponse> {
        self.with(&request.tenant, |tenant| mirror::frontier(tenant, request))
    }

    /// Apply one contiguous sync batch: its segments are installed, then
    /// published by one catalog commit with the new frontier.
    ///
    /// # Errors
    /// `analytics.frontier_moved`, a batch of another mirror generation
    /// (`core.compute.stale_input`), or an unknown table.
    pub fn sync(&self, request: &SyncRequest) -> Outcome<FrontierResponse> {
        self.with(&request.tenant, |tenant| mirror::sync(tenant, request))
    }

    /// QUERY-ENGINE-1: evaluate a lowered question exactly.
    ///
    /// # Errors
    /// The Core 03 refusals the exact engine would give, a frontier the
    /// mirror has not reached, or a stale source.
    pub fn evaluate(&self, request: &EvaluateRequest) -> Outcome<EvaluateResponse> {
        self.with(&request.tenant, |tenant| {
            aggregate::evaluate(self, tenant, request)
        })
    }

    /// Page one bucket's members in identity order, over the generation the
    /// question was prepared at.
    ///
    /// # Errors
    /// As [`Self::evaluate`].
    pub fn members(&self, request: &MembersRequest) -> Outcome<MembersResponse> {
        self.with(&request.tenant, |tenant| {
            aggregate::members(self, tenant, request)
        })
    }

    /// BULK-LOAD-1: load `file` (already downloaded) as the dataset's only load.
    ///
    /// # Errors
    /// `bulk.digest_mismatch`, an unreadable file, or an engine failure.
    pub fn load(&self, request: &LoadRequest, file: &Path) -> Outcome<LoadResponse> {
        self.with(&request.tenant, |tenant| {
            dataset::load(tenant, request, file)
        })
    }

    /// MANIFEST-LAND-1 / MANIFEST-LAWS-1: land `request` in a fresh attempt.
    ///
    /// # Errors
    /// As the `DuckDB` engine's landing.
    pub fn land(
        &self,
        request: &LandRequest,
        fetch: impl FnOnce(std::fs::File) -> Outcome<()>,
    ) -> Outcome<LandResponse> {
        landing::land(self, request, fetch)
    }

    /// MANIFEST-LAWS-1: settle a page of an attempt's residue rows.
    ///
    /// # Errors
    /// A stale, busy or unknown attempt, or an invalid finding.
    pub fn settle(&self, request: &SettleRequest) -> Outcome<LandResponse> {
        landing::settle(self, request)
    }

    /// MANIFEST-CACHE-1: keep an attempt's written files as its cache identity.
    ///
    /// # Errors
    /// A stale, busy or unknown attempt, a digest mismatch, or a refused send.
    pub fn store(
        &self,
        request: &StoreRequest,
        send: impl FnOnce(&Path, &Path) -> Outcome<()>,
    ) -> Outcome<LandResponse> {
        landing::store(self, request, send).map(|stored| stored.answer)
    }

    /// Store a call's landed files or return the exact recorded JSON body.
    ///
    /// # Errors
    /// As [`Self::store`], or a legacy body with no legacy receipt.
    pub fn store_json(
        &self,
        body: &StoreBody,
        send: impl FnOnce(&Path, &Path) -> Outcome<()>,
    ) -> Outcome<Box<RawValue>> {
        landing::store_json(self, body, send)
    }

    /// MANIFEST-LINEAGE-1: what the landing holds for one source row.
    ///
    /// # Errors
    /// A cold dataset, a stale load, or an engine failure.
    pub fn lineage(&self, request: &LineageRequest) -> Outcome<LineageResponse> {
        self.with(&request.tenant, |tenant| {
            dataset::lineage(self, tenant, request)
        })
    }

    /// One records page of a source.
    ///
    /// # Errors
    /// An unknown column, a comparison its kind does not take, or a stale source.
    pub fn page(&self, request: &PageRequest) -> Outcome<PageResponse> {
        self.with(&request.tenant, |tenant| page::page(self, tenant, request))
    }

    /// Exact column profiles of a source.
    ///
    /// # Errors
    /// An unknown column or a stale source.
    pub fn profile(&self, request: &ProfileRequest) -> Outcome<ProfileResponse> {
        self.with(&request.tenant, |tenant| {
            profile::profile(self, tenant, request)
        })
    }

    /// Export columns of every row of a source into a scratch file.
    ///
    /// # Errors
    /// An unknown column or a stale source.
    pub fn export(&self, request: &ExportRequest) -> Outcome<PathBuf> {
        let scratch = self
            .scratch(&request.tenant)
            .map_err(|error| engine_error(error.to_string()))?;
        self.with(&request.tenant, |tenant| {
            page::export(self, tenant, request, &scratch)
        })
    }
}

impl super::AnalyticalEngine for Engine {
    fn capabilities(&self) -> EngineCapabilities {
        EngineCapabilities::DATAFUSION
    }
    fn mirror_frontier(&self, request: &FrontierRequest) -> Outcome<FrontierResponse> {
        self.frontier(request)
    }
    fn mirror_sync(&self, request: &SyncRequest) -> Outcome<FrontierResponse> {
        self.sync(request)
    }
    fn evaluate_question(&self, request: &EvaluateRequest) -> Outcome<EvaluateResponse> {
        self.evaluate(request)
    }
    fn query_members(&self, request: &MembersRequest) -> Outcome<MembersResponse> {
        self.members(request)
    }
    fn land_dataset(
        &self,
        request: &LandRequest,
        fetch: &mut dyn FnMut(std::fs::File) -> Outcome<()>,
    ) -> Outcome<LandResponse> {
        self.land(request, fetch)
    }
    fn settle_dataset(&self, request: &SettleRequest) -> Outcome<LandResponse> {
        self.settle(request)
    }
    fn store_dataset(
        &self,
        body: &StoreBody,
        send: &mut dyn FnMut(&Path, &Path) -> Outcome<()>,
    ) -> Outcome<Box<RawValue>> {
        self.store_json(body, send)
    }
    fn query_lineage(&self, request: &LineageRequest) -> Outcome<LineageResponse> {
        self.lineage(request)
    }
    fn query_page(&self, request: &PageRequest) -> Outcome<PageResponse> {
        self.page(request)
    }
    fn profile_objects(&self, request: &ProfileRequest) -> Outcome<ProfileResponse> {
        self.profile(request)
    }
    fn export_dataset(&self, request: &ExportRequest) -> Outcome<PathBuf> {
        self.export(request)
    }
}
