//! Native zero-`CPython` lowering of declarative Python authoring modules
//! (`tools/norn-py/norn_py/schema.py`) directly into canonical `EntityIR` v2,
//! `Markings`, and `LawIR` (`irVersion: "0.1"`).

pub mod annotation;
mod annotation_check;
mod annotation_ctors;
mod env;
mod eval;
mod eval_ctors;
mod eval_markings;
mod eval_packs;
mod lower;
mod lower_class;
mod lower_concept;
pub mod packs;
pub mod types;
mod types_spec;

use std::collections::BTreeMap;
use std::fs::{self, File};
use std::io::Read;
use std::path::Path;

use crate::canonical::{EnvelopeOptions, JsonValue};
use crate::error::{CODE_IO, CODE_UNBOUNDED, PyAuthoringError, SourceSpan};
use crate::sandbox::{MAX_SOURCE_BYTES, SandboxLimits};

pub use annotation::{MAX_ANNOTATION_DEPTH, validate_annotation_source};
pub use packs::{
    CellHostKind, DeploymentSpec, PackCategory, PackConflictPolicy, PackManifest, SecretRef,
    SettingValue, compose_packs, is_valid_endpoint_key, is_valid_worker_endpoint_url,
};
pub use types::{
    FieldDefault, FieldMeta, FnRef, InvariantClassification, KeySpec, LawModality, LawSeverity,
    LinkSpec, MarkingDecl, MarkingSeverity, MarkingsCatalog, MaskSpec, Multiplicity,
    PropertyTypeSpec, RoleKind, Rounding, ScopeRootKind, TextFormat, TimezonePolicy, UnitRef,
    UnitSystem, is_valid_object_type_name,
};

/// Canonical elaboration output matching `norn_py.sandbox.ElaborationResult`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ElaborationResult {
    /// Ordered `irVersion: "0.1"` canonical artifact envelopes.
    pub artifacts: Vec<JsonValue>,
    /// Pretty-printed canonical JSON text (`UTF-16` sorted keys, 2-space indent, trailing newline).
    pub canonical_text: String,
    /// Compact `Norn1` canonical JSON bytes (`NFC` strings/keys, code-point sorted keys).
    pub norn1_bytes: Vec<u8>,
    /// Lower-case hex `SHA-256` digest of `NORN1_DOMAIN_TAG || norn1_bytes`.
    pub norn1_sha256: String,
    /// Deterministic map of top-level Python definitions to author [`SourceSpan`]s.
    pub source_map: BTreeMap<String, SourceSpan>,
}

/// Statically validates and elaborates a Python authoring module directly into canonical
/// Norn IR with zero `CPython` execution under default [`SandboxLimits`].
///
/// # Errors
///
/// Returns [`PyAuthoringError`] on sandbox preflight refusal, schema/annotation violations,
/// callback lowering errors, or canonical serialization errors.
pub fn elaborate_source(
    source: &str,
    options: &EnvelopeOptions,
) -> Result<ElaborationResult, PyAuthoringError> {
    elaborate_source_with_limits(source, options, SandboxLimits::default())
}

/// Statically validates and elaborates a Python authoring module with explicit [`SandboxLimits`].
///
/// # Errors
///
/// Returns [`PyAuthoringError`] on budget exhaustion, sandbox refusal, schema/annotation
/// violations, or canonical serialization errors.
pub fn elaborate_source_with_limits(
    source: &str,
    options: &EnvelopeOptions,
    limits: SandboxLimits,
) -> Result<ElaborationResult, PyAuthoringError> {
    lower::elaborate_module_source(source, options, limits)
}

/// Boundedly reads a regular Python authoring file (refusing symlinks, FIFOs, and oversized
/// files) and elaborates it with zero `CPython` execution.
///
/// # Errors
///
/// Returns [`PyAuthoringError`] with code [`CODE_IO`] if `path` is a symlink, non-regular
/// file, unreadable, or invalid `UTF-8`; [`CODE_UNBOUNDED`] if file size exceeds
/// [`MAX_SOURCE_BYTES`]; or any elaboration diagnostic from [`elaborate_source`].
pub fn elaborate_file(
    path: &Path,
    options: &EnvelopeOptions,
) -> Result<ElaborationResult, PyAuthoringError> {
    let source = read_regular_file_bounded(path, &options.source_path, MAX_SOURCE_BYTES)?;
    elaborate_source(&source, options)
}

/// Reads at most `max_bytes` from a regular non-symlink file, refusing symlinks, FIFOs,
/// and files exceeding `max_bytes`.
///
/// # Errors
///
/// Returns [`PyAuthoringError`] with code [`CODE_IO`] or [`CODE_UNBOUNDED`].
pub fn read_regular_file_bounded(
    path: &Path,
    logical_path: &str,
    max_bytes: usize,
) -> Result<String, PyAuthoringError> {
    let span = SourceSpan::new(logical_path, 1, 0);
    let meta = fs::symlink_metadata(path).map_err(|err| {
        PyAuthoringError::spanned(
            CODE_IO,
            format!("cannot stat '{logical_path}': {err}"),
            span.clone(),
        )
    })?;
    let file_type = meta.file_type();
    if file_type.is_symlink() {
        return Err(PyAuthoringError::spanned(
            CODE_IO,
            format!("refusing to read symlink authoring source '{logical_path}'"),
            span,
        ));
    }
    if !file_type.is_file() {
        return Err(PyAuthoringError::spanned(
            CODE_IO,
            format!("refusing to read non-regular authoring source '{logical_path}'"),
            span,
        ));
    }

    let file = File::open(path).map_err(|err| {
        PyAuthoringError::spanned(
            CODE_IO,
            format!("cannot open '{logical_path}': {err}"),
            span.clone(),
        )
    })?;
    let limit = u64::try_from(max_bytes.saturating_add(1)).unwrap_or(u64::MAX);
    let mut reader = file.take(limit);
    let mut bytes = Vec::new();
    let _read_count = reader.read_to_end(&mut bytes).map_err(|err| {
        PyAuthoringError::spanned(
            CODE_IO,
            format!("cannot read '{logical_path}': {err}"),
            span.clone(),
        )
    })?;
    if bytes.len() > max_bytes {
        return Err(PyAuthoringError::spanned(
            CODE_UNBOUNDED,
            format!("source exceeds {max_bytes} byte limit"),
            span,
        ));
    }
    String::from_utf8(bytes).map_err(|err| {
        PyAuthoringError::spanned(
            CODE_IO,
            format!("authoring source '{logical_path}' is not valid UTF-8: {err}"),
            span,
        )
    })
}
