//! ANALYTICS-FUNCTION-1: statistical host code has a typed, source-pinned manifest.

use norn_ir::{
    RuntimeValue,
    canonical::CanonicalJsonError,
    function::{
        CachePolicy, DependencyClosure, DependencyEntry, DependencyKind, DeterminismProfile,
        Digest, EffectfulPolicy, FUNCTION_ABI_V1, FunctionManifest, Implementation,
        ImplementationFormat, MAX_RECEIPT_SUMMARY_BYTES, Platform, ResourceBounds, RetentionClass,
        RuntimeIdentity, SchemaRef,
    },
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Scalar inputs to the OLS/chronological forecast Function.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ForecastInput {
    /// Immutable export version.
    pub table_version: u64,
    /// Canonical digest of the exact input export.
    pub table_digest: Digest,
    /// Source population/recipe identity, distinct from the export digest.
    pub source_digest: Digest,
    /// Authenticated source cursor supplied by the exporting host.
    pub source_head: u64,
    /// Training-label cutoff in signed UTC microseconds.
    pub split_boundary: i64,
}

/// Typed summary accepted by a governed publication action.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "receipt schema carries independent source-quality and diagnostic facts"
)]
pub struct ForecastSummary {
    /// Input export version.
    pub table_version: String,
    /// Exact input export digest.
    pub table_digest: Digest,
    /// Source population/recipe digest.
    pub source_digest: Digest,
    /// Exact source cursor.
    pub source_head: u64,
    /// Chronological split, in signed UTC microseconds.
    pub split_boundary: i64,
    /// Actual implementing source tree.
    pub implementation_digest: Digest,
    /// The library's fixed forecast alpha.
    pub alpha: String,
    /// Training rows, without silently dropping labels.
    pub training_rows: u32,
    /// Held-out rows.
    pub holdout_rows: u32,
    /// Features violating their valid/recorded prediction cutoffs.
    pub late_features: u32,
    /// Training outcomes recorded at/after the split boundary.
    pub late_training_labels: u32,
    /// Whether recorded-time preconditions fail.
    pub temporal_leakage: bool,
    /// Host-authenticated population completeness.
    pub complete: bool,
    /// Host-authenticated quarantined input count.
    pub quarantine_count: u32,
    /// Whether the supplied export frontier is current for its declared population.
    pub fresh: bool,
    /// Exact alpha, split, column and freshness parameter identity.
    pub parameters_digest: Digest,
    /// Declared assumptions document identity.
    pub assumptions_digest: Digest,
    /// Declared remedy document identity.
    pub remedy_digest: Digest,
    /// Split boundary in the cell receipt clock’s milliseconds.
    pub split_at_ms: i64,
    /// Latest actual training knowledge time in cell milliseconds.
    pub latest_training_label_ms: i64,
    /// Non-forward outcomes, retained as refusal evidence.
    pub non_forward_outcome_count: u32,
    /// Whether every computed library diagnostic passed.
    pub diagnostics_passed: bool,
    /// Source cursor lag in milliseconds.
    pub frontier_age_ms: u64,
    /// UCUM outcome code; 1 denotes a dimensionless integer column.
    pub error_unit: String,
    /// Ontology-resolved outcome unit, including dimensionless/unknown absence.
    pub unit: Option<norn_ir::UnitRef>,
    /// RMSE in billionths of the declared outcome unit; absent without a fit.
    pub rmse_nanos: Option<i64>,
    /// Training-mean baseline in the same units; absent without a fit.
    pub baseline_rmse_nanos: Option<i64>,
    /// Actual library report, retaining diagnostics, plots and predictions.
    pub report: Option<RuntimeValue>,
}

/// A typed Function failure; it never grants publication authority.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ForecastFailure {
    /// Stable refusal category.
    pub code: String,
    /// Library/host detail.
    pub message: String,
}

/// Source identity of the crate and locked dependencies; no binary attestation.
/// # Errors
/// The source bundle cannot be canonically represented.
pub fn implementation_digest() -> Result<Digest, CanonicalJsonError> {
    let sources = [
        ("Cargo.toml", include_str!("../Cargo.toml")),
        ("build.rs", include_str!("../build.rs")),
        ("src/analysis.rs", include_str!("analysis.rs")),
        ("src/host.rs", include_str!("host.rs")),
        ("src/lib.rs", include_str!("lib.rs")),
        ("src/producer.rs", include_str!("producer.rs")),
        ("src/performer.rs", include_str!("performer.rs")),
        ("src/temporal.rs", include_str!("temporal.rs")),
        ("workspace/Cargo.lock", include_str!("../../../Cargo.lock")),
        ("workspace/Cargo.toml", include_str!("../../../Cargo.toml")),
        (
            "workspace/.cargo/config.toml",
            include_str!("../../../.cargo/config.toml"),
        ),
    ];
    norn_ir::canonical::domain_hash_hex(b"norn.metrics.host-source.v1\0", &sources)
        .map(|hex| Digest::from_hex(&hex))
}

fn schema<T: JsonSchema>(id: &str, version: u32) -> Result<SchemaRef, CanonicalJsonError> {
    let bytes = norn_ir::canonical::canonical_json_of(&schemars::schema_for!(T))?;
    Ok(SchemaRef {
        id: id.to_owned(),
        version,
        digest: Digest::of_json_document(bytes)?,
    })
}

/// Declare the host forecast boundary; execution needs authenticated inputs and a meter.
/// # Errors
/// A source, schema or dependency digest cannot be computed.
pub fn manifest() -> Result<FunctionManifest, CanonicalJsonError> {
    let implementation = implementation_digest()?;
    let mut closure = DependencyClosure {
        digest: implementation.clone(),
        entries: vec![
            DependencyEntry {
                name: "norn-metrics".into(),
                kind: DependencyKind::Library,
                digest: implementation.clone(),
            },
            DependencyEntry {
                name: "workspace.lock".into(),
                kind: DependencyKind::Asset,
                digest: Digest::of_json_document(norn_ir::canonical::canonical_json_of(
                    include_str!("../../../Cargo.lock"),
                )?)?,
            },
        ],
    };
    closure.digest = closure.computed_digest()?;
    Ok(FunctionManifest {
        abi: FUNCTION_ABI_V1.into(),
        function_id: "core.analytics.ols_forecast".into(),
        version: 4,
        input_schema: schema::<ForecastInput>("core.analytics.ForecastInput", 1)?,
        output_schema: schema::<ForecastSummary>("core.analytics.ForecastSummary", 4)?,
        error_schema: schema::<ForecastFailure>("core.analytics.ForecastFailure", 1)?,
        implementation: Implementation {
            format: ImplementationFormat::Host,
            digest: implementation.clone(),
            entry_point: "norn_metrics::performer::perform_forecast".into(),
            locator: Some(format!(
                "{}/{}@{}",
                env!("CARGO_PKG_NAME"),
                env!("CARGO_PKG_VERSION"),
                implementation
            )),
        },
        dependency_closure: Some(closure),
        runtime: RuntimeIdentity {
            id: env!("CARGO_PKG_NAME").into(),
            version: env!("CARGO_PKG_VERSION").into(),
            digest: norn_ir::canonical::domain_hash_hex(
                b"norn.metrics.runtime-build.v1\0",
                &(&implementation, env!("NORN_NATIVE_RUNTIME_BUILD")),
            )
            .map(|hex| Digest::from_hex(&hex))?,
        },
        platform: Platform {
            os: std::env::consts::OS.into(),
            arch: std::env::consts::ARCH.into(),
        },
        capabilities: vec![],
        determinism: DeterminismProfile::Effectful(EffectfulPolicy {
            max_attempts: 1,
            provider_idempotency: false,
        }),
        bounds: ResourceBounds {
            max_cpu_millis: Some(30_000),
            max_wall_millis: Some(30_000),
            max_memory_bytes: Some(268_435_456),
            max_input_bytes: Some(4_194_304),
            max_decoded_bytes: Some(67_108_864),
            max_output_bytes: Some(MAX_RECEIPT_SUMMARY_BYTES),
        },
        cache: CachePolicy::None,
        retention: RetentionClass::Ledger,
        steps: vec![],
    })
}

#[cfg(test)]
#[allow(clippy::expect_used, reason = "manifest conformance scenarios")]
mod tests {
    use super::*;
    use proptest::prelude::*;
    #[test]
    fn host_manifest_is_admitted_and_refuses_a_changed_source_locator() {
        let mut declaration = manifest().expect("manifest");
        assert_ne!(
            declaration.runtime.digest,
            declaration.implementation.digest
        );
        norn_checker::function::admit_function_manifest(&declaration).expect("admitted");
        declaration.implementation.locator = Some("norn-metrics/mutable".into());
        assert!(norn_checker::function::admit_function_manifest(&declaration).is_err());
    }
    proptest! {
        #![proptest_config(ProptestConfig::with_cases(32))]
        #[test]
        fn scalar_inputs_round_trip_without_losing_table_or_split_pins(version in any::<u32>(), head in any::<u32>(), boundary in any::<i64>()) {
            let digest = implementation_digest().expect("digest");
            let input = ForecastInput { table_version: u64::from(version), table_digest: digest.clone(), source_digest: digest, source_head: u64::from(head), split_boundary: boundary };
            let encoded = norn_ir::runtime::encode(&input).expect("encode");
            prop_assert_eq!(norn_ir::runtime::decode::<ForecastInput>(&encoded).expect("decode"), input);
        }
    }
}
