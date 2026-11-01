#![allow(
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    missing_docs,
    reason = "integration-test crate"
)]

use norn_ir::{
    ArtifactEnvelope, ArtifactKind, ArtifactPayload,
    mapping::{
        InterpretationEndpoint, InterpretationFormat, InterpretationRole, LawInterpretationIr,
        MappingDocument, MappingIr,
    },
};
use serde_json::json;

fn interpretation() -> LawInterpretationIr {
    let endpoint = |id: &str| InterpretationEndpoint {
        artifact_id: id.to_owned(),
        version: "1.0.0".to_owned(),
        sha256: "a".repeat(64),
    };
    LawInterpretationIr {
        format: InterpretationFormat::V1,
        role: InterpretationRole::LawInterpretation,
        source_layer: endpoint("toy.capacity"),
        target_layer: endpoint("toy.credit"),
    }
}

#[test]
fn map_roles_interpretation_survives_closed_root_without_becoming_source_records() {
    let document = interpretation();
    let wire = serde_json::to_value(&document).expect("typed interpretation serializes");
    let root = ArtifactPayload::from_kind_json(
        &ArtifactKind::Mapping,
        serde_json::from_value(wire.clone()).expect("JSON tree"),
    )
    .expect("mapping root");
    assert!(root.unknown_fields().is_empty());
    assert_eq!(
        serde_json::to_value(root.to_json_value()).expect("wire JSON"),
        wire
    );
    assert_eq!(
        serde_json::from_value::<MappingDocument>(wire.clone()).expect("strict document"),
        MappingDocument::LawInterpretation(document)
    );
    assert!(serde_json::from_value::<MappingIr>(wire).is_err());
}

#[test]
fn map_roles_and_pins_decode_strictly() {
    let wire = serde_json::to_value(interpretation()).expect("typed interpretation serializes");
    for role in ["source_record", "term_correspondence"] {
        let mut invalid = wire.clone();
        invalid["role"] = json!(role);
        assert!(serde_json::from_value::<MappingDocument>(invalid).is_err());
    }
    for field in ["artifactId", "version", "sha256"] {
        let mut invalid = wire.clone();
        invalid["sourceLayer"]
            .as_object_mut()
            .expect("endpoint")
            .remove(field);
        assert!(serde_json::from_value::<MappingDocument>(invalid).is_err());
    }
    let mut invalid = wire;
    invalid["targetLayer"]["inferredNamespace"] = json!("toy");
    assert!(serde_json::from_value::<MappingDocument>(invalid).is_err());
}

#[test]
fn mapping_entitlements_and_identity_bindings_roundtrip_through_envelope_root() {
    let wire = json!({
        "irVersion": "0.1",
        "artifactId": "erpnext.norn_core",
        "kind": "mapping",
        "namespace": "erpnext",
        "name": "norn_core",
        "version": "0.2.0",
        "lifecycle": "active",
        "scope": {},
        "dependencies": [],
        "provenance": { "source": "system" },
        "payload": {
            "format": "norn.mapping/1",
            "source": {
                "schemaId": "erpnext.source_schema",
                "version": "0.2.0",
                "sha256": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
            },
            "interfaces": [],
            "maps": [],
            "entitlements": [{
                "id": "docperm",
                "sourceAlias": "permission",
                "source": { "dataset": "DocPerm" },
                "requiredSnapshot": ["DocPerm"],
                "targetType": "erpnext.SalesInvoice",
                "grantee": {
                    "kind": "role",
                    "objectType": "identity.Role",
                    "key": { "alias": "permission", "field": "role" }
                },
                "condition": { "kind": "none" },
                "sourceRecord": "name",
                "sourceVersion": "modified",
                "coverage": { "status": "exact", "reason": "direct role permission" },
                "status": "shadow"
            }],
            "identityBindings": [{
                "sourceKey": "Administrator",
                "objectType": "identity.Principal",
                "key": "principal-admin",
                "evidence": "reviewed mapping"
            }]
        }
    });

    let envelope: ArtifactEnvelope = serde_json::from_value(wire.clone())
        .expect("declared mapping members are retained by the closed payload root");
    assert!(envelope.payload.unknown_fields().is_empty());
    let payload =
        serde_json::to_value(envelope.payload.to_json_value()).expect("mapping payload serializes");
    assert_eq!(payload, wire["payload"]);
    assert_eq!(
        serde_json::to_value(envelope).expect("envelope serializes")["payload"],
        wire["payload"]
    );

    let mapping: MappingIr = serde_json::from_value(wire["payload"].clone())
        .expect("strict MappingIR decodes both new fields");
    assert_eq!(mapping.entitlements.len(), 1);
    assert_eq!(mapping.identity_bindings.len(), 1);
}
