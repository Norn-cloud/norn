//! `norn-code-ontology`: the pure `norn.code-ontology/1` contract.
//!
//! A host tool owns the code graph, file access and per-framework source
//! rules, and emits facts: which handler reads, writes, triggers on, calls,
//! schedules, guards or exposes what, each with its file, line, revision and
//! byte digest. This crate only decodes them. A resolved static edge is
//! neither behavioural parity nor authorization.
//!
//! # What `/1` fixes and what it leaves open
//!
//! - [`Relation`] and [`Confidence`] are closed: a new relation or resolution
//!   state is a new format version.
//! - Every object is closed: a new member is a new format version too.
//! - [`Diagnostic`] is open. It is a code string with named constants for the
//!   codes the current extractor emits. A reader built against an older list
//!   still decodes a document that carries a newer code.
//! - Entity and field names are the source system's own. The only extractor
//!   today, `tools/norn-port/code_ontology.py`, has rules for one framework;
//!   some diagnostic codes name what its source languages are
//!   (`python_parse_error`, `dynamic_client_handlers`). A reader for another
//!   system emits the same shape with its own codes where these do not fit.
//! - Facts say which field is touched, not what the field is. Field
//!   descriptors (native type, required, keys, code sets, relations) are not
//!   part of this format; they come from a schema reader's `SourceSchemaIR`.
//! - The source-unit inventory that facts can be joined to is not here yet; it
//!   arrives with its first producer.
//!
//! Ring 1; no IO, so it also builds for `wasm32-unknown-unknown`. See
//! `README.md` for the boundary.
#![forbid(unsafe_code)]

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// Exact source evidence: the file, where in it, and which bytes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SourceEvidence {
    /// Source file, relative to the analysed root.
    pub path: String,
    /// One-based source line.
    pub line: u64,
    /// JSON pointer inside a metadata document; empty for code.
    pub pointer: String,
    /// Revision of the analysed source, or `working-tree` when unpinned.
    pub revision: String,
    /// SHA-256 of the unchanged original file bytes.
    pub sha256: String,
}

/// Versioned host analysis format.
pub const FORMAT: &str = "norn.code-ontology/1";

/// Source-native relation; no target artifact or permission is implied.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Relation {
    /// Entity or field read.
    Reads,
    /// Entity or field write.
    Writes,
    /// Entity event, permission hook or form target.
    TriggersOn,
    /// Static call or a retained unresolved call.
    Calls,
    /// Scheduler registration.
    Schedules,
    /// Source condition enclosing a throw, without predicate translation.
    ThrowsIf,
    /// A method the source system exposes to callers.
    ExposesApi,
}

/// Static resolution state, deliberately not a probability.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Confidence {
    /// Exact target established by supplied source and metadata.
    Resolved,
    /// Typed gap requiring more evidence.
    Unresolved,
}

/// A typed gap: why a fact is unresolved, or why a file gave no facts.
///
/// Open by design. The constants are the codes the current extractor emits;
/// any other string decodes too, so a newer extractor does not break an older
/// reader. Compare with the constants; do not match exhaustively.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Diagnostic(pub String);

impl Diagnostic {
    /// A literal entity name absent from the supplied metadata.
    pub const UNKNOWN_ENTITY: &'static str = "unknown_entity";
    /// A field absent from the supplied entity metadata.
    pub const UNKNOWN_FIELD: &'static str = "unknown_field";
    /// A computed entity name that cannot be resolved statically.
    pub const DYNAMIC_ENTITY: &'static str = "dynamic_entity";
    /// A hook registration composed at run time.
    pub const DYNAMIC_HOOK: &'static str = "dynamic_hook";
    /// A callback not uniquely present in the source inventory.
    pub const UNRESOLVED_HANDLER: &'static str = "unresolved_handler";
    /// A receiver whose entity is not proved by the source.
    pub const UNTYPED_DOCUMENT: &'static str = "untyped_document";
    /// A call with no resolved target.
    pub const UNRESOLVED_CALL: &'static str = "unresolved_call";
    /// A throw with no enclosing recognized condition.
    pub const UNGUARDED_THROW: &'static str = "unguarded_throw";
    /// A computed or signature-ambiguous field list.
    pub const DYNAMIC_FIELDS: &'static str = "dynamic_fields";
    /// An expression, wildcard or alias where a literal field was needed.
    pub const FIELD_EXPRESSION: &'static str = "field_expression";
    /// A client script whose entity is computed or inconsistent.
    pub const DYNAMIC_CLIENT_TARGET: &'static str = "dynamic_client_target";
    /// A client script whose handler map is computed.
    pub const DYNAMIC_CLIENT_HANDLERS: &'static str = "dynamic_client_handlers";
    /// Metadata that is not valid JSON.
    pub const INVALID_METADATA: &'static str = "invalid_metadata";
    /// Python source that does not parse.
    pub const PYTHON_PARSE_ERROR: &'static str = "python_parse_error";
    /// A JavaScript registration with unbalanced syntax.
    pub const JAVASCRIPT_PARSE_ERROR: &'static str = "javascript_parse_error";

    /// Every code the current extractor emits.
    pub const KNOWN: &'static [&'static str] = &[
        Self::UNKNOWN_ENTITY,
        Self::UNKNOWN_FIELD,
        Self::DYNAMIC_ENTITY,
        Self::DYNAMIC_HOOK,
        Self::UNRESOLVED_HANDLER,
        Self::UNTYPED_DOCUMENT,
        Self::UNRESOLVED_CALL,
        Self::UNGUARDED_THROW,
        Self::DYNAMIC_FIELDS,
        Self::FIELD_EXPRESSION,
        Self::DYNAMIC_CLIENT_TARGET,
        Self::DYNAMIC_CLIENT_HANDLERS,
        Self::INVALID_METADATA,
        Self::PYTHON_PARSE_ERROR,
        Self::JAVASCRIPT_PARSE_ERROR,
    ];

    /// The code as text.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Whether this build names the code.
    #[must_use]
    pub fn is_known(&self) -> bool {
        Self::KNOWN.contains(&self.as_str())
    }
}

/// One evidence-bearing source relation.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Fact {
    /// Source handler identity (path plus qualified symbol), not a Norn artifact ID.
    pub handler: String,
    /// Source-native edge kind.
    pub relation: Relation,
    /// The source system's entity name, when literal or statically bound.
    pub entity: Option<String>,
    /// Native field, when literal.
    pub field: Option<String>,
    /// Method path, callback, event or call target.
    pub target: Option<String>,
    /// Exact source predicate spelling; never executed or translated here.
    pub condition: Option<String>,
    /// File, line, revision, byte digest and optional metadata pointer.
    pub source: SourceEvidence,
    /// Static resolution, not runtime confidence.
    pub confidence: Confidence,
    /// Required gap when confidence is unresolved.
    pub diagnostic: Option<Diagnostic>,
    /// One-based line inside a script embedded in a metadata document; the
    /// source pointer locates the JSON string that holds it.
    #[serde(default)]
    pub script_line: Option<u64>,
}

/// A function, controller, hook or client handler.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Handler {
    /// Stable source path plus qualified symbol.
    pub id: String,
    /// Qualified source symbol.
    pub symbol: String,
    /// Source category (not a runtime dispatch instruction).
    pub kind: String,
    /// Original source location.
    pub source: SourceEvidence,
}

/// File-level failure outside a particular edge.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceDiagnostic {
    /// Typed parse/metadata gap.
    pub code: Diagnostic,
    /// Original source location.
    pub source: SourceEvidence,
}

/// Counts from a single source app; unresolved edges remain in the denominator.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AppCoverage {
    /// All inventoried source handlers, including unreferenced functions.
    pub handlers: u64,
    /// Reads, writes and triggers, including unresolved edges.
    pub entity_edges: u64,
    /// Subset resolved to supplied entity and field metadata.
    pub resolved_entity_edges: u64,
    /// Percentage, or null when no entity edges exist.
    pub resolved_percent: Option<f64>,
    /// Gap counts across all relation kinds.
    pub unresolved: BTreeMap<Diagnostic, u64>,
}

/// Complete host result. Contains only serializable data; wasm-safe.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CodeOntology {
    /// Must equal [`FORMAT`]; callers check at admission.
    pub format: String,
    /// Pinned upstream version.
    pub substrate: String,
    /// Source inventory (not runtime installation).
    pub handlers: Vec<Handler>,
    /// Static source facts.
    pub facts: Vec<Fact>,
    /// File-level typed gaps.
    pub diagnostics: Vec<SourceDiagnostic>,
    /// Per-app coverage with an explicit denominator.
    pub coverage: BTreeMap<String, AppCoverage>,
    /// Native entity to resolved touching-handler identities.
    pub entities: BTreeMap<String, Vec<String>>,
    /// Analysis boundary, not a conformance claim.
    pub limitations: Vec<String>,
}
