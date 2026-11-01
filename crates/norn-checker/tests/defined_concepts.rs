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
    assert!(cycles(&[a.clone(), b.clone()], &[]).is_empty());
    assert!(cycles(&[b, a.clone()], &[]).is_empty());
    assert!(norn_checker::check_artifact(&a).is_empty());
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
    assert!(cycles(&[a.clone(), b], &[old]).is_empty());
    let b = declaration("test.b", "1.0.0", &[("test.a", "2.0.0")]);
    assert!(cycles(&[a, b.clone(), b], &[]).is_empty());
}
proptest::proptest! {
    #[test]
    fn classification_cycle_matches_transitive_closure(bits in proptest::collection::vec(proptest::bool::ANY, 144), dag in proptest::bool::ANY) {
        let names: Vec<_> = (0..12).map(|i| format!("test.c{i}")).collect();
        let mut reach: Vec<Vec<bool>> = bits.chunks(12).enumerate().map(|(i, row)|
            row.iter().enumerate().map(|(j, edge)| *edge && (!dag || j < i)).collect()).collect();
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
    assert!(findings(std::slice::from_ref(&class), std::slice::from_ref(&base)).is_empty());
    let mut newer = serde_json::to_value(&base).expect("wire");
    newer["version"] = json!("2.0.0");
    newer["payload"]["objectType"]["properties"][0]["type"] = json!({"kind":"text"});
    assert!(
        findings(
            std::slice::from_ref(&class),
            &[
                base.clone(),
                serde_json::from_value(newer).expect("new version")
            ]
        )
        .is_empty()
    );
    let mut with_member = serde_json::to_value(&class).expect("wire");
    with_member["payload"]["dependencies"] = json!([{"artifactId":"test.Item.entity", "version":"1.0.0",
        "member":{"kind":"property", "typeName":"test.Item", "property":"amount"}}]);
    assert!(
        findings(
            &[serde_json::from_value(with_member).expect("member")],
            std::slice::from_ref(&base)
        )
        .is_empty()
    );
    assert!(findings(&[base, class], &[]).is_empty());
    let mut interface = serde_json::to_value(item()).expect("wire");
    let definition = interface["payload"]
        .as_object_mut()
        .expect("object")
        .remove("objectType")
        .expect("type");
    interface["payload"]["interface"] = definition;
    assert!(
        findings(
            &[qualified()],
            &[serde_json::from_value(interface).expect("interface")]
        )
        .is_empty()
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
    assert!(!findings(&[qualified()], &[]).is_empty());
    let base = item();
    assert!(!findings(&[qualified()], &[base.clone(), base]).is_empty());
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
        let field = wire["payload"]["bounds"].pointer_mut(paths[slot]).expect("profile field");
        *field = json!(field.as_u64().expect("integer cap").saturating_add(extra));
        let class = serde_json::from_value(wire).expect("wire remains typed");
        proptest::prop_assert!(findings(&[class], &[item()]).iter()
            .any(|d| d.code.as_str() == "core.graph.incomplete_classification"));
    }
}
