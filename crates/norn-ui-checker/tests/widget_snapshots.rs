//! Widget envelope validation and invalid-fixture snapshot parity tests.

use std::{fs, path::Path};

use norn_checker::{codes, diagnostics_to_json};
use norn_ir::{ArtifactEnvelope, ArtifactKind};
use norn_ui_checker::{check_artifact_json, validate_artifact};
use serde_json::{Value, json};

fn repo_root() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("crate sits two levels below the repo root")
}

fn read_json(path: &Path) -> Value {
    let text = fs::read_to_string(path).expect("fixture should be readable");
    serde_json::from_str(&text).expect("fixture should be JSON")
}

fn widget_artifact(payload: &Value) -> ArtifactEnvelope {
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
    .expect("widget envelope should decode")
}

#[test]
fn invalid_widget_fixtures_match_declared_code_pointer_and_snapshot() {
    let dir = repo_root().join("fixtures/invalid");
    let manifest = read_json(&dir.join("manifest.json"));
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

        let diagnostics = check_artifact_json(&read_json(&dir.join(file)));
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
        let checked_in = read_json(&snapshots.join(file));
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
    }));

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
    }));

    let diagnostics = validate_artifact(&artifact)
        .expect_err("widget action binding should require graph dependency");
    assert!(
        diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code == "widget.validation.missing_dependency")
    );
}
