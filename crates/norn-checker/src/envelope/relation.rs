//! Relation artifact payload validation.

use norn_ir::identifier_limits::{MAX_LINK_NAME_BYTES, MAX_OBJECT_TYPE_NAME_BYTES};
use norn_ir::{Diagnostic, JsonValue};

use super::helpers::{
    expect_object, identifier_too_long, kind_invalid, reject_unknown_object_fields,
    require_non_empty_field,
};

pub(super) fn validate_relation_payload(
    value: &JsonValue,
    diagnostics: &mut Vec<Diagnostic>,
    artifact_id: Option<&str>,
    relation_name: &str,
) {
    const CODE: &str = "relation.validation.invalid_payload";
    if relation_name.len() > MAX_LINK_NAME_BYTES {
        diagnostics.push(identifier_too_long(
            "relation name",
            relation_name.len(),
            MAX_LINK_NAME_BYTES,
            "name",
            artifact_id,
        ));
    }
    let Some(payload) = expect_object(value, "payload", diagnostics, artifact_id, CODE) else {
        return;
    };
    reject_unknown_object_fields(
        payload,
        &["from", "to", "cardinality"],
        "payload",
        diagnostics,
        artifact_id,
        CODE,
    );

    // Canonical RelationIR v0: `from`/`to` name the source and target entity kinds
    // (entity-kind references such as "retail.Item"), and both are required. The relation's
    // kind is its artifact identity (id/name), not a payload field — mirrors the reset-era
    // retail pack, the only concrete RelationIR producer.
    if let Some(source) = require_non_empty_field(
        payload,
        "from",
        "payload.from",
        diagnostics,
        artifact_id,
        CODE,
    ) && source.len() > MAX_OBJECT_TYPE_NAME_BYTES
    {
        diagnostics.push(identifier_too_long(
            "source object type",
            source.len(),
            MAX_OBJECT_TYPE_NAME_BYTES,
            "payload.from",
            artifact_id,
        ));
    }
    if let Some(target) =
        require_non_empty_field(payload, "to", "payload.to", diagnostics, artifact_id, CODE)
        && target.len() > MAX_OBJECT_TYPE_NAME_BYTES
    {
        diagnostics.push(identifier_too_long(
            "target object type",
            target.len(),
            MAX_OBJECT_TYPE_NAME_BYTES,
            "payload.to",
            artifact_id,
        ));
    }

    // Optional `cardinality`: when present it must name a known relation cardinality.
    if let Some(cardinality) = payload.get("cardinality") {
        match cardinality.as_string() {
            Some("one_to_one" | "one_to_many" | "many_to_one" | "many_to_many") => {}
            _ => {
                diagnostics.push(kind_invalid(
                    "payload.cardinality must be one of one_to_one, one_to_many, many_to_one, many_to_many",
                    "payload.cardinality",
                    artifact_id,
                    CODE,
                ));
            }
        }
    }
}
