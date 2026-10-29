#![allow(
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    missing_docs,
    reason = "integration-test crate"
)]

use norn_ir::{ArtifactEnvelope, mapping::MappingIr};
use serde_json::json;

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
