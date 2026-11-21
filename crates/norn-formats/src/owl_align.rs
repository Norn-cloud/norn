//! Alignment of an imported OWL vocabulary to the REA core and the canon: a
//! short, reviewed table of anchor classes (`cmns-pts:Agent` →
//! `core.Agent`, canon term `norn.core.agent`) lowered to two packs.
//!
//! - **The ontology pack** has one `class` term per imported interface
//!   (`norn.<namespace>.<kebab-name>`, an `exact_match` mapping to the class
//!   IRI of the pinned release), parented by the interface's first `extends`.
//! - **The link pack** joins it to the canon. A class whose lineage (itself
//!   and every `extends` ancestor) meets exactly one anchor, after
//!   precedence, is `subsumed_by` that anchor's canon term. A class that meets
//!   none is a `canon_gap` residual. A class that still meets several after
//!   precedence (an instrument that is both a resource and a claim) is
//!   `non_conforming`: REA keeps those concepts disjoint, so it is never
//!   mapped to one of them.
//!
//! Precedence is part of the reviewed table, never a heuristic: an anchor may
//! name the anchors it `supersedes` (a debt is a commitment in FIBO, but its
//! holder's view of it is a claim). When a lineage meets both, the
//! superseded anchor is set aside and the finding names it.
//!
//! The checker proves the link total (`norn_checker::validate_link_registry`):
//! every active term of the imported pack is mapped or residual, with its
//! reason. Alignment reads only the lowered interfaces' names and `extends`;
//! it adds no member, drops no edge and changes no type. The type layer
//! stays as the reader lowered it, because FIBO's lattice places classes
//! under several REA concepts at once (a loan is a contract, a debt and an
//! instrument), and inheriting several core interfaces would either refuse
//! those types or pick one silently.

use std::collections::{BTreeMap, BTreeSet};

use norn_ir::{ArtifactEnvelope, OntologySource};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

/// One row of an alignment table.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct OwlAnchor {
    /// The anchor class IRI (an imported class).
    pub class: String,
    /// The core interface it is subsumed by (`core.Agent`).
    pub interface: String,
    /// The canon ontology term of that interface (`norn.core.agent`).
    pub term: String,
    /// Why the class is that REA concept.
    pub justification: String,
    /// Anchor class IRIs this row wins over when one lineage meets both.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub supersedes: Vec<String>,
}

/// An alignment table and the packs it is written to.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct OwlAlignment {
    /// The imported vocabulary's ontology pack id (`norn.ontology-pack.fibo`).
    pub pack_id: String,
    /// The link pack id (`norn.link.fibo.canon`).
    pub link_id: String,
    /// The canon ontology pack id the link targets.
    pub canon_pack: String,
    /// The canon pack version.
    pub canon_version: String,
    /// Who reviewed the table, recorded on every correspondence.
    pub reviewed_by: String,
    /// When (RFC 3339, UTC).
    pub reviewed_at: String,
    /// The anchors.
    pub anchors: Vec<OwlAnchor>,
}

/// What became of one imported class.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(
    tag = "outcome",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub enum AlignmentFinding {
    /// Its lineage meets exactly one anchor after precedence.
    Aligned {
        /// The interface name.
        interface: String,
        /// The core interface.
        anchor: String,
        /// The anchor class its lineage reaches.
        via: String,
        /// Core interfaces set aside by precedence.
        #[serde(skip_serializing_if = "Vec::is_empty")]
        superseded: Vec<String>,
    },
    /// Its lineage meets no anchor: the canon has no term for it.
    Gap {
        /// The interface name.
        interface: String,
    },
    /// Its lineage meets several anchors that no precedence orders.
    Conflict {
        /// The interface name.
        interface: String,
        /// The core interfaces it would be at once.
        anchors: Vec<String>,
    },
}

/// The two packs and one finding per class.
#[derive(Debug, Clone, PartialEq)]
pub struct Aligned {
    /// The imported vocabulary's ontology pack, then the link pack.
    pub packs: Vec<ArtifactEnvelope>,
    /// One finding per interface, sorted.
    pub findings: Vec<AlignmentFinding>,
}

impl Aligned {
    /// Counts by outcome and, for aligned classes, by core interface.
    #[must_use]
    pub fn summary(&self) -> Value {
        let mut by_anchor = BTreeMap::<&str, u64>::new();
        let (mut aligned, mut gaps, mut conflicts) = (0_u64, 0_u64, 0_u64);
        for finding in &self.findings {
            match finding {
                AlignmentFinding::Aligned { anchor, .. } => {
                    aligned = aligned.saturating_add(1);
                    let count = by_anchor.entry(anchor).or_default();
                    *count = count.saturating_add(1);
                }
                AlignmentFinding::Gap { .. } => gaps = gaps.saturating_add(1),
                AlignmentFinding::Conflict { .. } => conflicts = conflicts.saturating_add(1),
            }
        }
        let total = aligned.saturating_add(gaps).saturating_add(conflicts);
        let basis_points = aligned
            .saturating_mul(10_000)
            .checked_div(total)
            .unwrap_or(0);
        json!({
            "classes": total,
            "aligned": aligned,
            "alignedBasisPoints": basis_points,
            "canonGaps": gaps,
            "nonConforming": conflicts,
            "alignedByAnchor": by_anchor,
        })
    }
}

/// The term id of an interface name: `fibo.be.le.lp.LegalEntity` →
/// `norn.fibo.be.le.lp.legal-entity`.
#[must_use]
pub fn term_of(interface: &str) -> String {
    let kebab = |name: &str| crate::owl_lower::snake(name).replace('_', "-");
    match interface.rsplit_once('.') {
        Some((namespace, local)) => format!("norn.{namespace}.{}", kebab(local)),
        None => format!("norn.{}", kebab(interface)),
    }
}

fn envelope(id: &str, name: &str, payload: &Value) -> Result<ArtifactEnvelope, String> {
    serde_json::from_value(json!({
        "irVersion": "0.1",
        "artifactId": id,
        "kind": "business_pack",
        "namespace": "norn.ontology",
        "name": name,
        "version": "0.1.0",
        "lifecycle": "draft",
        "scope": {},
        "dependencies": [],
        "provenance": { "source": "import", "sourcePath": id },
        "payload": payload,
    }))
    .map_err(|error| format!("{id}: {error}"))
}

/// An imported interface: its class IRI, parents and description.
struct Class {
    iri: String,
    extends: Vec<String>,
    description: Option<String>,
}

fn classes(interfaces: &[ArtifactEnvelope]) -> Result<BTreeMap<String, Class>, String> {
    let mut by_name = BTreeMap::new();
    for artifact in interfaces {
        let value = serde_json::to_value(artifact).map_err(|error| error.to_string())?;
        let interface = value.pointer("/payload/interface").unwrap_or(&Value::Null);
        let name = interface
            .get("name")
            .and_then(Value::as_str)
            .ok_or_else(|| format!("{}: not an interface", artifact.artifact_id))?
            .to_owned();
        let iri = value
            .pointer("/provenance/sourcePath")
            .and_then(Value::as_str)
            .ok_or_else(|| format!("{name}: no class IRI in provenance.sourcePath"))?
            .to_owned();
        let extends = interface
            .get("extends")
            .and_then(Value::as_array)
            .map(|parents| {
                parents
                    .iter()
                    .filter_map(|parent| parent.as_str().map(str::to_owned))
                    .collect()
            })
            .unwrap_or_default();
        let description = interface
            .get("description")
            .and_then(Value::as_str)
            .filter(|text| !text.trim().is_empty())
            .map(str::to_owned);
        if by_name
            .insert(
                name.clone(),
                Class {
                    iri,
                    extends,
                    description,
                },
            )
            .is_some()
        {
            return Err(format!("{name} is imported twice"));
        }
    }
    Ok(by_name)
}

/// The anchor classes (by interface name) each class's lineage reaches.
fn lineages<'a>(
    by_name: &'a BTreeMap<String, Class>,
    anchored: &BTreeMap<&'a str, &'a OwlAnchor>,
) -> BTreeMap<&'a str, BTreeSet<&'a str>> {
    let mut reached = BTreeMap::new();
    for name in by_name.keys() {
        let mut seen = BTreeSet::new();
        let mut stack = vec![name.as_str()];
        let mut found = BTreeSet::new();
        while let Some(node) = stack.pop() {
            if !seen.insert(node) {
                continue;
            }
            if anchored.contains_key(node) {
                found.insert(node);
            }
            if let Some(class) = by_name.get(node) {
                stack.extend(class.extends.iter().map(String::as_str));
            }
        }
        reached.insert(name.as_str(), found);
    }
    reached
}

/// Align `interfaces` (the OWL reader's vocabulary layer) with `alignment`.
///
/// # Errors
///
/// When an anchor names a class that was not imported or supersedes one
/// that is not an anchor, an interface is malformed, or two classes would
/// share a term id.
#[allow(
    clippy::too_many_lines,
    reason = "one pass builds the findings, terms and link together"
)]
pub fn align(
    interfaces: &[ArtifactEnvelope],
    admission: &OntologySource,
    alignment: &OwlAlignment,
) -> Result<Aligned, String> {
    let by_name = classes(interfaces)?;
    let by_iri: BTreeMap<&str, &str> = by_name
        .iter()
        .map(|(name, class)| (class.iri.as_str(), name.as_str()))
        .collect();
    let mut anchored: BTreeMap<&str, &OwlAnchor> = BTreeMap::new();
    for anchor in &alignment.anchors {
        let name = by_iri
            .get(anchor.class.as_str())
            .ok_or_else(|| format!("anchor <{}> is not an imported class", anchor.class))?;
        if anchored.insert(name, anchor).is_some() {
            return Err(format!("anchor <{}> is listed twice", anchor.class));
        }
    }
    for anchor in &alignment.anchors {
        for superseded in &anchor.supersedes {
            if !alignment.anchors.iter().any(|row| &row.class == superseded) {
                return Err(format!(
                    "anchor <{}> supersedes <{superseded}>, which is not an anchor",
                    anchor.class
                ));
            }
        }
    }
    let reached = lineages(&by_name, &anchored);

    // Precedence, then one finding per class.
    let mut findings = Vec::new();
    let mut outcome: BTreeMap<&str, (Vec<&OwlAnchor>, Vec<String>)> = BTreeMap::new();
    for (name, found) in &reached {
        let rows: Vec<&OwlAnchor> = found
            .iter()
            .filter_map(|n| anchored.get(n).copied())
            .collect();
        let (kept, set_aside): (Vec<&OwlAnchor>, Vec<&OwlAnchor>) = rows.iter().partition(|row| {
            !rows
                .iter()
                .any(|other| other.supersedes.iter().any(|class| class == &row.class))
        });
        // Rows of one core interface agree: keep one per interface.
        let mut by_interface: BTreeMap<&str, &OwlAnchor> = BTreeMap::new();
        for row in &kept {
            by_interface.entry(row.interface.as_str()).or_insert(row);
        }
        let superseded: Vec<String> = set_aside
            .iter()
            .map(|row| row.interface.clone())
            .filter(|interface| !by_interface.contains_key(interface.as_str()))
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
        let finding = match by_interface.values().collect::<Vec<_>>().as_slice() {
            [] => AlignmentFinding::Gap {
                interface: (*name).to_owned(),
            },
            [row] => AlignmentFinding::Aligned {
                interface: (*name).to_owned(),
                anchor: row.interface.clone(),
                via: row.class.clone(),
                superseded: superseded.clone(),
            },
            several => AlignmentFinding::Conflict {
                interface: (*name).to_owned(),
                anchors: several.iter().map(|row| row.interface.clone()).collect(),
            },
        };
        outcome.insert(name, (by_interface.into_values().collect(), superseded));
        findings.push(finding);
    }
    findings.sort();

    // The imported vocabulary's ontology pack.
    let source = admission.source_id.as_str();
    let mut terms = Vec::new();
    let mut mappings = Vec::new();
    let mut term_ids = BTreeSet::new();
    for (name, class) in &by_name {
        let term = term_of(name);
        if !term_ids.insert(term.clone()) {
            return Err(format!("two classes share term id {term}"));
        }
        let local = name
            .rsplit_once('.')
            .map_or(name.as_str(), |(_, local)| local);
        let definition = class.description.clone().unwrap_or_else(|| {
            format!(
                "The class <{}> of {source}, which gives it no definition.",
                class.iri
            )
        });
        let mut entry = json!({
            "termId": term, "label": local, "definition": definition,
            "termKind": "class", "status": "active", "semanticAuthority": "semantic_only",
        });
        if let Some(parent) = class.extends.first()
            && let Some(fields) = entry.as_object_mut()
        {
            fields.insert("parentTermId".into(), json!(term_of(parent)));
        }
        terms.push(entry);
        mappings.push(json!({
            "mappingId": format!("map.{}", term.trim_start_matches("norn.")),
            "termId": term, "sourceId": source, "externalIri": class.iri,
            "relation": "exact_match",
            "justification": "The term is this class of the pinned release, imported by the OWL reader.",
            "evidenceRefs": [source], "confidenceBasisPoints": 10000,
            "reviewedBy": alignment.reviewed_by, "reviewedAt": alignment.reviewed_at,
        }));
    }
    let pack = envelope(
        &alignment.pack_id,
        &format!("{source} classes imported by the OWL reader"),
        &json!({
            "packId": alignment.pack_id, "packVersion": "0.1", "packKind": "ontology",
            "sources": [admission], "terms": terms, "mappings": mappings,
        }),
    )?;

    // The link: every term mapped or residual, with its reason.
    let mut correspondences = Vec::new();
    let mut residuals = Vec::new();
    for (name, (rows, superseded)) in &outcome {
        let term = term_of(name);
        match rows.as_slice() {
            [] => residuals.push(json!({
                "termId": term, "reason": "canon_gap",
                "note": "No anchor in its lineage: the alignment names it no agent, resource, economic event, commitment or claim.",
            })),
            [row] => {
                let set_aside = if superseded.is_empty() {
                    String::new()
                } else {
                    format!(" (precedence sets aside {})", superseded.join(", "))
                };
                let via = by_iri.get(row.class.as_str()).copied().unwrap_or_default();
                let reason = if via == *name {
                    row.justification.clone()
                } else {
                    format!("it specialises {via}. {}", row.justification)
                };
                correspondences.push(json!({
                    "correspondenceId": format!("corr.{}", term.trim_start_matches("norn.")),
                    "fromTermId": term, "relation": "subsumed_by", "toTermIds": [row.term],
                    "justification": format!("Its lineage reaches {}{set_aside}: {reason}", row.interface),
                    "evidenceRefs": [source], "confidenceBasisPoints": 7000,
                    "reviewedBy": alignment.reviewed_by, "reviewedAt": alignment.reviewed_at,
                }));
            }
            several => residuals.push(json!({
                "termId": term, "reason": "non_conforming",
                "note": format!(
                    "Its lineage reaches {}, which REA keeps disjoint and no precedence orders; never mapped to one of them.",
                    several.iter().map(|row| row.interface.as_str()).collect::<Vec<_>>().join(" and ")
                ),
            })),
        }
    }
    let mut link_payload = json!({
        "packId": alignment.link_id, "packVersion": "0.1", "packKind": "link",
        "fromPack": { "packId": alignment.pack_id, "version": "0.1.0" },
        "toPack": { "packId": alignment.canon_pack, "version": alignment.canon_version },
        "correspondences": correspondences,
    });
    if !residuals.is_empty()
        && let Some(fields) = link_payload.as_object_mut()
    {
        fields.insert("residuals".into(), Value::Array(residuals));
    }
    let link = envelope(
        &alignment.link_id,
        &format!("{} to the Norn canon", alignment.pack_id),
        &link_payload,
    )?;
    Ok(Aligned {
        packs: vec![pack, link],
        findings,
    })
}
