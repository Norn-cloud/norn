#![allow(
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::needless_pass_by_value,
    clippy::panic,
    missing_docs,
    reason = "integration-test crate"
)]

//! Admission tests for UI effect subtraction and invocation provenance (#3178).

use std::collections::BTreeSet;

use norn_checker::{
    Diagnostic, EffectiveCatalog, EntityArtifactSetContext, check_artifact_set,
    generated_transition_enabled_when,
};
use norn_ir::{ActionRoot, ArtifactEnvelope};
use serde_json::{Value, json};

const WIDGET_ID: &str = "test.widget";
const ACTION_ID: &str = "test.action";
const ACTION_PATH: &str = "/payload/ux/pages/0/root/children/0";

fn artifact(kind: &str, id: &str, payload: Value) -> Value {
    json!({
        "irVersion": "0.1", "artifactId": id, "kind": kind,
        "namespace": "test", "name": id, "version": "0.1.0", "lifecycle": "draft",
        "scope": { "orgId": "org_demo", "environmentId": "dev" },
        "dependencies": [], "provenance": { "source": "authoring_sdk" }, "payload": payload
    })
}

fn action() -> Value {
    artifact(
        "action",
        ACTION_ID,
        json!({
            "actionId": "test.invoke", "scopeRequired": ["orgId", "environmentId"],
            "idempotent": false, "ledgered": true, "ledgerCategory": "test",
            "inputSchema": { "type": "object", "properties": {}, "additionalProperties": false },
            "outputSchema": { "type": "object", "properties": {}, "additionalProperties": false }
        }),
    )
}

fn external_action() -> Value {
    let mut value = action();
    value["payload"]["commands"] = json!([{
        "connector": "test.remote", "operation": "create", "doctype": "Order", "fields": {}
    }]);
    value
}

fn confirmation(strength: &str) -> Value {
    let mut value = json!({ "severity": "safe", "strength": strength });
    if strength == "typed" {
        value["phrase"] = json!({ "en": "DELETE" });
    }
    value
}

fn outcome() -> Value {
    json!({ "kind": "outcome_surface", "states": ["pending", "success", "failure", "unknown_outcome"] })
}

fn node(id: &str, component: &Value, children: &Value) -> Value {
    json!({ "nodeId": id, "semanticId": id, "component": component, "children": children })
}

fn widget() -> Value {
    let mut value = artifact(
        "widget",
        WIDGET_ID,
        json!({
            "widgetVersion": "0.1", "widgetId": WIDGET_ID,
            "dataDependencies": [], "viewStates": [{ "stateId": "ready", "kind": "ready" }],
            "actionBindings": [], "permissions": [],
            "ux": {
                "uxVersion": "0.1", "pages": [{
                    "pageId": "main", "name": "Main", "contextParams": [],
                    "root": node("root", &json!({ "kind": "section", "section": {
                        "sectionId": "main", "kind": "section", "label": { "en": "Main" },
                        "fields": [], "order": 1
                    }}), &json!([node("invoke", &json!({ "kind": "action", "placementId": "invoke" }), &json!([]))]))
                }],
                "dataBindings": [], "fields": [], "forms": [], "navigationItems": [], "overrides": [],
                "actionBindings": [{
                    "bindingId": "invoke", "action": { "id": ACTION_ID, "version": "0.1.0" },
                    "inputMapping": {}, "inputs": {}, "requiredCapability": "test.invoke"
                }],
                "actionPlacements": [{
                    "placementId": "invoke", "bindingId": "invoke", "label": { "en": "Invoke" },
                    "location": "toolbar", "order": 1, "selectionInputMapping": {},
                    "confirmation": confirmation("none")
                }]
            }
        }),
    );
    value["dependencies"] = json!([{
        "artifactId": ACTION_ID, "kind": "action", "version": "0.1.0", "required": true
    }]);
    value
}

fn decode(value: Value) -> ArtifactEnvelope {
    serde_json::from_value(value).expect("artifact should decode")
}

fn findings(widget: Value, action: Value) -> Vec<Diagnostic> {
    findings_with(widget, vec![action], &[])
}

fn findings_with(widget: Value, mut support: Vec<Value>, base: &[Value]) -> Vec<Diagnostic> {
    support.push(widget);
    let artifacts: Vec<_> = support.into_iter().map(decode).collect();
    let base: Vec<_> = base.iter().cloned().map(decode).collect();
    check_artifact_set(
        &artifacts,
        &EntityArtifactSetContext {
            registry: &BTreeSet::new(),
            base: &base,
            tenant: None,
        },
    )
}

fn clean(found: &[Diagnostic]) {
    assert!(found.is_empty(), "unexpected findings: {found:#?}");
}

fn exact(found: &[Diagnostic], pointer: &str, message: &str) {
    assert!(
        found.iter().any(|finding| {
            finding.code.as_str() == "widget.validation.invalid_payload"
                && finding.artifact_id.as_deref() == Some(WIDGET_ID)
                && finding
                    .span
                    .as_ref()
                    .is_some_and(|span| span.pointer == pointer)
                && finding.message == message
        }),
        "expected {pointer}: {message}; got {found:#?}"
    );
}

fn residual(found: &[Diagnostic], pointer: &str, effect: &str) {
    exact(
        found,
        pointer,
        &format!("UI law: action {ACTION_ID:?} has unhandled {effect} at render path {pointer}"),
    );
}

#[test]
fn irreversible_requires_a_valid_typed_phrase_at_the_invocation() {
    let mut action = action();
    action["payload"]["uiEffects"] = json!({ "effectsVersion": "0.1", "irreversible": true });
    let mut widget = widget();
    residual(
        &findings(widget.clone(), action.clone()),
        ACTION_PATH,
        "irreversible",
    );
    widget["payload"]["ux"]["actionPlacements"][0]["confirmation"] = confirmation("click");
    residual(
        &findings(widget.clone(), action.clone()),
        ACTION_PATH,
        "irreversible",
    );
    widget["payload"]["ux"]["actionPlacements"][0]["confirmation"] = confirmation("typed");
    clean(&findings(widget.clone(), action.clone()));
    widget["payload"]["ux"]["actionPlacements"][0]["confirmation"]["phrase"] = json!({ "en": " " });
    residual(&findings(widget, action), ACTION_PATH, "irreversible");
}

#[test]
fn destructive_requires_click_confirmation_and_ancestors_can_guard_irreversibility() {
    let mut action = action();
    action["payload"]["uiEffects"] = json!({ "effectsVersion": "0.1", "destructive": true });
    let mut widget = widget();
    residual(
        &findings(widget.clone(), action.clone()),
        ACTION_PATH,
        "destructive",
    );
    widget["payload"]["ux"]["actionPlacements"][0]["confirmation"] = confirmation("click");
    clean(&findings(widget.clone(), action.clone()));
    action["payload"]["uiEffects"]["irreversible"] = json!(true);
    residual(
        &findings(widget.clone(), action.clone()),
        ACTION_PATH,
        "irreversible",
    );
    widget["payload"]["ux"]["pages"][0]["root"]["handlers"] = json!([{
        "kind": "confirm_guard", "confirmation": confirmation("typed")
    }]);
    clean(&findings(widget, action));
}

#[test]
fn retry_requires_the_action_idempotency_capability() {
    let mut widget = widget();
    widget["payload"]["ux"]["actionPlacements"][0]["retry"] = json!(true);
    let mut action = action();
    exact(
        &findings(widget.clone(), action.clone()),
        ACTION_PATH,
        "UI law: retry requires idempotent capability for action \"test.action\"",
    );
    action["payload"]["idempotent"] = json!(true);
    clean(&findings(widget, action));
}

#[test]
fn external_requires_one_complete_surface_and_pending() {
    let mut widget = widget();
    widget["payload"]["ux"]["pages"][0]["root"]["handlers"] = json!([
        { "kind": "outcome_surface", "states": ["success", "failure"] },
        { "kind": "outcome_surface", "states": ["pending", "unknown_outcome"] }
    ]);
    residual(
        &findings(widget.clone(), external_action()),
        ACTION_PATH,
        "external",
    );
    widget["payload"]["ux"]["pages"][0]["root"]["handlers"] = json!([outcome()]);
    clean(&findings(widget, external_action()));
}

#[test]
fn siblings_do_not_discharge_each_others_effects() {
    let mut widget = widget();
    let mut sibling = node(
        "sibling",
        &json!({ "kind": "section", "section": {
            "sectionId": "sibling", "kind": "section", "label": { "en": "Sibling" }, "fields": [], "order": 2
        }}),
        &json!([]),
    );
    sibling["handlers"] = json!([outcome()]);
    widget["payload"]["ux"]["pages"][0]["root"]["children"]
        .as_array_mut()
        .expect("children")
        .push(sibling);
    residual(
        &findings(widget.clone(), external_action()),
        ACTION_PATH,
        "external",
    );
    widget["payload"]["ux"]["pages"][0]["root"]["handlers"] = json!([outcome()]);
    clean(&findings(widget, external_action()));
}

#[test]
fn currencies_and_shell_codes_match_exactly() {
    let mut action = action();
    action["payload"]["uiEffects"] = json!({
        "effectsVersion": "0.1", "money": ["currency.USD"], "errorCodes": ["Denied"], "resultCodes": ["Saved"]
    });
    let mut widget = widget();
    widget["payload"]["ux"]["shellHandlers"] = json!({ "handlersVersion": "0.1", "handlers": [
        { "kind": "error_banner", "codes": ["denied"] }, { "kind": "result_surface", "codes": ["Saved"] }
    ] });
    widget["payload"]["ux"]["pages"][0]["root"]["handlers"] = json!([
        { "kind": "outcome_surface", "states": ["success"], "currencyRefs": ["currency.EUR"] }
    ]);
    let found = findings(widget.clone(), action.clone());
    residual(&found, ACTION_PATH, "money(\"currency.USD\")");
    residual(&found, ACTION_PATH, "error(\"Denied\")");
    widget["payload"]["ux"]["shellHandlers"]["handlers"][0]["codes"] = json!(["Denied"]);
    widget["payload"]["ux"]["pages"][0]["root"]["handlers"][0]["currencyRefs"] =
        json!(["currency.USD"]);
    clean(&findings(widget.clone(), action.clone()));
    widget["payload"]["ux"]["shellHandlers"]["handlers"][0]["codes"] =
        json!(["*", "Denied", "Denied"]);
    exact(
        &findings(widget, action),
        "/payload/ux/shellHandlers/handlers/0/codes/0",
        "UI law: references must be exact, nonblank, unique, and contain no wildcard, whitespace, or control characters",
    );
}

fn dialog(widget: &mut Value, blocking: bool, binding: Option<&str>, methods: &Value) {
    widget["payload"]["ux"]["pages"][0]["root"]["component"] = json!({
        "kind": "dialog", "blocking": blocking, "dismissalMethods": methods
    });
    if let Some(binding) = binding {
        widget["payload"]["ux"]["pages"][0]["root"]["component"]["bindingId"] = json!(binding);
    }
}

#[test]
fn ordinary_dialogs_need_dismissal_and_external_dialogs_block() {
    let mut widget = widget();
    dialog(&mut widget, false, None, &json!([]));
    exact(
        &findings(widget.clone(), action()),
        "/payload/ux/pages/0/root/component/dismissalMethods",
        "UI law: every dialog requires at least one terminal dismissal method",
    );
    dialog(&mut widget, false, None, &json!(["escape"]));
    clean(&findings(widget.clone(), action()));
    widget["payload"]["ux"]["pages"][0]["root"]["handlers"] = json!([outcome()]);
    exact(
        &findings(widget.clone(), external_action()),
        "/payload/ux/pages/0/root/component/blocking",
        "UI law: a dialog containing an external invocation must be blocking",
    );
    dialog(&mut widget, true, Some("invoke"), &json!(["visible_close"]));
    clean(&findings(widget, external_action()));
}

#[test]
fn blocking_dialog_requires_a_real_external_invocation_and_local_unknown_surface() {
    let mut widget = widget();
    dialog(&mut widget, true, Some("invoke"), &json!(["visible_close"]));
    exact(
        &findings(widget.clone(), action()),
        "/payload/ux/pages/0/root/component/bindingId",
        "UI law: blocking dialog must bind its actual external descendant invocation; all external descendants must use that binding",
    );
    exact(
        &findings(widget.clone(), external_action()),
        "/payload/ux/pages/0/root/handlers",
        "UI law: blocking dialog requires its own or descendant complete outcome surface including unknown outcome",
    );
    let dialog_node = widget["payload"]["ux"]["pages"][0]["root"].clone();
    let mut root = node(
        "outer",
        &json!({ "kind": "section", "section": {
            "sectionId": "outer", "kind": "section", "label": { "en": "Outer" }, "fields": [], "order": 1
        }}),
        &json!([]),
    );
    root["nodeId"] = json!("outer");
    root["semanticId"] = json!("outer");
    root["children"] = json!([dialog_node]);
    root["handlers"] = json!([outcome()]);
    widget["payload"]["ux"]["pages"][0]["root"] = root;
    exact(
        &findings(widget.clone(), external_action()),
        "/payload/ux/pages/0/root/children/0/handlers",
        "UI law: blocking dialog requires its own or descendant complete outcome surface including unknown outcome",
    );
    widget["payload"]["ux"]["pages"][0]["root"]["children"][0]["children"][0]["handlers"] =
        json!([outcome()]);
    clean(&findings(widget, external_action()));
}

fn with_form(mut widget: Value) -> Value {
    let ux = &mut widget["payload"]["ux"];
    ux["actionPlacements"][0]["formId"] = json!("input");
    ux["forms"] = json!([{
        "formId": "input", "mode": "action_input", "inputNames": [], "submitBindingId": "invoke",
        "submit": { "label": { "en": "Submit" }, "confirmation": confirmation("none") }
    }]);
    ux["pages"][0]["root"]["children"]
        .as_array_mut()
        .expect("children")
        .push(node(
            "form",
            &json!({ "kind": "form", "formId": "input" }),
            &json!([]),
        ));
    widget
}

#[test]
fn form_submission_owns_effects_and_opener_handlers_do_not_cover_it() {
    let mut widget = with_form(widget());
    let form_path = "/payload/ux/pages/0/root/children/1";
    widget["payload"]["ux"]["actionPlacements"][0]["handlers"] = json!([outcome()]);
    residual(
        &findings(widget.clone(), external_action()),
        form_path,
        "external",
    );
    widget["payload"]["ux"]["forms"][0]["submit"]["handlers"] = json!([outcome()]);
    clean(&findings(widget.clone(), external_action()));
    widget["payload"]["ux"]["actionPlacements"][0]["retry"] = json!(true);
    let mut action = external_action();
    action["payload"]["idempotent"] = json!(true);
    exact(
        &findings(widget, action),
        ACTION_PATH,
        "UI law: a form opener cannot expose retry because it invokes no action",
    );
}

#[test]
fn form_submit_confirmation_cannot_be_supplied_by_the_opener() {
    let mut action = action();
    action["payload"]["uiEffects"] = json!({ "effectsVersion": "0.1", "irreversible": true });
    let mut widget = with_form(widget());
    widget["payload"]["ux"]["actionPlacements"][0]["handlers"] = json!([{
        "kind": "confirm_guard", "confirmation": confirmation("typed")
    }]);
    residual(
        &findings(widget.clone(), action.clone()),
        "/payload/ux/pages/0/root/children/1",
        "irreversible",
    );
    widget["payload"]["ux"]["forms"][0]["submit"]["confirmation"] = confirmation("typed");
    clean(&findings(widget, action));
}

fn composing_widget(baseline: &Value) -> Value {
    let mut host = widget();
    host["dependencies"].as_array_mut().expect("dependencies").push(json!({
        "artifactId": baseline["artifactId"], "kind": "widget", "version": "0.1.0", "required": true
    }));
    let ux = &mut host["payload"]["ux"];
    ux["actionBindings"] = json!([]);
    ux["actionPlacements"] = json!([]);
    ux["pages"][0]["root"]["children"] = json!([node(
        "composed",
        &json!({
            "kind": "composition", "widget": { "id": baseline["artifactId"], "version": "0.1.0" }, "pageId": "main"
        }),
        &json!([])
    )]);
    host
}

fn baseline() -> Value {
    let mut baseline = widget();
    baseline["artifactId"] = json!("test.baseline");
    baseline["payload"]["widgetId"] = json!("test.baseline");
    baseline["payload"]["ux"]["pages"][0]["root"]["nodeId"] = json!("baseline-root");
    baseline["payload"]["ux"]["pages"][0]["root"]["semanticId"] = json!("baseline-root");
    baseline
}

#[test]
fn composed_invocations_use_the_host_shell_and_real_render_path() {
    let mut action = action();
    action["payload"]["uiEffects"] = json!({ "effectsVersion": "0.1", "errorCodes": ["Denied"] });
    let mut baseline = baseline();
    baseline["payload"]["ux"]["shellHandlers"] = json!({
        "handlersVersion": "0.1", "handlers": [{ "kind": "error_banner", "codes": ["Denied"] }]
    });
    let mut host = composing_widget(&baseline);
    let path = "/payload/ux/pages/0/root/children/0/composed/main/root/children/0";
    exact(
        &findings_with(host.clone(), vec![action.clone()], &[baseline.clone()]),
        ACTION_PATH,
        &format!(
            "UI law: action {ACTION_ID:?} has unhandled error(\"Denied\") at render path {path}"
        ),
    );
    host["payload"]["ux"]["shellHandlers"] = baseline["payload"]["ux"]["shellHandlers"].clone();
    clean(&findings_with(host, vec![action], &[baseline]));
}

#[test]
fn host_action_override_replaces_the_composed_invocation() {
    let mut dangerous = action();
    dangerous["artifactId"] = json!("test.dangerous.action");
    dangerous["payload"]["actionId"] = json!("test.dangerous");
    dangerous["payload"]["uiEffects"] = json!({ "effectsVersion": "0.1", "irreversible": true });
    let mut baseline = baseline();
    baseline["dependencies"][0]["artifactId"] = json!("test.dangerous.action");
    baseline["payload"]["ux"]["actionBindings"][0]["action"]["id"] = json!("test.dangerous.action");
    let mut host = composing_widget(&baseline);
    let safe = widget();
    host["payload"]["ux"]["actionBindings"] = safe["payload"]["ux"]["actionBindings"].clone();
    host["payload"]["ux"]["actionPlacements"] = safe["payload"]["ux"]["actionPlacements"].clone();
    host["payload"]["ux"]["overrides"] = json!([{
        "targetSemanticId": "invoke", "value": { "kind": "action_placement", "placementId": "invoke" }
    }]);
    clean(&findings_with(
        host.clone(),
        vec![action(), dangerous.clone()],
        &[baseline.clone()],
    ));
    host["payload"]["ux"]["overrides"] = json!([]);
    let found = findings_with(host, vec![action(), dangerous], &[baseline]);
    exact(
        &found,
        ACTION_PATH,
        "UI law: action \"test.dangerous.action\" has unhandled irreversible at render path /payload/ux/pages/0/root/children/0/composed/main/root/children/0",
    );
}

fn transition_entity() -> Value {
    artifact(
        "entity",
        "test.Record.entity",
        json!({
            "entityVersion": "2.0", "objectType": {
                "name": "test.Record", "properties": [
                    { "id": "id", "type": { "kind": "text" } },
                    { "id": "business_key", "type": { "kind": "text" } },
                    { "id": "status", "type": { "kind": "enum", "values": [
                        { "value": "draft" }, { "value": "review" }, { "value": "done" }
                    ] }, "default": "draft" }
                ], "identity": [
                    { "name": "primary", "properties": ["id"] },
                    { "name": "business", "properties": ["business_key"] }
                ],
                "lifecycles": [{
                    "property": "status", "initial": ["draft"], "terminal": ["done"],
                    "transitions": [
                        { "id": "review_enter", "from": ["draft"], "to": "review" },
                        { "id": "finish", "from": ["draft", "review"], "to": "done" }
                    ]
                }]
            }
        }),
    )
}

fn transition_action() -> Value {
    let mut action = action();
    action["payload"]["inputSchema"] = json!({
        "type": "object", "properties": { "id": { "type": "string" } },
        "required": ["id"], "additionalProperties": false
    });
    action["payload"]["writes"] = json!([{ "entityKind": "test.Record", "change": "modified" }]);
    action["payload"]["effectProgram"] = json!({
        "kind": "fire_transition", "objectIdField": "id", "objectKind": "test.Record",
        "entityArtifactId": "test.Record.entity", "entityArtifactVersion": "0.1.0",
        "lifecycle": "status", "transition": "finish"
    });
    action
}

fn transition_widget() -> Value {
    let mut widget = widget();
    widget["dependencies"].as_array_mut().expect("dependencies").push(json!({
        "artifactId": "test.Record.entity", "kind": "entity", "version": "0.1.0", "required": true
    }));
    let ux = &mut widget["payload"]["ux"];
    ux["pages"][0]["root"]["component"] = json!({ "kind": "detail", "dataBindingId": "record" });
    ux["actionPlacements"][0]["location"] = json!("detail");
    ux["dataBindings"] = json!([{
        "bindingId": "record", "object": { "id": "test.Record.entity", "version": "0.1.0" },
        "query": { "kind": "object_detail", "key": [{ "propertyId": "id", "value": { "kind": "literal", "value": "1" } }] }
    }]);
    ux["actionBindings"][0]["inputMapping"] = json!({ "id": "input.recordKey" });
    ux["actionBindings"][0]["inputs"] = json!({ "recordBindingId": "record" });
    widget
}

fn transition_context_widget() -> Value {
    let mut widget = transition_widget();
    let ux = &mut widget["payload"]["ux"];
    ux["pages"][0]["contextParams"] = json!([{
        "paramId": "runtimeId",
        "type": "text",
        "required": true,
        "runtimeKeyOf": { "id": "test.Record.entity", "version": "0.1.0" }
    }]);
    ux["actionBindings"][0]["inputs"]["contextPageId"] = json!("main");
    ux["actionBindings"][0]["inputMapping"]["id"] = json!("input.context.runtimeId");
    widget
}

fn transition_form_widget(location: &str) -> Value {
    let mut widget = transition_widget();
    let ux = &mut widget["payload"]["ux"];
    ux["forms"] = json!([{
        "formId": "transition-form",
        "mode": "action_input",
        "inputNames": [],
        "dataBindingId": "record",
        "submitBindingId": "invoke",
        "submit": {
            "label": { "en": "Submit transition" },
            "confirmation": { "severity": "safe", "strength": "none" }
        }
    }]);
    ux["actionPlacements"] = json!([{
        "placementId": "open-transition-form",
        "bindingId": "invoke",
        "label": { "en": "Open transition form" },
        "formId": "transition-form",
        "location": location,
        "order": 1,
        "selectionBindingId": "record",
        "selectionInputMapping": {},
        "confirmation": { "severity": "safe", "strength": "none" }
    }]);
    ux["dataBindings"][0]["query"] = json!({
        "kind": "object_list",
        "filters": [],
        "sort": [],
        "page": { "size": 10, "maxSize": 10 }
    });
    let root = &mut ux["pages"][0]["root"];
    root["component"] = json!({
        "kind": "section",
        "section": {
            "sectionId": "transition-section",
            "kind": "section",
            "label": { "en": "Transition" },
            "fields": [],
            "order": 1
        }
    });
    root["children"] = json!([
        {
            "nodeId": "transition-list-node",
            "semanticId": "transition-list",
            "component": {
                "kind": "list",
                "dataBindingId": "record",
                "fields": [],
                "selection": if location == "selection" { "single" } else { "none" }
            },
            "children": [node(
                "open-transition-form-node",
                &json!({ "kind": "action", "placementId": "open-transition-form" }),
                &json!([])
            )]
        },
        node(
            "transition-form-node",
            &json!({ "kind": "form", "formId": "transition-form" }),
            &json!([])
        )
    ]);
    widget
}

#[test]
fn lifecycle_conditions_and_authoritative_runtime_targets_are_admitted() {
    let action = transition_action();
    let entity = transition_entity();
    let mut widget = transition_widget();
    clean(&findings_with(
        widget.clone(),
        vec![action.clone(), entity.clone()],
        &[],
    ));
    widget["payload"]["ux"]["actionPlacements"][0]["enabledWhen"] = json!({
        "expression": "input.record.status == \"done\"", "inputs": { "recordBindingId": "record" }
    });
    let found = findings_with(widget.clone(), vec![action.clone(), entity.clone()], &[]);
    exact(
        &found,
        ACTION_PATH,
        "UI law: phase 1 lifecycle enabledWhen must exactly match generated source condition \"input.record.status == \\\"draft\\\" or input.record.status == \\\"review\\\"\" and inputs; extra predicates are unsupported",
    );
    assert!(
        !found
            .iter()
            .any(|finding| finding.message.contains("objectIdField must map directly"))
    );
    widget["payload"]["ux"]["actionPlacements"][0]["enabledWhen"]["expression"] =
        json!("  input.record.status == \"draft\" or input.record.status == \"review\"  ");
    let found = findings_with(widget, vec![action.clone(), entity.clone()], &[]);
    clean(&found);
    let has_condition_mismatch = found
        .iter()
        .any(|finding| finding.message.contains("enabledWhen must exactly match"));
    assert!(!has_condition_mismatch);
    let artifacts = vec![decode(entity)];
    let catalog = EffectiveCatalog::from_artifacts(&artifacts);
    let object = catalog
        .object_type("test.Record.entity")
        .expect("object lookup")
        .expect("effective object");
    let action: ActionRoot =
        serde_json::from_value(action["payload"].clone()).expect("typed action");
    let condition = generated_transition_enabled_when(&action, object, "record")
        .expect("generation")
        .expect("transition");
    assert_eq!(
        condition.expression.as_str(),
        "input.record.status == \"draft\" or input.record.status == \"review\""
    );
    assert_eq!(
        condition.inputs.record_binding_id.as_deref(),
        Some("record")
    );
}

#[test]
fn lifecycle_target_rejects_context_parameter_even_when_its_runtime_key_marker_matches() {
    // The page parameter may be arbitrary URL text B while the invocation's
    // detail record is A; the marker does not pair those values.
    let widget = transition_context_widget();
    exact(
        &findings_with(widget, vec![transition_action(), transition_entity()], &[]),
        ACTION_PATH,
        "UI law: runtime-key context parameters are not supported for transitions yet",
    );

    let mut wrong_type = transition_context_widget();
    wrong_type["payload"]["ux"]["pages"][0]["contextParams"][0]["type"] = json!("integer");
    exact(
        &findings_with(
            wrong_type,
            vec![transition_action(), transition_entity()],
            &[],
        ),
        "/payload/ux/pages/0/contextParams/0/runtimeKeyOf",
        "runtimeKeyOf requires a required text context parameter",
    );

    let mut optional = transition_context_widget();
    optional["payload"]["ux"]["pages"][0]["contextParams"][0]["required"] = json!(false);
    exact(
        &findings_with(
            optional,
            vec![transition_action(), transition_entity()],
            &[],
        ),
        "/payload/ux/pages/0/contextParams/0/runtimeKeyOf",
        "runtimeKeyOf requires a required text context parameter",
    );

    let mut unknown = transition_context_widget();
    unknown["payload"]["ux"]["actionBindings"][0]["inputMapping"]["id"] =
        json!("input.context.missing");
    exact(
        &findings_with(unknown, vec![transition_action(), transition_entity()], &[]),
        ACTION_PATH,
        "UI law: runtime-key context parameters are not supported for transitions yet",
    );

    let mut wrong_entity = transition_context_widget();
    wrong_entity["payload"]["ux"]["pages"][0]["contextParams"][0]["runtimeKeyOf"]["id"] =
        json!("crm.Customer.entity");
    exact(
        &findings_with(
            wrong_entity,
            vec![transition_action(), transition_entity()],
            &[],
        ),
        ACTION_PATH,
        "UI law: runtime-key context parameters are not supported for transitions yet",
    );
}

#[test]
fn lifecycle_form_submit_and_openers_keep_the_same_runtime_record_context() {
    for location in ["row", "selection"] {
        clean(&findings_with(
            transition_form_widget(location),
            vec![transition_action(), transition_entity()],
            &[],
        ));
    }
}

#[test]
fn lifecycle_form_submit_rejects_a_runtime_key_context_parameter() {
    let mut widget = transition_form_widget("row");
    let ux = &mut widget["payload"]["ux"];
    ux["pages"][0]["contextParams"] = json!([{
        "paramId": "runtimeId",
        "type": "text",
        "required": true,
        "runtimeKeyOf": { "id": "test.Record.entity", "version": "0.1.0" }
    }]);
    ux["actionBindings"][0]["inputs"]["contextPageId"] = json!("main");
    ux["actionBindings"][0]["inputMapping"]["id"] = json!("input.context.runtimeId");

    // The row opener captures record A, while a URL context value could be B.
    exact(
        &findings_with(widget, vec![transition_action(), transition_entity()], &[]),
        "/payload/ux/pages/0/root/children/1",
        "UI law: runtime-key context parameters are not supported for transitions yet",
    );
}

#[test]
fn lifecycle_form_cannot_capture_a_row_without_an_opener() {
    let mut widget = transition_form_widget("row");
    widget["payload"]["ux"]["actionPlacements"] = json!([]);
    widget["payload"]["ux"]["pages"][0]["root"]["children"][0]["children"] = json!([]);
    exact(
        &findings_with(widget, vec![transition_action(), transition_entity()], &[]),
        "/payload/ux/forms/0/inputs/recordBindingId",
        "record input requires a detail record, list row, or single-selection context",
    );
}

#[test]
fn lifecycle_transition_fails_closed_without_an_explicit_record_binding() {
    let mut widget = transition_widget();
    widget["payload"]["ux"]["actionBindings"][0]["inputs"] = json!({});
    exact(
        &findings_with(widget, vec![transition_action(), transition_entity()], &[]),
        ACTION_PATH,
        "UI law: lifecycle transition requires an explicit record binding at its invocation",
    );
}

#[test]
fn lifecycle_target_uses_runtime_key_when_business_id_differs() {
    // WidgetIR's record properties do not include the runtime ObjectRecord id.
    let runtime_record = json!({
        "objectType": "test.Record",
        "id": "runtime-A",
        "scope": "org_demo",
        "version": 1,
        "values": { "id": "business-B", "business_key": "external-B", "status": "draft" }
    });
    assert_ne!(runtime_record["id"], runtime_record["values"]["id"]);

    let mut widget = transition_widget();
    widget["payload"]["ux"]["dataBindings"][0]["query"]["key"][0]["value"]["value"] =
        runtime_record["values"]["id"].clone();
    assert_eq!(
        widget["payload"]["ux"]["actionBindings"][0]["inputMapping"]["id"],
        "input.recordKey"
    );
    clean(&findings_with(
        widget.clone(),
        vec![transition_action(), transition_entity()],
        &[],
    ));
    widget["payload"]["ux"]["actionBindings"][0]["inputMapping"]["id"] = json!("input.record.id");
    exact(
        &findings_with(widget, vec![transition_action(), transition_entity()], &[]),
        ACTION_PATH,
        "UI law: lifecycle transition objectIdField must map directly from input.recordKey",
    );
}

#[test]
fn lifecycle_target_rejects_other_single_property_identity_keys_too() {
    let mut widget = transition_widget();
    widget["payload"]["ux"]["dataBindings"][0]["query"]["key"][0]["propertyId"] =
        json!("business_key");
    widget["payload"]["ux"]["dataBindings"][0]["query"]["key"][0]["value"]["value"] =
        json!("external-B");
    widget["payload"]["ux"]["actionBindings"][0]["inputMapping"]["id"] =
        json!("input.record.business_key");
    exact(
        &findings_with(widget, vec![transition_action(), transition_entity()], &[]),
        ACTION_PATH,
        "UI law: lifecycle transition objectIdField must map directly from input.recordKey",
    );
}

#[test]
fn composed_dialog_cannot_bind_a_host_override_by_coincident_binding_id() {
    let mut baseline = baseline();
    dialog(
        &mut baseline,
        true,
        Some("invoke"),
        &json!(["visible_close"]),
    );
    baseline["payload"]["ux"]["pages"][0]["root"]["handlers"] = json!([outcome()]);
    let mut host = composing_widget(&baseline);
    let local = widget();
    host["payload"]["ux"]["actionBindings"] = local["payload"]["ux"]["actionBindings"].clone();
    host["payload"]["ux"]["actionPlacements"] = local["payload"]["ux"]["actionPlacements"].clone();
    host["payload"]["ux"]["overrides"] = json!([{
        "targetSemanticId": "invoke", "value": { "kind": "action_placement", "placementId": "invoke" }
    }]);
    exact(
        &findings_with(host, vec![external_action()], &[baseline]),
        ACTION_PATH,
        "UI law: blocking dialog must bind its actual external descendant invocation; all external descendants must use that binding",
    );
}

#[test]
fn lifecycle_target_identity_cannot_come_from_an_unrelated_constant() {
    let mut widget = transition_widget();
    widget["payload"]["ux"]["actionBindings"][0]["inputMapping"]["id"] =
        json!("\"another-record\"");
    exact(
        &findings_with(widget, vec![transition_action(), transition_entity()], &[]),
        ACTION_PATH,
        "UI law: lifecycle transition objectIdField must map directly from input.recordKey",
    );
}

#[test]
fn malformed_base_action_roots_fail_closed_at_the_widget_binding() {
    let mut action = action();
    action["payload"]["uiEffects"] = json!({ "effectsVersion": "0.1", "errorCodes": ["*"] });
    exact(
        &findings_with(widget(), vec![], &[action]),
        "/payload/ux/actionBindings/0/action",
        "UI law: referenced ActionIR \"test.action\" is malformed; effect admission fails closed",
    );
}

#[test]
fn every_invocation_starts_with_a_fresh_requirement_set() {
    let mut widget = widget();
    let mut guarded = widget["payload"]["ux"]["actionPlacements"][0].clone();
    guarded["placementId"] = json!("guarded");
    guarded["handlers"] = json!([outcome()]);
    widget["payload"]["ux"]["actionPlacements"]
        .as_array_mut()
        .expect("placements")
        .push(guarded);
    widget["payload"]["ux"]["pages"][0]["root"]["children"]
        .as_array_mut()
        .expect("children")
        .insert(
            0,
            node(
                "guarded",
                &json!({ "kind": "action", "placementId": "guarded" }),
                &json!([]),
            ),
        );
    residual(
        &findings(widget, external_action()),
        "/payload/ux/pages/0/root/children/1",
        "external",
    );
}

#[test]
fn a_shell_cannot_subtract_irreversibility_or_external_outcomes() {
    let mut widget = widget();
    widget["payload"]["ux"]["shellHandlers"] = json!({
        "handlersVersion": "0.1", "handlers": [outcome()]
    });
    let found = findings(widget, external_action());
    exact(
        &found,
        "/payload/ux/shellHandlers/handlers/0",
        "UI law: shell handlers may handle only exact error and result codes",
    );
    residual(&found, ACTION_PATH, "external");
}

#[test]
fn generated_lifecycle_conditions_use_effective_inherited_narrowing() {
    let base = transition_entity();
    let child = artifact(
        "entity",
        "test.NarrowRecord.entity",
        json!({
            "entityVersion": "2.0", "objectType": {
                "name": "test.NarrowRecord", "extends": "test.Record",
                "properties": [{ "id": "status", "type": { "kind": "enum", "values": [
                    { "value": "draft" }, { "value": "done" }
                ] }, "default": "draft" }]
            }
        }),
    );
    let artifacts = vec![decode(base.clone()), decode(child.clone())];
    let catalog = EffectiveCatalog::from_artifacts(&artifacts);
    let object = catalog
        .object_type("test.NarrowRecord.entity")
        .expect("lookup")
        .expect("effective child");
    let action: ActionRoot =
        serde_json::from_value(transition_action()["payload"].clone()).expect("action");
    let generated = generated_transition_enabled_when(&action, object, "record")
        .expect("generation")
        .expect("transition");
    assert_eq!(
        generated.expression.as_str(),
        "input.record.status == \"draft\""
    );
    let mut widget = transition_widget();
    widget["dependencies"].as_array_mut().expect("dependencies").push(json!({
        "artifactId": "test.NarrowRecord.entity", "kind": "entity", "version": "0.1.0", "required": true
    }));
    widget["payload"]["ux"]["dataBindings"][0]["object"]["id"] = json!("test.NarrowRecord.entity");
    clean(&findings_with(
        widget,
        vec![transition_action(), base, child],
        &[],
    ));
}

#[test]
fn lifecycle_extra_predicates_and_unrelated_record_surfaces_fail_closed() {
    let mut widget = transition_widget();
    widget["payload"]["ux"]["actionPlacements"][0]["enabledWhen"] = json!({
        "expression": "(input.record.status == \"draft\" or input.record.status == \"review\") and input.record.id != \"\"",
        "inputs": { "recordBindingId": "record" }
    });
    let found = findings_with(
        widget.clone(),
        vec![transition_action(), transition_entity()],
        &[],
    );
    exact(
        &found,
        ACTION_PATH,
        "UI law: phase 1 lifecycle enabledWhen must exactly match generated source condition \"input.record.status == \\\"draft\\\" or input.record.status == \\\"review\\\"\" and inputs; extra predicates are unsupported",
    );
    assert!(
        !found
            .iter()
            .any(|finding| finding.message.contains("objectIdField must map directly"))
    );
    let mut other = widget["payload"]["ux"]["dataBindings"][0].clone();
    other["bindingId"] = json!("other");
    widget["payload"]["ux"]["dataBindings"]
        .as_array_mut()
        .expect("bindings")
        .push(other);
    widget["payload"]["ux"]["pages"][0]["root"]["component"]["dataBindingId"] = json!("other");
    exact(
        &findings_with(widget, vec![transition_action(), transition_entity()], &[]),
        ACTION_PATH,
        "UI law: lifecycle transition record binding must match the actual record surface",
    );
}

#[test]
fn only_the_selected_composed_page_emits_effect_requirements() {
    let mut baseline = baseline();
    let mut hidden = baseline["payload"]["ux"]["pages"][0].clone();
    hidden["pageId"] = json!("hidden");
    hidden["root"]["nodeId"] = json!("hidden-root");
    hidden["root"]["semanticId"] = json!("hidden-root");
    hidden["root"]["component"]["section"]["sectionId"] = json!("hidden");
    hidden["root"]["children"][0]["nodeId"] = json!("hidden-action");
    hidden["root"]["children"][0]["semanticId"] = json!("hidden-action");
    baseline["payload"]["ux"]["pages"][0]["root"]["children"][0]["handlers"] = json!([outcome()]);
    baseline["payload"]["ux"]["pages"]
        .as_array_mut()
        .expect("pages")
        .push(hidden);
    let host = composing_widget(&baseline);
    clean(&findings_with(
        host.clone(),
        vec![external_action()],
        &[baseline.clone()],
    ));
    baseline["payload"]["ux"]["pages"][1]["root"]["children"][0]["component"]["placementId"] =
        json!("missing-placement");
    exact(
        &findings_with(host, vec![external_action()], &[baseline]),
        "/payload/ux/pages/0/root/children/0/component",
        "composed widget page or one of its referenced pages fails typed UX validation",
    );
}

#[test]
fn a_blocking_dialog_still_requires_a_terminal_dismissal_method() {
    let mut widget = widget();
    dialog(&mut widget, true, Some("invoke"), &json!([]));
    widget["payload"]["ux"]["pages"][0]["root"]["handlers"] = json!([outcome()]);
    exact(
        &findings(widget.clone(), external_action()),
        "/payload/ux/pages/0/root/component/dismissalMethods",
        "UI law: every dialog requires at least one terminal dismissal method",
    );
    widget["payload"]["ux"]["pages"][0]["root"]["component"]["dismissalMethods"] =
        json!(["visible_close"]);
    clean(&findings(widget, external_action()));
}

#[test]
fn currencies_require_one_surface_covering_the_original_full_set() {
    let mut action = action();
    action["payload"]["uiEffects"] = json!({
        "effectsVersion": "0.1", "money": ["currency.USD", "currency.EUR"]
    });
    let mut widget = widget();
    widget["payload"]["ux"]["pages"][0]["root"]["children"][0]["handlers"] = json!([
        { "kind": "outcome_surface", "states": ["success"], "currencyRefs": ["currency.USD"] }
    ]);
    widget["payload"]["ux"]["pages"][0]["root"]["handlers"] = json!([
        { "kind": "outcome_surface", "states": ["success"], "currencyRefs": ["currency.EUR"] }
    ]);
    let found = findings(widget.clone(), action.clone());
    residual(&found, ACTION_PATH, "money(\"currency.USD\")");
    residual(&found, ACTION_PATH, "money(\"currency.EUR\")");
    widget["payload"]["ux"]["pages"][0]["root"]["handlers"][0]["currencyRefs"] =
        json!(["currency.USD", "currency.EUR"]);
    clean(&findings(widget, action));
}

#[test]
fn external_currencies_and_terminal_states_must_share_one_surface() {
    let mut action = external_action();
    action["payload"]["uiEffects"] = json!({
        "effectsVersion": "0.1", "money": ["currency.USD", "currency.EUR"]
    });
    let mut widget = widget();
    let mut usd = outcome();
    usd["currencyRefs"] = json!(["currency.USD"]);
    let mut eur = outcome();
    eur["currencyRefs"] = json!(["currency.EUR"]);
    widget["payload"]["ux"]["pages"][0]["root"]["children"][0]["handlers"] = json!([usd]);
    widget["payload"]["ux"]["pages"][0]["root"]["handlers"] = json!([eur]);
    let found = findings(widget.clone(), action.clone());
    residual(&found, ACTION_PATH, "external");
    residual(&found, ACTION_PATH, "money(\"currency.USD\")");
    residual(&found, ACTION_PATH, "money(\"currency.EUR\")");
    widget["payload"]["ux"]["pages"][0]["root"]["children"][0]["handlers"] = json!([outcome()]);
    widget["payload"]["ux"]["pages"][0]["root"]["handlers"] = json!([{
        "kind": "outcome_surface", "states": ["pending"], "currencyRefs": ["currency.USD", "currency.EUR"]
    }]);
    let found = findings(widget.clone(), action.clone());
    residual(&found, ACTION_PATH, "external");
    residual(&found, ACTION_PATH, "money(\"currency.USD\")");
    residual(&found, ACTION_PATH, "money(\"currency.EUR\")");
    let mut complete = outcome();
    complete["currencyRefs"] = json!(["currency.USD", "currency.EUR"]);
    widget["payload"]["ux"]["pages"][0]["root"]["handlers"] = json!([complete]);
    clean(&findings(widget, action));
}

#[test]
fn a_malformed_action_is_reported_once_at_each_render_site() {
    let mut action = action();
    action["payload"]["uiEffects"] = json!({ "effectsVersion": "0.1", "errorCodes": ["*"] });
    let found = findings_with(widget(), vec![], &[action]);
    assert_eq!(
        found
            .iter()
            .filter(|finding| {
                finding.artifact_id.as_deref() == Some(WIDGET_ID)
                    && finding
                        .span
                        .as_ref()
                        .is_some_and(|span| span.pointer == ACTION_PATH)
                    && finding.message.contains("referenced ActionIR")
            })
            .count(),
        1
    );
}
