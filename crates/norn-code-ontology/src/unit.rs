//! The source-unit inventory the code ontology joins to: one entry per
//! function, method, clause or hook registration found by a host tool's
//! syntactic pass. An entry is evidence about source text; it is never
//! executed and claims no parity.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// Exact repository evidence, retaining raw-byte identity and native coordinates.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SourceEvidence {
    /// Fixture-relative source file.
    pub path: String,
    /// One-based source line.
    pub line: u64,
    /// JSON pointer for metadata; empty for code.
    pub pointer: String,
    /// Original repository revision.
    pub revision: String,
    /// SHA256 of the unchanged original file bytes.
    pub sha256: String,
}

/// Exact location of a source call, retained inside its complete-unit gap.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CallSite {
    /// Qualified syntactic call target; dynamic calls are never executed.
    pub symbol: String,
    /// One-based line within the unchanged source file.
    pub line: u64,
}

/// One inventoried source unit, as data.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CodeUnit {
    /// Unique source path plus qualified symbol.
    pub id: String,
    /// Source app namespace.
    pub app: String,
    /// Native controller `DocType`, empty for non-controller code.
    pub doctype: String,
    /// Inventory category, including parse/dynamic-hook gaps.
    pub kind: String,
    /// Exact file revision, digest and line.
    pub source: SourceEvidence,
    /// Qualified method/function or independent clause symbol.
    pub symbol: String,
    /// Syntactic reads (not a claim of runtime completeness).
    pub reads: Vec<String>,
    /// Syntactic writes.
    pub writes: Vec<String>,
    /// Calls including unsupported framework/DB/network effects.
    pub calls: Vec<String>,
    /// Per-call source locations, including unsupported framework/DB effects.
    #[serde(default)]
    pub call_sites: Vec<CallSite>,
    /// Controller method names or source hook event paths.
    pub triggers: Vec<String>,
    /// Recognized source clause; its enclosing method retains a separate gap.
    pub clause: Option<ValidationClause>,
    /// Extraction diagnostic for gaps.
    #[serde(default)]
    pub reason: Option<String>,
    /// Resolved hook target's source location when available.
    #[serde(default)]
    pub resolved_target: Option<String>,
    /// Explicit static state adjacency projection, never whole-handler parity.
    #[serde(default)]
    pub statechart: Option<StatechartProjection>,
}

/// Static adjacency over a source property. Additional handler logic stays a gap.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StatechartProjection {
    /// Exact source field.
    pub field: String,
    /// Only explicitly declared source edges; enum order is never used.
    pub edges: BTreeMap<String, Vec<String>>,
}

/// Conservative source validation clause; its text/link domains are established separately.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ValidationClause {
    /// Attributes conjoined by the source language's truthiness, unchanged.
    pub fields: Vec<String>,
    /// Literal source validation message.
    pub message: String,
    /// All fields absent rather than present; nullable text/link domain only.
    #[serde(default)]
    pub absent: bool,
}
