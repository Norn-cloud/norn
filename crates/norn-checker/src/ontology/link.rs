//! Single-artifact validation of link packs (`packKind: "link"`, norn#56):
//! checked correspondences from one ontology pack's terms into another's.
//! Resolution against the linked packs is a registry rule.

use std::collections::BTreeSet;

use norn_ir::{Diagnostic, JsonValue};

use super::{
    as_object, diagnostic, is_absolute_web_iri, non_empty_array, reject_unknown_fields,
    require_literal, required_string, valid_prefixed_id, valid_semantic_version,
    valid_utc_date_time,
};

pub(super) const LINK_CODE: &str = "ontology_link.validation.invalid_payload";
const RELATIONS: [&str; 4] = ["equivalent", "subsumed_by", "subsumes", "derived"];
const RESIDUAL_REASONS: [&str; 2] = ["system_specific", "canon_gap"];

/// Validates one link-pack payload in isolation.
pub(crate) fn validate_link_payload(
    value: &JsonValue,
    diagnostics: &mut Vec<Diagnostic>,
    artifact_id: Option<&str>,
) {
    let Some(link) = as_object(value) else {
        diagnostics.push(diagnostic(
            LINK_CODE,
            "payload must be an object",
            "payload",
            artifact_id,
        ));
        return;
    };
    let fields = [
        "packId",
        "packVersion",
        "packKind",
        "fromPack",
        "toPack",
        "correspondences",
        "residuals",
    ];
    reject_unknown_fields(
        link,
        &fields,
        "payload",
        diagnostics,
        artifact_id,
        LINK_CODE,
    );
    if let Some(pack_id) = required_string(
        link,
        "packId",
        "payload.packId",
        diagnostics,
        artifact_id,
        LINK_CODE,
    ) && !valid_prefixed_id(&pack_id, "norn.link.")
    {
        diagnostics.push(diagnostic(
            LINK_CODE,
            "link packId must use the opaque norn.link.* namespace",
            "payload.packId",
            artifact_id,
        ));
    }
    require_literal(
        link,
        "packKind",
        &["link"],
        "payload.packKind",
        diagnostics,
        artifact_id,
        LINK_CODE,
    );
    require_literal(
        link,
        "packVersion",
        &["0.1"],
        "payload.packVersion",
        diagnostics,
        artifact_id,
        LINK_CODE,
    );
    let from = validate_endpoint(link, "fromPack", diagnostics, artifact_id);
    let to = validate_endpoint(link, "toPack", diagnostics, artifact_id);
    if from.is_some() && from == to {
        diagnostics.push(diagnostic(
            LINK_CODE,
            "a link joins two different packs",
            "payload.toPack.packId",
            artifact_id,
        ));
    }

    let mut correspondence_ids = BTreeSet::new();
    let mut mapped = BTreeSet::new();
    if let Some(items) = non_empty_array(
        link.get("correspondences"),
        "payload.correspondences",
        diagnostics,
        artifact_id,
        LINK_CODE,
    ) {
        for (index, item) in items.iter().enumerate() {
            let path = format!("payload.correspondences[{index}]");
            if let Some((id, from_term)) =
                validate_correspondence(item, &path, diagnostics, artifact_id)
            {
                if !correspondence_ids.insert(id) {
                    diagnostics.push(diagnostic(
                        LINK_CODE,
                        "correspondenceId must be unique within the link",
                        format!("{path}.correspondenceId"),
                        artifact_id,
                    ));
                }
                mapped.insert(from_term);
            }
        }
    }
    validate_residuals(link.get("residuals"), &mapped, diagnostics, artifact_id);
}

fn validate_endpoint(
    link: &std::collections::BTreeMap<String, JsonValue>,
    field: &str,
    diagnostics: &mut Vec<Diagnostic>,
    artifact_id: Option<&str>,
) -> Option<String> {
    let path = format!("payload.{field}");
    let Some(endpoint) = link.get(field).and_then(as_object) else {
        diagnostics.push(diagnostic(
            LINK_CODE,
            format!("{field} must be an object"),
            path,
            artifact_id,
        ));
        return None;
    };
    reject_unknown_fields(
        endpoint,
        &["packId", "version"],
        &path,
        diagnostics,
        artifact_id,
        LINK_CODE,
    );
    let pack_id = required_string(
        endpoint,
        "packId",
        format!("{path}.packId"),
        diagnostics,
        artifact_id,
        LINK_CODE,
    );
    if let Some(version) = required_string(
        endpoint,
        "version",
        format!("{path}.version"),
        diagnostics,
        artifact_id,
        LINK_CODE,
    ) && !valid_semantic_version(&version)
    {
        diagnostics.push(diagnostic(
            LINK_CODE,
            "a linked pack is pinned to an exact semantic version",
            format!("{path}.version"),
            artifact_id,
        ));
    }
    pack_id
}

/// Returns the correspondence id and its source term when both are present.
fn validate_correspondence(
    item: &JsonValue,
    path: &str,
    diagnostics: &mut Vec<Diagnostic>,
    artifact_id: Option<&str>,
) -> Option<(String, String)> {
    let Some(item) = as_object(item) else {
        diagnostics.push(diagnostic(
            LINK_CODE,
            "correspondence must be an object",
            path,
            artifact_id,
        ));
        return None;
    };
    let fields = [
        "correspondenceId",
        "fromTermId",
        "relation",
        "toTermIds",
        "justification",
        "evidenceRefs",
        "confidenceBasisPoints",
        "reviewedBy",
        "reviewedAt",
        "transformRef",
        "rulingRef",
    ];
    reject_unknown_fields(item, &fields, path, diagnostics, artifact_id, LINK_CODE);
    let required = |field: &str, diagnostics: &mut Vec<Diagnostic>| {
        required_string(
            item,
            field,
            format!("{path}.{field}"),
            diagnostics,
            artifact_id,
            LINK_CODE,
        )
    };
    let id = required("correspondenceId", diagnostics);
    let from_term = required("fromTermId", diagnostics);
    required("justification", diagnostics);
    required("reviewedBy", diagnostics);
    if let Some(at) = required("reviewedAt", diagnostics)
        && !valid_utc_date_time(&at)
    {
        diagnostics.push(diagnostic(
            LINK_CODE,
            "reviewedAt must be a UTC RFC 3339 timestamp",
            format!("{path}.reviewedAt"),
            artifact_id,
        ));
    }
    let relation = require_literal(
        item,
        "relation",
        &RELATIONS,
        format!("{path}.relation"),
        diagnostics,
        artifact_id,
        LINK_CODE,
    );
    let targets = string_list(
        item.get("toTermIds"),
        &format!("{path}.toTermIds"),
        diagnostics,
        artifact_id,
    );
    if relation
        .as_deref()
        .is_some_and(|relation| relation != "derived")
        && targets.len() > 1
    {
        diagnostics.push(diagnostic(
            LINK_CODE,
            "only a derived correspondence has more than one target term",
            format!("{path}.toTermIds"),
            artifact_id,
        ));
    }
    if item.contains_key("transformRef") && relation.as_deref() != Some("derived") {
        diagnostics.push(diagnostic(
            LINK_CODE,
            "only a derived correspondence names an executable transform",
            format!("{path}.transformRef"),
            artifact_id,
        ));
    }
    let evidence = string_list(
        item.get("evidenceRefs"),
        &format!("{path}.evidenceRefs"),
        diagnostics,
        artifact_id,
    );
    for (index, reference) in evidence.iter().enumerate() {
        if !(valid_prefixed_id(reference, "source.") || is_absolute_web_iri(reference)) {
            diagnostics.push(diagnostic(
                LINK_CODE,
                "evidence is a source.* id of the from-pack manifest or an absolute HTTPS IRI",
                format!("{path}.evidenceRefs[{index}]"),
                artifact_id,
            ));
        }
    }
    if !evidence
        .iter()
        .any(|reference| reference.starts_with("source."))
    {
        diagnostics.push(diagnostic(
            LINK_CODE,
            "a correspondence cites at least one source of the from-pack manifest",
            format!("{path}.evidenceRefs"),
            artifact_id,
        ));
    }
    match item.get("confidenceBasisPoints") {
        Some(JsonValue::Number(value))
            if value.is_finite() && value.fract() == 0.0 && (0.0..=10_000.0).contains(value) => {}
        _ => diagnostics.push(diagnostic(
            LINK_CODE,
            "confidenceBasisPoints must be an integer from 0 through 10000",
            format!("{path}.confidenceBasisPoints"),
            artifact_id,
        )),
    }
    for field in ["transformRef", "rulingRef"] {
        if item.contains_key(field) {
            required(field, diagnostics);
        }
    }
    id.zip(from_term)
}

fn validate_residuals(
    value: Option<&JsonValue>,
    mapped: &BTreeSet<String>,
    diagnostics: &mut Vec<Diagnostic>,
    artifact_id: Option<&str>,
) {
    let Some(value) = value else { return };
    let Some(items) = non_empty_array(
        Some(value),
        "payload.residuals",
        diagnostics,
        artifact_id,
        LINK_CODE,
    ) else {
        return;
    };
    let mut seen = BTreeSet::new();
    for (index, item) in items.iter().enumerate() {
        let path = format!("payload.residuals[{index}]");
        let Some(item) = as_object(item) else {
            diagnostics.push(diagnostic(
                LINK_CODE,
                "residual must be an object",
                path,
                artifact_id,
            ));
            continue;
        };
        reject_unknown_fields(
            item,
            &["termId", "reason", "note"],
            &path,
            diagnostics,
            artifact_id,
            LINK_CODE,
        );
        require_literal(
            item,
            "reason",
            &RESIDUAL_REASONS,
            format!("{path}.reason"),
            diagnostics,
            artifact_id,
            LINK_CODE,
        );
        required_string(
            item,
            "note",
            format!("{path}.note"),
            diagnostics,
            artifact_id,
            LINK_CODE,
        );
        if let Some(term) = required_string(
            item,
            "termId",
            format!("{path}.termId"),
            diagnostics,
            artifact_id,
            LINK_CODE,
        ) {
            if mapped.contains(&term) {
                diagnostics.push(diagnostic(
                    LINK_CODE,
                    format!("term '{term}' has a correspondence and cannot also be residual"),
                    format!("{path}.termId"),
                    artifact_id,
                ));
            }
            if !seen.insert(term) {
                diagnostics.push(diagnostic(
                    LINK_CODE,
                    "a term is residual at most once",
                    format!("{path}.termId"),
                    artifact_id,
                ));
            }
        }
    }
}

/// A non-empty array of non-empty, distinct strings.
fn string_list(
    value: Option<&JsonValue>,
    path: &str,
    diagnostics: &mut Vec<Diagnostic>,
    artifact_id: Option<&str>,
) -> Vec<String> {
    let Some(items) = non_empty_array(value, path, diagnostics, artifact_id, LINK_CODE) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for (index, item) in items.iter().enumerate() {
        match item {
            JsonValue::String(text) if !text.trim().is_empty() && !out.contains(text) => {
                out.push(text.clone())
            }
            _ => diagnostics.push(diagnostic(
                LINK_CODE,
                "entries must be distinct non-empty strings",
                format!("{path}[{index}]"),
                artifact_id,
            )),
        }
    }
    out
}
