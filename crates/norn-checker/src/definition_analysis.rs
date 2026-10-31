//! Bounded decision-table evidence from the shared check/install path.
//! Upstream graph transformations and defaults never borrow the root schema.
use crate::{ArtifactRef, table_analysis};
use norn_expr::schema::Schema;
use norn_ir::{ArtifactEnvelope, ArtifactKind, JdmNode};
use std::collections::BTreeMap;

/// Why finite definition evidence is unavailable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Unknown {
    /// The candidate failed ordinary admission.
    NotAdmitted,
    /// The graph has no qualified table-local input schema.
    GraphInput,
    /// The artifact cannot be bound to its semantic identity.
    ArtifactIdentity,
    /// Aggregate analysis work or retained evidence is exhausted.
    Limit,
    /// The table evaluator cannot establish an exhaustive verdict.
    Evaluator(table_analysis::Unknown),
}

/// A table finding bound to a versioned artifact and stable graph member.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TableReport {
    /// Exact artifact id, version and Norn1 content digest.
    pub artifact: ArtifactRef,
    /// Exact typed-envelope commitment; preserves executable Unicode spelling.
    pub artifact_exact_digest: String,
    /// Stable table node identity inside that artifact.
    pub node_id: String,
    /// Exhaustive finite result, or an explicit reason for no conclusion.
    pub result: Result<table_analysis::Analysis, Unknown>,
}

/// Finite table evidence, separate from admission and proof claims.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DefinitionAnalysis {
    /// Findings for every retained decision-table node.
    pub tables: Vec<TableReport>,
}

/// Versioned finite-table semantics and resource profile.
pub const PROFILE: &str = "norn.definition-table-analysis/1";
const SET_WORK: usize = 8_192;
struct Work(usize);
impl Work {
    fn charge(&mut self, amount: usize) -> Result<(), Unknown> {
        self.0 = self.0.checked_sub(amount).ok_or(Unknown::Limit)?;
        Ok(())
    }
}

pub(crate) fn analyze(pool: &[ArtifactEnvelope]) -> Result<DefinitionAnalysis, Unknown> {
    let mut result = DefinitionAnalysis::default();
    let mut work = Work(SET_WORK);
    let mut report_bytes = Work(65_536);
    for artifact in pool {
        analyze_tables(artifact, &mut work, &mut report_bytes, &mut result.tables)?;
    }
    Ok(result)
}

fn analyze_tables(
    artifact: &ArtifactEnvelope,
    work: &mut Work,
    report_bytes: &mut Work,
    reports: &mut Vec<TableReport>,
) -> Result<(), Unknown> {
    if artifact.kind() != ArtifactKind::Rule {
        return Ok(());
    }
    let graph = crate::jdm_profile::decode_graph(artifact).ok_or(Unknown::GraphInput)?;
    let payload = artifact.payload.to_json_value();
    let schema = payload
        .as_object()
        .and_then(|p| p.get("inputSchema"))
        .and_then(|s| Schema::parse(s, "input").ok());
    let mut predecessors: BTreeMap<&str, Vec<&norn_ir::jdm::JdmEdge>> = BTreeMap::new();
    for edge in &graph.edges {
        predecessors.entry(&edge.target_id).or_default().push(edge);
    }
    let identity = ArtifactRef::of(artifact).map_err(|_error| Unknown::ArtifactIdentity)?;
    let exact = crate::obligations::exact_artifact_digest(artifact)
        .map_err(|_error| Unknown::ArtifactIdentity)?;
    for node in &graph.nodes {
        let JdmNode::DecisionTableNode { id, content, .. } = node else {
            continue;
        };
        let incoming = predecessors.get(id.as_str()).map_or(&[][..], Vec::as_slice);
        let no_defaults = payload
            .as_object()
            .and_then(|p| p.get("defaults"))
            .is_none_or(|v| v.as_object().is_some_and(BTreeMap::is_empty));
        let direct = no_defaults
            && incoming.len() == 1
            && incoming.first().is_some_and(|edge| {
                graph.nodes.iter().any(
                |n| matches!(n,JdmNode::InputNode {id,content:None,..} if id == &edge.source_id),
            )
            });
        let evidence = if direct {
            schema
                .as_ref()
                .ok_or(Unknown::GraphInput)
                .and_then(|schema| {
                    let inputs =
                        table_analysis::finite_inputs(schema).map_err(Unknown::Evaluator)?;
                    let evaluations = inputs
                        .len()
                        .saturating_mul(content.rules.len().max(1))
                        .saturating_mul(content.inputs.len().max(1))
                        .saturating_mul(3);
                    work.charge(evaluations)?;
                    table_analysis::analyze_table(content, schema).map_err(Unknown::Evaluator)
                })
        } else {
            Err(Unknown::GraphInput)
        };
        report_bytes.charge(
            identity
                .artifact_id
                .len()
                .saturating_add(identity.version.len())
                .saturating_add(identity.sha256.len())
                .saturating_add(exact.len())
                .saturating_add(id.len())
                .saturating_add(table_bytes(&evidence)),
        )?;
        reports.push(TableReport {
            artifact: identity.clone(),
            artifact_exact_digest: exact.clone(),
            node_id: id.clone(),
            result: evidence,
        });
    }
    Ok(())
}

fn json_bytes(value: &serde_json::Value) -> usize {
    // Every witness is already bounded by finite_inputs; six bytes per UTF-8
    // byte conservatively covers JSON string escaping in retained evidence.
    value.to_string().len().saturating_mul(6)
}
fn unknown_bytes(value: &Unknown) -> usize {
    match value {
        Unknown::Evaluator(table_analysis::Unknown::Expression(message)) => message.len(),
        Unknown::NotAdmitted
        | Unknown::GraphInput
        | Unknown::ArtifactIdentity
        | Unknown::Limit
        | Unknown::Evaluator(_) => 64,
    }
}
fn table_bytes(result: &Result<table_analysis::Analysis, Unknown>) -> usize {
    match result {
        Err(reason) => unknown_bytes(reason),
        Ok(value) => {
            let mut bytes = 128usize;
            if let Some(input) = &value.uncovered {
                bytes = bytes.saturating_add(json_bytes(input));
            }
            for id in &value.unreachable_rows {
                bytes = bytes.saturating_add(id.len().saturating_mul(6).saturating_add(32));
            }
            for overlap in &value.overlaps {
                bytes = bytes
                    .saturating_add(json_bytes(&overlap.input))
                    .saturating_add(128);
                for id in overlap.matching_rows.iter().chain(&overlap.selected_rows) {
                    bytes = bytes.saturating_add(id.len().saturating_mul(6).saturating_add(32));
                }
            }
            bytes
        }
    }
}
