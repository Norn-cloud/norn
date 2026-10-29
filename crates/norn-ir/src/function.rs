//! The Function contract (T1-FN-1, #2776; ruling 2026-09-27 compute boundary
//! §2): the **manifest**, the **invocation command**, and the **result
//! receipt**, plus the relevant-state dependency token the command pins (§4).
//!
//! A Function is an immutable, versioned implementation manifest invoked only
//! through `perform`. It is not a `CellHost` verb, a durable process, or a
//! custom decision node. These records are declarative data: this module
//! defines them and their canonical identities; admission (the semantic rules)
//! is `norn-checker`'s `function` module, and result reconciliation is
//! `norn-kernel` (T2-RES-1).
//!
//! # Identities
//!
//! Every identity is a domain-separated SHA-256 over Norn1 canonical JSON
//! ([`norn_canonical::domain_hash_hex`]); no second canonicalizer exists. The
//! records' numbers are integers and exact [`RuntimeValue`] decimals, so an
//! identity never passes through `f64` and is the same natively and on
//! `wasm32`. Identities are written `sha256:<64 lower-case hex>` ([`Digest`]),
//! the OCI digest form, so a code or input identity is visibly a content
//! digest and never a tag, URI, or object key.
//!
//! | Identity | Covers | Domain |
//! |---|---|---|
//! | [`FunctionManifest::content_digest`] | the whole manifest | [`MANIFEST_DOMAIN`] |
//! | [`FunctionManifest::execution_profile_digest`] | runtime, platform, determinism, capabilities, bounds | [`EXECUTION_PROFILE_DOMAIN`] |
//! | [`DependencyClosure::computed_digest`] | the closure entries | [`DEPENDENCY_CLOSURE_DOMAIN`] |
//! | [`FunctionInvocation::content_digest`] | the whole command (the dispatch binding) | [`INVOCATION_DOMAIN`] |
//! | [`FunctionInvocation::input_identity`] | scalar inputs + immutable blob digests | [`INPUT_IDENTITY_DOMAIN`] |
//! | [`RelevantStateToken::digest`] | the dependency token | [`DEPENDENCY_TOKEN_DOMAIN`] |
//! | [`FunctionInvocation::action_key`] | the action-cache key (§2) | [`ACTION_KEY_DOMAIN`] |
//! | [`output_manifest_digest`] | a success's outputs + summary | [`OUTPUT_MANIFEST_DOMAIN`] |
//! | [`FunctionReceipt::result_identity`] | the deterministic result (no metrics) | [`RESULT_IDENTITY_DOMAIN`] |
//! | [`FunctionReceipt::content_digest`] | the whole receipt, as ledgered | [`RECEIPT_DOMAIN`] |
//!
//! Command identity stays separate from the action key: two commands may
//! share one computation (one action key) but each has its own invocation
//! digest, receipt, authorization, and transition.
//!
//! # Projections
//!
//! WIT, OCI annotations, or a component manifest may *project* a
//! [`FunctionManifest`]; they never define its types.

use std::collections::BTreeMap;

use norn_canonical::CanonicalJsonError;
use schemars::{JsonSchema, Schema, schema_for};
use serde::{Deserialize, Serialize};

use crate::runtime::{RuntimeObject, RuntimeValue};

/// The Function ABI/contract version every v1 manifest declares.
pub const FUNCTION_ABI_V1: &str = "norn.function-abi/1";

/// Domain separator of [`FunctionManifest::content_digest`].
pub const MANIFEST_DOMAIN: &[u8] = b"norn.function/manifest/v1\0";
/// Domain separator of [`FunctionManifest::execution_profile_digest`].
pub const EXECUTION_PROFILE_DOMAIN: &[u8] = b"norn.function/execution-profile/v1\0";
/// Domain separator of [`DependencyClosure::computed_digest`].
pub const DEPENDENCY_CLOSURE_DOMAIN: &[u8] = b"norn.function/dependency-closure/v1\0";
/// Domain separator of [`FunctionInvocation::content_digest`].
pub const INVOCATION_DOMAIN: &[u8] = b"norn.function/invocation/v1\0";
/// Domain separator of [`FunctionInvocation::input_identity`].
pub const INPUT_IDENTITY_DOMAIN: &[u8] = b"norn.function/inputs/v1\0";
/// Domain separator of [`RelevantStateToken::digest`].
pub const DEPENDENCY_TOKEN_DOMAIN: &[u8] = b"norn.function/dependency-token/v1\0";
/// Domain separator of [`FunctionInvocation::action_key`] (the review's
/// `norn.function-action/v1`).
pub const ACTION_KEY_DOMAIN: &[u8] = b"norn.function-action/v1\0";
/// Domain separator of [`output_manifest_digest`].
pub const OUTPUT_MANIFEST_DOMAIN: &[u8] = b"norn.function/output-manifest/v1\0";
/// Domain separator of [`FunctionReceipt::result_identity`].
pub const RESULT_IDENTITY_DOMAIN: &[u8] = b"norn.function/result/v1\0";
/// Domain separator of [`FunctionReceipt::content_digest`].
pub const RECEIPT_DOMAIN: &[u8] = b"norn.function/receipt/v1\0";

/// The pattern every [`Digest`] matches.
pub const DIGEST_PATTERN: &str = "^sha256:[0-9a-f]{64}$";

/// The most canonical bytes a receipt's bounded summary may hold: the §1
/// decision/result envelope (32 KiB). Bytes beyond it live in object storage
/// and are named by output digests.
pub const MAX_RECEIPT_SUMMARY_BYTES: u64 = 32 * 1024;

/// The WASM imports a qualified capability-free deterministic module may
/// declare in v1: none. Deterministic ABI glue is admitted by name in a later
/// profile version, never by omission.
pub const ADMITTED_WASM_IMPORTS: &[&str] = &[];

/// A content digest, `sha256:<64 lower-case hex>`.
///
/// Decoding accepts any string so that admission can report a mutable
/// identity (a tag, URI, or object key) as a finding;
/// [`Digest::is_well_formed`] is the rule.
#[derive(
    Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
#[serde(transparent)]
pub struct Digest(#[schemars(regex(pattern = DIGEST_PATTERN))] pub String);

impl Digest {
    /// The digest of lower-case hex `hex`.
    #[must_use]
    pub fn from_hex(hex: &str) -> Self {
        Self(format!("sha256:{hex}"))
    }

    /// Whether this is `sha256:` followed by exactly 64 lower-case hex digits.
    #[must_use]
    pub fn is_well_formed(&self) -> bool {
        self.0.strip_prefix("sha256:").is_some_and(|hex| {
            hex.len() == 64
                && hex
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        })
    }

    /// The content digest of a JSON document (a schema, for example): the
    /// Norn1 hash of its canonical bytes, `sha256(norn1 tag || canonical)`.
    ///
    /// # Errors
    ///
    /// The text is not a Norn1 JSON document.
    pub fn of_json_document(text: impl AsRef<[u8]>) -> Result<Self, CanonicalJsonError> {
        norn_canonical::HashProfile::Norn1
            .hash_json(text)
            .map(|hex| Self::from_hex(&hex))
    }

    /// The digest text.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for Digest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

fn digest_of<T: Serialize + ?Sized>(
    domain: &[u8],
    value: &T,
) -> Result<Digest, CanonicalJsonError> {
    norn_canonical::domain_hash_hex(domain, value).map(|hex| Digest::from_hex(&hex))
}

// ---------------------------------------------------------------------------
// The manifest
// ---------------------------------------------------------------------------

/// The identity of one schema (input, output, error, or a pipeline step's).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SchemaRef {
    /// The schema's id.
    pub id: String,
    /// Its version.
    pub version: u32,
    /// The content digest of the schema document
    /// ([`Digest::of_json_document`]).
    pub digest: Digest,
}

/// How the implementation's bytes are packaged.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ImplementationFormat {
    /// A core WebAssembly module.
    WasmModule,
    /// An OCI image.
    OciImage,
}

/// The implementation: what runs, by content.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Implementation {
    /// The packaging.
    pub format: ImplementationFormat,
    /// The content digest of the module bytes or the OCI image manifest.
    pub digest: Digest,
    /// The entry point (a WASM export or the image's command).
    pub entry_point: String,
    /// Where to fetch it, informational only. If present it must pin
    /// `digest` (`registry/name@sha256:…`); a tag or bare URI is never an
    /// identity.
    pub locator: Option<String>,
}

/// What a dependency-closure entry is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum DependencyKind {
    /// A linked or loaded library.
    Library,
    /// A static asset (dictionary, lookup table).
    Asset,
    /// Model weights.
    Model,
    /// A font.
    Font,
    /// A codec.
    Codec,
    /// An OCI base image layer set.
    BaseImage,
}

/// One entry of the transitive dependency/asset closure.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DependencyEntry {
    /// The dependency's name.
    pub name: String,
    /// What it is.
    pub kind: DependencyKind,
    /// Its content digest.
    pub digest: Digest,
}

/// The transitive dependency/asset closure and its digest.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DependencyClosure {
    /// The declared closure digest; admission requires it to equal
    /// [`DependencyClosure::computed_digest`].
    pub digest: Digest,
    /// Every entry, in declaration order (the digest is order-sensitive;
    /// admission requires entries sorted by name).
    pub entries: Vec<DependencyEntry>,
}

impl DependencyClosure {
    /// The digest of the entries.
    ///
    /// # Errors
    ///
    /// A canonicalization failure.
    pub fn computed_digest(&self) -> Result<Digest, CanonicalJsonError> {
        digest_of(DEPENDENCY_CLOSURE_DOMAIN, &self.entries)
    }
}

/// The runtime the implementation is executed by.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RuntimeIdentity {
    /// The runtime (`wasmtime`, `docker`, `cloudflare-containers`).
    pub id: String,
    /// Its version.
    pub version: String,
    /// The digest of its build and engine configuration.
    pub digest: Digest,
}

/// The supported platform. Part of the execution identity unless
/// cross-platform equivalence is qualified.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Platform {
    /// The operating system (`none` for a capability-free WASM module).
    pub os: String,
    /// The architecture (`wasm32`, `amd64`, `arm64`).
    pub arch: String,
}

/// One scoped capability an effectful producer is granted.
#[derive(
    Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Capability {
    /// The capability (`net.egress`, `model.invoke`).
    pub name: String,
    /// Its scope (a host, a model id, a provider account alias). Never a
    /// secret: credentials are bound by the `perform` supervisor.
    pub scope: String,
}

/// How a qualified WASM module's float results are made deterministic.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum FloatPolicy {
    /// NaN results canonicalized (the Wasmtime foundation's setting).
    CanonicalNan,
}

/// Evidence for the qualified capability-free deterministic WASM profile
/// (§2 profile 1). Every member is required evidence; a `null` is a missing
/// claim and is refused at admission.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WasmQualification {
    /// The module's whole import/link closure (not merely "no WASI"); every
    /// entry must be in [`ADMITTED_WASM_IMPORTS`].
    pub import_closure: Option<Vec<String>>,
    /// A fresh or reset instance per invocation.
    pub fresh_instance: Option<bool>,
    /// Threads or shared mutable state enabled (must be `false`).
    pub threads: Option<bool>,
    /// SIMD enabled (permitted only with a defined float policy).
    pub simd: Option<bool>,
    /// The float/NaN policy.
    pub float_policy: Option<FloatPolicy>,
    /// The deterministic fuel budget.
    pub fuel: Option<u64>,
    /// The maximum guest stack, in bytes.
    pub max_stack_bytes: Option<u64>,
    /// The maximum module size, in bytes.
    pub max_module_bytes: Option<u64>,
}

/// One golden run of a hermetic container transform.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct GoldenRun {
    /// Where it ran (pool and node class).
    pub placement: String,
    /// The output digest it produced.
    pub output_digest: Digest,
}

/// Evidence for a hermetic container transform (§2 profile 2): *declared*
/// deterministic, with reproducibility evidence and enforced isolation. This
/// is evidence, not proof.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct HermeticEvidence {
    /// Guest network enabled (must be `false`).
    pub network: Option<bool>,
    /// Guest credentials present (must be `false`).
    pub credentials: Option<bool>,
    /// Isolated scratch storage.
    pub isolated_scratch: Option<bool>,
    /// The pinned locale.
    pub locale: Option<String>,
    /// The pinned timezone.
    pub timezone: Option<String>,
    /// Repeated and cross-placement golden runs.
    pub golden_runs: Vec<GoldenRun>,
    /// Whether the profile is qualified (cacheable). A container starts
    /// unqualified and non-cacheable.
    pub qualified: bool,
}

/// The policy of an effectful or unqualified producer (§2 profile 3).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EffectfulPolicy {
    /// The bounded attempt ceiling.
    pub max_attempts: u32,
    /// Whether the provider deduplicates on the command's idempotency key.
    pub provider_idempotency: bool,
}

/// The determinism profile, one of the three ruled profiles. Capability is
/// the separate [`FunctionManifest::capabilities`] axis.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum DeterminismProfile {
    /// Qualified capability-free deterministic WASM.
    QualifiedWasm(WasmQualification),
    /// A hermetic container transform.
    HermeticContainer(HermeticEvidence),
    /// An effectful or unqualified producer.
    Effectful(EffectfulPolicy),
}

impl DeterminismProfile {
    /// The profile's wire name.
    #[must_use]
    pub const fn name(&self) -> &'static str {
        match self {
            Self::QualifiedWasm(_) => "qualified_wasm",
            Self::HermeticContainer(_) => "hermetic_container",
            Self::Effectful(_) => "effectful",
        }
    }
}

/// Resource bounds. Every bound is required and positive; a `null` is an
/// unbounded resource and is refused at admission.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ResourceBounds {
    /// CPU time, in milliseconds.
    pub max_cpu_millis: Option<u64>,
    /// Wall time, in milliseconds.
    pub max_wall_millis: Option<u64>,
    /// Memory, in bytes.
    pub max_memory_bytes: Option<u64>,
    /// Staged input bytes (canonical scalar inputs plus blobs).
    pub max_input_bytes: Option<u64>,
    /// Decoded working size (for example a decoded image), in bytes.
    pub max_decoded_bytes: Option<u64>,
    /// Output bytes (every output object together).
    pub max_output_bytes: Option<u64>,
}

/// Whether successful results may be reused by content.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum CachePolicy {
    /// No content-based reuse.
    None,
    /// The per-tenant action cache (qualified deterministic profiles only).
    ActionCache,
}

/// How long outputs are kept. Outputs referenced by an authoritative receipt
/// are always kept under ledger retention, independently of this class and of
/// cache eviction.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum RetentionClass {
    /// Ledger retention.
    Ledger,
    /// Unreferenced outputs may be evicted.
    Transient,
}

/// One step of a packaged pipeline (§3): recorded identities only; the
/// pipeline runs as one Function and one pool job.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PipelineStep {
    /// The step id.
    pub id: String,
    /// Its input schema.
    pub input_schema: SchemaRef,
    /// Its output schema.
    pub output_schema: SchemaRef,
}

/// The immutable Function manifest (§2, record 1).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct FunctionManifest {
    /// The contract/ABI version ([`FUNCTION_ABI_V1`]).
    pub abi: String,
    /// The Function's id.
    pub function_id: String,
    /// Its version. Any change is a new version with a new digest.
    pub version: u32,
    /// The input schema identity.
    pub input_schema: SchemaRef,
    /// The output schema identity.
    pub output_schema: SchemaRef,
    /// The typed error schema identity.
    pub error_schema: SchemaRef,
    /// The implementation.
    pub implementation: Implementation,
    /// The transitive dependency/asset closure. Required: an absent closure
    /// is refused at admission.
    pub dependency_closure: Option<DependencyClosure>,
    /// The runtime.
    pub runtime: RuntimeIdentity,
    /// The platform.
    pub platform: Platform,
    /// The capability profile; empty for the deterministic profiles.
    pub capabilities: Vec<Capability>,
    /// The determinism profile.
    pub determinism: DeterminismProfile,
    /// Resource bounds.
    pub bounds: ResourceBounds,
    /// Cache policy.
    pub cache: CachePolicy,
    /// Retention class.
    pub retention: RetentionClass,
    /// Pipeline step identities (§3); empty for a single step.
    pub steps: Vec<PipelineStep>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ExecutionProfileIdentity<'a> {
    runtime: &'a RuntimeIdentity,
    platform: &'a Platform,
    determinism: &'a DeterminismProfile,
    capabilities: &'a [Capability],
    bounds: &'a ResourceBounds,
}

impl FunctionManifest {
    /// The manifest digest: the identity invocations name.
    ///
    /// # Errors
    ///
    /// A canonicalization failure.
    pub fn content_digest(&self) -> Result<Digest, CanonicalJsonError> {
        digest_of(MANIFEST_DOMAIN, self)
    }

    /// The execution-profile digest: runtime, platform, determinism profile,
    /// capabilities, and bounds.
    ///
    /// # Errors
    ///
    /// A canonicalization failure.
    pub fn execution_profile_digest(&self) -> Result<Digest, CanonicalJsonError> {
        digest_of(
            EXECUTION_PROFILE_DOMAIN,
            &ExecutionProfileIdentity {
                runtime: &self.runtime,
                platform: &self.platform,
                determinism: &self.determinism,
                capabilities: &self.capabilities,
                bounds: &self.bounds,
            },
        )
    }

    /// Whether the profile may use the action cache: qualified WASM, or a
    /// hermetic container whose evidence is marked qualified.
    #[must_use]
    pub const fn is_cacheable_profile(&self) -> bool {
        match &self.determinism {
            DeterminismProfile::QualifiedWasm(_) => true,
            DeterminismProfile::HermeticContainer(evidence) => evidence.qualified,
            DeterminismProfile::Effectful(_) => false,
        }
    }
}

// ---------------------------------------------------------------------------
// The relevant-state dependency token (§4)
// ---------------------------------------------------------------------------

/// One object pinned at an exact version (`0` pins absence: never written).
#[derive(
    Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct FactPin {
    /// The object type.
    pub object_type: String,
    /// The identity key.
    pub key: String,
    /// The version read at dispatch.
    pub version: u64,
}

/// One member of a predicate's result set, with the version read.
#[derive(
    Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct MemberPin {
    /// The identity key.
    pub key: String,
    /// The version read at dispatch.
    pub version: u64,
}

/// A versioned predicate token: the exact ordered membership of a declared
/// predicate over one object type at dispatch. It covers membership, order,
/// and absence (an empty member list pins "no match"), so an inserted or
/// deleted matching object invalidates it; row versions alone miss phantoms.
#[derive(
    Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PredicatePin {
    /// The object type the predicate ranges over.
    pub object_type: String,
    /// The declared predicate's id (a slice declaration or query name).
    pub predicate: String,
    /// The members in the predicate's declared order.
    pub members: Vec<MemberPin>,
}

/// A conservative relevant-collection epoch: any insert, delete, or update in
/// the collection invalidates it.
#[derive(
    Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CollectionPin {
    /// The object type.
    pub object_type: String,
    /// The epoch read at dispatch.
    pub epoch: u64,
}

/// The relevant-state dependency token a command pins at dispatch (§4).
///
/// Its [`RelevantStateToken::digest`] is the kernel's opaque
/// `DependencyToken`; the structured token travels with the command so the
/// cell can re-evaluate it at completion.
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RelevantStateToken {
    /// Objects pinned at exact versions.
    pub facts: Vec<FactPin>,
    /// Predicates pinned at exact membership.
    pub predicates: Vec<PredicatePin>,
    /// Collections pinned at an epoch.
    pub collections: Vec<CollectionPin>,
}

impl RelevantStateToken {
    /// Whether the token pins nothing.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.facts.is_empty() && self.predicates.is_empty() && self.collections.is_empty()
    }

    /// The token digest.
    ///
    /// # Errors
    ///
    /// A canonicalization failure.
    pub fn digest(&self) -> Result<Digest, CanonicalJsonError> {
        digest_of(DEPENDENCY_TOKEN_DOMAIN, self)
    }
}

// ---------------------------------------------------------------------------
// The invocation command
// ---------------------------------------------------------------------------

/// Whose authority the command runs under.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum AuthorityKind {
    /// A human or agent actor.
    Actor,
    /// A service principal.
    Service,
}

/// The authority a command runs under.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Authority {
    /// The kind.
    pub kind: AuthorityKind,
    /// The principal id.
    pub id: String,
}

/// The `Run` and request generation the command belongs to.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RunRef {
    /// The `Run` id.
    pub id: String,
    /// The request generation; a re-issue or cancellation bumps it.
    pub generation: u64,
}

/// One immutable input blob, by content.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BlobRef {
    /// The content digest (never a URL or object key).
    pub digest: Digest,
    /// The object version, informational.
    pub version: Option<String>,
    /// The size in bytes.
    pub size_bytes: u64,
}

/// Where an accepted result is written, and the owner version expected.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Destination {
    /// The object type.
    pub object_type: String,
    /// The identity key.
    pub key: String,
    /// The owner version expected at acceptance.
    pub expected_owner_version: u64,
}

/// Retry and cost ceilings.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RetryCeiling {
    /// The attempt ceiling (at least 1).
    pub max_attempts: u32,
    /// The cost ceiling, in the pool's cost units.
    pub max_cost_units: u64,
}

/// The cancellation/fencing identity.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Fencing {
    /// The fencing token a late attempt is refused by.
    pub token: String,
    /// The cancellation generation at dispatch.
    pub cancellation_generation: u64,
}

/// The command's declared freshness policy (§4); the kernel's
/// `FreshnessPolicy` carries the pinned values.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum FreshnessMode {
    /// Exact relevant dependencies (state-sensitive decisions).
    RelevantState,
    /// Immutable-input identity plus destination CAS (transforms).
    DestinationCas,
    /// Append-only observation (stays historically useful).
    AppendOnly,
}

/// The invocation command (§2, record 2).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct FunctionInvocation {
    /// The tenant.
    pub tenant: String,
    /// The world (only `main` performs).
    pub world: String,
    /// The authority.
    pub authority: Authority,
    /// The command id.
    pub command_id: String,
    /// The `Run` and generation.
    pub run: RunRef,
    /// The manifest digest.
    pub manifest: Digest,
    /// Canonical scalar inputs (null, boolean, exact number, string).
    pub inputs: BTreeMap<String, RuntimeValue>,
    /// Immutable input blobs by content digest.
    pub blobs: BTreeMap<String, BlobRef>,
    /// The ledger position of the commit that dispatched the command.
    pub provenance_commit: u64,
    /// The relevant-state dependency token.
    pub dependency: RelevantStateToken,
    /// The declared freshness policy.
    pub freshness: FreshnessMode,
    /// The destination and expected owner version, if the result is written.
    pub destination: Option<Destination>,
    /// The deadline, in logical milliseconds.
    pub deadline_millis: u64,
    /// Retry and cost ceilings.
    pub retry: RetryCeiling,
    /// Cancellation/fencing identity.
    pub fencing: Fencing,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct InputIdentityDoc<'a> {
    inputs: &'a BTreeMap<String, RuntimeValue>,
    blobs: BTreeMap<&'a str, &'a Digest>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ActionKeyDoc<'a> {
    tenant: &'a str,
    function_manifest: &'a Digest,
    execution_profile: &'a Digest,
    input_values: &'a BTreeMap<String, RuntimeValue>,
    input_digests: BTreeMap<&'a str, &'a Digest>,
}

impl FunctionInvocation {
    /// The command digest: the identity a receipt binds to (its dispatch).
    ///
    /// # Errors
    ///
    /// A canonicalization failure.
    pub fn content_digest(&self) -> Result<Digest, CanonicalJsonError> {
        digest_of(INVOCATION_DOMAIN, self)
    }

    fn blob_digests(&self) -> BTreeMap<&str, &Digest> {
        self.blobs
            .iter()
            .map(|(name, blob)| (name.as_str(), &blob.digest))
            .collect()
    }

    /// The immutable-input identity: scalar inputs and blob content digests
    /// (not blob versions, which are informational).
    ///
    /// # Errors
    ///
    /// A canonicalization failure.
    pub fn input_identity(&self) -> Result<Digest, CanonicalJsonError> {
        digest_of(
            INPUT_IDENTITY_DOMAIN,
            &InputIdentityDoc {
                inputs: &self.inputs,
                blobs: self.blob_digests(),
            },
        )
    }

    /// The action-cache key (§2):
    /// `H(norn.function-action/v1, {tenant, manifest, execution profile,
    /// input values, immutable input digests})`. The default cache scope is
    /// the tenant; cross-tenant reuse is a separate decision.
    ///
    /// `manifest` must be the manifest this command names; the caller
    /// (admission) checks that.
    ///
    /// # Errors
    ///
    /// A canonicalization failure.
    pub fn action_key(&self, manifest: &FunctionManifest) -> Result<Digest, CanonicalJsonError> {
        let execution_profile = manifest.execution_profile_digest()?;
        digest_of(
            ACTION_KEY_DOMAIN,
            &ActionKeyDoc {
                tenant: &self.tenant,
                function_manifest: &self.manifest,
                execution_profile: &execution_profile,
                input_values: &self.inputs,
                input_digests: self.blob_digests(),
            },
        )
    }
}

// ---------------------------------------------------------------------------
// The result receipt
// ---------------------------------------------------------------------------

/// One output object, by content.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct OutputRef {
    /// The output's name.
    pub name: String,
    /// Its content digest.
    pub digest: Digest,
    /// Its size in bytes.
    pub size_bytes: u64,
}

/// A successful result.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SuccessOutcome {
    /// The output-manifest digest ([`output_manifest_digest`] of `outputs`
    /// and `summary`).
    pub output_manifest: Digest,
    /// The output objects.
    pub outputs: Vec<OutputRef>,
    /// The bounded summary the cell consumes (at most
    /// [`MAX_RECEIPT_SUMMARY_BYTES`] canonical bytes), typed by the output
    /// schema.
    pub summary: RuntimeObject,
}

/// A typed failure, by the error schema.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct FailureOutcome {
    /// The error code.
    pub code: String,
    /// Bounded detail, typed by the error schema.
    pub detail: RuntimeObject,
}

/// Which bound stopped the attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ExhaustedResource {
    /// Fuel or CPU.
    Cpu,
    /// Memory.
    Memory,
    /// Output size.
    Output,
    /// Wall time.
    Wall,
}

/// The typed outcome of one attempt.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum FunctionOutcome {
    /// The Function returned a value.
    Success(SuccessOutcome),
    /// The Function returned a typed error.
    Failure(FailureOutcome),
    /// A resource bound stopped it. Never a cache entry.
    ResourceExhausted {
        /// The bound.
        resource: ExhaustedResource,
    },
    /// The deadline passed. Never a cache entry.
    DeadlineExceeded {},
    /// The command was cancelled.
    Cancelled {},
}

/// Where and how the attempt ran.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ExecutionProvenance {
    /// The executor identity (pool, node) the receipt is authenticated as.
    pub executor: String,
    /// The placement.
    pub placement: String,
    /// The runtime digest the attempt actually ran.
    pub runtime: Digest,
}

/// Measured resource use. Metrics, never part of the result identity.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ResourceUse {
    /// CPU time, in milliseconds.
    pub cpu_millis: u64,
    /// Wall time, in milliseconds.
    pub wall_millis: u64,
    /// Peak memory, in bytes.
    pub peak_memory_bytes: u64,
    /// Output bytes written.
    pub output_bytes: u64,
    /// Fuel consumed, for a WASM attempt.
    pub fuel: Option<u64>,
}

/// Whether the result came from the action cache.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum CacheOrigin {
    /// Computed by this attempt.
    Miss {},
    /// Served from the tenant's action cache.
    Hit {
        /// The action key that hit.
        action_key: Digest,
    },
}

/// The result receipt (§2, record 3).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct FunctionReceipt {
    /// The command id.
    pub command_id: String,
    /// The `Run` and generation it was dispatched under.
    pub run: RunRef,
    /// The attempt number (1-based).
    pub attempt: u32,
    /// The invocation digest it is bound to.
    pub dispatch: Digest,
    /// The manifest digest it executed.
    pub manifest: Digest,
    /// The execution-profile digest it executed under.
    pub execution_profile: Digest,
    /// The immutable-input identity it consumed.
    pub input_identity: Digest,
    /// The dependency token digest of its dispatch.
    pub dependency_token: Digest,
    /// The typed outcome.
    pub outcome: FunctionOutcome,
    /// Execution provenance.
    pub provenance: ExecutionProvenance,
    /// Resource use.
    pub resources: ResourceUse,
    /// Cache-hit origin.
    pub cache: CacheOrigin,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct OutputManifestDoc<'a> {
    outputs: &'a [OutputRef],
    summary: &'a RuntimeObject,
}

/// The output-manifest digest of a success: its outputs and summary.
///
/// # Errors
///
/// A canonicalization failure.
pub fn output_manifest_digest(
    outputs: &[OutputRef],
    summary: &RuntimeObject,
) -> Result<Digest, CanonicalJsonError> {
    digest_of(
        OUTPUT_MANIFEST_DOMAIN,
        &OutputManifestDoc { outputs, summary },
    )
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ResultIdentityDoc<'a> {
    command_id: &'a str,
    run: &'a RunRef,
    manifest: &'a Digest,
    execution_profile: &'a Digest,
    input_identity: &'a Digest,
    dependency_token: &'a Digest,
    outcome: &'a FunctionOutcome,
}

impl FunctionReceipt {
    /// The whole receipt's digest, as ledgered.
    ///
    /// # Errors
    ///
    /// A canonicalization failure.
    pub fn content_digest(&self) -> Result<Digest, CanonicalJsonError> {
        digest_of(RECEIPT_DOMAIN, self)
    }

    /// The deterministic result identity: command, generation, manifest,
    /// profile, input and dependency identities, and outcome. Attempt,
    /// provenance, resource use, and cache origin (metrics) are excluded, so
    /// two attempts that computed the same result have the same identity.
    ///
    /// # Errors
    ///
    /// A canonicalization failure.
    pub fn result_identity(&self) -> Result<Digest, CanonicalJsonError> {
        digest_of(
            RESULT_IDENTITY_DOMAIN,
            &ResultIdentityDoc {
                command_id: &self.command_id,
                run: &self.run,
                manifest: &self.manifest,
                execution_profile: &self.execution_profile,
                input_identity: &self.input_identity,
                dependency_token: &self.dependency_token,
                outcome: &self.outcome,
            },
        )
    }
}

/// The published JSON Schema of [`FunctionManifest`].
#[must_use]
pub fn function_manifest_schema() -> Schema {
    schema_for!(FunctionManifest)
}

/// The published JSON Schema of [`FunctionInvocation`].
#[must_use]
pub fn function_invocation_schema() -> Schema {
    schema_for!(FunctionInvocation)
}

/// The published JSON Schema of [`FunctionReceipt`].
#[must_use]
pub fn function_receipt_schema() -> Schema {
    schema_for!(FunctionReceipt)
}
