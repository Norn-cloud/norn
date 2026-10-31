//! The consistency report over the system packs: per link, every obligation
//! with its findings and its honest evidence level, as JSON a UI can render
//! (`norn-consistency-report@1`).

use std::collections::BTreeSet;

use norn_ir::ArtifactEnvelope;
use serde_json::{Value, json};

use super::chase::ChaseBudget;
use super::exchange::{Exchange, check_mapping_totality, check_schema_compatibility};
use super::packs::{PackLink, lower_pack_links};
use super::{check_non_vacuity, codes};
use crate::Diagnostic;

/// Format tag of [`pack_consistency_report`].
pub const REPORT_FORMAT: &str = "norn-consistency-report@1";

fn finding(diagnostic: &Diagnostic) -> Value {
    json!({
        "code": diagnostic.code.as_str(),
        "severity": if diagnostic.is_error() { "error" } else { "warning" },
        "message": diagnostic.message,
        "artifactId": diagnostic.artifact_id,
        "pointer": diagnostic.span.as_ref().map(|span| span.pointer.clone()),
    })
}

fn obligation(name: &str, evidence: String, findings: &[&Diagnostic]) -> Value {
    json!({
        "obligation": name,
        "evidence": evidence,
        "findings": findings.iter().map(|d| finding(d)).collect::<Vec<_>>(),
    })
}

fn link_report(link: &PackLink, budget: ChaseBudget) -> Value {
    let exchange = Exchange {
        source_schema: &link.source_schema,
        source: &link.source,
        mapping: &link.mapping,
        target: &link.target,
    };
    let mut merged = link.source.clone();
    merged
        .dependencies
        .extend(link.mapping.dependencies.iter().cloned());
    merged
        .dependencies
        .extend(link.target.dependencies.iter().cloned());
    let vacuity = check_non_vacuity(&merged, budget);
    let compatibility = check_schema_compatibility(&exchange, budget);
    let totality = check_mapping_totality(&exchange, &link.gaps);
    let refusals: BTreeSet<&str> = [
        codes::MALFORMED,
        codes::NOT_WEAKLY_ACYCLIC,
        codes::CHASE_REFUSED,
    ]
    .into();
    let with = |all: &[Diagnostic], keep: &[&str]| -> Vec<&Diagnostic> {
        all.iter()
            .filter(|d| keep.contains(&d.code.as_str()) || refusals.contains(d.code.as_str()))
            .collect()
    };
    let bound = format!(
        "chase budget: {} facts, {} nulls, {} steps per run",
        budget.max_facts, budget.max_nulls, budget.max_steps
    );
    let mut divergence = obligation(
        "divergence",
        "not evaluated: divergences need mapped instance records, and none are supplied".to_owned(),
        &[],
    );
    if let Some(object) = divergence.as_object_mut() {
        object.insert("evaluated".to_owned(), Value::Bool(false));
    }
    json!({
        "link": link.link_id,
        "lowered": {
            "sourceRules": link.source.dependencies.len(),
            "mappingRules": link.mapping.dependencies.len(),
            "targetRules": link.target.dependencies.len(),
            "declaredGaps": link.gaps.len(),
            "notLowered": link.not_lowered,
        },
        "obligations": [
            obligation(
                "non_vacuity",
                format!("exhaustive-finite over the lowered theory: a frozen tuple per relation is chased to a model or a conflict ({bound}); a refusal is reported, never a pass"),
                &with(&vacuity, &[codes::VACUOUS]),
            ),
            obligation(
                "schema_incompatibility",
                format!("exhaustive-finite over the lowered theory: each source relation's frozen tuple is chased with source, mapping and canon ({bound})"),
                &with(&compatibility, &[codes::INCOMPATIBLE]),
            ),
            obligation(
                "unpreserved_constraint",
                format!("bounded: each reported witness is verified as concrete data, but the search tries only single-step unfoldings of each canon constraint through the link's rules ({bound}) and can miss witnesses; no finding means none was found within the bound, not that the constraint is preserved"),
                &compatibility.iter().filter(|d| d.code == codes::UNPRESERVED).collect::<Vec<_>>(),
            ),
            obligation(
                "mapping_totality",
                "structural: every source term is read by a lowered correspondence or is a declared gap (residual, derived or canon → system only)".to_owned(),
                &with(&totality, &[codes::NON_TOTAL]),
            ),
            divergence,
        ],
    })
}

/// The report for every link in `artifacts`.
///
/// Scope: only what the packs declare as typed terms, shapes and
/// correspondences. Prose (such as a disjointness stated in a description)
/// and correspondences listed as not lowered contribute nothing.
///
/// # Errors
/// A pack or link that does not lower.
pub fn pack_consistency_report(
    artifacts: &[ArtifactEnvelope],
    budget: ChaseBudget,
) -> Result<Value, String> {
    let links = lower_pack_links(artifacts)?;
    Ok(json!({
        "format": REPORT_FORMAT,
        "scope": "typed pack terms, shapes and correspondences only; prose and not-lowered correspondences contribute nothing",
        "links": links.iter().map(|link| link_report(link, budget)).collect::<Vec<_>>(),
    }))
}
