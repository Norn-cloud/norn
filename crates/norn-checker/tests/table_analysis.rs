#![allow(
    clippy::expect_used,
    clippy::indexing_slicing,
    missing_docs,
    reason = "integration tests"
)]

use norn_checker::table_analysis::{Unknown, analyze_table};
use norn_expr::schema::Schema;
use norn_ir::jdm::JdmDecisionTableContent;
use serde_json::json;

fn schema(value: serde_json::Value) -> Schema {
    Schema::parse(&serde_json::from_value(value).expect("IR schema"), "input").expect("schema")
}
fn boolean() -> Schema {
    schema(
        json!({"type":"object","properties":{"approved":{"type":"boolean"}},
        "required":["approved"],"additionalProperties":false}),
    )
}
fn table(policy: &str, conditions: &[&str]) -> JdmDecisionTableContent {
    serde_json::from_value(json!({"hitPolicy":policy,
        "inputs":[{"id":"condition","field":"approved"}],
        "outputs":[{"id":"result","field":"result"}],
        "rules":conditions.iter().enumerate().map(|(index, condition)| json!({
            "_id":format!("row_{index}"),"condition":condition,"result":"true"
        })).collect::<Vec<_>>()
    }))
    .expect("table")
}

#[test]
fn finite_gap_has_exact_input_golden() {
    let result = analyze_table(&table("first", &["true"]), &boolean()).expect("exhaustive");
    let actual = json!({"inputsChecked":result.inputs_checked,
        "uncovered":result.uncovered,"uncoveredCount":result.uncovered_count,
        "unreachableRows":result.unreachable_rows,"overlaps":result.overlaps.len()});
    let expected: serde_json::Value = serde_json::from_str(include_str!(
        "../../../fixtures/rule-ir-v2/analysis/finite-gap.expected.json"
    ))
    .expect("golden");
    assert_eq!(actual, expected);
}

#[test]
fn first_shadowing_and_collect_overlap_have_distinct_reachability() {
    let first =
        analyze_table(&table("first", &["false", "false", "true"]), &boolean()).expect("first");
    assert!(first.uncovered.is_none());
    assert_eq!(first.unreachable_rows, ["row_1"]);
    assert_eq!(first.overlaps[0].input, json!({"approved":false}));
    assert_eq!(first.overlaps[0].matching_rows, ["row_0", "row_1"]);
    assert_eq!(first.overlaps[0].selected_rows, ["row_0"]);
    let collect =
        analyze_table(&table("collect", &["false", "false", "true"]), &boolean()).expect("collect");
    assert!(collect.unreachable_rows.is_empty());
    assert_eq!(collect.overlaps[0].selected_rows, ["row_0", "row_1"]);
}

#[test]
fn impossible_row_is_unreachable_under_collect() {
    let result =
        analyze_table(&table("collect", &["true", "false", "null"]), &boolean()).expect("analysis");
    assert_eq!(result.unreachable_rows, ["row_2"]);
}

#[test]
fn standard_expression_cells_use_context_without_a_subject() {
    let mut value = table("first", &["approved == true", "approved == false"]);
    value.inputs[0].field = None;
    let result = analyze_table(&value, &boolean()).expect("analysis");
    assert!(result.uncovered.is_none());
    assert!(result.overlaps.is_empty());
}

#[test]
fn input_field_is_applied_before_column_selectors() {
    let mut value = table("first", &["true", "false"]);
    value.input_field = Some("wrapped".to_owned());
    let schema = schema(json!({"type":"object","properties":{"wrapped":{
        "type":"object","properties":{"approved":{"type":"boolean"}},
        "required":["approved"],"additionalProperties":false}},
        "required":["wrapped"],"additionalProperties":false}));
    assert!(
        analyze_table(&value, &schema)
            .expect("analysis")
            .uncovered
            .is_none()
    );
}

#[test]
fn optional_nullable_and_enum_inputs_are_exhaustive() {
    let schema = schema(json!({"type":"object","properties":{
        "flag":{"type":["boolean","null"]},
        "kind":{"type":"string","enum":["a","b"]}},
        "required":["kind"],"additionalProperties":false}));
    let result = analyze_table(&table("collect", &[""]), &schema).expect("analysis");
    assert_eq!(result.inputs_checked, 8);
    assert!(result.uncovered.is_none());
}

#[test]
fn empty_table_is_not_vacuously_complete() {
    let result = analyze_table(&table("first", &[]), &boolean()).expect("analysis");
    assert_eq!(result.uncovered_count, 2);
}

#[test]
fn unbounded_domains_and_loop_modes_never_claim_a_verdict() {
    let unbounded = schema(json!({"type":"number"}));
    assert_eq!(
        analyze_table(&table("first", &[""]), &unbounded),
        Err(Unknown::UnboundedDomain)
    );
    let mut value = table("first", &[""]);
    value.execution_mode = Some(norn_ir::jdm::JdmExecutionMode::Loop);
    assert_eq!(
        analyze_table(&value, &boolean()),
        Err(Unknown::LoopExecution)
    );
}

#[test]
fn domain_and_work_budgets_fail_closed() {
    let properties: serde_json::Map<String, serde_json::Value> = (0..9)
        .map(|i| (format!("b{i}"), json!({"type":"boolean"})))
        .collect();
    let required: Vec<_> = properties.keys().cloned().collect();
    let schema = schema(json!({"type":"object","properties":properties,
        "required":required,"additionalProperties":false}));
    assert_eq!(
        analyze_table(&table("first", &[""]), &schema),
        Err(Unknown::Limit)
    );
    let mut value = table("collect", &[""]);
    value.rules = (0..2_000)
        .map(|i| {
            let mut row = value.rules[0].clone();
            row.id = format!("r{i}");
            row
        })
        .collect();
    assert_eq!(analyze_table(&value, &boolean()), Err(Unknown::Limit));
}

#[test]
fn evaluation_errors_are_unknown_not_uncovered_inputs() {
    let mut value = table("first", &["true"]);
    value.inputs[0].field = Some("(".to_owned());
    assert!(matches!(
        analyze_table(&value, &boolean()),
        Err(Unknown::Expression(_))
    ));
}

#[test]
fn whitespace_normalization_matches_admission() {
    let mut value = table("first", &["  approved == true  ", "  approved == false  "]);
    value.inputs[0].field = Some("  ".to_owned());
    value.input_field = Some("  ".to_owned());
    assert!(
        analyze_table(&value, &boolean())
            .expect("analysis")
            .uncovered
            .is_none()
    );
    value.rules[0].cells.insert(
        "condition".to_owned(),
        norn_ir::ZenExpression("  ".to_owned()),
    );
    let result = analyze_table(&value, &boolean()).expect("wildcard");
    assert_eq!(result.unreachable_rows, ["row_1"]);
}

#[test]
fn absence_and_null_are_distinct_counterexample_states() {
    let optional = schema(
        json!({"type":"object", "properties":{"flag":{"type":"null"}},
        "additionalProperties":false}),
    );
    let absent = analyze_table(&table("first", &[]), &optional).expect("analysis");
    assert_eq!(absent.uncovered_count, 2);
    assert_eq!(absent.uncovered, Some(json!({})));
    let required = schema(
        json!({"type":"object", "properties":{"flag":{"type":"null"}},
        "required":["flag"], "additionalProperties":false}),
    );
    let null = analyze_table(&table("first", &[]), &required).expect("analysis");
    assert_eq!(null.uncovered_count, 1);
    assert_eq!(null.uncovered, Some(json!({"flag":null})));
}

#[test]
fn oversized_finite_values_are_unknown() {
    let input = schema(json!({"type":"string","enum":["x".repeat(65_536)]}));
    assert_eq!(
        analyze_table(&table("first", &[""]), &input),
        Err(Unknown::Limit)
    );
}

#[test]
fn array_outputs_do_not_claim_first_policy_shadowing() {
    let mut value = table("first", &["", ""]);
    value.outputs[0].field = "result[]".to_owned();
    assert_eq!(analyze_table(&value, &boolean()), Err(Unknown::ArrayOutput));
}

#[test]
fn shared_subject_cells_never_claim_independent_evaluation() {
    let mut value = table("collect", &["true"]);
    let mut second = value.inputs[0].clone();
    second.id = "shared".to_owned();
    second.field = None;
    value.inputs.push(second);
    value.rules[0].cells.insert(
        "shared".to_owned(),
        norn_ir::ZenExpression("$ == true".to_owned()),
    );
    assert_eq!(
        analyze_table(&value, &boolean()),
        Err(Unknown::SharedSubject)
    );
}

#[test]
fn opaque_temporal_intermediates_never_claim_string_semantics() {
    let mut value = table("first", &["type(d) == \"string\""]);
    value.inputs[0].field = None;
    value.input_field = Some("{d: date('2026-01-01')}".to_owned());
    assert_eq!(
        analyze_table(
            &value,
            &schema(json!({"type":"object","properties":{},"additionalProperties":false}))
        ),
        Err(Unknown::TemporalIntermediate)
    );
    value.input_field = None;
    value.inputs[0].field = Some("date('2026-01-01')".to_owned());
    assert_eq!(
        analyze_table(&value, &boolean()),
        Err(Unknown::TemporalIntermediate)
    );
}

#[test]
fn noncanonical_intermediate_strings_never_claim_renormalized_semantics() {
    let composed = "join(['e', '\u{0301}'], '')";
    let mut value = table("first", &[&format!("d == {composed}")]);
    value.inputs[0].field = None;
    value.input_field = Some(format!("{{d: {composed}}}"));
    assert_eq!(
        analyze_table(
            &value,
            &schema(json!({"type":"object","properties":{},"additionalProperties":false}))
        ),
        Err(Unknown::NonCanonicalIntermediate)
    );
    value.input_field = None;
    value.inputs[0].field = Some(composed.to_owned());
    value.rules[0].cells.insert(
        "condition".to_owned(),
        norn_ir::ZenExpression("'é'".to_owned()),
    );
    assert_eq!(
        analyze_table(&value, &boolean()),
        Err(Unknown::NonCanonicalIntermediate)
    );
}

#[test]
fn overlap_witness_identity_bytes_are_bounded_before_allocation() {
    let mut large = table("collect", &["true", "true"]);
    large.rules[0].id = "r".repeat(65_536);
    assert_eq!(analyze_table(&large, &boolean()), Err(Unknown::Limit));
}
