//! PATH-MAP-1: declared mutation, composition and cost controls.
#![allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    reason = "integration-test crate"
)]

use std::collections::BTreeSet;

use norn_checker::consistency::{
    ChaseBudget, PATH_MAPPING_INVALID, admit_path_mapping, codes, compose_path_mappings,
};
use norn_ir::ArtifactEnvelope;
use norn_ir::entity::PathMapping;
use serde_json::{Value, json};

const CONCEPTS: [&str; 4] = ["Delivery", "OrderLine", "Holding", "Party"];

fn link(id: &str, target: &str) -> Value {
    json!({"id": id, "target": target, "cardinality": {"to": "one", "from": "many", "required": true}})
}

fn catalog(ns: &str) -> Vec<ArtifactEnvelope> {
    let t = |name: &str| format!("{ns}.{name}");
    let mut delivery = vec![
        link("provider", &t("Party")),
        link("receiver", &t("Party")),
        link("resource", &t("Holding")),
        link("fulfils", &t("OrderLine")),
    ];
    if ns == "s" {
        delivery.push(link("unjoined", &t("OrderLine")));
    }
    let equations = json!([
        {"id": "provider", "left": ["fulfils", "provider"], "right": ["provider"]},
        {"id": "receiver", "left": ["fulfils", "receiver"], "right": ["receiver"]},
        {"id": "resource", "left": ["fulfils", "resource"], "right": ["resource"]},
        {"id": "owner", "left": ["resource", "owner"], "right": ["provider"]}
    ]);
    let links = [
        json!(delivery),
        json!([
            link("provider", &t("Party")),
            link("receiver", &t("Party")),
            link("resource", &t("Holding"))
        ]),
        json!([link("owner", &t("Party"))]),
        json!([]),
    ];
    CONCEPTS
        .iter()
        .zip(links)
        .map(|(name, links)| {
            let mut payload =
                json!({"entityVersion": "2.0", "objectType": {"name": t(name), "links": links}});
            if *name == "Delivery" {
                payload["extensions"] = json!({"norn.path-equations/1.0.0": equations});
            }
            serde_json::from_value(json!({
                "irVersion": "0.1", "artifactId": format!("{}.entity", t(name)), "kind": "entity",
                "namespace": ns, "name": name, "version": "0.1.0", "lifecycle": "draft",
                "scope": {"orgId": "org_demo", "environmentId": "dev"}, "dependencies": [],
                "provenance": {"source": "authoring_sdk", "sourcePath": "paths.ts"},
                "payload": payload
            }))
            .unwrap()
        })
        .collect()
}

const LINKS: [(&str, &[&str]); 3] = [
    ("Delivery", &["provider", "receiver", "resource", "fulfils"]),
    ("OrderLine", &["provider", "receiver", "resource"]),
    ("Holding", &["owner"]),
];

fn mapping(id: &str, from: &str, to: &str, edit: &[(&str, &[&str])]) -> PathMapping {
    let mut links = Vec::new();
    for (concept, ids) in LINKS {
        for link in ids {
            let edited: Vec<_> = edit
                .iter()
                .filter(|(l, _)| concept == "Delivery" && l == link)
                .collect();
            let paths: Vec<Vec<&str>> = if edited.is_empty() {
                vec![vec![*link]]
            } else {
                edited.iter().map(|(_, path)| path.to_vec()).collect()
            };
            for path in paths {
                links.push(
                    json!({"concept": format!("{from}.{concept}"), "link": link, "path": path}),
                );
            }
        }
    }
    let objects: serde_json::Map<_, _> = CONCEPTS
        .iter()
        .map(|c| (format!("{from}.{c}"), json!(format!("{to}.{c}"))))
        .collect();
    serde_json::from_value(json!({"id": id, "objects": objects, "links": links})).unwrap()
}

fn admit(mapping: &PathMapping) -> Result<usize, Vec<norn_checker::Diagnostic>> {
    admit_path_mapping(
        &catalog("s"),
        &catalog("t"),
        mapping,
        &BTreeSet::new(),
        ChaseBudget::default(),
    )
    .map(|evidence| evidence.obligations)
}

fn unpreserved(mapping: &PathMapping) -> BTreeSet<String> {
    let findings = admit(mapping).unwrap_err();
    assert!(
        findings
            .iter()
            .all(|d| d.code.as_str() == codes::UNPRESERVED),
        "{findings:?}"
    );
    findings
        .iter()
        .filter_map(|d| {
            d.message
                .split_whitespace()
                .nth(6)
                .map(|id| id.trim_end_matches(':').to_owned())
        })
        .collect()
}

const SWAPPED: &[(&str, &[&str])] = &[("provider", &["receiver"]), ("receiver", &["provider"])];

#[test]
fn identity_and_equivalent_path_controls_are_admitted() {
    assert_eq!(admit(&mapping("identity", "t", "s", &[])), Ok(12));
    let equivalent = mapping(
        "equivalent",
        "t",
        "s",
        &[("provider", &["fulfils", "provider"])],
    );
    assert_eq!(admit(&equivalent), Ok(12));
}

fn equations(ids: &[&str]) -> BTreeSet<String> {
    ids.iter()
        .map(|id| format!("t.Delivery.entity:path-equation:{id}"))
        .collect()
}

#[test]
fn swapped_roles_break_the_role_equations() {
    let broken = unpreserved(&mapping("swapped", "t", "s", SWAPPED));
    assert_eq!(broken, equations(&["provider", "owner"]));
}

#[test]
fn a_dropped_join_breaks_the_fulfilment_equations() {
    let broken = unpreserved(&mapping("dropped", "t", "s", &[("fulfils", &["unjoined"])]));
    assert_eq!(broken, equations(&["provider"]));
}

#[test]
fn a_non_functional_image_breaks_the_target_link_contract() {
    let both: &[(&str, &[&str])] = &[("provider", &["provider"]), ("provider", &["receiver"])];
    let broken = unpreserved(&mapping("both", "t", "s", both));
    assert!(
        broken.contains("target:t.Delivery.provider:functional"),
        "{broken:?}"
    );
}

#[test]
fn a_bad_component_breaks_its_composite_and_identity_composes() {
    let identity = mapping("identity", "t", "s", &[]);
    let middle = catalog("s");
    let compose = |then: &PathMapping| {
        compose_path_mappings(&identity, then, &middle, &BTreeSet::new()).unwrap()
    };
    let good = compose(&mapping("endo", "s", "s", &[]));
    assert_eq!(good.links, identity.links);
    assert_eq!(admit(&good), Ok(12));
    let bad = compose(&mapping("swap", "s", "s", SWAPPED));
    assert!(unpreserved(&bad).contains("t.Delivery.entity:path-equation:provider"));
    let both: &[(&str, &[&str])] = &[("provider", &["provider"]), ("provider", &["receiver"])];
    let refused = compose_path_mappings(
        &identity,
        &mapping("both", "s", "s", both),
        &middle,
        &BTreeSet::new(),
    );
    assert_eq!(refused.unwrap_err()[0].code.as_str(), PATH_MAPPING_INVALID);
    let mut missing = identity.clone();
    missing.objects.remove("t.Delivery");
    missing.links[0].path.clear();
    let refused = compose_path_mappings(
        &missing,
        &mapping("endo", "s", "s", &[]),
        &middle,
        &BTreeSet::new(),
    );
    assert_eq!(refused.unwrap_err()[0].code.as_str(), PATH_MAPPING_INVALID);
    let mut many = serde_json::to_value(&middle).unwrap();
    many[0]["payload"]["objectType"]["links"][0]["cardinality"]["to"] = json!("many");
    let many: Vec<ArtifactEnvelope> = serde_json::from_value(many).unwrap();
    let refused = compose_path_mappings(
        &identity,
        &mapping("endo", "s", "s", &[]),
        &many,
        &BTreeSet::new(),
    );
    assert_eq!(refused.unwrap_err()[0].code.as_str(), PATH_MAPPING_INVALID);
}

#[test]
fn ill_typed_mappings_refuse_before_search() {
    for edit in [
        &[("provider", &["resource"][..])][..],
        &[("provider", &["missing"][..])][..],
        &[("provider", &["fulfils"][..])][..],
    ] {
        let findings = admit(&mapping("bad", "t", "s", edit)).unwrap_err();
        assert_eq!(findings.len(), 1);
        assert_eq!(
            findings[0].code.as_str(),
            PATH_MAPPING_INVALID,
            "{findings:?}"
        );
    }
    let overlapping = admit_path_mapping(
        &catalog("t"),
        &catalog("t"),
        &mapping("same", "t", "t", &[]),
        &BTreeSet::new(),
        ChaseBudget::default(),
    );
    assert_eq!(
        overlapping.unwrap_err()[0].code.as_str(),
        PATH_MAPPING_INVALID
    );
}

#[test]
fn preservation_cost_on_the_574_fixtures() {
    let middle = catalog("s");
    let identity = mapping("identity", "t", "s", &[]);
    let composite = compose_path_mappings(
        &identity,
        &mapping("swap", "s", "s", SWAPPED),
        &middle,
        &BTreeSet::new(),
    )
    .unwrap();
    let cases = [
        mapping("identity", "t", "s", &[]),
        mapping(
            "equivalent",
            "t",
            "s",
            &[("provider", &["fulfils", "provider"])],
        ),
        mapping("swapped", "t", "s", SWAPPED),
        mapping("dropped", "t", "s", &[("fulfils", &["unjoined"])]),
        mapping(
            "both",
            "t",
            "s",
            &[("provider", &["provider"]), ("provider", &["receiver"])],
        ),
        composite,
    ];
    let source = catalog("s");
    let target = catalog("t");
    let registry = BTreeSet::new();
    for case in &cases {
        let start = std::time::Instant::now();
        for _ in 0..50 {
            let admitted =
                admit_path_mapping(&source, &target, case, &registry, ChaseBudget::default())
                    .is_ok();
            assert_eq!(
                admitted,
                ["identity", "equivalent"].contains(&case.id.as_str())
            );
        }
        eprintln!(
            "PATH-MAP-COST {} {:?}/admission",
            case.id,
            start.elapsed() / 50
        );
    }
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
            "id": "overlap", "target": "t.Delivery", "cardinality": {"to": "one", "from": "many"}
        }));
    let source = serde_json::from_value::<Vec<ArtifactEnvelope>>(source).unwrap();
    let mapping = mapping("overlap", "t", "s", &[]);
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

#[test]
fn composition_substitutes_each_step_and_refuses_unmapped_intermediates() {
    let first = mapping("first", "t", "s", &[("provider", &["fulfils", "provider"])]);
    let mut then = mapping("then", "s", "s", &[]);
    for image in &mut then.links {
        if image.concept == "s.Delivery" && image.link == "fulfils" {
            image.path = vec!["unjoined".into()];
        }
        if image.concept == "s.OrderLine" && image.link == "provider" {
            image.path = vec!["receiver".into()];
        }
    }
    let composed = compose_path_mappings(&first, &then, &catalog("s"), &BTreeSet::new()).unwrap();
    let provider = composed
        .links
        .iter()
        .find(|image| image.concept == "t.Delivery" && image.link == "provider")
        .unwrap();
    assert_eq!(provider.path, vec!["unjoined", "receiver"]);
    let mut first = first;
    first.objects.remove("t.OrderLine");
    first.links.retain(|image| image.concept != "t.OrderLine");
    then.objects.remove("s.OrderLine");
    let findings =
        compose_path_mappings(&first, &then, &catalog("s"), &BTreeSet::new()).unwrap_err();
    assert_eq!(findings[0].code.as_str(), PATH_MAPPING_INVALID);
    assert!(findings[0].message.contains("s.OrderLine is not mapped"));
}

// Exercise the runtime's admission entry point with an otherwise valid import.
fn runtime_set(paths: &[PathMapping]) -> Vec<ArtifactEnvelope> {
    let envelope = |id: &str, kind: &str, payload: Value| -> ArtifactEnvelope {
        serde_json::from_value(json!({
            "irVersion":"0.1", "artifactId":id, "kind":kind, "namespace":"erpnext",
            "name":id, "version":"0.1.0", "lifecycle":"draft",
            "scope":{"orgId":"org_demo","environmentId":"dev"}, "dependencies":[],
            "provenance":{"source":"import"}, "payload":payload
        }))
        .unwrap()
    };
    let read = |path: &std::path::Path| {
        serde_json::from_str::<Value>(&std::fs::read_to_string(path).unwrap()).unwrap()
    };
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut set: Vec<ArtifactEnvelope> =
        std::fs::read_dir(root.join("tests/fixtures/mapping/model"))
            .unwrap()
            .map(|e| serde_json::from_value(read(&e.unwrap().path())).unwrap())
            .collect();
    let source = envelope(
        "erpnext.source_schema",
        "source_schema",
        read(&root.join("../norn-ir/tests/fixtures/source_schema/frappe.json")),
    );
    let mut payload = read(&root.join("../norn-ir/tests/fixtures/mapping/erpnext.mapping.json"));
    payload["source"]["sha256"] = json!(
        norn_checker::receipt::ArtifactRef::of(&source)
            .unwrap()
            .sha256
    );
    let sources = catalog("s");
    let targets = catalog("t");
    payload["pathMappings"] = json!(paths.iter().map(|p| json!({
        "sourceEntities": sources.iter().map(|a| &a.artifact_id).collect::<Vec<_>>(),
        "targetEntities": targets.iter().map(|a| &a.artifact_id).collect::<Vec<_>>(), "mapping": p
    })).collect::<Vec<_>>());
    set.extend(sources);
    set.extend(targets);
    set.push(source);
    let mut import = envelope("erpnext.mapping", "mapping", payload);
    import.dependencies = set
        .iter()
        .map(|a| norn_ir::ArtifactDependency {
            artifact_id: a.artifact_id.clone(),
            kind: a.kind(),
            version: Some(a.version.clone()),
            required: true,
        })
        .collect();
    set.push(import);
    set
}

fn runtime_check(set: &[ArtifactEnvelope]) -> Vec<norn_checker::Diagnostic> {
    norn_checker::check_artifact_set(
        set,
        &norn_checker::EntityArtifactSetContext {
            registry: &BTreeSet::new(),
            base: &[],
            tenant: None,
        },
    )
}

#[test]
fn runtime_admission_enforces_required_paths_and_budget() {
    let identity = mapping("identity", "t", "s", &[]);
    let good = runtime_set(std::slice::from_ref(&identity));
    let findings = runtime_check(&good);
    assert!(
        !findings.iter().any(norn_checker::Diagnostic::is_error),
        "{findings:?}"
    );
    let evidence = findings
        .iter()
        .find(|d| d.code == "consistency.path_mapping.preserved")
        .unwrap();
    assert!(evidence.message.contains("bounded-no-counterexample"));
    assert!(
        evidence.message.contains(
            &norn_checker::receipt::ArtifactRef::of(good.last().unwrap())
                .unwrap()
                .sha256
        )
    );
    let composite = compose_path_mappings(
        &identity,
        &mapping("swap", "s", "s", SWAPPED),
        &catalog("s"),
        &BTreeSet::new(),
    )
    .unwrap();
    for broken in [
        mapping("swapped", "t", "s", SWAPPED),
        mapping("dropped", "t", "s", &[("fulfils", &["unjoined"])]),
        mapping(
            "both",
            "t",
            "s",
            &[("provider", &["provider"]), ("provider", &["receiver"])],
        ),
        composite,
    ] {
        let findings = runtime_check(&runtime_set(&[broken]));
        assert!(
            findings.iter().any(|d| d.is_error()
                && d.code == codes::UNPRESERVED
                && d.message.contains("breaks")),
            "{findings:?}"
        );
    }
    let exhausted = runtime_check(&runtime_set(&vec![identity; 100]));
    assert!(
        exhausted.iter().any(|d| d.is_error()
            && d.code == codes::CHASE_REFUSED
            && d.message.contains("whole-admission")),
        "{exhausted:?}"
    );
    assert!(runtime_check(&runtime_set(&[])).is_empty());
}

#[test]
fn runtime_admission_cost_before_and_after() {
    let baseline = runtime_set(&[]);
    let enforced = runtime_set(&[mapping("identity", "t", "s", &[])]);
    for (label, set) in [("without-paths", baseline), ("with-paths", enforced)] {
        let start = std::time::Instant::now();
        for _ in 0..20 {
            assert!(
                !runtime_check(&set)
                    .iter()
                    .any(norn_checker::Diagnostic::is_error)
            );
        }
        eprintln!(
            "RUNTIME-PATH-COST {label} {:?}/admission",
            start.elapsed() / 20
        );
    }
}

fn edit_artifact(set: &mut [ArtifactEnvelope], id: &str, edit: impl FnOnce(&mut Value)) {
    let artifact = set.iter_mut().find(|a| a.artifact_id == id).unwrap();
    let mut value = serde_json::to_value(&*artifact).unwrap();
    edit(&mut value);
    *artifact = serde_json::from_value(value).unwrap();
}

fn constrain(set: &mut [ArtifactEnvelope], id: &str) {
    edit_artifact(set, id, |v| {
        v["payload"]["extensions"] = json!({"norn.path-equations/1.0.0": [
            {"id": "identity", "left": [], "right": []}
        ]});
    });
}

#[test]
fn runtime_requires_bindings_for_actual_mapped_targets() {
    let mut set = runtime_set(&[]);
    assert!(
        !runtime_check(&set)
            .iter()
            .any(norn_checker::Diagnostic::is_error)
    );
    constrain(&mut set, "party.Customer.entity");
    let findings = runtime_check(&set);
    assert!(
        findings.iter().any(|d| d.is_error()
            && d.code == "consistency.path_mapping.missing_binding"
            && d.message
                .contains("party.Customer.entity (equation identity)")),
        "{findings:?}"
    );

    // Cover Customer successfully but omit a second constrained mapped target.
    edit_artifact(&mut set, "erpnext.mapping", |v| {
        v["payload"]["pathMappings"] = json!([{
            "sourceEntities":["s.Party.entity"],
            "targetEntities":["party.Customer.entity", "core.Party.interface"],
            "mapping":{"id":"customer", "objects":{"party.Customer":"s.Party"}, "links":[]}
        }]);
    });
    let covered = runtime_check(&set);
    assert!(!covered.iter().any(norn_checker::Diagnostic::is_error), "{covered:?}");
    constrain(&mut set, "commerce.SalesOrder.entity");
    let partial = runtime_check(&set);
    let missing: Vec<_> = partial.iter().filter(|d| d.code == "consistency.path_mapping.missing_binding").collect();
    assert_eq!(missing.len(), 1, "{partial:?}");
    assert!(missing[0].message.contains("commerce.SalesOrder.entity (equation identity)"));

}

#[test]
fn runtime_requires_inherited_only_binding() {
    let mut set = runtime_set(&[]);
    let mut parent = serde_json::to_value(
        set.iter()
            .find(|a| a.artifact_id == "party.Customer.entity")
            .unwrap(),
    )
    .unwrap();
    parent["artifactId"] = json!("party.Parent.entity");
    parent["name"] = json!("Parent");
    parent["payload"] = json!({"entityVersion":"2.0", "objectType":{"name":"party.Parent"}});
    set.push(serde_json::from_value(parent).unwrap());
    constrain(&mut set, "party.Parent.entity");
    edit_artifact(&mut set, "party.Customer.entity", |v| {
        v["payload"]["objectType"]["extends"] = json!("party.Parent");
        v["dependencies"] = json!([{"artifactId":"party.Parent.entity", "kind":"entity", "version":"0.1.0", "required":true}]);
    });
    let findings = runtime_check(&set);
    assert!(
        findings.iter().any(|d| d.is_error()
            && d.code == "consistency.path_mapping.missing_binding"
            && d.message
                .contains("party.Customer.entity (equation identity)")),
        "{findings:?}"
    );
    assert!(
        findings
            .iter()
            .filter(|d| d.is_error())
            .all(|d| d.code == "consistency.path_mapping.missing_binding"),
        "{findings:?}"
    );
}

#[test]
fn runtime_keeps_evidence_for_distinct_catalog_bindings() {
    let mut set = runtime_set(&[mapping("first", "t", "s", &[])]);
    let sources = catalog("u");
    let targets = catalog("v");
    edit_artifact(&mut set, "erpnext.mapping", |v| {
        v["payload"]["pathMappings"]
            .as_array_mut()
            .unwrap()
            .push(json!({
                "sourceEntities":sources.iter().map(|a| &a.artifact_id).collect::<Vec<_>>(),
                "targetEntities":targets.iter().map(|a| &a.artifact_id).collect::<Vec<_>>(),
                "mapping":mapping("second", "v", "u", &[])
            }));
        for a in sources.iter().chain(&targets) {
            v["dependencies"].as_array_mut().unwrap().push(json!({"artifactId":a.artifact_id,"kind":"entity","version":a.version,"required":true}));
        }
    });
    set.extend(sources);
    set.extend(targets);
    let findings = runtime_check(&set);
    assert!(
        !findings.iter().any(norn_checker::Diagnostic::is_error),
        "{findings:?}"
    );
    let evidence: Vec<_> = findings
        .iter()
        .filter(|d| d.code == "consistency.path_mapping.preserved")
        .collect();
    assert_eq!(evidence.len(), 2, "{findings:?}");
    for (index, ns) in ["t", "v"].iter().enumerate() {
        let d = evidence
            .iter()
            .find(|d| d.span.as_ref().unwrap().pointer == format!("/payload/pathMappings/{index}"))
            .unwrap();
        let declaration = set
            .iter()
            .find(|a| a.artifact_id == format!("{ns}.Delivery.entity"))
            .unwrap();
        assert!(
            d.message.contains(
                &norn_checker::receipt::ArtifactRef::of(declaration)
                    .unwrap()
                    .sha256
            )
        );
    }
}
