#![allow(
    clippy::expect_used,
    clippy::needless_pass_by_value,
    missing_docs,
    reason = "integration-test crate"
)]

use norn_checker::ArtifactValidation;
use norn_ir::{
    ArtifactEnvelope, ArtifactLifecycle, ArtifactPayload, ArtifactProvenance, ProvenanceSource,
    SUPPORTED_IR_VERSION, ScopeRef,
};

fn action_artifact(effect_program: serde_json::Value) -> ArtifactEnvelope {
    ArtifactEnvelope {
        ir_version: SUPPORTED_IR_VERSION.to_string(),
        artifact_id: "custom.record.upsert".to_string(),
        namespace: "custom".to_string(),
        name: "UpsertCustomRecord".to_string(),
        version: "1.0.0".to_string(),
        lifecycle: ArtifactLifecycle::Proposed,
        scope: ScopeRef {
            org_id: Some("org_demo".to_string()),
            tenant_id: None,
            workspace_id: Some("workspace_demo".to_string()),
            environment_id: Some("test".to_string()),
            business_unit_id: None,
        },
        dependencies: Vec::new(),
        provenance: ArtifactProvenance {
            source: ProvenanceSource::AuthoringSdk,
            source_path: None,
            actor_id: Some("author_1".to_string()),
            created_at: None,
        },
        payload: ArtifactPayload::Action(
            serde_json::from_value(serde_json::json!({
                "actionId": "custom.record.upsert",
                "scopeRequired": ["orgId", "workspaceId", "environmentId"],
                "idempotent": true,
                "ledgered": true,
                "inputSchema": {
                    "type": "object",
                    "required": ["objectId", "value"],
                    "properties": {
                        "objectId": { "type": "string" },
                        "value": { "type": "integer" }
                    },
                    "additionalProperties": false
                },
                "outputSchema": {
                    "type": "object",
                    "properties": {},
                    "additionalProperties": false
                },
                "effectProgram": effect_program
            }))
            .expect("Action payload should convert to Norn JSON"),
        ),
    }
}

fn action_artifact_with_ui_effects(ui_effects: serde_json::Value) -> ArtifactEnvelope {
    let mut artifact = action_artifact(serde_json::json!({
        "kind": "upsert_input_object",
        "objectIdField": "objectId",
        "objectKind": "custom.Record",
        "entityArtifactId": "custom.entity.record",
        "entityArtifactVersion": "1.0.0"
    }));
    let mut payload = serde_json::to_value(&artifact.payload).expect("payload should encode");
    payload
        .get_mut("payload")
        .and_then(serde_json::Value::as_object_mut)
        .expect("action payload should be an object")
        .insert("uiEffects".to_owned(), ui_effects);
    artifact.payload = serde_json::from_value(payload).expect("payload should decode");
    artifact
}

#[test]
fn action_payload_and_schemas_are_closed_before_promotion() {
    let mut unknown_payload = action_artifact(serde_json::json!({
        "kind": "upsert_input_object",
        "objectIdField": "objectId",
        "objectKind": "custom.Record",
        "entityArtifactId": "custom.entity.record",
        "entityArtifactVersion": "1.0.0"
    }));
    let mut payload =
        serde_json::to_value(&unknown_payload.payload).expect("payload should encode");
    payload["payload"]["unverifiedEffect"] = serde_json::json!(true);
    unknown_payload.payload = serde_json::from_value(payload).expect("payload should decode");
    let diagnostics = unknown_payload
        .validate()
        .expect_err("unknown action payload fields must fail closed");
    assert!(diagnostics.iter().any(|diagnostic| {
        diagnostic.code == "action.validation.invalid_payload"
            && diagnostic.path.as_deref() == Some("payload.unverifiedEffect")
    }));

    let mut ignored_keyword = action_artifact(serde_json::json!({
        "kind": "upsert_input_object",
        "objectIdField": "objectId",
        "objectKind": "custom.Record",
        "entityArtifactId": "custom.entity.record",
        "entityArtifactVersion": "1.0.0"
    }));
    let mut payload =
        serde_json::to_value(&ignored_keyword.payload).expect("payload should encode");
    payload["payload"]["inputSchema"]["properties"]["value"]["minimum"] = serde_json::json!(0);
    ignored_keyword.payload = serde_json::from_value(payload).expect("payload should decode");
    let diagnostics = ignored_keyword
        .validate()
        .expect_err("uninterpreted schema keywords must fail closed");
    assert!(diagnostics.iter().any(|diagnostic| {
        diagnostic.code == "action.validation.invalid_schema"
            && diagnostic.path.as_deref() == Some("payload.inputSchema.properties.value.minimum")
    }));
}

#[test]
fn closed_upsert_input_object_program_is_admitted() {
    let artifact = action_artifact(serde_json::json!({
        "kind": "upsert_input_object",
        "objectIdField": "objectId",
        "objectKind": "custom.Record",
        "entityArtifactId": "custom.entity.record",
        "entityArtifactVersion": "1.0.0"
    }));

    assert!(artifact.validate().is_ok());
}

#[test]
fn create_only_upsert_can_omit_object_id_but_default_upsert_cannot() {
    let omit_object_id = |artifact: &mut norn_ir::ArtifactEnvelope| {
        let mut payload = serde_json::to_value(&artifact.payload).expect("payload encodes");
        payload["payload"]["inputSchema"]["required"] = serde_json::json!(["value"]);
        artifact.payload = serde_json::from_value(payload).expect("payload decodes");
    };

    let mut create_only = action_artifact(serde_json::json!({
        "kind": "upsert_input_object",
        "objectIdField": "objectId",
        "objectKind": "custom.Record",
        "entityArtifactId": "custom.entity.record",
        "entityArtifactVersion": "1.0.0",
        "writeMode": { "kind": "create_only" }
    }));
    omit_object_id(&mut create_only);
    assert!(create_only.validate().is_ok());

    let mut upsert = action_artifact(serde_json::json!({
        "kind": "upsert_input_object",
        "objectIdField": "objectId",
        "objectKind": "custom.Record",
        "entityArtifactId": "custom.entity.record",
        "entityArtifactVersion": "1.0.0"
    }));
    omit_object_id(&mut upsert);
    let diagnostics = upsert
        .validate()
        .expect_err("an ordinary upsert still requires its object id");
    assert!(diagnostics.iter().any(|diagnostic| {
        diagnostic.code == "action.validation.invalid_effect_program"
            && diagnostic.path.as_deref() == Some("payload.effectProgram.objectIdField")
    }));
}

#[test]
fn upsert_write_modes_are_closed_and_draft_mode_is_well_formed() {
    for write_mode in [
        serde_json::json!("create_only"),
        serde_json::json!({"kind": "create_only", "unknown": true}),
        serde_json::json!({"kind": "draft_only", "lifecycle": "status"}),
        serde_json::json!({"kind": "draft_only", "lifecycle": "Bad Status", "state": "draft"}),
    ] {
        let artifact = action_artifact(serde_json::json!({
            "kind": "upsert_input_object",
            "objectIdField": "objectId",
            "objectKind": "custom.Record",
            "entityArtifactId": "custom.entity.record",
            "entityArtifactVersion": "1.0.0",
            "writeMode": write_mode
        }));
        let diagnostics = artifact
            .validate()
            .expect_err("malformed upsert write modes must fail closed");
        assert!(
            diagnostics.iter().any(|diagnostic| {
                diagnostic.code == "action.validation.invalid_effect_program"
                    && diagnostic
                        .path
                        .as_deref()
                        .is_some_and(|path| path.starts_with("payload.effectProgram.writeMode"))
            }),
            "{diagnostics:?}"
        );
    }

    let artifact = action_artifact(serde_json::json!({
        "kind": "upsert_input_object",
        "objectIdField": "objectId",
        "objectKind": "custom.Record",
        "entityArtifactId": "custom.entity.record",
        "entityArtifactVersion": "1.0.0",
        "writeMode": {"kind": "draft_only", "lifecycle": "status", "state": "draft"}
    }));
    assert!(artifact.validate().is_ok());
}

#[test]
fn versioned_ui_effect_declarations_are_admitted_and_closed() {
    let artifact = action_artifact_with_ui_effects(serde_json::json!({
        "effectsVersion": "0.1",
        "irreversible": true,
        "destructive": true,
        "money": ["currency.USD"],
        "errorCodes": ["payment.declined"],
        "resultCodes": ["payment.posted"]
    }));
    assert!(artifact.validate().is_ok());

    for (value, path) in [
        (serde_json::Value::Null, "payload.uiEffects"),
        (serde_json::json!("not-an-object"), "payload.uiEffects"),
        (
            serde_json::json!({ "irreversible": true }),
            "payload.uiEffects.effectsVersion",
        ),
        (
            serde_json::json!({ "effectsVersion": "9.9" }),
            "payload.uiEffects.effectsVersion",
        ),
        (
            serde_json::json!({ "effectsVersion": "0.1", "unknown": true }),
            "payload.uiEffects.unknown",
        ),
        (
            serde_json::json!({ "effectsVersion": "0.1", "irreversible": "yes" }),
            "payload.uiEffects.irreversible",
        ),
        (
            serde_json::json!({
                "effectsVersion": "0.1",
                "money": ["currency.USD", "currency.USD"]
            }),
            "payload.uiEffects.money[1]",
        ),
        (
            serde_json::json!({ "effectsVersion": "0.1", "money": ["currency. USD"] }),
            "payload.uiEffects.money[0]",
        ),
        (
            serde_json::json!({
                "effectsVersion": "0.1",
                "errorCodes": ["payment.declined", "payment.declined"]
            }),
            "payload.uiEffects.errorCodes[1]",
        ),
        (
            serde_json::json!({ "effectsVersion": "0.1", "resultCodes": ["*"] }),
            "payload.uiEffects.resultCodes[0]",
        ),
        (
            serde_json::json!({
                "effectsVersion": "0.1",
                "errorCodes": [" payment.declined"]
            }),
            "payload.uiEffects.errorCodes[0]",
        ),
    ] {
        let diagnostics = action_artifact_with_ui_effects(value)
            .validate()
            .expect_err("invalid UI effect declarations must fail admission");
        assert!(
            diagnostics.iter().any(|diagnostic| {
                diagnostic.code == "action.validation.invalid_payload"
                    && diagnostic.path.as_deref() == Some(path)
            }),
            "expected diagnostic at {path}: {diagnostics:?}"
        );
    }
}

#[test]
fn effect_program_rejects_unknown_operations_fields_and_unbound_object_id() {
    let unknown_operation = action_artifact(serde_json::json!({
        "kind": "run_sql",
        "objectIdField": "missingId",
        "objectKind": "custom.Record",
        "entityArtifactId": "custom.entity.record",
        "entityArtifactVersion": "1.0.0",
        "sql": "UPDATE authority"
    }));
    let diagnostics = unknown_operation
        .validate()
        .expect_err("unknown authority operations must fail closed");

    assert!(diagnostics.iter().any(|diagnostic| {
        diagnostic.code == "action.validation.invalid_effect_program"
            && diagnostic.path.as_deref() == Some("payload.effectProgram.sql")
    }));
    assert!(diagnostics.iter().any(|diagnostic| {
        diagnostic.code == "action.validation.invalid_effect_program"
            && diagnostic.path.as_deref() == Some("payload.effectProgram.objectIdField")
    }));
}

/// T3-LIFE-3 (#2867): the closed `fire_transition` effect.
#[test]
fn closed_fire_transition_program_is_admitted() {
    let artifact = action_artifact(serde_json::json!({
        "kind": "fire_transition",
        "objectIdField": "objectId",
        "objectKind": "custom.Record",
        "entityArtifactId": "custom.entity.record",
        "entityArtifactVersion": "1.0.0",
        "lifecycle": "status",
        "transition": "approve"
    }));

    assert!(artifact.validate().is_ok());
}

#[test]
fn fire_transition_names_its_lifecycle_and_transition_by_member_id() {
    let diagnostics = action_artifact(serde_json::json!({
        "kind": "fire_transition",
        "objectIdField": "objectId",
        "objectKind": "custom.Record",
        "entityArtifactId": "custom.entity.record",
        "entityArtifactVersion": "1.0.0",
        "transition": "Approve Now"
    }))
    .validate()
    .expect_err("a transition effect without a lifecycle, or with a malformed id, fails closed");
    for path in [
        "payload.effectProgram.lifecycle",
        "payload.effectProgram.transition",
    ] {
        assert!(
            diagnostics.iter().any(|diagnostic| {
                diagnostic.code == "action.validation.invalid_effect_program"
                    && diagnostic.path.as_deref() == Some(path)
            }),
            "{path}: {diagnostics:?}"
        );
    }

    // The transition members belong to `fire_transition` only.
    let diagnostics = action_artifact(serde_json::json!({
        "kind": "upsert_input_object",
        "objectIdField": "objectId",
        "objectKind": "custom.Record",
        "entityArtifactId": "custom.entity.record",
        "entityArtifactVersion": "1.0.0",
        "transition": "approve"
    }))
    .validate()
    .expect_err("an upsert carries no transition");
    assert!(diagnostics.iter().any(|diagnostic| {
        diagnostic.code == "action.validation.invalid_effect_program"
            && diagnostic.path.as_deref() == Some("payload.effectProgram.transition")
    }));
}
