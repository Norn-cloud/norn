//! #1762 milestone B: the outcome algebra, UI SDK layers (L1), every pending exit (L2),
//! notifications (L3), Retry or Close (L4), the authority split (L5), agent capabilities (L6),
//! marked and exact values, and behaviours from the generated UI SDK catalog. Each law has a
//! fixture it refuses and one it admits.

use super::{
    WIDGET_ID, action_artifact, approve_machine, customer_entity, drop_transition, edit, envelope,
    findings_with_support, machine_widget, order_entity, widget_findings,
};
use crate::{JsonAt, TestMust};
use norn_checker::Diagnostic;
use serde_json::{Value, json};

/// `(code, pointer, message)` of every finding on the widget whose code starts with a prefix.
fn coded(found: Vec<Diagnostic>, prefixes: &[&str]) -> Vec<(String, String, String)> {
    found
        .into_iter()
        .filter(|finding| finding.artifact_id.as_deref() == Some(WIDGET_ID))
        .filter(|finding| {
            prefixes
                .iter()
                .any(|p| finding.code.as_str().starts_with(p))
        })
        .map(|finding| {
            let pointer = finding.span.map_or_default(|span| span.pointer);
            (finding.code.as_str().to_owned(), pointer, finding.message)
        })
        .collect()
}

fn assert_refuses(found: &[(String, String, String)], code: &str, part: &str) {
    assert!(
        found.iter().any(|(c, _, m)| c == code && m.contains(part)),
        "expected {code} containing {part:?}; got {found:#?}"
    );
}

fn assert_admits(found: &[(String, String, String)], code: &str) {
    assert!(
        !found.iter().any(|(c, _, _)| c == code),
        "expected no {code}; got {found:#?}"
    );
}

const LAYER: &str = "widget.layer.dismissal_refused_without_cause";

/// The approve page with a `confirming` idle state the orders list opens, and a UI SDK layer.
fn layered(entry: &str, props: &Value, held_in: &Value, change: impl FnOnce(&mut Value)) -> Value {
    let mut widget = machine_widget(approve_machine());
    let ux = widget.at_mut("payload").at_mut("ux");
    let page = ux.at_mut("pages").at_mut(0);
    page.at_mut("root").at_mut("children").as_array_mut().req("children").push(json!({
        "nodeId": "confirm-layer",
        "component": { "kind": "catalog", "entry": { "entryId": entry }, "props": props, "heldIn": held_in },
        "children": [{
            "nodeId": "cancel-node",
            "component": { "kind": "control", "label": { "en": "Cancel", "ar": "إلغاء" }, "input": "trigger" },
            "children": []
        }]
    }));
    let machine = page.at_mut("machine");
    machine
        .at_mut("states")
        .as_array_mut()
        .req("states")
        .push(json!({ "stateId": "confirming", "role": "idle" }));
    machine.at_mut("events").as_array_mut().req("events").extend([
        json!({ "eventId": "confirm-open", "source": { "kind": "user", "nodeId": "orders-list-node" } }),
        json!({ "eventId": "confirm-cancel", "source": { "kind": "user", "nodeId": "cancel-node" } }),
        json!({ "eventId": "confirm-ask", "source": { "kind": "user", "nodeId": "confirm-layer", "emits": "onDismissAttempt" } }),
    ]);
    machine.at_mut("transitions").as_array_mut().req("transitions").extend([
        json!({ "transitionId": "confirm-open", "from": "browsing", "on": "confirm-open", "target": "confirming" }),
        json!({ "transitionId": "confirm-cancel", "from": "confirming", "on": "confirm-cancel", "target": "browsing" }),
    ]);
    change(machine);
    widget
}

fn without(machine: &mut Value, transition: &str) {
    drop_transition(machine, transition);
}

fn layer_codes(widget: Value) -> Vec<(String, String, String)> {
    coded(
        widget_findings(widget),
        &[
            "widget.layer.",
            "widget.catalog.",
            "widget.view.",
            "widget.validation.",
        ],
    )
}

#[test]
fn l1_total_non_dismissal_only_while_a_call_is_pending() {
    let refuse = json!({ "dismissible": "false" });
    // Refused: in an idle state with no Cancel and no question, nothing lets a person out.
    let found = layer_codes(layered(
        "ui-sdk:dialog",
        &refuse,
        &json!(["confirming"]),
        |m| {
            without(m, "confirm-cancel");
        },
    ));
    assert_refuses(&found, LAYER, "cannot be dismissed at all");
    // Admitted: held while the approve call is pending (it has a deadline and exits on every outcome).
    let found = layer_codes(layered(
        "ui-sdk:dialog",
        &refuse,
        &json!(["approving"]),
        |_| {},
    ));
    assert_admits(&found, LAYER);
    // Refused: dismissible:false with no heldIn refuses whenever a person opens it.
    let found = layer_codes(layered("ui-sdk:dialog", &refuse, &json!([]), |_| {}));
    assert_refuses(&found, LAYER, "whenever it is open");
}

#[test]
fn l1_an_accidental_dismissal_lock_needs_pending_a_question_or_cancel() {
    // Admitted: an alert dialog (no outside press, no swipe) in an idle confirmation with Cancel.
    let found = layer_codes(layered(
        "ui-sdk:alert-dialog",
        &json!({}),
        &json!(["confirming"]),
        |_| {},
    ));
    assert_admits(&found, LAYER);
    // Refused: the same confirmation without Cancel; Escape alone is not enough in idle.
    let found = layer_codes(layered(
        "ui-sdk:alert-dialog",
        &json!({}),
        &json!(["confirming"]),
        |m| {
            without(m, "confirm-cancel");
        },
    ));
    assert_refuses(&found, LAYER, "locks out an outside press or swipe");
    // Admitted: a dirty form refuses dismissal and asks through onDismissAttempt.
    let found = layer_codes(layered(
        "ui-sdk:dialog",
        &json!({ "dismissible": "false" }),
        &json!(["confirming"]),
        |m| {
            without(m, "confirm-cancel");
            m.at_mut("transitions").as_array_mut().req("transitions").push(json!({
            "transitionId": "confirm-ask", "from": "confirming", "on": "confirm-ask", "target": "browsing"
        }));
        },
    ));
    assert_admits(&found, LAYER);
    // Refused: a lock held in an error state.
    let found = layer_codes(layered(
        "ui-sdk:alert-dialog",
        &json!({}),
        &json!(["failed"]),
        |_| {},
    ));
    assert_refuses(&found, LAYER, "`failed`");
    // Admitted: a plain dialog refuses nothing and needs no heldIn.
    let found = layer_codes(layered("ui-sdk:dialog", &json!({}), &json!([]), |_| {}));
    assert_admits(&found, LAYER);
}

fn push_transition(machine: &mut Value, transition: Value) {
    machine
        .at_mut("transitions")
        .as_array_mut()
        .req("transitions")
        .push(transition);
}

#[test]
fn l1_asking_is_a_way_out_only_when_it_leaves_the_held_states() {
    let refuse = json!({ "dismissible": "false" });
    // Refused: `onDismissAttempt` back into the same state lets nobody out.
    let found = layer_codes(layered(
        "ui-sdk:dialog",
        &refuse,
        &json!(["confirming"]),
        |m| {
            without(m, "confirm-cancel");
            push_transition(
                m,
                json!({ "transitionId": "confirm-ask", "from": "confirming", "on": "confirm-ask", "target": "confirming" }),
            );
        },
    ));
    assert_refuses(&found, LAYER, "cannot be dismissed at all");
    // Refused: asking loops between two states the layer is held in.
    let looping = |exit: Option<Value>| {
        layer_codes(layered(
            "ui-sdk:dialog",
            &refuse,
            &json!(["confirming", "discarding"]),
            |m| {
                without(m, "confirm-cancel");
                m.at_mut("states")
                    .as_array_mut()
                    .req("states")
                    .push(json!({ "stateId": "discarding", "role": "idle" }));
                push_transition(
                    m,
                    json!({ "transitionId": "ask-1", "from": "confirming", "on": "confirm-ask", "target": "discarding" }),
                );
                push_transition(
                    m,
                    json!({ "transitionId": "ask-2", "from": "discarding", "on": "confirm-ask", "target": "confirming" }),
                );
                if let Some(exit) = exit {
                    push_transition(m, exit);
                }
            },
        ))
    };
    let found = looping(None);
    assert_refuses(&found, LAYER, "`confirming`");
    assert_refuses(&found, LAYER, "`discarding`");
    // Admitted: "Discard changes?" leads on, through a held state, to Cancel inside the layer.
    assert_admits(
        &looping(Some(
            json!({ "transitionId": "discard", "from": "discarding", "on": "confirm-cancel", "target": "browsing" }),
        )),
        LAYER,
    );
}

#[test]
fn l1_a_control_that_starts_the_call_is_not_cancel() {
    // The layer's only control is Confirm: it leaves the held state, but to send the command.
    let confirm_only = |transition: Value| {
        layer_codes(layered(
            "ui-sdk:alert-dialog",
            &json!({}),
            &json!(["confirming"]),
            |m| {
                without(m, "confirm-cancel");
                push_transition(m, transition);
            },
        ))
    };
    let start = json!([{ "kind": "start", "call": { "kind": "action", "bindingId": "approve" } }]);
    // Refused: Confirm starts the call and waits on it.
    assert_refuses(
        &confirm_only(
            json!({ "transitionId": "confirm", "from": "confirming", "on": "confirm-cancel", "target": "approving", "effects": start }),
        ),
        LAYER,
        "locks out an outside press or swipe",
    );
    // Refused: a step that starts a call is no way out, wherever it goes.
    assert_refuses(
        &confirm_only(
            json!({ "transitionId": "confirm", "from": "confirming", "on": "confirm-cancel", "target": "browsing", "effects": start }),
        ),
        LAYER,
        "`confirming`",
    );
    // Refused: nor is a step into a pending state.
    assert_refuses(
        &confirm_only(
            json!({ "transitionId": "confirm", "from": "confirming", "on": "confirm-cancel", "target": "approving" }),
        ),
        LAYER,
        "`confirming`",
    );
}

/// A layer held in `confirming` with no Cancel, whose own `onOpenChange` (raised as
/// `confirm-close`) leads back to `browsing`.
fn closed_by_open_change(entry: &str) -> Vec<(String, String, String)> {
    layer_codes(layered(entry, &json!({}), &json!(["confirming"]), |m| {
        without(m, "confirm-cancel");
        m.at_mut("events").as_array_mut().req("events").push(json!({
            "eventId": "confirm-close",
            "source": { "kind": "user", "nodeId": "confirm-layer", "emits": "onOpenChange" },
            "payload": [{ "fieldId": "value", "type": "boolean", "required": true }]
        }));
        push_transition(
            m,
            json!({ "transitionId": "confirm-close", "from": "confirming", "on": "confirm-close", "target": "browsing" }),
        );
    }))
}

#[test]
fn l1_a_held_layer_that_refuses_nothing_still_needs_a_way_out() {
    // Refused: a plain dialog held in an idle state with no Cancel. Held, it stays open whatever
    // Escape asks, and no transition hears it.
    let found = layer_codes(layered(
        "ui-sdk:dialog",
        &json!({}),
        &json!(["confirming"]),
        |m| without(m, "confirm-cancel"),
    ));
    assert_refuses(&found, LAYER, "is held open in `confirming`");
    // Admitted: the same dialog whose own `onOpenChange` (Escape, an outside press, Close) leaves.
    let found = closed_by_open_change("ui-sdk:dialog");
    assert_admits(&found, LAYER);
    assert_admits(&found, "widget.catalog.unknown_event");
    assert_admits(&found, "widget.catalog.event_payload_mismatch");
    // Refused: a plain dialog held in an error state whose only exit is outside the layer.
    let failed = |exit: Option<Value>| {
        layer_codes(layered(
            "ui-sdk:dialog",
            &json!({}),
            &json!(["failed"]),
            |m| {
                if let Some(exit) = exit {
                    push_transition(m, exit);
                }
            },
        ))
    };
    assert_refuses(&failed(None), LAYER, "is held open in `failed`");
    // Admitted: with Cancel inside it.
    assert_admits(
        &failed(Some(
            json!({ "transitionId": "failed-cancel", "from": "failed", "on": "confirm-cancel", "target": "browsing" }),
        )),
        LAYER,
    );
}

#[test]
fn l1_a_way_out_is_the_event_escape_raises() {
    // Refused: an alert dialog whose only exit is `onDismissAttempt`. It refuses only an outside
    // press or a swipe, never Escape, so a keyboard user never raises it.
    let found = layer_codes(layered(
        "ui-sdk:alert-dialog",
        &json!({}),
        &json!(["confirming"]),
        |m| {
            without(m, "confirm-cancel");
            push_transition(
                m,
                json!({ "transitionId": "confirm-ask", "from": "confirming", "on": "confirm-ask", "target": "browsing" }),
            );
        },
    ));
    assert_refuses(
        &found,
        LAYER,
        "locks out an outside press or swipe in `confirming`",
    );
    // Admitted: Escape closes an alert dialog and raises `onOpenChange`, which leaves.
    assert_admits(&closed_by_open_change("ui-sdk:alert-dialog"), LAYER);
}

#[test]
fn l1_held_in_is_the_one_source_of_whether_a_layer_is_open() {
    // Refused: a controlled `open` keeps a plain dialog open whatever Escape asks.
    let found = layer_codes(layered(
        "ui-sdk:dialog",
        &json!({ "open": "true" }),
        &json!([]),
        |_| {},
    ));
    assert_refuses(&found, LAYER, "sets `open`");
    // Refused: a lock held only while the call is pending, but opened by `open` in every state.
    let found = layer_codes(layered(
        "ui-sdk:dialog",
        &json!({ "dismissible": "false", "open": "true" }),
        &json!(["approving"]),
        |_| {},
    ));
    assert_refuses(&found, LAYER, "sets `open`");
    // Refused: an alert dialog with Cancel, whose `open` follows a field instead of its states.
    let found = layer_codes(layered(
        "ui-sdk:alert-dialog",
        &json!({ "open": "input.fields.lastCode == 'locked'" }),
        &json!(["confirming"]),
        |_| {},
    ));
    assert_refuses(&found, LAYER, "sets `open`");
    // Refused: on every primitive with dismissal attempts, not only dialogs.
    for entry in ["ui-sdk:select", "ui-sdk:popover"] {
        let found = layer_codes(layered(
            entry,
            &json!({ "open": "true" }),
            &json!([]),
            |_| {},
        ));
        assert_refuses(&found, LAYER, "sets `open`");
    }
    // Admitted: a collapsible has no dismissal, so `open` is its own state to set.
    let found = layer_codes(layered(
        "ui-sdk:collapsible",
        &json!({ "open": "true" }),
        &json!([]),
        |_| {},
    ));
    assert_admits(&found, LAYER);
    // Refused: `defaultOpen` opens a lock on load, before any state holds it.
    let found = layer_codes(layered(
        "ui-sdk:dialog",
        &json!({ "dismissible": "false", "defaultOpen": "true" }),
        &json!(["approving"]),
        |_| {},
    ));
    assert_refuses(&found, LAYER, "sets `defaultOpen`");
    // Refused: and on a layer that names `heldIn`, whatever it refuses.
    let found = layer_codes(layered(
        "ui-sdk:dialog",
        &json!({ "defaultOpen": "true" }),
        &json!(["confirming"]),
        |_| {},
    ));
    assert_refuses(&found, LAYER, "sets `defaultOpen`");
    // Admitted: a literal `defaultOpen: false` never opens it on load.
    let found = layer_codes(layered(
        "ui-sdk:dialog",
        &json!({ "dismissible": "false", "defaultOpen": "false" }),
        &json!(["approving"]),
        |_| {},
    ));
    assert_admits(&found, LAYER);
    // Admitted: a plain dialog that starts open, which Escape closes.
    let found = layer_codes(layered(
        "ui-sdk:dialog",
        &json!({ "defaultOpen": "true" }),
        &json!([]),
        |_| {},
    ));
    assert_admits(&found, LAYER);
}

#[test]
fn ui_sdk_nodes_are_checked_against_the_pinned_catalog() {
    let found = layer_codes(layered(
        "ui-sdk:no-such-primitive",
        &json!({}),
        &json!([]),
        |_| {},
    ));
    assert_refuses(
        &found,
        "widget.catalog.unknown_entry",
        "ui-sdk:no-such-primitive",
    );
    let found = layer_codes(layered(
        "ui-sdk:dialog",
        &json!({ "modal": "\"sideways\"" }),
        &json!([]),
        |_| {},
    ));
    assert_refuses(&found, "widget.catalog.invalid_prop", "modal");
    let found = layer_codes(layered(
        "ui-sdk:dialog",
        &json!({ "onOpenChange": "true" }),
        &json!([]),
        |_| {},
    ));
    assert_refuses(&found, "widget.catalog.unknown_prop", "onOpenChange");
    let found = layer_codes(layered(
        "ui-sdk:dialog",
        &json!({ "modal": "\"trap-focus\"" }),
        &json!([]),
        |_| {},
    ));
    assert_admits(&found, "widget.catalog.invalid_prop");
    // A tenant cannot declare its own entry in the UI SDK namespace.
    let mut widget = layered("ui-sdk:dialog", &json!({}), &json!([]), |_| {});
    widget.at_mut("payload").at_mut("ux").set(
        "catalog",
        json!([{ "entryId": "ui-sdk:dialog", "label": { "en": "Mine" }, "trust": "native" }]),
    );
    assert_refuses(
        &layer_codes(widget),
        "widget.catalog.duplicate_entry",
        "shadow",
    );
}

#[test]
fn behaviours_are_the_generated_foundation_behaviours_of_the_component() {
    let with_behaviors = |behaviors: Value| {
        let mut widget = layered("ui-sdk:dialog", &json!({}), &json!([]), |_| {});
        widget["payload"]["ux"]["pages"][0]["root"]["children"][3]["behaviors"] = behaviors;
        layer_codes(widget)
    };
    assert_admits(
        &with_behaviors(json!(["popups/modal", "popups/dismissal"])),
        "widget.view.behavior_unfit",
    );
    assert_refuses(
        &with_behaviors(json!(["selection/selection-model"])),
        "widget.view.behavior_unfit",
        "built from",
    );
    // The old hand-written names are gone.
    assert_refuses(
        &with_behaviors(json!(["focus_scope"])),
        "widget.view.behavior_unfit",
        "not a foundation behaviour",
    );
}

const NOTIFICATION: &str = "widget.layer.notification_intrusive";

fn toast(props: &Value, held_in: &Value) -> Vec<(String, String, String)> {
    let mut widget = machine_widget(approve_machine());
    widget
        .at_mut("payload")
        .at_mut("ux")
        .at_mut("pages")
        .at_mut(0)
        .at_mut("root")
        .at_mut("children")
        .as_array_mut()
        .req("children")
        .push(json!({
        "nodeId": "status-toast",
        "component": { "kind": "catalog", "entry": { "entryId": "ui-sdk:toast" }, "props": props, "heldIn": held_in },
        "children": []
    }));
    layer_codes(widget)
}

#[test]
fn l3_notifications_never_hold_the_page() {
    assert_admits(
        &toast(
            &json!({ "priority": "\"polite\"", "duration": "5000" }),
            &json!([]),
        ),
        NOTIFICATION,
    );
    assert_admits(&toast(&json!({}), &json!([])), NOTIFICATION);
    // How a toast is announced is UI SDK's call: assertive is its own default for an error.
    assert_admits(
        &toast(&json!({ "priority": "\"assertive\"" }), &json!([])),
        NOTIFICATION,
    );
    assert_admits(
        &toast(&json!({ "type": "\"error\"" }), &json!([])),
        NOTIFICATION,
    );
    assert_refuses(
        &toast(
            &json!({ "type": "\"loading\"", "dismissible": "false" }),
            &json!([]),
        ),
        NOTIFICATION,
        "neither expires nor can be dismissed",
    );
    assert_refuses(
        &toast(&json!({}), &json!(["browsing"])),
        NOTIFICATION,
        "held open",
    );
    // A native live region never traps focus.
    let mut widget = machine_widget(approve_machine());
    widget["payload"]["ux"]["pages"][0]["root"]["behaviors"] =
        json!(["platform/announce", "popups/modal"]);
    assert_refuses(&layer_codes(widget), NOTIFICATION, "never modal");
}

fn machine_codes(widget: Value) -> Vec<(String, String, String)> {
    coded(widget_findings(widget), &["widget.machine."])
}

#[test]
fn l2_every_pending_state_exits_on_not_served() {
    assert_admits(
        &machine_codes(machine_widget(approve_machine())),
        "widget.machine.pending_outcome_uncovered",
    );
    let found = machine_codes(edit(approve_machine(), |m| {
        drop_transition(m, "on-not-served");
    }));
    assert_refuses(
        &found,
        "widget.machine.pending_outcome_uncovered",
        "not_served",
    );
}

/// A pending read of `orders`, which the page also subscribes to.
fn subscribed_read(live_outcomes: bool) -> Value {
    edit(approve_machine(), |m| {
        let events = m.at_mut("events").as_array_mut().req("events");
        events.push(json!({ "eventId": "orders-changed", "source": { "kind": "subscription", "bindingId": "orders" } }));
        events.push(json!({ "eventId": "moved", "source": { "kind": "outcome", "outcome": "answer_moved" } }));
        events.push(
            json!({ "eventId": "stale", "source": { "kind": "outcome", "outcome": "stale" } }),
        );
        events.push(json!({ "eventId": "sub-refused", "source": { "kind": "outcome", "outcome": "subscription_refused" } }));
        m.at_mut("states").as_array_mut().req("states").push(json!({
            "stateId": "reading", "role": "pending", "deadlineMs": 15000, "awaits": { "kind": "read", "bindingId": "orders" }
        }));
        let transitions = m.at_mut("transitions").as_array_mut().req("transitions");
        transitions.push(json!({ "transitionId": "refresh", "from": "browsing", "on": "orders-changed", "target": "reading",
            "effects": [{ "kind": "start", "call": { "kind": "read", "bindingId": "orders" } }] }));
        let mut outcomes = vec![
            "accepted",
            "refused",
            "network",
            "not-served",
            "timeout",
            "moved",
        ];
        if live_outcomes {
            outcomes.extend(["stale", "sub-refused"]);
        }
        for on in outcomes {
            let target = if on == "accepted" || on == "moved" || on == "stale" {
                "browsing"
            } else {
                "failed"
            };
            transitions.push(json!({ "transitionId": format!("read-{on}"), "from": "reading", "on": on, "target": target }));
        }
    })
}

#[test]
fn l2_a_subscribed_read_handles_a_lapsed_feed_and_a_refused_subscription() {
    let found = machine_codes(subscribed_read(false));
    assert_refuses(&found, "widget.machine.pending_outcome_uncovered", "stale");
    assert_refuses(
        &found,
        "widget.machine.pending_outcome_uncovered",
        "subscription_refused",
    );
    let found = machine_codes(subscribed_read(true));
    assert_admits(&found, "widget.machine.pending_outcome_uncovered");
    assert_admits(&found, "widget.machine.outcome_impossible");
    // An action never sees a feed lapse.
    let found = machine_codes(edit(approve_machine(), |m| {
        m["events"][5]["source"]["outcome"] = json!("stale");
    }));
    assert_refuses(&found, "widget.machine.outcome_impossible", "stale");
}

fn coded_handler(code: &str, declared: &[&str]) -> Vec<(String, String, String)> {
    let widget = edit(approve_machine(), |m| {
        m.at_mut("events").as_array_mut().req("events").push(json!({
            "eventId": "locked", "source": { "kind": "outcome", "outcome": "refused", "category": "conflict", "code": code }
        }));
        m.at_mut("transitions").as_array_mut().req("transitions").push(json!({
            "transitionId": "on-locked", "from": "approving", "on": "locked", "target": "refused"
        }));
    });
    let mut action = action_artifact();
    if !declared.is_empty() {
        action.at_mut("payload").set(
            "uiEffects",
            json!({ "effectsVersion": "0.1", "errorCodes": declared }),
        );
    }
    coded(
        findings_with_support(widget, vec![order_entity(), customer_entity(), action], &[]),
        &["widget.machine."],
    )
}

#[test]
fn l2_a_code_handler_names_a_code_the_action_declares_and_never_covers_its_category() {
    assert_admits(
        &coded_handler("order.locked", &["order.locked"]),
        "widget.machine.outcome_code_undeclared",
    );
    assert_refuses(
        &coded_handler("order.unknown", &["order.locked"]),
        "widget.machine.outcome_code_undeclared",
        "order.unknown",
    );
    assert_refuses(
        &coded_handler("order.locked", &[]),
        "widget.machine.outcome_code_undeclared",
        "order.locked",
    );
    // Coverage stays at the category: a code handler alone leaves `refused:conflict` uncovered.
    let widget = edit(approve_machine(), |m| {
        m["events"][4]["source"]["category"] = json!("permission");
        m.at_mut("events").as_array_mut().req("events").push(json!({
            "eventId": "locked", "source": { "kind": "outcome", "outcome": "refused", "category": "conflict", "code": "order.locked" }
        }));
        m.at_mut("transitions").as_array_mut().req("transitions").push(json!({
            "transitionId": "on-locked", "from": "approving", "on": "locked", "target": "refused"
        }));
    });
    assert_refuses(
        &machine_codes(widget),
        "widget.machine.pending_outcome_uncovered",
        "refused:conflict",
    );
    // Only a refused outcome names a code.
    let found = machine_codes(edit(approve_machine(), |m| {
        m["events"][3]["source"]["code"] = json!("order.locked");
    }));
    assert_refuses(&found, "widget.machine.invalid_event", "code");
}

#[test]
fn l4_a_failure_offers_retry_or_close_a_person_drives() {
    assert_admits(
        &machine_codes(machine_widget(approve_machine())),
        "widget.machine.error_without_recovery",
    );
    // A timer alone is no recovery.
    let found =
        machine_codes(edit(approve_machine(), |m| {
            m.at_mut("events")
                .as_array_mut()
                .req("events")
                .push(json!({ "eventId": "tick", "source": { "kind": "timer", "afterMs": 5000 } }));
            drop_transition(m, "retry-failed");
            drop_transition(m, "dismiss-failed");
            m.at_mut("transitions").as_array_mut().req("transitions").push(json!({
            "transitionId": "failed-tick", "from": "failed", "on": "tick", "target": "browsing"
        }));
        }));
    assert_refuses(
        &found,
        "widget.machine.error_without_recovery",
        "Retry or Close",
    );
    // An idle state a failure enters needs a person's way out too.
    let gave_up = |exit: Value| {
        machine_codes(edit(approve_machine(), |m| {
            m.at_mut("events")
                .as_array_mut()
                .req("events")
                .push(json!({ "eventId": "tick", "source": { "kind": "timer", "afterMs": 5000 } }));
            m.at_mut("states")
                .as_array_mut()
                .req("states")
                .push(json!({ "stateId": "gave-up", "role": "idle" }));
            m["transitions"][5]["target"] = json!("gave-up");
            m.at_mut("transitions")
                .as_array_mut()
                .req("transitions")
                .push(exit);
        }))
    };
    assert_refuses(
        &gave_up(
            json!({ "transitionId": "gave-up-tick", "from": "gave-up", "on": "tick", "target": "browsing" }),
        ),
        "widget.machine.failure_without_retry_or_close",
        "gave-up",
    );
    assert_admits(
        &gave_up(
            json!({ "transitionId": "gave-up-close", "from": "gave-up", "on": "dismiss", "target": "browsing" }),
        ),
        "widget.machine.failure_without_retry_or_close",
    );
    // A terminal state is the page's declared end: the host closes it.
    let found = machine_codes(edit(approve_machine(), |m| {
        m.at_mut("states")
            .as_array_mut()
            .req("states")
            .push(json!({ "stateId": "gone", "role": "terminal" }));
        m["transitions"][5]["target"] = json!("gone");
    }));
    assert_admits(&found, "widget.machine.failure_without_retry_or_close");
}

const AUTHORITY: &str = "widget.authority.action_outside_binding";

#[test]
fn l5_an_action_is_reached_only_through_its_binding() {
    let operation = |operation_id: &str| {
        coded(
            widget_findings(edit(approve_machine(), |m| {
                m["states"][1]["awaits"] =
                    json!({ "kind": "operation", "operationId": operation_id });
            })),
            &["widget.authority."],
        )
    };
    assert_refuses(&operation("callAction"), AUTHORITY, "callAction");
    assert_refuses(&operation("post_message"), AUTHORITY, "post_message");
    assert_admits(&operation("decideDecision"), AUTHORITY);
    assert_admits(
        &coded(
            widget_findings(machine_widget(approve_machine())),
            &["widget.authority."],
        ),
        AUTHORITY,
    );
}

#[test]
fn l6_a_renderer_without_agent_redaction_and_confirm_must_refuse() {
    let widget = envelope(layered("ui-sdk:dialog", &json!({}), &json!([]), |_| {}));
    let norn_ir::ArtifactPayload::Widget(payload) = &widget.payload else {
        panic!("widget payload");
    };
    let ux: norn_ir::WidgetUx =
        serde_json::from_value(serde_json::to_value(payload.ux.as_value().req("ux")).req("ux"))
            .req("typed ux");
    let required = norn_ui_checker::ui_sdk::renderer_capabilities(&ux);
    assert!(required.contains("ui-sdk:dialog"), "{required:?}");
    let without_agent: std::collections::BTreeSet<String> = required
        .iter()
        .filter(|capability| !capability.starts_with("agent:"))
        .cloned()
        .collect();
    let missing = norn_ui_checker::ui_sdk::missing_renderer_capabilities(&ux, &without_agent);
    assert_eq!(
        missing.into_iter().collect::<Vec<_>>(),
        ["agent:confirm", "agent:redaction"]
    );
    assert!(norn_ui_checker::ui_sdk::missing_renderer_capabilities(&ux, &required).is_empty());
    // The catalog's agent descriptor says which page-level steps an agent can take on the layer.
    let steps = norn_ui_checker::ui_sdk::agent_steps(&ux);
    assert!(
        steps["confirm-layer"].iter().any(|step| step == "setOpen"),
        "{steps:?}"
    );
}

/// The order entity with an exact decimal `total` and a marked `email`, and a widget listing both
/// through the contract's own renderers for numbers and text.
fn handled(number: Value, text: Value) -> Vec<(String, String, String)> {
    let mut order = order_entity();
    let payload = order.at_mut("payload");
    let properties = payload
        .at_mut("objectType")
        .at_mut("properties")
        .as_array_mut()
        .req("properties");
    properties.push(json!({ "id": "total", "type": { "kind": "decimal", "scale": 2 } }));
    properties
        .push(json!({ "id": "email", "type": { "kind": "text" }, "markings": ["pii.contact"] }));
    let mut widget = machine_widget(approve_machine());
    let ux = widget.at_mut("payload").at_mut("ux");
    ux.at_mut("pages")
        .at_mut(0)
        .at_mut("root")
        .at_mut("children")
        .at_mut(0)
        .at_mut("component")
        .at_mut("fields")
        .as_array_mut()
        .req("fields")
        .extend([
            json!({ "bindingId": "orders", "propertyId": "total" }),
            json!({ "bindingId": "orders", "propertyId": "email" }),
        ]);
    let mut catalog = Vec::new();
    for (entry, value_type, handles) in [("money", "number", number), ("plain", "text", text)] {
        if !handles.is_null() {
            catalog.push(json!({ "entryId": entry, "label": { "en": entry }, "trust": "native", "renders": value_type, "handles": handles }));
        }
    }
    ux.set("catalog", Value::Array(catalog));
    coded(
        findings_with_support(
            widget,
            vec![order, customer_entity(), action_artifact()],
            &[],
        ),
        &["widget.binding."],
    )
}

#[test]
fn a_marked_or_exact_value_needs_a_renderer_that_handles_it() {
    let found = handled(json!([]), json!([]));
    assert_refuses(&found, "widget.binding.exact_value_unhandled", "total");
    assert_refuses(&found, "widget.binding.marked_value_unhandled", "email");
    let found = handled(json!(["exact"]), json!(["masked"]));
    assert_eq!(found, [], "{found:#?}");
    // The native renderer draws both faithfully by its own contract.
    let found = handled(Value::Null, Value::Null);
    assert_eq!(found, [], "{found:#?}");
}

#[test]
fn a_held_confirmation_layer_is_admitted_with_no_finding_at_all() {
    let widget = layered(
        "ui-sdk:alert-dialog",
        &json!({ "pendingAnnouncement": "\"Approving\"" }),
        &json!(["confirming", "approving"]),
        |_| {},
    );
    let found = coded(widget_findings(widget), &["widget."]);
    assert_eq!(found, [], "{found:#?}");
}
