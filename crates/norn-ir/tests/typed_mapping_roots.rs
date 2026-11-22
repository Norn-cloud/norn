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
        members: Vec::new(),
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
    assert_eq!(root.unknown_fields().len(), 0);
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
fn map_roles_schema_requires_each_closed_forms_own_contract() {
    let schema = serde_json::to_value(norn_ir::artifact_payload_schema(&ArtifactKind::Mapping))
        .expect("mapping schema");
    let forms = schema["oneOf"].as_array().expect("exclusive mapping forms");
    assert_eq!(forms.len(), 2);
    assert_eq!(forms[0]["$ref"], "#/$defs/MappingIr");
    assert_eq!(forms[1]["$ref"], "#/$defs/LawInterpretationIr");
    for (form, required) in [
        ("MappingIr", vec!["format", "source", "interfaces", "maps"]),
        (
            "LawInterpretationIr",
            vec!["format", "role", "sourceLayer", "targetLayer", "members"],
        ),
    ] {
        assert_eq!(schema["$defs"][form]["required"], json!(required));
        assert_eq!(schema["$defs"][form]["additionalProperties"], false);
    }
    for (form, format) in [
        ("MappingFormat", "norn.mapping/1"),
        ("InterpretationFormat", "norn.interpretation/1"),
    ] {
        assert_eq!(schema["$defs"][form]["oneOf"][0]["const"], format);
    }
}

fn member_binding(role: &str) -> norn_ir::mapping::MemberInterpretation {
    use norn_ir::definition_catalog::{Member, MemberRef};
    let at = |artifact: &str, name: &str| MemberRef {
        artifact_id: artifact.to_owned(),
        version: "1.0.0".to_owned(),
        member: Member::Link {
            type_name: name.to_owned(),
            link: role.to_owned(),
        },
    };
    norn_ir::mapping::MemberInterpretation {
        source: at("toy.events", "toy.Event"),
        target: at("toy.receipts", "toy.Receipt"),
        transform: None,
    }
}

#[test]
fn event_multi_member_contract_keeps_both_object_roles() {
    let mut document = interpretation();
    document.members = vec![member_binding("actor"), member_binding("resource")];
    let wire = serde_json::to_value(&document).expect("wire");
    let decoded: LawInterpretationIr = serde_json::from_value(wire).expect("typed members");
    assert_eq!(decoded, document);
    assert_ne!(decoded.members[0].source, decoded.members[1].source);
    assert_ne!(decoded.members[0].target, decoded.members[1].target);
}

#[test]
fn map_members_contract_refuses_missing_identity_and_unknown_fields() {
    let mut document = interpretation();
    document.members.push(member_binding("resource"));
    let wire = serde_json::to_value(document).expect("wire");
    for field in ["artifactId", "version", "member"] {
        let mut invalid = wire.clone();
        invalid["members"][0]["source"]
            .as_object_mut()
            .expect("source")
            .remove(field);
        assert!(serde_json::from_value::<LawInterpretationIr>(invalid).is_err());
    }
    for (field, value) in [("requiredMembers", json!([])), ("transform", json!(null))] {
        let mut invalid = wire.clone();
        invalid["members"][0][field] = value;
        assert!(serde_json::from_value::<LawInterpretationIr>(invalid).is_err());
    }
}

proptest::proptest! {
    #[test]
    fn map_member_contract_generated_identities_survive_payload_roots(
        roles in proptest::collection::vec("[a-z][a-z0-9]{0,12}", 0..24),
        transition in proptest::bool::ANY,
    ) {
        let mut document = interpretation();
        document.members = roles.iter().map(|role| {
            let mut binding = member_binding(role);
            if transition {
                binding.source.member = norn_ir::definition_catalog::Member::Transition {
                    type_name: "toy.Event".to_owned(), lifecycle: "status".to_owned(),
                    transition: role.clone(),
                };
            }
            binding
        }).collect();
        let wire = serde_json::to_value(&document).expect("wire");
        let root = ArtifactPayload::from_kind_json(
            &ArtifactKind::Mapping, serde_json::from_value(wire).expect("JSON tree"),
        ).expect("mapping root");
        proptest::prop_assert!(root.unknown_fields().is_empty());
        let decoded: LawInterpretationIr = serde_json::from_value(
            serde_json::to_value(root.to_json_value()).expect("wire JSON"),
        ).expect("strict document");
        proptest::prop_assert_eq!(decoded, document);
    }
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
    assert_eq!(envelope.payload.unknown_fields().len(), 0);
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
