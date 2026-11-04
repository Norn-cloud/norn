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

fn inherited_equation_mapping(
    preserves: bool,
    identity: bool,
) -> (Vec<ArtifactEnvelope>, Vec<ArtifactEnvelope>, PathMapping) {
    let entity = |name: &str, extends: Option<&str>, constrained: bool| -> ArtifactEnvelope {
        let mut value = json!({
            "irVersion": "0.1", "artifactId": format!("{name}.entity"), "kind": "entity",
            "namespace": name.split('.').next().unwrap(), "name": name.split('.').nth(1).unwrap(),
            "version": "0.1.0", "lifecycle": "draft",
            "scope": {"orgId": "org_demo", "environmentId": "dev"}, "dependencies": [],
            "provenance": {"source": "authoring_sdk", "sourcePath": "paths.ts"},
            "payload": {"entityVersion": "2.0", "objectType": {
                "name": name,
                "links": if extends.is_some() || name.ends_with(".End") { vec![] } else {
                    ["f", "g"].map(|id| json!({"id": id, "target": format!("{}.{}", name.split('.').next().unwrap(), if identity { "Parent" } else { "End" }),
                        "cardinality": {"to": "one", "from": "many", "required": false}})).to_vec()
                }}, "extensions": {"norn.path-equations/1.0.0": if constrained {
                    vec![json!({"id": "fg", "left": if identity { vec![] } else { vec!["g"] }, "right": ["f"]})]
                } else { vec![] }}}
        });
        if let Some(parent) = extends {
            value["payload"]["objectType"]["extends"] = json!(parent);
        }
        serde_json::from_value(value).unwrap()
    };
    let source = vec![
        entity("s.Parent", None, true),
        entity(
            "s.Child",
            identity.then_some("s.Parent"),
            preserves && !identity,
        ),
        entity("s.End", None, false),
    ];
    let target = vec![
        entity("t.Parent", None, true),
        entity("t.Child", Some("t.Parent"), false),
        entity("t.End", None, false),
    ];
    let mapping = serde_json::from_value(json!({
        "id": "inherited", "objects": {"t.Parent": "s.Parent", "t.Child": "s.Child", "t.End": "s.End"},
        "links": (["Parent", "Child"].into_iter().flat_map(|name| ["f", "g"].map(|link|
            json!({"concept": format!("t.{name}"), "link": link, "path": [link]}))).collect::<Vec<_>>())
    })).unwrap();
    (source, target, mapping)
}

#[test]
fn inherited_target_equation_refuses_unconstrained_subtype_image() {
    let (source, target, mapping) = inherited_equation_mapping(false, false);
    let findings = admit_path_mapping(
        &source,
        &target,
        &mapping,
        &BTreeSet::new(),
        ChaseBudget::default(),
    )
    .unwrap_err();
    assert!(
        findings.iter().any(|finding| {
            finding.code.as_str() == "consistency.schema.unpreserved_constraint"
                && finding.message.contains("t.Parent.entity:path-equation:fg")
                && finding.message.contains("t.Child")
        }),
        "{findings:?}"
    );
}

#[test]
fn inherited_target_equation_admits_preserving_subtype_image() {
    for identity in [false, true] {
        let (source, target, mapping) = inherited_equation_mapping(true, identity);
        admit_path_mapping(
            &source,
            &target,
            &mapping,
            &BTreeSet::new(),
            ChaseBudget::default(),
        )
        .unwrap();
    }
}

#[test]
fn inherited_equation_ids_do_not_collide_with_declarations() {
    let (source, mut target, mapping) = inherited_equation_mapping(true, false);
    let mut value = serde_json::to_value(&target[0]).unwrap();
    value["payload"]["extensions"]["norn.path-equations/1.0.0"] = json!([
        {"id": "e", "left": ["g"], "right": ["f"]},
        {"id": "e:t.Child", "left": ["g"], "right": ["f"]},
        {"id": "e:t.Child:inherited", "left": ["g"], "right": ["f"]}
    ]);
    target[0] = serde_json::from_value(value).unwrap();
    let theory =
        norn_checker::consistency::lower_path_equations(&target, &BTreeSet::new()).unwrap();
    theory.validate().unwrap();
    assert!(
        theory
            .dependencies
            .iter()
            .any(|rule| rule.id() == "t.Parent.entity:path-equation:e:t.Child")
    );
    admit_path_mapping(
        &source,
        &target,
        &mapping,
        &BTreeSet::new(),
        ChaseBudget::default(),
    )
    .unwrap();
}

#[test]
fn source_link_target_cannot_reuse_a_target_concept() {
    let mut source = serde_json::to_value(catalog("s")).unwrap();
    source[0]["payload"]["objectType"]["links"]
        .as_array_mut()
        .unwrap()
        .push(json!({
            "id": "overlap", "target": "t.Node", "cardinality": {"to": "one", "from": "many"}
        }));
    let source = serde_json::from_value::<Vec<ArtifactEnvelope>>(source).unwrap();
    let mapping = serde_json::from_value(json!({
        "id": "overlap", "objects": {"t.Node": "s.Node"},
        "links": [{"concept": "t.Node", "link": "next", "path": ["next"]}]
    }))
    .unwrap();
    let findings = admit_path_mapping(
        &source,
        &catalog("t"),
        &mapping,
        &BTreeSet::new(),
        ChaseBudget::default(),
    )
    .unwrap_err();
    assert!(findings.iter().all(|finding| matches!(
        finding.code.as_str(),
        PATH_MAPPING_INVALID | "consistency.path_equation.invalid"
    )));
}
