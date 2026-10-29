#![allow(
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    missing_docs,
    reason = "integration-test crate"
)]

//! Entity and `ActionIR` admission for owned ordered child collections (#3254).

use std::collections::BTreeSet;

use norn_checker::object_types::codes as object_codes;
use norn_checker::{
    EntityArtifactSetContext, EntityContext, EntityModel, check_artifact_set, check_entity_model,
};
use norn_ir::{ArtifactEnvelope, ObjectTypeDef, PropertyTypeName};
use serde_json::{Value, json};

const PARENT: &str = "test.Order";
const CHILD: &str = "test.OrderLine";
const COLLECTION: &str = "lines";
const PARENT_ARTIFACT: &str = "test.Order.entity";
const CHILD_ARTIFACT: &str = "test.OrderLine.entity";
const CHILD_BASE: &str = "test.OrderLineBase";
const CHILD_BASE_ARTIFACT: &str = "test.OrderLineBase.entity";
const ACTION_ARTIFACT: &str = "test.Order.edit.action";
const TARGET_NOT_WRITABLE: &str = "artifact_set.action.target_not_writable";
const DIRECT_LIFECYCLE_WRITE: &str = "artifact_set.lifecycle.direct_write";

fn parent_type(max_rows: u64, cardinality: &Value) -> Value {
    json!({
        "name": PARENT,
        "properties": [{ "id": "title", "type": { "kind": "text" }, "constraints": { "nullable": true } }],
        "links": [{
            "id": COLLECTION,
            "target": CHILD,
            "cardinality": cardinality
        }],
        "ownedCollections": [{
            "id": COLLECTION,
            "childType": CHILD,
            "rowIdProperty": "row_id",
            "orderingKey": "idx",
            "cascadeOnParentDelete": true,
            "maxRows": max_rows
        }]
    })
}

fn child_type(row_id_kind: &str, ordering_kind: &str) -> Value {
    json!({
        "name": CHILD,
        "properties": [
            { "id": "row_id", "type": { "kind": row_id_kind } },
            { "id": "idx", "type": { "kind": ordering_kind } },
            { "id": "label", "type": { "kind": "text" } },
            { "id": "quantity", "type": { "kind": "integer" } },
            { "id": "double_quantity", "type": { "kind": "integer" } }
        ]
    })
}

fn entity_model_findings(parent: Value, child: Value) -> Vec<(String, String)> {
    model_findings(vec![parent, child])
}

fn model_findings(definitions: Vec<Value>) -> Vec<(String, String)> {
    let object_types: Vec<ObjectTypeDef> = definitions
        .into_iter()
        .map(|definition| serde_json::from_value(definition).expect("object type decodes"))
        .collect();
    let interfaces = [];
    let extensions = [];
    let base = EntityModel {
        interfaces: &interfaces,
        object_types: &[],
        extensions: &extensions,
    };
    let model = EntityModel {
        interfaces: &interfaces,
        object_types: &object_types,
        extensions: &extensions,
    };
    let registry = BTreeSet::<PropertyTypeName>::new();
    let context = EntityContext {
        catalog: &[],
        registry: &registry,
        base,
        tenant: None,
    };
    check_entity_model(&model, &context)
        .into_iter()
        .map(|finding| {
            (
                finding.code.as_str().to_owned(),
                finding.span.map(|span| span.pointer).unwrap_or_default(),
            )
        })
        .collect()
}

fn envelope(
    artifact_id: &str,
    kind: &str,
    namespace: &str,
    name: &str,
    payload: &Value,
    dependencies: &Value,
) -> Value {
    json!({
        "irVersion": "0.1",
        "artifactId": artifact_id,
        "kind": kind,
        "namespace": namespace,
        "name": name,
        "version": "1.0.0",
        "lifecycle": "draft",
        "scope": { "orgId": "org_demo", "environmentId": "dev" },
        "dependencies": dependencies,
        "provenance": {
            "source": "authoring_sdk",
            "sourcePath": "tests/owned_collections.rs"
        },
        "payload": payload
    })
}

fn entity_artifact(
    artifact_id: &str,
    name: &str,
    object_type: &Value,
    dependencies: &Value,
) -> Value {
    let namespace = name.split('.').next().expect("qualified object name");
    let display_name = name.split('.').nth(1).expect("qualified object name");
    envelope(
        artifact_id,
        "entity",
        namespace,
        display_name,
        &json!({ "entityVersion": "2.0", "objectType": object_type }),
        dependencies,
    )
}

fn row_effect(config: &Value) -> Value {
    json!({
        "kind": "mutate_parent_with_rows",
        "objectIdField": "parentId",
        "objectKind": PARENT,
        "entityArtifactId": PARENT_ARTIFACT,
        "entityArtifactVersion": "1.0.0",
        "parentValuesField": "parentValues",
        "rowOpsField": "rowOps",
        "parentVersionField": "parentVersion",
        "collections": [config]
    })
}

fn valid_collection_action() -> Value {
    json!({
        "id": COLLECTION,
        "preconditions": ["self.quantity > 0"],
        "derive": { "double_quantity": "self.quantity * 2" }
    })
}

fn action_artifact(effect: &Value) -> Value {
    envelope(
        ACTION_ARTIFACT,
        "action",
        "test",
        "Edit order",
        &json!({
            "actionId": "test.order.edit",
            "scopeRequired": ["orgId", "environmentId"],
            "idempotent": true,
            "ledgered": true,
            "inputSchema": {
                "type": "object",
                "properties": {
                    "parentId": { "type": "string" },
                    "parentValues": { "type": ["null", "boolean", "number", "string", "array", "object"] },
                    "parentVersion": { "type": "string" },
                    "rowOps": { "type": ["null", "boolean", "number", "string", "array", "object"] }
                },
                "required": ["parentId", "parentValues", "parentVersion", "rowOps"],
                "additionalProperties": false
            },
            "outputSchema": {
                "type": "object",
                "properties": {},
                "additionalProperties": false
            },
            "writes": [
                { "entityKind": PARENT, "change": "created" },
                { "entityKind": PARENT, "change": "modified" },
                { "entityKind": CHILD, "change": "created" },
                { "entityKind": CHILD, "change": "modified" },
                { "entityKind": CHILD, "change": "deleted" }
            ],
            "effectProgram": effect
        }),
        &json!([
            { "artifactId": PARENT_ARTIFACT, "kind": "entity", "version": "1.0.0", "required": true },
            { "artifactId": CHILD_ARTIFACT, "kind": "entity", "version": "1.0.0", "required": true }
        ]),
    )
}

fn ordinary_child_action(
    effect: &Value,
    artifact_id: &str,
    action_id: &str,
    writes: Value,
) -> Value {
    let mut action = action_artifact(effect);
    action["artifactId"] = json!(artifact_id);
    action["name"] = json!(action_id);
    action["payload"]["actionId"] = json!(action_id);
    action["payload"]["inputSchema"] = json!({
        "type": "object",
        "properties": { "objectId": { "type": "string" } },
        "required": ["objectId"],
        "additionalProperties": false
    });
    action["payload"]["writes"] = writes;
    action
}

fn admission_findings(effect: &Value) -> Vec<(String, String)> {
    admission_diagnostics(effect)
        .into_iter()
        .map(|finding| {
            (
                finding.code.as_str().to_owned(),
                finding.span.map(|span| span.pointer).unwrap_or_default(),
            )
        })
        .collect()
}

fn admission_diagnostics(effect: &Value) -> Vec<norn_checker::Diagnostic> {
    let parent = entity_artifact(
        PARENT_ARTIFACT,
        PARENT,
        &parent_type(20, &json!({ "to": "many", "from": "one" })),
        &json!([
            { "artifactId": CHILD_ARTIFACT, "kind": "entity", "version": "1.0.0", "required": true }
        ]),
    );
    let child = entity_artifact(
        CHILD_ARTIFACT,
        CHILD,
        &child_type("text", "integer"),
        &json!([]),
    );
    admission_diagnostics_for_artifacts(vec![parent, child], action_artifact(effect))
}

fn admission_findings_for_artifacts(entities: Vec<Value>, action: Value) -> Vec<(String, String)> {
    admission_diagnostics_for_artifacts(entities, action)
        .into_iter()
        .map(|finding| {
            (
                finding.code.as_str().to_owned(),
                finding.span.map(|span| span.pointer).unwrap_or_default(),
            )
        })
        .collect()
}

fn admission_diagnostics_for_artifacts(
    entities: Vec<Value>,
    action: Value,
) -> Vec<norn_checker::Diagnostic> {
    let artifacts: Vec<ArtifactEnvelope> = entities
        .into_iter()
        .chain(std::iter::once(action))
        .map(|artifact| serde_json::from_value(artifact).expect("artifact envelope decodes"))
        .collect();
    let registry = BTreeSet::<PropertyTypeName>::new();
    check_artifact_set(
        &artifacts,
        &EntityArtifactSetContext {
            registry: &registry,
            base: &[],
            tenant: None,
        },
    )
}

fn assert_finding(findings: &[(String, String)], code: &str, pointer: &str) {
    assert!(
        findings
            .iter()
            .any(|(found_code, found_pointer)| found_code == code && found_pointer == pointer),
        "expected {code} at {pointer}; found {findings:?}"
    );
}

#[test]
fn positive_parent_and_rows_artifact_set_is_admitted() {
    let findings = admission_findings(&row_effect(&valid_collection_action()));
    assert!(
        findings.is_empty(),
        "valid parent-and-rows set: {findings:?}"
    );
}

#[test]
fn ordinary_upsert_and_transition_effects_cannot_target_owned_child_types() {
    let parent = entity_artifact(
        PARENT_ARTIFACT,
        PARENT,
        &parent_type(20, &json!({ "to": "many", "from": "one" })),
        &json!([{
            "artifactId": CHILD_ARTIFACT,
            "kind": "entity",
            "version": "1.0.0",
            "required": true
        }]),
    );
    let mut child_definition = child_type("text", "integer");
    child_definition["properties"]
        .as_array_mut()
        .expect("child properties")
        .push(json!({
            "id": "status",
            "type": { "kind": "enum", "values": [{ "value": "open" }, { "value": "closed" }] }
        }));
    child_definition["lifecycles"] = json!([{
        "property": "status",
        "initial": ["open"],
        "terminal": ["closed"],
        "transitions": [{ "id": "close", "from": ["open"], "to": "closed" }]
    }]);
    let child = entity_artifact(CHILD_ARTIFACT, CHILD, &child_definition, &json!([]));

    let cases = [
        (
            "test.OrderLine.upsert.action",
            "test.order_line.upsert",
            json!({
                "kind": "upsert_input_object",
                "objectIdField": "objectId",
                "objectKind": CHILD,
                "entityArtifactId": CHILD_ARTIFACT,
                "entityArtifactVersion": "1.0.0"
            }),
            json!([
                { "entityKind": CHILD, "change": "created" },
                { "entityKind": CHILD, "change": "modified" }
            ]),
        ),
        (
            "test.OrderLine.transition.action",
            "test.order_line.close",
            json!({
                "kind": "fire_transition",
                "objectIdField": "objectId",
                "objectKind": CHILD,
                "entityArtifactId": CHILD_ARTIFACT,
                "entityArtifactVersion": "1.0.0",
                "lifecycle": "status",
                "transition": "close"
            }),
            json!([{ "entityKind": CHILD, "change": "modified" }]),
        ),
    ];

    for (artifact_id, action_id, effect, writes) in cases {
        let action = ordinary_child_action(&effect, artifact_id, action_id, writes);
        let findings =
            admission_diagnostics_for_artifacts(vec![parent.clone(), child.clone()], action);
        assert!(
            findings.iter().any(|finding| {
                finding.code.as_str() == TARGET_NOT_WRITABLE
                    && finding
                        .span
                        .as_ref()
                        .is_some_and(|span| span.pointer == "/payload/effectProgram/objectKind")
                    && finding.message.contains("owned child type")
            }),
            "expected ordinary owned-child refusal for {action_id}; found {findings:?}"
        );
    }
}

#[test]
fn owned_collection_bounds_are_one_through_one_hundred() {
    for max_rows in [0, 101] {
        let findings = entity_model_findings(
            parent_type(max_rows, &json!({ "to": "many", "from": "one" })),
            child_type("text", "integer"),
        );
        assert_finding(
            &findings,
            object_codes::INVALID_OWNED_COLLECTION,
            "/objectTypes/0/ownedCollections/0/maxRows",
        );
    }
}

#[test]
fn row_identity_and_ordering_properties_must_have_the_declared_types() {
    let invalid_row_id = entity_model_findings(
        parent_type(20, &json!({ "to": "many", "from": "one" })),
        child_type("integer", "integer"),
    );
    assert_finding(
        &invalid_row_id,
        object_codes::INVALID_OWNED_COLLECTION,
        "/objectTypes/0/ownedCollections/0/rowIdProperty",
    );

    let invalid_order = entity_model_findings(
        parent_type(20, &json!({ "to": "many", "from": "one" })),
        child_type("text", "text"),
    );
    assert_finding(
        &invalid_order,
        object_codes::INVALID_OWNED_COLLECTION,
        "/objectTypes/0/ownedCollections/0/orderingKey",
    );
}

#[test]
fn ordering_constraints_must_admit_every_dense_position_through_max_rows() {
    for constraints in [json!({ "min": "10" }), json!({ "max": "19" })] {
        let mut child = child_type("text", "integer");
        child["properties"][1]["constraints"] = constraints;
        let findings = entity_model_findings(
            parent_type(20, &json!({ "to": "many", "from": "one" })),
            child,
        );
        assert_finding(
            &findings,
            object_codes::INVALID_OWNED_COLLECTION,
            "/objectTypes/0/ownedCollections/0/orderingKey",
        );
    }

    let mut child = child_type("text", "integer");
    child["properties"][1]["constraints"] = json!({ "min": "1", "max": "20" });
    let findings = entity_model_findings(
        parent_type(20, &json!({ "to": "many", "from": "one" })),
        child,
    );
    assert!(
        findings.is_empty(),
        "bounds containing every dense row position: {findings:?}"
    );
}

#[test]
fn owned_collection_requires_a_many_from_one_link_to_its_child_type() {
    let findings = entity_model_findings(
        parent_type(20, &json!({ "to": "one", "from": "many" })),
        child_type("text", "integer"),
    );
    assert_finding(
        &findings,
        object_codes::INVALID_OWNED_COLLECTION,
        "/objectTypes/0/ownedCollections/0/id",
    );
}

#[test]
fn row_preconditions_are_checked_in_the_child_type_environment() {
    let invalid_guard = json!({
        "id": COLLECTION,
        "preconditions": ["self.missing > 0"],
        "derive": { "double_quantity": "self.quantity * 2" }
    });
    let findings = admission_findings(&row_effect(&invalid_guard));
    assert_finding(
        &findings,
        TARGET_NOT_WRITABLE,
        "/payload/effectProgram/collections",
    );
}

#[test]
fn row_preconditions_cannot_read_computed_child_properties() {
    let parent = entity_artifact(
        PARENT_ARTIFACT,
        PARENT,
        &parent_type(20, &json!({ "to": "many", "from": "one" })),
        &json!([{
            "artifactId": CHILD_ARTIFACT,
            "kind": "entity",
            "version": "1.0.0",
            "required": true
        }]),
    );
    let mut child_definition = child_type("text", "integer");
    child_definition["properties"][4]["computed"] = json!("self.quantity * 2");
    let child = entity_artifact(CHILD_ARTIFACT, CHILD, &child_definition, &json!([]));
    let computed_read = json!({
        "id": COLLECTION,
        "preconditions": ["self.double_quantity > 0"],
        "derive": {}
    });

    let findings = admission_diagnostics_for_artifacts(
        vec![parent, child],
        action_artifact(&row_effect(&computed_read)),
    );
    assert!(
        findings.iter().any(|finding| {
            finding.code.as_str() == TARGET_NOT_WRITABLE
                && finding
                    .span
                    .as_ref()
                    .is_some_and(|span| span.pointer == "/payload/effectProgram/collections")
                && finding.message.contains("double_quantity")
        }),
        "expected computed row-property read refusal; found {findings:?}"
    );
}

#[test]
fn row_preconditions_may_read_materialized_derived_properties() {
    let mut config = valid_collection_action();
    config["preconditions"] = json!(["self.double_quantity > 0"]);

    let findings = admission_findings(&row_effect(&config));
    assert!(
        findings.is_empty(),
        "preconditions can read completed row derivations: {findings:?}"
    );
}

#[test]
fn row_derivations_cannot_read_other_derived_properties() {
    let dependent_derivations = json!({
        "id": COLLECTION,
        "preconditions": [],
        "derive": {
            "double_quantity": "self.quantity * 2",
            "quantity": "self.double_quantity"
        }
    });

    let findings = admission_diagnostics(&row_effect(&dependent_derivations));
    assert!(
        findings.iter().any(|finding| {
            finding.code.as_str() == TARGET_NOT_WRITABLE
                && finding
                    .span
                    .as_ref()
                    .is_some_and(|span| span.pointer == "/payload/effectProgram/collections")
                && finding.message.contains("double_quantity")
        }),
        "expected dependent derivation refusal; found {findings:?}"
    );
}

#[test]
fn parent_values_schema_cannot_include_a_lifecycle_property() {
    let mut parent_definition = parent_type(20, &json!({ "to": "many", "from": "one" }));
    parent_definition["properties"] = json!([{
        "id": "status",
        "type": { "kind": "enum", "values": [{ "value": "open" }, { "value": "closed" }] }
    }]);
    parent_definition["lifecycles"] = json!([{
        "property": "status",
        "initial": ["open"],
        "terminal": ["closed"],
        "transitions": [{ "id": "close", "from": ["open"], "to": "closed" }]
    }]);
    let parent = entity_artifact(
        PARENT_ARTIFACT,
        PARENT,
        &parent_definition,
        &json!([{
            "artifactId": CHILD_ARTIFACT,
            "kind": "entity",
            "version": "1.0.0",
            "required": true
        }]),
    );
    let child = entity_artifact(
        CHILD_ARTIFACT,
        CHILD,
        &child_type("text", "integer"),
        &json!([]),
    );
    let mut action = action_artifact(&row_effect(&valid_collection_action()));
    action["payload"]["inputSchema"]["properties"]["parentValues"] = json!({
        "type": "object",
        "properties": { "status": { "type": "string" } },
        "required": ["status"],
        "additionalProperties": false
    });

    let findings = admission_diagnostics_for_artifacts(vec![parent, child], action);
    assert!(
        findings.iter().any(|finding| {
            finding.code.as_str() == DIRECT_LIFECYCLE_WRITE
                && finding
                    .span
                    .as_ref()
                    .is_some_and(|span| span.pointer == "/payload/effectProgram/parentValuesField")
                && finding.message.contains("lifecycle property")
        }),
        "expected parent lifecycle schema member refusal; found {findings:?}"
    );
}

#[test]
fn row_derivations_may_not_target_reserved_identity_or_order_properties() {
    let invalid_derive_target = json!({
        "id": COLLECTION,
        "preconditions": ["self.quantity > 0"],
        "derive": { "row_id": "self.quantity" }
    });
    let findings = admission_findings(&row_effect(&invalid_derive_target));
    assert_finding(
        &findings,
        TARGET_NOT_WRITABLE,
        "/payload/effectProgram/collections",
    );
}

#[test]
fn nested_collection_action_config_rejects_unknown_members() {
    let mut config = valid_collection_action();
    config["untrustedPolicy"] = json!("silently ignored is unsafe");

    let findings = admission_diagnostics(&row_effect(&config));
    assert!(
        findings.iter().any(|finding| {
            finding.code.as_str() == "action.validation.invalid_effect_program"
                && finding
                    .span
                    .as_ref()
                    .is_some_and(|span| span.pointer == "/payload/effectProgram")
                && finding.message.contains("untrustedPolicy")
        }),
        "expected unknown nested collection member rejection; found {findings:?}"
    );
}

#[test]
fn parent_and_rows_rejects_types_without_owned_collections() {
    let parent = entity_artifact(
        PARENT_ARTIFACT,
        PARENT,
        &json!({ "name": PARENT }),
        &json!([{
            "artifactId": CHILD_ARTIFACT,
            "kind": "entity",
            "version": "1.0.0",
            "required": true
        }]),
    );
    let child = entity_artifact(
        CHILD_ARTIFACT,
        CHILD,
        &child_type("text", "integer"),
        &json!([]),
    );
    let mut action = action_artifact(&row_effect(&valid_collection_action()));
    action["payload"]["effectProgram"]["collections"] = json!([]);
    action["payload"]["writes"] = json!([
        { "entityKind": PARENT, "change": "created" },
        { "entityKind": PARENT, "change": "modified" }
    ]);

    let findings = admission_findings_for_artifacts(vec![parent, child], action);
    assert_finding(
        &findings,
        TARGET_NOT_WRITABLE,
        "/payload/effectProgram/collections",
    );
}

#[test]
fn row_derivations_cannot_target_inherited_child_lifecycle_properties() {
    let parent = entity_artifact(
        PARENT_ARTIFACT,
        PARENT,
        &parent_type(20, &json!({ "to": "many", "from": "one" })),
        &json!([{
            "artifactId": CHILD_ARTIFACT,
            "kind": "entity",
            "version": "1.0.0",
            "required": true
        }]),
    );
    let lifecycle_base = entity_artifact(
        CHILD_BASE_ARTIFACT,
        CHILD_BASE,
        &json!({
            "name": CHILD_BASE,
            "properties": [{
                "id": "status",
                "type": { "kind": "enum", "values": [{ "value": "open" }, { "value": "closed" }] }
            }],
            "lifecycles": [{
                "property": "status",
                "initial": ["open"],
                "terminal": ["closed"],
                "transitions": [{ "id": "close", "from": ["open"], "to": "closed" }]
            }]
        }),
        &json!([]),
    );
    let mut child_type = child_type("text", "integer");
    child_type["extends"] = json!(CHILD_BASE);
    let child = entity_artifact(
        CHILD_ARTIFACT,
        CHILD,
        &child_type,
        &json!([{
            "artifactId": CHILD_BASE_ARTIFACT,
            "kind": "entity",
            "version": "1.0.0",
            "required": true
        }]),
    );
    let mut config = valid_collection_action();
    config["derive"] = json!({ "status": "self.status" });

    let findings = admission_diagnostics_for_artifacts(
        vec![parent, lifecycle_base, child],
        action_artifact(&row_effect(&config)),
    );
    assert!(
        findings.iter().any(|finding| {
            finding.code.as_str() == DIRECT_LIFECYCLE_WRITE
                && finding
                    .span
                    .as_ref()
                    .is_some_and(|span| span.pointer == "/payload/effectProgram/collections")
                && finding.message.contains("lifecycle")
        }),
        "expected inherited lifecycle derivation rejection; found {findings:?}"
    );
}

#[test]
fn nested_owned_collections_are_rejected_including_inherited_child_ownership() {
    let parent = parent_type(20, &json!({ "to": "many", "from": "one" }));
    let mut child = child_type("text", "integer");
    child["links"] = json!([{ "id": "parts", "target": "test.Part", "cardinality": { "to": "many", "from": "one" } }]);
    child["ownedCollections"] = json!([{
        "id": "parts", "childType": "test.Part", "rowIdProperty": "row_id", "orderingKey": "idx", "cascadeOnParentDelete": true, "maxRows": 20
    }]);
    let mut part = child_type("text", "integer");
    part["name"] = json!("test.Part");
    let direct = model_findings(vec![parent.clone(), child.clone(), part.clone()]);
    assert_finding(
        &direct,
        object_codes::INVALID_OWNED_COLLECTION,
        "/objectTypes/0/ownedCollections/0/childType",
    );
    child["name"] = json!(CHILD_BASE);
    let inherited_child = json!({ "name": CHILD, "extends": CHILD_BASE });
    let inherited = model_findings(vec![parent, inherited_child, child, part]);
    assert_finding(
        &inherited,
        object_codes::INVALID_OWNED_COLLECTION,
        "/objectTypes/0/ownedCollections/0/childType",
    );
}

#[test]
fn owned_links_require_defaults_for_required_stored_link_properties() {
    for (property, refused) in [
        (json!({ "id": "label", "type": { "kind": "text" } }), true),
        (
            json!({ "id": "label", "type": { "kind": "text" }, "default": "owned" }),
            false,
        ),
        (
            json!({ "id": "label", "type": { "kind": "text" }, "constraints": { "nullable": true } }),
            false,
        ),
    ] {
        let mut parent = parent_type(20, &json!({ "to": "many", "from": "one" }));
        parent["links"][0]["properties"] = json!([property]);
        let findings = entity_model_findings(parent, child_type("text", "integer"));
        assert_eq!(
            findings
                .iter()
                .any(|(code, _)| code == object_codes::INVALID_OWNED_COLLECTION),
            refused,
            "{findings:?}"
        );
    }
}

#[test]
fn parent_row_actions_reject_lifecycle_only_cas_touch_targets() {
    for computed in [false, true] {
        let mut parent = parent_type(20, &json!({ "to": "many", "from": "one" }));
        parent["properties"] = json!([{
            "id": "status", "type": { "kind": "enum", "values": [{ "value": "open" }, { "value": "closed" }] }, "default": "open"
        }]);
        parent["lifecycles"] = json!([{
            "property": "status", "initial": ["open"], "terminal": ["closed"], "transitions": [{ "id": "close", "from": ["open"], "to": "closed" }]
        }]);
        if computed {
            parent["properties"].as_array_mut().expect("properties").push(json!({ "id": "display_status", "type": { "kind": "text" }, "computed": "self.status" }));
        }
        let findings = admission_diagnostics_for_artifacts(
            vec![
                entity_artifact(PARENT_ARTIFACT, PARENT, &parent, &json!([])),
                entity_artifact(
                    CHILD_ARTIFACT,
                    CHILD,
                    &child_type("text", "integer"),
                    &json!([]),
                ),
            ],
            action_artifact(&row_effect(&valid_collection_action())),
        );
        assert!(
            findings
                .iter()
                .any(|finding| finding.code.as_str() == TARGET_NOT_WRITABLE
                    && finding.message.contains("CAS touches")),
            "{findings:?}"
        );
    }
}

#[test]
fn closed_parent_patch_schema_rejects_unknown_and_reserved_members() {
    for name in ["unknown_required", "_norn_internal"] {
        let mut action = action_artifact(&row_effect(&valid_collection_action()));
        action["payload"]["inputSchema"]["properties"]["parentValues"] = json!({
            "type": "object", "properties": { name: { "type": "string" } }, "required": [name], "additionalProperties": false
        });
        let findings = admission_findings_for_artifacts(
            vec![
                entity_artifact(
                    PARENT_ARTIFACT,
                    PARENT,
                    &parent_type(20, &json!({ "to": "many", "from": "one" })),
                    &json!([]),
                ),
                entity_artifact(
                    CHILD_ARTIFACT,
                    CHILD,
                    &child_type("text", "integer"),
                    &json!([]),
                ),
            ],
            action,
        );
        assert_finding(
            &findings,
            TARGET_NOT_WRITABLE,
            "/payload/effectProgram/parentValuesField",
        );
    }
}

#[test]
fn owned_links_also_check_required_properties_inherited_from_a_parent_link() {
    let mut base = parent_type(20, &json!({ "to": "many", "from": "one" }));
    let collections = base["ownedCollections"].clone();
    base.as_object_mut()
        .expect("base")
        .remove("ownedCollections");
    base["name"] = json!("test.OrderBase");
    base["links"][0]["properties"] = json!([{ "id": "label", "type": { "kind": "text" } }]);
    let parent =
        json!({ "name": PARENT, "extends": "test.OrderBase", "ownedCollections": collections });
    let findings = model_findings(vec![base, parent, child_type("text", "integer")]);
    assert_finding(
        &findings,
        object_codes::INVALID_OWNED_COLLECTION,
        "/objectTypes/1/ownedCollections",
    );
}

#[test]
fn row_effect_plumbing_can_share_a_name_with_parent_lifecycle_properties() {
    let mut parent = parent_type(20, &json!({ "to": "many", "from": "one" }));
    parent["properties"].as_array_mut().expect("properties").push(json!({
        "id": "parent_version", "type": { "kind": "enum", "values": [{ "value": "open" }, { "value": "closed" }] }, "default": "open"
    }));
    parent["lifecycles"] = json!([{
        "property": "parent_version", "initial": ["open"], "terminal": ["closed"], "transitions": [{ "id": "close", "from": ["open"], "to": "closed" }]
    }]);
    let entities = vec![
        entity_artifact(PARENT_ARTIFACT, PARENT, &parent, &json!([])),
        entity_artifact(
            CHILD_ARTIFACT,
            CHILD,
            &child_type("text", "integer"),
            &json!([]),
        ),
    ];
    let mut action = action_artifact(&row_effect(&valid_collection_action()));
    action["payload"]["effectProgram"]["parentVersionField"] = json!("parent_version");
    let input = &mut action["payload"]["inputSchema"];
    let properties = input["properties"]
        .as_object_mut()
        .expect("input properties");
    let version = properties.remove("parentVersion").expect("CAS input");
    properties.insert("parent_version".to_owned(), version);
    for field in input["required"].as_array_mut().expect("required inputs") {
        if field.as_str() == Some("parentVersion") {
            *field = json!("parent_version");
        }
    }
    let findings = admission_diagnostics_for_artifacts(entities.clone(), action.clone());
    assert!(
        findings.is_empty(),
        "CAS input only reads the version: {findings:?}"
    );
    action["payload"]["inputSchema"]["properties"]["parentValues"] = json!({
        "type": "object", "properties": { "parent_version": { "type": "string" } }, "additionalProperties": false
    });
    let findings = admission_diagnostics_for_artifacts(entities, action);
    assert!(
        findings
            .iter()
            .any(|finding| finding.code.as_str() == DIRECT_LIFECYCLE_WRITE),
        "nested parent state writes remain forbidden: {findings:?}"
    );
}

#[test]
fn owned_row_derivations_cannot_write_generated_naming_series_keys() {
    for inherited in [false, true] {
        let mut child = child_type("text", "integer");
        let series = json!({
            "name": "primary", "properties": ["number"], "namingSeries": { "pattern": "LINE-{YYYY}-{#####}", "timezone": "UTC", "resetPolicy": "year", "gapPolicy": "gapless_within_commit" }
        });
        let mut entities = vec![entity_artifact(
            PARENT_ARTIFACT,
            PARENT,
            &parent_type(20, &json!({ "to": "many", "from": "one" })),
            &json!([]),
        )];
        if inherited {
            child["extends"] = json!(CHILD_BASE);
            entities.push(entity_artifact(CHILD_BASE_ARTIFACT, CHILD_BASE, &json!({
                "name": CHILD_BASE, "properties": [{ "id": "number", "type": { "kind": "text" } }], "identity": [series]
            }), &json!([])));
        } else {
            child["properties"]
                .as_array_mut()
                .expect("properties")
                .push(json!({ "id": "number", "type": { "kind": "text" } }));
            child["identity"] = json!([series]);
        }
        entities.push(entity_artifact(CHILD_ARTIFACT, CHILD, &child, &json!([])));
        let mut config = valid_collection_action();
        config["derive"]["number"] = json!("\"LINE-1\"");
        let findings =
            admission_diagnostics_for_artifacts(entities, action_artifact(&row_effect(&config)));
        assert!(
            findings
                .iter()
                .any(|finding| finding.code.as_str() == TARGET_NOT_WRITABLE
                    && finding.message.contains("generated naming-series")),
            "{findings:?}"
        );
    }
}

#[test]
fn row_operation_items_must_admit_the_bounded_json_operation_profile() {
    for (items, accepted) in [
        (json!({ "type": "string" }), false),
        (
            json!({ "type": "object", "properties": { "unrelated": { "type": "string" } }, "additionalProperties": false }),
            false,
        ),
        (
            json!({ "type": ["null", "boolean", "number", "string", "array", "object"] }),
            true,
        ),
    ] {
        let mut action = action_artifact(&row_effect(&valid_collection_action()));
        action["payload"]["inputSchema"]["properties"]["rowOps"] =
            json!({ "type": "array", "items": items });
        let findings = admission_diagnostics_for_artifacts(
            vec![
                entity_artifact(
                    PARENT_ARTIFACT,
                    PARENT,
                    &parent_type(20, &json!({ "to": "many", "from": "one" })),
                    &json!([]),
                ),
                entity_artifact(
                    CHILD_ARTIFACT,
                    CHILD,
                    &child_type("text", "integer"),
                    &json!([]),
                ),
            ],
            action,
        );
        if accepted {
            assert!(findings.is_empty(), "{findings:?}");
        } else {
            assert!(
                findings
                    .iter()
                    .any(|finding| finding.code.as_str() == TARGET_NOT_WRITABLE
                        && finding.message.contains("row-operation items")),
                "{findings:?}"
            );
        }
    }
}
