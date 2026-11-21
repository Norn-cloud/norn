//! Alignment of an imported OWL vocabulary to the REA core and the canon: a
//! short, reviewed table of anchor classes (`cmns-pts:Agent` →
//! `core.Agent`, canon term `norn.core.agent`) lowered to two packs.
//!
//! - **The ontology pack** has one `class` term per imported interface
//!   (`norn.<namespace>.<kebab-name>`, an `exact_match` mapping to the class
//!   IRI of the pinned release, ratified as generated identity), parented by
//!   the interface's first `extends`, and one closed shape: the class IRI and
//!   every single-valued member it declares or inherits.
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
//! A `held` row keeps a lineage out of the canon on purpose: a class that is
//! only partly one REA concept (a credit agreement binds both sides, and only
//! the creditor's right is a claim) is reported `non_conforming` with the
//! row's note until the part that conforms is modelled as its own class.
//! Holding does not hide the rest of the lineage: the anchors each held
//! class still reaches (after precedence) are on its finding and residual
//! note, so a held descendant that also reaches disjoint concepts through
//! another parent is reported as that conflict too. An anchor on a held
//! class or below one is refused: it would be held without a word.
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

/// A class whose lineage is held out of the canon, and why.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct OwlHeld {
    /// The held class IRI (an imported class).
    pub class: String,
    /// Why no REA concept covers the whole lineage, recorded on its residual.
    pub note: String,
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
    /// Lineages reported as residuals whatever anchors they reach.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub held: Vec<OwlHeld>,
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
    /// Its lineage reaches a held class: reported, never aligned.
    Held {
        /// The interface name.
        interface: String,
        /// The held class it reaches.
        via: String,
        /// The core interfaces its lineage still reaches after precedence:
        /// more than one is also a conflict.
        #[serde(skip_serializing_if = "Vec::is_empty")]
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
        let (mut aligned, mut gaps, mut conflicts, mut held) = (0_u64, 0_u64, 0_u64, 0_u64);
        let mut held_conflicts = 0_u64;
        for finding in &self.findings {
            match finding {
                AlignmentFinding::Aligned { anchor, .. } => {
                    aligned = aligned.saturating_add(1);
                    let count = by_anchor.entry(anchor).or_default();
                    *count = count.saturating_add(1);
                }
                AlignmentFinding::Gap { .. } => gaps = gaps.saturating_add(1),
                AlignmentFinding::Conflict { .. } => conflicts = conflicts.saturating_add(1),
                AlignmentFinding::Held { anchors, .. } => {
                    held = held.saturating_add(1);
                    if anchors.len() > 1 {
                        held_conflicts = held_conflicts.saturating_add(1);
                    }
                }
            }
        }
        let total = aligned
            .saturating_add(gaps)
            .saturating_add(conflicts)
            .saturating_add(held);
        let basis_points = aligned
            .saturating_mul(10_000)
            .checked_div(total)
            .unwrap_or(0);
        json!({
            "classes": total,
            "aligned": aligned,
            "alignedBasisPoints": basis_points,
            "canonGaps": gaps,
            "nonConforming": conflicts.saturating_add(held),
            "held": held,
            "heldConflicts": held_conflicts,
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
    members: Vec<Member>,
}

/// One declared member of an imported interface, as its shape field sees it.
struct Member {
    id: String,
    /// The shape value type (`string`, `term_ref`, ...).
    value_type: &'static str,
    /// The link target's interface name.
    target: Option<String>,
    required: bool,
    to_one: bool,
}

/// The members an interface declares: properties (one value each) and links.
fn members_of(interface: &Value) -> Vec<Member> {
    let list = |key: &str| {
        interface
            .get(key)
            .and_then(Value::as_array)
            .map_or(&[][..], Vec::as_slice)
            .iter()
    };
    let text = |value: &Value, pointer: &str| {
        value
            .pointer(pointer)
            .and_then(Value::as_str)
            .map(str::to_owned)
    };
    let mut members = Vec::new();
    for property in list("properties") {
        let Some(id) = text(property, "/id") else {
            continue;
        };
        let value_type = match text(property, "/type/kind").as_deref() {
            Some("boolean") => "boolean",
            Some("date" | "timestamp") => "date_time",
            _ => "string",
        };
        let nullable = property
            .pointer("/constraints/nullable")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        members.push(Member {
            id,
            value_type,
            target: None,
            required: !nullable,
            to_one: true,
        });
    }
    for link in list("links") {
        let (Some(id), Some(target)) = (text(link, "/id"), text(link, "/target")) else {
            continue;
        };
        members.push(Member {
            id,
            value_type: "term_ref",
            target: Some(target),
            required: link
                .pointer("/cardinality/required")
                .and_then(Value::as_bool)
                .unwrap_or(false),
            to_one: text(link, "/cardinality/to").as_deref() == Some("one"),
        });
    }
    members
}

/// The closed promotion shape of a class: its source IRI, then every
/// single-valued member it declares or inherits (the nearest declaration
/// wins, as interfaces only narrow). A shape field's `maxCount` is a number,
/// so many-valued members stay on the interface and out of the shape.
fn shape_of(name: &str, by_name: &BTreeMap<String, Class>) -> Value {
    let mut fields = vec![json!({
        "fieldId": "source_iri", "valueType": "iri", "minCount": 1, "maxCount": 1,
    })];
    let mut seen = BTreeSet::from(["source_iri".to_owned()]);
    let mut visited = BTreeSet::new();
    let mut queue = std::collections::VecDeque::from([name.to_owned()]);
    while let Some(node) = queue.pop_front() {
        if !visited.insert(node.clone()) {
            continue;
        }
        let Some(class) = by_name.get(&node) else {
            continue;
        };
        for member in &class.members {
            if !seen.insert(member.id.clone()) {
                continue;
            }
            if !member.to_one {
                continue;
            }
            let mut field = json!({
                "fieldId": member.id, "valueType": member.value_type,
                "minCount": u8::from(member.required), "maxCount": 1,
            });
            if let (Some(target), Some(object)) = (&member.target, field.as_object_mut()) {
                object.insert("targetTermId".into(), json!(term_of(target)));
            }
            fields.push(field);
        }
        queue.extend(class.extends.iter().cloned());
    }
    let term = term_of(name);
    json!({
        "shapeId": format!("shape.{}", term.trim_start_matches("norn.")),
        "termId": term, "closed": true, "fields": fields,
    })
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
        let members = members_of(interface);
        if by_name
            .insert(
                name.clone(),
                Class {
                    iri,
                    extends,
                    description,
                    members,
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
fn lineages<'a, T>(
    by_name: &'a BTreeMap<String, Class>,
    anchored: &BTreeMap<&'a str, T>,
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
    let mut held: BTreeMap<&str, &OwlHeld> = BTreeMap::new();
    for row in &alignment.held {
        let name = by_iri
            .get(row.class.as_str())
            .ok_or_else(|| format!("held <{}> is not an imported class", row.class))?;
        if anchored.contains_key(name) || held.insert(name, row).is_some() {
            return Err(format!(
                "held <{}> is also an anchor or is listed twice",
                row.class
            ));
        }
    }
    let reached = lineages(&by_name, &anchored);
    let held_by = lineages(&by_name, &held);
    // An anchor at or below a held class would be held without a word.
    for (name, anchor) in &anchored {
        if let Some(via) = held_by.get(name).and_then(|set| set.first()) {
            return Err(format!(
                "anchor <{}> is held: its lineage reaches held {via}; anchor the part that conforms as its own class or drop the hold",
                anchor.class
            ));
        }
    }

    // Precedence, then one finding per class.
    let mut findings = Vec::new();
    let mut outcome: BTreeMap<&str, (Vec<&OwlAnchor>, Vec<String>)> = BTreeMap::new();
    let mut held_out: BTreeMap<&str, (&OwlHeld, Vec<String>)> = BTreeMap::new();
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
        // A held lineage stays a residual, but still reports what it reaches.
        if let Some(row) = held_by
            .get(name)
            .and_then(|set| set.first())
            .and_then(|via| held.get(via).copied())
        {
            let anchors: Vec<String> = by_interface.keys().map(|key| (*key).to_owned()).collect();
            held_out.insert(name, (row, anchors.clone()));
            findings.push(AlignmentFinding::Held {
                interface: (*name).to_owned(),
                via: row.class.clone(),
                anchors,
            });
            continue;
        }
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
    let mut shapes = Vec::new();
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
        shapes.push(shape_of(name, &by_name));
        mappings.push(json!({
            "mappingId": format!("map.{}", term.trim_start_matches("norn.")),
            "termId": term, "sourceId": source, "externalIri": class.iri,
            "relation": "exact_match",
            "justification": "The term is this class of the pinned release, imported by the OWL reader.",
            "evidenceRefs": [source], "confidenceBasisPoints": 10000,
            "reviewedBy": alignment.reviewed_by, "reviewedAt": alignment.reviewed_at,
            // The term is generated from this class: identity, not judgement.
            "ratifiedBy": format!("{} (generated term: the class itself)", crate::owl::OWL_READER_ADAPTER),
            "ratifiedAt": alignment.reviewed_at,
        }));
    }
    let pack = envelope(
        &alignment.pack_id,
        &format!("{source} classes imported by the OWL reader"),
        &json!({
            "packId": alignment.pack_id, "packVersion": "0.1", "packKind": "ontology",
            "sources": [admission], "terms": terms, "mappings": mappings, "shapes": shapes,
        }),
    )?;

    // The link: every term mapped or residual, with its reason.
    let mut correspondences = Vec::new();
    let mut residuals = Vec::new();
    for (name, (row, anchors)) in &held_out {
        let via = by_iri.get(row.class.as_str()).copied().unwrap_or_default();
        let held_note = if via == *name {
            format!("Held out of the canon: {}", row.note)
        } else {
            format!("It specialises {via}, held out of the canon: {}", row.note)
        };
        let also = match anchors.as_slice() {
            [] => String::new(),
            [one] => format!(" Its lineage also reaches {one}."),
            several => format!(
                " Its lineage also reaches {}, which REA keeps disjoint and no precedence orders: a conflict beside the hold.",
                several.join(" and ")
            ),
        };
        residuals.push(json!({
            "termId": term_of(name), "reason": "non_conforming",
            "note": format!("{held_note}{also}"),
        }));
    }
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
    residuals.sort_by(|a, b| a["termId"].as_str().cmp(&b["termId"].as_str()));
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
