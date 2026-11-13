//! UI-LIFECYCLE-1 (#1236): a page machine composed with the lifecycle it acts on. The purchase
//! order fixture's `status` lifecycle and its approval decision (which can emit `submit` and
//! `approve`) back a page that submits an order.

#![allow(clippy::expect_used, reason = "fixtures")]

use std::collections::BTreeSet;

use norn_checker::{Diagnostic, EntityArtifactSetContext, check_artifact_set};
use norn_ir::ArtifactEnvelope;
use serde_json::{Value, json};

const FIXTURES: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../fixtures");
const ACTION_ID: &str = "commerce.submit_purchase_order.action";
const WIDGET_ID: &str = "commerce.submit_order.widget";

fn read_json(path: &str) -> Value {
    serde_json::from_str(&std::fs::read_to_string(format!("{FIXTURES}/{path}")).expect("fixture"))
        .expect("fixture parses")
}

fn artifact(kind: &str, id: &str, payload: Value) -> Value {
    json!({
        "irVersion": "0.1", "artifactId": id, "kind": kind,
        "namespace": "commerce", "name": id, "version": "0.1.0", "lifecycle": "draft",
        "scope": { "orgId": "org_demo", "environmentId": "dev" },
        "dependencies": [], "provenance": { "source": "authoring_sdk" }, "payload": payload
    })
}

/// The action that submits an order, deciding with the approval rule.
fn action() -> Value {
    artifact(
        "action",
        ACTION_ID,
        json!({
            "actionId": "commerce.submit_purchase_order", "scopeRequired": ["orgId", "environmentId"],
            "idempotent": true, "ledgered": true, "ledgerCategory": "commerce",
            "inputSchema": { "type": "object", "properties": {}, "additionalProperties": false },
            "outputSchema": { "type": "object", "properties": {}, "additionalProperties": false },
            "synchronousRule": { "artifactId": "commerce.purchase_order_approval.rule", "version": "0.1.0" }
        }),
    )
}

/// A page: `open` (idle) → submit → `submitting` (pending on the action) → accepted → `done`
/// (terminal); every other outcome returns to `open`.
fn machine(lifecycle: &Value) -> Value {
    let outcome = |id: &str| json!({ "eventId": id, "source": { "kind": "outcome", "outcome": id } });
    let back = |id: &str| json!({ "transitionId": format!("on-{id}"), "from": "submitting", "on": id, "target": "open" });
    json!({
        "machineVersion": "0.1",
        "initialStateId": "open",
        "fields": [],
        "events": [
            { "eventId": "submit", "source": { "kind": "user", "nodeId": "invoke" } },
            outcome("accepted"), outcome("refused"), outcome("pending_approval"),
            outcome("network_failure"), outcome("timeout")
        ],
        "states": [
            { "stateId": "open", "role": "idle" },
            { "stateId": "submitting", "role": "pending", "deadlineMs": 15000, "awaits": { "kind": "action", "bindingId": "invoke" } },
            { "stateId": "done", "role": "terminal" }
        ],
        "transitions": [
            { "transitionId": "submit", "from": "open", "on": "submit", "target": "submitting",
              "effects": [{ "kind": "start", "call": { "kind": "action", "bindingId": "invoke" } }] },
            { "transitionId": "on-accepted", "from": "submitting", "on": "accepted", "target": "done" },
            back("refused"), back("pending_approval"), back("network_failure"), back("timeout")
        ],
        "lifecycle": lifecycle
    })
}

fn widget(lifecycle: &Value) -> Value {
    let node = |id: &str, component: Value, children: Value| {
        json!({ "nodeId": id, "semanticId": id, "component": component, "children": children })
    };
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
                    "root": node("root", json!({ "kind": "section", "section": {
                        "sectionId": "main", "kind": "section", "label": { "en": "Main" },
                        "fields": [], "order": 1
                    }}), json!([node("invoke", json!({ "kind": "action", "placementId": "invoke" }), json!([]))])),
                    "machine": machine(lifecycle)
                }],
                "dataBindings": [], "fields": [], "forms": [], "navigationItems": [], "overrides": [],
                "actionBindings": [{
                    "bindingId": "invoke", "action": { "id": ACTION_ID, "version": "0.1.0" },
                    "inputMapping": {}, "inputs": {}, "requiredCapability": "commerce.submit"
                }],
                "actionPlacements": [{
                    "placementId": "invoke", "bindingId": "invoke", "label": { "en": "Submit" },
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

fn entity(edit: impl FnOnce(&mut Value)) -> Value {
    let mut entity = read_json("ir/entity-v2/artifact-set/commerce-purchase-order.entity.json");
    edit(&mut entity);
    entity
}

/// The `widget.lifecycle.*` findings as (code, pointer, message).
fn lifecycle_findings(lifecycle: &Value, edit: impl FnOnce(&mut Value)) -> Vec<(String, String, String)> {
    let artifacts: Vec<ArtifactEnvelope> = [
        entity(edit),
        read_json("rule-ir-v2/valid/purchase-order-approval.rule.json"),
        action(),
        widget(lifecycle),
    ]
    .into_iter()
    .map(|value| serde_json::from_value(value).expect("artifact decodes"))
    .collect();
    let found: Vec<Diagnostic> = check_artifact_set(
        &artifacts,
        &EntityArtifactSetContext { registry: &BTreeSet::new(), base: &[], tenant: None },
    );
    found
        .into_iter()
        .filter(|finding| finding.code.as_str().starts_with("widget.lifecycle."))
        .map(|finding| {
            let pointer = finding.span.map(|span| format!("{span:?}")).unwrap_or_default();
            (finding.code.as_str().to_owned(), pointer, finding.message)
        })
        .collect()
}

fn codes(found: &[(String, String, String)]) -> Vec<&str> {
    found.iter().map(|(code, _, _)| code.as_str()).collect()
}

fn submit_sync() -> Value {
    json!({ "objectType": "commerce.PurchaseOrder", "property": "status",
            "syncs": [{ "transitionId": "on-accepted", "lifecycleTransition": "submit" }] })
}

#[test]
fn a_page_synced_with_a_transition_its_decision_emits_composes_cleanly() {
    let found = lifecycle_findings(&submit_sync(), |_| {});
    assert!(found.is_empty(), "{found:#?}");
}

#[test]
fn a_page_that_shows_done_while_the_object_can_only_come_back_to_draft_is_stuck() {
    // Without `cancel`, a rejected order is revised back to `draft`, where only the page's own
    // submit moves it on; the page already shows `done`, so the pair can never end.
    let found = lifecycle_findings(&submit_sync(), |entity| {
        let lifecycles = entity["payload"]["objectType"]["lifecycles"]
            .as_array_mut()
            .expect("lifecycles");
        for lifecycle in lifecycles {
            if lifecycle["property"] == "status"
                && let Some(transitions) = lifecycle["transitions"].as_array_mut()
            {
                transitions.retain(|transition| transition["id"] != "cancel");
            }
        }
    });
    let stuck: Vec<&(String, String, String)> =
        found.iter().filter(|(code, _, _)| code == "widget.lifecycle.stuck").collect();
    assert!(!stuck.is_empty(), "{found:#?}");
    let (_, pointer, message) = stuck.first().expect("a stuck finding");
    assert!(pointer.contains("/payload/ux/pages/0/machine/states/2"), "{pointer}");
    assert!(message.contains("page state `done` with the object `draft`"), "{message}");
    assert!(
        message.contains("page submit, page on-accepted with object submit, object reject, object revise"),
        "{message}"
    );
}

#[test]
fn a_page_state_shown_where_its_assumption_does_not_hold_is_reported_with_its_run() {
    let mut lifecycle = submit_sync();
    lifecycle["assumes"] = json!([
        { "stateId": "open", "lifecycleStates": ["draft"] },
        { "stateId": "done", "lifecycleStates": ["submitted", "approved", "received"] }
    ]);
    let found = lifecycle_findings(&lifecycle, |_| {});
    assert_eq!(codes(&found), ["widget.lifecycle.assumption_violated"; 2], "{found:#?}");
    let messages: Vec<&str> = found.iter().map(|(_, _, message)| message.as_str()).collect();
    // The page opens only on a draft order, yet another actor can cancel it under the open page;
    // after a submit, the order can be rejected or cancelled under `done`.
    assert!(messages.iter().any(|m| m.contains("page state `open` is shown while the object is `cancelled`")
        && m.contains("shortest run: object cancel")), "{messages:#?}");
    assert!(messages.iter().any(|m| m.contains("page state `done` is shown while the object is `")
        && m.contains("page on-accepted with object submit")), "{messages:#?}");
}

#[test]
fn a_sync_needs_the_accepted_outcome_of_an_action_whose_decision_emits_the_transition() {
    let mut lifecycle = submit_sync();
    lifecycle["syncs"] = json!([
        { "transitionId": "on-accepted", "lifecycleTransition": "reject" }
    ]);
    let found = lifecycle_findings(&lifecycle, |_| {});
    assert!(
        found.iter().any(|(code, pointer, message)| code == "widget.lifecycle.sync_unfired"
            && pointer.contains("/syncs/0/lifecycleTransition")
            && message.contains("does not name transition `reject`")),
        "{found:#?}"
    );

    lifecycle["syncs"] = json!([{ "transitionId": "submit", "lifecycleTransition": "submit" }]);
    let found = lifecycle_findings(&lifecycle, |_| {});
    assert_eq!(codes(&found), ["widget.lifecycle.sync_not_accepted"], "{found:#?}");

    lifecycle["syncs"] = json!([{ "transitionId": "on-accepted", "lifecycleTransition": "ship" }]);
    let found = lifecycle_findings(&lifecycle, |_| {});
    assert_eq!(codes(&found), ["widget.lifecycle.unknown_reference"], "{found:#?}");
}

#[test]
fn a_page_names_a_lifecycle_its_object_type_declares() {
    let found = lifecycle_findings(
        &json!({ "objectType": "commerce.PurchaseOrder", "property": "shipping_status" }),
        |_| {},
    );
    assert_eq!(codes(&found), ["widget.lifecycle.unknown_lifecycle"], "{found:#?}");
    let found = lifecycle_findings(&json!({ "objectType": "commerce.Invoice", "property": "status" }), |_| {});
    assert_eq!(codes(&found), ["widget.lifecycle.unknown_lifecycle"], "{found:#?}");
}
