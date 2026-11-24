//! D136/#1661: typed analytical engine boundary, capability declarations,
//! snapshot provenance verification, and fail-closed refusals.
//!
//! This module compiles on every target (including `wasm32` and
//! `--no-default-features`) with zero native or `DuckDB` imports. Concrete
//! backend implementations live behind their respective feature gates.

use std::fmt;
use std::path::{Path, PathBuf};

use norn_checker::metric::compute::UNSUPPORTED_ENGINE;
use norn_checker::transform::TransformPlan;
use norn_checker::transform::eval::{TransformInput, TransformOutput};
use serde::de::{self, MapAccess, Visitor};
use serde::ser::SerializeStruct;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::value::RawValue;

pub use super::store_body::StoreBody;
#[cfg(all(feature = "duckdb", feature = "native"))]
pub(crate) use super::store_body::{legacy_land_response_from_raw, legacy_store_request_from_raw};
use super::{
    EngineRefusal, EvaluateRequest, EvaluateResponse, ExportRequest, FrontierRequest,
    FrontierResponse, LandRequest, LandResponse, LineageRequest, LineageResponse, LoadRequest,
    LoadResponse, MembersRequest, MembersResponse, PageRequest, PageResponse, ProfileRequest,
    ProfileResponse, SettleRequest, Source, StoreRequest, SyncRequest, TenantId,
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
    /// All six D136/#1661 capabilities in canonical declaration order.
    pub const ALL: [Self; 6] = [
        Self::ExactBitemporal,
        Self::LiveSubSecond,
        Self::Vector,
        Self::Geo,
        Self::IcebergZeroCopy,
        Self::MarkingsEnforced,
    ];

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

    const fn wire_field(self) -> &'static str {
        match self {
            Self::ExactBitemporal => "exactBitemporal",
            Self::LiveSubSecond => "liveSubSecond",
            Self::Vector => "vector",
            Self::Geo => "geo",
            Self::IcebergZeroCopy => "icebergZeroCopy",
            Self::MarkingsEnforced => "markingsEnforced",
        }
    }

    const fn mask(self) -> u8 {
        match self {
            Self::ExactBitemporal => 1 << 0,
            Self::LiveSubSecond => 1 << 1,
            Self::Vector => 1 << 2,
            Self::Geo => 1 << 3,
            Self::IcebergZeroCopy => 1 << 4,
            Self::MarkingsEnforced => 1 << 5,
        }
    }
}

/// D136/#1661: individual capabilities an [`AnalyticalEngine`] qualifies for.
///
/// Stored as a compact bitset covering all six capabilities
/// (`exact_bitemporal`, `live_sub_second`, `vector`, `geo`,
/// `iceberg_zero_copy`, `markings_enforced`). Unqualified capabilities stay
/// `false`. Callers check required capabilities before execution and fail
/// closed on any missing capability.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct EngineCapabilities(u8);

impl EngineCapabilities {
    /// No capabilities qualified (fail-closed baseline).
    #[must_use]
    pub const fn none() -> Self {
        Self(0)
    }

    /// Capabilities demonstrated by the existing `DuckDB` engine:
    /// exact bitemporal reads over the governed mirror (`ASOF-KNOWLEDGE-1`,
    /// `METRIC-SNAPSHOT-ASOF-1`); all other capabilities stay `false` until
    /// independently qualified.
    #[must_use]
    pub const fn duckdb_default() -> Self {
        Self::none().with(Capability::ExactBitemporal, true)
    }

    /// Return a copy with `capability` set to `supported`.
    #[must_use]
    pub const fn with(self, capability: Capability, supported: bool) -> Self {
        if supported {
            Self(self.0 | capability.mask())
        } else {
            Self(self.0 & !capability.mask())
        }
    }

    /// Exact bitemporal (as-of knowledge cutoff / valid-time snapshot) reads.
    #[must_use]
    pub const fn exact_bitemporal(self) -> bool {
        self.supports(Capability::ExactBitemporal)
    }

    /// Live sub-second freshness / incremental mirror reads.
    #[must_use]
    pub const fn live_sub_second(self) -> bool {
        self.supports(Capability::LiveSubSecond)
    }

    /// Vector similarity / embedding execution.
    #[must_use]
    pub const fn vector(self) -> bool {
        self.supports(Capability::Vector)
    }

    /// Geospatial predicates and indexing.
    #[must_use]
    pub const fn geo(self) -> bool {
        self.supports(Capability::Geo)
    }

    /// Zero-copy Apache Iceberg table scanning.
    #[must_use]
    pub const fn iceberg_zero_copy(self) -> bool {
        self.supports(Capability::IcebergZeroCopy)
    }

    /// Engine-side marking enforcement for protected columns/rows.
    #[must_use]
    pub const fn markings_enforced(self) -> bool {
        self.supports(Capability::MarkingsEnforced)
    }

    /// Whether `capability` is qualified on this engine.
    #[must_use]
    pub const fn supports(self, capability: Capability) -> bool {
        (self.0 & capability.mask()) != 0
    }

    /// Fail closed if `capability` is not qualified.
    ///
    /// # Errors
    /// [`AnalyticalRefusal::UnsupportedCapability`] when `capability` is `false`.
    pub fn require_capability(self, capability: Capability) -> Result<(), AnalyticalRefusal> {
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
    pub fn require(self, required: Self) -> Result<(), AnalyticalRefusal> {
        for capability in Capability::ALL {
            if required.supports(capability) {
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
        self,
        has_marked_columns: bool,
    ) -> Result<(), AnalyticalRefusal> {
        if has_marked_columns && !self.markings_enforced() {
            return Err(AnalyticalRefusal::InsufficientProtection {
                reason: ProtectionReason::MarkingsNotEnforced,
                detail: "analytical engine does not enforce column markings".to_owned(),
            });
        }
        Ok(())
    }
}

impl Serialize for EngineCapabilities {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut state = serializer.serialize_struct("EngineCapabilities", Capability::ALL.len())?;
        for capability in Capability::ALL {
            state.serialize_field(capability.wire_field(), &self.supports(capability))?;
        }
        state.end()
    }
}

impl<'de> Deserialize<'de> for EngineCapabilities {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        const FIELDS: &[&str] = &[
            "exactBitemporal",
            "liveSubSecond",
            "vector",
            "geo",
            "icebergZeroCopy",
            "markingsEnforced",
        ];

        struct CapabilitiesVisitor;

        impl<'de> Visitor<'de> for CapabilitiesVisitor {
            type Value = EngineCapabilities;

            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("a capabilities object with all six D136 boolean flags")
            }

            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Self::Value, A::Error> {
                let mut seen: u8 = 0;
                let mut caps = EngineCapabilities::none();
                while let Some(key) = map.next_key::<String>()? {
                    let Some(cap) = Capability::ALL
                        .iter()
                        .copied()
                        .find(|cap| cap.wire_field() == key)
                    else {
                        return Err(de::Error::unknown_field(&key, FIELDS));
                    };
                    if (seen & cap.mask()) != 0 {
                        return Err(de::Error::duplicate_field(cap.wire_field()));
                    }
                    let value: bool = map.next_value()?;
                    seen |= cap.mask();
                    caps = caps.with(cap, value);
                }
                for cap in Capability::ALL {
                    if (seen & cap.mask()) == 0 {
                        return Err(de::Error::missing_field(cap.wire_field()));
                    }
                }
                Ok(caps)
            }
        }

        deserializer.deserialize_struct("EngineCapabilities", FIELDS, CapabilitiesVisitor)
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
    /// Snapshot, tenant, world, model/schema pin, commit frontier, freshness,
    /// or dataset load does not match the prepared request.
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
        /// Active mirror table schema pin generation actually selected by the engine.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        schema_pin: Option<String>,
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
    /// Build the snapshot identity from a verified [`Source`] and active mirror
    /// schema pin generation (when reading a mirror).
    #[must_use]
    pub fn from_source_with_schema(source: &Source, schema_pin: Option<String>) -> Self {
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
                schema_pin,
            },
            Source::Dataset { table, load } => Self::Dataset {
                table: table.clone(),
                load: load.clone(),
            },
        }
    }

    /// Build the snapshot identity from a verified [`Source`] without an
    /// explicit schema pin.
    #[must_use]
    pub fn from_source(source: &Source) -> Self {
        Self::from_source_with_schema(source, None)
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
    /// Snapshot metadata actually read (mirror world/frontier/scope/model/schema or dataset load).
    pub snapshot: ReadSnapshot,
    /// Freshness classification observed by the engine for this read.
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

    /// Attach an engine-observed freshness classification to this provenance envelope.
    #[must_use]
    pub const fn with_freshness(mut self, freshness: Freshness) -> Self {
        self.freshness = Some(freshness);
        self
    }

    /// Verify that the snapshot actually read matches the cell's prepared
    /// tenant, world/table/scope, model pin, active schema pin generation,
    /// and commit frontier or dataset load.
    ///
    /// # Errors
    /// [`AnalyticalRefusal::StaleRead`] on any mismatch or missing expected pin.
    pub fn verify_against(
        &self,
        expected_tenant: &TenantId,
        expected_source: &Source,
        expected_schema_pin: Option<&str>,
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
                    schema_pin,
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
                if let Some(expected_pin) = expected_schema_pin
                    && schema_pin.as_deref() != Some(expected_pin)
                {
                    return Err(AnalyticalRefusal::StaleRead {
                        detail: "analytical response schema pin does not match prepared generation"
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
    /// strict law reads require a governed mirror snapshot where BOTH the
    /// cell and the engine explicitly attest [`Freshness::Exact`] and
    /// `complete == true` (missing engine freshness `None`, non-exact
    /// freshness, incomplete answers, and loaded datasets all fail closed).
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
        if self.freshness != Some(Freshness::Exact) {
            return Err(AnalyticalRefusal::InsufficientProtection {
                reason: ProtectionReason::StrictLawRequiresExact,
                detail: "strict law reads require explicit exact engine freshness".to_owned(),
            });
        }
        Ok(())
    }
}

/// Verify that a [`ProfileResponse`] carries provenance matching `tenant`,
/// `source`, `expected_schema_pin`, and explicit `Some(Freshness::Exact)`
/// before returning `approximate: false`.
///
/// # Errors
/// [`AnalyticalRefusal::StaleRead`] when provenance is absent, mismatched,
/// or does not explicitly attest [`Freshness::Exact`].
pub fn verify_profile_provenance<'a>(
    response: &'a ProfileResponse,
    tenant: &TenantId,
    source: &Source,
    expected_schema_pin: Option<&str>,
) -> Result<&'a SnapshotProvenance, AnalyticalRefusal> {
    let provenance = response
        .provenance
        .as_ref()
        .ok_or_else(|| AnalyticalRefusal::StaleRead {
            detail: "analytical profile response omitted snapshot provenance".to_owned(),
        })?;
    provenance.verify_against(tenant, source, expected_schema_pin)?;
    if provenance.freshness != Some(Freshness::Exact) {
        return Err(AnalyticalRefusal::StaleRead {
            detail: "analytical profile response requires explicit exact freshness".to_owned(),
        });
    }
    Ok(provenance)
}

/// Required D136 capabilities for evaluating `request`.
#[must_use]
pub const fn required_evaluate_capabilities(request: &EvaluateRequest) -> EngineCapabilities {
    let mut required = EngineCapabilities::none();
    if request.query.as_of.is_some()
        || matches!(&request.source, Source::Mirror { model: Some(_), .. })
    {
        required = required.with(Capability::ExactBitemporal, true);
    }
    required
}

/// Required D136 capabilities for paging bucket members of `request`.
#[must_use]
pub const fn required_members_capabilities(request: &MembersRequest) -> EngineCapabilities {
    let mut required = EngineCapabilities::none();
    if request.query.as_of.is_some()
        || matches!(&request.source, Source::Mirror { model: Some(_), .. })
    {
        required = required.with(Capability::ExactBitemporal, true);
    }
    required
}

/// Enforce required capabilities before dispatching an [`EvaluateRequest`].
///
/// # Errors
/// `core.compute.unsupported_engine` when a required capability is missing,
/// or the engine's own refusal.
pub fn dispatch_evaluate(
    engine: &dyn AnalyticalEngine,
    request: &EvaluateRequest,
) -> EngineOutcome<EvaluateResponse> {
    engine
        .require_capabilities(required_evaluate_capabilities(request))
        .map_err(|refusal| refusal.to_engine_refusal())?;
    engine.evaluate_question(request)
}

/// Enforce required capabilities before dispatching a [`MembersRequest`].
///
/// # Errors
/// `core.compute.unsupported_engine` when a required capability is missing,
/// or the engine's own refusal.
pub fn dispatch_members(
    engine: &dyn AnalyticalEngine,
    request: &MembersRequest,
) -> EngineOutcome<MembersResponse> {
    engine
        .require_capabilities(required_members_capabilities(request))
        .map_err(|refusal| refusal.to_engine_refusal())?;
    engine.query_members(request)
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
    /// # Errors
    /// An engine refusal.
    fn mirror_frontier(&self, request: &FrontierRequest) -> EngineOutcome<FrontierResponse>;
    /// Apply one contiguous commit batch to the governed-fact mirror.
    /// # Errors
    /// `analytics.frontier_moved` or an engine refusal.
    fn mirror_sync(&self, request: &SyncRequest) -> EngineOutcome<FrontierResponse>;
    /// Evaluate a lowered question over a verified mirror frontier or dataset load.
    /// # Errors
    /// Core 03 compute refusals, stale frontier/load, or an engine refusal.
    fn evaluate_question(&self, request: &EvaluateRequest) -> EngineOutcome<EvaluateResponse>;
    /// Page one bucket's contributing members in identity order.
    /// # Errors
    /// As [`Self::evaluate_question`].
    fn query_members(&self, request: &MembersRequest) -> EngineOutcome<MembersResponse>;
    /// Load a downloaded dataset file directly into its object type's table.
    /// # Errors
    /// `bulk.digest_mismatch`, unreadable file, or an engine refusal.
    fn load_dataset(&self, request: &LoadRequest, file: &Path) -> EngineOutcome<LoadResponse>;
    /// Land a presigned dataset upload in an engine-minted attempt; host supplies authenticated download.
    /// # Errors
    /// `bulk.*` refusals or an engine refusal.
    fn land_dataset(
        &self,
        request: &LandRequest,
        fetch: &mut dyn FnMut(std::fs::File) -> EngineOutcome<()>,
    ) -> EngineOutcome<LandResponse>;
    /// Settle a page of residue rows with the cell's per-row law findings.
    /// # Errors
    /// Stale/busy/unknown attempt, invalid finding, or an engine refusal.
    fn settle_dataset(&self, request: &SettleRequest) -> EngineOutcome<LandResponse>;
    /// Store an attempt's landed files; host supplies authenticated upload.
    /// # Errors
    /// Stale/busy/unknown attempt, digest mismatch, or upload/engine refusal.
    fn store_dataset(
        &self,
        request: &StoreRequest,
        send: &mut dyn FnMut(&Path, &Path) -> EngineOutcome<()>,
    ) -> EngineOutcome<LandResponse>;
    /// Store an attempt's landed files, preserving exact raw JSON bytes on replay.
    /// # Errors
    /// As [`Self::store_dataset`], or legacy receipt mismatch.
    fn store_dataset_json(
        &self,
        body: &StoreBody,
        send: &mut dyn FnMut(&Path, &Path) -> EngineOutcome<()>,
    ) -> EngineOutcome<Box<RawValue>>;
    /// Trace one dataset source row's admission status and law findings.
    /// # Errors
    /// Cold dataset, stale load, or an engine refusal.
    fn query_lineage(&self, request: &LineageRequest) -> EngineOutcome<LineageResponse>;
    /// Return one filtered, sorted records page from a dataset or mirror source.
    /// # Errors
    /// Unknown column, unsupported comparison, or stale source.
    fn query_page(&self, request: &PageRequest) -> EngineOutcome<PageResponse>;
    /// Compute exact column profiles over a verified snapshot.
    /// # Errors
    /// Unknown column or stale source.
    fn profile_objects(&self, request: &ProfileRequest) -> EngineOutcome<ProfileResponse>;
    /// Export columns of every row of a source into a tenant scratch file.
    /// # Errors
    /// Unknown column or stale source.
    fn export_dataset(&self, request: &ExportRequest) -> EngineOutcome<PathBuf>;
    /// Admit (validate/lower) a transform plan *before* reading any cell snapshot.
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
#[path = "backend_tests.rs"]
mod tests;
