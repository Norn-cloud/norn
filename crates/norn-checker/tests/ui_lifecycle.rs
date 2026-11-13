//! UI-LIFECYCLE-1 (#1236): a page machine composed with the lifecycle it acts on. The purchase
//! order fixture's `status` lifecycle and its approval decision (which can emit `submit` and
//! `approve`) back a page that submits an order.

#![allow(
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::needless_pass_by_value,
    reason = "JSON fixtures built and edited in place"
)]

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
    let outcome =
        |id: &str| json!({ "eventId": id, "source": { "kind": "outcome", "outcome": id } });
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
    let node = |id: &str, component: Value, children: Value| json!({ "nodeId": id, "semanticId": id, "component": component, "children": children });
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
fn lifecycle_findings(
    lifecycle: &Value,
    edit: impl FnOnce(&mut Value),
) -> Vec<(String, String, String)> {
    lifecycle_diagnostics(lifecycle, edit)
        .into_iter()
        .map(|finding| {
            let pointer = finding.span.map(|span| span.pointer).unwrap_or_default();
            (finding.code.as_str().to_owned(), pointer, finding.message)
        })
        .collect()
}

/// The `widget.lifecycle.*` findings as their wire JSON (with any witness).
fn lifecycle_json(lifecycle: &Value, edit: impl FnOnce(&mut Value)) -> Vec<Value> {
    lifecycle_diagnostics(lifecycle, edit)
        .iter()
        .map(Diagnostic::to_json)
        .collect()
}

fn lifecycle_diagnostics(lifecycle: &Value, edit: impl FnOnce(&mut Value)) -> Vec<Diagnostic> {
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
        &EntityArtifactSetContext {
            registry: &BTreeSet::new(),
            base: &[],
            tenant: None,
        },
    );
    found
        .into_iter()
        .filter(|finding| finding.code.as_str().starts_with("widget.lifecycle."))
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
    // When a draft cannot be cancelled, a rejected order revised back to `draft` waits for the
    // page's own submit; the page already shows `done`, so neither pair can ever end. The planted
    // case is (`done`, `draft`); (`done`, `rejected`) is stuck with it. Exactly these two, each
    // with its witness.
    let found = lifecycle_json(&submit_sync(), |entity| {
        let lifecycles = entity["payload"]["objectType"]["lifecycles"]
            .as_array_mut()
            .expect("lifecycles");
        for lifecycle in lifecycles {
            if lifecycle["property"] == "status"
                && let Some(transitions) = lifecycle["transitions"].as_array_mut()
            {
                for transition in transitions.iter_mut() {
                    if transition["id"] == "cancel" {
                        transition["from"] = json!(["submitted", "approved"]);
                    }
                }
            }
        }
    });
    let mut stuck: Vec<Value> = found
        .iter()
        .filter(|finding| finding["code"] == "widget.lifecycle.stuck")
        .map(|finding| {
            let steps: Vec<Value> = finding["witness"]["steps"]
                .as_array()
                .expect("a witness")
                .iter()
                .map(|step| {
                    json!(step["moves"]
                        .as_array()
                        .expect("moves")
                        .iter()
                        .map(|one| json!([one["side"], one["label"]]))
                        .collect::<Vec<_>>())
                })
                .collect();
            json!({ "message": finding["message"], "pointer": finding["span"]["pointer"], "witness": steps })
        })
        .collect();
    stuck.sort_by_key(|finding| finding["message"].to_string());
    assert_eq!(
        json!(stuck),
        json!([
            {
                "message": "page state `done` with the object `draft`: no ending of both is reachable; shortest run: page submit, page on-accepted with object submit, object reject, object revise",
                "pointer": "/payload/ux/pages/0/machine/states/2",
                "witness": [[["ui", "submit"]], [["ui", "on-accepted"], ["backend", "submit"]], [["backend", "reject"]], [["backend", "revise"]]]
            },
            {
                "message": "page state `done` with the object `rejected`: no ending of both is reachable; shortest run: page submit, page on-accepted with object submit, object reject",
                "pointer": "/payload/ux/pages/0/machine/states/2",
                "witness": [[["ui", "submit"]], [["ui", "on-accepted"], ["backend", "submit"]], [["backend", "reject"]]]
            }
        ]),
        "{found:#?}"
    );
}

#[test]
fn a_page_state_shown_where_its_assumption_does_not_hold_is_reported_with_its_run() {
    let mut lifecycle = submit_sync();
    lifecycle["assumes"] = json!([
        { "stateId": "open", "lifecycleStates": ["draft"] },
        { "stateId": "done", "lifecycleStates": ["submitted", "approved", "received"] }
    ]);
    let found = lifecycle_json(&lifecycle, |_| {});
    // The page opens only on a draft order, yet another actor can cancel it under the open page;
    // after a submit the order can be rejected, cancelled or revised back to a draft under `done`.
    // Every violating pair is reported, each with its own witness.
    let mut messages: Vec<String> = found
        .iter()
        .map(|finding| {
            assert_eq!(finding["code"], "widget.lifecycle.assumption_violated", "{finding:#}");
            assert!(
                finding["witness"]["steps"]
                    .as_array()
                    .is_some_and(|steps| !steps.is_empty()),
                "{finding:#}"
            );
            finding["message"].as_str().unwrap_or_default().to_owned()
        })
        .collect();
    messages.sort_unstable();
    let shown = |page: &str, object: &str| {
        format!("page state `{page}` is shown while the object is `{object}`, which it does not assume")
    };
    let pairs = [("done", "cancelled"), ("done", "draft"), ("done", "rejected"), ("open", "cancelled")];
    assert_eq!(messages.len(), pairs.len(), "{messages:#?}");
    for ((page, object), message) in pairs.iter().zip(&messages) {
        assert!(message.starts_with(&shown(page, object)), "{messages:#?}");
    }
    assert!(
        messages.contains(&format!("{}; shortest run: object cancel", shown("open", "cancelled"))),
        "{messages:#?}"
    );
}

#[test]
fn a_sync_needs_the_accepted_outcome_of_an_action_whose_decision_emits_the_transition() {
    let mut lifecycle = submit_sync();
    lifecycle["syncs"] = json!([
        { "transitionId": "on-accepted", "lifecycleTransition": "reject" }
    ]);
    let found = lifecycle_findings(&lifecycle, |_| {});
    assert!(
        found.iter().any(
            |(code, pointer, message)| code == "widget.lifecycle.sync_unfired"
                && pointer.contains("/syncs/0/lifecycleTransition")
                && message.contains("does not name transition `reject`")
        ),
        "{found:#?}"
    );

    lifecycle["syncs"] = json!([{ "transitionId": "submit", "lifecycleTransition": "submit" }]);
    let found = lifecycle_findings(&lifecycle, |_| {});
    assert_eq!(
        codes(&found),
        ["widget.lifecycle.sync_not_accepted"],
        "{found:#?}"
    );

    lifecycle["syncs"] = json!([{ "transitionId": "on-accepted", "lifecycleTransition": "ship" }]);
    let found = lifecycle_findings(&lifecycle, |_| {});
    assert_eq!(
        codes(&found),
        ["widget.lifecycle.unknown_reference"],
        "{found:#?}"
    );
}

#[test]
fn a_page_names_a_lifecycle_its_object_type_declares() {
    let found = lifecycle_findings(
        &json!({ "objectType": "commerce.PurchaseOrder", "property": "shipping_status" }),
        |_| {},
    );
    assert_eq!(
        codes(&found),
        ["widget.lifecycle.unknown_lifecycle"],
        "{found:#?}"
    );
    let found = lifecycle_findings(
        &json!({ "objectType": "commerce.Invoice", "property": "status" }),
        |_| {},
    );
    assert_eq!(
        codes(&found),
        ["widget.lifecycle.unknown_lifecycle"],
        "{found:#?}"
    );
}
