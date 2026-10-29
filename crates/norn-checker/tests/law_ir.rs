#![allow(
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::pedantic,
    missing_docs,
    reason = "law contract regression tests"
)]
use norn_checker::{
    EntityArtifactSetContext, check_artifact_set,
    verification::{codes, conservative_closure},
};
use norn_graph::Budget;
use norn_ir::ArtifactEnvelope;
use serde_json::{Value, json};
use std::collections::BTreeSet;
fn entities() -> Vec<ArtifactEnvelope> {
    serde_json::from_str(include_str!("../../../fixtures/law-ir/entities.json")).expect("entities")
}
fn laws() -> Vec<Value> {
    serde_json::from_str(include_str!("../../../fixtures/law-ir/laws.json")).expect("laws")
}
fn run(values: Vec<Value>, base: &[ArtifactEnvelope]) -> Vec<String> {
    let pool: Vec<ArtifactEnvelope> = values
        .into_iter()
        .map(|v| serde_json::from_value(v).expect("envelope"))
        .collect();
    check_artifact_set(
        &pool,
        &EntityArtifactSetContext {
            registry: &BTreeSet::new(),
            base,
            tenant: None,
        },
    )
    .into_iter()
    .map(|d| d.code.as_str().to_owned())
    .collect()
}
#[test]
fn erpnext_laws_admit_through_the_shared_set_checker() {
    let entity_findings = check_artifact_set(
        &entities(),
        &EntityArtifactSetContext {
            registry: &BTreeSet::new(),
            base: &[],
            tenant: None,
        },
    );
    assert!(entity_findings.is_empty(), "{entity_findings:?}");
    assert_eq!(run(laws(), &entities()), Vec::<String>::new());
}
#[test]
fn law_types_and_absent_bindings_fail_closed() {
    for expr in [
        "self.missing == 1",
        "self.amount",
        "self.amount == 'wrong'",
        "self.amount == 9007199254740993.00000000000000000000000000001",
    ] {
        let mut law = laws()[1].clone();
        law["payload"]["response"]["expression"] = json!(expr);
        assert!(
            run(vec![law], &entities()).contains(&codes::PREDICATE_TYPE.to_owned()),
            "{expr}"
        );
    }
    let mut law = laws()[0].clone();
    law["dependencies"] = json!([]);
    assert!(run(vec![law], &entities()).contains(&codes::UNKNOWN_BINDING.to_owned()));
    let mut law = laws()[0].clone();
    law["payload"]["modality"] = json!({"kind":"next"});
    assert!(run(vec![law], &entities()).contains(&codes::INVALID_DOCUMENT.to_owned()));
}
#[test]
fn unsupported_temporal_combinations_are_stable_errors() {
    let mut law = laws()[0].clone();
    law["payload"]["timing"] = json!({"kind":"within","millis":"1"});
    assert!(run(vec![law], &entities()).contains(&codes::UNSUPPORTED_TEMPORAL.to_owned()));
    for millis in ["0", "01", "-1", "1.0", "18446744073709551616"] {
        let mut law = laws()[2].clone();
        law["payload"]["timing"]["millis"] = json!(millis);
        assert!(run(vec![law], &entities()).contains(&codes::INVALID_DOCUMENT.to_owned()));
    }
}
#[test]
fn existing_laws_survive_tightening_and_cannot_be_replaced() {
    let parent: ArtifactEnvelope = serde_json::from_value(laws()[0].clone()).expect("parent");
    let mut base = entities();
    base.push(parent);
    let mut child = laws()[0].clone();
    child["artifactId"] = json!("erpnext.child.law");
    child["payload"]["modality"] = json!({"kind":"always"});
    assert!(run(vec![child.clone()], &base).contains(&codes::WEAKENS_INHERITED.to_owned()));
    child["payload"]["id"] = json!("erpnext.additional_safety");
    assert!(run(vec![child], &base).is_empty());
}
#[test]
fn whole_set_closure_includes_hidden_new_writers_and_refuses_missing_dependencies() {
    let law: ArtifactEnvelope = serde_json::from_value(laws()[0].clone()).expect("law");
    let mut pool = entities();
    pool.push(law.clone());
    let budget = Budget {
        max_nodes: 100,
        max_edges: 100,
        max_steps: 1000,
    };
    let before = conservative_closure(&law, &pool, budget).expect("closure");
    let mut hidden = serde_json::to_value(&pool[0]).expect("writer envelope");
    hidden["artifactId"] = json!("erpnext.hidden.rule");
    hidden["kind"] = json!("rule");
    hidden["payload"] = json!({"ruleId":"hidden", "ruleVersion":"2.0", "graph": {}});
    pool.push(serde_json::from_value(hidden).expect("lenient envelope"));
    let after = conservative_closure(&law, &pool, budget).expect("closure");
    assert!(after.artifacts.is_superset(&before.artifacts));
    assert!(after.writers.contains("erpnext.hidden.rule"));
    assert!(after.writers.contains("erpnext.SalesOrder:status:shipped"));
    assert!(after.open_boundaries.contains("restore"));
    let mut edited = pool.clone();
    let mut value = serde_json::to_value(&edited[0]).expect("envelope");
    value["payload"]["objectType"]["description"] = json!("same-version changed meaning");
    edited[0] = serde_json::from_value(value).expect("modified envelope");
    let changed = conservative_closure(&law, &edited, budget).expect("closure");
    assert_ne!(changed.content_digests, after.content_digests);
    pool[0].dependencies.push(norn_ir::ArtifactDependency {
        artifact_id: "absent".to_owned(),
        kind: norn_ir::ArtifactKind::Entity,
        version: Some("0.1.0".to_owned()),
        required: true,
    });
    assert!(conservative_closure(&law, &pool, budget).is_err());
    assert!(
        conservative_closure(
            &law,
            &pool,
            Budget {
                max_nodes: 0,
                ..budget
            }
        )
        .is_err()
    );
}

#[test]
fn local_law_diagnostics_keep_real_json_pointer_spans() {
    let mut law = laws()[0].clone();
    law["payload"]["timing"] = json!({"kind":"within", "millis":"1"});
    let findings = norn_checker::check_artifact_json(&law);
    let diagnostic = findings
        .iter()
        .find(|d| d.code.as_str() == codes::UNSUPPORTED_TEMPORAL)
        .expect("timing diagnostic");
    assert_eq!(
        diagnostic.span.as_ref().expect("span").pointer,
        "/payload/timing"
    );
}

#[test]
fn closure_lists_inherited_transitions_on_the_actual_subtype() {
    let law: ArtifactEnvelope = serde_json::from_value(laws()[0].clone()).expect("law");
    let mut pool = entities();
    let mut child = serde_json::to_value(&pool[0]).expect("parent");
    child["artifactId"] = json!("erpnext.SpecialOrder.entity");
    child["dependencies"] = json!([{"artifactId":"erpnext.SalesOrder.entity", "kind":"entity", "version":"0.1.0", "required":true}]);
    child["payload"]["objectType"] =
        json!({"name":"erpnext.SpecialOrder", "extends":"erpnext.SalesOrder"});
    pool.push(serde_json::from_value(child).expect("subtype"));
    pool.push(law.clone());
    let closure = conservative_closure(
        &law,
        &pool,
        Budget {
            max_nodes: 100,
            max_edges: 100,
            max_steps: 1000,
        },
    )
    .expect("closure");
    assert!(
        closure
            .writers
            .contains("erpnext.SpecialOrder:status:shipped")
    );
}

#[test]
fn conditions_until_state_action_transition_and_deadline_slots_are_checked() {
    let mut law = laws()[0].clone();
    law["payload"]["condition"] = json!("self.missing == 0");
    assert!(run(vec![law], &entities()).contains(&codes::PREDICATE_TYPE.to_owned()));
    let mut law = laws()[3].clone();
    law["payload"]["modality"]["predicate"] = json!("self.status");
    assert!(run(vec![law], &entities()).contains(&codes::PREDICATE_TYPE.to_owned()));
    let mut law = laws()[0].clone();
    law["payload"]["scope"]["lifecycleStates"] =
        json!([{"objectType":"erpnext.SalesOrder", "property":"status", "states":["absent"]}]);
    assert!(run(vec![law], &entities()).contains(&codes::UNKNOWN_BINDING.to_owned()));
    let mut law = laws()[0].clone();
    law["payload"]["response"] = json!({"kind":"action", "artifactId":"absent.action"});
    assert!(run(vec![law], &entities()).contains(&codes::UNKNOWN_BINDING.to_owned()));
    let mut law = laws()[0].clone();
    law["payload"]["response"] = json!({"kind":"transition", "objectType":"erpnext.SalesOrder", "property":"status", "transitionId":"shipped"});
    assert!(run(vec![law.clone()], &entities()).is_empty());
    law["payload"]["response"]["transitionId"] = json!("absent");
    assert!(run(vec![law], &entities()).contains(&codes::UNKNOWN_BINDING.to_owned()));
    let mut law = laws()[2].clone();
    law["payload"]["timing"] = json!({"kind":"before_deadline", "property":"deadline"});
    assert!(run(vec![law.clone()], &entities()).contains(&codes::UNKNOWN_BINDING.to_owned()));
    let mut base = entities();
    let mut request = serde_json::to_value(&base[2]).expect("request");
    request["payload"]["objectType"]["properties"]
        .as_array_mut()
        .expect("properties")
        .push(json!({"id":"deadline", "type":{"kind":"timestamp", "timezone":"utc"}}));
    base[2] = serde_json::from_value(request).expect("deadline type");
    assert!(run(vec![law.clone()], &base).is_empty());
    let mut computed = serde_json::to_value(&base[2]).expect("request with deadline");
    computed["payload"]["objectType"]["properties"].as_array_mut().expect("properties").push(json!({"id":"computed_deadline", "type":{"kind":"timestamp", "timezone":"utc"}, "computed":"self.deadline"}));
    base[2] = serde_json::from_value(computed).expect("computed timestamp type");
    let entity_findings = check_artifact_set(
        &base,
        &EntityArtifactSetContext {
            registry: &BTreeSet::new(),
            base: &[],
            tenant: None,
        },
    );
    assert!(entity_findings.is_empty(), "{entity_findings:?}");
    law["payload"]["timing"]["property"] = json!("computed_deadline");
    assert!(run(vec![law], &base).contains(&codes::UNKNOWN_BINDING.to_owned()));
}
