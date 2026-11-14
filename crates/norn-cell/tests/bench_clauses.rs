//! BENCH-RULE-2: ERP clauses as ordinary declarations through the real cell seam.
#![cfg(feature = "native")]
#![allow(
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::needless_pass_by_value,
    clippy::panic,
    clippy::unwrap_in_result,
    missing_docs,
    reason = "benchmark scenarios"
)]
#[path = "stress_purchase.rs"]
mod stress;
use norn_checker::{EntityArtifactSetContext, check_artifact_set};
use norn_ir::{ArtifactEnvelope, DiagnosticSeverity};
use norn_kernel::{ActionOutcome, TypeCatalog};
use serde_json::{Map, Value, json};
use std::collections::BTreeSet;

fn property_type(shorthand: &str) -> Value {
    let unit = json!({"system":"ucum","code":"1"});
    match shorthand {
        "text" => json!({"kind":"text"}),
        "bool" => json!({"kind":"boolean"}),
        "date" => json!({"kind":"date"}),
        "qty" => json!({"kind":"decimal","scale":0,"unit":unit}),
        "dec2" => json!({"kind":"decimal","scale":2,"unit":unit}),
        "dec3" => json!({"kind":"decimal","scale":3,"unit":unit}),
        "money" => {
            json!({"kind":"decimal","scale":2,"currencySource":{"links":[],"property":"currency"}})
        }
        other => panic!("unknown property shorthand {other}"),
    }
}

fn envelope(id: &str, kind: &str, payload: Value, dependencies: &Value) -> Value {
    json!({"artifactId":id,"dependencies":dependencies,"irVersion":"0.1","kind":kind,
        "lifecycle":"draft","name":id,"namespace":"bench","payload":payload,
        "provenance":{"source":"authoring_sdk","sourcePath":"fixtures/bench/clause-cases.json"},
        "scope":{"environmentId":"dev","orgId":"org_demo"},"version":"1.0.0"})
}

fn entity(spec: &Value, drop_keys: bool) -> Value {
    let props = spec["props"].as_object().expect("props");
    let properties: Vec<Value> = props
        .iter()
        .map(|(id, ty)| json!({"id":id,"type":property_type(ty.as_str().expect("type"))}))
        .collect();
    let keys = spec["keys"]
        .as_array()
        .cloned()
        .unwrap_or_else(|| vec![json!(["id"])]);
    let identity: Vec<Value> = keys
        .iter()
        .take(if drop_keys { 1 } else { keys.len() })
        .enumerate()
        .map(|(index, key)| json!({"name":format!("key{index}"),"properties":key}))
        .collect();
    let name = spec["name"].as_str().expect("name");
    let units =
        json!([{"artifactId":"core.units","kind":"entity","required":true,"version":"1.0.0"}]);
    let mut object = json!({"name":name,"identity":identity,"properties":properties});
    if spec["links"].is_array() {
        object["links"] = spec["links"].clone();
    }
    envelope(
        &format!("{name}.entity"),
        "entity",
        json!({"entityVersion":"2.0","objectType":object}),
        &units,
    )
}

fn action(spec: &Value, entities: &[Value], drop_pre: bool) -> Value {
    let on = spec["on"].as_str().expect("target");
    let target = entities
        .iter()
        .find(|e| e["name"] == on)
        .expect("target entity");
    let mut properties = Map::new();
    for input in spec["inputs"].as_array().expect("inputs") {
        let name = input.as_str().expect("input");
        let shorthand = target["props"][name].as_str().expect("input type");
        let ty = property_type(shorthand);
        let schema = match shorthand {
            "text" | "date" => json!({"type":"string"}),
            "bool" => json!({"type":"boolean"}),
            _ => json!({"type":"number","nornType":ty}),
        };
        properties.insert(name.to_owned(), schema);
    }
    let id = spec["id"].as_str().expect("action id");
    let mode = spec["mode"].as_str().unwrap_or("create_only");
    let writes = if mode == "upsert" {
        json!([{"change":"created","entityKind":on},{"change":"modified","entityKind":on}])
    } else {
        json!([{"change":"created","entityKind":on}])
    };
    let mut payload = json!({"actionId":id,"title":id,"idempotent":true,"ledgered":true,
        "scopeRequired":["orgId","environmentId"],
        "inputSchema":{"type":"object","additionalProperties":false,"required":spec["inputs"],"properties":properties},
        "outputSchema":{"type":"object","additionalProperties":false,"required":["id"],"properties":{"id":{"type":"string"}}},
        "effectProgram":{"kind":"upsert_input_object","objectIdField":"id","objectKind":on,
            "entityArtifactId":format!("{on}.entity"),"entityArtifactVersion":"1.0.0","writeMode":{"kind":mode}},
        "writes":writes});
    if let Some(slices) = spec["slice"].as_object() {
        let declared: Vec<Value> = slices
            .iter()
            .map(|(name, source)| {
                json!({"name":name,"objectType":source[0],"key":{"property":{"from":"input","property":source[1]}}})
            })
            .collect();
        payload["slice"] = json!(declared);
    }
    if spec["sliceRaw"].is_array() {
        payload["slice"] = spec["sliceRaw"].clone();
    }
    if spec["createLinks"].is_array() {
        payload["createLinks"] = spec["createLinks"].clone();
    }
    if !drop_pre && spec["pre"].is_array() {
        payload["preconditions"] = spec["pre"].clone();
    }
    let entity_dependency = json!({"artifactId":format!("{on}.entity"),"kind":"entity","required":true,"version":"1.0.0"});
    let units =
        json!({"artifactId":"core.units","kind":"entity","required":true,"version":"1.0.0"});
    envelope(
        &format!("{id}.action"),
        "action",
        payload,
        &json!([units, entity_dependency]),
    )
}

fn law(spec: &Value) -> Value {
    let on = spec["on"].as_str().expect("subject");
    let id = spec["id"].as_str().expect("law id");
    envelope(
        &format!("bench.{id}.law"),
        "law",
        json!({"id":id,"lawVersion":"0.1","modality":{"kind":"always"},"owner":"bench",
            "provenance":{"source":"authoring_sdk","sourcePath":"fixtures/bench/clause-cases.json"},
            "rationale":spec["text"],"response":{"kind":"predicate","expression":spec["expression"]},
            "scope":{"objectTypes":[on],"root":{"kind":"entity","id":on}},
            "severity":"error","subject":on}),
        &json!([{"artifactId":format!("{on}.entity"),"kind":"entity","required":true,"version":"1.0.0"}]),
    )
}

fn artifacts(case: &Value, remove: Option<&str>) -> Vec<ArtifactEnvelope> {
    let entities = case["entities"].as_array().expect("entities");
    let actions = case["actions"].as_array().expect("actions");
    let laws = case["laws"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|_| remove != Some("laws"))
        .map(law);
    entities
        .iter()
        .map(|e| entity(e, remove == Some("keys")))
        .chain(
            actions
                .iter()
                .map(|a| action(a, entities, remove == Some("pre"))),
        )
        .chain(laws)
        .map(|v| serde_json::from_value(v).expect("generated declaration"))
        .collect()
}

fn base() -> Vec<ArtifactEnvelope> {
    [
        include_str!("../../../registry/core/artifacts/core.Process.interface.json"),
        include_str!("../../../registry/core/artifacts/core.units.json"),
    ]
    .into_iter()
    .map(|v| serde_json::from_str(v).expect("core"))
    .collect()
}

fn diagnostics(artifacts: &[ArtifactEnvelope]) -> Vec<String> {
    let registry = BTreeSet::new();
    let base = base();
    check_artifact_set(
        artifacts,
        &EntityArtifactSetContext {
            registry: &registry,
            base: &base,
            tenant: None,
        },
    )
    .into_iter()
    .filter(|d| d.severity == DiagnosticSeverity::Error)
    .map(|d| format!("{}: {}", d.code, d.message))
    .collect()
}

/// Runs seeds then one step; returns the refusal code, if any, plus atomicity.
fn attempt(
    case: &Value,
    artifacts: &[ArtifactEnvelope],
    step: &Value,
) -> Result<(), (String, String, bool)> {
    let (mut cell, definitions, interfaces) = stress::setup(artifacts);
    let catalog = TypeCatalog::new(definitions.iter())
        .with_interfaces(&interfaces)
        .with_governing_laws(artifacts);
    for (index, seed) in case["seed"].as_array().into_iter().flatten().enumerate() {
        let action = format!("{}.action", seed[0].as_str().expect("seed action"));
        let outcome = stress::run(
            &action,
            seed[1].clone(),
            &mut cell,
            &catalog,
            artifacts,
            &format!("seed-{index}"),
        );
        assert!(
            matches!(outcome, Ok(ActionOutcome::Committed { .. })),
            "{} seed: {:?}",
            case["id"],
            outcome.err().map(|e| format!("{e:?}"))
        );
    }
    let before = cell.store().read().dump().expect("before");
    let action = format!("{}.action", step[0].as_str().expect("step action"));
    match stress::run(
        &action,
        step[1].clone(),
        &mut cell,
        &catalog,
        artifacts,
        "step",
    ) {
        Ok(ActionOutcome::Committed { .. }) => Ok(()),
        Ok(_) => panic!("{}: uncommitted outcome", case["id"]),
        Err(refusal) => {
            let atomic = before == cell.store().read().dump().expect("after");
            Err((refusal.code().to_owned(), format!("{refusal:?}"), atomic))
        }
    }
}

#[test]
fn encoded_clauses_commit_refuse_atomically_and_fail_open_when_removed() {
    let corpus: Value =
        serde_json::from_str(include_str!("../../../fixtures/bench/clause-cases.json"))
            .expect("cases");
    let mut failures = Vec::new();
    for case in corpus["cases"].as_array().expect("cases") {
        let id = case["id"].as_str().expect("id");
        let retained = artifacts(case, None);
        let refused = diagnostics(&retained);
        if !refused.is_empty() {
            println!(
                "BENCH-CLAUSE {}",
                json!({"id":id,"admitted":false,"diagnostics":refused})
            );
            failures.push(id.to_owned());
            continue;
        }
        let valid = attempt(case, &retained, &case["valid"]);
        let invalid = attempt(case, &retained, &case["invalid"]);
        let removed = artifacts(case, case["remove"].as_str());
        let control = attempt(case, &removed, &case["invalid"]);
        let passed = valid.is_ok() && matches!(&invalid, Err((_, _, true))) && control.is_ok();
        println!(
            "BENCH-CLAUSE {}",
            json!({"id":id,"admitted":true,"seam":case["seam"],"valid":valid.is_ok(),
            "refusal":invalid.as_ref().err().map(|e| json!({"code":e.0,"message":e.1,"atomic":e.2})),
            "control_commits":control.is_ok(),"control_error":control.err().map(|e| e.1),"valid_error":valid.err().map(|e| e.1)})
        );
        if !passed {
            failures.push(id.to_owned());
        }
    }
    assert_eq!(
        failures.len(),
        0,
        "clauses without complete controls: {failures:?}"
    );
}

#[test]
fn cross_object_law_probes_report_their_exact_refusal() {
    let corpus: Value =
        serde_json::from_str(include_str!("../../../fixtures/bench/clause-cases.json"))
            .expect("cases");
    let entity_spec = json!({"name":"bench.StockMove","props":{"id":"text","value":"qty"}});
    let target = entity(&entity_spec, false);
    for probe in corpus["unsupported_probes"].as_array().expect("probes") {
        let set: Vec<ArtifactEnvelope> = [target.clone(), law(probe)]
            .into_iter()
            .map(|v| serde_json::from_value(v).expect("declaration"))
            .collect();
        let refused = diagnostics(&set);
        println!(
            "BENCH-PROBE {}",
            json!({"id":probe["id"],"expression":probe["expression"],"diagnostics":refused})
        );
        assert_ne!(refused.len(), 0, "{} unexpectedly admitted", probe["id"]);
    }
}
