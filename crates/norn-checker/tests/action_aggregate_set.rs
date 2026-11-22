#![allow(
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::needless_pass_by_value,
    missing_docs,
    reason = "integration-test crate"
)]

//! Admission of bounded aggregate action inputs (G-QUESTION-FOLD, #1105).

use std::collections::BTreeSet;

use norn_checker::{Diagnostic, EntityArtifactSetContext, check_artifact_set};
use norn_ir::ArtifactEnvelope;
use serde_json::{Value, json};

const AGGREGATE: &str = "artifact_set.action.invalid_aggregate";
const PAYLOAD: &str = "action.validation.invalid_payload";

fn artifact(id: &str, kind: &str, payload: Value) -> Value {
    json!({
        "irVersion": "0.1", "artifactId": id, "kind": kind,
        "namespace": "books", "name": id, "version": "0.1.0", "lifecycle": "draft",
        "scope": {"orgId": "org_demo", "environmentId": "dev"}, "dependencies": [],
        "provenance": {"source": "authoring_sdk"}, "payload": payload
    })
}

fn documents() -> Vec<Value> {
    vec![
        artifact(
            "books.Entry",
            "entity",
            json!({
                "entityVersion": "2.0", "objectType": {
                    "name": "books.Entry", "properties": [
                        {"id": "account", "type": {"kind": "text"}},
                        {"id": "state", "type": {"kind": "enum", "values": [
                            {"value": "draft"}, {"value": "posted"}
                        ]}},
                        {"id": "booked_on", "type": {"kind": "date"}},
                        {"id": "posted_at", "type": {"kind": "timestamp", "timezone": "utc"}},
                        {"id": "amount", "type": {"kind": "decimal", "scale": 2,
                            "additivity": {"kind": "additive"}}},
                        {"id": "balance", "type": {"kind": "decimal", "scale": 2,
                            "additivity": {"kind": "semi_additive"}}},
                        {"id": "rate", "type": {"kind": "decimal", "scale": 4}},
                        {"id": "credit", "constraints": {"nullable": true}, "type": {"kind": "decimal",
                            "scale": 2, "additivity": {"kind": "additive"}}},
                        {"id": "account_copy", "type": {"kind": "text"},
                            "computed": "self.account"},
                        {"id": "lines", "type": {"kind": "integer",
                            "additivity": {"kind": "additive"}}}
                    ]
                }
            }),
        ),
        artifact(
            "books.post",
            "action",
            json!({
                "actionId": "books.post", "scopeRequired": ["orgId", "environmentId"],
                "idempotent": true, "ledgered": true,
                "outputSchema": {"type": "object", "properties": {}, "additionalProperties": false},
                "inputSchema": {"type": "object", "properties": {
                    "account": {"type": "string"},
                    "state": {"type": "string"},
                    "from": {"type": "string"},
                    "to": {"type": "string"},
                    "limit": {"type": "integer"},
                    "note": {"type": ["string", "null"]}
                }, "required": ["account", "state", "from", "to", "limit"],
                "additionalProperties": false},
                "aggregates": [
                    {"name": "total", "objectType": "books.Entry",
                        "filter": {"account": "account", "state": "state"},
                        "window": {"property": "booked_on", "start": "from", "end": "to",
                            "startInclusive": true, "endInclusive": false},
                        "aggregate": {"kind": "sum", "property": "amount"},
                        "maxItems": 200},
                    {"name": "entries", "objectType": "books.Entry",
                        "filter": {"account": "account"},
                        "aggregate": {"kind": "count"},
                        "maxItems": 200}
                ],
                "preconditions": ["aggregates.entries < input.limit"]
            }),
        ),
    ]
}

fn findings(documents: Vec<Value>) -> Vec<Diagnostic> {
    let artifacts: Vec<ArtifactEnvelope> = documents
        .into_iter()
        .map(|value| serde_json::from_value(value).expect("artifact"))
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

fn with_action(edit: impl FnOnce(&mut Value)) -> Vec<Diagnostic> {
    let mut documents = documents();
    edit(&mut documents[1]["payload"]);
    findings(documents)
}

fn has(found: &[Diagnostic], code: &str, needle: &str) -> bool {
    found
        .iter()
        .any(|finding| finding.code == code && finding.message.contains(needle))
}

#[test]
fn filtered_windowed_sums_and_counts_are_admitted_and_typed_for_preconditions() {
    assert_eq!(findings(documents()), []);
    let found = with_action(|action| {
        action["preconditions"] = json!(["aggregates.total"]);
    });
    assert!(
        found.iter().any(|finding| finding.code == PAYLOAD),
        "a non-boolean precondition is refused: {found:?}"
    );
    let found = with_action(|action| {
        action["preconditions"] = json!(["aggregates.missing < 1"]);
    });
    assert_ne!(found, [], "an unknown aggregate member is refused");
}

#[test]
fn integer_sums_timestamp_windows_and_unwindowed_semi_additive_sums_are_admitted() {
    let found = with_action(|action| {
        action["aggregates"][0]["aggregate"] = json!({"kind": "sum", "property": "lines"});
        action["aggregates"][0]["window"]["property"] = json!("posted_at");
        action["aggregates"][1]["aggregate"] = json!({"kind": "sum", "property": "balance"});
    });
    assert_eq!(found, []);
}

#[test]
fn unknown_types_and_properties_are_refused_against_the_model() {
    for (edit, needle) in [
        (json!({"objectType": "books.Missing"}), "not an object type"),
        (
            json!({"filter": {"missing": "account"}}),
            "not a stored property",
        ),
        (
            json!({"aggregate": {"kind": "sum", "property": "missing"}}),
            "not a stored property",
        ),
    ] {
        let found = with_action(|action| {
            for (key, value) in edit.as_object().expect("edit") {
                action["aggregates"][0][key] = value.clone();
            }
        });
        assert!(has(&found, AGGREGATE, needle), "{edit}: {found:?}");
    }
}

#[test]
fn property_kinds_the_fold_cannot_read_are_refused() {
    for (edit, needle) in [
        // A window over a text property.
        (
            json!({"window": {"property": "account", "start": "from", "end": "to",
                "startInclusive": true, "endInclusive": true}}),
            "cannot compare",
        ),
        // A filter over a computed property: predicates select by stored,
        // indexable properties only (shared with #1104).
        (
            json!({"filter": {"account_copy": "account"}}),
            "is computed",
        ),
        // A filter whose input has another value class.
        (
            json!({"filter": {"account": "limit"}}),
            "cannot be compared",
        ),
        // A sum without a declared additivity contract.
        (
            json!({"aggregate": {"kind": "sum", "property": "rate"}}),
            "additive",
        ),
        // A nullable sum.
        (
            json!({"aggregate": {"kind": "sum", "property": "credit"}}),
            "non-nullable",
        ),
        // A semi-additive sum across a time window.
        (
            json!({"aggregate": {"kind": "sum", "property": "balance"}}),
            "semi-additive",
        ),
        // A sum over a non-numeric property.
        (
            json!({"aggregate": {"kind": "sum", "property": "account"}}),
            "non-nullable decimal or integer",
        ),
    ] {
        let found = with_action(|action| {
            for (key, value) in edit.as_object().expect("edit") {
                action["aggregates"][0][key] = value.clone();
            }
        });
        assert!(has(&found, AGGREGATE, needle), "{edit}: {found:?}");
    }
}

#[test]
fn names_bounds_and_input_fields_are_checked_per_artifact() {
    let found = with_action(|action| action["aggregates"][1]["name"] = json!("total"));
    assert!(has(&found, PAYLOAD, "repeated"), "{found:?}");

    let found = with_action(|action| {
        action["slice"] = json!([{"name": "entries", "objectType": "books.Entry",
            "key": {"property": {"from": "input", "property": "account"}}}]);
    });
    assert!(has(&found, PAYLOAD, "also a slice entry"), "{found:?}");

    let found = with_action(|action| action["aggregates"][0]["maxItems"] = json!(0));
    assert!(has(&found, PAYLOAD, "at least 1"), "{found:?}");

    let found = with_action(|action| action["aggregates"][0]["maxItems"] = json!(900));
    assert!(has(&found, PAYLOAD, "in total"), "{found:?}");

    let found = with_action(|action| {
        action["aggregates"][0]["filter"] = json!({"account": "unknown"});
    });
    assert!(
        has(&found, PAYLOAD, "not declared in inputSchema"),
        "{found:?}"
    );

    let found = with_action(|action| {
        action["aggregates"][0]["filter"] = json!({"account": "note"});
    });
    assert!(has(&found, PAYLOAD, "may be absent"), "{found:?}");
}

#[test]
fn window_edges_must_declare_their_inclusivity() {
    for missing in ["startInclusive", "endInclusive"] {
        let found = with_action(|action| {
            action["aggregates"][0]["window"]
                .as_object_mut()
                .expect("window")
                .remove(missing);
        });
        assert!(
            found.iter().any(|finding| finding.code == PAYLOAD),
            "{missing}: {found:?}"
        );
    }
}

#[test]
fn aggregate_only_actions_report_no_slice_findings_when_types_cannot_resolve() {
    let mut documents = documents();
    documents[0]["payload"]["objectType"]["properties"][0]["type"] =
        json!({"kind": "ref", "name": "books.missing_type"});
    let found = findings(documents);
    assert!(
        !found
            .iter()
            .any(|finding| finding.code == "artifact_set.action.invalid_slice"),
        "{found:?}"
    );
}

#[test]
fn an_input_field_named_aggregates_stays_reachable_beside_the_operand() {
    let with_aggregates = with_action(|action| {
        action["inputSchema"]["properties"]["aggregates"] = json!({"type": "string"});
        action["inputSchema"]["required"]
            .as_array_mut()
            .expect("required")
            .push(json!("aggregates"));
        action["preconditions"] = json!([
            "aggregates.entries < input.limit",
            "input.aggregates == \"x\""
        ]);
    });
    assert_eq!(with_aggregates, []);

    let without_aggregates = with_action(|action| {
        action["inputSchema"]["properties"]["aggregates"] = json!({"type": "string"});
        action["inputSchema"]["required"]
            .as_array_mut()
            .expect("required")
            .push(json!("aggregates"));
        action.as_object_mut().expect("action").remove("aggregates");
        action["preconditions"] = json!(["aggregates == \"x\""]);
    });
    assert_eq!(without_aggregates, []);
}
