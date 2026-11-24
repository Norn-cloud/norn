//! D136 (#1661): the analytical engine seam.
//!
//! The cell decides and verifies; an engine executes the typed operations of
//! this module's [`AnalyticalEngine`] over one tenant's data, declares what it
//! answers exactly ([`EngineCapabilities`]), and names the snapshot it read
//! ([`SnapshotProvenance`]). The cell accepts an answer only when that
//! snapshot is the one it prepared, read exactly (QUERY-FRESH-1).

use std::path::{Path, PathBuf};

use norn_checker::metric::compute::UNSUPPORTED_ENGINE;
use serde::{Deserialize, Serialize};
use serde_json::value::RawValue;

pub use super::store_body::StoreBody;
use super::{
    EngineRefusal, EvaluateRequest, EvaluateResponse, ExportRequest, FrontierRequest,
    FrontierResponse, LandRequest, LandResponse, LineageRequest, LineageResponse, Lowered,
    MembersRequest, MembersResponse, PageRequest, PageResponse, ProfileRequest, ProfileResponse,
    SettleRequest, Source, SyncRequest, TenantId,
};
use crate::CellError;
use crate::compute::{Engine as ComputeEngine, Freshness, refused};

/// An engine operation's answer or its refusal.
pub type EngineOutcome<T> = Result<T, EngineRefusal>;

/// The analytical engine that read a snapshot.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum AnalyticalEngineId {
    /// `DuckDB`.
    #[serde(rename = "duckdb")]
    DuckDb,
    /// `DataFusion`.
    #[serde(rename = "datafusion")]
    DataFusion,
}

impl AnalyticalEngineId {
    /// The wire name, as an answer envelope reports it.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::DuckDb => "duckdb",
            Self::DataFusion => "datafusion",
        }
    }
}

impl From<AnalyticalEngineId> for ComputeEngine {
    fn from(id: AnalyticalEngineId) -> Self {
        match id {
            AnalyticalEngineId::DuckDb => Self::Duckdb,
            AnalyticalEngineId::DataFusion => Self::DataFusion,
        }
    }
}

/// What an engine answers exactly (D136). A read that needs a capability the
/// engine lacks is refused before it is sent (`core.compute.unsupported_engine`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct EngineCapabilities {
    /// Reads as of a valid time or knowledge cutoff (`exact_bitemporal`).
    pub exact_bitemporal: bool,
}

impl EngineCapabilities {
    /// `DuckDB`'s.
    pub const DUCKDB: Self = Self {
        exact_bitemporal: true,
    };

    /// Whether this engine answers `query` over `source` exactly.
    #[must_use]
    pub const fn answers(self, query: &Lowered, source: &Source) -> bool {
        let historical =
            query.as_of.is_some() || matches!(source, Source::Mirror { model: Some(_), .. });
        self.exact_bitemporal || !historical
    }
}

/// The snapshot an engine read.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", tag = "kind", deny_unknown_fields)]
pub enum ReadSnapshot {
    /// The governed-fact mirror at a commit frontier.
    #[serde(rename_all = "camelCase")]
    Mirror {
        /// The execution world.
        world: String,
        /// The root type's table.
        table: String,
        /// The commit frontier.
        frontier: u64,
        /// The cell scope every row was checked against.
        scope: String,
        /// The cell model the mirror was synced under.
        model_pin: String,
        /// The table schema the mirror holds.
        schema_pin: String,
    },
    /// A loaded dataset at its cache identity.
    Dataset {
        /// The object type's table.
        table: String,
        /// The load's cache identity.
        load: String,
    },
}

/// Which engine answered, over which snapshot, how fresh.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SnapshotProvenance {
    /// The engine.
    pub engine: AnalyticalEngineId,
    /// The tenant database it opened.
    pub tenant: TenantId,
    /// The snapshot it read.
    pub snapshot: ReadSnapshot,
    /// How fresh that snapshot was when read.
    pub freshness: Freshness,
}

impl SnapshotProvenance {
    /// QUERY-FRESH-1: the engine read exactly the snapshot the cell prepared:
    /// its tenant, source (world, table, frontier and scope, or dataset load),
    /// the cell's current model and, for the mirror, the table schema.
    ///
    /// # Errors
    /// `core.compute.stale_input` on any difference or a non-exact read.
    pub fn verify(
        &self,
        tenant: &TenantId,
        source: &Source,
        model_pin: &str,
        schema_pin: Option<&str>,
    ) -> Result<(), CellError> {
        let read = match (&self.snapshot, source) {
            (
                ReadSnapshot::Mirror {
                    world,
                    table,
                    frontier,
                    scope,
                    model_pin: read_model,
                    schema_pin: read_schema,
                },
                Source::Mirror {
                    world: w,
                    table: t,
                    frontier: f,
                    scope: s,
                    ..
                },
            ) => {
                (world, table, frontier, scope) == (w, t, f, s)
                    && read_model == model_pin
                    && schema_pin == Some(read_schema.as_str())
            }
            (ReadSnapshot::Dataset { table, load }, Source::Dataset { table: t, load: l }) => {
                (table, load) == (t, l)
            }
            _ => false,
        };
        if read && self.tenant == *tenant && self.freshness == Freshness::Exact {
            Ok(())
        } else {
            Err(refused("core.compute.stale_input"))
        }
    }
}

fn unsupported() -> EngineRefusal {
    EngineRefusal {
        code: UNSUPPORTED_ENGINE.to_owned(),
        detail: "the analytics engine does not answer this read exactly".to_owned(),
    }
}

/// Evaluate `request` on `engine` only if it answers it exactly.
///
/// # Errors
/// `core.compute.unsupported_engine`, or the engine's refusal.
pub fn dispatch_evaluate(
    engine: &dyn AnalyticalEngine,
    request: &EvaluateRequest,
) -> EngineOutcome<EvaluateResponse> {
    if !engine
        .capabilities()
        .answers(&request.query, &request.source)
    {
        return Err(unsupported());
    }
    engine.evaluate(request)
}

/// Page `request`'s members on `engine` only if it answers them exactly.
///
/// # Errors
/// As [`dispatch_evaluate`].
pub fn dispatch_members(
    engine: &dyn AnalyticalEngine,
    request: &MembersRequest,
) -> EngineOutcome<MembersResponse> {
    if !engine
        .capabilities()
        .answers(&request.query, &request.source)
    {
        return Err(unsupported());
    }
    engine.members(request)
}

/// D136: the typed operations the tenant analytics sidecar serves. No
/// operation carries SQL, a table path or a column expression (#733).
pub trait AnalyticalEngine: Send + Sync {
    /// What this engine answers exactly.
    fn capabilities(&self) -> EngineCapabilities;
    /// The mirror's frontier for a world; a changed model, schema or link
    /// set resets the mirror.
    ///
    /// # Errors
    /// An engine refusal.
    fn frontier(&self, request: &FrontierRequest) -> EngineOutcome<FrontierResponse>;
    /// Apply one contiguous batch of commits to the mirror.
    ///
    /// # Errors
    /// `analytics.frontier_moved`, or an engine refusal.
    fn sync(&self, request: &SyncRequest) -> EngineOutcome<FrontierResponse>;
    /// Answer a lowered question, naming the snapshot read.
    ///
    /// # Errors
    /// A Core 03 refusal, a stale source, or an engine refusal.
    fn evaluate(&self, request: &EvaluateRequest) -> EngineOutcome<EvaluateResponse>;
    /// Page one bucket's members in identity order.
    ///
    /// # Errors
    /// As [`Self::evaluate`].
    fn members(&self, request: &MembersRequest) -> EngineOutcome<MembersResponse>;
    /// MANIFEST-LAND-1: land a dataset upload in an attempt the engine mints;
    /// `fetch` downloads the source into the attempt's file.
    ///
    /// # Errors
    /// A `bulk.*` refusal, or an engine refusal.
    fn land(
        &self,
        request: &LandRequest,
        fetch: &mut dyn FnMut(std::fs::File) -> EngineOutcome<()>,
    ) -> EngineOutcome<LandResponse>;
    /// MANIFEST-LAWS-1: settle a page of an attempt's residue rows.
    ///
    /// # Errors
    /// A stale, busy or unknown attempt, or an engine refusal.
    fn settle(&self, request: &SettleRequest) -> EngineOutcome<LandResponse>;
    /// MANIFEST-LAND-1: store an attempt's landed files through `send`; a
    /// replay answers the recorded bytes.
    ///
    /// # Errors
    /// A stale, busy or unknown attempt, a digest mismatch, or a refused send.
    fn store(
        &self,
        body: &StoreBody,
        send: &mut dyn FnMut(&Path, &Path) -> EngineOutcome<()>,
    ) -> EngineOutcome<Box<RawValue>>;
    /// Trace one dataset source row's admission.
    ///
    /// # Errors
    /// A cold dataset, a stale load, or an engine refusal.
    fn lineage(&self, request: &LineageRequest) -> EngineOutcome<LineageResponse>;
    /// One filtered, sorted page of a source's rows.
    ///
    /// # Errors
    /// An unknown column, a comparison its kind does not take, or a stale source.
    fn page(&self, request: &PageRequest) -> EngineOutcome<PageResponse>;
    /// Exact column profiles of a source, naming the snapshot read.
    ///
    /// # Errors
    /// An unknown column or a stale source.
    fn profile(&self, request: &ProfileRequest) -> EngineOutcome<ProfileResponse>;
    /// Export a source's rows into a tenant scratch file.
    ///
    /// # Errors
    /// An unknown column or a stale source.
    fn export(&self, request: &ExportRequest) -> EngineOutcome<PathBuf>;
}

#[cfg(all(feature = "duckdb", feature = "native"))]
impl AnalyticalEngine for super::engine::Engine {
    fn capabilities(&self) -> EngineCapabilities {
        EngineCapabilities::DUCKDB
    }
    fn frontier(&self, request: &FrontierRequest) -> EngineOutcome<FrontierResponse> {
        Self::frontier(self, request)
    }
    fn sync(&self, request: &SyncRequest) -> EngineOutcome<FrontierResponse> {
        Self::sync(self, request)
    }
    fn evaluate(&self, request: &EvaluateRequest) -> EngineOutcome<EvaluateResponse> {
        Self::evaluate(self, request)
    }
    fn members(&self, request: &MembersRequest) -> EngineOutcome<MembersResponse> {
        Self::members(self, request)
    }
    fn land(
        &self,
        request: &LandRequest,
        fetch: &mut dyn FnMut(std::fs::File) -> EngineOutcome<()>,
    ) -> EngineOutcome<LandResponse> {
        Self::land(self, request, fetch)
    }
    fn settle(&self, request: &SettleRequest) -> EngineOutcome<LandResponse> {
        Self::settle(self, request)
    }
    fn store(
        &self,
        body: &StoreBody,
        send: &mut dyn FnMut(&Path, &Path) -> EngineOutcome<()>,
    ) -> EngineOutcome<Box<RawValue>> {
        self.store_json(body, send)
    }
    fn lineage(&self, request: &LineageRequest) -> EngineOutcome<LineageResponse> {
        Self::lineage(self, request)
    }
    fn page(&self, request: &PageRequest) -> EngineOutcome<PageResponse> {
        Self::page(self, request)
    }
    fn profile(&self, request: &ProfileRequest) -> EngineOutcome<ProfileResponse> {
        Self::profile(self, request)
    }
    fn export(&self, request: &ExportRequest) -> EngineOutcome<PathBuf> {
        Self::export(self, request)
    }
}

#[cfg(all(test, feature = "native"))]
#[path = "backend_tests.rs"]
mod tests;
