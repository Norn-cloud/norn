//! D136/#1661: typed analytical engine boundary, capability declarations,
//! snapshot provenance verification, and fail-closed refusals.
//!
//! This module compiles on every target (including `wasm32` and
//! `--no-default-features`) with zero native or `DuckDB` imports. Concrete
//! backend implementations live behind their respective feature gates.

use std::collections::BTreeMap;
use std::fmt;
use std::path::{Path, PathBuf};

use norn_checker::metric::compute::UNSUPPORTED_ENGINE;
use norn_checker::transform::TransformPlan;
use norn_checker::transform::eval::{TransformInput, TransformOutput};
use serde::{Deserialize, Serialize};
use serde_json::value::RawValue;

use super::{
    EngineRefusal, EvaluateRequest, EvaluateResponse, ExportRequest, FrontierRequest,
    FrontierResponse, LandRequest, LandResponse, LandedFile, LineageRequest, LineageResponse,
    LoadRequest, LoadResponse, MembersRequest, MembersResponse, PageRequest, PageResponse,
    ProfileRequest, ProfileResponse, RESIDUE_PAGE_ROWS, SettleRequest, Source, StoreRequest,
    StoredObject, SyncRequest, TenantId, ValidRange,
};
use crate::CellError;
use crate::compute::{Engine as ComputeEngine, Freshness};

/// Stable refusal code for stale or mismatched analytical reads (`QUERY-FRESH-1`).
pub const STALE_INPUT_CODE: &str = "core.compute.stale_input";
/// Stable refusal code for marked columns outside an enforcing projection (`MANIFEST-MARK-1`).
pub const MARKED_COLUMN_CODE: &str = "privacy.marked_column";

/// Outcome returned by an [`AnalyticalEngine`] operation.
pub type EngineOutcome<T> = Result<T, EngineRefusal>;

/// The analytical engine backend that produced a result.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum AnalyticalEngineId {
    /// Historical `DuckDB` analytical engine (`"duckdb"`).
    #[serde(rename = "duckdb")]
    DuckDb,
    /// `DataFusion` analytical engine (`"datafusion"`).
    #[serde(rename = "datafusion")]
    DataFusion,
}

impl AnalyticalEngineId {
    /// Canonical lowercase wire string (`"duckdb"` or `"datafusion"`).
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::DuckDb => "duckdb",
            Self::DataFusion => "datafusion",
        }
    }

    /// Convert to the shared [`ComputeEngine`] envelope discriminator.
    #[must_use]
    pub const fn into_compute_engine(self) -> ComputeEngine {
        match self {
            Self::DuckDb => ComputeEngine::Duckdb,
            Self::DataFusion => ComputeEngine::DataFusion,
        }
    }
}

impl fmt::Display for AnalyticalEngineId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl From<AnalyticalEngineId> for ComputeEngine {
    fn from(id: AnalyticalEngineId) -> Self {
        id.into_compute_engine()
    }
}

impl TryFrom<ComputeEngine> for AnalyticalEngineId {
    type Error = AnalyticalRefusal;

    fn try_from(engine: ComputeEngine) -> Result<Self, Self::Error> {
        match engine {
            ComputeEngine::Duckdb => Ok(Self::DuckDb),
            ComputeEngine::DataFusion => Ok(Self::DataFusion),
            ComputeEngine::Cell | ComputeEngine::Ivm => Err(AnalyticalRefusal::StaleRead {
                detail: "compute engine is not an analytical backend".to_owned(),
            }),
        }
    }
}

/// One individual D136/#1661 analytical engine capability.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Capability {
    /// Exact bitemporal (as-of knowledge cutoff / valid-time snapshot) reads.
    ExactBitemporal,
    /// Live sub-second freshness / incremental mirror reads.
    LiveSubSecond,
    /// Vector similarity / embedding execution.
    Vector,
    /// Geospatial predicates and indexing.
    Geo,
    /// Zero-copy Apache Iceberg table scanning.
    IcebergZeroCopy,
    /// Engine-side marking enforcement for protected columns/rows.
    MarkingsEnforced,
}

impl Capability {
    /// Stable lowercase identifier for diagnostics and logs.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ExactBitemporal => "exact_bitemporal",
            Self::LiveSubSecond => "live_sub_second",
            Self::Vector => "vector",
            Self::Geo => "geo",
            Self::IcebergZeroCopy => "iceberg_zero_copy",
            Self::MarkingsEnforced => "markings_enforced",
        }
    }
}

/// D136/#1661: individual capabilities an [`AnalyticalEngine`] qualifies for.
///
/// Unqualified capabilities stay `false`. Callers check required capabilities
/// before execution and fail closed on any missing capability.
#[expect(
    clippy::struct_excessive_bools,
    reason = "D136/#1661 declares six independent analytical engine capabilities as explicit booleans"
)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EngineCapabilities {
    /// Exact bitemporal (as-of knowledge cutoff / valid-time snapshot) reads.
    pub exact_bitemporal: bool,
    /// Live sub-second freshness / incremental mirror reads.
    pub live_sub_second: bool,
    /// Vector similarity / embedding execution.
    pub vector: bool,
    /// Geospatial predicates and indexing.
    pub geo: bool,
    /// Zero-copy Apache Iceberg table scanning.
    pub iceberg_zero_copy: bool,
    /// Engine-side marking enforcement for protected columns/rows.
    pub markings_enforced: bool,
}

impl EngineCapabilities {
    /// No capabilities qualified (fail-closed baseline).
    #[must_use]
    pub const fn none() -> Self {
        Self {
            exact_bitemporal: false,
            live_sub_second: false,
            vector: false,
            geo: false,
            iceberg_zero_copy: false,
            markings_enforced: false,
        }
    }

    /// Capabilities demonstrated by the existing `DuckDB` engine:
    /// exact bitemporal reads over the governed mirror (`ASOF-KNOWLEDGE-1`,
    /// `METRIC-SNAPSHOT-ASOF-1`); all other capabilities stay `false` until
    /// independently qualified.
    #[must_use]
    pub const fn duckdb_default() -> Self {
        Self {
            exact_bitemporal: true,
            live_sub_second: false,
            vector: false,
            geo: false,
            iceberg_zero_copy: false,
            markings_enforced: false,
        }
    }

    /// Whether `capability` is qualified on this engine.
    #[must_use]
    pub const fn supports(&self, capability: Capability) -> bool {
        match capability {
            Capability::ExactBitemporal => self.exact_bitemporal,
            Capability::LiveSubSecond => self.live_sub_second,
            Capability::Vector => self.vector,
            Capability::Geo => self.geo,
            Capability::IcebergZeroCopy => self.iceberg_zero_copy,
            Capability::MarkingsEnforced => self.markings_enforced,
        }
    }

    /// Fail closed if `capability` is not qualified.
    ///
    /// # Errors
    /// [`AnalyticalRefusal::UnsupportedCapability`] when `capability` is `false`.
    pub fn require_capability(&self, capability: Capability) -> Result<(), AnalyticalRefusal> {
        if self.supports(capability) {
            Ok(())
        } else {
            Err(AnalyticalRefusal::UnsupportedCapability {
                capability,
                detail: format!(
                    "analytical engine does not qualify for {}",
                    capability.as_str()
                ),
            })
        }
    }

    /// Fail closed if any capability requested in `required` is absent.
    ///
    /// # Errors
    /// [`AnalyticalRefusal::UnsupportedCapability`] for the first missing capability.
    pub fn require(&self, required: Self) -> Result<(), AnalyticalRefusal> {
        for (needed, capability) in [
            (required.exact_bitemporal, Capability::ExactBitemporal),
            (required.live_sub_second, Capability::LiveSubSecond),
            (required.vector, Capability::Vector),
            (required.geo, Capability::Geo),
            (required.iceberg_zero_copy, Capability::IcebergZeroCopy),
            (required.markings_enforced, Capability::MarkingsEnforced),
        ] {
            if needed {
                self.require_capability(capability)?;
            }
        }
        Ok(())
    }

    /// Fail closed when a read touches marked columns without engine-side
    /// marking enforcement (`PRIVACY-1` / `MANIFEST-MARK-1`).
    ///
    /// # Errors
    /// [`AnalyticalRefusal::InsufficientProtection`] when `has_marked_columns`
    /// is true and `markings_enforced` is false.
    pub fn verify_markings_protection(
        &self,
        has_marked_columns: bool,
    ) -> Result<(), AnalyticalRefusal> {
        if has_marked_columns && !self.markings_enforced {
            return Err(AnalyticalRefusal::InsufficientProtection {
                reason: ProtectionReason::MarkingsNotEnforced,
                detail: "analytical engine does not enforce column markings".to_owned(),
            });
        }
        Ok(())
    }
}

/// Why a read lacked required protection at the analytical boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProtectionReason {
    /// Strict law reads require exact, complete governed snapshots (`QUERY-FRESH-1`).
    StrictLawRequiresExact,
    /// Marked columns cannot be evaluated on an engine without marking enforcement.
    MarkingsNotEnforced,
    /// Loaded datasets are analytical evidence, never authoritative for strict law reads.
    UnverifiedDatasetForStrictLaw,
}

impl ProtectionReason {
    /// Stable refusal code for this protection failure.
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::StrictLawRequiresExact => STALE_INPUT_CODE,
            Self::MarkingsNotEnforced => MARKED_COLUMN_CODE,
            Self::UnverifiedDatasetForStrictLaw => UNSUPPORTED_ENGINE,
        }
    }
}

/// Fail-closed typed refusals at the analytical engine boundary.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind", deny_unknown_fields)]
pub enum AnalyticalRefusal {
    /// Snapshot, tenant, world, model pin, commit frontier, or dataset load
    /// does not match the prepared request.
    StaleRead {
        /// Operator-facing reason; never row data.
        detail: String,
    },
    /// The backend does not qualify for a required D136 capability.
    UnsupportedCapability {
        /// The missing capability.
        capability: Capability,
        /// Operator-facing reason; never row data.
        detail: String,
    },
    /// The read lacks required protection (strict law exactness or markings).
    InsufficientProtection {
        /// Why protection is insufficient.
        reason: ProtectionReason,
        /// Operator-facing reason; never row data.
        detail: String,
    },
}

impl AnalyticalRefusal {
    /// Stable Core 03 / privacy refusal code.
    #[must_use]
    pub const fn code(&self) -> &'static str {
        match self {
            Self::StaleRead { .. } => STALE_INPUT_CODE,
            Self::UnsupportedCapability { .. } => UNSUPPORTED_ENGINE,
            Self::InsufficientProtection { reason, .. } => reason.code(),
        }
    }

    /// Operator-facing detail string; never carries row data.
    #[must_use]
    pub fn detail(&self) -> &str {
        match self {
            Self::StaleRead { detail }
            | Self::UnsupportedCapability { detail, .. }
            | Self::InsufficientProtection { detail, .. } => detail,
        }
    }

    /// Convert to the wire [`EngineRefusal`] payload.
    #[must_use]
    pub fn to_engine_refusal(&self) -> EngineRefusal {
        EngineRefusal {
            code: self.code().to_owned(),
            detail: self.detail().to_owned(),
        }
    }

    /// Convert to the cell's typed [`CellError::ComputeRefused`].
    #[must_use]
    pub const fn to_cell_error(&self) -> CellError {
        CellError::ComputeRefused(self.code())
    }
}

impl fmt::Display for AnalyticalRefusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.code(), self.detail())
    }
}

impl std::error::Error for AnalyticalRefusal {}

impl From<AnalyticalRefusal> for EngineRefusal {
    fn from(refusal: AnalyticalRefusal) -> Self {
        refusal.to_engine_refusal()
    }
}

impl From<AnalyticalRefusal> for CellError {
    fn from(refusal: AnalyticalRefusal) -> Self {
        refusal.to_cell_error()
    }
}

/// Which snapshot an analytical engine actually read to answer a request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind", deny_unknown_fields)]
pub enum ReadSnapshot {
    /// Governed-fact mirror read at an exact commit frontier.
    Mirror {
        /// Execution world actually read.
        world: String,
        /// Root table actually read.
        table: String,
        /// Commit frontier actually read.
        frontier: u64,
        /// Cell scope verified on the read.
        scope: String,
        /// Model hash pin verified on the read (when pinned, e.g. knowledge cutoff).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        model_pin: Option<String>,
    },
    /// Loaded dataset read at a specific committed load / cache identity.
    Dataset {
        /// Dataset object type table actually read.
        table: String,
        /// Committed load / cache identity actually read.
        load: String,
    },
}

impl ReadSnapshot {
    /// Build the snapshot identity from a verified [`Source`].
    #[must_use]
    pub fn from_source(source: &Source) -> Self {
        match source {
            Source::Mirror {
                world,
                table,
                frontier,
                scope,
                model,
                ..
            } => Self::Mirror {
                world: world.clone(),
                table: table.clone(),
                frontier: *frontier,
                scope: scope.clone(),
                model_pin: model.clone(),
            },
            Source::Dataset { table, load } => Self::Dataset {
                table: table.clone(),
                load: load.clone(),
            },
        }
    }
}

/// Snapshot-actually-read and provenance/freshness envelope returned with
/// analytical results.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SnapshotProvenance {
    /// The analytical engine backend that executed the read.
    pub engine: AnalyticalEngineId,
    /// Tenant database actually opened and read.
    pub tenant: TenantId,
    /// Snapshot metadata actually read (mirror world/frontier/scope/model or dataset load).
    pub snapshot: ReadSnapshot,
    /// Optional freshness classification reported alongside the read.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub freshness: Option<Freshness>,
}

impl SnapshotProvenance {
    /// Construct provenance for a completed analytical read.
    #[must_use]
    pub const fn new(engine: AnalyticalEngineId, tenant: TenantId, snapshot: ReadSnapshot) -> Self {
        Self {
            engine,
            tenant,
            snapshot,
            freshness: None,
        }
    }

    /// Attach a freshness classification to this provenance envelope.
    #[must_use]
    pub const fn with_freshness(mut self, freshness: Freshness) -> Self {
        self.freshness = Some(freshness);
        self
    }

    /// Verify that the snapshot actually read matches the cell's prepared
    /// tenant, world/table/scope, model pin, and commit frontier or dataset load.
    ///
    /// # Errors
    /// [`AnalyticalRefusal::StaleRead`] on any mismatch.
    pub fn verify_against(
        &self,
        expected_tenant: &TenantId,
        expected_source: &Source,
    ) -> Result<(), AnalyticalRefusal> {
        if &self.tenant != expected_tenant {
            return Err(AnalyticalRefusal::StaleRead {
                detail: "analytical response tenant does not match prepared tenant".to_owned(),
            });
        }
        match (&self.snapshot, expected_source) {
            (
                ReadSnapshot::Mirror {
                    world,
                    table,
                    frontier,
                    scope,
                    model_pin,
                },
                Source::Mirror {
                    world: exp_world,
                    table: exp_table,
                    frontier: exp_frontier,
                    scope: exp_scope,
                    model: exp_model,
                    ..
                },
            ) => {
                if world != exp_world || table != exp_table || scope != exp_scope {
                    return Err(AnalyticalRefusal::StaleRead {
                        detail: "analytical response world, table, or scope does not match"
                            .to_owned(),
                    });
                }
                if frontier != exp_frontier {
                    return Err(AnalyticalRefusal::StaleRead {
                        detail: "analytical response frontier does not match prepared commit"
                            .to_owned(),
                    });
                }
                if model_pin != exp_model {
                    return Err(AnalyticalRefusal::StaleRead {
                        detail: "analytical response model pin does not match prepared model"
                            .to_owned(),
                    });
                }
                Ok(())
            }
            (
                ReadSnapshot::Dataset { table, load },
                Source::Dataset {
                    table: exp_table,
                    load: exp_load,
                },
            ) => {
                if table != exp_table || load != exp_load {
                    return Err(AnalyticalRefusal::StaleRead {
                        detail: "analytical response dataset load does not match prepared manifest"
                            .to_owned(),
                    });
                }
                Ok(())
            }
            (ReadSnapshot::Mirror { .. }, Source::Dataset { .. })
            | (ReadSnapshot::Dataset { .. }, Source::Mirror { .. }) => {
                Err(AnalyticalRefusal::StaleRead {
                    detail: "analytical response source kind does not match prepared source"
                        .to_owned(),
                })
            }
        }
    }

    /// Enforce `QUERY-FRESH-1` / Core 03 strict law read requirements:
    /// strict law reads require a governed mirror snapshot that is both
    /// [`Freshness::Exact`] and complete (never stale, incomplete, or a loaded
    /// dataset).
    ///
    /// # Errors
    /// [`AnalyticalRefusal::InsufficientProtection`] when the read is not
    /// eligible for strict law evaluation.
    pub fn verify_strict_law_read(
        &self,
        freshness: Freshness,
        complete: bool,
    ) -> Result<(), AnalyticalRefusal> {
        if matches!(self.snapshot, ReadSnapshot::Dataset { .. }) {
            return Err(AnalyticalRefusal::InsufficientProtection {
                reason: ProtectionReason::UnverifiedDatasetForStrictLaw,
                detail: "strict law reads require governed mirror facts, not loaded datasets"
                    .to_owned(),
            });
        }
        if !complete || freshness != Freshness::Exact {
            return Err(AnalyticalRefusal::InsufficientProtection {
                reason: ProtectionReason::StrictLawRequiresExact,
                detail: "strict law reads require exact, complete snapshots".to_owned(),
            });
        }
        if self
            .freshness
            .is_some_and(|reported| reported != Freshness::Exact)
        {
            return Err(AnalyticalRefusal::InsufficientProtection {
                reason: ProtectionReason::StrictLawRequiresExact,
                detail: "analytical provenance reported non-exact freshness for strict law read"
                    .to_owned(),
            });
        }
        Ok(())
    }
}

/// Verify that a [`ProfileResponse`] carries provenance matching `tenant` and `source`.
///
/// # Errors
/// [`AnalyticalRefusal::StaleRead`] when provenance is absent or mismatched.
pub fn verify_profile_provenance<'a>(
    response: &'a ProfileResponse,
    tenant: &TenantId,
    source: &Source,
) -> Result<&'a SnapshotProvenance, AnalyticalRefusal> {
    let provenance = response
        .provenance
        .as_ref()
        .ok_or_else(|| AnalyticalRefusal::StaleRead {
            detail: "analytical profile response omitted snapshot provenance".to_owned(),
        })?;
    provenance.verify_against(tenant, source)?;
    Ok(provenance)
}

/// A store body from either the current cell or a cell replaying a
/// pre-law store. Legacy bodies can only replay a legacy receipt.
#[derive(Deserialize)]
#[serde(try_from = "Box<RawValue>")]
pub struct StoreBody {
    request: StoreRequest,
    legacy: bool,
}

impl StoreBody {
    /// The decoded request, including the scope and object digests.
    #[must_use]
    pub const fn request(&self) -> &StoreRequest {
        &self.request
    }

    /// Whether this body decoded from the pre-law legacy store shape.
    #[must_use]
    pub const fn is_legacy(&self) -> bool {
        self.legacy
    }
}

impl TryFrom<Box<RawValue>> for StoreBody {
    type Error = serde_json::Error;

    fn try_from(body: Box<RawValue>) -> Result<Self, Self::Error> {
        match serde_json::from_str(body.get()) {
            Ok(request) => Ok(Self {
                request,
                legacy: false,
            }),
            Err(error) => legacy_store_request(&body)
                .map(|request| Self {
                    request,
                    legacy: true,
                })
                .map_err(|_old_shape_error| error),
        }
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct LegacyLanding {
    load: LoadRequest,
    pins: crate::dataset_manifest::ManifestPins,
    #[serde(default)]
    landing: String,
    #[serde(default)]
    attempt: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct LegacyLandingAnswer {
    landing: String,
    attempt: String,
    load: LoadResponse,
    data: LandedFile,
    held: LandedFile,
    valid_time: Vec<ValidRange>,
    stored: Option<String>,
}

impl From<LegacyLandingAnswer> for LandResponse {
    fn from(old: LegacyLandingAnswer) -> Self {
        Self {
            landing: old.landing,
            attempt: old.attempt,
            load: old.load,
            data: Some(old.data),
            held: Some(old.held),
            valid_time: old.valid_time,
            stored: old.stored,
            findings: BTreeMap::new(),
            held_by_rules: 0,
            residue_total: 0,
            residue: Vec::new(),
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct LegacyStore {
    land: LegacyLanding,
    landed: LegacyLandingAnswer,
    data: StoredObject,
    held: StoredObject,
}

/// Decode a pre-law legacy store request payload into [`StoreRequest`].
///
/// # Errors
/// JSON decoding error if `raw` does not match the legacy store shape.
#[cfg(all(feature = "duckdb", feature = "native"))]
pub(crate) fn legacy_store_request_from_raw(raw: &RawValue) -> serde_json::Result<StoreRequest> {
    legacy_store_request(raw)
}

/// Decode a pre-law legacy landing answer payload into [`LandResponse`].
///
/// # Errors
/// JSON decoding error if `raw` does not match the legacy answer shape.
#[cfg(all(feature = "duckdb", feature = "native"))]
pub(crate) fn legacy_land_response_from_raw(raw: &RawValue) -> serde_json::Result<LandResponse> {
    serde_json::from_str::<LegacyLandingAnswer>(raw.get()).map(Into::into)
}

fn legacy_store_request(raw: &RawValue) -> serde_json::Result<StoreRequest> {
    let old: LegacyStore = serde_json::from_str(raw.get())?;
    Ok(StoreRequest {
        land: LandRequest {
            load: old.land.load,
            pins: old.land.pins,
            landing: old.land.landing,
            attempt: old.land.attempt,
            checks: Vec::new(),
            residue_page: RESIDUE_PAGE_ROWS,
        },
        landed: old.landed.into(),
        data: old.data,
        held: old.held,
    })
}

/// An engine-admitted transform ready to execute over an authorized snapshot.
///
/// Produced by [`AnalyticalEngine::admit_transform`] *before* the cell reads
/// any rows, preserving the invariant that an unsupported plan refuses before
/// touching cell data.
pub trait AdmittedTransform: Send + Sync {
    /// Execute the admitted transform over one authorized snapshot.
    ///
    /// # Errors
    /// Bound exceeded, unsupported semantics, or engine failure.
    fn execute(
        &self,
        plan: &TransformPlan,
        input: &TransformInput,
    ) -> Result<TransformOutput, CellError>;
}

/// D136/#1661: typed analytical engine boundary shared by `InProcess` and `norn-olapd`.
///
/// The cell keeps authorization, `mirror`, `lower`, `assemble`, and `dataset_laws`;
/// the engine executes only typed operations over one verified tenant's data.
pub trait AnalyticalEngine: Send + Sync {
    /// Canonical engine backend identity reported in provenance and envelopes.
    fn engine_id(&self) -> AnalyticalEngineId;

    /// Individual D136/#1661 capabilities this engine qualifies for.
    fn capabilities(&self) -> EngineCapabilities;

    /// Fail closed unless this engine qualifies for every capability in `required`.
    ///
    /// # Errors
    /// [`AnalyticalRefusal::UnsupportedCapability`] when any required capability is absent.
    fn require_capabilities(&self, required: EngineCapabilities) -> Result<(), AnalyticalRefusal> {
        self.capabilities().require(required)
    }

    /// Return the mirror's committed frontier for a tenant world, resetting on schema pin drift.
    ///
    /// # Errors
    /// An engine refusal.
    fn mirror_frontier(&self, request: &FrontierRequest) -> EngineOutcome<FrontierResponse>;

    /// Apply one contiguous commit batch to the governed-fact mirror.
    ///
    /// # Errors
    /// `analytics.frontier_moved` or an engine refusal.
    fn mirror_sync(&self, request: &SyncRequest) -> EngineOutcome<FrontierResponse>;

    /// Evaluate a lowered question over a verified mirror frontier or dataset load.
    ///
    /// # Errors
    /// Core 03 compute refusals, stale frontier/load, or an engine refusal.
    fn evaluate_question(&self, request: &EvaluateRequest) -> EngineOutcome<EvaluateResponse>;

    /// Page one bucket's contributing members in identity order.
    ///
    /// # Errors
    /// As [`Self::evaluate_question`].
    fn query_members(&self, request: &MembersRequest) -> EngineOutcome<MembersResponse>;

    /// Load a downloaded dataset file directly into its object type's table.
    ///
    /// # Errors
    /// `bulk.digest_mismatch`, unreadable file, or an engine refusal.
    fn load_dataset(&self, request: &LoadRequest, file: &Path) -> EngineOutcome<LoadResponse>;

    /// Land a presigned dataset upload in an engine-minted attempt; host supplies authenticated download.
    ///
    /// # Errors
    /// `bulk.*` refusals or an engine refusal.
    fn land_dataset(
        &self,
        request: &LandRequest,
        fetch: &mut dyn FnMut(std::fs::File) -> EngineOutcome<()>,
    ) -> EngineOutcome<LandResponse>;

    /// Settle a page of residue rows with the cell's per-row law findings.
    ///
    /// # Errors
    /// Stale/busy/unknown attempt, invalid finding, or an engine refusal.
    fn settle_dataset(&self, request: &SettleRequest) -> EngineOutcome<LandResponse>;

    /// Store an attempt's landed files; host supplies authenticated upload.
    ///
    /// # Errors
    /// Stale/busy/unknown attempt, digest mismatch, or upload/engine refusal.
    fn store_dataset(
        &self,
        request: &StoreRequest,
        send: &mut dyn FnMut(&Path, &Path) -> EngineOutcome<()>,
    ) -> EngineOutcome<LandResponse>;

    /// Store an attempt's landed files, preserving exact raw JSON bytes on replay.
    ///
    /// # Errors
    /// As [`Self::store_dataset`], or legacy receipt mismatch.
    fn store_dataset_json(
        &self,
        body: &StoreBody,
        send: &mut dyn FnMut(&Path, &Path) -> EngineOutcome<()>,
    ) -> EngineOutcome<Box<RawValue>>;

    /// Trace one dataset source row's admission status and law findings.
    ///
    /// # Errors
    /// Cold dataset, stale load, or an engine refusal.
    fn query_lineage(&self, request: &LineageRequest) -> EngineOutcome<LineageResponse>;

    /// Return one filtered, sorted records page from a dataset or mirror source.
    ///
    /// # Errors
    /// Unknown column, unsupported comparison, or stale source.
    fn query_page(&self, request: &PageRequest) -> EngineOutcome<PageResponse>;

    /// Compute exact column profiles over a verified snapshot.
    ///
    /// # Errors
    /// Unknown column or stale source.
    fn profile_objects(&self, request: &ProfileRequest) -> EngineOutcome<ProfileResponse>;

    /// Export columns of every row of a source into a tenant scratch file.
    ///
    /// # Errors
    /// Unknown column or stale source.
    fn export_dataset(&self, request: &ExportRequest) -> EngineOutcome<PathBuf>;

    /// Admit (validate/lower) a transform plan *before* reading any cell snapshot.
    ///
    /// # Errors
    /// `core.compute.unsupported_engine` when the backend cannot lower the plan.
    fn admit_transform(
        &self,
        plan: &TransformPlan,
    ) -> Result<Box<dyn AdmittedTransform>, CellError>;
}

#[cfg(all(feature = "duckdb", feature = "native"))]
impl AnalyticalEngine for super::engine::Engine {
    fn engine_id(&self) -> AnalyticalEngineId {
        AnalyticalEngineId::DuckDb
    }

    fn capabilities(&self) -> EngineCapabilities {
        EngineCapabilities::duckdb_default()
    }

    fn mirror_frontier(&self, request: &FrontierRequest) -> EngineOutcome<FrontierResponse> {
        self.frontier(request)
    }

    fn mirror_sync(&self, request: &SyncRequest) -> EngineOutcome<FrontierResponse> {
        self.sync(request)
    }

    fn evaluate_question(&self, request: &EvaluateRequest) -> EngineOutcome<EvaluateResponse> {
        self.evaluate(request)
    }

    fn query_members(&self, request: &MembersRequest) -> EngineOutcome<MembersResponse> {
        self.members(request)
    }

    fn load_dataset(&self, request: &LoadRequest, file: &Path) -> EngineOutcome<LoadResponse> {
        self.load(request, file)
    }

    fn land_dataset(
        &self,
        request: &LandRequest,
        fetch: &mut dyn FnMut(std::fs::File) -> EngineOutcome<()>,
    ) -> EngineOutcome<LandResponse> {
        self.land(request, fetch)
    }

    fn settle_dataset(&self, request: &SettleRequest) -> EngineOutcome<LandResponse> {
        self.settle(request)
    }

    fn store_dataset(
        &self,
        request: &StoreRequest,
        send: &mut dyn FnMut(&Path, &Path) -> EngineOutcome<()>,
    ) -> EngineOutcome<LandResponse> {
        self.store(request, send)
    }

    fn store_dataset_json(
        &self,
        body: &StoreBody,
        send: &mut dyn FnMut(&Path, &Path) -> EngineOutcome<()>,
    ) -> EngineOutcome<Box<RawValue>> {
        self.store_json(body, send)
    }

    fn query_lineage(&self, request: &LineageRequest) -> EngineOutcome<LineageResponse> {
        self.lineage(request)
    }

    fn query_page(&self, request: &PageRequest) -> EngineOutcome<PageResponse> {
        self.page(request)
    }

    fn profile_objects(&self, request: &ProfileRequest) -> EngineOutcome<ProfileResponse> {
        self.profile(request)
    }

    fn export_dataset(&self, request: &ExportRequest) -> EngineOutcome<PathBuf> {
        self.export(request)
    }

    fn admit_transform(
        &self,
        plan: &TransformPlan,
    ) -> Result<Box<dyn AdmittedTransform>, CellError> {
        crate::compute::olap_transform::admit(plan)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn engine_ids_serialize_exact_lowercase_datafusion_and_duckdb() {
        assert_eq!(
            serde_json::to_string(&AnalyticalEngineId::DataFusion)
                .ok()
                .as_deref(),
            Some("\"datafusion\"")
        );
        assert_eq!(
            serde_json::to_string(&AnalyticalEngineId::DuckDb)
                .ok()
                .as_deref(),
            Some("\"duckdb\"")
        );
        assert_eq!(
            serde_json::from_str::<AnalyticalEngineId>("\"datafusion\"").ok(),
            Some(AnalyticalEngineId::DataFusion)
        );
        assert_eq!(
            serde_json::from_str::<AnalyticalEngineId>("\"duckdb\"").ok(),
            Some(AnalyticalEngineId::DuckDb)
        );
        assert!(serde_json::from_str::<AnalyticalEngineId>("\"data_fusion\"").is_err());

        assert_eq!(
            serde_json::to_string(&ComputeEngine::DataFusion)
                .ok()
                .as_deref(),
            Some("\"datafusion\"")
        );
        assert_eq!(
            serde_json::to_string(&ComputeEngine::Duckdb)
                .ok()
                .as_deref(),
            Some("\"duckdb\"")
        );
        assert_eq!(
            serde_json::from_str::<ComputeEngine>("\"datafusion\"").ok(),
            Some(ComputeEngine::DataFusion)
        );
        assert_eq!(
            serde_json::from_str::<ComputeEngine>("\"duckdb\"").ok(),
            Some(ComputeEngine::Duckdb)
        );
        assert!(serde_json::from_str::<ComputeEngine>("\"data_fusion\"").is_err());
        assert_eq!(
            AnalyticalEngineId::try_from(ComputeEngine::DataFusion).ok(),
            Some(AnalyticalEngineId::DataFusion)
        );
        assert_eq!(
            AnalyticalEngineId::try_from(ComputeEngine::Duckdb).ok(),
            Some(AnalyticalEngineId::DuckDb)
        );
        assert!(AnalyticalEngineId::try_from(ComputeEngine::Cell).is_err());
    }

    #[test]
    fn capabilities_declare_all_six_d136_flags_and_fail_closed() {
        let caps = EngineCapabilities::duckdb_default();
        assert!(caps.exact_bitemporal);
        assert!(!caps.live_sub_second);
        assert!(!caps.vector);
        assert!(!caps.geo);
        assert!(!caps.iceberg_zero_copy);
        assert!(!caps.markings_enforced);

        assert!(caps.require_capability(Capability::ExactBitemporal).is_ok());
        for missing in [
            Capability::LiveSubSecond,
            Capability::Vector,
            Capability::Geo,
            Capability::IcebergZeroCopy,
            Capability::MarkingsEnforced,
        ] {
            let err = caps.require_capability(missing).err();
            assert!(matches!(
                err,
                Some(AnalyticalRefusal::UnsupportedCapability { capability, .. })
                    if capability == missing
            ));
            assert_eq!(
                err.as_ref().map(AnalyticalRefusal::code),
                Some(UNSUPPORTED_ENGINE)
            );
        }

        let need_vector = EngineCapabilities {
            exact_bitemporal: true,
            vector: true,
            ..EngineCapabilities::none()
        };
        assert!(matches!(
            caps.require(need_vector),
            Err(AnalyticalRefusal::UnsupportedCapability {
                capability: Capability::Vector,
                ..
            })
        ));
    }

    #[test]
    fn snapshot_provenance_verifies_tenant_world_model_frontier_and_dataset_load() {
        let tenant = TenantId::of_scope("org_demo/prod");
        let other_tenant = TenantId::of_scope("org_other/prod");
        let mirror = Source::Mirror {
            world: "main".to_owned(),
            table: "demo.Conversion".to_owned(),
            frontier: 42,
            scope: "org_demo/prod".to_owned(),
            excluded: Vec::new(),
            model: Some("model-hash-1".to_owned()),
        };
        let ok = SnapshotProvenance::new(
            AnalyticalEngineId::DataFusion,
            tenant.clone(),
            ReadSnapshot::from_source(&mirror),
        );
        assert!(ok.verify_against(&tenant, &mirror).is_ok());

        // Tenant mismatch refuses as stale read.
        assert!(matches!(
            ok.verify_against(&other_tenant, &mirror),
            Err(AnalyticalRefusal::StaleRead { .. })
        ));

        // Frontier mismatch refuses as stale read.
        let moved_frontier = Source::Mirror {
            world: "main".to_owned(),
            table: "demo.Conversion".to_owned(),
            frontier: 43,
            scope: "org_demo/prod".to_owned(),
            excluded: Vec::new(),
            model: Some("model-hash-1".to_owned()),
        };
        assert!(matches!(
            ok.verify_against(&tenant, &moved_frontier),
            Err(AnalyticalRefusal::StaleRead { .. })
        ));

        // Model pin mismatch refuses as stale read.
        let moved_model = Source::Mirror {
            world: "main".to_owned(),
            table: "demo.Conversion".to_owned(),
            frontier: 42,
            scope: "org_demo/prod".to_owned(),
            excluded: Vec::new(),
            model: Some("model-hash-2".to_owned()),
        };
        assert!(matches!(
            ok.verify_against(&tenant, &moved_model),
            Err(AnalyticalRefusal::StaleRead { .. })
        ));

        // World mismatch refuses as stale read.
        let moved_world = Source::Mirror {
            world: "fork-1".to_owned(),
            table: "demo.Conversion".to_owned(),
            frontier: 42,
            scope: "org_demo/prod".to_owned(),
            excluded: Vec::new(),
            model: Some("model-hash-1".to_owned()),
        };
        assert!(matches!(
            ok.verify_against(&tenant, &moved_world),
            Err(AnalyticalRefusal::StaleRead { .. })
        ));

        // Dataset load verification and cross-kind rejection.
        let dataset = Source::Dataset {
            table: "demo.Conversion".to_owned(),
            load: "l-abc".to_owned(),
        };
        assert!(matches!(
            ok.verify_against(&tenant, &dataset),
            Err(AnalyticalRefusal::StaleRead { .. })
        ));
        let ds_prov = SnapshotProvenance::new(
            AnalyticalEngineId::DuckDb,
            tenant.clone(),
            ReadSnapshot::from_source(&dataset),
        );
        assert!(ds_prov.verify_against(&tenant, &dataset).is_ok());
        let other_load = Source::Dataset {
            table: "demo.Conversion".to_owned(),
            load: "l-def".to_owned(),
        };
        assert!(matches!(
            ds_prov.verify_against(&tenant, &other_load),
            Err(AnalyticalRefusal::StaleRead { .. })
        ));
    }

    #[test]
    fn insufficient_protection_refuses_strict_law_and_unmarked_reads_fail_closed() {
        let tenant = TenantId::of_scope("org_demo/prod");
        let mirror_prov = SnapshotProvenance::new(
            AnalyticalEngineId::DataFusion,
            tenant.clone(),
            ReadSnapshot::Mirror {
                world: "main".to_owned(),
                table: "demo.Conversion".to_owned(),
                frontier: 10,
                scope: "org_demo/prod".to_owned(),
                model_pin: None,
            },
        );
        assert!(
            mirror_prov
                .verify_strict_law_read(Freshness::Exact, true)
                .is_ok()
        );
        assert!(matches!(
            mirror_prov.verify_strict_law_read(Freshness::Stale, true),
            Err(AnalyticalRefusal::InsufficientProtection {
                reason: ProtectionReason::StrictLawRequiresExact,
                ..
            })
        ));
        assert!(matches!(
            mirror_prov.verify_strict_law_read(Freshness::Exact, false),
            Err(AnalyticalRefusal::InsufficientProtection {
                reason: ProtectionReason::StrictLawRequiresExact,
                ..
            })
        ));
        let stale_tagged = mirror_prov.with_freshness(Freshness::Fresh);
        assert!(matches!(
            stale_tagged.verify_strict_law_read(Freshness::Exact, true),
            Err(AnalyticalRefusal::InsufficientProtection {
                reason: ProtectionReason::StrictLawRequiresExact,
                ..
            })
        ));

        let ds_prov = SnapshotProvenance::new(
            AnalyticalEngineId::DuckDb,
            tenant,
            ReadSnapshot::Dataset {
                table: "demo.Conversion".to_owned(),
                load: "l-1".to_owned(),
            },
        );
        assert!(matches!(
            ds_prov.verify_strict_law_read(Freshness::Exact, true),
            Err(AnalyticalRefusal::InsufficientProtection {
                reason: ProtectionReason::UnverifiedDatasetForStrictLaw,
                ..
            })
        ));

        let caps = EngineCapabilities::duckdb_default();
        assert!(caps.verify_markings_protection(false).is_ok());
        assert!(matches!(
            caps.verify_markings_protection(true),
            Err(AnalyticalRefusal::InsufficientProtection {
                reason: ProtectionReason::MarkingsNotEnforced,
                ..
            })
        ));
    }
}

#[cfg(all(test, feature = "native"))]
mod store_tests {
    use std::collections::{BTreeMap, BTreeSet};
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicUsize, Ordering};

    use norn_checker::EffectiveCatalog;
    use norn_checker::metric::compute::UNSUPPORTED_ENGINE;
    use norn_checker::transform::TransformPlan;
    use norn_checker::transform::eval::{TransformInput, TransformOutput};
    use norn_ir::{ArtifactEnvelope, MetricDefinition, ObjectTypeDef, QuestionIR};
    use serde_json::json;

    use super::*;
    use crate::compute::transform::RowAccess;
    use crate::compute::{QueryContext, Watermark};
    use crate::{CellError, CellStore, MAIN_WORLD, Model};

    struct PassThroughTransform;

    impl AdmittedTransform for PassThroughTransform {
        fn execute(
            &self,
            plan: &TransformPlan,
            input: &TransformInput,
        ) -> Result<TransformOutput, CellError> {
            norn_checker::transform::eval::evaluate(plan, input)
                .map_err(|_err| CellError::ComputeRefused(UNSUPPORTED_ENGINE))
        }
    }

    struct StubAnalyticalEngine {
        id: AnalyticalEngineId,
        admit_transforms: bool,
    }

    impl StubAnalyticalEngine {
        fn unsupported() -> EngineRefusal {
            EngineRefusal {
                code: UNSUPPORTED_ENGINE.to_owned(),
                detail: "stub engine".to_owned(),
            }
        }
    }

    impl AnalyticalEngine for StubAnalyticalEngine {
        fn engine_id(&self) -> AnalyticalEngineId {
            self.id
        }

        fn capabilities(&self) -> EngineCapabilities {
            EngineCapabilities::none()
        }

        fn mirror_frontier(&self, _request: &FrontierRequest) -> EngineOutcome<FrontierResponse> {
            Err(Self::unsupported())
        }

        fn mirror_sync(&self, _request: &SyncRequest) -> EngineOutcome<FrontierResponse> {
            Err(Self::unsupported())
        }

        fn evaluate_question(&self, _request: &EvaluateRequest) -> EngineOutcome<EvaluateResponse> {
            Err(Self::unsupported())
        }

        fn query_members(&self, _request: &MembersRequest) -> EngineOutcome<MembersResponse> {
            Err(Self::unsupported())
        }

        fn load_dataset(
            &self,
            _request: &LoadRequest,
            _file: &Path,
        ) -> EngineOutcome<LoadResponse> {
            Err(Self::unsupported())
        }

        fn land_dataset(
            &self,
            _request: &LandRequest,
            _fetch: &mut dyn FnMut(std::fs::File) -> EngineOutcome<()>,
        ) -> EngineOutcome<LandResponse> {
            Err(Self::unsupported())
        }

        fn settle_dataset(&self, _request: &SettleRequest) -> EngineOutcome<LandResponse> {
            Err(Self::unsupported())
        }

        fn store_dataset(
            &self,
            _request: &StoreRequest,
            _send: &mut dyn FnMut(&Path, &Path) -> EngineOutcome<()>,
        ) -> EngineOutcome<LandResponse> {
            Err(Self::unsupported())
        }

        fn store_dataset_json(
            &self,
            _body: &StoreBody,
            _send: &mut dyn FnMut(&Path, &Path) -> EngineOutcome<()>,
        ) -> EngineOutcome<Box<RawValue>> {
            Err(Self::unsupported())
        }

        fn query_lineage(&self, _request: &LineageRequest) -> EngineOutcome<LineageResponse> {
            Err(Self::unsupported())
        }

        fn query_page(&self, _request: &PageRequest) -> EngineOutcome<PageResponse> {
            Err(Self::unsupported())
        }

        fn profile_objects(&self, _request: &ProfileRequest) -> EngineOutcome<ProfileResponse> {
            Err(Self::unsupported())
        }

        fn export_dataset(&self, _request: &ExportRequest) -> EngineOutcome<PathBuf> {
            Err(Self::unsupported())
        }

        fn admit_transform(
            &self,
            _plan: &TransformPlan,
        ) -> Result<Box<dyn AdmittedTransform>, CellError> {
            if self.admit_transforms {
                Ok(Box::new(PassThroughTransform))
            } else {
                Err(CellError::ComputeRefused(UNSUPPORTED_ENGINE))
            }
        }
    }

    fn order_entity_artifact()
    -> Result<(ObjectTypeDef, ArtifactEnvelope), Box<dyn std::error::Error>> {
        let object: ObjectTypeDef = serde_json::from_value(json!({
            "name": "demo.Order",
            "properties": [
                {"id": "amount", "type": {"kind": "decimal", "scale": 2, "additivity": {"kind": "additive"}}},
                {"id": "occurred_at", "type": {"kind": "timestamp", "timezone": "utc"}}
            ]
        }))?;
        let artifact: ArtifactEnvelope = serde_json::from_value(json!({
            "irVersion": "0.1",
            "artifactId": "demo.Order",
            "kind": "entity",
            "namespace": "demo",
            "name": "demo.Order",
            "version": "1.0.0",
            "lifecycle": "active",
            "scope": {"orgId": "org_demo", "environmentId": "dev"},
            "dependencies": [],
            "provenance": {"source": "authoring_sdk"},
            "payload": {"entityVersion": "2.0", "objectType": object}
        }))?;
        Ok((object, artifact))
    }

    #[test]
    fn evaluate_transform_on_admits_before_reading_snapshot_and_reports_engine_id()
    -> Result<(), Box<dyn std::error::Error>> {
        let (object, entity) = order_entity_artifact()?;
        let view: ArtifactEnvelope = serde_json::from_value(json!({
            "irVersion": "0.1",
            "artifactId": "demo.OrderScan",
            "kind": "derived",
            "namespace": "demo",
            "name": "demo.OrderScan",
            "version": "1.0.0",
            "lifecycle": "active",
            "scope": {"orgId": "org_demo", "environmentId": "dev"},
            "dependencies": [
                {"artifactId": "demo.Order", "kind": "entity", "version": "1.0.0", "required": true}
            ],
            "provenance": {"source": "authoring_sdk"},
            "payload": {
                "viewVersion": "0.1",
                "viewId": "demo.OrderScan",
                "maintenance": "recompute_on_read",
                "inputs": [
                    {"artifactId": "demo.Order", "kind": "entity", "version": "1.0.0", "alias": "o"}
                ],
                "outputSchema": {},
                "calc": {
                    "kind": "relational",
                    "output": "s",
                    "nodes": {
                        "s": {"op": "scan", "input": "o"}
                    }
                }
            }
        }))?;
        let pool = [entity, view.clone()];
        let plan = norn_checker::transform::admit(&view, &pool, &BTreeSet::new())
            .map_err(|diags| format!("admit failed: {diags:?}"))?;
        let model = Model::from_definitions(&[object], &[])?;
        let store = CellStore::open_in_memory(model)?;

        let reads = AtomicUsize::new(0);
        let refusing = StubAnalyticalEngine {
            id: AnalyticalEngineId::DataFusion,
            admit_transforms: false,
        };
        let refused =
            store.evaluate_transform_on(&refusing, &plan, MAIN_WORLD, "org_demo/dev", |_| {
                reads.fetch_add(1, Ordering::SeqCst);
                Ok(RowAccess::Read)
            });
        assert!(matches!(
            refused,
            Err(CellError::ComputeRefused(code)) if code == UNSUPPORTED_ENGINE
        ));
        assert_eq!(reads.load(Ordering::SeqCst), 0);

        let accepting = StubAnalyticalEngine {
            id: AnalyticalEngineId::DataFusion,
            admit_transforms: true,
        };
        let result =
            store.evaluate_transform_on(&accepting, &plan, MAIN_WORLD, "org_demo/dev", |_| {
                Ok(RowAccess::Read)
            })?;
        assert_eq!(result.engine, ComputeEngine::DataFusion);
        Ok(())
    }

    fn order_metric_fixture()
    -> Result<(CellStore, norn_checker::metric::compute::QuestionPlan), Box<dyn std::error::Error>>
    {
        let (object, entity) = order_entity_artifact()?;
        let catalog = EffectiveCatalog::from_artifacts(&[entity]);
        let metric: MetricDefinition = serde_json::from_value(json!({
            "name": "demo.total_amount",
            "label": "Total amount",
            "type": "simple",
            "type_params": {
                "expr": "amount",
                "metric_aggregation_params": {
                    "semantic_model": "demo.Order",
                    "agg": "sum",
                    "agg_time_dimension": "occurred_at"
                }
            },
            "x-norn": {
                "irVersion": "0.1",
                "additivity": {"kind": "additive"},
                "timeDimension": {"property": "occurred_at"},
                "owner": "finance",
                "certified": true
            }
        }))?;
        let question: QuestionIR = serde_json::from_value(json!({
            "irVersion": "0.1",
            "name": "demo.order_total",
            "objectSet": "demo.Order",
            "aggregate": [{"metric": "demo.total_amount"}],
            "window": {
                "start": "2026-01-01T00:00:00Z",
                "end": "2026-12-31T00:00:00Z"
            },
            "freshness": {"kind": "live", "maxAgeMs": 5000},
            "calendar": "iso_utc"
        }))?;
        let plan = norn_checker::metric::compute::admit(&[metric], &question, &catalog, 100)
            .map_err(|diags| format!("admit failed: {diags:?}"))?;
        let model = Model::from_definitions(&[object], &[])?;
        let store = CellStore::open_in_memory(model)?;
        Ok((store, plan))
    }

    fn empty_eval_response(provenance: Option<SnapshotProvenance>) -> EvaluateResponse {
        EvaluateResponse {
            population: 0,
            excluded: Vec::new(),
            buckets: Vec::new(),
            unlinked: 0,
            provenance,
        }
    }

    #[test]
    fn assemble_verifies_provenance_and_propagates_engine_id()
    -> Result<(), Box<dyn std::error::Error>> {
        let (store, plan) = order_metric_fixture()?;
        let tenant = TenantId::of_scope("org_demo/dev");
        let exact_context = QueryContext {
            world: MAIN_WORLD.to_owned(),
            scope: "org_demo/dev".to_owned(),
            population_pin: "test".to_owned(),
            authorization_epoch: 1,
            mapping_pin: "test".to_owned(),
            now_ms: 10_000,
            source_cursors: BTreeMap::new(),
            watermarks: BTreeMap::new(),
        };

        // Missing provenance refuses fail-closed.
        let prepared = store.analytics_prepare(&plan, exact_context.clone(), &tenant)?;
        assert!(matches!(
            store.analytics_answer(prepared, empty_eval_response(None)),
            Err(CellError::ComputeRefused(code)) if code == STALE_INPUT_CODE
        ));

        // Mismatched tenant in provenance refuses fail-closed.
        let prepared = store.analytics_prepare(&plan, exact_context.clone(), &tenant)?;
        let wrong_tenant = SnapshotProvenance::new(
            AnalyticalEngineId::DataFusion,
            TenantId::of_scope("org_other/dev"),
            ReadSnapshot::from_source(&prepared.request.source),
        );
        assert!(matches!(
            store.analytics_answer(prepared, empty_eval_response(Some(wrong_tenant))),
            Err(CellError::ComputeRefused(code)) if code == STALE_INPUT_CODE
        ));

        // Valid DataFusion provenance propagates Engine::DataFusion and passes strict law read.
        let prepared = store.analytics_prepare(&plan, exact_context, &tenant)?;
        let valid_prov = SnapshotProvenance::new(
            AnalyticalEngineId::DataFusion,
            tenant,
            ReadSnapshot::from_source(&prepared.request.source),
        );
        let result =
            store.analytics_answer_strict(prepared, empty_eval_response(Some(valid_prov)))?;
        assert_eq!(result.engine, ComputeEngine::DataFusion);
        assert_eq!(result.freshness, Freshness::Exact);
        Ok(())
    }

    #[test]
    fn assemble_strict_law_read_refuses_stale_watermark() -> Result<(), Box<dyn std::error::Error>>
    {
        let (store, plan) = order_metric_fixture()?;
        let tenant = TenantId::of_scope("org_demo/dev");
        let stale_context = QueryContext {
            world: MAIN_WORLD.to_owned(),
            scope: "org_demo/dev".to_owned(),
            population_pin: "test".to_owned(),
            authorization_epoch: 1,
            mapping_pin: "test".to_owned(),
            now_ms: 50_000,
            source_cursors: BTreeMap::from([(
                "erp".to_owned(),
                BTreeMap::from([("orders".to_owned(), "c1".to_owned())]),
            )]),
            watermarks: BTreeMap::from([(
                "erp".to_owned(),
                BTreeMap::from([(
                    "orders".to_owned(),
                    Watermark {
                        observed: "c1".to_owned(),
                        heartbeat_ms: Some(1_000),
                        complete: true,
                    },
                )]),
            )]),
        };
        let prepared = store.analytics_prepare(&plan, stale_context, &tenant)?;
        let prov = SnapshotProvenance::new(
            AnalyticalEngineId::DataFusion,
            tenant,
            ReadSnapshot::from_source(&prepared.request.source),
        );
        assert!(matches!(
            store.analytics_answer_strict(prepared, empty_eval_response(Some(prov))),
            Err(CellError::ComputeRefused(code)) if code == STALE_INPUT_CODE
        ));
        Ok(())
    }
}
