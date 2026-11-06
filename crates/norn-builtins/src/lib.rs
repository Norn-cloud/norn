//! Norn-owned host built-in Functions (FUNCTION-HOST-1, Core 07; #761).
//!
//! This crate is the engine's compiled-in registry. Its only entry so far is
//! [`decision`] (`norn.decision@1`). A built-in's manifest pins this crate
//! by name, version and HOST-TREE-1 source-tree digest. The digest lives in
//! `registry/builtins/norn-builtins.json`, outside the tree it digests.
//! `build.rs` recomputes the digest and fails the build when the crate's
//! sources no longer match the pin, so a stale pin never becomes a registry.
//! Only this registry hands out a [`Registered`] manifest, and the decision
//! functions accept nothing else. Authored and pack definitions still cannot
//! install `host` Functions; nothing here is reachable through definition
//! admission.
//!
//! Ring 0: pure, sans-IO and wasm32-portable.

pub mod decision;

use norn_ir::function::{
    Capability, DependencyClosure, DeterminismProfile, Digest, EffectfulPolicy, FUNCTION_ABI_V1,
    FunctionManifest, Implementation, ImplementationFormat, Platform, ResourceBounds,
    RetentionClass, RuntimeIdentity, SchemaRef,
};
use serde::Deserialize;

/// This crate's name: every built-in's `runtime.id`.
pub const CRATE: &str = env!("CARGO_PKG_NAME");
/// This crate's version: every built-in's `runtime.version`.
pub const CRATE_VERSION: &str = env!("CARGO_PKG_VERSION");
/// The reviewed pin of this crate's source tree.
pub const PIN: &str = include_str!("../../../registry/builtins/norn-builtins.json");
/// This crate's source-tree digest as `build.rs` computed it; the build
/// refused unless it equals the pin.
pub const BUILT_SOURCE_TREE: &str = env!("NORN_BUILTINS_SOURCE_TREE");

/// Why the registry cannot produce a manifest.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RegistryError {
    /// The pin file is malformed, names another crate or version, or is not
    /// the source tree this crate was built from (`builtin.pin_mismatch`).
    PinMismatch,
    /// A schema document or closure does not canonicalize.
    Canonical,
}

impl std::fmt::Display for RegistryError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::PinMismatch => "builtin.pin_mismatch",
            Self::Canonical => "builtin.canonical",
        })
    }
}

impl std::error::Error for RegistryError {}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Pin {
    #[serde(rename = "crate")]
    krate: String,
    version: String,
    source_tree: Digest,
}

/// The pinned HOST-TREE-1 digest of this crate's source tree.
///
/// # Errors
/// [`RegistryError::PinMismatch`] when the pin file is malformed, names
/// another crate or version, or differs from [`BUILT_SOURCE_TREE`].
pub fn source_tree_pin() -> Result<Digest, RegistryError> {
    let pin: Pin = serde_json::from_str(PIN).map_err(|_shape| RegistryError::PinMismatch)?;
    if pin.krate != CRATE
        || pin.version != CRATE_VERSION
        || !pin.source_tree.is_well_formed()
        || pin.source_tree.0 != BUILT_SOURCE_TREE
    {
        return Err(RegistryError::PinMismatch);
    }
    Ok(pin.source_tree)
}

fn schema(id: &str, document: &str) -> Result<SchemaRef, RegistryError> {
    Ok(SchemaRef {
        id: id.to_owned(),
        version: 1,
        digest: Digest::of_json_document(document)
            .map_err(|_canonical| RegistryError::Canonical)?,
    })
}

/// A built-in manifest from the compiled-in registry, with its content
/// digest. Only this crate constructs one, so a decision call can name no
/// other manifest: a tampered copy with a recomputed digest is not
/// `Registered`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Registered {
    manifest: FunctionManifest,
    digest: Digest,
}

impl Registered {
    fn new(manifest: FunctionManifest) -> Result<Self, RegistryError> {
        let digest = manifest
            .content_digest()
            .map_err(|_canonical| RegistryError::Canonical)?;
        Ok(Self { manifest, digest })
    }

    /// The manifest.
    #[must_use]
    pub const fn manifest(&self) -> &FunctionManifest {
        &self.manifest
    }

    /// Its content digest, the one a dispatch of it pins.
    #[must_use]
    pub const fn digest(&self) -> &Digest {
        &self.digest
    }
}

/// The registered `norn.decision@1`.
///
/// # Errors
/// A [`RegistryError`].
pub fn registered_decision() -> Result<Registered, RegistryError> {
    Registered::new(decision_manifest()?)
}

/// The `norn.decision@1` manifest. A host-format, effectful, uncacheable
/// Function that runs only behind `perform` and makes no sandbox claim. The
/// dependency closure is empty: the Norn crates it builds on ship in the same
/// engine release as `runtime`, which this manifest pins exactly.
///
/// # Errors
/// A [`RegistryError`].
fn decision_manifest() -> Result<FunctionManifest, RegistryError> {
    let tree = source_tree_pin()?;
    let closure = DependencyClosure {
        digest: Digest(String::new()),
        entries: Vec::new(),
    };
    let closure = DependencyClosure {
        digest: closure
            .computed_digest()
            .map_err(|_canonical| RegistryError::Canonical)?,
        ..closure
    };
    Ok(FunctionManifest {
        abi: FUNCTION_ABI_V1.to_owned(),
        function_id: decision::FUNCTION_ID.to_owned(),
        version: decision::VERSION,
        input_schema: schema("norn.decision.input", decision::INPUT_SCHEMA)?,
        output_schema: schema("norn.decision.output", decision::OUTPUT_SCHEMA)?,
        error_schema: schema("norn.decision.error", decision::ERROR_SCHEMA)?,
        implementation: Implementation {
            format: ImplementationFormat::Host,
            digest: tree.clone(),
            entry_point: "norn.decision@1".to_owned(),
            locator: Some(format!("{CRATE}/{CRATE_VERSION}@{}", tree.0)),
        },
        dependency_closure: Some(closure),
        runtime: RuntimeIdentity {
            id: CRATE.to_owned(),
            version: CRATE_VERSION.to_owned(),
            digest: tree,
        },
        platform: Platform {
            os: "any".to_owned(),
            arch: "any".to_owned(),
        },
        capabilities: Vec::<Capability>::new(),
        determinism: DeterminismProfile::Effectful(EffectfulPolicy {
            max_attempts: 1,
            provider_idempotency: true,
        }),
        bounds: ResourceBounds {
            max_cpu_millis: None,
            max_wall_millis: None,
            max_memory_bytes: None,
            max_input_bytes: Some(64 * 1024),
            max_decoded_bytes: None,
            max_output_bytes: Some(norn_ir::function::MAX_RECEIPT_SUMMARY_BYTES),
        },
        cache: norn_ir::function::CachePolicy::None,
        retention: RetentionClass::Ledger,
        steps: Vec::new(),
    })
}

/// The registered built-in `function_id@version`, if any. Only this
/// compiled-in table registers built-ins; a namespace or authored manifest
/// never does.
///
/// # Errors
/// A [`RegistryError`].
pub fn resolve(function_id: &str, version: u32) -> Result<Option<Registered>, RegistryError> {
    if function_id == decision::FUNCTION_ID && version == decision::VERSION {
        registered_decision().map(Some)
    } else {
        Ok(None)
    }
}
