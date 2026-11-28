#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::indexing_slicing,
    missing_docs,
    reason = "integration-test assertions"
)]

use norn_checker::decode_artifact_json;
use norn_checker::ripple::{
    ImpactKind, RIPPLE_FORMAT, RippleStatus, compute_ripple_effects,
    compute_ripple_effects_with_budget,
};
use norn_graph::Budget;
use norn_ir::ArtifactEnvelope;
use serde_json::{Value, json};

fn envelope(
    artifact_id: &str,
    kind: &str,
    dependencies: Vec<Value>,
    payload: Value,
) -> ArtifactEnvelope {
    let value = json!({
        "irVersion": "0.1",
        "artifactId": artifact_id,
        "kind": kind,
        "namespace": artifact_id.split('.').next().unwrap_or("demo"),
        "name": artifact_id.rsplit('.').next().unwrap_or(artifact_id),
        "version": "0.1.0",
        "lifecycle": "draft",
        "scope": { "orgId": "org_demo", "environmentId": "dev" },
        "dependencies": dependencies,
        "provenance": { "source": "authoring_sdk" },
        "payload": payload,
    });
    decode_artifact_json(&value).expect("test envelope decodes")
}

fn dep(artifact_id: &str, kind: &str) -> Value {
    json!({
        "artifactId": artifact_id,
        "kind": kind,
        "version": "0.1.0",
        "required": true,
    })
}

fn entity_v2(
    artifact_id: &str,
    name: &str,
    extends: Option<&str>,
    implements: &[&str],
    deps: Vec<Value>,
) -> ArtifactEnvelope {
    let mut object_type = json!({
        "name": name,
        "properties": [
            { "id": "id", "type": { "kind": "text" } }
        ]
    });
    if let Some(parent) = extends {
        object_type["extends"] = json!(parent);
    }
    if !implements.is_empty() {
        object_type["implements"] = json!(implements);
    }
    envelope(
        artifact_id,
        "entity",
        deps,
        json!({
            "entityVersion": "2.0",
            "objectType": object_type,
        }),
    )
}

fn action_upsert(
    artifact_id: &str,
    action_id: &str,
    entity_artifact_id: &str,
    object_kind: &str,
) -> ArtifactEnvelope {
    envelope(
        artifact_id,
        "action",
        vec![dep(entity_artifact_id, "entity")],
        json!({
            "actionId": action_id,
            "scopeRequired": ["orgId", "environmentId"],
            "idempotent": true,
            "ledgered": true,
            "inputSchema": {
                "type": "object",
                "properties": { "id": { "type": "string" } },
                "required": ["id"],
                "additionalProperties": false
            },
            "outputSchema": {
                "type": "object",
                "properties": { "ok": { "type": "boolean" } },
                "required": ["ok"],
                "additionalProperties": false
            },
            "writes": [{ "entityKind": object_kind, "change": "modified" }],
            "effectProgram": {
                "kind": "upsert_input_object",
                "objectIdField": "id",
                "objectKind": object_kind,
                "entityArtifactId": entity_artifact_id,
                "entityArtifactVersion": "0.1.0"
            }
        }),
    )
}

fn law_on_action(
    artifact_id: &str,
    law_id: &str,
    subject: &str,
    action_artifact_id: &str,
) -> ArtifactEnvelope {
    envelope(
        artifact_id,
        "law",
        vec![dep(action_artifact_id, "action")],
        json!({
            "lawVersion": "0.1",
            "id": law_id,
            "owner": "treasury",
            "rationale": "Purity law must hold after every melt",
            "subject": subject,
            "scope": {
                "root": { "kind": "entity", "id": subject },
                "objectTypes": [subject]
            },
            "modality": { "kind": "always" },
            "response": { "kind": "action", "artifactId": action_artifact_id },
            "severity": "error",
            "provenance": { "source": "authoring_sdk" }
        }),
    )
}

fn widget_for_action(
    artifact_id: &str,
    widget_id: &str,
    action_artifact_id: &str,
    action_id: &str,
) -> ArtifactEnvelope {
    envelope(
        artifact_id,
        "widget",
        vec![dep(action_artifact_id, "action")],
        json!({
            "widgetVersion": "0.1",
            "widgetId": widget_id,
            "dataDependencies": [],
            "viewStates": [{ "stateId": "ready", "kind": "ready" }],
            "actionBindings": [{
                "bindingId": "run_melt",
                "actionId": action_id,
                "label": "Melt",
                "requiredPermissions": ["jewelry:melt"]
            }],
            "permissions": []
        }),
    )
}

#[test]
fn multi_hop_transitive_ripple_across_core_subtype_action_law_widget() {
    let holding = entity_v2("core.Holding", "core.Holding", None, &[], vec![]);
    let metal = entity_v2(
        "jewelry.Metal",
        "jewelry.Metal",
        Some("core.Holding"),
        &[],
        vec![dep("core.Holding", "entity")],
    );
    let melt = action_upsert(
        "jewelry.melt.action",
        "jewelry.melt",
        "jewelry.Metal",
        "jewelry.Metal",
    );
    let purity = law_on_action(
        "jewelry.purity.law",
        "jewelry.purity",
        "jewelry.Metal",
        "jewelry.melt.action",
    );
    let widget = widget_for_action(
        "jewelry.melt.widget",
        "jewelry.melt_page",
        "jewelry.melt.action",
        "jewelry.melt",
    );

    let before = vec![
        holding.clone(),
        metal.clone(),
        melt.clone(),
        purity.clone(),
        widget.clone(),
    ];
    let after = before.clone();

    let report = compute_ripple_effects(&before, &after, &["core.Holding".to_owned()]);
    assert_eq!(report.status, RippleStatus::Exact);
    assert!(report.budget_exceeded.is_none());
    assert_eq!(report.roots, vec!["core.Holding".to_owned()]);

    let find = |id: &str| {
        report
            .ripples
            .iter()
            .find(|entry| entry.artifact_id == id)
            .expect("expected ripple entry")
    };

    let r_holding = find("core.Holding");
    assert_eq!(r_holding.impact_kind, ImpactKind::DirectChange);
    assert_eq!(r_holding.via, vec!["core.Holding"]);

    let r_metal = find("jewelry.Metal");
    assert_eq!(r_metal.impact_kind, ImpactKind::SpecializationAffected);
    assert_eq!(r_metal.triggered_by, vec!["core.Holding"]);
    assert_eq!(r_metal.via, vec!["core.Holding", "jewelry.Metal"]);

    let r_melt = find("jewelry.melt.action");
    assert_eq!(r_melt.impact_kind, ImpactKind::WriterAffected);
    assert_eq!(
        r_melt.via,
        vec!["core.Holding", "jewelry.Metal", "jewelry.melt.action"]
    );

    let r_law = find("jewelry.purity.law");
    assert_eq!(r_law.impact_kind, ImpactKind::LawAffected);
    assert_eq!(r_law.triggered_by, vec!["core.Holding"]);
    assert_eq!(r_law.via.first().map(String::as_str), Some("core.Holding"));
    assert_eq!(
        r_law.via.last().map(String::as_str),
        Some("jewelry.purity.law")
    );

    let r_widget = find("jewelry.melt.widget");
    assert_eq!(r_widget.impact_kind, ImpactKind::ProjectionOrUiAffected);
    assert_eq!(
        r_widget.via,
        vec![
            "core.Holding",
            "jewelry.Metal",
            "jewelry.melt.action",
            "jewelry.melt.widget",
        ]
    );

    // Changing only the action still ripples into its governing law and bound widget.
    let action_report =
        compute_ripple_effects(&before, &after, &["jewelry.melt.action".to_owned()]);
    assert_eq!(action_report.status, RippleStatus::Exact);
    let ids: Vec<&str> = action_report
        .ripples
        .iter()
        .map(|entry| entry.artifact_id.as_str())
        .collect();
    assert_eq!(
        ids,
        vec![
            "jewelry.melt.action",
            "jewelry.melt.widget",
            "jewelry.purity.law",
        ]
    );

    let wire = report.to_json_value();
    assert_eq!(wire["format"], RIPPLE_FORMAT);
    assert_eq!(wire["status"], "exact");
}

#[test]
fn removed_dependency_marks_dependents_as_broken() {
    let base = entity_v2("core.Item", "core.Item", None, &[], vec![]);
    let action = action_upsert(
        "app.create_item",
        "app.create_item",
        "core.Item",
        "core.Item",
    );

    let before = vec![base, action.clone()];
    let after = vec![action];

    let report = compute_ripple_effects(&before, &after, &[]);
    assert_eq!(report.status, RippleStatus::Exact);
    assert_eq!(report.roots, vec!["core.Item".to_owned()]);

    let broken = report
        .ripples
        .iter()
        .find(|entry| entry.artifact_id == "app.create_item")
        .expect("dependent action is reported");
    assert_eq!(broken.impact_kind, ImpactKind::RemovedDependencyBroken);
    assert_eq!(broken.triggered_by, vec!["core.Item"]);
    assert_eq!(broken.via, vec!["core.Item", "app.create_item"]);
}

#[test]
fn cycle_safe_and_input_order_independent_deterministic_ordering() {
    let a = entity_v2(
        "cycle.A",
        "cycle.A",
        None,
        &[],
        vec![dep("cycle.B", "entity")],
    );
    let b = entity_v2(
        "cycle.B",
        "cycle.B",
        None,
        &[],
        vec![dep("cycle.A", "entity")],
    );
    let c = entity_v2(
        "cycle.C",
        "cycle.C",
        Some("cycle.B"),
        &[],
        vec![dep("cycle.B", "entity")],
    );

    let forward = compute_ripple_effects(
        &[a.clone(), b.clone(), c.clone()],
        &[a.clone(), b.clone(), c.clone()],
        &["cycle.A".to_owned()],
    );
    let reversed = compute_ripple_effects(
        &[c.clone(), b.clone(), a.clone()],
        &[c, b, a],
        &["cycle.A".to_owned()],
    );

    assert_eq!(forward.status, RippleStatus::Exact);
    assert_eq!(forward, reversed);
    let ids: Vec<&str> = forward
        .ripples
        .iter()
        .map(|entry| entry.artifact_id.as_str())
        .collect();
    assert_eq!(ids, vec!["cycle.A", "cycle.B", "cycle.C"]);
    assert_eq!(
        forward.ripples[2].via,
        vec!["cycle.A", "cycle.B", "cycle.C"]
    );
}

#[test]
fn budget_exhaustion_fails_closed_with_unknown_and_no_silent_truncation() {
    let a = entity_v2("b.A", "b.A", None, &[], vec![]);
    let b = entity_v2("b.B", "b.B", Some("b.A"), &[], vec![dep("b.A", "entity")]);
    let c = entity_v2("b.C", "b.C", Some("b.B"), &[], vec![dep("b.B", "entity")]);
    let set = vec![a, b, c];

    let node_refused = compute_ripple_effects_with_budget(
        &set,
        &set,
        &["b.A".to_owned()],
        Budget {
            max_nodes: 2,
            max_edges: 100,
            max_steps: 1000,
        },
    );
    assert_eq!(node_refused.status, RippleStatus::Unknown);
    assert!(node_refused.ripples.is_empty());
    assert_eq!(
        node_refused
            .budget_exceeded
            .as_ref()
            .map(|r| r.resource.as_str()),
        Some("nodes")
    );

    let edge_refused = compute_ripple_effects_with_budget(
        &set,
        &set,
        &["b.A".to_owned()],
        Budget {
            max_nodes: 10,
            max_edges: 1,
            max_steps: 1000,
        },
    );
    assert_eq!(edge_refused.status, RippleStatus::Unknown);
    assert!(edge_refused.ripples.is_empty());
    assert_eq!(
        edge_refused
            .budget_exceeded
            .as_ref()
            .map(|r| r.resource.as_str()),
        Some("edges")
    );

    let step_refused = compute_ripple_effects_with_budget(
        &set,
        &set,
        &["b.A".to_owned()],
        Budget {
            max_nodes: 10,
            max_edges: 10,
            max_steps: 1,
        },
    );
    assert_eq!(step_refused.status, RippleStatus::Unknown);
    assert!(step_refused.ripples.is_empty());
    assert_eq!(
        step_refused
            .budget_exceeded
            .as_ref()
            .map(|r| r.resource.as_str()),
        Some("steps")
    );
}
