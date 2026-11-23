//! Business-pack, ontology-pack, link-pack, and composition-pack payload validation.

use std::collections::BTreeMap;

use norn_ir::{Diagnostic, JsonValue, PackArtifactKind};

use super::helpers::{
    expect_object, invalid_diagnostic, reject_unknown_object_fields, require_literal_field,
    require_non_empty_field, validate_string_array,
};
use crate::ontology;

pub(super) fn validate_business_pack_payload(
    value: &JsonValue,
    diagnostics: &mut Vec<Diagnostic>,
    artifact_id: Option<&str>,
) {
    const CODE: &str = "business_pack.validation.invalid_payload";
    let Some(payload) = expect_object(value, "payload", diagnostics, artifact_id, CODE) else {
        return;
    };

    require_non_empty_field(
        payload,
        "packId",
        "payload.packId",
        diagnostics,
        artifact_id,
        CODE,
    );
    require_non_empty_field(
        payload,
        "packVersion",
        "payload.packVersion",
        diagnostics,
        artifact_id,
        CODE,
    );

    // IR 0.1 composition packs predate the discriminator. Preserve their wire compatibility;
    // ontology packs remain explicit and all newly authored packs should emit `packKind`.
    let pack_kind = if payload.contains_key("packKind") {
        require_literal_field(
            payload,
            "packKind",
            &["composition", "ontology", "link"],
            "payload.packKind",
            diagnostics,
            artifact_id,
            CODE,
        )
    } else {
        Some("composition".to_string())
    };

    match pack_kind.as_deref() {
        Some("composition") => {
            reject_unknown_object_fields(
                payload,
                &[
                    "packId",
                    "packVersion",
                    "packKind",
                    "provides",
                    "artifacts",
                    "instances",
                ],
                "payload",
                diagnostics,
                artifact_id,
                CODE,
            );
            validate_composition_pack_payload(payload, diagnostics, artifact_id);
        }
        Some("ontology") => {
            ontology::validate_ontology_pack_payload(value, diagnostics, artifact_id);
        }
        Some("link") => ontology::validate_link_payload(value, diagnostics, artifact_id),
        _ => {}
    }
}

fn validate_composition_pack_payload(
    payload: &BTreeMap<String, JsonValue>,
    diagnostics: &mut Vec<Diagnostic>,
    artifact_id: Option<&str>,
) {
    const CODE: &str = "business_pack.validation.invalid_payload";

    if let Some(provides) = payload.get("provides") {
        validate_string_array(provides, "payload.provides", diagnostics, artifact_id, CODE);
    }

    match payload.get("artifacts") {
        Some(JsonValue::Array(artifacts)) if !artifacts.is_empty() => {
            for (index, artifact_ref) in artifacts.iter().enumerate() {
                let path = format!("payload.artifacts[{index}]");
                let Some(artifact_ref) =
                    expect_object(artifact_ref, &path, diagnostics, artifact_id, CODE)
                else {
                    continue;
                };
                require_non_empty_field(
                    artifact_ref,
                    "artifactId",
                    format!("{path}.artifactId"),
                    diagnostics,
                    artifact_id,
                    CODE,
                );
                match artifact_ref.get("kind") {
                    None => diagnostics.push(invalid_diagnostic(
                        CODE,
                        "kind is required",
                        format!("{path}.kind"),
                        artifact_id,
                    )),
                    Some(JsonValue::String(kind)) => {
                        if let Err(error) = serde_json::to_value(kind)
                            .and_then(serde_json::from_value::<PackArtifactKind>)
                        {
                            diagnostics.push(invalid_diagnostic(
                                CODE,
                                format!("{path}.kind {error}"),
                                format!("{path}.kind"),
                                artifact_id,
                            ));
                        }
                    }
                    Some(_) => diagnostics.push(invalid_diagnostic(
                        CODE,
                        "kind must be a string",
                        format!("{path}.kind"),
                        artifact_id,
                    )),
                }
                require_non_empty_field(
                    artifact_ref,
                    "version",
                    format!("{path}.version"),
                    diagnostics,
                    artifact_id,
                    CODE,
                );
            }
        }
        Some(JsonValue::Array(_)) => diagnostics.push(invalid_diagnostic(
            CODE,
            "artifacts must be a non-empty array",
            "payload.artifacts",
            artifact_id,
        )),
        Some(_) => diagnostics.push(invalid_diagnostic(
            CODE,
            "artifacts must be an array",
            "payload.artifacts",
            artifact_id,
        )),
        None => diagnostics.push(invalid_diagnostic(
            CODE,
            "artifacts is required",
            "payload.artifacts",
            artifact_id,
        )),
    }
}
