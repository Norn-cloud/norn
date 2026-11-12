#![allow(
    clippy::expect_used,
    clippy::indexing_slicing,
    missing_docs,
    reason = "integration-test crate"
)]

//! METRIC-ADDITIVITY-REF-1: a `ref` declares additivity at its use site. A
//! flow quantity sharing a value type with a stock derives a sum over its own
//! time property; the stock does not. The law's three refusals are checked
//! over every pair of declared classes (exhaustive over the four non-ratio
//! classes, which is the whole finite space the refinement rule ranges over).

use std::collections::BTreeSet;

use norn_checker::metric::derive::derive_metric_space;
use norn_checker::object_types::codes as object_codes;
use norn_checker::property_types::codes as property_codes;
use norn_checker::{EntityArtifactSetContext, check_artifact_set_entity_model};
use norn_ir::{ArtifactEnvelope, DiagnosticSeverity, LawRoot};
use serde_json::{Value, json};

const CLASSES: [&str; 4] = ["undeclared", "additive", "semi_additive", "non_additive"];

fn envelope(id: &str, payload: &Value) -> ArtifactEnvelope {
    serde_json::from_value(json!({
        "irVersion": "0.1", "artifactId": id, "kind": "entity", "namespace": "demo",
        "name": id, "version": "0.1.0", "lifecycle": "active", "scope": {},
        "dependencies": [], "provenance": {"source": "system"}, "payload": payload,
    }))
    .expect("envelope")
}

/// A `ref` to `name`, with `class` at its use site unless undeclared.
fn reference(name: &str, class: &str) -> Value {
    if class == "undeclared" {
        json!({"kind": "ref", "name": name})
    } else {
        json!({"kind": "ref", "name": name, "additivity": {"kind": class}})
    }
}

/// One value type shared by a flow and a stock, as `core.quantity` is.
fn model(required: &str, delivered: &Value, held: &Value) -> Vec<ArtifactEnvelope> {
    let types = json!({"entityVersion": "2.0", "propertyTypes": [
        {"name": "demo.quantity", "type": {"kind": "decimal", "scale": 6}},
        {"name": "demo.balance", "type": {"kind": "decimal", "scale": 6, "additivity": {"kind": "semi_additive"}}},
        {"name": "demo.code", "type": {"kind": "text"}},
        {"name": "demo.at", "type": {"kind": "timestamp", "timezone": "utc"}},
    ]});
    let event = json!({"entityVersion": "2.0", "interface": {
        "name": "demo.Event", "role": "happening", "links": [],
        "properties": [
            {"id": "occurred_at", "type": {"kind": "ref", "name": "demo.at"}},
            {"id": "quantity", "type": reference("demo.quantity", required)},
        ],
    }});
    let delivery = json!({"entityVersion": "2.0", "objectType": {
        "name": "demo.Delivery", "implements": ["demo.Event"], "links": [],
        "properties": [
            {"id": "occurred_at", "type": {"kind": "ref", "name": "demo.at"}},
            {"id": "quantity", "type": delivered},
        ],
    }});
    let holding = json!({"entityVersion": "2.0", "objectType": {
        "name": "demo.Holding", "links": [],
        "properties": [
            {"id": "as_of", "type": {"kind": "ref", "name": "demo.at"}},
            {"id": "quantity", "type": held},
        ],
    }});
    vec![
        envelope("demo.value_types.entity", &types),
        envelope("demo.Event.interface", &event),
        envelope("demo.Delivery.entity", &delivery),
        envelope("demo.Holding.entity", &holding),
    ]
}

fn errors(artifacts: &[ArtifactEnvelope]) -> BTreeSet<String> {
    check_artifact_set_entity_model(
        artifacts,
        &EntityArtifactSetContext {
            registry: &BTreeSet::new(),
            base: &[],
            tenant: None,
        },
    )
    .into_iter()
    .filter(|finding| finding.severity == DiagnosticSeverity::Error)
    .map(|finding| finding.code.as_str().to_owned())
    .collect()
}

fn admitted() -> Vec<ArtifactEnvelope> {
    model(
        "undeclared",
        &reference("demo.quantity", "additive"),
        &reference("demo.quantity", "semi_additive"),
    )
}

#[test]
fn a_flow_quantity_derives_its_sum_and_a_stock_of_the_same_type_does_not() {
    let artifacts = admitted();
    assert_eq!(errors(&artifacts), BTreeSet::new());
    let space = derive_metric_space(
        &artifacts,
        &BTreeSet::new(),
        &LawRoot::Entity("demo.Delivery".to_owned()),
    )
    .expect("derived space");
    let derived: BTreeSet<_> = space
        .candidates()
        .iter()
        .map(|candidate| {
            let params = &candidate.definition.type_params;
            (
                params
                    .metric_aggregation_params
                    .as_ref()
                    .expect("simple sum")
                    .semantic_model
                    .as_str()
                    .to_owned(),
                params.expr.clone().expect("measure"),
                candidate
                    .definition
                    .norn
                    .time_dimension
                    .as_ref()
                    .expect("time grain")
                    .property
                    .as_str()
                    .to_owned(),
            )
        })
        .collect();
    assert_eq!(
        derived,
        BTreeSet::from([(
            "demo.Delivery".to_owned(),
            "quantity".to_owned(),
            "occurred_at".to_owned()
        )])
    );
}

#[test]
fn a_member_refines_an_undeclared_ref_requirement_and_keeps_a_declared_one() {
    for required in CLASSES {
        for member in CLASSES {
            let artifacts = model(
                required,
                &reference("demo.quantity", member),
                &reference("demo.quantity", "undeclared"),
            );
            let refused = errors(&artifacts).contains(object_codes::ADDITIVITY_NOT_REFINED);
            let refines = required == "undeclared" || required == member;
            assert_eq!(refused, !refines, "requirement {required}, member {member}");
        }
    }
}

#[test]
fn use_site_additivity_needs_a_numeric_type_and_never_contradicts_its_declaration() {
    for class in &CLASSES[1..] {
        let text = model(
            "undeclared",
            &reference("demo.quantity", "undeclared"),
            &reference("demo.code", class),
        );
        assert!(
            errors(&text).contains(property_codes::REF_ADDITIVITY_NOT_NUMERIC),
            "{class} on text"
        );
        let balance = model(
            "undeclared",
            &reference("demo.quantity", "undeclared"),
            &reference("demo.balance", class),
        );
        assert_eq!(
            errors(&balance).contains(property_codes::REF_ADDITIVITY_CONFLICT),
            *class != "semi_additive",
            "{class} on a semi-additive type"
        );
    }
}

#[test]
fn an_inline_requirement_still_needs_the_same_additivity() {
    let mut artifacts = admitted();
    let mut event: Value = serde_json::to_value(&artifacts[1]).expect("wire");
    event["payload"]["interface"]["properties"][1]["type"] = json!({"kind": "decimal", "scale": 6});
    artifacts[1] = serde_json::from_value(event).expect("envelope");
    assert!(errors(&artifacts).contains(object_codes::INTERFACE_PROPERTY_TYPE_MISMATCH));
}
