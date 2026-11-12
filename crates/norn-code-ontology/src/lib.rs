//! `norn-code-ontology`: the pure `norn.code-ontology/1` contract shared by
//! the port and connect front ends.
//!
//! A host tool (`tools/norn-port/code_ontology.py`) owns the code graph, file
//! access and per-framework source rules. This crate only decodes the facts it
//! emits and joins them to the source-unit inventory. A resolved static edge
//! is neither behavioural parity nor authorization.
//!
//! Ring 1; no IO, so it also builds for `wasm32-unknown-unknown`. See
//! `README.md` for the boundary.
#![forbid(unsafe_code)]

pub mod unit;

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

pub use unit::{CodeUnit, SourceEvidence};

/// Versioned host analysis format.
pub const FORMAT: &str = "norn.code-ontology/1";

/// Source-native relation; no target artifact or permission is implied.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Relation {
    /// Document or field read.
    Reads,
    /// Document or field write.
    Writes,
    /// Document event, permission hook or form target.
    TriggersOn,
    /// Static call or a retained unresolved call.
    Calls,
    /// Scheduler registration.
    Schedules,
    /// Source condition enclosing a throw, without predicate translation.
    ThrowsIf,
    /// Whitelisted method path.
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

/// Conservative source-analysis gaps.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Diagnostic {
    /// Literal `DocType` absent from supplied metadata.
    UnknownDoctype,
    /// Field absent from supplied `DocType` metadata.
    UnknownField,
    /// Computed `DocType` cannot be resolved.
    DynamicDoctype,
    /// Runtime hook composition.
    DynamicHook,
    /// Callback not uniquely present in source inventory.
    UnresolvedHandler,
    /// Receiver does not have a proved source `DocType`.
    UntypedDocument,
    /// Source call has no resolved target.
    UnresolvedCall,
    /// Throw has no enclosing recognized condition.
    UnguardedThrow,
    /// Computed or signature-ambiguous field list.
    DynamicFields,
    /// Expression, wildcard or alias rather than a literal field.
    FieldExpression,
    /// Computed or inconsistent Client Script `DocType`.
    DynamicClientTarget,
    /// Computed Client Script handler map.
    DynamicClientHandlers,
    /// Invalid metadata JSON.
    InvalidMetadata,
    /// Source Python could not be parsed.
    PythonParseError,
    /// Client registration has unbalanced syntax.
    JavascriptParseError,
}

/// One evidence-bearing source relation.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Fact {
    /// Existing source unit identity, not a Norn artifact ID.
    pub handler: String,
    /// Source-native edge kind.
    pub relation: Relation,
    /// Native `DocType`, when literal or statically bound.
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
    /// One-based embedded Client Script line; source pointer locates its JSON string.
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
    /// Subset resolved to supplied `DocType`/field metadata.
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

impl CodeOntology {
    /// Join to the syntactic source inventory without replacing it with a second
    /// graph-driven inventory. Clauses retain their enclosing handler's edges;
    /// this does not establish enclosing-handler parity.
    pub fn facts_for<'a>(&'a self, unit: &'a CodeUnit) -> impl Iterator<Item = &'a Fact> {
        let path = unit
            .source
            .path
            .strip_prefix("code/")
            .unwrap_or(&unit.source.path);
        let symbol = unit.symbol.split('#').next().unwrap_or(&unit.symbol);
        let handler = format!("{path}:{symbol}");
        // Registration facts retain hooks.py evidence, so byte identity must
        // be checked against the callback handler rather than the edge source.
        let source_matches = self.handlers.iter().any(|candidate| {
            candidate.id == handler
                && candidate.source.path == path
                && candidate.source.sha256 == unit.source.sha256
                && candidate.source.revision == unit.source.revision
        });
        self.facts.iter().filter(move |fact| {
            source_matches
                && fact.handler == handler
                && fact.source.revision == unit.source.revision
        })
    }
}
