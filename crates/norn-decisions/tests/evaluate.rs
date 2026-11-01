#![allow(
    clippy::arithmetic_side_effects,
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::pedantic,
    missing_docs,
    reason = "integration-test crate"
)]
#![deny(
    clippy::match_wildcard_for_single_variants,
    clippy::format_collect,
    clippy::format_push_string,
    clippy::inefficient_to_string,
    clippy::large_types_passed_by_value,
    clippy::needless_pass_by_value,
    clippy::large_futures
)]

//! `norn-decisions` behaviour beyond the corpus: determinism and replay,
//! insertion-independent map order, exact numeric goldens (T3-NUM-1),
//! typed strict-error and budget details, input binding and schema checks,
//! sub-decision closure admission, and the engine pin.

mod support;

use std::collections::BTreeMap;

use norn_decisions::{
    AdmissionError, AdmittedDecision, DecisionError, ENGINE_TAG, StrictSite, evaluate,
    evaluate_explained_with_fact_check,
};
use norn_expr::{EvaluationBudget, Value};
use serde_json::{Value as Json, json};
use support::{admit_fixture, binding, member, num, read_json, repo_root, rule, run, value};

fn corpus(name: &str) -> Json {
    read_json(
        &repo_root()
            .join("crates/norn-decisions/tests/zen_corpus")
            .join(name),
    )
}

fn closed(properties: &Json) -> Json {
    let required: Vec<Json> = properties
        .as_object()
        .unwrap()
        .keys()
        .map(|key| Json::String(key.clone()))
        .collect();
    json!({ "type": "object", "properties": properties, "required": required, "additionalProperties": false })
}

/// A decide rule whose single expression node emits `commands` (items
/// `{actionId: string, value?: number}`).
fn commands_rule(rule_id: &str, slice: &Json, commands: &str) -> Json {
    let command = json!({
        "type": "object",
        "properties": { "actionId": { "type": "string" }, "value": { "type": "number" } },
        "required": ["actionId"],
        "additionalProperties": false
    });
    let record = closed(&json!({ "id": { "type": "string" } }));
    json!({
        "ruleId": rule_id,
        "ruleVersion": "2.0",
        "contract": "decide",
        "inputSchema": closed(&json!({ "slice": slice, "event": closed(&json!({ "kind": { "type": "string" } })) })),
        "outputSchema": closed(&json!({
            "facts": { "type": "array", "items": record },
            "commands": { "type": "array", "items": command },
            "timers": { "type": "array", "items": record }
        })),
        "scopeRequired": [],
        "graph": {
            "nodes": [
                { "id": "in", "name": "in", "type": "inputNode" },
                { "id": "x", "name": "x", "type": "expressionNode", "content": { "expressions": [
                    { "id": "f", "key": "facts", "value": "[]" },
                    { "id": "c", "key": "commands", "value": commands },
                    { "id": "t", "key": "timers", "value": "[]" }
                ] } },
                { "id": "out", "name": "out", "type": "outputNode" }
            ],
            "edges": [
                { "id": "e1", "sourceId": "in", "targetId": "x" },
                { "id": "e2", "sourceId": "x", "targetId": "out" }
            ]
        }
    })
}

/// A record parsed exactly from JSON text.
fn exact_record(text: &str) -> norn_ir::RuntimeObject {
    match norn_ir::RuntimeValue::from_json_text(text).unwrap() {
        norn_ir::RuntimeValue::Object(record) => record,
        other @ (norn_ir::RuntimeValue::Null
        | norn_ir::RuntimeValue::Bool(_)
        | norn_ir::RuntimeValue::Number(_)
        | norn_ir::RuntimeValue::String(_)
        | norn_ir::RuntimeValue::Array(_)) => panic!("not a record: {other:?}"),
    }
}

fn admit_inline(rule_json: &Json) -> AdmittedDecision {
    AdmittedDecision::admit(&rule(rule_json), &BTreeMap::new(), binding()).unwrap()
}

fn event() -> Value {
    Value::object([("kind", Value::from("k"))])
}

#[test]
fn a_host_fact_refusal_happens_before_patch_lowering_and_returns_no_effects() {
    let mut source = commands_rule(
        "demo.checked_patch",
        &closed(&json!({})),
        "[{actionId: 'notify'}]",
    );
    source["outputSchema"]["properties"]["facts"]["items"] = closed(&json!({
        "op": {"type": "string", "enum": ["patch"]},
        "objectType": {"type": "string", "enum": ["demo.Order"]},
        "key": {"type": "string"},
        "patch": closed(&json!({"half": {"type": "number"}}))
    }));
    source["graph"]["nodes"][1]["content"]["expressions"][0]["value"] =
        json!("[{op: 'patch', objectType: 'demo.Order', key: 'o1', patch: {half: 1.25}}]");
    let decision = admit_inline(&source);
    let slice = decision
        .validate_slice(Value::object(std::iter::empty::<(&str, Value)>()))
        .unwrap();
    let event = decision.type_event(event()).unwrap();
    let mut checked = 0;
    let result = evaluate_explained_with_fact_check(
        &decision,
        &slice,
        &event,
        EvaluationBudget::default(),
        |record| {
            checked += 1;
            assert_eq!(
                record.get("op").and_then(norn_ir::RuntimeValue::as_str),
                Some("patch")
            );
            assert_eq!(
                record.get("patch").unwrap().get("half"),
                Some(&norn_ir::RuntimeValue::from_json_text("1.25").unwrap())
            );
            Err("effective property refused".to_owned())
        },
    );
    assert_eq!(checked, 1);
    assert_eq!(
        result,
        Err(DecisionError::FactContract {
            path: "output.facts[0]".to_owned(),
            reason: "effective property refused".to_owned(),
        })
    );
}

fn reorder_case() -> (Value, Value) {
    (
        value(
            &json!({ "sku": { "skuId": "sku-1", "onHand": 5, "reorderPoint": 10, "status": "active" } }),
        ),
        value(&json!({ "quantity": 100, "unitCost": 50 })),
    )
}

#[test]
fn evaluation_is_deterministic_and_replayable() {
    let fixture = corpus("reorder_decision.json");
    let decision = admit_fixture(&fixture).unwrap();
    let (slice, event) = reorder_case();
    let first = run(
        &decision,
        slice.clone(),
        event.clone(),
        EvaluationBudget::default(),
    )
    .unwrap();
    assert!(first.units_used > 0);
    for _ in 0..3 {
        let again = run(
            &decision,
            slice.clone(),
            event.clone(),
            EvaluationBudget::default(),
        );
        assert_eq!(again.as_ref(), Ok(&first));
    }
    // A clone, and an independent admission of the same closure, replay it
    // exactly, including the unit count.
    let replayed = run(
        &decision,
        slice.clone(),
        event.clone(),
        EvaluationBudget::default(),
    );
    assert_eq!(replayed.as_ref(), Ok(&first));
    let readmitted = admit_fixture(&fixture).unwrap();
    assert_eq!(readmitted.binding(), decision.binding());
    assert_eq!(readmitted.engine_tag(), ENGINE_TAG);
    let replayed = run(&readmitted, slice, event, EvaluationBudget::default());
    assert_eq!(replayed, Ok(first));
}

#[test]
fn a_budget_of_exactly_the_units_used_succeeds_and_one_less_fails() {
    let decision = admit_fixture(&corpus("reorder_decision.json")).unwrap();
    let (slice, event) = reorder_case();
    let used = run(
        &decision,
        slice.clone(),
        event.clone(),
        EvaluationBudget::default(),
    )
    .unwrap()
    .units_used;
    let exact = run(
        &decision,
        slice.clone(),
        event.clone(),
        EvaluationBudget::new(used),
    );
    assert_eq!(exact.map(|output| output.units_used), Ok(used));
    match run(&decision, slice, event, EvaluationBudget::new(used - 1)) {
        Err(DecisionError::BudgetExhausted {
            limit,
            used: needed,
        }) => {
            assert_eq!(limit, used - 1);
            assert!(needed > limit);
        }
        other => panic!("expected BudgetExhausted, got {other:?}"),
    }
}

#[test]
fn map_order_does_not_depend_on_hash_seeds_above_the_small_map_threshold() {
    // 40 keys: above zen's 32-key small-map threshold, where only the fork's
    // `deterministic-maps` keeps iteration in insertion (here: key) order.
    let keys: Vec<String> = (0..40).rev().map(|index| format!("k{index:02}")).collect();
    let properties: serde_json::Map<String, Json> = keys
        .iter()
        .map(|key| (key.clone(), json!({ "type": "integer" })))
        .collect();
    let slice_schema = closed(&json!({ "big": closed(&Json::Object(properties)) }));
    let decision = admit_inline(&commands_rule(
        "demo.rule.keys",
        &slice_schema,
        "map(keys(slice.big), {actionId: #})",
    ));
    let big = Value::Object(
        keys.iter()
            .enumerate()
            .map(|(index, key)| (key.clone(), Value::from(i64::try_from(index).unwrap())))
            .collect(),
    );
    let slice = Value::object([("big", big)]);
    let mut sorted = keys;
    sorted.sort();
    let expected: Vec<Value> = sorted
        .iter()
        .map(|key| Value::object([("actionId", Value::from(key.as_str()))]))
        .collect();
    for _ in 0..4 {
        let output = run(
            &decision,
            slice.clone(),
            event(),
            EvaluationBudget::default(),
        )
        .unwrap();
        assert_eq!(member(&output, "commands"), expected);
    }
}

#[test]
fn numeric_goldens_are_exact() {
    let slice_schema = closed(&json!({ "a": { "type": "number" }, "b": { "type": "number" } }));
    let decision = admit_inline(&commands_rule(
        "demo.rule.numbers",
        &slice_schema,
        "[{actionId: 'sum', value: slice.a + slice.b}, {actionId: 'product', value: slice.a * slice.b}, {actionId: 'quotient', value: slice.a / slice.b}]",
    ));
    let cases = [
        // (a, b, sum, product, quotient)
        ("0.1", "0.2", "0.3", "0.02", "0.5"),
        ("12.50", "4", "16.5", "50", "3.125"),
        ("7", "2", "9", "14", "3.5"),
        // 2^53 is the largest integer every neighbour of which f64 keeps.
        (
            "9007199254740991",
            "1",
            "9007199254740992",
            "9007199254740991",
            "9007199254740991",
        ),
        ("-1.5", "0.5", "-1", "-0.75", "-3"),
    ];
    for (a, b, sum, product, quotient) in cases {
        let slice = Value::object([("a", num(a)), ("b", num(b))]);
        let output = run(&decision, slice, event(), EvaluationBudget::default()).unwrap();
        let expected: Vec<Value> = [("sum", sum), ("product", product), ("quotient", quotient)]
            .into_iter()
            .map(|(id, number)| {
                Value::object([("actionId", Value::from(id)), ("value", num(number))])
            })
            .collect();
        assert_eq!(member(&output, "commands"), expected, "a = {a}, b = {b}");
    }
}

#[test]
fn outputs_beyond_f64_are_exact() {
    // The engine computes 2^53 + 1 exactly, and the canonical DecisionOutput
    // carries it exactly (#2770): no f64, no refusal, no rounding.
    let decision = admit_inline(&commands_rule(
        "demo.rule.exact",
        &closed(&json!({ "a": { "type": "number" } })),
        "[{actionId: 'sum', value: slice.a + 1}, {actionId: 'money', value: slice.a / 4 + 0.01}]",
    ));
    let slice = Value::object([("a", num("9007199254740992"))]);
    let output = run(&decision, slice, event(), EvaluationBudget::default()).unwrap();
    assert_eq!(
        member(&output, "commands"),
        vec![
            Value::object([
                ("actionId", Value::from("sum")),
                ("value", num("9007199254740993")),
            ]),
            Value::object([
                ("actionId", Value::from("money")),
                ("value", num("2251799813685248.01")),
            ]),
        ]
    );
    assert_eq!(
        output.output.commands[0]["value"],
        norn_ir::RuntimeValue::Number("9007199254740993".parse().unwrap())
    );
}

#[test]
fn strict_errors_are_typed_with_their_site() {
    let decision = admit_fixture(&corpus("strict_table_deny_row.json")).unwrap();
    let slice = Value::object([("amount", num("5")), ("limit", num("0"))]);
    match run(&decision, slice, event(), EvaluationBudget::default()) {
        Err(DecisionError::Strict {
            site, node_id, id, ..
        }) => {
            assert_eq!(site, StrictSite::DecisionTableInput);
            assert_eq!(node_id, "gate");
            assert_eq!(id, "c_over");
        }
        other => panic!("expected a strict error, got {other:?}"),
    }
    let decision = admit_fixture(&corpus("strict_switch.json")).unwrap();
    let slice = Value::object([("amount", num("5")), ("limit", num("0"))]);
    match run(&decision, slice, event(), EvaluationBudget::default()) {
        Err(DecisionError::Strict {
            site, node_id, id, ..
        }) => {
            assert_eq!(site, StrictSite::SwitchCondition);
            assert_eq!(node_id, "route");
            assert_eq!(id, "big");
        }
        other => panic!("expected a strict error, got {other:?}"),
    }
}

#[test]
fn inputs_are_bound_to_their_decision_and_schema() {
    let numbers = admit_fixture(&corpus("expression_node.json")).unwrap();
    let defaults = admit_fixture(&corpus("defaults.json")).unwrap();
    let slice = numbers
        .validate_slice(Value::object([("amount", num("1"))]))
        .unwrap();
    let event = numbers
        .type_event(Value::object([("kind", Value::from("k"))]))
        .unwrap();
    assert!(matches!(
        evaluate(&defaults, &slice, &event, EvaluationBudget::default()),
        Err(DecisionError::InputBinding { .. })
    ));
    for bad in [
        Value::object([("amount", Value::from("1"))]),
        Value::object([("amount", num("1")), ("extra", Value::Null)]),
        Value::object::<&str>([]),
    ] {
        assert!(
            matches!(
                numbers.validate_slice(bad.clone()),
                Err(DecisionError::InputSchema {
                    member: "slice",
                    ..
                })
            ),
            "{bad:?}"
        );
    }
}

fn sub_decision(rule_id: &str, calls: Option<&str>) -> Json {
    let record = closed(&json!({ "n": { "type": "number" } }));
    let mut nodes = vec![json!({ "id": "in", "name": "in", "type": "inputNode" })];
    let mut edges = Vec::new();
    let mut last = "in";
    if let Some(key) = calls {
        nodes.push(json!({ "id": "call", "name": "call", "type": "decisionNode", "content": { "key": key } }));
        edges.push(json!({ "id": "e_call", "sourceId": "in", "targetId": "call" }));
        last = "call";
    }
    nodes.push(json!({ "id": "out", "name": "out", "type": "outputNode" }));
    edges.push(json!({ "id": "e_out", "sourceId": last, "targetId": "out" }));
    json!({
        "ruleId": rule_id,
        "ruleVersion": "2.0",
        "contract": "sub_decision",
        "inputSchema": record,
        "outputSchema": record,
        "scopeRequired": [],
        "graph": { "nodes": nodes, "edges": edges }
    })
}

#[test]
fn malformed_fold_is_refused_instead_of_an_ordinary_sub_decision() {
    let fixture = corpus("reorder_decision.json");
    let mut root = rule(&fixture);
    let node = root
        .graph
        .as_mut()
        .unwrap()
        .nodes
        .iter_mut()
        .find_map(|node| {
            if let norn_ir::JdmNode::DecisionNode { content, .. } = node {
                Some(content)
            } else {
                None
            }
        })
        .unwrap();
    node.ordered_fold = Some(norn_ir::jdm::OrderedFold {
        semantics_version: "1.0".to_owned(),
        order: vec![],
        identity: "id".to_owned(),
        item_schema: BTreeMap::new(),
        accumulator_schema: BTreeMap::new(),
        initial: norn_ir::ZenExpression("{}".to_owned()),
        max_items: 1,
        max_accumulator_bytes: 100,
        arithmetic: norn_ir::jdm::FoldArithmetic {
            scale: 0,
            rounding: norn_ir::jdm::FoldRounding::Refuse,
        },
    });
    let subs = BTreeMap::from([(
        "retail.reorder_priority.rule".to_owned(),
        rule(&fixture["subDecisions"]["retail.reorder_priority.rule"]),
    )]);
    assert!(matches!(
        AdmittedDecision::admit(&root, &subs, binding()),
        Err(AdmissionError::InvalidSchema { .. })
    ));
}

#[test]
fn the_sub_decision_closure_is_exact_and_bounded() {
    let fixture = corpus("reorder_decision.json");
    let root = rule(&fixture);
    let priority = rule(&fixture["subDecisions"]["retail.reorder_priority.rule"]);
    let admit = |subs: &[(&str, &Json)]| {
        let subs: BTreeMap<String, norn_ir::RuleRoot> = subs
            .iter()
            .map(|(key, sub)| ((*key).to_owned(), rule(sub)))
            .collect();
        AdmittedDecision::admit(&root, &subs, binding())
    };

    assert!(matches!(
        admit(&[]),
        Err(AdmissionError::UnresolvedSubDecision { .. })
    ));
    let unused = sub_decision("demo.rule.unused", None);
    let mut subs = BTreeMap::new();
    subs.insert("retail.reorder_priority.rule".to_owned(), priority.clone());
    subs.insert("demo.unused".to_owned(), rule(&unused));
    assert!(matches!(
        AdmittedDecision::admit(&root, &subs, binding()),
        Err(AdmissionError::UnusedSubDecision { key }) if key == "demo.unused"
    ));

    // The root must be a decide graph and every sub-decision a sub_decision.
    assert!(matches!(
        AdmittedDecision::admit(&priority, &BTreeMap::new(), binding()),
        Err(AdmissionError::WrongContract {
            expected: "decide",
            ..
        })
    ));
    let mut wrong = BTreeMap::new();
    wrong.insert("retail.reorder_priority.rule".to_owned(), root.clone());
    assert!(matches!(
        AdmittedDecision::admit(&root, &wrong, binding()),
        Err(AdmissionError::WrongContract {
            expected: "sub_decision",
            ..
        })
    ));

    // A self-recursive sub-decision exceeds the depth bound at load time.
    let mut looped = root.clone();
    let graph = looped.graph.as_mut().unwrap();
    for node in &mut graph.nodes {
        if let norn_ir::jdm::JdmNode::DecisionNode { content, .. } = node {
            content.key = "demo.loop".to_owned();
        }
    }
    let looping = sub_decision("demo.rule.loop", Some("demo.loop"));
    let mut subs = BTreeMap::new();
    subs.insert("demo.loop".to_owned(), rule(&looping));
    assert!(matches!(
        AdmittedDecision::admit(&looped, &subs, binding()),
        Err(AdmissionError::SubDecisionBound { .. })
    ));
}

#[test]
fn engine_tag_is_the_qualified_pin() {
    let pin = read_json(&repo_root().join("crates/norn-skeleton-guard/zen-pin.json"));
    assert_eq!(pin["fork"]["tag"].as_str(), Some(ENGINE_TAG));
}

#[test]
fn admission_rechecks_bounded_string_literals_at_syntax_only_sites() {
    let mut bounded = commands_rule("demo.rule.bounded_strings", &closed(&json!({})), "[]");
    bounded["graph"]["nodes"][1]["content"]["expressions"][1]["value"] =
        json!("[{actionId: 'x', value: len(tokens(input.s, 'alnum', 8, 16))}]");
    assert!(AdmittedDecision::admit(&rule(&bounded), &BTreeMap::new(), binding()).is_ok());

    let mut invalid_input_field = commands_rule(
        "demo.rule.invalid_input_field_cap",
        &closed(&json!({})),
        "[]",
    );
    invalid_input_field["graph"]["nodes"][1]["content"]["inputField"] =
        json!("take(input.s, input.limit)");
    assert!(matches!(
        AdmittedDecision::admit(&rule(&invalid_input_field), &BTreeMap::new(), binding()),
        Err(AdmissionError::InvalidExpression { .. })
    ));

    let mut invalid_table_field = commands_rule(
        "demo.rule.invalid_table_field_cap",
        &closed(&json!({})),
        "[]",
    );
    invalid_table_field["graph"]["nodes"][1] = json!({
        "id": "x",
        "name": "x",
        "type": "decisionTableNode",
        "content": {
            "hitPolicy": "first",
            "inputs": [{
                "id": "i",
                "name": "Input",
                "field": "tokens(input.s, 'alnum', input.limit, 16)"
            }],
            "outputs": [{ "id": "o", "name": "Output", "field": "commands" }],
            "rules": [{ "_id": "r", "i": "true", "o": "[]" }]
        }
    });
    assert!(matches!(
        AdmittedDecision::admit(&rule(&invalid_table_field), &BTreeMap::new(), binding()),
        Err(AdmissionError::InvalidExpression { .. })
    ));

    let mut invalid_table_input_field = commands_rule(
        "demo.rule.invalid_table_input_field_cap",
        &closed(&json!({})),
        "[]",
    );
    invalid_table_input_field["graph"]["nodes"][1] = json!({
        "id": "x",
        "name": "x",
        "type": "decisionTableNode",
        "content": {
            "hitPolicy": "first",
            "inputField": "take(input.s, input.limit)",
            "inputs": [{ "id": "i", "name": "Input" }],
            "outputs": [{ "id": "o", "name": "Output", "field": "commands" }],
            "rules": [{ "_id": "r", "i": "true", "o": "[]" }]
        }
    });
    assert!(matches!(
        AdmittedDecision::admit(
            &rule(&invalid_table_input_field),
            &BTreeMap::new(),
            binding()
        ),
        Err(AdmissionError::InvalidExpression { .. })
    ));

    let mut invalid_subdecision_input_field = commands_rule(
        "demo.rule.invalid_subdecision_input_field_cap",
        &closed(&json!({})),
        "[]",
    );
    invalid_subdecision_input_field["graph"]["nodes"][1] = json!({
        "id": "x",
        "name": "x",
        "type": "decisionNode",
        "content": {
            "key": "demo.rule.bounded_child",
            "inputField": "take(input.s, input.limit)"
        }
    });
    let mut child = commands_rule("demo.rule.bounded_child", &closed(&json!({})), "[]");
    child["contract"] = json!("sub_decision");
    let sub_decisions = BTreeMap::from([("demo.rule.bounded_child".to_owned(), rule(&child))]);
    assert!(matches!(
        AdmittedDecision::admit(
            &rule(&invalid_subdecision_input_field),
            &sub_decisions,
            binding()
        ),
        Err(AdmissionError::InvalidExpression { .. })
    ));

    for source in [
        "[{actionId: 'x', value: len(tokens(input.s, 'alnum', input.limit, 16))}]",
        "[{actionId: 'x', value: len(tokens(input.s, 'alnum', 257, 16))}]",
        "[{actionId: 'x', value: take(input.s, 16385)}]",
    ] {
        let mut invalid = commands_rule("demo.rule.invalid_string_cap", &closed(&json!({})), "[]");
        invalid["graph"]["nodes"][1]["content"]["expressions"][1]["value"] = json!(source);
        assert!(
            matches!(
                AdmittedDecision::admit(&rule(&invalid), &BTreeMap::new(), binding()),
                Err(AdmissionError::InvalidExpression { .. })
            ),
            "syntax-only decision admission accepted `{source}`"
        );
    }
}

#[test]
fn inputs_validated_under_another_version_of_the_rule_are_refused() {
    let v1 = commands_rule(
        "demo.rule.versioned",
        &closed(&json!({ "a": { "type": "number" } })),
        "[]",
    );
    let mut v2 = v1.clone();
    v2["inputSchema"]["properties"]["slice"] = closed(&json!({ "a": { "type": "integer" } }));
    let (v1, v2) = (admit_inline(&v1), admit_inline(&v2));
    let slice = v1
        .validate_slice(Value::object([("a", num("1.5"))]))
        .unwrap();
    let event = v1.type_event(event()).unwrap();
    assert!(evaluate(&v1, &slice, &event, EvaluationBudget::default()).is_ok());
    assert!(matches!(
        evaluate(&v2, &slice, &event, EvaluationBudget::default()),
        Err(DecisionError::InputBinding { .. })
    ));
    // An independent admission of the identical rule accepts it.
    let same = admit_inline(&commands_rule(
        "demo.rule.versioned",
        &closed(&json!({ "a": { "type": "number" } })),
        "[]",
    ));
    assert!(evaluate(&same, &slice, &event, EvaluationBudget::default()).is_ok());
}

#[test]
fn decide_members_must_be_required_and_sub_decision_schemas_closed() {
    let mut optional = commands_rule("demo.rule.optional", &closed(&json!({})), "[]");
    optional["outputSchema"]["required"] = json!(["facts", "commands"]);
    assert!(matches!(
        AdmittedDecision::admit(&rule(&optional), &BTreeMap::new(), binding()),
        Err(AdmissionError::InvalidSchema { .. })
    ));

    let fixture = corpus("reorder_decision.json");
    let root = rule(&fixture);
    let mut priority = fixture["subDecisions"]["retail.reorder_priority.rule"].clone();
    let path = repo_root().join(priority["ruleFile"].as_str().unwrap());
    priority = read_json(&path)["payload"].clone();
    priority["inputSchema"]["additionalProperties"] = json!(true);
    let mut subs = BTreeMap::new();
    subs.insert("retail.reorder_priority.rule".to_owned(), rule(&priority));
    assert!(matches!(
        AdmittedDecision::admit(&root, &subs, binding()),
        Err(AdmissionError::InvalidSchema { .. })
    ));
}

#[test]
fn numeric_defaults_come_from_source_text() {
    let mut rule_json = commands_rule("demo.rule.big_default", &closed(&json!({})), "[]");
    rule_json["graph"]["nodes"][1]["content"]["expressions"] = json!([
        { "id": "f", "key": "facts", "value": "[]" },
        { "id": "t", "key": "timers", "value": "[]" }
    ]);
    // f64 decoding would turn these into 10000000000000000 and 1, and even a
    // short decimal is refused: the authored text is not in the IR.
    for number in [
        json!(10000000000000001_u64),
        serde_json::from_str::<Json>("1.00000000000000001").unwrap(),
        json!(12.5),
    ] {
        rule_json["defaults"] = json!({ "commands": [{ "actionId": "x", "value": number }] });
        assert!(
            matches!(
                AdmittedDecision::admit(&rule(&rule_json), &BTreeMap::new(), binding()),
                Err(AdmissionError::InvalidDefault { .. })
            ),
            "{number}"
        );
    }
    // From the artifact's source text, numeric defaults are exact (#2770).
    let source = r#"{ "commands": [{ "actionId": "x", "value": 10000000000000001 }] }"#;
    rule_json["defaults"] = serde_json::from_str(source).unwrap();
    let exact = exact_record(source);
    let decision = AdmittedDecision::admit_with_defaults(
        &rule(&rule_json),
        &BTreeMap::new(),
        binding(),
        Some(&exact),
    )
    .unwrap();
    let output = run(
        &decision,
        Value::object::<&str>([]),
        event(),
        EvaluationBudget::default(),
    )
    .unwrap();
    assert_eq!(
        member(&output, "commands"),
        vec![Value::object([
            ("actionId", Value::from("x")),
            ("value", num("10000000000000001")),
        ])]
    );
    // A number whose historical JSON decoding differs from a correctly
    // rounded parse is still its own artifact's default.
    let source = r#"{ "commands": [{ "actionId": "x", "value": 0.9999999999999999 }] }"#;
    rule_json["defaults"] = serde_json::from_str(source).unwrap();
    let own = AdmittedDecision::admit_with_defaults(
        &rule(&rule_json),
        &BTreeMap::new(),
        binding(),
        Some(&exact_record(source)),
    );
    assert!(own.is_ok(), "{own:?}");
    // Source text that is not this artifact's defaults is refused.
    let other = exact_record(r#"{ "commands": [{ "actionId": "x", "value": 2 }] }"#);
    assert!(matches!(
        AdmittedDecision::admit_with_defaults(
            &rule(&rule_json),
            &BTreeMap::new(),
            binding(),
            Some(&other),
        ),
        Err(AdmissionError::InvalidDefault { .. })
    ));
    // Non-numeric defaults are exact and applied.
    rule_json["defaults"] = json!({ "commands": [{ "actionId": "x" }] });
    let decision = AdmittedDecision::admit(&rule(&rule_json), &BTreeMap::new(), binding()).unwrap();
    let output = run(
        &decision,
        Value::object::<&str>([]),
        event(),
        EvaluationBudget::default(),
    )
    .unwrap();
    assert_eq!(
        member(&output, "commands"),
        vec![Value::object([("actionId", Value::from("x"))])]
    );
}

fn fold_rules(
    item: &Json,
    state: &Json,
    stages: &[Vec<(&str, &str)>],
) -> (norn_ir::RuleRoot, BTreeMap<String, norn_ir::RuleRoot>) {
    let mut root = commands_rule(
        "example.fold",
        &closed(&json!({"items":{"type":"array","items":item},"initial":state})),
        "[{actionId: 'result', state: result}]",
    );
    root["outputSchema"]["properties"]["commands"]["items"] =
        closed(&json!({"actionId":{"type":"string"},"state":state}));
    root["graph"]["nodes"].as_array_mut().unwrap().insert(
        1,
        json!({"id":"fold","name":"fold","type":"decisionNode","content":{
            "key":"example.step","inputField":"slice.items","outputPath":"result","passThrough":true
        }}),
    );
    root["graph"]["edges"][0]["targetId"] = json!("fold");
    root["graph"]["edges"]
        .as_array_mut()
        .unwrap()
        .push(json!({"id":"ef","sourceId":"fold","targetId":"x"}));
    let mut root = rule(&root);
    if let norn_ir::JdmNode::DecisionNode { content, .. } =
        &mut root.graph.as_mut().unwrap().nodes[1]
    {
        content.ordered_fold = Some(norn_ir::jdm::OrderedFold {
            semantics_version: "1.0".into(),
            identity: "id".into(),
            order: vec![norn_ir::jdm::FoldOrderKey {
                property: "id".into(),
                kind: norn_ir::jdm::FoldOrderKind::String,
            }],
            item_schema: serde_json::from_value(item.clone()).unwrap(),
            accumulator_schema: serde_json::from_value(state.clone()).unwrap(),
            initial: norn_ir::ZenExpression("slice.initial".into()),
            max_items: 4096,
            max_accumulator_bytes: 1048576,
            arithmetic: norn_ir::jdm::FoldArithmetic {
                scale: 0,
                rounding: norn_ir::jdm::FoldRounding::HalfAwayFromZero,
            },
        });
    }
    let mut helper = sub_decision("example.step", None);
    helper["inputSchema"] =
        closed(&json!({"accumulator":state,"item":item,"index":{"type":"integer"}}));
    helper["outputSchema"] = state.clone();
    let mut nodes = vec![json!({"id":"in","name":"in","type":"inputNode"})];
    let mut edges = Vec::new();
    let mut previous = "in".to_owned();
    for (i, assignments) in stages.iter().enumerate() {
        let id = format!("s{i}");
        let expressions: Vec<Json> = assignments
            .iter()
            .map(|(key, expression)| json!({"id":key,"key":key,"value":expression}))
            .collect();
        nodes.push(json!({"id":id,"name":id,"type":"expressionNode","content":{"expressions":expressions,"passThrough":i+1<stages.len()}}));
        edges.push(json!({"id":format!("e{i}"),"sourceId":previous,"targetId":id}));
        previous = id;
    }
    nodes.push(json!({"id":"out","name":"out","type":"outputNode"}));
    edges.push(json!({"id":"eo","sourceId":previous,"targetId":"out"}));
    helper["graph"] = json!({"nodes":nodes,"edges":edges});
    (
        root,
        BTreeMap::from([("example.step".into(), rule(&helper))]),
    )
}

fn folded(
    rules: &(norn_ir::RuleRoot, BTreeMap<String, norn_ir::RuleRoot>),
    initial: Json,
    items: Json,
) -> Result<norn_ir::RuntimeValue, Box<dyn std::error::Error>> {
    let admitted = AdmittedDecision::admit(&rules.0, &rules.1, binding())?;
    let result = run(
        &admitted,
        value(&Json::Object(
            [("items".into(), items), ("initial".into(), initial)]
                .into_iter()
                .collect(),
        )),
        event(),
        EvaluationBudget::new(2_000_000),
    )?;
    Ok(result.output.commands[0]["state"].clone())
}

#[test]
fn declared_capacity_allocation_uses_order_and_preserves_residuals() {
    let item = closed(&json!({"id":{"type":"string"},"capacity":{"type":"integer"}}));
    let state = closed(
        &json!({"demand":{"type":"integer"},"residual":{"type":"integer"},"allocated":{"type":"array","items":{"type":"integer"}}}),
    );
    let rules = fold_rules(
        &item,
        &state,
        &[
            vec![(
                "taken",
                "accumulator.demand < item.capacity ? accumulator.demand : item.capacity",
            )],
            vec![
                ("demand", "accumulator.demand - taken"),
                ("residual", "accumulator.residual + item.capacity - taken"),
                ("allocated", "flatten([accumulator.allocated, [taken]])"),
            ],
        ],
    );
    let initial = json!({"demand":7,"residual":0,"allocated":[]});
    let expected = value(&json!({"demand":0,"residual":3,"allocated":[5,2]}));
    for items in [
        json!([{"id":"a","capacity":5},{"id":"b","capacity":5}]),
        json!([{"id":"b","capacity":5},{"id":"a","capacity":5}]),
    ] {
        assert_eq!(
            folded(&rules, initial.clone(), items).unwrap(),
            norn_decisions::value_to_runtime(&expected).unwrap()
        );
    }
}

fn fifo_fold() -> (norn_ir::RuleRoot, BTreeMap<String, norn_ir::RuleRoot>) {
    let layer = closed(
        &json!({"id":{"type":"string"},"quantity":{"type":"integer"},"value":{"type":"integer"}}),
    );
    let state = closed(
        &json!({"demand":{"type":"integer"},"value":{"type":"integer"},"layers":{"type":"array","items":layer},"allocations":{"type":"array","items":layer}}),
    );
    let mut rules = fold_rules(
        &layer,
        &state,
        &[
            vec![(
                "taken",
                "accumulator.demand < item.quantity ? accumulator.demand : item.quantity",
            )],
            vec![(
                "cost",
                "taken == item.quantity ? item.value : taken * item.value / item.quantity",
            )],
            vec![
                (
                    "demand",
                    "item.quantity > 0 and item.value >= 0 and accumulator.demand >= 0 ? accumulator.demand - taken : null",
                ),
                ("value", "accumulator.value + cost"),
                (
                    "layers",
                    "flatten([accumulator.layers, taken == item.quantity ? [] : [{id:item.id,quantity:item.quantity-taken,value:item.value-cost}]])",
                ),
                (
                    "allocations",
                    "flatten([accumulator.allocations, taken == 0 ? [] : [{id:item.id,quantity:taken,value:cost}]])",
                ),
            ],
        ],
    );
    let expected: norn_ir::JsonValue =
        serde_json::from_value(json!({"type":"array","items":layer})).unwrap();
    if let Some(norn_ir::JsonValue::Object(properties)) = rules.0.input_schema.get_mut("properties")
        && let Some(norn_ir::JsonValue::Object(slice)) = properties.get_mut("slice")
        && let Some(norn_ir::JsonValue::Object(fields)) = slice.get_mut("properties")
    {
        fields.insert("expected".into(), expected);
    }
    if let norn_ir::JdmNode::ExpressionNode { content, .. } =
        &mut rules.0.graph.as_mut().unwrap().nodes[2]
    {
        content.expressions[1].value=norn_ir::ZenExpression("slice.initial.demand > 0 and result.demand == 0 and (slice.expected == null or result.allocations == slice.expected) ? [{actionId:'result',state:result}] : null".into());
    }
    rules
}

#[test]
fn declared_fifo_matches_all_retained_reference_histories_and_cp1() {
    let rules = fifo_fold();
    let cases: Vec<Json> =
        serde_json::from_str(include_str!("fixtures/fold-fifo-oracle.json")).unwrap();
    assert_eq!(cases.len(), 1008);
    for case in cases {
        let mut layers = json!([]);
        let mut last = None;
        for event in case["history"].as_array().unwrap() {
            let quantity = event["quantity"].as_str().unwrap().parse::<i64>().unwrap();
            if event["kind"] == "receipt" {
                layers.as_array_mut().unwrap().push(json!({"id":event["id"],"quantity":quantity,"value":event["value"].as_str().unwrap().parse::<i64>().unwrap()}));
            } else {
                let result = folded(
                    &rules,
                    json!({"demand":quantity,"value":0,"layers":[],"allocations":[]}),
                    layers,
                )
                .unwrap();
                let result: Json =
                    serde_json::from_slice(&result.canonical_json().unwrap()).unwrap();
                assert_eq!(result["demand"], 0);
                layers = result["layers"].clone();
                last = Some(result);
            }
        }
        for (actual, field) in [
            (layers, "layers"),
            (last.unwrap()["allocations"].clone(), "allocations"),
        ] {
            let expected:Vec<Json>=case[field].as_array().unwrap().iter().map(|l|json!({"id":l["receipt"],"quantity":l["quantity"].as_str().unwrap().parse::<i64>().unwrap(),"value":l["value"].as_str().unwrap().parse::<i64>().unwrap()})).collect();
            assert_eq!(actual, json!(expected));
        }
    }
    let result = folded(
        &rules,
        json!({"demand":4,"value":0,"layers":[],"allocations":[]}),
        json!([{"id":"a","quantity":2,"value":20},{"id":"b","quantity":3,"value":60}]),
    )
    .unwrap();
    assert_eq!(result, norn_decisions::value_to_runtime(&value(&json!({"demand":0,"value":60,"layers":[{"id":"b","quantity":1,"value":20}],"allocations":[{"id":"a","quantity":2,"value":20},{"id":"b","quantity":2,"value":40}]}))).unwrap());
}

fn average_fold() -> (norn_ir::RuleRoot, BTreeMap<String, norn_ir::RuleRoot>) {
    let number = json!({"type":"integer"});
    let issue = closed(
        &json!({"id":{"type":"string"},"quantity":number,"value":number,"rq":number,"rv":number}),
    );
    let item = closed(
        &json!({"id":{"type":"string"},"eventId":{"type":"string"},"kind":{"type":"string","enum":["receipt","delivery","return","transfer"]},"reference":{"type":"string"},"expected":{"type":["integer","null"]},"quantity":number,"value":number}),
    );
    let state = closed(
        &json!({"quantity":number,"value":number,"allocation":number,"issues":{"type":"array","items":issue}}),
    );
    fold_rules(
        &item,
        &state,
        &[
            vec![(
                "prior",
                "item.kind == 'return' ? filter(accumulator.issues, #.id == item.reference)[0] : {id:'',quantity:0,value:0,rq:0,rv:0}",
            )],
            vec![(
                "share",
                "item.kind == 'receipt' ? item.value : item.kind == 'return' ? (item.quantity == prior.quantity-prior.rq ? prior.value-prior.rv : item.quantity*prior.value/prior.quantity) : (item.quantity == accumulator.quantity ? accumulator.value : item.quantity*accumulator.value/accumulator.quantity)",
            )],
            vec![
                (
                    "cost",
                    "item.kind == 'return' and share > prior.value-prior.rv ? prior.value-prior.rv : share",
                ),
                (
                    "valid",
                    "item.quantity > 0 and (item.kind == 'receipt' ? item.value >= 0 : item.kind == 'return' ? item.quantity <= prior.quantity-prior.rq : item.quantity <= accumulator.quantity and len(filter(accumulator.issues, #.id == item.eventId)) == 0)",
                ),
            ],
            vec![
                (
                    "quantity",
                    "valid and (item.expected == null or cost == item.expected) ? accumulator.quantity + (item.kind == 'delivery' or item.kind == 'transfer' ? -item.quantity : item.quantity) : null",
                ),
                (
                    "value",
                    "accumulator.value + (item.kind == 'delivery' or item.kind == 'transfer' ? -cost : cost)",
                ),
                ("allocation", "cost"),
                (
                    "issues",
                    "item.kind == 'delivery' or item.kind == 'transfer' ? flatten([accumulator.issues,[{id:item.eventId,quantity:item.kind == 'transfer' ? 0 : item.quantity,value:cost,rq:0,rv:0}]]) : item.kind == 'return' ? map(accumulator.issues, #.id == item.reference ? {id:#.id,quantity:#.quantity,value:#.value,rq:#.rq+item.quantity,rv:#.rv+cost} : #) : accumulator.issues",
                ),
            ],
        ],
    )
}

#[test]
fn declared_average_matches_every_retained_prefix_with_original_cost_returns() {
    let rules = average_fold();
    let cases: Vec<Vec<Json>> =
        serde_json::from_str(include_str!("fixtures/fold-average-oracle.json")).unwrap();
    assert_eq!(cases.len(), 648);
    for history in cases {
        let mut items = Vec::new();
        for (i, step) in history.iter().enumerate() {
            let event = &step["event"];
            items.push(json!({"id":format!("{i:03}"),"eventId":event["id"],"expected":null,"reference":event.get("delivery").and_then(Json::as_str).unwrap_or(""),"kind":event["kind"],"quantity":event["quantity"].as_str().unwrap().parse::<i64>().unwrap(),"value":event.get("value").and_then(Json::as_str).unwrap_or("0").parse::<i64>().unwrap()}));
            let result = folded(
                &rules,
                json!({"quantity":0,"value":0,"allocation":0,"issues":[]}),
                json!(items),
            )
            .unwrap();
            let result: Json = serde_json::from_slice(&result.canonical_json().unwrap()).unwrap();
            for (field, expected) in [
                ("quantity", &step["balance"]["quantity"]),
                ("value", &step["balance"]["value"]),
                ("allocation", &step["allocation"]["value"]),
            ] {
                assert_eq!(
                    result[field],
                    expected.as_str().unwrap().parse::<i64>().unwrap(),
                    "{i}: {items:?}"
                );
            }
        }
    }
}

fn movement(
    index: usize,
    id: &str,
    kind: &str,
    quantity: i64,
    amount: i64,
    reference: &str,
) -> Json {
    json!({"id":format!("{index:03}"),"eventId":id,"expected":null,"kind":kind,"quantity":quantity,"value":amount,"reference":reference})
}
fn average_result(items: Vec<Json>) -> Result<Json, Box<dyn std::error::Error>> {
    let result = folded(
        &average_fold(),
        json!({"quantity":0,"value":0,"allocation":0,"issues":[]}),
        Json::Array(items),
    )?;
    Ok(serde_json::from_slice(&result.canonical_json()?)?)
}

#[test]
fn declared_transfer_return_and_cancellation_vectors_preserve_original_allocations() {
    for quantity in 1..=6 {
        for amount in 0..=6 {
            for moved in 1..=quantity {
                let receipt = movement(0, "r", "receipt", quantity, amount, "");
                let transfer = movement(1, "t", "transfer", moved, 0, "");
                let source = average_result(vec![receipt.clone(), transfer.clone()]).unwrap();
                let carried = source["allocation"].as_i64().unwrap();
                let destination =
                    average_result(vec![movement(0, "t", "receipt", moved, carried, "")]).unwrap();
                assert_eq!(
                    source["quantity"].as_i64().unwrap()
                        + destination["quantity"].as_i64().unwrap(),
                    quantity
                );
                assert_eq!(
                    source["value"].as_i64().unwrap() + destination["value"].as_i64().unwrap(),
                    amount
                );
                assert!(
                    average_result(vec![
                        receipt.clone(),
                        transfer.clone(),
                        movement(2, "ret", "return", moved, 0, "t")
                    ])
                    .is_err()
                );
                assert!(
                    average_result(vec![
                        receipt.clone(),
                        transfer.clone(),
                        movement(2, "more", "receipt", quantity, amount, ""),
                        movement(3, "t", "delivery", 1, 0, "")
                    ])
                    .is_err()
                );
                let restored = average_result(vec![receipt]).unwrap();
                assert_eq!(restored["quantity"], quantity);
                assert_eq!(restored["value"], amount);
            }
        }
    }
    let receipt = movement(0, "r", "receipt", 5, 3, "");
    let delivery = movement(1, "d", "delivery", 3, 0, "");
    let returned = movement(2, "ret", "return", 1, 0, "d");
    let before = average_result(vec![receipt.clone(), delivery.clone(), returned.clone()]).unwrap();
    assert_eq!(before["allocation"], 1);
    assert!(average_result(vec![receipt.clone(), returned]).is_err());
    let after_return_cancel = average_result(vec![receipt.clone(), delivery]).unwrap();
    assert_eq!(after_return_cancel["quantity"], 2);
    assert_eq!(after_return_cancel["value"], 1);
    let restored = average_result(vec![receipt]).unwrap();
    assert_eq!(restored["quantity"], 5);
    assert_eq!(restored["value"], 3);
}

#[test]
fn declared_live_allocation_guards_refuse_changed_cost_or_receipt_identity() {
    let receipt = movement(0, "r", "receipt", 5, 3, "");
    let later = movement(1, "r2", "receipt", 1, 100, "");
    let mut delivery = movement(2, "d", "delivery", 1, 0, "");
    delivery["expected"] = json!(17);
    assert!(average_result(vec![receipt.clone(), later, delivery.clone()]).is_ok());
    assert!(average_result(vec![receipt, delivery]).is_err());
    let rules = fifo_fold();
    let admitted = AdmittedDecision::admit(&rules.0, &rules.1, binding()).unwrap();
    let input = value(
        &json!({"items":[{"id":"b","quantity":1,"value":10}],"initial":{"demand":1,"value":0,"layers":[],"allocations":[]},"expected":[{"id":"a","quantity":1,"value":10}]}),
    );
    assert!(run(&admitted, input, event(), EvaluationBudget::new(100_000)).is_err());
    for a in 0..=6 {
        for b in 0..=6 {
            for moved in 1..=5 {
                let source = folded(
                    &rules,
                    json!({"demand":moved,"value":0,"layers":[],"allocations":[]}),
                    json!([{"id":"a","quantity":2,"value":a},{"id":"b","quantity":3,"value":b}]),
                )
                .unwrap();
                let source: Json =
                    serde_json::from_slice(&source.canonical_json().unwrap()).unwrap();
                let carried = source["value"].as_i64().unwrap();
                let residual: i64 = source["layers"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|layer| layer["value"].as_i64().unwrap())
                    .sum();
                assert_eq!(carried + residual, a + b);
                let destination = movement(0, "t", "receipt", moved, carried, "");
                let issued = movement(1, "d", "delivery", moved, 0, "");
                assert_eq!(
                    average_result(vec![destination, issued.clone()]).unwrap()["allocation"],
                    carried
                );
                assert!(average_result(vec![issued]).is_err());
            }
        }
    }
}
