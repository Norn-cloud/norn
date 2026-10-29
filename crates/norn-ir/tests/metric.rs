#![allow(
    clippy::expect_used,
    clippy::indexing_slicing,
    missing_docs,
    reason = "integration-test assertions"
)]

use norn_ir::{MetricDefinition, MetricIR, QuestionIR};
use serde_json::{Value, json};

const SALES: &str = include_str!("../../../fixtures/ir/metrics/net-sales.definition.json");
const DOCUMENT: &str = include_str!("../../../fixtures/ir/metrics/net-sales.metric.json");
const QUESTION: &str =
    include_str!("../../../fixtures/ir/metrics/sales-by-store-day.question.json");

#[test]
fn golden_definitions_round_trip_with_closed_versioned_roots() {
    let metric: MetricIR = serde_json::from_str(DOCUMENT).expect("OSI metric file");
    let question: QuestionIR = serde_json::from_str(QUESTION).expect("object-set question");
    for (source, rendered) in [
        (DOCUMENT, serde_json::to_string(&metric).expect("encode")),
        (QUESTION, serde_json::to_string(&question).expect("encode")),
    ] {
        assert_eq!(
            serde_json::from_str::<Value>(source).expect("json"),
            serde_json::from_str::<Value>(&rendered).expect("json")
        );
    }
    for (schema, members) in [
        (
            norn_ir::metric_ir_schema(),
            &["version", "semantic_model"][..],
        ),
        (
            norn_ir::question_ir_schema(),
            &[
                "irVersion",
                "name",
                "objectSet",
                "aggregate",
                "window",
                "freshness",
            ][..],
        ),
    ] {
        let schema = serde_json::to_value(schema).expect("schema");
        assert_eq!(schema["additionalProperties"], false);
        for member in members {
            assert!(
                schema["required"]
                    .as_array()
                    .expect("required")
                    .contains(&json!(member))
            );
        }
    }
}

#[test]
fn unknown_members_null_optionals_and_unknown_versions_fail_decoding() {
    let metric: Value = serde_json::from_str(SALES).expect("metric");
    for pointer in [
        "/x-norn/currency",
        "/x-norn/safeAggregation",
        "/type_params/expr",
    ] {
        let mut value = metric.clone();
        if pointer.ends_with("safeAggregation") {
            value["x-norn"]["safeAggregation"] = Value::Null;
        } else {
            *value.pointer_mut(pointer).expect("pointer") = Value::Null;
        }
        assert!(
            serde_json::from_str::<MetricDefinition>(&value.to_string()).is_err(),
            "{pointer}"
        );
    }
    let mut value = metric;
    value["x-norn"]["irVersion"] = json!("9.0");
    assert!(serde_json::from_str::<MetricDefinition>(&value.to_string()).is_err());
    let mut value: Value = serde_json::from_str(DOCUMENT).expect("doc");
    value["version"] = json!("0.1.0");
    assert!(serde_json::from_str::<MetricIR>(&value.to_string()).is_err());
    let question: Value = serde_json::from_str(QUESTION).expect("question");
    for field in ["compare", "filter", "groupBy", "searchAround"] {
        let mut value = question.clone();
        value[field] = Value::Null;
        assert!(serde_json::from_str::<QuestionIR>(&value.to_string()).is_err());
    }
    let mut value = question;
    value["freshness"]["engine"] = json!("duckdb");
    assert!(serde_json::from_str::<QuestionIR>(&value.to_string()).is_err());
}

#[test]
fn filter_literals_preserve_exact_decimal_tokens() {
    let source = SALES.replace("\"Submitted\"", "9007199254740993.125");
    let metric: MetricDefinition = serde_json::from_str(&source).expect("exact literal");
    assert_eq!(
        metric.norn.default_filters[0]
            .value
            .canonical_json()
            .expect("canonical"),
        b"9007199254740993.125"
    );
    let rendered = serde_json::to_string(&metric).expect("encode");
    assert!(rendered.contains("9007199254740993.125"));
    assert_eq!(
        serde_json::from_str::<MetricDefinition>(&rendered).expect("round trip"),
        metric
    );
}

#[test]
fn metricflow_types_are_serializable_and_norn_policy_is_separate() {
    let mut value: Value = serde_json::from_str(SALES).expect("metric");
    for kind in ["simple", "ratio", "cumulative", "derived", "conversion"] {
        value["type"] = json!(kind);
        let metric: MetricDefinition = serde_json::from_str(&value.to_string()).expect("type");
        assert_eq!(serde_json::to_value(metric).expect("encode"), value);
    }
    value["type_params"]["callback"] = json!("execute()");
    assert!(serde_json::from_str::<MetricDefinition>(&value.to_string()).is_err());
}

#[test]
fn calendar_and_comparison_mapping_are_explicit_closed_contracts() {
    let mut value: Value = serde_json::from_str(QUESTION).expect("question");
    value["compare"] = json!({"kind":"plan","question":"erpnext.plan","mapping":[{"metric":"erpnext.net_sales","referenceMetric":"erpnext.budget_sales"}]});
    let question: QuestionIR = serde_json::from_str(&value.to_string()).expect("mapping");
    assert_eq!(serde_json::to_value(&question).expect("encode"), value);
    value.as_object_mut().expect("question").remove("calendar");
    let default: QuestionIR = serde_json::from_str(&value.to_string()).expect("default");
    assert_eq!(default.calendar, norn_ir::metric::CalendarPolicy::IsoUtc);
    value["calendar"] = json!("ambient_timezone");
    assert!(serde_json::from_str::<QuestionIR>(&value.to_string()).is_err());
    value["calendar"] = json!("iso_utc");
    value["compare"]["mapping"][0]["uncheckedJoin"] = json!(true);
    assert!(serde_json::from_str::<QuestionIR>(&value.to_string()).is_err());
}

#[test]
fn monetary_decimal_currency_source_survives_the_typed_contract() {
    let source = r#"{"kind":"decimal","scale":2,"currencySource":{"links":["invoice"],"property":"currency"}}"#;
    let property: norn_ir::PropertyType = serde_json::from_str(source).expect("money");
    assert_eq!(
        serde_json::to_value(&property).expect("encode"),
        serde_json::from_str::<Value>(source).expect("json")
    );
    let mut value: Value = serde_json::from_str(source).expect("json");
    value["currencySource"] = Value::Null;
    assert!(serde_json::from_str::<norn_ir::PropertyType>(&value.to_string()).is_err());
}
