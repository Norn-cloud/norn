#![allow(clippy::expect_used, missing_docs, reason = "integration-test crate")]

//! Wire form of bounded aggregate action inputs (G-QUESTION-FOLD, #1105).

use norn_ir::{ActionAggregateInput, AggregateFunction};
use serde_json::json;

#[test]
fn sum_and_count_round_trip_in_canonical_form() {
    let sum = json!({
        "name": "total", "objectType": "books.Entry",
        "filter": {"account": "account"},
        "window": {"property": "booked_on", "start": "from", "end": "to",
            "startInclusive": true, "endInclusive": false},
        "aggregate": {"kind": "sum", "property": "amount"},
        "maxItems": 200
    });
    let decoded: ActionAggregateInput = serde_json::from_value(sum.clone()).expect("sum");
    assert_eq!(decoded.input_fields(), ["account", "from", "to"]);
    assert_eq!(serde_json::to_value(&decoded).expect("encode"), sum);

    let count = json!({
        "name": "entries", "objectType": "books.Entry",
        "aggregate": {"kind": "count"}, "maxItems": 1
    });
    let decoded: ActionAggregateInput = serde_json::from_value(count.clone()).expect("count");
    assert_eq!(decoded.aggregate, AggregateFunction::Count {});
    assert!(decoded.filter.is_empty() && decoded.window.is_none());
    assert_eq!(serde_json::to_value(&decoded).expect("encode"), count);
}

#[test]
fn undeclared_edges_and_unknown_members_are_refused() {
    let base = json!({
        "name": "total", "objectType": "books.Entry",
        "window": {"property": "booked_on", "start": "from", "end": "to",
            "startInclusive": true, "endInclusive": true},
        "aggregate": {"kind": "count"}, "maxItems": 5
    });
    let mut missing_edge = base.clone();
    missing_edge["window"]
        .as_object_mut()
        .expect("window")
        .remove("endInclusive");
    let mut unknown = base.clone();
    unknown["limit"] = json!(5);
    let mut unknown_kind = base;
    unknown_kind["aggregate"] = json!({"kind": "avg", "property": "amount"});
    for value in [missing_edge, unknown, unknown_kind] {
        assert!(
            serde_json::from_value::<ActionAggregateInput>(value.clone()).is_err(),
            "{value}"
        );
    }
}
