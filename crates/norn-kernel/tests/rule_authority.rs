#![allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    missing_docs,
    reason = "integration-test fixture setup and assertions"
)]

//! Correspondence for `field_authority.exactRuleBindingTest` and `forgedOriginRefusedTest`.

use std::collections::{BTreeMap, BTreeSet};
use std::convert::Infallible;

use norn_decisions::DecisionBinding;
use norn_expr::{EvaluationBudget, Value};
use norn_ir::{ObjectTypeDef, RuleRoot};
use norn_kernel::cell_host::JsonValue;
use norn_kernel::{
    AdmittedRule, DecisionTurn, FactKey, FactSlice, FactState, FactVersion, LogicalTime,
    TypeCatalog, World, evaluate_rule, prepare_rule_decision,
};

fn root() -> RuleRoot {
    let fixture: serde_json::Value = serde_json::from_str(include_str!(
        "../../norn-decisions/tests/zen_corpus/expression_node.json"
    ))
    .unwrap();
    serde_json::from_value(fixture["rule"].clone()).unwrap()
}

fn admit(root: &RuleRoot, environment: &str) -> AdmittedRule {
    AdmittedRule::admit(
        root,
        &BTreeMap::new(),
        DecisionBinding {
            profile_id: "norn-zen/6".to_owned(),
            environment_hash: environment.to_owned(),
        },
        None,
    )
    .unwrap()
}

#[test]
fn forged_rule_identity_or_definition_pin_is_refused() {
    let root = root();
    let rule = admit(&root, "environment-a");
    let evaluated = evaluate_rule(
        &rule,
        &rule
            .decision()
            .validate_slice(Value::object([("amount", Value::from(8))]))
            .unwrap(),
        &rule
            .decision()
            .type_event(Value::object([("kind", Value::from("submitted"))]))
            .unwrap(),
        EvaluationBudget::default(),
    )
    .unwrap();
    let turn = DecisionTurn {
        id: "rule-turn".to_owned(),
        now: LogicalTime::from_millis(1),
        world: World::Main,
        consumes: None,
    };
    let mut renamed = root.clone();
    renamed.rule_id = "another.rule".to_owned();
    let other_id = admit(&renamed, "environment-a");
    let other_binding = admit(&root, "environment-b");
    let mut changed = root.clone();
    if let norn_ir::jdm::JdmNode::ExpressionNode { name, .. } =
        &mut changed.graph.as_mut().unwrap().nodes[1]
    {
        *name = "changed definition".to_owned();
    }
    let other_definition = admit(&changed, "environment-a");
    for registered in [
        None,
        Some(&other_id),
        Some(&other_binding),
        Some(&other_definition),
    ] {
        let catalog = TypeCatalog::default().with_rules(registered);
        let refusal = prepare_rule_decision::<Infallible>(
            &catalog,
            &FactSlice::new(),
            &turn,
            &evaluated,
            &BTreeSet::new(),
        )
        .unwrap_err();
        assert_eq!(refusal.code(), "decision.invalid_output");
        assert!(
            refusal
                .to_string()
                .contains("rule identity and definition pin")
        );
    }
    let types: Vec<ObjectTypeDef> = serde_json::from_value(serde_json::json!([{
        "name": "demo.Order", "properties": [{"id": "half", "type": {"kind": "decimal", "scale": 0}}]
    }]))
    .unwrap();
    let slice = FactSlice::from([(
        FactKey::new("demo.Order", "o1"),
        FactState {
            version: FactVersion(1),
            values: Some(BTreeMap::from([("half".to_owned(), JsonValue::from(0))])),
        },
    )]);
    let catalog = TypeCatalog::new(&types).with_rules([&rule]);
    let prepared =
        prepare_rule_decision::<Infallible>(&catalog, &slice, &turn, &evaluated, &BTreeSet::new())
            .unwrap();
    assert_eq!(prepared.writes.len(), 1);
}
