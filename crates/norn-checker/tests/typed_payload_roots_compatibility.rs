#![allow(clippy::expect_used, missing_docs, reason = "integration-test crate")]

use norn_checker::ArtifactValidation;
use norn_ir::{ArtifactEnvelope, JsonValue};
use serde_json::json;

#[test]
fn setting_json_value_is_an_existing_wire_compatible_payload_surface() {
    let source = json!({
        "irVersion": "0.1",
        "artifactId": "commerce.setting.notification_preferences",
        "kind": "setting",
        "namespace": "commerce",
        "name": "Notification preferences",
        "version": "0.1.0",
        "lifecycle": "draft",
        "scope": { "orgId": "org_demo" },
        "dependencies": [],
        "provenance": { "source": "authoring_sdk" },
        "payload": {
            "settingVersion": "0.1",
            "settingKey": "commerce.notification_preferences",
            "valueType": "json",
            "value": {
                "channels": ["email", "push"],
                "digest": { "enabled": true, "hour": 9 },
                "quietHours": null
            },
            "effectiveFrom": "2026-08-27T00:00:00Z",
            "review": { "state": "draft" },
            "scopeRequired": ["orgId"]
        }
    });

    let artifact: ArtifactEnvelope =
        serde_json::from_value(source.clone()).expect("setting fixture should deserialize");

    assert!(artifact.validate().is_ok());
    let expected: JsonValue =
        serde_json::from_value(source).expect("setting fixture should encode as canonical IR JSON");
    assert_eq!(
        serde_json::to_value(artifact).expect("setting fixture should serialize"),
        serde_json::to_value(expected).expect("canonical setting fixture should serialize")
    );
}

#[test]
fn entity_payload_admits_engine_generated_overlay_sources() {
    // `bac.rs` appends `overlaySources` to entity payloads when applying `add_field`
    // customizations; the closed root must admit it or overlays fail revalidation.
    let source = serde_json::json!({
        "irVersion": "0.1",
        "artifactId": "entity.test.overlay",
        "kind": "entity",
        "namespace": "test",
        "name": "Overlay",
        "version": "0.1.0",
        "lifecycle": "draft",
        "scope": { "orgId": "org-1" },
        "dependencies": [],
        "provenance": { "source": "system" },
        "payload": {
            "objectKind": "Overlay",
            "fields": { "id": { "type": "string", "required": true } },
            "overlaySources": ["customization.test.add_field"]
        }
    });
    let artifact: ArtifactEnvelope =
        serde_json::from_value(source.clone()).expect("overlay fixture should deserialize");
    assert!(
        artifact.validate().is_ok(),
        "{:?}",
        artifact.validate().err()
    );
    assert_eq!(
        serde_json::to_value(artifact).expect("overlay fixture should serialize"),
        source
    );
}

#[test]
fn unknown_payload_field_kept_by_the_decoder_is_a_checker_diagnostic() {
    // Counterpart of norn-ir's
    // `envelope_kind_is_the_payload_tag_and_mismatched_roots_cannot_be_encoded_by_kind`,
    // whose `validate()` assertion moved here with the validator (T1-CHK-1).
    let envelope: ArtifactEnvelope = serde_json::from_value(json!({
        "irVersion": "0.1",
        "artifactId": "test.relation",
        "kind": "relation",
        "namespace": "test",
        "name": "Relation",
        "version": "0.1.0",
        "lifecycle": "draft",
        "scope": {},
        "dependencies": [],
        "provenance": { "source": "system" },
        "payload": { "from": "A", "to": "B", "actionId": "x" }
    }))
    .expect("envelope should deserialize");
    assert!(
        norn_checker::validate_artifact(&envelope).is_err(),
        "unknown payload field stays a diagnostic"
    );
}
