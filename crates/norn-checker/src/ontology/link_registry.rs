//! Registry rules of link packs (norn#56): a link resolves against the two
//! ontology packs it joins, and accounts for every active term of its source
//! pack. The coverage it returns is the data of the projection report.

use std::collections::{BTreeMap, BTreeSet};

use norn_ir::{ArtifactEnvelope, Diagnostic, JsonValue};

use super::{REGISTRY_CODE, as_array, as_object, diagnostic, string_field};

/// What one admitted link maps, leaves residual, and finds missing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinkCoverage {
    /// The link pack's artifact id.
    pub link_id: String,
    /// The source pack's id.
    pub from_pack: String,
    /// The target pack's id.
    pub to_pack: String,
    /// Active source terms.
    pub active_terms: usize,
    /// Source term → relation and target terms, one entry per correspondence.
    pub mapped: BTreeMap<String, Vec<(String, Vec<String>)>>,
    /// System-specific detail the canon drops.
    pub residual: BTreeSet<String>,
    /// Source terms the canon lacks a term for.
    pub canon_gaps: BTreeSet<String>,
    /// Source behaviour that breaks a canon law; flagged, never mapped.
    pub non_conforming: BTreeSet<String>,
}

#[derive(Default)]
struct PackIndex {
    version: String,
    /// Term id → (status, kind).
    terms: BTreeMap<String, (String, String)>,
    /// Source id → import mode.
    sources: BTreeMap<String, String>,
}

/// Validates every link pack in `artifacts` against the ontology packs in
/// the same set and returns one coverage record per link.
///
/// # Errors
/// Returns registry diagnostics for unresolved packs or terms, incompatible
/// term kinds, evidence outside the source manifest, and unaccounted terms.
pub fn validate_link_registry(
    artifacts: &[ArtifactEnvelope],
) -> Result<Vec<LinkCoverage>, Vec<Diagnostic>> {
    let mut packs = BTreeMap::new();
    let mut links = Vec::new();
    for artifact in artifacts {
        let payload = artifact.payload.to_json_value();
        let Some(object) = as_object(&payload) else {
            continue;
        };
        match string_field(object, "packKind") {
            Some("ontology") => {
                packs.insert(
                    artifact.artifact_id.clone(),
                    index_pack(object, &artifact.version),
                );
            }
            Some("link") => links.push((artifact.artifact_id.clone(), payload.clone())),
            _ => {}
        }
    }
    let mut diagnostics = Vec::new();
    let coverage = links
        .iter()
        .filter_map(|(id, link)| validate_link(id, link, &packs, &mut diagnostics))
        .collect();
    if diagnostics.is_empty() {
        Ok(coverage)
    } else {
        Err(diagnostics)
    }
}

fn index_pack(pack: &BTreeMap<String, JsonValue>, version: &str) -> PackIndex {
    let mut index = PackIndex {
        version: version.to_owned(),
        ..PackIndex::default()
    };
    for term in pack
        .get("terms")
        .and_then(as_array)
        .unwrap_or_default()
        .iter()
        .filter_map(as_object)
    {
        if let (Some(id), Some(status), Some(kind)) = (
            string_field(term, "termId"),
            string_field(term, "status"),
            string_field(term, "termKind"),
        ) {
            index
                .terms
                .insert(id.to_owned(), (status.to_owned(), kind.to_owned()));
        }
    }
    for source in pack
        .get("sources")
        .and_then(as_array)
        .unwrap_or_default()
        .iter()
        .filter_map(as_object)
    {
        if let (Some(id), Some(mode)) = (
            string_field(source, "sourceId"),
            string_field(source, "importMode"),
        ) {
            index.sources.insert(id.to_owned(), mode.to_owned());
        }
    }
    index
}

fn resolve<'a>(
    link: &BTreeMap<String, JsonValue>,
    field: &str,
    packs: &'a BTreeMap<String, PackIndex>,
    report: &mut impl FnMut(String, String),
) -> Option<(&'a PackIndex, String)> {
    let endpoint = link.get(field).and_then(as_object);
    let pinned = endpoint.and_then(|endpoint| {
        Some((
            string_field(endpoint, "packId")?,
            string_field(endpoint, "version")?,
        ))
    });
    let Some((pack_id, version)) = pinned else {
        // A malformed link never drops out of the coverage silently.
        report(
            format!("{field} must pin a packId and a version"),
            format!("payload.{field}"),
        );
        return None;
    };
    match packs.get(pack_id) {
        Some(pack) if pack.version == version => Some((pack, pack_id.to_owned())),
        Some(pack) => {
            report(
                format!(
                    "{field} pins {pack_id}@{version}, the registry has @{}",
                    pack.version
                ),
                format!("payload.{field}.version"),
            );
            None
        }
        None => {
            report(
                format!("{field} '{pack_id}' is not an ontology pack in the registry"),
                format!("payload.{field}.packId"),
            );
            None
        }
    }
}

fn validate_link(
    link_id: &str,
    link: &JsonValue,
    packs: &BTreeMap<String, PackIndex>,
    diagnostics: &mut Vec<Diagnostic>,
) -> Option<LinkCoverage> {
    let Some(link) = as_object(link) else {
        diagnostics.push(diagnostic(
            REGISTRY_CODE,
            "link payload must be an object",
            "payload",
            Some(link_id),
        ));
        return None;
    };
    let before = diagnostics.len();
    let mut report = |message: String, path: String| {
        diagnostics.push(diagnostic(REGISTRY_CODE, message, path, Some(link_id)));
    };
    let from = resolve(link, "fromPack", packs, &mut report);
    let to = resolve(link, "toPack", packs, &mut report);
    let ((from, from_id), (to, to_id)) = (from?, to?);
    let active = |pack: &PackIndex, term: &str| {
        pack.terms
            .get(term)
            .filter(|(status, _)| status == "active")
            .map(|(_, kind)| kind.clone())
    };

    let mut coverage = LinkCoverage {
        link_id: link_id.to_owned(),
        from_pack: from_id,
        to_pack: to_id,
        active_terms: from
            .terms
            .values()
            .filter(|(status, _)| status == "active")
            .count(),
        mapped: BTreeMap::new(),
        residual: BTreeSet::new(),
        canon_gaps: BTreeSet::new(),
        non_conforming: BTreeSet::new(),
    };
    let correspondences = link
        .get("correspondences")
        .and_then(as_array)
        .unwrap_or_default();
    for (index, item) in correspondences
        .iter()
        .enumerate()
        .filter_map(|(i, c)| as_object(c).map(|c| (i, c)))
    {
        let path = format!("payload.correspondences[{index}]");
        let relation = string_field(item, "relation").unwrap_or_default();
        let Some(from_term) = string_field(item, "fromTermId") else {
            continue;
        };
        let Some(from_kind) = active(from, from_term) else {
            report(
                format!("fromTermId '{from_term}' is not an active term of the from pack"),
                format!("{path}.fromTermId"),
            );
            continue;
        };
        let targets: Vec<String> = item
            .get("toTermIds")
            .and_then(as_array)
            .unwrap_or_default()
            .iter()
            .filter_map(|t| t.as_string().map(str::to_owned))
            .collect();
        for (target_index, target) in targets.iter().enumerate() {
            let target_path = format!("{path}.toTermIds[{target_index}]");
            match active(to, target) {
                None => report(
                    format!("toTermId '{target}' is not an active term of the to pack"),
                    target_path,
                ),
                Some(kind) if relation != "derived" && kind != from_kind => report(
                    format!(
                        "a {relation} correspondence joins terms of one kind, not {from_kind} and {kind}"
                    ),
                    target_path,
                ),
                Some(kind)
                    if relation == "derived"
                        && !matches!(kind.as_str(), "property" | "predicate") =>
                {
                    report(
                        format!(
                            "a derived correspondence reads properties or predicates, not a {kind}"
                        ),
                        target_path,
                    );
                }
                Some(_) => {}
            }
        }
        if relation == "derived" && !matches!(from_kind.as_str(), "property" | "predicate") {
            report(
                format!("only a property or predicate is derived, not a {from_kind}"),
                format!("{path}.fromTermId"),
            );
        }
        let evidence = item
            .get("evidenceRefs")
            .and_then(as_array)
            .unwrap_or_default();
        for (evidence_index, reference) in evidence.iter().filter_map(|r| r.as_string()).enumerate()
        {
            if !reference.starts_with("source.") {
                continue;
            }
            match from.sources.get(reference).map(String::as_str) {
                Some("import" | "map_only") => {}
                Some(mode) => report(
                    format!("evidence '{reference}' is a {mode} source"),
                    format!("{path}.evidenceRefs[{evidence_index}]"),
                ),
                None => report(
                    format!("evidence '{reference}' is not in the from pack's source manifest"),
                    format!("{path}.evidenceRefs[{evidence_index}]"),
                ),
            }
        }
        coverage
            .mapped
            .entry(from_term.to_owned())
            .or_default()
            .push((relation.to_owned(), targets));
    }
    let residuals = link.get("residuals").and_then(as_array).unwrap_or_default();
    for (index, item) in residuals
        .iter()
        .enumerate()
        .filter_map(|(i, r)| as_object(r).map(|r| (i, r)))
    {
        let Some(term) = string_field(item, "termId") else {
            continue;
        };
        if active(from, term).is_none() {
            report(
                format!("residual '{term}' is not an active term of the from pack"),
                format!("payload.residuals[{index}].termId"),
            );
        } else if string_field(item, "reason") == Some("canon_gap") {
            coverage.canon_gaps.insert(term.to_owned());
        } else if string_field(item, "reason") == Some("non_conforming") {
            coverage.non_conforming.insert(term.to_owned());
        } else {
            coverage.residual.insert(term.to_owned());
        }
    }
    for (term, (status, _)) in &from.terms {
        let accounted = coverage.mapped.contains_key(term)
            || coverage.residual.contains(term)
            || coverage.canon_gaps.contains(term)
            || coverage.non_conforming.contains(term);
        if status == "active" && !accounted {
            report(
                format!("active term '{term}' has neither a correspondence nor a residual"),
                "payload.correspondences".to_owned(),
            );
        }
    }
    (diagnostics.len() == before).then_some(coverage)
}
