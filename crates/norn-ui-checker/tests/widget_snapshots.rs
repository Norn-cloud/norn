//! Widget envelope validation and invalid-fixture snapshot parity tests.

use std::collections::BTreeSet;
use std::{fs, path::Path};

use norn_checker::{EntityArtifactSetContext, codes, diagnostics_to_json};
use norn_ir::explain::ExplainKind;
use norn_ir::{ArtifactEnvelope, ArtifactKind};
use norn_ui_checker::{check_artifact_json, explain, validate_artifact};
use serde_json::{Value, json};

fn repo_root() -> Result<&'static Path, &'static str> {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .ok_or("crate sits two levels below the repo root")
}

fn read_json(path: &Path) -> Result<Value, String> {
    let text = fs::read_to_string(path).map_err(|error| format!("{}: {error}", path.display()))?;
    serde_json::from_str(&text).map_err(|error| format!("{}: {error}", path.display()))
}

fn widget_artifact(payload: &Value) -> Result<ArtifactEnvelope, serde_json::Error> {
    serde_json::from_value(json!({
        "irVersion": "0.1",
        "artifactId": "retail.widget.test",
        "kind": ArtifactKind::Widget,
        "namespace": "retail",
        "name": "Test Widget",
        "version": "0.1.0",
        "lifecycle": "draft",
        "scope": { "orgId": "org_demo", "environmentId": "dev" },
        "dependencies": [],
        "provenance": { "source": "authoring_sdk" },
        "payload": payload,
    }))
}

#[test]
fn invalid_widget_fixtures_match_declared_code_pointer_and_snapshot() {
    let dir = repo_root().expect("repo root").join("fixtures/invalid");
    let manifest = read_json(&dir.join("manifest.json")).expect("manifest");
    let entries = manifest
        .get("fixtures")
        .and_then(Value::as_array)
        .expect("manifest lists fixtures");
    let snapshots = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/snapshots");
    let mut checked_count = 0usize;

    for entry in entries {
        let code = entry["code"].as_str().expect("code");
        if !code.starts_with("widget.") {
            continue;
        }
        checked_count = checked_count.saturating_add(1);
        let file = entry["file"].as_str().expect("file");
        let error_at = entry["error_at"].as_str().expect("error_at");
        assert!(codes::lookup(code).is_some(), "{file}: {code} unregistered");

        let diagnostics = check_artifact_json(&read_json(&dir.join(file)).expect("fixture"));
        assert!(
            diagnostics.iter().any(norn_checker::Diagnostic::is_error),
            "{file}: the UI checker accepted an invalid widget fixture"
        );
        assert!(
            diagnostics.iter().any(|diagnostic| {
                diagnostic.code == code
                    && diagnostic
                        .span
                        .as_ref()
                        .is_some_and(|span| span.pointer == error_at)
            }),
            "{file}: expected {code} at {error_at}, got {diagnostics:#?}"
        );
        let output = diagnostics_to_json(&diagnostics);
        let checked_in = read_json(&snapshots.join(file)).expect("snapshot");
        assert_eq!(checked_in, output, "{file}: snapshot mismatch");
    }
    assert!(
        checked_count > 0,
        "expected at least one invalid widget fixture in fixtures/invalid/manifest.json"
    );
}

#[test]
fn rejects_relation_traversal_widget_dependency_without_relation_kind() {
    let artifact = widget_artifact(&json!({
        "widgetVersion": "0.1",
        "widgetId": "retail.stock.overview",
        "dataDependencies": [{
            "dependencyId": "locationEdge",
            "objectKind": "retail.StockPosition",
            "query": { "kind": "relation_traversal" }
        }],
        "viewStates": [{ "stateId": "ready", "kind": "ready" }],
        "actionBindings": [],
        "permissions": [{
            "permission": "retail.inventory.read",
            "reason": "show scoped inventory data"
        }]
    }))
    .expect("widget envelope should decode");

    let diagnostics =
        validate_artifact(&artifact).expect_err("relation traversal must declare relation kind");
    assert!(diagnostics.iter().any(|diagnostic| {
        diagnostic.code == "widget.validation.invalid_payload"
            && diagnostic.path.as_deref() == Some("payload.dataDependencies[0].query.relationKind")
    }));
}

#[test]
fn rejects_widget_action_binding_without_dependency() {
    let artifact = widget_artifact(&json!({
        "widgetVersion": "0.1",
        "widgetId": "retail.replenishment.proposal",
        "dataDependencies": [],
        "viewStates": [{ "stateId": "preview", "kind": "preview" }],
        "actionBindings": [{
            "bindingId": "approve",
            "actionId": "retail.replenishment.approve",
            "label": "Approve",
            "inputMapping": {},
            "requiredPermissions": ["retail.replenishment.approve"]
        }],
        "permissions": []
    }))
    .expect("widget envelope should decode");

    let diagnostics = validate_artifact(&artifact)
        .expect_err("widget action binding should require graph dependency");
    assert!(
        diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code == "widget.validation.missing_dependency")
    );
}

#[test]
fn valid_legacy_widget_fixture_and_null_projection_hints_reported_once() {
    let raw: Value = serde_json::from_str(include_str!(
        "../../../fixtures/ir/retail-replenishment.widget.json"
    ))
    .expect("widget fixture JSON");
    let valid: ArtifactEnvelope = serde_json::from_value(raw.clone()).expect("widget decodes");
    assert_eq!(validate_artifact(&valid), Ok(()));

    let mut null_hints = raw.clone();
    null_hints["payload"]["projectionHints"] = Value::Null;
    let null_env: ArtifactEnvelope =
        serde_json::from_value(null_hints).expect("widget with null hints decodes");
    let errs = validate_artifact(&null_env).expect_err("null projectionHints rejected");
    assert_eq!(errs.len(), 1, "{errs:?}");
    assert_eq!(errs[0].path.as_deref(), Some("payload.projectionHints"));

    let mut omitted = raw;
    omitted["payload"]
        .as_object_mut()
        .expect("payload object")
        .remove("projectionHints");
    let omitted_env: ArtifactEnvelope =
        serde_json::from_value(omitted).expect("widget without hints decodes");
    assert_eq!(validate_artifact(&omitted_env), Ok(()));
}

#[test]
fn explain_admits_valid_widget_and_refuses_invalid_widget() {
    let mut base_artifacts: Vec<ArtifactEnvelope> =
        serde_json::from_str(include_str!("../../../fixtures/law-ir/entities.json"))
            .expect("entities");
    let laws: Vec<ArtifactEnvelope> =
        serde_json::from_str(include_str!("../../../fixtures/law-ir/laws.json")).expect("laws");
    base_artifacts.extend(laws);

    let valid_widget = widget_artifact(&json!({
        "widgetVersion": "0.1",
        "widgetId": "retail.widget.valid",
        "dataDependencies": [],
        "viewStates": [{ "stateId": "ready", "kind": "ready" }],
        "actionBindings": [],
        "permissions": []
    }))
    .expect("valid widget");
    let invalid_widget = widget_artifact(&json!({
        "widgetVersion": "0.1",
        "widgetId": "retail.widget.invalid",
        "dataDependencies": [{
            "dependencyId": "edge",
            "objectKind": "retail.StockPosition",
            "query": { "kind": "relation_traversal" }
        }],
        "viewStates": [{ "stateId": "ready", "kind": "ready" }],
        "actionBindings": [],
        "permissions": []
    }))
    .expect("invalid widget");

    let registry = BTreeSet::new();
    let context = EntityArtifactSetContext {
        registry: &registry,
        base: &[],
        tenant: None,
    };

    let mut with_valid = base_artifacts.clone();
    with_valid.push(valid_widget);
    let report = explain(
        &with_valid,
        &context,
        ExplainKind::Law,
        "erpnext.no_unpaid_ship",
    )
    .expect("explain admits pack with valid widget");
    assert_eq!(report.laws.len(), 1);

    let mut with_invalid = base_artifacts;
    with_invalid.push(invalid_widget);
    let diagnostics = explain(
        &with_invalid,
        &context,
        ExplainKind::Law,
        "erpnext.no_unpaid_ship",
    )
    .expect_err("explain refuses pack with invalid widget");
    assert!(
        diagnostics
            .iter()
            .any(|d| { d.is_error() && d.code.as_str() == "widget.validation.invalid_payload" })
    );
}
