#![allow(
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::needless_pass_by_value,
    clippy::panic,
    missing_docs,
    reason = "integration-test crate"
)]

//! Interaction rules over server-driven UI documents (#3372): accessible names,
//! dialog close paths and restore targets, transition/control correspondence and
//! keyboard reachability. Each rule has a positive and a negative document.

use std::collections::BTreeSet;

use norn_checker::{Diagnostic, EntityArtifactSetContext, check_artifact_set};
use norn_ir::ArtifactEnvelope;
use serde_json::{Value, json};

const WIDGET_ID: &str = "test.widget";
const ACTION_ID: &str = "test.action";

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

fn section(id: &str, label: &str) -> Value {
    json!({ "kind": "section", "section": {
        "sectionId": id, "kind": "section", "label": { "en": label }, "fields": [], "order": 1
    }})
}

fn node(id: &str, component: &Value, children: &Value) -> Value {
    json!({ "nodeId": id, "semanticId": id, "component": component, "children": children })
}

fn invoke_node() -> Value {
    node(
        "invoke",
        &json!({ "kind": "action", "placementId": "invoke" }),
        &json!([]),
    )
}

fn page(id: &str, name: &str, root: &Value) -> Value {
    json!({ "pageId": id, "name": name, "contextParams": [], "root": root })
}

/// A one-page document whose root section contains one named action control.
fn widget() -> Value {
    let mut value = artifact(
        "widget",
        WIDGET_ID,
        json!({
            "widgetVersion": "0.1", "widgetId": WIDGET_ID,
            "dataDependencies": [], "viewStates": [{ "stateId": "ready", "kind": "ready" }],
            "actionBindings": [], "permissions": [],
            "ux": {
                "uxVersion": "0.1",
                "pages": [page("main", "Main", &node("root", &section("main", "Main"), &json!([invoke_node()])))],
                "dataBindings": [], "fields": [], "forms": [], "navigationItems": [], "overrides": [],
                "actionBindings": [{
                    "bindingId": "invoke", "action": { "id": ACTION_ID, "version": "0.1.0" },
                    "inputMapping": {}, "inputs": {}, "requiredCapability": "test.invoke"
                }],
                "actionPlacements": [{
                    "placementId": "invoke", "bindingId": "invoke", "label": { "en": "Invoke" },
                    "location": "toolbar", "order": 1, "selectionInputMapping": {},
                    "confirmation": { "severity": "safe", "strength": "none" }
                }]
            }
        }),
    );
    value["dependencies"] = json!([{
        "artifactId": ACTION_ID, "kind": "action", "version": "0.1.0", "required": true
    }]);
    value
}

fn findings(widget: Value) -> Vec<Diagnostic> {
    findings_of([action(), widget])
}

fn findings_of(values: impl IntoIterator<Item = Value>) -> Vec<Diagnostic> {
    let artifacts: Vec<ArtifactEnvelope> = values
        .into_iter()
        .map(|value| serde_json::from_value(value).expect("artifact should decode"))
        .collect();
    check_artifact_set(
        &artifacts,
        &EntityArtifactSetContext {
            registry: &BTreeSet::new(),
            base: &[],
            tenant: None,
        },
    )
}

fn interaction(found: &[Diagnostic]) -> Vec<(&str, &str)> {
    found
        .iter()
        .filter(|finding| finding.code.as_str().starts_with("widget.interaction."))
        .map(|finding| {
            (
                finding.code.as_str(),
                finding
                    .span
                    .as_ref()
                    .map_or("", |span| span.pointer.as_str()),
            )
        })
        .collect()
}

fn has(found: &[Diagnostic], code: &str, pointer: &str) -> bool {
    interaction(found).contains(&(code, pointer))
}

fn none(found: &[Diagnostic]) {
    assert!(
        interaction(found).is_empty(),
        "unexpected interaction findings: {:#?}",
        interaction(found)
    );
}

fn ux(widget: &mut Value) -> &mut Value {
    &mut widget["payload"]["ux"]
}

fn dialog(methods: &Value, children: &Value) -> Value {
    node(
        "dialog",
        &json!({ "kind": "dialog", "blocking": false, "dismissalMethods": methods }),
        children,
    )
}

#[test]
fn the_baseline_document_has_no_interaction_findings() {
    none(&findings(widget()));
}

#[test]
fn a_control_label_without_a_letter_or_digit_is_unnamed() {
    let mut value = widget();
    ux(&mut value)["actionPlacements"][0]["label"] = json!({ "en": "…", "ar": "Run" });
    let found = findings(value);
    assert!(has(
        &found,
        "widget.interaction.unnamed_control",
        "/payload/ux/pages/0/root/children/0"
    ));
}

#[test]
fn a_tab_is_an_interactive_control_that_needs_a_name() {
    let mut value = widget();
    ux(&mut value)["pages"][0]["root"]["children"] = json!([
        node(
            "tab",
            &json!({ "kind": "section", "section": {
                "sectionId": "tab", "kind": "tab", "label": { "en": "--" }, "fields": [], "order": 1
            }}),
            &json!([])
        ),
        invoke_node()
    ]);
    let found = findings(value);
    assert!(has(
        &found,
        "widget.interaction.unnamed_control",
        "/payload/ux/pages/0/root/children/0"
    ));
}

#[test]
fn a_pointer_only_dialog_traps_keyboard_users() {
    let mut value = widget();
    ux(&mut value)["pages"][0]["root"]["children"] =
        json!([dialog(&json!(["scrim", "swipe"]), &json!([invoke_node()]))]);
    let found = findings(value.clone());
    assert!(has(
        &found,
        "widget.interaction.dialog_no_keyboard_close",
        "/payload/ux/pages/0/root/children/0"
    ));
    assert!(has(
        &found,
        "widget.interaction.keyboard_unreachable_control",
        "/payload/ux/pages/0/root/children/0/children/0"
    ));
    ux(&mut value)["pages"][0]["root"]["children"][0]["component"]["dismissalMethods"] =
        json!(["scrim", "escape"]);
    none(&findings(value));
}

#[test]
fn a_dialog_needs_an_invoker_or_a_named_container() {
    let layout = |name: &str, label: &str, inside: &Value| {
        page(
            "main",
            name,
            &node(
                "root",
                &section("main", label),
                &json!([dialog(&json!(["escape"]), inside), invoke_node()]),
            ),
        )
    };
    let mut value = widget();
    // Nothing inside the dialog, an unnamed section and a blank page name.
    ux(&mut value)["pages"][0] = layout("   ", "-", &json!([]));
    let found = findings(value.clone());
    assert!(has(
        &found,
        "widget.interaction.dialog_no_restore_target",
        "/payload/ux/pages/0/root/children/0"
    ));
    // A named page, a named enclosing region, or an action inside each give a target.
    ux(&mut value)["pages"][0] = layout("Main", "-", &json!([]));
    none(&findings(value.clone()));
    ux(&mut value)["pages"][0] = layout("   ", "Region", &json!([]));
    none(&findings(value.clone()));
    let inner = node(
        "inner",
        &json!({ "kind": "action", "placementId": "invoke" }),
        &json!([]),
    );
    ux(&mut value)["pages"][0] = layout("   ", "-", &json!([inner]));
    none(&findings(value));
}

fn nested_dialogs(depth: usize) -> Value {
    let mut inner = invoke_node();
    for level in (0..depth).rev() {
        let mut layer = dialog(&json!(["escape"]), &json!([inner]));
        layer["nodeId"] = json!(format!("dialog{level}"));
        layer["semanticId"] = json!(format!("dialog{level}"));
        inner = layer;
    }
    inner
}

#[test]
fn dialogs_must_stay_within_the_verified_stack_depth() {
    let mut value = widget();
    ux(&mut value)["pages"][0]["root"]["children"] = json!([nested_dialogs(3)]);
    none(&findings(value.clone()));
    ux(&mut value)["pages"][0]["root"]["children"] = json!([nested_dialogs(4)]);
    let found = findings(value);
    assert!(has(
        &found,
        "widget.interaction.dialog_depth_exceeds_verified_bound",
        "/payload/ux/pages/0/root/children/0/children/0/children/0/children/0"
    ));
}

fn two_page_app(widget: &mut Value) {
    let ux = ux(widget);
    ux["pages"] = json!([
        ux["pages"][0].clone(),
        page(
            "other",
            "Other",
            &node("other_root", &section("other", "Other"), &json!([]))
        )
    ]);
    ux["app"] = json!({
        "appId": "demo", "entryPageId": "main",
        "pageRefs": [{ "pageId": "main" }, { "pageId": "other" }],
        "routes": [], "navigationItemIds": []
    });
    ux["navigationItems"] = json!([{
        "itemId": "go_other", "label": { "en": "Other" },
        "destination": { "kind": "page", "pageId": "other", "parameters": {} },
        "requiredCapability": "test.nav"
    }]);
}

#[test]
fn every_app_page_needs_a_control_that_reaches_it() {
    let mut value = widget();
    two_page_app(&mut value);
    let found = findings(value.clone());
    assert!(has(
        &found,
        "widget.interaction.unreachable_page",
        "/payload/ux/pages/1"
    ));
    assert!(has(
        &found,
        "widget.interaction.control_missing_for_transition",
        "/payload/ux/navigationItems/0"
    ));
    // The shell lists the item on every page: the transition has a control.
    ux(&mut value)["app"]["navigationItemIds"] = json!(["go_other"]);
    none(&findings(value.clone()));
    // A navigation node in the entry page's tree is an equally good control.
    ux(&mut value)["app"]["navigationItemIds"] = json!([]);
    ux(&mut value)["pages"][0]["root"]["children"] = json!([
        invoke_node(),
        node(
            "go",
            &json!({ "kind": "navigation", "itemId": "go_other" }),
            &json!([])
        )
    ]);
    none(&findings(value));
}

#[test]
fn a_navigation_control_may_not_lead_outside_the_declared_app_pages() {
    let mut value = widget();
    two_page_app(&mut value);
    ux(&mut value)["app"]["navigationItemIds"] = json!(["go_other"]);
    ux(&mut value)["app"]["pageRefs"] = json!([{ "pageId": "main" }]);
    let found = findings(value);
    assert!(has(
        &found,
        "widget.interaction.control_without_legal_transition",
        "/payload/ux/navigationItems/0"
    ));
}

#[test]
fn a_declared_action_binding_needs_a_control() {
    let mut value = widget();
    ux(&mut value)["actionBindings"]
        .as_array_mut()
        .expect("bindings")
        .push(json!({
            "bindingId": "orphan", "action": { "id": ACTION_ID, "version": "0.1.0" },
            "inputMapping": {}, "inputs": {}, "requiredCapability": "test.invoke"
        }));
    let found = findings(value);
    assert!(has(
        &found,
        "widget.interaction.control_missing_for_transition",
        "/payload/ux/actionBindings/1"
    ));
}

#[test]
fn a_page_reachable_only_by_a_route_is_a_host_entry_not_an_orphan() {
    let mut value = widget();
    two_page_app(&mut value);
    ux(&mut value)["app"]["navigationItemIds"] = json!(["go_other"]);
    ux(&mut value)["navigationItems"] = json!([]);
    ux(&mut value)["app"]["navigationItemIds"] = json!([]);
    assert!(has(
        &findings(value.clone()),
        "widget.interaction.unreachable_page",
        "/payload/ux/pages/1"
    ));
    ux(&mut value)["app"]["routes"] = json!([{
        "routeId": "other", "path": "/other", "pageId": "other", "parameters": []
    }]);
    none(&findings(value));
}

#[test]
fn a_shell_navigation_item_needs_an_accessible_name() {
    let mut value = widget();
    two_page_app(&mut value);
    ux(&mut value)["app"]["navigationItemIds"] = json!(["go_other"]);
    ux(&mut value)["navigationItems"][0]["label"] = json!({ "en": "…" });
    let found = findings(value);
    assert!(has(
        &found,
        "widget.interaction.unnamed_control",
        "/payload/ux/navigationItems/0"
    ));
}

#[test]
fn a_permanently_hidden_navigation_item_is_not_a_control() {
    let mut value = widget();
    two_page_app(&mut value);
    ux(&mut value)["app"]["navigationItemIds"] = json!(["go_other"]);
    ux(&mut value)["navigationItems"][0]["visibleWhen"] =
        json!({ "expression": "false", "inputs": {} });
    let found = findings(value);
    assert!(has(
        &found,
        "widget.interaction.control_missing_for_transition",
        "/payload/ux/navigationItems/0"
    ));
    assert!(has(
        &found,
        "widget.interaction.unreachable_page",
        "/payload/ux/pages/1"
    ));
}

#[test]
fn a_label_override_is_the_name_the_user_gets() {
    let mut value = widget();
    ux(&mut value)["overrides"] = json!([{
        "targetSemanticId": "invoke",
        "value": { "kind": "label", "value": { "en": "…" } }
    }]);
    let found = findings(value);
    assert!(has(
        &found,
        "widget.interaction.unnamed_control",
        "/payload/ux/pages/0/root/children/0"
    ));
}

/// A field shown read-only in the baseline, labelled without a letter or digit.
fn with_unnamed_display_field(value: &mut Value) {
    ux(value)["fields"] = json!([{
        "fieldId": "note",
        "target": { "kind": "property", "property": { "bindingId": "rows", "propertyId": "note" } },
        "label": { "en": "…" },
        "control": "display"
    }]);
    ux(value)["pages"][0]["root"]["children"]
        .as_array_mut()
        .expect("root children")
        .push(node(
            "note",
            &json!({ "kind": "field", "fieldId": "note" }),
            &json!([]),
        ));
}

#[test]
fn a_control_override_decides_whether_a_field_is_an_input() {
    let pointer = "/payload/ux/pages/0/root/children/1";
    let mut display = widget();
    with_unnamed_display_field(&mut display);
    assert!(
        !has(
            &findings(display.clone()),
            "widget.interaction.unnamed_control",
            pointer
        ),
        "a read-only display is not an input"
    );
    let mut input = display;
    ux(&mut input)["overrides"] = json!([{
        "targetSemanticId": "note",
        "value": { "kind": "control", "control": "text" }
    }]);
    let found = findings(input);
    assert!(
        has(&found, "widget.interaction.unnamed_control", pointer),
        "the override renders a text input, which needs a name: {found:#?}"
    );
}

const BASELINE_ID: &str = "test.baseline";

/// A host whose only page composes the baseline's `main` page; `overrides` are the host's.
fn composing_host(baseline: &Value, overrides: &Value) -> Value {
    let mut host = artifact(
        "widget",
        "test.host",
        json!({
            "widgetVersion": "0.1", "widgetId": "test.host",
            "dataDependencies": [], "viewStates": [{ "stateId": "ready", "kind": "ready" }],
            "actionBindings": [], "permissions": [],
            "ux": {
                "uxVersion": "0.1",
                "pages": [page("host", "Host", &node(
                    "host_root",
                    &json!({ "kind": "composition",
                        "widget": { "id": BASELINE_ID, "version": "0.1.0" }, "pageId": "main" }),
                    &json!([])
                ))],
                "dataBindings": [], "fields": [], "forms": [], "navigationItems": [],
                "overrides": overrides, "actionBindings": [], "actionPlacements": []
            }
        }),
    );
    host["dependencies"] = json!([{
        "artifactId": baseline["artifactId"], "kind": "widget", "version": "0.1.0", "required": true
    }]);
    host
}

fn baseline() -> Value {
    let mut value = widget();
    value["artifactId"] = json!(BASELINE_ID);
    value["payload"]["widgetId"] = json!(BASELINE_ID);
    value
}

#[test]
fn a_host_label_override_of_a_composed_control_is_checked() {
    let base = baseline();
    let label = |text: &str| {
        json!([{
            "targetSemanticId": "invoke",
            "value": { "kind": "label", "value": { "en": text } }
        }])
    };
    let clean = findings_of([action(), base.clone(), composing_host(&base, &label("Run"))]);
    assert!(
        !clean.iter().any(
            |finding| finding.code.as_str() == "widget.interaction.unnamed_control"
                && finding.artifact_id.as_deref() == Some("test.host")
        ),
        "a named override is fine: {clean:#?}"
    );
    let blank = findings_of([action(), base.clone(), composing_host(&base, &label("…"))]);
    let host: Vec<_> = blank
        .iter()
        .filter(|finding| finding.artifact_id.as_deref() == Some("test.host"))
        .cloned()
        .collect();
    assert!(
        has(
            &host,
            "widget.interaction.unnamed_control",
            "/payload/ux/pages/0/root"
        ),
        "{host:#?}"
    );
}

#[test]
fn a_reusable_page_outside_the_app_is_not_stranded() {
    let mut value = widget();
    two_page_app(&mut value);
    // `other` is reachable; `shared` is a reusable page that another widget composes, not an app page.
    ux(&mut value)["app"]["navigationItemIds"] = json!(["go_other"]);
    ux(&mut value)["pages"]
        .as_array_mut()
        .expect("pages")
        .push(page(
            "shared",
            "Shared",
            &node(
                "shared_root",
                &section("shared", "Shared"),
                &json!([node(
                    "shared_invoke",
                    &json!({ "kind": "action", "placementId": "invoke" }),
                    &json!([])
                )]),
            ),
        ));
    none(&findings(value));
}
