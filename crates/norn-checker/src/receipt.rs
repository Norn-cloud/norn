//! The checker receipt (#2851, completing #2519): what one artifact-set check
//! was run over and under, content-addressed so later tiers can pin it.
//!
//! [`check_artifact_set_with_receipt`] runs [`crate::check_artifact_set`] and
//! issues a [`CheckReceipt`] that binds:
//!
//! - the **semantic artifact set**: every candidate and installed-base
//!   artifact as `(artifactId, version, sha256)`, each definition hashed as
//!   Norn1 canonical JSON after removing editor-only layout fields, and one
//!   hash over the whole list ([`ArtifactSetRef`]);
//! - the **resource-limit profile** it was checked under
//!   ([`LimitProfileRef`], `norn.limits/1` by default), which also fixes the
//!   type environment's link depth;
//! - the **ZEN Expression type environment** derived from the set's `EntityIR`
//!   v2 definitions ([`TypeEnvRef`], `norn.expr.env/1`), and the expression
//!   checker profile beside it ([`EXPR_PROFILE`], `norn-zen/6`);
//! - the **checker** name and version, the **identifier admission profile**,
//!   and a digest of the findings ([`DiagnosticsDigest`], over
//!   [`crate::diagnostics_to_json`]).
//!
//! The receipt is Norn1 canonical JSON ([`CheckReceipt::canonical_json`]) and
//! has its own content hash ([`CheckReceipt::content_sha256`]); the format is
//! [`RECEIPT_FORMAT`].
//!
//! # The type environment
//!
//! The environment is derived only for a set with no error findings: its
//! derivation expects a checked model. It records the set-level environment,
//! with only `actor` bound. Expression sites are not yet type-checked here:
//! wiring `norn_expr::check_expression` into T2 for every site, with each
//! site's `self`/`input` binding and its own environment reference, is
//! T1-EXPR-5 (#2523). A clean set whose environment cannot be derived (for
//! example a `ref` to a registry value type the checker has only by name) is
//! not admitted: [`codes::ENVIRONMENT_UNDERIVABLE`], fail-closed.

use std::collections::{BTreeMap, BTreeSet};

use norn_canonical::{CanonicalJsonError, HashProfile, JsonValue};
use norn_expr::{EntityDefinitions, EnvOptions, TypeEnvRef, ZenTypeEnv};
use norn_ir::identifier_limits::HISTORICAL_IDENTIFIER_PROFILE;
pub use norn_ir::identifier_limits::IDENTIFIER_ADMISSION_PROFILE;
use norn_ir::{
    ArtifactEnvelope, DiagnosticRemediation, DiagnosticSeverity, FlowSourceMap, LimitProfileRef,
    PropertyTypeDef, ResourceLimitProfile,
};

use crate::entity_artifacts::{Decoded, EntityArtifactSetContext, Layer, decode_v2};
use crate::{Diagnostic, DiagnosticCode, diagnostics_to_json};

/// Diagnostic codes emitted by [`check_artifact_set_with_receipt`].
pub mod codes {
    /// The set checks clean, but its ZEN Expression type environment cannot
    /// be derived, so no environment can be bound into the receipt.
    pub const ENVIRONMENT_UNDERIVABLE: &str = "artifact_set.expression.environment_underivable";
}

/// Format tag of newly issued mapless [`CheckReceipt`]s.
///
/// Mapless receipts keep this exact format and byte representation so existing
/// stored definitions do not receive a spurious readmission.
pub const RECEIPT_FORMAT: &str = "norn-checker/receipt@2";

/// Format tag of a newly issued [`CheckReceipt`] that carries a flow source map.
pub const FLOW_SOURCE_MAP_RECEIPT_FORMAT: &str = "norn-checker/receipt@3";

/// Previous receipt format, which predates identifier admission profiles.
const LEGACY_RECEIPT_FORMAT: &str = "norn-checker/receipt@1";

/// The checker name a receipt records.
pub const CHECKER_NAME: &str = "norn-checker";

/// The checker version a receipt records (this crate's version).
pub const CHECKER_VERSION: &str = env!("CARGO_PKG_VERSION");

/// The ZEN Expression checker profile a receipt records beside the type
/// environment: `norn_expr::check::CHECKER_PROFILE`, the versioned Norn
/// profile `norn-zen/6` published in `crates/norn-expr/PROFILE.md`
/// (T1-EXPR-6, #2766).
pub const EXPR_PROFILE: &str = norn_expr::check::CHECKER_PROFILE;

/// The hash profile of every hash in a receipt.
pub const RECEIPT_HASH_PROFILE: HashProfile = HashProfile::Norn1;

// Definition receipts existed at `/3`; `/1` and `/2` predate the installed
// definition receipt path. Keep every persisted identity explicit here so a
// future checker/profile bump does not orphan dormant cells.
const HISTORIC_ADMISSION_PROFILES: &[(&str, &str)] = &[
    ("0.1.0", "norn-zen/3"),
    ("0.1.0", "norn-zen/4"),
    ("0.1.0", "norn-zen/5"),
];

/// Checker 0.1.0 receipts bound complete envelopes, including inline JDM
/// editor geometry. Keep that hash contract for verification/reconstruction.
const LEGACY_INLINE_LAYOUT_CHECKER_VERSION: &str = "0.1.0";

const IDENTIFIER_LIMIT_DIAGNOSTIC: &str = crate::codes::IDENTIFIER_TOO_LONG;

/// One checker/profile identity attested by a verified admission receipt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReceiptVersion {
    /// The persisted receipt format that was verified.
    receipt_format: String,
    /// The checker crate version recorded in the receipt.
    checker_version: String,
    /// The expression profile recorded in the receipt.
    expr_profile: String,
    identifier_profile: String,
    /// Artifact-set identity as hashed by the retained receipt version.
    artifact_set: ArtifactSetRef,
    flow_source_map: Option<FlowSourceMap>,
    /// Semantic identity used to recheck definitions after inline layout is
    /// extracted from retained source artifacts.
    semantic_artifact_set: ArtifactSetRef,
}

impl ReceiptVersion {
    /// The checker crate version recorded in the receipt.
    #[must_use]
    pub fn checker_version(&self) -> &str {
        &self.checker_version
    }

    /// The expression profile recorded in the receipt.
    #[must_use]
    pub fn expr_profile(&self) -> &str {
        &self.expr_profile
    }

    /// The persisted receipt format that was verified.
    #[must_use]
    pub fn receipt_format(&self) -> &str {
        &self.receipt_format
    }

    /// The identifier admission rules attested by the verified receipt.
    #[must_use]
    pub fn identifier_profile(&self) -> &str {
        &self.identifier_profile
    }

    /// The non-executable flow projection bound to this receipt, when present.
    #[must_use]
    pub const fn flow_source_map(&self) -> Option<&FlowSourceMap> {
        self.flow_source_map.as_ref()
    }
}

/// The identity of one artifact: its id, version, and the Norn1 content hash
/// of its semantic envelope projection.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ArtifactRef {
    /// The envelope's `artifactId`.
    pub artifact_id: String,
    /// The envelope's `version`.
    pub version: String,
    /// Lower-case hex Norn1 hash of the envelope's JSON.
    pub sha256: String,
}

/// The identity of a checked artifact set: its candidates and the installed
/// base they were checked against, each sorted by `(artifactId, version,
/// sha256)`, and one hash over both lists.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ArtifactSetRef {
    /// The candidate artifacts.
    pub artifacts: Vec<ArtifactRef>,
    /// The installed base artifacts.
    pub base: Vec<ArtifactRef>,
    /// Lower-case hex Norn1 hash of `{"artifacts": [...], "base": [...]}`.
    pub sha256: String,
}

/// A digest of a check's findings.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct DiagnosticsDigest {
    /// Every finding.
    pub count: u64,
    /// Findings of error severity.
    pub errors: u64,
    /// Lower-case hex Norn1 hash of [`crate::diagnostics_to_json`] of the
    /// findings, in reported order.
    pub sha256: String,
}

/// What one artifact-set check was run over and under. See the module docs.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct CheckReceipt {
    /// [`CHECKER_NAME`].
    pub checker: String,
    /// [`CHECKER_VERSION`].
    pub checker_version: String,
    /// The checked set.
    pub artifact_set: ArtifactSetRef,
    /// The resource-limit profile the set was checked under.
    pub limit_profile: LimitProfileRef,
    /// [`EXPR_PROFILE`].
    pub expr_profile: String,
    /// The identifier admission rules applied to this set.
    pub identifier_profile: String,
    /// The set's ZEN Expression type environment; `None` when the set has
    /// error findings.
    pub type_env: Option<TypeEnvRef>,
    /// The findings.
    pub diagnostics: DiagnosticsDigest,
    /// Whether the set has no error findings.
    pub admitted: bool,
    /// Non-executable authoring projection, present only when installed with
    /// the definitions it describes.
    pub flow_source_map: Option<FlowSourceMap>,
}

/// The findings of one artifact-set check and its receipt.
#[derive(Debug, Clone, PartialEq)]
pub struct CheckedArtifactSet {
    /// Every finding, as [`crate::check_artifact_set`] reports them, plus
    /// [`codes::ENVIRONMENT_UNDERIVABLE`].
    pub diagnostics: Vec<Diagnostic>,
    /// The receipt.
    pub receipt: CheckReceipt,
}

/// Why a receipt could not be issued.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum ReceiptError {
    /// An artifact envelope does not serialize to JSON.
    Unserializable {
        /// The artifact's id.
        artifact_id: String,
        /// The serializer's message.
        detail: String,
    },
    /// A value is outside the Norn1 canonical JSON profile.
    Canonical(CanonicalJsonError),
    /// A stored receipt is malformed, mismatched, or does not bind the
    /// supplied definitions and resource limits.
    InvalidStoredReceipt {
        /// A stable, actionable description of the failed check.
        detail: String,
    },
    /// A receipt claims a checker/profile pair that this binary does not
    /// recognize as one of its current or explicitly supported predecessors.
    UnsupportedReceiptVersion {
        /// The checker version claimed by the receipt.
        checker_version: String,
        /// The expression profile claimed by the receipt.
        expr_profile: String,
    },
    /// The canonical receipt bytes do not match the digest persisted beside
    /// them.
    ReceiptHashMismatch {
        /// The digest persisted beside the receipt.
        expected: String,
        /// The digest recomputed from the receipt body.
        actual: String,
    },
}

impl std::fmt::Display for ReceiptError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unserializable {
                artifact_id,
                detail,
            } => write!(f, "artifact {artifact_id:?} does not serialize: {detail}"),
            Self::Canonical(error) => write!(f, "not Norn1 canonical JSON: {error}"),
            Self::InvalidStoredReceipt { detail } => {
                write!(f, "invalid stored admission receipt: {detail}")
            }
            Self::UnsupportedReceiptVersion {
                checker_version,
                expr_profile,
            } => write!(
                f,
                "unsupported admission receipt checker {checker_version:?} with expression profile {expr_profile:?}"
            ),
            Self::ReceiptHashMismatch { expected, actual } => write!(
                f,
                "stored admission receipt hash mismatch: expected {expected}, got {actual}"
            ),
        }
    }
}

impl std::error::Error for ReceiptError {}

impl From<CanonicalJsonError> for ReceiptError {
    fn from(error: CanonicalJsonError) -> Self {
        Self::Canonical(error)
    }
}

/// Check `artifacts` against `context` ([`crate::check_artifact_set`]) under
/// `limits`, derive the set's type environment when the set is clean, and
/// issue the [`CheckReceipt`]. See the module docs.
///
/// # Errors
///
/// [`ReceiptError`] if an artifact envelope or the findings cannot be hashed
/// as Norn1 canonical JSON (for example a JSON value nested deeper than the
/// profile admits). The check itself never fails; its findings are in
/// [`CheckedArtifactSet::diagnostics`].
pub fn check_artifact_set_with_receipt(
    artifacts: &[ArtifactEnvelope],
    context: &EntityArtifactSetContext<'_>,
    limits: &ResourceLimitProfile,
) -> Result<CheckedArtifactSet, ReceiptError> {
    check_artifact_set_with_profile(
        artifacts,
        context,
        limits,
        IDENTIFIER_ADMISSION_PROFILE,
        None,
    )
}

/// Check a candidate artifact set and bind an optional non-executable flow
/// source map into its admission receipt.
///
/// The sidecar is checked against the candidate and installed-base artifacts;
/// it never participates in artifact execution or artifact-set resolution.
///
/// # Errors
/// As [`check_artifact_set_with_receipt`].
pub fn check_artifact_set_with_flow_source_map(
    artifacts: &[ArtifactEnvelope],
    context: &EntityArtifactSetContext<'_>,
    limits: &ResourceLimitProfile,
    flow_source_map: Option<&FlowSourceMap>,
) -> Result<CheckedArtifactSet, ReceiptError> {
    check_artifact_set_with_profile(
        artifacts,
        context,
        limits,
        IDENTIFIER_ADMISSION_PROFILE,
        flow_source_map,
    )
}

/// Re-check definitions using the identifier profile attested by a verified
/// retained-definition receipt. The receipt token is opaque and bound to the
/// semantic artifact set, so callers cannot choose a permissive profile for a
/// new install or a different retained set.
///
/// # Errors
/// As [`check_artifact_set_with_receipt`], or when `version` does not bind the
/// supplied artifacts and base.
pub fn check_artifact_set_with_verified_receipt(
    artifacts: &[ArtifactEnvelope],
    context: &EntityArtifactSetContext<'_>,
    limits: &ResourceLimitProfile,
    version: &ReceiptVersion,
) -> Result<CheckedArtifactSet, ReceiptError> {
    let artifact_set = ArtifactSetRef::of(artifacts, context.base)?;
    let retained_identity = if version.checker_version == LEGACY_INLINE_LAYOUT_CHECKER_VERSION {
        &version.semantic_artifact_set
    } else {
        &version.artifact_set
    };
    if retained_identity != &artifact_set || version.semantic_artifact_set != artifact_set {
        return Err(invalid_receipt(
            "verified receipt profile does not bind the supplied artifact set",
        ));
    }
    check_artifact_set_with_profile(
        artifacts,
        context,
        limits,
        &version.identifier_profile,
        version.flow_source_map.as_ref(),
    )
}

/// Check an install candidate, reusing an active receipt's identifier profile
/// only when the candidate and base have the same semantic artifact-set
/// identity that the verified receipt attests.
///
/// A changed semantic set is checked under the current admission profile. A
/// failure while identifying or strictly rechecking an unchanged set is
/// returned directly; verified receipt evidence is never treated as a hint.
///
/// # Errors
/// As [`check_artifact_set_with_receipt`], or when an unchanged candidate is
/// not bound by `verified_active`.
pub fn check_artifact_set_for_install(
    artifacts: &[ArtifactEnvelope],
    context: &EntityArtifactSetContext<'_>,
    limits: &ResourceLimitProfile,
    verified_active: Option<&ReceiptVersion>,
) -> Result<CheckedArtifactSet, ReceiptError> {
    check_artifact_set_for_install_with_flow_source_map(
        artifacts,
        context,
        limits,
        verified_active,
        None,
    )
}

/// Check an install with explicit flow editor evidence while retaining the
/// identifier profile only for an unchanged, verified semantic artifact set.
/// Omitting the map clears it; retained re-admission preserves its own map.
///
/// # Errors
/// As [`check_artifact_set_for_install`].
pub fn check_artifact_set_for_install_with_flow_source_map(
    artifacts: &[ArtifactEnvelope],
    context: &EntityArtifactSetContext<'_>,
    limits: &ResourceLimitProfile,
    verified_active: Option<&ReceiptVersion>,
    flow_source_map: Option<&FlowSourceMap>,
) -> Result<CheckedArtifactSet, ReceiptError> {
    if let Some(version) = verified_active {
        let artifact_set = ArtifactSetRef::of(artifacts, context.base)?;
        if artifact_set == version.semantic_artifact_set {
            let mut install_version = version.clone();
            install_version.flow_source_map = flow_source_map.cloned();
            return check_artifact_set_with_verified_receipt(
                artifacts,
                context,
                limits,
                &install_version,
            );
        }
    }
    check_artifact_set_with_flow_source_map(artifacts, context, limits, flow_source_map)
}

fn check_artifact_set_with_profile(
    artifacts: &[ArtifactEnvelope],
    context: &EntityArtifactSetContext<'_>,
    limits: &ResourceLimitProfile,
    identifier_profile: &str,
    flow_source_map: Option<&FlowSourceMap>,
) -> Result<CheckedArtifactSet, ReceiptError> {
    let artifact_set = ArtifactSetRef::of(artifacts, context.base)?;
    let limit_profile = limits.reference()?;
    let mut diagnostics = crate::check_artifact_set(artifacts, context);
    if let Some(flow_source_map) = flow_source_map {
        diagnostics.extend(crate::validate_flow_source_map(
            flow_source_map,
            artifacts,
            context.base,
        ));
    }
    if identifier_profile == HISTORICAL_IDENTIFIER_PROFILE {
        diagnostics.retain(|diagnostic| diagnostic.code.as_str() != IDENTIFIER_LIMIT_DIAGNOSTIC);
    } else if identifier_profile != IDENTIFIER_ADMISSION_PROFILE {
        return Err(invalid_receipt(format!(
            "unsupported identifier admission profile {identifier_profile:?}"
        )));
    }
    let mut type_env = None;
    if !has_errors(&diagnostics) {
        match derive_type_env(artifacts, context.base, limits) {
            Ok(env) => type_env = Some(env.reference()?),
            Err(errors) => diagnostics.extend(errors.iter().map(|error| Diagnostic {
                code: DiagnosticCode::new(codes::ENVIRONMENT_UNDERIVABLE),
                severity: DiagnosticSeverity::Error,
                message: format!(
                    "the ZEN Expression type environment of the artifact set cannot be derived: {error}"
                ),
                span: None,
                artifact_id: None,
                remediation: Some(DiagnosticRemediation::FixDefinition),
            })),
        }
    }
    let receipt = CheckReceipt {
        checker: CHECKER_NAME.to_owned(),
        checker_version: CHECKER_VERSION.to_owned(),
        artifact_set,
        limit_profile,
        expr_profile: EXPR_PROFILE.to_owned(),
        identifier_profile: identifier_profile.to_owned(),
        type_env,
        diagnostics: DiagnosticsDigest::of(&diagnostics)?,
        admitted: !has_errors(&diagnostics),
        flow_source_map: flow_source_map.cloned(),
    };
    Ok(CheckedArtifactSet {
        diagnostics,
        receipt,
    })
}

/// Recompute the Norn1 content hash of a canonical admission receipt body.
///
/// Receipt formats `norn-checker/receipt@1`, `@2`, and `@3` fix Norn1 as their
/// hash profile.
/// The input must already be canonical JSON; accepting alternate encodings
/// would make duplicate keys and non-canonical number spellings ambiguous.
///
/// # Errors
///
/// [`ReceiptError::Canonical`] if the input is invalid for Norn1, or
/// [`ReceiptError::InvalidStoredReceipt`] if it is not already canonical.
pub fn admission_receipt_sha256(stored: &str) -> Result<String, ReceiptError> {
    let canonical = RECEIPT_HASH_PROFILE.canonicalize_json(stored.as_bytes())?;
    if canonical.as_slice() != stored.as_bytes() {
        return Err(invalid_receipt(
            "receipt body must be stored as Norn1 canonical JSON",
        ));
    }
    Ok(RECEIPT_HASH_PROFILE.hash_canonical_bytes(&canonical))
}

/// Verify a stored clean admission receipt without rerunning the current
/// admission checker.
///
/// This is used when opening persisted definitions: it verifies the receipt's
/// integrity, its exact versioned checker identity, and its bindings to the
/// supplied definitions, base, limits, and derived type environment. Historic
/// versions are accepted only when explicitly listed by this crate. A
/// retained-definition caller can use the returned token to recheck current
/// semantics while preserving the attested identifier profile; new installs
/// separately use [`check_artifact_set_with_receipt`] and current limits.
///
/// # Errors
///
/// [`ReceiptError`] if the body is malformed or tampered with, names an
/// unknown checker/profile pair, is not an admitted receipt, or is bound to
/// different artifacts, limits, or type environment.
pub fn verify_admission_receipt(
    stored: &str,
    expected_sha256: &str,
    artifacts: &[ArtifactEnvelope],
    base: &[ArtifactEnvelope],
    limits: &ResourceLimitProfile,
) -> Result<ReceiptVersion, ReceiptError> {
    let value: serde_json::Value = serde_json::from_str(stored)
        .map_err(|error| invalid_receipt(format!("receipt JSON is invalid: {error}")))?;
    let parsed = parse_stored_receipt(&value)?;

    let identifier_profile = retained_identifier_profile(&parsed)?;

    if !is_lower_hex_sha256(expected_sha256) {
        return Err(invalid_receipt(
            "expected receipt hash must be 64 lowercase hexadecimal characters",
        ));
    }
    let actual_sha256 = admission_receipt_sha256(stored)?;
    if actual_sha256 != expected_sha256 {
        return Err(ReceiptError::ReceiptHashMismatch {
            expected: expected_sha256.to_owned(),
            actual: actual_sha256,
        });
    }

    if !parsed.admitted || parsed.diagnostics_errors != 0 {
        return Err(invalid_receipt(
            "receipt does not attest an admitted definition set with zero errors",
        ));
    }
    if !is_lower_hex_sha256(&parsed.diagnostics_sha256) {
        return Err(invalid_receipt(
            "diagnostics hash must be 64 lowercase hexadecimal characters",
        ));
    }

    let semantic_artifact_set = ArtifactSetRef::of(artifacts, base)?;
    let receipt_artifact_set = if parsed.checker_version == LEGACY_INLINE_LAYOUT_CHECKER_VERSION {
        ArtifactSetRef::of_legacy(artifacts, base)?
    } else {
        semantic_artifact_set.clone()
    };
    if parsed.artifact_set != receipt_artifact_set {
        return Err(invalid_receipt(
            "receipt artifact-set identity does not match the persisted definitions",
        ));
    }
    if let Some(flow_source_map) = &parsed.flow_source_map {
        let findings = crate::validate_flow_source_map(flow_source_map, artifacts, base);
        if let Some(finding) = findings.first() {
            return Err(invalid_receipt(format!(
                "stored flow source map does not match its admitted artifacts: {}",
                finding.message
            )));
        }
    }

    let expected_limit_profile = limits.reference()?;
    if !profile_ref_matches(
        &parsed.limit_profile,
        &expected_limit_profile.id,
        expected_limit_profile.version,
        &expected_limit_profile.sha256,
    ) {
        return Err(invalid_receipt(
            "receipt resource-limit profile does not match the active profile",
        ));
    }

    let derived_env = derive_type_env(artifacts, base, limits).map_err(|errors| {
        invalid_receipt(format!(
            "cannot derive the receipt's definition environment: {}",
            errors
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join("; ")
        ))
    })?;
    let expected_env = derived_env.reference()?;
    if !parsed.type_env.as_ref().is_some_and(|reference| {
        profile_ref_matches(
            reference,
            &expected_env.id,
            expected_env.version,
            &expected_env.sha256,
        )
    }) {
        return Err(invalid_receipt(
            "receipt type-environment identity does not match the persisted definitions",
        ));
    }

    Ok(ReceiptVersion {
        receipt_format: parsed.format,
        checker_version: parsed.checker_version,
        expr_profile: parsed.expr_profile,
        identifier_profile,
        artifact_set: receipt_artifact_set,
        semantic_artifact_set,
        flow_source_map: parsed.flow_source_map,
    })
}

fn retained_identifier_profile(parsed: &StoredReceipt) -> Result<String, ReceiptError> {
    if parsed.format != RECEIPT_FORMAT
        && parsed.format != FLOW_SOURCE_MAP_RECEIPT_FORMAT
        && parsed.format != LEGACY_RECEIPT_FORMAT
    {
        return Err(invalid_receipt(format!(
            "unsupported receipt format {:?}",
            parsed.format
        )));
    }
    let historical_pair = HISTORIC_ADMISSION_PROFILES.contains(&(
        parsed.checker_version.as_str(),
        parsed.expr_profile.as_str(),
    ));
    if !supported_checker_profile(&parsed.checker_version, &parsed.expr_profile)
        || (parsed.format == LEGACY_RECEIPT_FORMAT && !historical_pair)
    {
        return Err(ReceiptError::UnsupportedReceiptVersion {
            checker_version: parsed.checker_version.clone(),
            expr_profile: parsed.expr_profile.clone(),
        });
    }
    let profile = if parsed.format == LEGACY_RECEIPT_FORMAT {
        HISTORICAL_IDENTIFIER_PROFILE.to_owned()
    } else {
        parsed
            .identifier_profile
            .clone()
            .ok_or_else(|| invalid_receipt("receipt identifierProfile is missing"))?
    };
    if profile != HISTORICAL_IDENTIFIER_PROFILE && profile != IDENTIFIER_ADMISSION_PROFILE {
        return Err(invalid_receipt(format!(
            "unsupported identifier admission profile {profile:?}"
        )));
    }
    Ok(profile)
}

/// Reconstruct the canonical body of a known hash-only admission receipt.
///
/// Before receipts were retained as bodies, the cell stored only their digest.
/// For those cells the receipt can be recovered only when the digest matches
/// one of the explicitly supported receipt templates. Recovered format@1
/// receipts map to the historical identifier profile. The `/3` and `/4`
/// templates use their historic checker identity and carry forward current
/// non-error findings; current errors are never treated as evidence of
/// historic findings.
///
/// # Errors
///
/// [`ReceiptError`] if the definitions cannot supply the receipt bindings or
/// if no supported template reproduces `expected_sha256` exactly.
pub fn recover_legacy_admission_receipt(
    expected_sha256: &str,
    artifacts: &[ArtifactEnvelope],
    base: &[ArtifactEnvelope],
    limits: &ResourceLimitProfile,
) -> Result<String, ReceiptError> {
    if !is_lower_hex_sha256(expected_sha256) {
        return Err(invalid_receipt(
            "expected receipt hash must be 64 lowercase hexadecimal characters",
        ));
    }

    // The checker context's registry contains external property type names.
    let empty_registry = BTreeSet::new();
    let context = EntityArtifactSetContext {
        registry: &empty_registry,
        base,
        tenant: None,
    };
    let checked = check_artifact_set_with_receipt(artifacts, &context, limits)?;

    if checked.receipt.admitted {
        let bytes = checked.receipt.canonical_json()?;
        let recovered = std::str::from_utf8(&bytes)
            .map_err(|error| invalid_receipt(format!("recovered receipt is not UTF-8: {error}")))?
            .to_owned();
        if admission_receipt_sha256(&recovered)? == expected_sha256 {
            return Ok(recovered);
        }
    }

    let historical_check = check_artifact_set_with_profile(
        artifacts,
        &context,
        limits,
        HISTORICAL_IDENTIFIER_PROFILE,
        None,
    )?;

    let limit_profile = limits.reference()?;
    let type_env = derive_type_env(artifacts, base, limits)
        .map_err(|errors| {
            invalid_receipt(format!("cannot derive receipt environment: {errors:?}"))
        })?
        .reference()?;
    let warnings: Vec<Diagnostic> = historical_check
        .diagnostics
        .iter()
        .filter(|diagnostic| diagnostic.severity != DiagnosticSeverity::Error)
        .cloned()
        .collect();
    for &(checker_version, expr_profile) in HISTORIC_ADMISSION_PROFILES {
        let artifact_set = if checker_version == LEGACY_INLINE_LAYOUT_CHECKER_VERSION {
            ArtifactSetRef::of_legacy(artifacts, base)?
        } else {
            ArtifactSetRef::of(artifacts, base)?
        };
        for findings in [&[][..], warnings.as_slice()] {
            let historical = CheckReceipt {
                checker: CHECKER_NAME.to_owned(),
                checker_version: checker_version.to_owned(),
                artifact_set: artifact_set.clone(),
                limit_profile: limit_profile.clone(),
                expr_profile: expr_profile.to_owned(),
                identifier_profile: HISTORICAL_IDENTIFIER_PROFILE.to_owned(),
                type_env: Some(type_env.clone()),
                diagnostics: diagnostics_digest_for_version(findings, checker_version)?,
                admitted: true,
                flow_source_map: None,
            };
            let bytes = historical.legacy_canonical_json()?;
            let recovered = std::str::from_utf8(&bytes)
                .map_err(|error| {
                    invalid_receipt(format!("recovered receipt is not UTF-8: {error}"))
                })?
                .to_owned();
            if admission_receipt_sha256(&recovered)? == expected_sha256 {
                return Ok(recovered);
            }
        }
    }

    Err(ReceiptError::ReceiptHashMismatch {
        expected: expected_sha256.to_owned(),
        actual: checked.receipt.content_sha256()?,
    })
}

/// The ZEN Expression type environment of the `EntityIR` v2 definitions in
/// `base` and `artifacts` together, with the link depth of `limits`.
///
/// # Errors
///
/// Every [`norn_expr::EnvError`] of the derivation.
pub fn derive_type_env(
    artifacts: &[ArtifactEnvelope],
    base: &[ArtifactEnvelope],
    limits: &ResourceLimitProfile,
) -> Result<ZenTypeEnv, Vec<norn_expr::EnvError>> {
    let mut layer = Layer::default();
    for (index, artifact) in base.iter().chain(artifacts).enumerate() {
        if let Decoded::Root(root) = decode_v2(artifact) {
            layer.push(index, *root);
        }
    }
    let catalog: Vec<PropertyTypeDef> = layer.catalog.iter().map(|(_, def)| def.clone()).collect();
    let definitions = EntityDefinitions {
        property_types: &catalog,
        interfaces: &layer.interfaces,
        object_types: &layer.object_types,
        extensions: &layer.extensions,
    };
    ZenTypeEnv::from_artifact_set(&definitions, EnvOptions::from_limit_profile(limits))
}

fn has_errors(diagnostics: &[Diagnostic]) -> bool {
    diagnostics
        .iter()
        .any(|diagnostic| diagnostic.severity == DiagnosticSeverity::Error)
}

fn hash_serde_json(value: &serde_json::Value) -> Result<String, CanonicalJsonError> {
    // Re-canonicalized under Norn1, so key order and number spelling do not
    // matter.
    let bytes = norn_canonical::canonical_json_of(value)?;
    Ok(RECEIPT_HASH_PROFILE.hash_canonical_bytes(&bytes))
}

impl ArtifactRef {
    /// The reference of one artifact's semantic envelope projection.
    ///
    /// # Errors
    ///
    /// [`ReceiptError`] if the envelope cannot be hashed.
    pub fn of(artifact: &ArtifactEnvelope) -> Result<Self, ReceiptError> {
        let value = semantic_artifact_projection(artifact_value(artifact)?);
        Ok(Self {
            artifact_id: artifact.artifact_id.clone(),
            version: artifact.version.clone(),
            sha256: hash_serde_json(&value)?,
        })
    }

    /// The full-envelope identity used by persisted checker 0.1.0 receipts.
    fn of_legacy(artifact: &ArtifactEnvelope) -> Result<Self, ReceiptError> {
        let value = artifact_value(artifact)?;
        Ok(Self {
            artifact_id: artifact.artifact_id.clone(),
            version: artifact.version.clone(),
            sha256: hash_serde_json(&value)?,
        })
    }

    fn to_canonical_value(&self) -> JsonValue {
        object([
            ("artifactId", string(&self.artifact_id)),
            ("version", string(&self.version)),
            ("sha256", string(&self.sha256)),
        ])
    }
}

impl ArtifactSetRef {
    /// The reference of a candidate set checked against `base`.
    ///
    /// # Errors
    ///
    /// [`ReceiptError`] if an envelope cannot be hashed.
    pub fn of(
        artifacts: &[ArtifactEnvelope],
        base: &[ArtifactEnvelope],
    ) -> Result<Self, ReceiptError> {
        Self::of_with(artifacts, base, ArtifactRef::of)
    }

    /// The full-envelope identity used by persisted checker 0.1.0 receipts.
    /// Full-envelope identity for checker 0.1.0 receipt reconstruction.
    /// New admissions use [`Self::of`] to exclude editor geometry.
    ///
    /// # Errors
    /// [`ReceiptError`] when an envelope cannot be hashed.
    pub fn of_legacy(
        artifacts: &[ArtifactEnvelope],
        base: &[ArtifactEnvelope],
    ) -> Result<Self, ReceiptError> {
        Self::of_with(artifacts, base, ArtifactRef::of_legacy)
    }

    fn of_with(
        artifacts: &[ArtifactEnvelope],
        base: &[ArtifactEnvelope],
        artifact_ref: fn(&ArtifactEnvelope) -> Result<ArtifactRef, ReceiptError>,
    ) -> Result<Self, ReceiptError> {
        let refs = |envelopes: &[ArtifactEnvelope]| -> Result<Vec<ArtifactRef>, ReceiptError> {
            let mut refs = envelopes
                .iter()
                .map(artifact_ref)
                .collect::<Result<Vec<_>, _>>()?;
            refs.sort();
            Ok(refs)
        };
        let artifacts = refs(artifacts)?;
        let base = refs(base)?;
        let sha256 = RECEIPT_HASH_PROFILE.hash_canonical_bytes(&norn_canonical::serialize(
            &Self::members_value(&artifacts, &base),
        )?);
        Ok(Self {
            artifacts,
            base,
            sha256,
        })
    }

    fn members_value(artifacts: &[ArtifactRef], base: &[ArtifactRef]) -> JsonValue {
        object([
            ("artifacts", refs_value(artifacts)),
            ("base", refs_value(base)),
        ])
    }

    fn to_canonical_value(&self) -> JsonValue {
        object([
            ("artifacts", refs_value(&self.artifacts)),
            ("base", refs_value(&self.base)),
            ("sha256", string(&self.sha256)),
        ])
    }
}

fn artifact_value(artifact: &ArtifactEnvelope) -> Result<serde_json::Value, ReceiptError> {
    serde_json::to_value(artifact).map_err(|error| ReceiptError::Unserializable {
        artifact_id: artifact.artifact_id.clone(),
        detail: error.to_string(),
    })
}

/// Remove legacy inline JDM editor fields that the layout migration extracts.
/// Receipt identity therefore stays tied to executable definition semantics.
fn semantic_artifact_projection(mut value: serde_json::Value) -> serde_json::Value {
    let Some(envelope) = value.as_object_mut() else {
        return value;
    };
    if envelope.get("kind").and_then(serde_json::Value::as_str) != Some("rule") {
        return value;
    }
    let Some(payload) = envelope
        .get_mut("payload")
        .and_then(serde_json::Value::as_object_mut)
    else {
        return value;
    };
    if payload
        .get("ruleVersion")
        .and_then(serde_json::Value::as_str)
        != Some("2.0")
    {
        return value;
    }
    let Some(graph) = payload
        .get_mut("graph")
        .and_then(serde_json::Value::as_object_mut)
    else {
        return value;
    };
    graph.remove("settings");
    graph.remove("viewport");
    if let Some(nodes) = graph
        .get_mut("nodes")
        .and_then(serde_json::Value::as_array_mut)
    {
        for node in nodes {
            let Some(node) = node.as_object_mut() else {
                continue;
            };
            node.remove("position");
            node.remove("description");
            node.remove("folded");
            let Some(content) = node
                .get_mut("content")
                .and_then(serde_json::Value::as_object_mut)
            else {
                continue;
            };
            for collection in ["inputs", "outputs"] {
                if let Some(columns) = content
                    .get_mut(collection)
                    .and_then(serde_json::Value::as_array_mut)
                {
                    for column in columns {
                        if let Some(column) = column.as_object_mut() {
                            column.remove("width");
                        }
                    }
                }
            }
        }
    }
    if let Some(edges) = graph
        .get_mut("edges")
        .and_then(serde_json::Value::as_array_mut)
    {
        for edge in edges {
            if let Some(edge) = edge.as_object_mut() {
                edge.remove("name");
                edge.remove("targetHandle");
                edge.remove("type");
            }
        }
    }
    value
}

fn refs_value(refs: &[ArtifactRef]) -> JsonValue {
    JsonValue::Array(refs.iter().map(ArtifactRef::to_canonical_value).collect())
}

impl DiagnosticsDigest {
    /// The digest of `diagnostics`, in reported order.
    ///
    /// # Errors
    ///
    /// [`CanonicalJsonError`] if the findings' JSON is outside Norn1.
    pub fn of(diagnostics: &[Diagnostic]) -> Result<Self, CanonicalJsonError> {
        let errors = diagnostics
            .iter()
            .filter(|diagnostic| diagnostic.severity == DiagnosticSeverity::Error)
            .count();
        Ok(Self {
            count: u64::try_from(diagnostics.len()).unwrap_or(u64::MAX),
            errors: u64::try_from(errors).unwrap_or(u64::MAX),
            sha256: hash_serde_json(&diagnostics_to_json(diagnostics))?,
        })
    }

    fn to_canonical_value(&self) -> JsonValue {
        object([
            ("count", number(self.count)),
            ("errors", number(self.errors)),
            ("sha256", string(&self.sha256)),
        ])
    }
}

impl CheckReceipt {
    /// The receipt as a Norn1 canonical JSON value. `typeEnv` is omitted when
    /// no environment was derived.
    ///
    /// # Errors
    ///
    /// [`CanonicalJsonError`] if the typed flow source map cannot be serialized
    /// or parsed under the Norn1 profile.
    pub fn to_canonical_value(&self) -> Result<JsonValue, CanonicalJsonError> {
        let format = if self.flow_source_map.is_some() {
            FLOW_SOURCE_MAP_RECEIPT_FORMAT
        } else {
            RECEIPT_FORMAT
        };
        self.to_canonical_value_with_format(format, true)
    }

    fn to_canonical_value_with_format(
        &self,
        format: &str,
        include_identifier_profile: bool,
    ) -> Result<JsonValue, CanonicalJsonError> {
        let reference = |id: &str, version: u32, sha256: &str| {
            object([
                ("id", string(id)),
                ("version", number(u64::from(version))),
                ("sha256", string(sha256)),
            ])
        };
        let mut members: BTreeMap<String, JsonValue> = [
            ("format", string(format)),
            (
                "checker",
                object([
                    ("name", string(&self.checker)),
                    ("version", string(&self.checker_version)),
                ]),
            ),
            ("artifactSet", self.artifact_set.to_canonical_value()),
            (
                "limitProfile",
                reference(
                    &self.limit_profile.id,
                    self.limit_profile.version,
                    &self.limit_profile.sha256,
                ),
            ),
            ("exprProfile", string(&self.expr_profile)),
            ("diagnostics", self.diagnostics.to_canonical_value()),
            ("admitted", JsonValue::Bool(self.admitted)),
        ]
        .into_iter()
        .map(|(key, value)| (key.to_owned(), value))
        .collect();
        if include_identifier_profile {
            members.insert(
                "identifierProfile".to_owned(),
                string(&self.identifier_profile),
            );
        }
        if let Some(env) = &self.type_env {
            members.insert(
                "typeEnv".to_owned(),
                reference(&env.id, env.version, &env.sha256),
            );
        }
        if format == FLOW_SOURCE_MAP_RECEIPT_FORMAT
            && let Some(flow_source_map) = &self.flow_source_map
        {
            let bytes = norn_canonical::canonical_json_of(flow_source_map)?;
            members.insert("flowSourceMap".to_owned(), norn_canonical::parse(bytes)?);
        }
        Ok(JsonValue::Object(members))
    }

    /// Norn1 canonical JSON bytes of the receipt.
    ///
    /// # Errors
    ///
    /// [`CanonicalJsonError`] if `norn-canonical` rejects the value (not
    /// expected: the receipt is strings, integers, and booleans).
    pub fn canonical_json(&self) -> Result<Vec<u8>, CanonicalJsonError> {
        norn_canonical::serialize(&self.to_canonical_value()?)
    }

    fn legacy_canonical_json(&self) -> Result<Vec<u8>, CanonicalJsonError> {
        norn_canonical::serialize(
            &self.to_canonical_value_with_format(LEGACY_RECEIPT_FORMAT, false)?,
        )
    }

    /// Lower-case hex content hash of the receipt under
    /// [`RECEIPT_HASH_PROFILE`].
    ///
    /// # Errors
    ///
    /// As [`CheckReceipt::canonical_json`].
    pub fn content_sha256(&self) -> Result<String, CanonicalJsonError> {
        Ok(RECEIPT_HASH_PROFILE.hash_canonical_bytes(&self.canonical_json()?))
    }
}

#[derive(Debug)]
struct StoredReceipt {
    format: String,
    checker_version: String,
    expr_profile: String,
    identifier_profile: Option<String>,
    artifact_set: ArtifactSetRef,
    flow_source_map: Option<FlowSourceMap>,
    limit_profile: StoredProfileRef,
    type_env: Option<StoredProfileRef>,
    diagnostics_errors: u64,
    diagnostics_sha256: String,
    admitted: bool,
}

#[derive(Debug, PartialEq, Eq)]
struct StoredProfileRef {
    id: String,
    version: u32,
    sha256: String,
}

fn parse_stored_receipt(value: &serde_json::Value) -> Result<StoredReceipt, ReceiptError> {
    let object = exact_object(
        value,
        "receipt",
        &[
            "format",
            "checker",
            "artifactSet",
            "limitProfile",
            "exprProfile",
            "diagnostics",
            "admitted",
        ],
        &["typeEnv", "identifierProfile", "flowSourceMap"],
    )?;
    let format = required_string(object, "format", "receipt")?;
    let identifier_profile = object
        .get("identifierProfile")
        .map(|value| {
            value
                .as_str()
                .map(str::to_owned)
                .ok_or_else(|| invalid_receipt("receipt.identifierProfile must be a string"))
        })
        .transpose()?;
    let flow_source_map = object
        .get("flowSourceMap")
        .map(|value| {
            serde_json::from_value(value.clone()).map_err(|error| {
                invalid_receipt(format!("receipt.flowSourceMap is invalid: {error}"))
            })
        })
        .transpose()?;
    validate_stored_receipt_format(
        &format,
        identifier_profile.as_deref(),
        flow_source_map.as_ref(),
    )?;
    let checker = exact_object(
        required_value(object, "checker", "receipt")?,
        "receipt.checker",
        &["name", "version"],
        &[],
    )?;
    let checker_name = required_string(checker, "name", "receipt.checker")?;
    if checker_name != CHECKER_NAME {
        return Err(invalid_receipt(format!(
            "receipt checker name must be {CHECKER_NAME:?}"
        )));
    }

    let artifact_set = parse_artifact_set(required_value(object, "artifactSet", "receipt")?)?;
    let limit_profile = parse_profile_ref(
        required_value(object, "limitProfile", "receipt")?,
        "receipt.limitProfile",
    )?;
    let type_env = object
        .get("typeEnv")
        .map(|value| parse_profile_ref(value, "receipt.typeEnv"))
        .transpose()?;
    let diagnostics = exact_object(
        required_value(object, "diagnostics", "receipt")?,
        "receipt.diagnostics",
        &["count", "errors", "sha256"],
        &[],
    )?;
    let diagnostics_count = required_u64(diagnostics, "count", "receipt.diagnostics")?;
    let diagnostics_errors = required_u64(diagnostics, "errors", "receipt.diagnostics")?;
    if diagnostics_errors > diagnostics_count {
        return Err(invalid_receipt(
            "receipt diagnostics error count exceeds its total count",
        ));
    }
    let diagnostics_sha256 = required_string(diagnostics, "sha256", "receipt.diagnostics")?;
    if !is_lower_hex_sha256(&diagnostics_sha256) {
        return Err(invalid_receipt(
            "diagnostics hash must be 64 lowercase hexadecimal characters",
        ));
    }

    Ok(StoredReceipt {
        format,
        checker_version: required_string(checker, "version", "receipt.checker")?,
        expr_profile: required_string(object, "exprProfile", "receipt")?,
        identifier_profile,
        artifact_set,
        flow_source_map,
        limit_profile,
        type_env,
        diagnostics_errors,
        diagnostics_sha256,
        admitted: required_bool(object, "admitted", "receipt")?,
    })
}

fn validate_stored_receipt_format(
    format: &str,
    identifier_profile: Option<&str>,
    flow_source_map: Option<&FlowSourceMap>,
) -> Result<(), ReceiptError> {
    match format {
        LEGACY_RECEIPT_FORMAT if identifier_profile.is_some() || flow_source_map.is_some() => {
            return Err(invalid_receipt(
                "receipt@1 cannot contain identifierProfile or flowSourceMap",
            ));
        }
        RECEIPT_FORMAT if identifier_profile.is_none() || flow_source_map.is_some() => {
            return Err(invalid_receipt(
                "receipt@2 requires identifierProfile and must not contain flowSourceMap",
            ));
        }
        FLOW_SOURCE_MAP_RECEIPT_FORMAT
            if identifier_profile.is_none() || flow_source_map.is_none() =>
        {
            return Err(invalid_receipt(
                "receipt@3 requires identifierProfile and flowSourceMap",
            ));
        }
        _ => {}
    }
    Ok(())
}

fn parse_artifact_set(value: &serde_json::Value) -> Result<ArtifactSetRef, ReceiptError> {
    let object = exact_object(
        value,
        "receipt.artifactSet",
        &["artifacts", "base", "sha256"],
        &[],
    )?;
    let artifacts = parse_artifact_refs(
        required_value(object, "artifacts", "receipt.artifactSet")?,
        "receipt.artifactSet.artifacts",
    )?;
    let base = parse_artifact_refs(
        required_value(object, "base", "receipt.artifactSet")?,
        "receipt.artifactSet.base",
    )?;
    let sha256 = required_string(object, "sha256", "receipt.artifactSet")?;
    if !is_lower_hex_sha256(&sha256) {
        return Err(invalid_receipt(
            "artifact-set hash must be 64 lowercase hexadecimal characters",
        ));
    }
    Ok(ArtifactSetRef {
        artifacts,
        base,
        sha256,
    })
}

fn parse_artifact_refs(
    value: &serde_json::Value,
    what: &str,
) -> Result<Vec<ArtifactRef>, ReceiptError> {
    let values = value
        .as_array()
        .ok_or_else(|| invalid_receipt(format!("{what} must be an array")))?;
    values
        .iter()
        .enumerate()
        .map(|(index, value)| {
            let path = format!("{what}[{index}]");
            let object = exact_object(value, &path, &["artifactId", "version", "sha256"], &[])?;
            let artifact_id = required_string(object, "artifactId", &path)?;
            let version = required_string(object, "version", &path)?;
            let sha256 = required_string(object, "sha256", &path)?;
            if !is_lower_hex_sha256(&sha256) {
                return Err(invalid_receipt(format!(
                    "{path}.sha256 must be 64 lowercase hexadecimal characters"
                )));
            }
            Ok(ArtifactRef {
                artifact_id,
                version,
                sha256,
            })
        })
        .collect()
}

fn parse_profile_ref(
    value: &serde_json::Value,
    what: &str,
) -> Result<StoredProfileRef, ReceiptError> {
    let object = exact_object(value, what, &["id", "version", "sha256"], &[])?;
    let id = required_string(object, "id", what)?;
    let version = required_u64(object, "version", what)?;
    let version = u32::try_from(version).map_err(|error| {
        invalid_receipt(format!("{what}.version is outside the u32 range: {error}"))
    })?;
    let sha256 = required_string(object, "sha256", what)?;
    if !is_lower_hex_sha256(&sha256) {
        return Err(invalid_receipt(format!(
            "{what}.sha256 must be 64 lowercase hexadecimal characters"
        )));
    }
    Ok(StoredProfileRef {
        id,
        version,
        sha256,
    })
}

fn exact_object<'a>(
    value: &'a serde_json::Value,
    what: &str,
    required: &[&str],
    optional: &[&str],
) -> Result<&'a serde_json::Map<String, serde_json::Value>, ReceiptError> {
    let object = value
        .as_object()
        .ok_or_else(|| invalid_receipt(format!("{what} must be an object")))?;
    for field in required {
        if !object.contains_key(*field) {
            return Err(invalid_receipt(format!("{what}.{field} is missing")));
        }
    }
    if let Some(field) = object
        .keys()
        .find(|field| !required.contains(&field.as_str()) && !optional.contains(&field.as_str()))
    {
        return Err(invalid_receipt(format!("{what}.{field} is unknown")));
    }
    Ok(object)
}

fn required_string(
    object: &serde_json::Map<String, serde_json::Value>,
    field: &str,
    what: &str,
) -> Result<String, ReceiptError> {
    required_value(object, field, what)?
        .as_str()
        .map(str::to_owned)
        .ok_or_else(|| invalid_receipt(format!("{what}.{field} must be a string")))
}

fn required_u64(
    object: &serde_json::Map<String, serde_json::Value>,
    field: &str,
    what: &str,
) -> Result<u64, ReceiptError> {
    required_value(object, field, what)?
        .as_u64()
        .ok_or_else(|| invalid_receipt(format!("{what}.{field} must be a nonnegative integer")))
}

fn required_bool(
    object: &serde_json::Map<String, serde_json::Value>,
    field: &str,
    what: &str,
) -> Result<bool, ReceiptError> {
    required_value(object, field, what)?
        .as_bool()
        .ok_or_else(|| invalid_receipt(format!("{what}.{field} must be a boolean")))
}

fn required_value<'a>(
    object: &'a serde_json::Map<String, serde_json::Value>,
    field: &str,
    what: &str,
) -> Result<&'a serde_json::Value, ReceiptError> {
    object
        .get(field)
        .ok_or_else(|| invalid_receipt(format!("{what}.{field} is missing")))
}

fn supported_checker_profile(checker_version: &str, expr_profile: &str) -> bool {
    (checker_version == CHECKER_VERSION && expr_profile == EXPR_PROFILE)
        || HISTORIC_ADMISSION_PROFILES.contains(&(checker_version, expr_profile))
}

fn profile_ref_matches(
    actual: &StoredProfileRef,
    expected_id: &str,
    expected_version: u32,
    expected_sha256: &str,
) -> bool {
    actual.id == expected_id
        && actual.version == expected_version
        && actual.sha256 == expected_sha256
}

fn diagnostics_digest_for_version(
    diagnostics: &[Diagnostic],
    checker_version: &str,
) -> Result<DiagnosticsDigest, CanonicalJsonError> {
    let errors = diagnostics
        .iter()
        .filter(|diagnostic| diagnostic.severity == DiagnosticSeverity::Error)
        .count();
    let mut json = diagnostics_to_json(diagnostics);
    if let Some(object) = json.as_object_mut() {
        object.insert(
            "checkerVersion".to_owned(),
            serde_json::Value::String(checker_version.to_owned()),
        );
    }
    Ok(DiagnosticsDigest {
        count: u64::try_from(diagnostics.len()).unwrap_or(u64::MAX),
        errors: u64::try_from(errors).unwrap_or(u64::MAX),
        sha256: hash_serde_json(&json)?,
    })
}

fn is_lower_hex_sha256(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn invalid_receipt(detail: impl Into<String>) -> ReceiptError {
    ReceiptError::InvalidStoredReceipt {
        detail: detail.into(),
    }
}

fn string(value: &str) -> JsonValue {
    JsonValue::String(value.to_owned())
}

fn number(value: u64) -> JsonValue {
    JsonValue::Number(value.to_string())
}

fn object<const N: usize>(entries: [(&str, JsonValue); N]) -> JsonValue {
    JsonValue::Object(
        entries
            .into_iter()
            .map(|(key, value)| (key.to_owned(), value))
            .collect(),
    )
}

#[cfg(test)]
#[allow(
    clippy::expect_used,
    clippy::panic,
    reason = "unit-test fixture and hashing assertions"
)]
mod tests {
    use std::collections::BTreeSet;

    use norn_ir::{ArtifactEnvelope, ResourceLimitProfile, RuntimeValue};

    use super::{
        ArtifactRef, ArtifactSetRef, CHECKER_NAME, CheckReceipt, HISTORIC_ADMISSION_PROFILES,
        HISTORICAL_IDENTIFIER_PROFILE, LEGACY_INLINE_LAYOUT_CHECKER_VERSION, LEGACY_RECEIPT_FORMAT,
        derive_type_env, diagnostics_digest_for_version, recover_legacy_admission_receipt,
        verify_admission_receipt,
    };
    use crate::EntityArtifactSetContext;

    #[test]
    fn historical_warning_digest_binds_its_checker_version() {
        let artifact =
            serde_json::from_str(include_str!("../../../fixtures/ir/retail-sku.entity.json"))
                .expect("v1 entity fixture");
        let warning = crate::entity_v1_deprecation(&artifact).expect("registered warning");
        let diagnostics = [crate::Diagnostic::from_legacy(warning)];
        let historical = diagnostics_digest_for_version(&diagnostics, "0.1.0")
            .expect("historical findings digest");
        let next = diagnostics_digest_for_version(&diagnostics, "0.2.0")
            .expect("next checker findings digest");
        assert_eq!(historical.count, 1);
        assert_eq!(historical.errors, 0);
        assert_eq!(next.count, historical.count);
        assert_eq!(next.errors, historical.errors);
        assert_ne!(next.sha256, historical.sha256);
    }

    #[test]
    fn geometry_edits_preserve_semantic_receipt_identity_but_legacy_hash_is_full() {
        let original_raw = RuntimeValue::from_json_text(include_str!(
            "../../../fixtures/ir/retail-reorder-routing.rule.json"
        ))
        .expect("rule fixture JSON");
        let original: ArtifactEnvelope =
            norn_ir::runtime::decode(&original_raw).expect("typed rule fixture");

        let mut moved = original_raw.clone();
        let graph = moved
            .as_object_mut()
            .and_then(|root| root.get_mut("payload"))
            .and_then(RuntimeValue::as_object_mut)
            .and_then(|payload| payload.get_mut("graph"))
            .and_then(RuntimeValue::as_object_mut)
            .expect("graph object");
        graph.insert(
            "settings".to_owned(),
            RuntimeValue::from_json_text(r#"{"grid":{"step":0.1234567890123456789012345678}}"#)
                .expect("exact settings JSON"),
        );
        let RuntimeValue::Array(nodes) = graph.get_mut("nodes").expect("nodes array") else {
            panic!("JDM nodes should be an array");
        };
        nodes[0].as_object_mut().expect("first node").insert(
            "position".to_owned(),
            RuntimeValue::from_json_text(r#"{"x":777.125,"y":900}"#).expect("moved position"),
        );
        nodes[0].as_object_mut().expect("first node").insert(
            "description".to_owned(),
            RuntimeValue::String("editor note".to_owned()),
        );
        let sidecar =
            crate::layout::extract_inline_layout(&mut moved).expect("move extracts as layout");
        assert!(
            sidecar
                .artifacts
                .contains_key("retail.reorder_routing.rule")
        );
        let moved: ArtifactEnvelope = norn_ir::runtime::decode(&moved).expect("normalized rule");

        assert_eq!(ArtifactRef::of(&original), ArtifactRef::of(&moved));
        assert_ne!(
            ArtifactRef::of_legacy(&original),
            ArtifactRef::of_legacy(&moved)
        );

        let mut semantic_edit = original_raw;
        let graph = semantic_edit
            .as_object_mut()
            .and_then(|root| root.get_mut("payload"))
            .and_then(RuntimeValue::as_object_mut)
            .and_then(|payload| payload.get_mut("graph"))
            .and_then(RuntimeValue::as_object_mut)
            .expect("graph object");
        let RuntimeValue::Array(nodes) = graph.get_mut("nodes").expect("nodes array") else {
            panic!("JDM nodes should be an array");
        };
        nodes[0].as_object_mut().expect("first node").insert(
            "name".to_owned(),
            RuntimeValue::String("changed semantic name".to_owned()),
        );
        let semantic_edit: ArtifactEnvelope =
            norn_ir::runtime::decode(&semantic_edit).expect("semantic edit");
        assert_ne!(ArtifactRef::of(&original), ArtifactRef::of(&semantic_edit));
    }

    #[test]
    fn old_receipt_hashes_recover_and_recheck_after_inline_layout_extraction() {
        let raw = RuntimeValue::from_json_text(include_str!(
            "../../../fixtures/ir/retail-reorder-routing.rule.json"
        ))
        .expect("rule fixture JSON");
        let original: ArtifactEnvelope = norn_ir::runtime::decode(&raw).expect("typed fixture");
        let artifacts = [original];
        let base: [ArtifactEnvelope; 0] = [];
        let limits = ResourceLimitProfile::norn_limits_v1();
        let registry = BTreeSet::new();
        let context = EntityArtifactSetContext {
            registry: &registry,
            base: &base,
            tenant: None,
        };
        let artifact_set = ArtifactSetRef::of_legacy(&artifacts, &base)
            .expect("historical full-envelope identity");
        let type_env = derive_type_env(&artifacts, &base, &limits)
            .expect("definition environment")
            .reference()
            .expect("environment reference");
        let historical = CheckReceipt {
            checker: CHECKER_NAME.to_owned(),
            checker_version: LEGACY_INLINE_LAYOUT_CHECKER_VERSION.to_owned(),
            artifact_set,
            limit_profile: limits.reference().expect("limit reference"),
            expr_profile: HISTORIC_ADMISSION_PROFILES[2].1.to_owned(),
            identifier_profile: HISTORICAL_IDENTIFIER_PROFILE.to_owned(),
            type_env: Some(type_env),
            diagnostics: diagnostics_digest_for_version(&[], LEGACY_INLINE_LAYOUT_CHECKER_VERSION)
                .expect("empty diagnostics hash"),
            admitted: true,
            flow_source_map: None,
        };
        let legacy_body = String::from_utf8(
            historical
                .legacy_canonical_json()
                .expect("legacy canonical receipt"),
        )
        .expect("receipt is UTF-8");
        let expected_sha256 =
            super::admission_receipt_sha256(&legacy_body).expect("legacy receipt hash");

        let recovered =
            recover_legacy_admission_receipt(&expected_sha256, &artifacts, &base, &limits)
                .expect("known historical template");
        assert_eq!(recovered, legacy_body);
        let verified =
            verify_admission_receipt(&recovered, &expected_sha256, &artifacts, &base, &limits)
                .expect("old receipt verifies against original inline source");

        let mut normalized_raw = raw;
        crate::layout::extract_inline_layout(&mut normalized_raw).expect("inline layout migration");
        let normalized: ArtifactEnvelope =
            norn_ir::runtime::decode(&normalized_raw).expect("normalized artifact");
        let rechecked = super::check_artifact_set_with_verified_receipt(
            &[normalized],
            &context,
            &limits,
            &verified,
        )
        .expect("semantic receipt identity remains bound after migration");
        assert!(rechecked.receipt.admitted);
        assert_eq!(
            verified.checker_version(),
            LEGACY_INLINE_LAYOUT_CHECKER_VERSION
        );
        assert_eq!(verified.receipt_format(), LEGACY_RECEIPT_FORMAT);
    }
}
