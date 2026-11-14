#![allow(
    clippy::expect_used,
    clippy::indexing_slicing,
    missing_docs,
    reason = "integration scenarios"
)]
use norn_checker::{EntityArtifactSetContext, check_artifact_set};
use norn_ir::{ArtifactEnvelope, ResourceLimitProfile};
use serde_json::json;
use std::collections::BTreeSet;

fn declaration(id: &str, version: &str, dependencies: &[(&str, &str)]) -> ArtifactEnvelope {
    serde_json::from_value(json!({
        "irVersion":"0.1", "artifactId":id, "version":version, "kind":"defined_concept",
        "namespace":"test", "name":"Class", "lifecycle":"draft", "scope":{},
        "provenance":{"source":"system"},
        "dependencies": dependencies.iter().map(|(id, version)| json!({
            "artifactId":id, "version":version, "kind":"defined_concept", "required":true
        })).collect::<Vec<_>>(),
        "payload": {"id":id, "version":version,
            "base":{"artifactId":"test.Item.entity", "version":"1.0.0", "member":{"kind":"type", "typeName":"test.Item"}},
            "predicate":"true", "bounds":ResourceLimitProfile::norn_limits_v1(),
            "dependencies":dependencies.iter().map(|(id, version)| json!({
                "artifactId":id, "version":version, "member":{"kind":"artifact"}
            })).collect::<Vec<_>>()}
    })).expect("wire declaration")
}
fn cycles(candidates: &[ArtifactEnvelope], base: &[ArtifactEnvelope]) -> Vec<String> {
    check_artifact_set(
        candidates,
        &EntityArtifactSetContext {
            registry: &BTreeSet::new(),
            base,
            tenant: None,
        },
    )
    .into_iter()
    .filter(|finding| finding.code.as_str() == "core.graph.cycle")
    .filter_map(|finding| finding.artifact_id)
    .collect()
}
#[test]
fn forward_dependencies_are_acyclic() {
    let a = declaration("test.a", "1.0.0", &[("test.b", "1.0.0")]);
    let b = declaration("test.b", "1.0.0", &[]);
    assert_eq!(cycles(&[a.clone(), b.clone()], &[]).len(), 0);
    assert_eq!(cycles(&[b, a.clone()], &[]).len(), 0);
    assert_eq!(norn_checker::check_artifact(&a), []);
}
#[test]
fn self_and_retained_classification_cycles_refuse() {
    let a = declaration("test.a", "1.0.0", &[("test.a", "1.0.0")]);
    assert_eq!(cycles(&[a], &[]), ["test.a"]);
    let a = declaration("test.a", "1.0.0", &[("test.b", "1.0.0")]);
    let b = declaration("test.b", "1.0.0", &[("test.a", "1.0.0")]);
    assert_eq!(
        cycles(std::slice::from_ref(&a), std::slice::from_ref(&b)),
        ["test.a"]
    );
    assert_eq!(cycles(&[b, a], &[]), ["test.a"]);
}
#[test]
fn retained_versions_and_ambiguous_identities_never_select_a_cycle_target() {
    let a = declaration("test.a", "2.0.0", &[("test.b", "1.0.0")]);
    let b = declaration("test.b", "1.0.0", &[("test.a", "1.0.0")]);
    let old = declaration("test.a", "1.0.0", &[]);
    assert_eq!(cycles(&[a.clone(), b], &[old]).len(), 0);
    let b = declaration("test.b", "1.0.0", &[("test.a", "2.0.0")]);
    assert_eq!(cycles(&[a, b.clone(), b], &[]).len(), 0);
}
proptest::proptest! {
    // Sparse edges and no self-loops: with uniform bits nearly every non-DAG
    // case held a self-loop, so "back edges but still acyclic" never ran.
    #[test]
    fn classification_cycle_matches_transitive_closure(bits in proptest::collection::vec(proptest::bool::weighted(0.1), 144), dag in proptest::bool::ANY) {
        let names: Vec<_> = (0..12).map(|i| format!("test.c{i}")).collect();
        let mut reach: Vec<Vec<bool>> = bits.chunks(12).enumerate().map(|(i, row)|
            row.iter().enumerate().map(|(j, edge)| *edge && j != i && (!dag || j < i)).collect()).collect();
        let declarations: Vec<_> = reach.iter().enumerate().map(|(i, row)| declaration(&names[i], "1.0.0",
            &row.iter().enumerate().filter(|(_, edge)| **edge)
                .map(|(j, _)| (names[j].as_str(), "1.0.0")).collect::<Vec<_>>())).collect();
        for k in 0..12 { for i in 0..12 { for j in 0..12 {
            reach[i][j] |= reach[i][k] && reach[k][j];
        } } }
        proptest::prop_assert_eq!(cycles(&declarations, &[]).is_empty(), !(0..12).any(|i| reach[i][i]));
    }
}

fn item() -> ArtifactEnvelope {
    serde_json::from_value(json!({
        "irVersion":"0.1", "artifactId":"test.Item.entity", "version":"1.0.0",
        "kind":"entity", "namespace":"test", "name":"Item", "lifecycle":"draft",
        "scope":{}, "provenance":{"source":"system"}, "dependencies":[],
        "payload":{"entityVersion":"2.0", "objectType":{"name":"test.Item",
            "properties":[{"id":"amount", "type":{"kind":"integer"}}]}}
    }))
    .expect("base entity")
}
fn qualified() -> ArtifactEnvelope {
    let mut wire = serde_json::to_value(declaration("test.HighValue", "1.0.0", &[])).expect("wire");
    wire["dependencies"] = json!([{"artifactId":"test.Item.entity", "version":"1.0.0",
        "kind":"entity", "required":true}]);
    wire["payload"]["predicate"] = json!("self.amount > 100");
    serde_json::from_value(wire).expect("qualified declaration")
}
fn findings(
    candidates: &[ArtifactEnvelope],
    base: &[ArtifactEnvelope],
) -> Vec<norn_checker::Diagnostic> {
    check_artifact_set(
        candidates,
        &EntityArtifactSetContext {
            registry: &BTreeSet::new(),
            base,
            tenant: None,
        },
    )
}
#[test]
fn cross_artifact_admission_types_a_pure_boolean_over_the_exact_base() {
    let base = item();
    let class = qualified();
    assert_eq!(
        findings(std::slice::from_ref(&class), std::slice::from_ref(&base)),
        []
    );
    let mut newer = serde_json::to_value(&base).expect("wire");
    newer["version"] = json!("2.0.0");
    newer["payload"]["objectType"]["properties"][0]["type"] = json!({"kind":"text"});
    assert_eq!(
        findings(
            std::slice::from_ref(&class),
            &[
                base.clone(),
                serde_json::from_value(newer).expect("new version")
            ]
        ),
        []
    );
    let mut with_member = serde_json::to_value(&class).expect("wire");
    with_member["payload"]["dependencies"] = json!([{"artifactId":"test.Item.entity", "version":"1.0.0",
        "member":{"kind":"property", "typeName":"test.Item", "property":"amount"}}]);
    assert_eq!(
        findings(
            &[serde_json::from_value(with_member).expect("member")],
            std::slice::from_ref(&base)
        ),
        []
    );
    assert_eq!(findings(&[base, class], &[]), []);
    let mut interface = serde_json::to_value(item()).expect("wire");
    let definition = interface["payload"]
        .as_object_mut()
        .expect("object")
        .remove("objectType")
        .expect("type");
    interface["payload"]["interface"] = definition;
    assert_eq!(
        findings(
            &[qualified()],
            &[serde_json::from_value(interface).expect("interface")]
        ),
        []
    );
}
#[test]
fn missing_pins_members_types_and_unbounded_or_impure_predicates_refuse() {
    let original = serde_json::to_value(qualified()).expect("wire");
    for (pointer, value) in [
        ("/dependencies", json!([])),
        ("/dependencies/0/required", json!(false)),
        ("/dependencies/0/version", json!("2.0.0")),
        ("/dependencies/0/kind", json!("defined_concept")),
        ("/payload/version", json!("2.0.0")),
        ("/payload/base/member/typeName", json!("test.Absent")),
        ("/payload/base/member", json!({"kind":"artifact"})),
        (
            "/payload/dependencies",
            json!([{"artifactId":"test.Item.entity", "version":"1.0.0",
            "member":{"kind":"property", "typeName":"test.Item", "property":"absent"}}]),
        ),
        ("/payload/predicate", json!("self.amount")),
        ("/payload/predicate", json!("self.absent > 0")),
        ("/payload/predicate", json!("fetch('https://example.com')")),
        ("/payload/bounds/id", json!("unadmitted")),
    ] {
        let mut wire = original.clone();
        *wire.pointer_mut(pointer).expect("field") = value;
        let class = serde_json::from_value(wire).expect("wire remains typed");
        assert!(
            findings(&[class], &[item()])
                .iter()
                .any(|d| d.code.as_str() == "core.graph.incomplete_classification"),
            "{pointer}"
        );
    }
    assert_ne!(findings(&[qualified()], &[]), []);
    let base = item();
    assert_ne!(findings(&[qualified()], &[base.clone(), base]), []);
    let mut pinned = serde_json::to_value(item()).expect("wire");
    pinned["dependencies"] =
        json!([{"artifactId":"test.amounts", "version":"1.0.0", "kind":"entity", "required":true}]);
    pinned["payload"]["objectType"]["properties"][0]["type"] =
        json!({"kind":"ref", "name":"test.amount"});
    let mut catalog = pinned.clone();
    catalog["artifactId"] = json!("test.amounts");
    catalog["dependencies"] = json!([]);
    catalog["payload"] = json!({"entityVersion":"2.0", "propertyTypes":[{"name":"test.amount", "type":{"kind":"integer"}}]});
    let pinned = serde_json::from_value(pinned).expect("base");
    let catalog: ArtifactEnvelope = serde_json::from_value(catalog).expect("catalog");
    assert!(
        findings(&[qualified()], &[pinned, catalog.clone(), catalog])
            .iter()
            .any(|d| d.code.as_str() == "core.graph.incomplete_classification")
    );
}
proptest::proptest! {
    #[test]
    fn excessive_resource_bounds_never_admit(extra in 1_u64..1_000_000, slot in 0_usize..18) {
        let paths = [
            "/decide/cpu_p99_micros", "/decide/fuel_envelope_micros", "/decide/fuel_units",
            "/turn/cpu_p99_micros", "/turn/fuel_units", "/slice/max_canonical_bytes",
            "/slice/max_records", "/slice/max_relation_depth", "/slice/qualification_ceiling_bytes",
            "/memory/max_turn_working_bytes", "/memory/live_footprint_target_bytes",
            "/envelope/max_output_bytes", "/envelope/max_events", "/envelope/max_commands",
            "/envelope/max_timers", "/helper/cpu_p99_micros", "/helper/aggregate_micros_per_decision",
            "/helper/max_guest_memory_bytes",
        ];
        let mut wire = serde_json::to_value(qualified()).expect("wire");
        let mut bounds = serde_json::to_value(ResourceLimitProfile::norn_limits_v1()).expect("limits");
        let field = bounds.pointer_mut(paths[slot]).expect("profile field");
        *field = json!(field.as_u64().expect("integer cap").saturating_add(extra));
        wire["payload"]["bounds"] = bounds;
        let class = serde_json::from_value(wire).expect("wire remains typed");
        proptest::prop_assert!(findings(&[class], &[item()]).iter()
            .any(|d| d.code.as_str() == "core.graph.incomplete_classification"));
    }
}

fn classifier(wire: serde_json::Value) -> norn_checker::classification::Classifier {
    norn_checker::classification::Classifier::prepare(
        &serde_json::from_value(wire).expect("class"),
        &EntityArtifactSetContext {
            registry: &BTreeSet::new(),
            base: &[item()],
            tenant: None,
        },
    )
    .expect("admitted classifier")
}
fn row(id: &str, amount: i64) -> (String, norn_ir::RuntimeValue) {
    (
        id.into(),
        norn_ir::RuntimeValue::from_json_text(format!("{{\"amount\":{amount}}}")).expect("row"),
    )
}
#[test]
fn graph_classify_keeps_base_ids_and_membership_exit_is_a_pure_read() {
    let class = classifier(serde_json::to_value(qualified()).expect("wire"));
    let original = [row("lot-7", 101)];
    assert_eq!(
        class.classify(&original, true).expect("complete"),
        BTreeSet::from(["lot-7".into()])
    );
    assert_eq!(
        class
            .classify(&[row("lot-7", 99)], true)
            .expect("complete")
            .len(),
        0
    );
    assert_eq!(original, [row("lot-7", 101)]);
    assert_eq!(class.definitions().len(), 2);
}
#[test]
fn graph_classify_refuses_partial_missing_duplicate_and_exhausted_inputs() {
    let wire = serde_json::to_value(qualified()).expect("wire");
    let class = classifier(wire.clone());
    assert!(class.classify(&[], false).is_err());
    assert!(
        class
            .classify(
                &[(
                    "unknown".into(),
                    norn_ir::RuntimeValue::Object(std::collections::BTreeMap::new())
                )],
                true
            )
            .is_err()
    );
    assert!(
        class
            .classify(&[row("same", 101), row("same", 99)], true)
            .is_err()
    );
    for (pointer, cap) in [
        ("/payload/bounds/slice/max_records", 1),
        ("/payload/bounds/slice/max_canonical_bytes", 1),
    ] {
        let mut bounded = wire.clone();
        *bounded.pointer_mut(pointer).expect("cap") = json!(cap);
        let class = classifier(bounded);
        assert!(
            class
                .classify(&[row("a", 101), row("b", 101)], true)
                .is_err()
        );
    }
    // Admission estimates per evaluation; one runtime budget covers every row.
    let mut bounded = wire;
    bounded["payload"]["bounds"]["decide"]["fuel_units"] = json!(1000);
    let class = classifier(bounded);
    let rows: Vec<_> = (0..1000).map(|i| row(&i.to_string(), 101)).collect();
    assert!(class.classify(&rows, true).is_err());
}
proptest::proptest! {
    #[test]
    fn graph_classify_matches_recompute_over_original_ids(amounts in proptest::collection::vec(-1000_i64..1000, 0..40)) {
        let class = classifier(serde_json::to_value(qualified()).expect("wire"));
        let rows: Vec<_> = amounts.iter().enumerate().map(|(i, amount)| row(&i.to_string(), *amount)).collect();
        let expected: BTreeSet<_> = amounts.iter().enumerate().filter(|(_, amount)| **amount > 100)
            .map(|(i, _)| i.to_string()).collect();
        proptest::prop_assert_eq!(class.classify(&rows, true).expect("complete"), expected);
        proptest::prop_assert!(class.classify(&rows, false).is_err());
    }
}
