//! PATH-MAP-1: preservation bridge controls; the next cut adds #574 mutations.
#![allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    reason = "integration-test crate"
)]
use norn_checker::consistency::{ChaseBudget, PATH_MAPPING_INVALID, admit_path_mapping};
use norn_ir::{ArtifactEnvelope, entity::PathMapping};
use serde_json::json;
use std::collections::BTreeSet;

fn catalog(ns: &str) -> Vec<ArtifactEnvelope> {
    vec![
        serde_json::from_value(json!({
            "irVersion": "0.1", "artifactId": format!("{ns}.Node.entity"), "kind": "entity",
            "namespace": ns, "name": "Node", "version": "0.1.0", "lifecycle": "draft",
            "scope": {"orgId": "org_demo", "environmentId": "dev"}, "dependencies": [],
            "provenance": {"source": "authoring_sdk", "sourcePath": "paths.ts"},
            "payload": {"entityVersion": "2.0", "objectType": {"name": format!("{ns}.Node"),
                "links": [{"id": "next", "target": format!("{ns}.Node"),
                    "cardinality": {"to": "one", "from": "many", "required": false}}]},
                "extensions": {"norn.path-equations/1.0.0": [
                    {"id": "next", "left": ["next"], "right": ["next"]}]}}
        }))
        .unwrap(),
    ]
}

#[test]
fn preservation_admits_identity_and_refuses_an_unknown_image() {
    let mut mapping: PathMapping = serde_json::from_value(json!({
        "id": "identity", "objects": {"t.Node": "s.Node"},
        "links": [{"concept": "t.Node", "link": "next", "path": ["next"]}]
    }))
    .unwrap();
    let source = catalog("s");
    let target = catalog("t");
    let check = |mapping: &PathMapping| {
        admit_path_mapping(
            &source,
            &target,
            mapping,
            &BTreeSet::new(),
            ChaseBudget::default(),
        )
    };
    assert_eq!(check(&mapping).unwrap().obligations, 2);
    let mut missing = mapping.clone();
    missing.objects.clear();
    missing.links.clear();
    assert_eq!(
        check(&missing).unwrap_err()[0].code.as_str(),
        PATH_MAPPING_INVALID
    );
    missing = mapping.clone();
    missing.links.clear();
    assert_eq!(
        check(&missing).unwrap_err()[0].code.as_str(),
        PATH_MAPPING_INVALID
    );
    mapping.links[0].path = vec!["missing".into()];
    assert_eq!(
        check(&mapping).unwrap_err()[0].code.as_str(),
        PATH_MAPPING_INVALID
    );
}


fn inherited_equation_mapping(preserves: bool) -> Result<(), Vec<norn_checker::Diagnostic>> {
    let entity = |name: &str, extends: Option<&str>, constrained: bool| {
        serde_json::from_value(json!({
            "irVersion": "0.1", "artifactId": format!("{name}.entity"), "kind": "entity",
            "namespace": name.split('.').next().unwrap(), "name": name.split('.').nth(1).unwrap(),
            "version": "0.1.0", "lifecycle": "draft",
            "scope": {"orgId": "org_demo", "environmentId": "dev"}, "dependencies": [],
            "provenance": {"source": "authoring_sdk", "sourcePath": "paths.ts"},
            "payload": {"entityVersion": "2.0", "objectType": {
                "name": name, "extends": extends,
                "links": if extends.is_some() { vec![] } else {
                    ["f", "g"].map(|id| json!({"id": id, "target": "s.End",
                        "cardinality": {"to": "one", "from": "many", "required": false}})).to_vec()
                }}, "extensions": {"norn.path-equations/1.0.0": if constrained {
                    vec![json!({"id": "fg", "left": ["f"], "right": ["g"]})]
                } else { vec![] }}}
        })).unwrap()
    };
    let mut source = vec![entity("s.Parent", None, true), entity("s.Child", None, preserves)];
    let mut end = serde_json::to_value(entity("s.End", None, false)).unwrap();
    end["payload"]["objectType"]["links"] = json!([]);
    source.push(serde_json::from_value(end.clone()).unwrap());
    let mut target: Vec<ArtifactEnvelope> = vec![entity("t.Parent", None, true),
        entity("t.Child", Some("t.Parent"), false)];
    for artifact in &mut target {
        let mut value = serde_json::to_value(&*artifact).unwrap();
        for link in value["payload"]["objectType"]["links"].as_array_mut().unwrap() {
            link["target"] = json!("t.End");
        }
        *artifact = serde_json::from_value(value).unwrap();
    }
    end["artifactId"] = json!("t.End.entity");
    end["namespace"] = json!("t");
    end["payload"]["objectType"]["name"] = json!("t.End");
    target.push(serde_json::from_value(end).unwrap());
    let mapping = serde_json::from_value(json!({
        "id": "inherited", "objects": {"t.Parent": "s.Parent", "t.Child": "s.Child", "t.End": "s.End"},
        "links": ["Parent", "Child"].into_iter().flat_map(|name| ["f", "g"].map(|link|
            json!({"concept": format!("t.{name}"), "link": link, "path": [link]}))).collect::<Vec<_>>()
    })).unwrap();
    admit_path_mapping(&source, &target, &mapping, &BTreeSet::new(), ChaseBudget::default()).map(|_| ())
}

#[test]
fn inherited_target_equation_refuses_unconstrained_subtype_image() {
    let findings = inherited_equation_mapping(false).unwrap_err();
    assert!(findings.iter().any(|finding| finding.code.as_str() == "consistency.schema.unpreserved_constraint"));
}

#[test]
fn inherited_target_equation_admits_preserving_subtype_image() {
    inherited_equation_mapping(true).unwrap();
}
