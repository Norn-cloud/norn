#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::indexing_slicing,
    missing_docs,
    reason = "integration-test assertions"
)]

use norn_checker::{DefinitionDiff, Narrowing, decode_artifact_json, diff_definition_sets};
use serde_json::{Value, json};

const SALES_ORDER: &str = include_str!("fixtures/mapping/model/commerce.sales-order.entity.json");
const SALES_ORDER_MAPPING: &str = include_str!("../../../fixtures/ir/synthetic-rows.mapping.json");
const CRM_DEFINITIONS: &str =
    include_str!("../../../tools/norn-ts/fixtures/definitions/crm.ir.json");

fn decode(value: &Value) -> norn_ir::ArtifactEnvelope {
    decode_artifact_json(value).expect("fixture is a decodable artifact")
}

fn object_type_artifact(artifact_id: &str, object_type: Value) -> norn_ir::ArtifactEnvelope {
    let mut artifact: Value = serde_json::from_str(SALES_ORDER).expect("entity fixture JSON");
    artifact["artifactId"] = json!(artifact_id);
    artifact["name"] = json!(artifact_id.rsplit('.').next().unwrap_or("entity"));
    artifact["payload"] = json!({"entityVersion": "2.0"});
    artifact["payload"]["objectType"] = object_type;
    decode(&artifact)
}

fn interface_artifact(artifact_id: &str, interface: Value) -> norn_ir::ArtifactEnvelope {
    let mut artifact: Value = serde_json::from_str(SALES_ORDER).expect("entity fixture JSON");
    artifact["artifactId"] = json!(artifact_id);
    artifact["name"] = json!(artifact_id.rsplit('.').next().unwrap_or("entity"));
    artifact["payload"] = json!({"entityVersion": "2.0"});
    artifact["payload"]["interface"] = interface;
    decode(&artifact)
}

fn invariant(id: &str, expression: &str, classification: Option<&str>) -> Value {
    let mut invariant = json!({
        "id": id,
        "expression": expression,
        "message": format!("{id} must hold")
    });
    if let Some(classification) = classification {
        invariant["classification"] = json!(classification);
    }
    invariant
}

fn inherited_parent_invariants(classification: &str) -> Vec<norn_ir::ArtifactEnvelope> {
    vec![
        object_type_artifact(
            "commerce.entity.base",
            json!({
                "name": "commerce.Base",
                "invariants": [invariant("parent-rule", "self.id != ''", Some(classification))]
            }),
        ),
        object_type_artifact(
            "commerce.entity.child",
            json!({"name": "commerce.Child", "extends": "commerce.Base"}),
        ),
    ]
}

fn inherited_interface_invariants(classification: &str) -> Vec<norn_ir::ArtifactEnvelope> {
    vec![
        interface_artifact(
            "commerce.entity.named",
            json!({
                "name": "commerce.Named",
                "invariants": [invariant("interface-rule", "self.id != ''", Some(classification))]
            }),
        ),
        object_type_artifact(
            "commerce.entity.base",
            json!({
                "name": "commerce.Base",
                "implements": ["commerce.Named"]
            }),
        ),
        object_type_artifact(
            "commerce.entity.child",
            json!({"name": "commerce.Child", "extends": "commerce.Base"}),
        ),
    ]
}

fn kinds(diff: &DefinitionDiff) -> Vec<&str> {
    diff.narrowing
        .iter()
        .map(|item| item.kind.as_str())
        .collect()
}

#[test]
fn identifies_removed_stored_members_and_lifecycle_state() {
    let previous: Value = serde_json::from_str(SALES_ORDER).expect("fixture JSON");
    let mut candidate = previous.clone();
    let payload = candidate["payload"]["objectType"].as_object_mut().unwrap();
    payload["properties"]
        .as_array_mut()
        .unwrap()
        .retain(|property| property["id"] != "order_date");
    payload["links"]
        .as_array_mut()
        .unwrap()
        .retain(|link| link["id"] != "counterparty");

    // Keep the candidate lifecycle internally coherent while dropping the
    // old terminal state and the transition that targeted it.
    let lifecycle = &mut candidate["payload"]["objectType"]["lifecycles"][0];
    lifecycle["terminal"] = json!([]);
    lifecycle["transitions"]
        .as_array_mut()
        .unwrap()
        .retain(|transition| transition["id"] != "cancel");
    candidate["payload"]["objectType"]["properties"]
        .as_array_mut()
        .unwrap()
        .iter_mut()
        .find(|property| property["id"] == "status")
        .unwrap()["type"]["values"]
        .as_array_mut()
        .unwrap()
        .retain(|state| state["value"] != "cancelled");

    let before = decode(&previous);
    let after = decode(&candidate);
    let diff = diff_definition_sets(&[before], &[after]);

    assert!(kinds(&diff).contains(&"property_removed"));
    assert!(kinds(&diff).contains(&"link_removed"));
    assert!(kinds(&diff).contains(&"lifecycle_state_removed"));
    assert!(kinds(&diff).contains(&"lifecycle_transition_removed"));
    assert!(diff.narrowing.iter().any(|item| {
        item.kind == "lifecycle_state_removed"
            && item.object_type.as_deref() == Some("commerce.SalesOrder")
            && item.property.as_deref() == Some("status")
            && item.ids == vec!["cancelled".to_owned()]
    }));
}

#[test]
fn adding_changing_or_removing_a_series_key_fails_closed() {
    let unnumbered = || {
        object_type_artifact(
            "commerce.entity.numbered",
            json!({
                "name": "commerce.Numbered",
                "properties": [
                    { "id": "number", "type": { "kind": "text" } },
                    { "id": "alternate", "type": { "kind": "text" } }
                ],
                "identity": [{ "name": "primary", "properties": ["number"] }]
            }),
        )
    };
    let numbered = |pattern: &str, component: &str| {
        object_type_artifact(
            "commerce.entity.numbered",
            json!({
                "name": "commerce.Numbered",
                "properties": [
                    { "id": "number", "type": { "kind": "text" } },
                    { "id": "alternate", "type": { "kind": "text" } }
                ],
                "identity": [{
                    "name": "primary",
                    "properties": [component],
                    "namingSeries": {
                        "pattern": pattern,
                        "timezone": "UTC",
                        "resetPolicy": "year",
                        "gapPolicy": "gapless_within_commit"
                    }
                }]
            }),
        )
    };

    let baseline = unnumbered();
    let first = numbered("PO-{YYYY}-{#####}", "number");
    let added = diff_definition_sets(
        std::slice::from_ref(&baseline),
        std::slice::from_ref(&first),
    );
    assert!(added.narrowing.iter().any(|item| {
        item.kind == "naming_series_contract_changed"
            && item.object_type.as_deref() == Some("commerce.Numbered")
            && item.property.as_deref() == Some("number")
            && item.ids == vec!["primary".to_owned()]
    }));

    let changed = numbered("PO/{YYYY}/{#####}", "number");
    assert!(
        diff_definition_sets(std::slice::from_ref(&first), std::slice::from_ref(&changed))
            .narrowing
            .iter()
            .any(|item| item.kind == "naming_series_contract_changed")
    );
    let component_changed = numbered("PO-{YYYY}-{#####}", "alternate");
    assert!(
        diff_definition_sets(std::slice::from_ref(&first), &[component_changed])
            .narrowing
            .iter()
            .any(|item| item.kind == "naming_series_contract_changed")
    );
    assert!(
        diff_definition_sets(&[first], &[baseline])
            .narrowing
            .iter()
            .any(|item| item.kind == "naming_series_contract_changed")
    );
}

#[test]
fn reparenting_between_series_owners_changes_the_effective_contract() {
    let numbered_parent = |artifact_id: &str, object_type: &str| {
        object_type_artifact(
            artifact_id,
            json!({
                "name": object_type,
                "properties": [{ "id": "number", "type": { "kind": "text" } }],
                "identity": [{
                    "name": "primary",
                    "properties": ["number"],
                    "namingSeries": {
                        "pattern": "DOC-{YYYY}-{#####}",
                        "timezone": "UTC",
                        "resetPolicy": "year",
                        "gapPolicy": "gapless_within_commit"
                    }
                }]
            }),
        )
    };
    let child = |parent: &str| {
        object_type_artifact(
            "commerce.entity.child",
            json!({ "name": "commerce.Child", "extends": parent }),
        )
    };
    let old_parent = numbered_parent("commerce.entity.old-parent", "commerce.OldParent");
    let new_parent = numbered_parent("commerce.entity.new-parent", "commerce.NewParent");
    let previous = vec![
        old_parent.clone(),
        new_parent.clone(),
        child("commerce.OldParent"),
    ];
    let candidate = vec![old_parent, new_parent, child("commerce.NewParent")];

    let diff = diff_definition_sets(&previous, &candidate);
    let child_series_changes = diff
        .narrowing
        .iter()
        .filter(|item| {
            item.kind == "naming_series_contract_changed"
                && item.object_type.as_deref() == Some("commerce.Child")
        })
        .collect::<Vec<_>>();
    assert_eq!(child_series_changes.len(), 1);
    assert_eq!(child_series_changes[0].artifact_id, "commerce.entity.child");
    assert_eq!(child_series_changes[0].ids, vec!["primary".to_owned()]);
    assert!(
        child_series_changes[0]
            .detail
            .contains("commerce.OldParent.primary")
    );
    assert!(
        child_series_changes[0]
            .detail
            .contains("commerce.NewParent.primary")
    );
}

#[test]
fn integrity_classification_changes_are_directional_narrowings() {
    let quality = object_type_artifact(
        "commerce.entity.item",
        json!({
            "name": "commerce.Item",
            "invariants": [invariant("nonnegative", "self.amount >= 0", Some("quality"))]
        }),
    );
    let integrity = object_type_artifact(
        "commerce.entity.item",
        json!({
            "name": "commerce.Item",
            "invariants": [invariant("nonnegative", "self.amount >= 0", Some("integrity"))]
        }),
    );

    let narrowed = diff_definition_sets(
        std::slice::from_ref(&quality),
        std::slice::from_ref(&integrity),
    );
    assert!(narrowed.narrowing.iter().any(|item| {
        item.kind == "integrity_invariant_narrowed"
            && item.artifact_id == "commerce.entity.item"
            && item.object_type.as_deref() == Some("commerce.Item")
            && item.ids == vec!["nonnegative".to_owned()]
    }));

    let widened = diff_definition_sets(
        std::slice::from_ref(&integrity),
        std::slice::from_ref(&quality),
    );
    assert!(
        !widened
            .narrowing
            .iter()
            .any(|item| item.kind == "integrity_invariant_narrowed")
    );

    let unchanged = diff_definition_sets(
        std::slice::from_ref(&quality),
        std::slice::from_ref(&quality),
    );
    assert!(
        !unchanged
            .narrowing
            .iter()
            .any(|item| item.kind == "integrity_invariant_narrowed")
    );
}

#[test]
fn adding_integrity_or_changing_its_expression_is_narrowing_but_quality_is_not() {
    let without_invariant =
        object_type_artifact("commerce.entity.item", json!({"name": "commerce.Item"}));
    let omitted_class = object_type_artifact(
        "commerce.entity.item",
        json!({
            "name": "commerce.Item",
            "invariants": [invariant("required", "self.amount > 0", None)]
        }),
    );
    let quality = object_type_artifact(
        "commerce.entity.item",
        json!({
            "name": "commerce.Item",
            "invariants": [invariant("expectation", "self.amount > 0", Some("quality"))]
        }),
    );
    let added = diff_definition_sets(
        std::slice::from_ref(&without_invariant),
        std::slice::from_ref(&omitted_class),
    );
    assert!(added.narrowing.iter().any(|item| {
        item.kind == "integrity_invariant_narrowed" && item.ids == vec!["required".to_owned()]
    }));
    let quality_added = diff_definition_sets(&[without_invariant], &[quality]);
    assert!(
        !quality_added
            .narrowing
            .iter()
            .any(|item| item.kind == "integrity_invariant_narrowed")
    );

    let changed_expression = object_type_artifact(
        "commerce.entity.item",
        json!({
            "name": "commerce.Item",
            "invariants": [invariant("required", "self.amount >= 0", None)]
        }),
    );
    let expression_diff = diff_definition_sets(&[omitted_class], &[changed_expression]);
    assert!(expression_diff.narrowing.iter().any(|item| {
        item.kind == "integrity_invariant_narrowed" && item.ids == vec!["required".to_owned()]
    }));
}

#[test]
fn integrity_invariants_added_to_supertypes_and_interfaces_reach_effective_types() {
    let previous = vec![
        interface_artifact("commerce.entity.named", json!({"name": "commerce.Named"})),
        object_type_artifact(
            "commerce.entity.base",
            json!({
                "name": "commerce.Base",
                "implements": ["commerce.Named"]
            }),
        ),
        object_type_artifact(
            "commerce.entity.child",
            json!({"name": "commerce.Child", "extends": "commerce.Base"}),
        ),
    ];
    let candidate = vec![
        interface_artifact(
            "commerce.entity.named",
            json!({
                "name": "commerce.Named",
                "invariants": [invariant("has-name", "self.name != ''", None)]
            }),
        ),
        object_type_artifact(
            "commerce.entity.base",
            json!({
                "name": "commerce.Base",
                "implements": ["commerce.Named"],
                "invariants": [invariant("has-id", "self.id != ''", None)]
            }),
        ),
        object_type_artifact(
            "commerce.entity.child",
            json!({"name": "commerce.Child", "extends": "commerce.Base"}),
        ),
    ];

    let diff = diff_definition_sets(&previous, &candidate);
    assert!(diff.narrowing.iter().any(|item| {
        item.kind == "integrity_invariant_narrowed"
            && item.artifact_id == "commerce.entity.base"
            && item.object_type.as_deref() == Some("commerce.Child")
            && item.ids == vec!["has-id".to_owned()]
    }));
    assert!(diff.narrowing.iter().any(|item| {
        item.kind == "integrity_invariant_narrowed"
            && item.artifact_id == "commerce.entity.named"
            && item.object_type.as_deref() == Some("commerce.Child")
            && item.ids == vec!["has-name".to_owned()]
    }));
}

#[test]
fn inherited_invariant_class_changes_are_directional_for_parent_and_interface() {
    for (before_class, after_class, is_narrowing) in [
        ("quality", "integrity", true),
        ("integrity", "quality", false),
    ] {
        let parent_diff = diff_definition_sets(
            &inherited_parent_invariants(before_class),
            &inherited_parent_invariants(after_class),
        );
        assert_eq!(
            parent_diff.narrowing.iter().any(|item| {
                item.kind == "integrity_invariant_narrowed"
                    && item.object_type.as_deref() == Some("commerce.Child")
                    && item.ids == vec!["parent-rule".to_owned()]
            }),
            is_narrowing,
            "parent invariant class transition {before_class} -> {after_class}"
        );

        let interface_diff = diff_definition_sets(
            &inherited_interface_invariants(before_class),
            &inherited_interface_invariants(after_class),
        );
        assert_eq!(
            interface_diff.narrowing.iter().any(|item| {
                item.kind == "integrity_invariant_narrowed"
                    && item.object_type.as_deref() == Some("commerce.Child")
                    && item.ids == vec!["interface-rule".to_owned()]
            }),
            is_narrowing,
            "interface invariant class transition {before_class} -> {after_class}"
        );
    }
}

#[test]
fn detects_effective_lifecycle_guard_and_terminal_narrowing() {
    let previous: Value = serde_json::from_str(SALES_ORDER).expect("fixture JSON");
    let mut candidate = previous.clone();
    let lifecycle = &mut candidate["payload"]["objectType"]["lifecycles"][0];
    lifecycle["terminal"] = json!(["submitted"]);
    lifecycle["transitions"]
        .as_array_mut()
        .unwrap()
        .retain(|transition| transition["id"] != "cancel");
    lifecycle["transitions"][0]["guards"] = json!(["self.total_amount > 0"]);
    candidate["payload"]["objectType"]["properties"]
        .as_array_mut()
        .unwrap()
        .iter_mut()
        .find(|property| property["id"] == "status")
        .unwrap()["type"]["values"]
        .as_array_mut()
        .unwrap()
        .retain(|state| state["value"] != "cancelled");

    let diff = diff_definition_sets(&[decode(&previous)], &[decode(&candidate)]);
    assert!(kinds(&diff).contains(&"lifecycle_guard_added"));
    assert!(kinds(&diff).contains(&"lifecycle_terminal_added"));
    assert!(kinds(&diff).contains(&"lifecycle_state_removed"));
}

#[test]
fn inherited_lifecycle_uses_effective_subtype_enum_states() {
    let parent: Value = serde_json::from_str(SALES_ORDER).expect("fixture JSON");
    let mut child = parent.clone();
    child["artifactId"] = json!("commerce.ChildOrder.entity");
    child["name"] = json!("ChildOrder");
    child["payload"]["objectType"]["name"] = json!("commerce.ChildOrder");
    child["payload"]["objectType"]["extends"] = json!("commerce.SalesOrder");
    child["payload"]["objectType"]
        .as_object_mut()
        .unwrap()
        .remove("lifecycles");

    let narrowed_child = {
        let mut narrowed = child.clone();
        narrowed["payload"]["objectType"]["properties"] = json!([{
            "id": "status",
            "type": {
                "kind": "enum",
                "values": [{ "value": "draft" }, { "value": "submitted" }]
            }
        }]);
        narrowed
    };
    let diff = diff_definition_sets(
        &[decode(&parent), decode(&child)],
        &[decode(&parent), decode(&narrowed_child)],
    );
    assert!(diff.narrowing.iter().any(|item| {
        item.kind == "lifecycle_state_removed"
            && item.artifact_id == "commerce.ChildOrder.entity"
            && item.object_type.as_deref() == Some("commerce.ChildOrder")
            && item.property.as_deref() == Some("status")
            && item.ids == vec!["cancelled".to_owned()]
    }));
}

#[test]
fn detects_action_identity_and_mapping_object_map_removals() {
    let definitions: Value = serde_json::from_str(CRM_DEFINITIONS).expect("fixture JSON");
    let action = definitions
        .as_array()
        .unwrap()
        .iter()
        .find(|artifact| artifact["artifactId"] == "crm.contact.upsert.action")
        .unwrap()
        .clone();
    let mut replacement = action.clone();
    replacement["payload"]["actionId"] = json!("crm.contact.upsert_v2");
    let action_diff = diff_definition_sets(&[decode(&action)], &[decode(&replacement)]);
    assert!(action_diff.narrowing.iter().any(|item| {
        item.kind == "action_removed" && item.ids == vec!["crm.contact.upsert".to_owned()]
    }));

    let mapping: Value = serde_json::from_str(SALES_ORDER_MAPPING).expect("mapping JSON");
    let mut changed_mapping = mapping.clone();
    changed_mapping["payload"]["maps"] = json!([]);
    let mapping_diff = diff_definition_sets(&[decode(&mapping)], &[decode(&changed_mapping)]);
    assert!(
        mapping_diff
            .narrowing
            .iter()
            .any(|item| { item.kind == "mapping_removed" && item.ids == vec!["row".to_owned()] })
    );
}

#[test]
fn provenance_only_change_is_not_a_semantic_change_and_event_summary_is_bounded() {
    let mut previous: Value = serde_json::from_str(SALES_ORDER).expect("fixture JSON");
    let mut candidate = previous.clone();
    previous["provenance"]["sourcePath"] = json!("before.ts");
    candidate["provenance"]["sourcePath"] = json!("after.ts");
    let diff = diff_definition_sets(&[decode(&previous)], &[decode(&candidate)]);
    assert!(diff.changed.is_empty());
    assert!(diff.added.is_empty());
    assert!(diff.removed.is_empty());
    let summary = diff.to_json();
    assert_eq!(summary["addedCount"], 0);
    assert_eq!(summary["narrowingCount"], 0);
}

#[test]
fn ledger_summary_bounds_counts_ids_and_member_lists() {
    let diff = DefinitionDiff {
        added: (0..101)
            .map(|index| norn_checker::DefinitionId {
                artifact_id: format!("artifact-{index}"),
                kind: "entity".to_owned(),
            })
            .collect(),
        narrowing: (0..101)
            .map(|index| Narrowing {
                kind: "property_removed".to_owned(),
                artifact_id: format!("{}-{index}", "界".repeat(100)),
                object_type: Some("type".to_owned()),
                property: None,
                ids: (0..100).map(|member| format!("member-{member}")).collect(),
                detail: "sensitive or lengthy explanation".repeat(20),
            })
            .collect(),
        ..DefinitionDiff::default()
    };
    let summary = diff.to_json();
    assert_eq!(summary["addedCount"], 101);
    assert_eq!(summary["added"].as_array().unwrap().len(), 100);
    assert_eq!(summary["narrowingCount"], 101);
    assert_eq!(summary["narrowing"].as_array().unwrap().len(), 20);
    let narrowing = &summary["narrowing"][0];
    assert!(narrowing["artifactId"].as_str().unwrap().len() <= 128);
    assert_eq!(summary["narrowing"][0]["ids"].as_array().unwrap().len(), 8);
    let total_narrowing_ids = summary["narrowing"]
        .as_array()
        .unwrap()
        .iter()
        .map(|item| item["ids"].as_array().unwrap().len())
        .sum::<usize>();
    assert_eq!(total_narrowing_ids, 64);
    assert!(narrowing.get("detail").is_none());
    assert!(serde_json::to_vec(&summary).unwrap().len() < 100_000);
}

#[test]
fn owned_collection_bound_increases_are_not_narrowing() {
    let parent = json!({
        "name": "commerce.Parent", "links": [{ "id": "lines", "target": "commerce.Line", "cardinality": { "to": "many", "from": "one" } }],
        "ownedCollections": [{ "id": "lines", "childType": "commerce.Line", "rowIdProperty": "row_id", "orderingKey": "idx", "maxRows": 10, "cascadeOnParentDelete": true }]
    });
    let child = object_type_artifact(
        "commerce.Line.entity",
        json!({ "name": "commerce.Line", "properties": [{ "id": "row_id", "type": { "kind": "text" } }, { "id": "idx", "type": { "kind": "integer" } }] }),
    );
    let previous = vec![
        object_type_artifact("commerce.Parent.entity", parent.clone()),
        child.clone(),
    ];
    for (max_rows, cascade, narrowing) in [(20, true, false), (5, true, true), (20, false, true)] {
        let mut candidate_parent = parent.clone();
        candidate_parent["ownedCollections"][0]["maxRows"] = json!(max_rows);
        candidate_parent["ownedCollections"][0]["cascadeOnParentDelete"] = json!(cascade);
        let candidate = vec![
            object_type_artifact("commerce.Parent.entity", candidate_parent),
            child.clone(),
        ];
        let diff = diff_definition_sets(&previous, &candidate);
        assert_eq!(
            diff.narrowing
                .iter()
                .any(|item| item.kind == "owned_collection_contract_narrowed"),
            narrowing,
            "{diff:?}"
        );
    }
}

#[test]
fn adding_an_interface_parent_invariant_is_a_narrowing_for_transitive_implementers() {
    let before = vec![
        interface_artifact(
            "commerce.parent",
            json!({"name":"commerce.Parent","properties":[{"id":"name","type":{"kind":"text"}}],"invariants":[invariant("required","self.name != ''",None)]}),
        ),
        interface_artifact("commerce.child", json!({"name":"commerce.Child"})),
        object_type_artifact(
            "commerce.item",
            json!({"name":"commerce.Item","implements":["commerce.Child"],"properties":[{"id":"name","type":{"kind":"text"}}]}),
        ),
    ];
    let mut after = before.clone();
    after[1] = interface_artifact(
        "commerce.child",
        json!({"name":"commerce.Child","extends":["commerce.Parent"]}),
    );
    let diff = diff_definition_sets(&before, &after);
    assert!(
        diff.narrowing
            .iter()
            .any(|item| item.kind == "integrity_invariant_narrowed"
                && item.object_type.as_deref() == Some("commerce.Item")
                && item.ids == vec!["required".to_owned()])
    );
}
