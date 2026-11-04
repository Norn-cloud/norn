//! BENCH-RULE-1: measure selected pointwise clauses through the STRESS cell seam.
#![cfg(feature = "native")]
#![allow(
    clippy::expect_used,
    clippy::panic,
    missing_docs,
    reason = "benchmark scenarios"
)]
#[path = "stress_purchase.rs"]
mod stress;
use norn_checker::{EntityArtifactSetContext, check_artifact_set};
use norn_ir::{ArtifactEnvelope, DiagnosticSeverity};
use norn_kernel::{ActionOutcome, TypeCatalog};
use serde_json::{Value, json};
use std::collections::BTreeSet;

fn set(value: &mut Value, pointer: &str, replacement: Value) {
    *value.pointer_mut(pointer).expect("existing template field") = replacement;
}

fn declarations(rule: &Value) -> Vec<ArtifactEnvelope> {
    let baseline: Vec<Value> =
        serde_json::from_str(include_str!("../../../fixtures/stress/purchase.json"))
            .expect("STRESS declarations");
    let find = |id: &str| {
        baseline
            .iter()
            .find(|a| a["artifactId"] == id)
            .expect("template")
            .clone()
    };
    let mut entity = find("stress.Purchase.entity");
    entity
        .pointer_mut("/payload/objectType")
        .expect("object type")
        .as_object_mut()
        .expect("entity")
        .remove("lifecycles");
    let numeric = json!({"kind":"decimal","scale":0,"unit":{"system":"ucum","code":"1"}});
    let fields = [
        "ordered",
        "received",
        "accepted",
        "rejected",
        "delivered",
        "returned",
        "billed",
    ];
    let mut properties = vec![json!({"id":"id","type":{"kind":"text"}})];
    let mut schema = json!({"id":{"type":"string"}});
    for field in fields {
        properties.push(json!({"id":field,"type":numeric,"default":"0"}));
        schema
            .as_object_mut()
            .expect("input properties")
            .insert(field.into(), json!({"type":"number","nornType":numeric}));
    }
    set(
        &mut entity,
        "/payload/objectType/properties",
        json!(properties),
    );
    let mut action = find("stress.purchase.create.action");
    set(&mut action, "/payload/inputSchema/properties", schema);
    set(&mut action, "/payload/inputSchema/required", json!(["id"]));
    let mut law = find("stress.purchase_positive.law");
    set(
        &mut law,
        "/artifactId",
        json!(format!("bench.{}.law", rule["id"].as_str().expect("id"))),
    );
    set(&mut law, "/payload/id", rule["id"].clone());
    set(&mut law, "/payload/rationale", rule["requirement"].clone());
    set(
        &mut law,
        "/payload/response/expression",
        rule["expression"].clone(),
    );
    [entity, action, law]
        .into_iter()
        .map(|v| serde_json::from_value(v).expect("LawIR corpus"))
        .collect()
}

#[test]
fn selected_rules_admit_commit_and_refuse_with_atomicity_and_controls() {
    let corpus: Value =
        serde_json::from_str(include_str!("../../../fixtures/bench/erp-rules.json"))
            .expect("rules");
    let rules = corpus["rules"].as_array().expect("rules array");
    assert_eq!(rules.len(), 30);
    let base: Vec<ArtifactEnvelope> = [
        include_str!("../../../registry/core/artifacts/core.Process.interface.json"),
        include_str!("../../../registry/core/artifacts/core.units.json"),
    ]
    .into_iter()
    .map(|v| serde_json::from_str(v).expect("core"))
    .collect();
    let registry = BTreeSet::new();
    let mut tested = 0;
    for rule in rules {
        if rule["classification"] != "runtime_candidate" {
            assert!(
                !rule["reason"]
                    .as_str()
                    .expect("unsupported reason")
                    .is_empty()
            );
            continue;
        }
        let artifacts = declarations(rule);
        let diagnostics = check_artifact_set(
            &artifacts,
            &EntityArtifactSetContext {
                registry: &registry,
                base: &base,
                tenant: None,
            },
        );
        assert!(
            !diagnostics
                .iter()
                .any(|d| d.severity == DiagnosticSeverity::Error),
            "{}: {diagnostics:?}",
            rule["id"]
        );
        for (sample, accepted) in [("valid", true), ("invalid", false)] {
            for retained in [true, false] {
                let mut artifacts = artifacts.clone();
                if !retained {
                    artifacts.pop();
                }
                let (mut cell, definitions, interfaces) = stress::setup(&artifacts);
                let catalog = TypeCatalog::new(definitions.iter())
                    .with_interfaces(&interfaces)
                    .with_governing_laws(&artifacts);
                let mut input = rule[sample].clone();
                input
                    .as_object_mut()
                    .expect("sample input")
                    .insert("id".into(), json!("sample"));
                let before = cell.store().read().dump().expect("before");
                let result = stress::run(
                    "stress.purchase.create.action",
                    input,
                    &mut cell,
                    &catalog,
                    &artifacts,
                    "sample",
                );
                if accepted || !retained {
                    assert!(
                        matches!(
                            result.expect("commit/control"),
                            ActionOutcome::Committed { .. }
                        ),
                        "{}",
                        rule["id"]
                    );
                } else {
                    let refusal = result.expect_err("violating clause must refuse");
                    assert_eq!(
                        refusal.code(),
                        "core.laws.violated",
                        "{}: {refusal}",
                        rule["id"]
                    );
                    assert!(
                        refusal
                            .to_string()
                            .contains(rule["id"].as_str().expect("id"))
                    );
                    assert_eq!(before, cell.store().read().dump().expect("after"));
                }
            }
        }
        tested += 1;
    }
    assert_eq!(tested, 10);
    println!(
        "BENCH-RULE-1: {tested}/30 selected pointwise clauses exercised; 20/30 not yet encoded (not engine refusals); no bounded or inductive proof claimed"
    );
}
