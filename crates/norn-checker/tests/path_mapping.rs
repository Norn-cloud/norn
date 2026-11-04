//! PATH-MAP-1: preservation bridge controls; the next cut adds #574 mutations.
#![allow(clippy::unwrap_used, clippy::indexing_slicing, reason = "integration-test crate")]
use std::collections::BTreeSet;
use norn_checker::consistency::{admit_path_mapping, ChaseBudget, PATH_MAPPING_INVALID};
use norn_ir::{ArtifactEnvelope, entity::PathMapping};
use serde_json::json;

fn catalog(ns: &str) -> Vec<ArtifactEnvelope> {
    vec![serde_json::from_value(json!({
        "irVersion": "0.1", "artifactId": format!("{ns}.Node.entity"), "kind": "entity",
        "namespace": ns, "name": "Node", "version": "0.1.0", "lifecycle": "draft",
        "scope": {"orgId": "org_demo", "environmentId": "dev"}, "dependencies": [],
        "provenance": {"source": "authoring_sdk", "sourcePath": "paths.ts"},
        "payload": {"entityVersion": "2.0", "objectType": {"name": format!("{ns}.Node"),
            "links": [{"id": "next", "target": format!("{ns}.Node"),
                "cardinality": {"to": "one", "from": "many", "required": false}}]},
            "extensions": {"norn.path-equations/1.0.0": [
                {"id": "next", "left": ["next"], "right": ["next"]}]}}
    })).unwrap()]
}

#[test]
fn preservation_admits_identity_and_refuses_an_unknown_image() {
    let mut mapping: PathMapping = serde_json::from_value(json!({
        "id": "identity", "objects": {"t.Node": "s.Node"},
        "links": [{"concept": "t.Node", "link": "next", "path": ["next"]}]
    })).unwrap();
    let source = catalog("s");
    let target = catalog("t");
    let check = |mapping: &PathMapping| admit_path_mapping(
        &source, &target, mapping, &BTreeSet::new(), ChaseBudget::default());
    assert_eq!(check(&mapping).unwrap().obligations, 2);
    mapping.links[0].path = vec!["missing".into()];
    assert_eq!(check(&mapping).unwrap_err()[0].code.as_str(), PATH_MAPPING_INVALID);
}
