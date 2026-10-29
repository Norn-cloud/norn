//! Provenance presence (T8-LINT-1) over every candidate artifact's
//! envelope `provenance`.
//!
//! - [`codes::UNTRACEABLE_PROVENANCE`] (error): the origin cannot be traced.
//!   An `import` or `migration` artifact names no `sourcePath` (which source
//!   it came from), or an `agent` artifact names no `actorId` (which agent
//!   proposed it).
//! - [`codes::INCOMPLETE_PROVENANCE`] (warning): the origin is traceable
//!   but a member is missing: `createdAt` on any artifact, or `actorId` on an
//!   `authoring_sdk`, `import`, or `migration` artifact. A `system` artifact
//!   needs no `actorId`.
//!
//! A blank (empty or whitespace-only) member counts as missing.

use norn_ir::{ArtifactProvenance, ProvenanceSource};

use super::finding;
use super::model::LintSet;
use crate::Diagnostic;

/// Diagnostic codes of the provenance rules.
pub mod codes {
    /// The artifact's origin cannot be traced from its provenance.
    pub const UNTRACEABLE_PROVENANCE: &str = "ontology.lint.untraceable_provenance";
    /// The artifact's provenance is traceable but missing a member.
    pub const INCOMPLETE_PROVENANCE: &str = "ontology.lint.incomplete_provenance";
}

/// The provenance rules over every candidate artifact.
#[must_use]
pub(crate) fn lint_provenance(set: &LintSet<'_>) -> Vec<Diagnostic> {
    let mut out = Vec::new();
    for artifact in set.candidates {
        let provenance = &artifact.provenance;
        let source = source_name(&provenance.source);
        let untraceable = match provenance.source {
            ProvenanceSource::Import | ProvenanceSource::Migration => {
                missing(provenance.source_path.as_deref()).then_some("sourcePath")
            }
            ProvenanceSource::Agent => missing(provenance.actor_id.as_deref()).then_some("actorId"),
            ProvenanceSource::AuthoringSdk | ProvenanceSource::System => None,
        };
        if let Some(member) = untraceable {
            out.push(finding(
                codes::UNTRACEABLE_PROVENANCE,
                format!(
                    "this {source} artifact names no provenance {member}, so its origin cannot be traced"
                ),
                &artifact.artifact_id,
                Some("/provenance".to_owned()),
            ));
            continue;
        }
        let incomplete = incomplete_members(provenance);
        if !incomplete.is_empty() {
            out.push(finding(
                codes::INCOMPLETE_PROVENANCE,
                format!(
                    "the provenance of this {source} artifact is missing {}",
                    incomplete.join(" and ")
                ),
                &artifact.artifact_id,
                Some("/provenance".to_owned()),
            ));
        }
    }
    out
}

fn incomplete_members(provenance: &ArtifactProvenance) -> Vec<&'static str> {
    let mut members = Vec::new();
    let needs_actor = !matches!(provenance.source, ProvenanceSource::System);
    if needs_actor && missing(provenance.actor_id.as_deref()) {
        members.push("actorId");
    }
    if missing(provenance.created_at.as_deref()) {
        members.push("createdAt");
    }
    members
}

fn missing(member: Option<&str>) -> bool {
    member.is_none_or(|value| value.trim().is_empty())
}

const fn source_name(source: &ProvenanceSource) -> &'static str {
    match source {
        ProvenanceSource::AuthoringSdk => "authoring_sdk",
        ProvenanceSource::Agent => "agent",
        ProvenanceSource::Migration => "migration",
        ProvenanceSource::Import => "import",
        ProvenanceSource::System => "system",
    }
}
