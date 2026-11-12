#![allow(
    clippy::expect_used,
    clippy::indexing_slicing,
    missing_docs,
    reason = "integration-test crate"
)]

//! METRIC-ADDITIVITY-REF-1: a `ref` declares additivity at its use site. A
//! flow quantity sharing a value type with a stock derives a sum over its own
//! time property; the stock derives only its as-of snapshot
//! (METRIC-SNAPSHOT-ASOF-1). The law's three refusals are checked
//! over every pair of declared classes (exhaustive over the four non-ratio
//! classes, which is the whole finite space the refinement rule ranges over).
//! METRIC-UNIT-PARTITION-1 is checked over every free/grouped/eq/ne choice for
//! each of a quantity's two unit sources, and for an amount's currency source.

use std::collections::BTreeSet;

use norn_checker::metric::derive::derive_metric_space;
use norn_checker::object_types::codes as object_codes;
use norn_checker::property_types::codes as property_codes;
use norn_checker::{EntityArtifactSetContext, check_artifact_set_entity_model};
use norn_ir::{ArtifactEnvelope, DiagnosticSeverity, LawRoot, QuestionIR};
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
fn a_flow_quantity_derives_its_sum_and_a_stock_of_the_same_type_only_its_snapshot() {
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
                matches!(
                    candidate.definition.norn.additivity,
                    norn_ir::metric::MetricAdditivity::SemiAdditive { .. }
                ),
            )
        })
        .collect();
    assert_eq!(
        derived,
        BTreeSet::from([
            (
                "demo.Delivery".to_owned(),
                "quantity".to_owned(),
                "occurred_at".to_owned(),
                false
            ),
            (
                "demo.Holding".to_owned(),
                "quantity".to_owned(),
                "as_of".to_owned(),
                true
            ),
        ])
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

// ----- METRIC-UNIT-PARTITION-1 ---------------------------------------------------

/// A flow whose unit is stored beside it, in `unit` or (for money) `currency`.
fn flow(quantity: &Value, required: &Value) -> Vec<ArtifactEnvelope> {
    let mut artifacts = model(
        "undeclared",
        quantity,
        &reference("demo.quantity", "undeclared"),
    );
    let mut event: Value = serde_json::to_value(&artifacts[1]).expect("wire");
    event["payload"]["interface"]["properties"][1]["type"] = required.clone();
    artifacts[1] = serde_json::from_value(event).expect("envelope");
    let mut delivery: Value = serde_json::to_value(&artifacts[2]).expect("wire");
    let properties = delivery["payload"]["objectType"]["properties"]
        .as_array_mut()
        .expect("properties");
    properties.extend([
        json!({"id": "unit", "type": {"kind": "ref", "name": "demo.code"}, "constraints": {"nullable": true}}),
        json!({"id": "currency", "type": {"kind": "ref", "name": "demo.code"}, "constraints": {"nullable": true}}),
        json!({"id": "weight", "type": {"kind": "decimal", "scale": 3}}),
    ]);
    artifacts[2] = serde_json::from_value(delivery).expect("envelope");
    artifacts
}

fn partitioned(sources: &[&str]) -> Value {
    json!({"kind": "ref", "name": "demo.quantity", "additivity": {"kind": "additive"}, "unitSource": sources})
}

fn space(artifacts: &[ArtifactEnvelope]) -> norn_checker::metric::derive::MetricSpace {
    derive_metric_space(
        artifacts,
        &BTreeSet::new(),
        &LawRoot::Entity("demo.Delivery".to_owned()),
    )
    .expect("derived space")
}

/// A question over the derived Delivery quantity sum: weekly, plus each
/// source path either free, grouped, `eq`-filtered or `ne`-filtered.
fn question(space: &norn_checker::metric::derive::MetricSpace, shape: [&str; 2]) -> QuestionIR {
    let metric = space
        .candidates()
        .iter()
        .find(|candidate| candidate.definition.type_params.expr.as_deref() == Some("quantity"))
        .expect("quantity sum");
    let mut dimensions = vec![json!({"path": {"property": "occurred_at"}, "grain": "week"})];
    let mut filters = Vec::new();
    for (source, how) in ["unit", "currency"].into_iter().zip(shape) {
        let value = if source == "unit" {
            json!("g")
        } else {
            Value::Null
        };
        match how {
            "grouped" => dimensions.push(json!({"path": {"property": source}})),
            "eq" | "ne" => {
                filters
                    .push(json!({"path": {"property": source}, "operator": how, "value": value}));
            }
            _ => {}
        }
    }
    serde_json::from_value(json!({
        "irVersion": "0.1", "name": "demo.delivered", "objectSet": "demo.Delivery",
        "aggregate": [{"metric": metric.definition.name}], "groupBy": dimensions, "filter": filters,
        "window": {"start": "2026-07-06T00:00:00Z", "end": "2026-10-26T00:00:00Z"},
        "freshness": {"kind": "live", "maxAgeMs": 5000},
    }))
    .expect("question")
}

#[test]
fn a_quantity_with_a_stored_unit_aggregates_only_with_every_source_grouped_or_fixed() {
    let artifacts = flow(
        &partitioned(&["unit", "currency"]),
        &reference("demo.quantity", "undeclared"),
    );
    assert_eq!(errors(&artifacts), BTreeSet::new());
    let space = space(&artifacts);
    let metric = space
        .candidates()
        .iter()
        .find(|candidate| candidate.definition.type_params.expr.as_deref() == Some("quantity"))
        .expect("quantity sum");
    let paths: Vec<_> = metric
        .unit_partition
        .iter()
        .map(|path| path.property.as_str())
        .collect();
    assert_eq!(paths, ["unit", "currency"]);
    let catalog = norn_checker::EffectiveCatalog::from_artifacts(&artifacts);
    let ways = ["free", "grouped", "eq", "ne"];
    for unit in ways {
        for currency in ways {
            let question = question(&space, [unit, currency]);
            // A hand-written question admits only with every source grouped or fixed.
            let refused = norn_checker::metric::compute::admit(
                std::slice::from_ref(&metric.definition),
                &question,
                &catalog,
                100,
            )
            .err()
            .unwrap_or_default()
            .iter()
            .any(|finding| finding.code.as_str() == norn_checker::metric::codes::MIXED_UNIT);
            let partitioned = [unit, currency]
                .iter()
                .all(|how| matches!(*how, "grouped" | "eq"));
            assert_eq!(refused, !partitioned, "unit {unit}, currency {currency}");
            // A derived question groups by every source it leaves free.
            let plan = space.admit(&question, 100).expect("derived question");
            for (source, how) in ["unit", "currency"].into_iter().zip([unit, currency]) {
                let grouped = plan
                    .question()
                    .dimensions
                    .iter()
                    .any(|dimension| dimension.path.property.as_str() == source);
                assert_eq!(grouped, how != "eq", "{source} {how}");
            }
        }
    }
}

/// METRIC-CURRENCY-PARTITION and METRIC-CURRENCY-SINGLE: an amount whose
/// currency lives in a code property aggregates per currency, as a unit source
/// does; a single-code currency needs no partition.
#[test]
fn an_amount_aggregates_per_its_stored_currency_unless_it_has_one_code() {
    for codes in [&["USD", "EUR"][..], &["USD"][..]] {
        let values: Vec<_> = codes.iter().map(|code| json!({"value": code})).collect();
        let payment = json!({"entityVersion": "2.0", "objectType": {
            "name": "demo.Payment", "links": [],
            "properties": [
                {"id": "paid_at", "type": {"kind": "timestamp", "timezone": "utc"}},
                {"id": "amount", "type": {"kind": "decimal", "scale": 2, "additivity": {"kind": "additive"}, "currencySource": {"property": "currency"}}},
                {"id": "currency", "type": {"kind": "enum", "values": values}},
            ],
        }});
        let artifacts = vec![envelope("demo.Payment.entity", &payment)];
        assert_eq!(errors(&artifacts), BTreeSet::new());
        let space = derive_metric_space(
            &artifacts,
            &BTreeSet::new(),
            &LawRoot::Entity("demo.Payment".to_owned()),
        )
        .expect("derived space");
        let metric = space
            .candidates()
            .iter()
            .find(|candidate| candidate.definition.type_params.expr.as_deref() == Some("amount"))
            .expect("amount sum");
        let single = codes.len() == 1;
        let paths: Vec<_> = metric
            .unit_partition
            .iter()
            .map(|path| path.property.as_str())
            .collect();
        assert_eq!(paths, if single { vec![] } else { vec!["currency"] });
        let catalog = norn_checker::EffectiveCatalog::from_artifacts(&artifacts);
        for how in ["free", "grouped", "eq", "ne"] {
            let mut dimensions = vec![json!({"path": {"property": "paid_at"}, "grain": "week"})];
            let mut filters = Vec::new();
            match how {
                "grouped" => dimensions.push(json!({"path": {"property": "currency"}})),
                "eq" | "ne" => filters.push(
                    json!({"path": {"property": "currency"}, "operator": how, "value": "USD"}),
                ),
                _ => {}
            }
            let question: QuestionIR = serde_json::from_value(json!({
                "irVersion": "0.1", "name": "demo.paid", "objectSet": "demo.Payment",
                "aggregate": [{"metric": metric.definition.name}], "groupBy": dimensions, "filter": filters,
                "window": {"start": "2026-07-06T00:00:00Z", "end": "2026-10-26T00:00:00Z"},
                "freshness": {"kind": "live", "maxAgeMs": 5000},
            }))
            .expect("question");
            // The exact engine admits the sum once the currency is partitioned.
            let findings = norn_checker::metric::compute::admit(
                std::slice::from_ref(&metric.definition),
                &question,
                &catalog,
                100,
            )
            .err()
            .unwrap_or_default();
            let mixed = !single && matches!(how, "free" | "ne");
            assert_eq!(
                findings
                    .iter()
                    .map(|finding| finding.code.as_str())
                    .collect::<Vec<_>>(),
                if mixed {
                    vec![norn_checker::metric::codes::MIXED_CURRENCY]
                } else {
                    vec![]
                },
                "{codes:?} {how}"
            );
            let plan = space.admit(&question, 100).expect("derived question");
            let grouped = plan
                .question()
                .dimensions
                .iter()
                .any(|dimension| dimension.path.property.as_str() == "currency");
            assert_eq!(
                grouped,
                how == "grouped" || (!single && how != "eq"),
                "{codes:?} {how}"
            );
        }
    }
}

#[test]
fn a_unit_source_names_other_stored_code_properties_and_members_keep_it() {
    let refused = |quantity: Value, required: Value| {
        errors(&flow(&quantity, &required)).contains(property_codes::INVALID_UNIT_SOURCE)
    };
    let free = reference("demo.quantity", "undeclared");
    assert!(refused(partitioned(&["quantity"]), free.clone()), "itself");
    assert!(refused(partitioned(&["missing"]), free.clone()), "missing");
    assert!(refused(partitioned(&["weight"]), free.clone()), "a decimal");
    assert!(
        refused(partitioned(&["unit", "unit"]), free.clone()),
        "repeated"
    );
    assert!(
        refused(
            json!({"kind": "ref", "name": "demo.code", "unitSource": ["unit"]}),
            free.clone()
        ),
        "on text"
    );
    assert!(
        refused(
            json!({"kind": "decimal", "scale": 6, "unit": {"system": "ucum", "code": "g"}, "unitSource": ["unit"]}),
            free
        ),
        "with a static unit"
    );
    // A member keeps a source its ref requirement declares, and may add one.
    let required =
        json!({"kind": "ref", "name": "demo.quantity", "unitSource": ["unit", "currency"]});
    assert!(
        errors(&flow(&reference("demo.quantity", "additive"), &required))
            .contains(object_codes::INTERFACE_PROPERTY_TYPE_MISMATCH)
    );
    assert_eq!(
        errors(&flow(&partitioned(&["unit", "currency"]), &required)),
        BTreeSet::new()
    );
}
