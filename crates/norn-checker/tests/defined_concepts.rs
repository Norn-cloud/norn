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
fn forward_dependencies_are_acyclic_and_do_not_enable_execution() {
    let a = declaration("test.a", "1.0.0", &[("test.b", "1.0.0")]);
    let b = declaration("test.b", "1.0.0", &[]);
    assert!(cycles(&[a.clone(), b.clone()], &[]).is_empty());
    assert!(cycles(&[b, a.clone()], &[]).is_empty());
    assert!(
        norn_checker::check_artifact(&a)
            .iter()
            .any(|d| d.code.as_str() == "core.graph.incomplete_classification")
    );
}
#[test]
fn self_and_retained_classification_cycles_refuse() {
    let a = declaration("test.a", "1.0.0", &[("test.a", "1.0.0")]);
    assert_eq!(cycles(&[a], &[]), ["test.a"]);
    let a = declaration("test.a", "1.0.0", &[("test.b", "1.0.0")]);
    let b = declaration("test.b", "1.0.0", &[("test.a", "1.0.0")]);
    assert_eq!(cycles(&[a.clone()], &[b.clone()]), ["test.a"]);
    assert_eq!(cycles(&[b, a], &[]), ["test.a"]);
}
#[test]
fn retained_versions_and_ambiguous_identities_never_select_a_cycle_target() {
    let a = declaration("test.a", "2.0.0", &[("test.b", "1.0.0")]);
    let b = declaration("test.b", "1.0.0", &[("test.a", "1.0.0")]);
    let old = declaration("test.a", "1.0.0", &[]);
    assert!(cycles(&[a.clone(), b.clone()], &[old]).is_empty());
    let b = declaration("test.b", "1.0.0", &[("test.a", "2.0.0")]);
    assert!(cycles(&[a, b.clone(), b], &[]).is_empty());
}
proptest::proptest! {
    #[test]
    fn classification_cycle_matches_transitive_closure(bits in proptest::collection::vec(proptest::bool::ANY, 144), dag in proptest::bool::ANY) {
        let names: Vec<_> = (0..12).map(|i| format!("test.c{i}")).collect();
        let declarations: Vec<_> = (0..12).map(|i| declaration(&names[i], "1.0.0", &(0..12)
            .filter(|j| bits[i * 12 + j] && (!dag || *j < i)).map(|j| (names[j].as_str(), "1.0.0")).collect::<Vec<_>>())).collect();
        let mut reach: Vec<Vec<bool>> = (0..12).map(|i| (0..12).map(|j|
            bits[i * 12 + j] && (!dag || j < i)).collect()).collect();
        for k in 0..12 { for i in 0..12 { for j in 0..12 {
            reach[i][j] |= reach[i][k] && reach[k][j];
        } } }
        proptest::prop_assert_eq!(cycles(&declarations, &[]).is_empty(), !(0..12).any(|i| reach[i][i]));
    }
}
