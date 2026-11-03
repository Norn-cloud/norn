#![allow(
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::pedantic,
    missing_docs,
    reason = "law contract regression tests"
)]
use norn_checker::{
    EntityArtifactSetContext, check_artifact_set,
    verification::{LawWriter, codes, conservative_closure},
};
use norn_graph::Budget;
use norn_ir::{
    ArtifactEnvelope, ArtifactPayload, CompositionPackRoot, LawIR, PackArtifactKind,
    PackArtifactRef, PackInstance, PackInstanceRef,
};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};

fn projected(
    artifact: &ArtifactEnvelope,
    base: &[ArtifactEnvelope],
) -> Vec<norn_ir::verification::EffectiveLawInstance> {
    norn_checker::verification::effective_law_instances(
        std::slice::from_ref(artifact),
        &EntityArtifactSetContext {
            registry: &BTreeSet::new(),
            base,
            tenant: None,
        },
        Budget {
            max_nodes: 128,
            max_edges: 1024,
            max_steps: 4096,
        },
    )
    .expect("complete projection")
}

fn diamond_model(base: &mut Vec<ArtifactEnvelope>) {
    let template = base
        .iter()
        .find(|a| a.artifact_id == "core.Order.interface")
        .expect("contract")
        .clone();
    for name in ["Left", "Right"] {
        let mut artifact = template.clone();
        artifact.artifact_id = format!("toy.{name}.interface");
        artifact.namespace = "toy".to_owned();
        artifact.name = name.to_owned();
        let mut root: norn_ir::EntityInterfaceRootV2 = serde_json::from_value(
            serde_json::to_value(template.payload.to_json_value()).expect("wire"),
        )
        .expect("interface");
        root.interface.name = norn_ir::InterfaceName(format!("toy.{name}"));
        root.interface.extends = vec![norn_ir::InterfaceName("core.Order".to_owned())];
        root.interface.properties.clear();
        root.interface.links.clear();
        artifact.payload = norn_ir::ArtifactPayload::from_kind_json(
            &norn_ir::ArtifactKind::Entity,
            serde_json::from_value(serde_json::to_value(root).expect("wire")).expect("tree"),
        )
        .expect("root");
        base.push(artifact);
    }
    let parent = base
        .iter_mut()
        .find(|a| a.artifact_id == "commerce.SalesOrder.entity")
        .expect("implementor");
    let mut root: norn_ir::EntityRootV2 =
        serde_json::from_value(serde_json::to_value(parent.payload.to_json_value()).expect("wire"))
            .expect("root");
    root.object_type.implements = vec![
        norn_ir::InterfaceName("toy.Left".to_owned()),
        norn_ir::InterfaceName("toy.Right".to_owned()),
    ];
    parent.payload = norn_ir::ArtifactPayload::from_kind_json(
        &norn_ir::ArtifactKind::Entity,
        serde_json::from_value(serde_json::to_value(&root).expect("wire")).expect("tree"),
    )
    .expect("root");
    let mut child = parent.clone();
    child.artifact_id = "toy.Child.entity".to_owned();
    child.namespace = "toy".to_owned();
    child.name = "Child".to_owned();
    root.object_type.extends = Some(root.object_type.name.clone());
    root.object_type.name = norn_ir::ObjectTypeName("toy.Child".to_owned());
    root.object_type.implements.clear();
    root.object_type.properties.clear();
    root.object_type.links.clear();
    root.object_type.identity.clear();
    root.object_type.lifecycles.clear();
    root.object_type.invariants.clear();
    root.object_type.owned_collections.clear();
    child.payload = norn_ir::ArtifactPayload::from_kind_json(
        &norn_ir::ArtifactKind::Entity,
        serde_json::from_value(serde_json::to_value(root).expect("wire")).expect("tree"),
    )
    .expect("root");
    base.push(child);
}

#[test]
fn law_transitive_diamond_is_one_instance_per_subject_with_all_paths() {
    let (law, mut base) = interface_case(true, "self.total_amount >= 0");
    diamond_model(&mut base);
    let instances = projected(&law, &base);
    assert_eq!(instances.len(), 2);
    for instance in &instances {
        assert_eq!(instance.law.artifact_id, law.artifact_id);
        assert_eq!(
            instance.law_digest,
            norn_checker::ArtifactRef::of(&law).expect("digest").sha256
        );
        assert_eq!(instance.origin.artifact_id, "core.Order.interface");
        assert_eq!(instance.paths.len(), 2);
        assert_ne!(instance.paths[0], instance.paths[1]);
        assert!(
            instance
                .paths
                .iter()
                .all(|path| path.first() == Some(&instance.origin)
                    && path.last() == Some(&instance.subject))
        );
    }
    base.reverse();
    assert_eq!(projected(&law, &base), instances);
    let (empty, interfaces) = interface_case(false, "self.total_amount >= 0");
    assert!(projected(&empty, &interfaces).is_empty());
    assert!(
        norn_checker::verification::effective_law_instances(
            &[law],
            &EntityArtifactSetContext {
                registry: &BTreeSet::new(),
                base: &base,
                tenant: None
            },
            Budget {
                max_nodes: 1,
                max_edges: 1,
                max_steps: 1
            }
        )
        .is_err()
    );
}

proptest::proptest! {
    #![proptest_config(proptest::test_runner::Config::with_cases(16))]
    #[test]
    fn law_diamond_generated_orderings_preserve_instances(seed in proptest::collection::vec(proptest::bool::ANY, 0..16)) {
        let (law, mut base) = interface_case(true, "self.total_amount >= 0"); diamond_model(&mut base);
        let expected = projected(&law, &base);
        for reverse in seed { if reverse { base.reverse(); } else { base.rotate_left(1); } }
        proptest::prop_assert_eq!(projected(&law, &base), expected);
    }
}
fn membership_case() -> (ArtifactEnvelope, Vec<ArtifactEnvelope>) {
    let (mut law, mut base) = interface_case(true, "self.total_amount >= 0");
    let pack = |id: &str, member: &str, instances| {
        let mut artifact = law.clone();
        artifact.artifact_id = id.to_owned();
        artifact.namespace = "toy".to_owned();
        artifact.dependencies.clear();
        let root = CompositionPackRoot {
            pack_id: id.to_owned(),
            pack_version: artifact.version.clone(),
            pack_kind: None,
            provides: None,
            artifacts: vec![PackArtifactRef {
                artifact_id: member.to_owned(),
                version: "0.1.0".to_owned(),
                kind: PackArtifactKind::Entity,
                additional: BTreeMap::new(),
            }],
            instances,
            extensions: None,
        };
        artifact.payload = ArtifactPayload::from_kind_json(
            &norn_ir::ArtifactKind::BusinessPack,
            serde_json::from_value(serde_json::to_value(root).expect("pack")).expect("tree"),
        )
        .expect("payload");
        artifact
    };
    let root = pack("toy.root", "core.Order.interface", None);
    let leaf = pack("toy.leaf", "commerce.SalesOrder.entity", None);
    let reference = |id: &str, artifact: &ArtifactEnvelope| PackInstanceRef {
        instance_id: id.to_owned(),
        pack: norn_ir::mapping::InterpretationEndpoint {
            artifact_id: artifact.artifact_id.clone(),
            version: artifact.version.clone(),
            sha256: norn_checker::ArtifactRef::of(artifact).expect("pin").sha256,
        },
    };
    let refs = [
        reference("app-root", &root),
        reference("left", &root),
        reference("right", &root),
        reference("leaf", &leaf),
    ];
    let nodes = refs
        .iter()
        .enumerate()
        .map(|(index, at)| PackInstance {
            instance_id: at.instance_id.clone(),
            scope: law.scope.clone(),
            pack: at.pack.clone(),
            imports: if index == 0 {
                BTreeMap::from([
                    ("left".to_owned(), refs[1].clone()),
                    ("right".to_owned(), refs[2].clone()),
                ])
            } else {
                BTreeMap::new()
            },
            instances: if index == 1 || index == 2 {
                vec![refs[3].clone()]
            } else {
                Vec::new()
            },
        })
        .collect();
    let holder = pack("toy.instances", "core.Order.interface", Some(nodes));
    base.extend([root, leaf, holder]);
    assert!(
        check_artifact_set(
            &base,
            &EntityArtifactSetContext {
                registry: &BTreeSet::new(),
                base: &[],
                tenant: None
            }
        )
        .is_empty(),
        "retained fixture admission"
    );
    let mut declaration: LawIR =
        serde_json::from_value(serde_json::to_value(law.payload.to_json_value()).expect("law"))
            .expect("typed");
    declaration.scope.root = norn_ir::LawRoot::App("app-root".to_owned());
    law.payload = ArtifactPayload::from_kind_json(
        &norn_ir::ArtifactKind::Law,
        serde_json::from_value(serde_json::to_value(declaration).expect("law")).expect("tree"),
    )
    .expect("payload");
    (law, base)
}

#[test]
fn law_contained_shared_graph_contract_is_closed_and_scoped() {
    let (law, base) = membership_case();
    let owner = base.last().expect("holder");
    let root: CompositionPackRoot =
        serde_json::from_value(serde_json::to_value(owner.payload.to_json_value()).expect("wire"))
            .expect("pack");
    let instances = root.instances.expect("declarations");
    assert_eq!(instances.len(), 4);
    assert!(instances.iter().all(|instance| instance.scope == law.scope));
    assert_eq!(instances[0].imports.len(), 2);
}

fn membership(
    law: &ArtifactEnvelope,
    base: &[ArtifactEnvelope],
) -> Result<norn_checker::verification::RetainedLawMembership, Vec<norn_checker::Diagnostic>> {
    norn_checker::verification::retained_law_membership(
        law,
        base,
        Budget {
            max_nodes: 128,
            max_edges: 1024,
            max_steps: 4096,
        },
    )
}

#[test]
fn law_contained_retains_both_import_diamond_paths_without_global_membership() {
    let (law, base) = membership_case();
    let members = membership(&law, &base).expect("retained root");
    let order = norn_ir::definition_catalog::ArtifactKey {
        artifact_id: "commerce.SalesOrder.entity".to_owned(),
        version: "0.1.0".to_owned(),
    };
    let paths = members.get(&order).expect("contained order");
    assert_eq!(paths.len(), 2);
    assert!(
        paths
            .iter()
            .all(|path| path.first().expect("root").instance_id == "app-root"
                && path.last().expect("leaf").instance_id == "leaf")
    );
    assert_eq!(members.len(), 2);
}

#[test]
fn law_contained_unknown_stale_missing_duplicate_cycle_and_budget_refuse() {
    let (law, original) = membership_case();
    for case in 0..6 {
        let mut base = original.clone();
        let owner = base.last_mut().expect("declarations");
        let mut root: CompositionPackRoot = serde_json::from_value(
            serde_json::to_value(owner.payload.to_json_value()).expect("wire"),
        )
        .expect("root");
        let nodes = root.instances.as_mut().expect("nodes");
        match case {
            0 => {
                nodes[0].instance_id = "unknown".to_owned();
            }
            1 => {
                nodes[3].pack.sha256 = "0".repeat(64);
            }
            2 => {
                nodes.push(nodes[0].clone());
            }
            3 => {
                nodes[3].instances = vec![PackInstanceRef {
                    instance_id: nodes[0].instance_id.clone(),
                    pack: nodes[0].pack.clone(),
                }];
            }
            4 => {
                nodes[0].scope.org_id = Some("other".to_owned());
            }
            _ => {
                nodes[0].imports.clear();
                nodes[0].instances = vec![PackInstanceRef {
                    instance_id: "missing".to_owned(),
                    pack: nodes[0].pack.clone(),
                }];
            }
        }
        owner.payload = ArtifactPayload::from_kind_json(
            &norn_ir::ArtifactKind::BusinessPack,
            serde_json::from_value(serde_json::to_value(root).expect("root")).expect("tree"),
        )
        .expect("payload");
        let errors = membership(&law, &base).expect_err("incomplete membership");
        assert_eq!(errors[0].code.as_str(), codes::UNRESOLVED_SCOPE);
    }
    let mut missing = original;
    missing.retain(|a| a.artifact_id != "commerce.SalesOrder.entity");
    assert!(membership(&law, &missing).is_err());
}

#[test]
fn law_contained_pending_path_bytes_refuse_before_multiplying_large_identities() {
    let (mut law, mut base) = membership_case();
    let large_id = "r".repeat(1_600_000);
    let mut declaration: LawIR =
        serde_json::from_value(serde_json::to_value(law.payload.to_json_value()).expect("wire"))
            .expect("law");
    declaration.scope.root = norn_ir::LawRoot::App(large_id.clone());
    law.payload = ArtifactPayload::from_kind_json(
        &norn_ir::ArtifactKind::Law,
        serde_json::from_value(serde_json::to_value(declaration).expect("law")).expect("tree"),
    )
    .expect("payload");
    let owner = base.last_mut().expect("holder");
    let mut root: CompositionPackRoot =
        serde_json::from_value(serde_json::to_value(owner.payload.to_json_value()).expect("wire"))
            .expect("pack");
    root.instances.as_mut().expect("instances")[0].instance_id = large_id;
    owner.payload = ArtifactPayload::from_kind_json(
        &norn_ir::ArtifactKind::BusinessPack,
        serde_json::from_value(serde_json::to_value(root).expect("root")).expect("tree"),
    )
    .expect("payload");
    assert!(membership(&law, &base).is_err());
}

proptest::proptest! {
    #![proptest_config(proptest::test_runner::Config::with_cases(16))]
    #[test]
    fn law_contained_generated_declaration_order_preserves_membership(seed in proptest::collection::vec(proptest::bool::ANY, 0..16)) {
        let (law, mut base) = membership_case(); let expected = membership(&law, &base).expect("membership");
        for reverse in seed { if reverse { base.reverse(); } else { base.rotate_left(1); } }
        proptest::prop_assert_eq!(membership(&law, &base).expect("order independent"), expected);
    }
}

#[test]
fn law_contained_node_edge_and_step_budget_exhaustion_refuses() {
    let (law, base) = membership_case();
    for budget in [
        Budget {
            max_nodes: 1,
            max_edges: 1024,
            max_steps: 4096,
        },
        Budget {
            max_nodes: 128,
            max_edges: 1,
            max_steps: 4096,
        },
        Budget {
            max_nodes: 128,
            max_edges: 1024,
            max_steps: 1,
        },
    ] {
        let errors = norn_checker::verification::retained_law_membership(&law, &base, budget)
            .expect_err("budget exhaustion refuses");
        assert!(
            errors
                .iter()
                .any(|d| d.code.as_str() == codes::UNRESOLVED_SCOPE)
        );
    }
}

#[test]
fn law_weaken_candidate_containment_cannot_replace_retained_membership() {
    let (law, base) = membership_case();
    let mut owner = base.last().expect("holder").clone();
    let mut root: CompositionPackRoot =
        serde_json::from_value(serde_json::to_value(owner.payload.to_json_value()).expect("wire"))
            .expect("pack");
    root.instances.as_mut().expect("instances")[0]
        .imports
        .clear();
    owner.payload = ArtifactPayload::from_kind_json(
        &norn_ir::ArtifactKind::BusinessPack,
        serde_json::from_value(serde_json::to_value(root).expect("root")).expect("tree"),
    )
    .expect("payload");
    let errors = check_artifact_set(
        &[law, owner],
        &EntityArtifactSetContext {
            registry: &BTreeSet::new(),
            base: &base,
            tenant: None,
        },
    );
    assert!(
        errors
            .iter()
            .any(|d| d.code.as_str() == "artifact_set.resolution.shadows_base")
    );
}

#[test]
fn law_contained_global_subtype_outside_retained_scope_is_excluded() {
    let (law, mut base) = membership_case();
    let mut outside = base
        .iter()
        .find(|a| a.artifact_id == "commerce.SalesOrder.entity")
        .expect("order")
        .clone();
    let mut root: norn_ir::EntityRootV2 = serde_json::from_value(
        serde_json::to_value(outside.payload.to_json_value()).expect("wire"),
    )
    .expect("entity");
    root.object_type.extends = Some(root.object_type.name.clone());
    root.object_type.name = norn_ir::ObjectTypeName("outside.Order".to_owned());
    root.object_type.implements.clear();
    root.object_type.properties.clear();
    root.object_type.links.clear();
    root.object_type.identity.clear();
    root.object_type.lifecycles.clear();
    root.object_type.invariants.clear();
    root.object_type.owned_collections.clear();
    outside.artifact_id = "outside.Order.entity".to_owned();
    outside.namespace = "outside".to_owned();
    outside.payload = ArtifactPayload::from_kind_json(
        &norn_ir::ArtifactKind::Entity,
        serde_json::from_value(serde_json::to_value(root).expect("entity")).expect("tree"),
    )
    .expect("payload");
    base.push(outside);
    let instances = projected(&law, &base);
    assert_eq!(instances.len(), 1);
    assert_eq!(
        instances[0].subject.artifact_id,
        "commerce.SalesOrder.entity"
    );
}

#[test]
fn law_contained_instances_keep_origins_and_import_diamonds() {
    let (law, base) = membership_case();
    let instances = projected(&law, &base);
    assert_eq!(instances.len(), 1);
    assert_eq!(instances[0].instance_paths.len(), 2);
    assert_ne!(
        instances[0].instance_paths[0],
        instances[0].instance_paths[1]
    );
    assert_eq!(instances[0].origin.artifact_id, "core.Order.interface");
    assert_eq!(
        instances[0].subject.artifact_id,
        "commerce.SalesOrder.entity"
    );
    assert!(
        instances[0]
            .instance_paths
            .iter()
            .all(|p| p.last().expect("leaf").instance_id == "leaf")
    );
    let mut pack_law = law;
    let mut declaration: norn_ir::LawIR = serde_json::from_value(
        serde_json::to_value(pack_law.payload.to_json_value()).expect("wire"),
    )
    .expect("law");
    declaration.scope.root = norn_ir::LawRoot::Pack("app-root".to_owned());
    pack_law.payload = norn_ir::ArtifactPayload::from_kind_json(
        &norn_ir::ArtifactKind::Law,
        serde_json::from_value(serde_json::to_value(declaration).expect("law")).expect("tree"),
    )
    .expect("payload");
    assert_eq!(projected(&pack_law, &base)[0].instance_paths.len(), 2);
}

#[test]
fn law_tighten_is_conjunctive_and_weakening_or_stale_containment_refuses() {
    let (law, mut base) = membership_case();
    base.push(law.clone());
    let mut tighter = law.clone();
    tighter.artifact_id = "toy.extra_bound.law".to_owned();
    let mut declaration: norn_ir::LawIR =
        serde_json::from_value(serde_json::to_value(law.payload.to_json_value()).expect("wire"))
            .expect("law");
    declaration.id = "toy.extra_bound".to_owned();
    declaration.response = norn_ir::LawResponse::Predicate {
        expression: norn_ir::ZenExpression("self.total_amount >= 1".to_owned()),
    };
    tighter.payload = norn_ir::ArtifactPayload::from_kind_json(
        &norn_ir::ArtifactKind::Law,
        serde_json::from_value(serde_json::to_value(declaration.clone()).expect("law"))
            .expect("tree"),
    )
    .expect("payload");
    let all = projected(&tighter, &base);
    assert_eq!(all.len(), 2);
    assert!(
        all.iter()
            .any(|instance| instance.law.artifact_id == law.artifact_id)
    );
    declaration.id = serde_json::from_value::<norn_ir::LawIR>(
        serde_json::to_value(law.payload.to_json_value()).expect("wire"),
    )
    .expect("law")
    .id;
    declaration.condition = Some(norn_ir::ZenExpression("self.total_amount >= 2".to_owned()));
    tighter.payload = norn_ir::ArtifactPayload::from_kind_json(
        &norn_ir::ArtifactKind::Law,
        serde_json::from_value(serde_json::to_value(declaration).expect("law")).expect("tree"),
    )
    .expect("payload");
    let errors = check_artifact_set(
        &[tighter],
        &EntityArtifactSetContext {
            registry: &BTreeSet::new(),
            base: &base,
            tenant: None,
        },
    );
    assert!(
        errors
            .iter()
            .any(|d| d.code.as_str() == codes::WEAKENS_INHERITED)
    );
    base.retain(|a| {
        a.artifact_id != "commerce.SalesOrder.entity" && a.artifact_id != law.artifact_id
    });
    let findings = norn_checker::verification::effective_law_instances(
        &[law],
        &EntityArtifactSetContext {
            registry: &BTreeSet::new(),
            base: &base,
            tenant: None,
        },
        RETAINED_TEST_BUDGET,
    )
    .expect_err("removed retained member refuses");
    assert!(
        findings
            .iter()
            .any(|d| d.code.as_str() == codes::UNRESOLVED_SCOPE)
    );
}

fn update_instances(
    base: &mut [ArtifactEnvelope],
    edit: impl FnOnce(&mut Vec<norn_ir::PackInstance>),
) {
    let owner = base
        .iter_mut()
        .find(|a| a.artifact_id == "toy.instances")
        .expect("holder");
    let mut root: norn_ir::CompositionPackRoot =
        serde_json::from_value(serde_json::to_value(owner.payload.to_json_value()).expect("wire"))
            .expect("root");
    edit(root.instances.as_mut().expect("instances"));
    owner.payload = norn_ir::ArtifactPayload::from_kind_json(
        &norn_ir::ArtifactKind::BusinessPack,
        serde_json::from_value(serde_json::to_value(root).expect("root")).expect("tree"),
    )
    .expect("payload");
}

#[test]
fn law_contained_same_pack_instances_remain_distinct_applications() {
    let (law, mut base) = membership_case();
    update_instances(&mut base, |nodes| {
        let mut other = nodes[3].clone();
        other.instance_id = "other-leaf".to_owned();
        nodes[1].instances = vec![norn_ir::PackInstanceRef {
            instance_id: other.instance_id.clone(),
            pack: other.pack.clone(),
        }];
        nodes.push(other);
    });
    let instances = projected(&law, &base);
    assert_eq!(instances.len(), 2);
    assert_eq!(instances[0].subject, instances[1].subject);
    assert_ne!(instances[0].instance_paths, instances[1].instance_paths);
}

#[test]
fn law_contained_empty_scoped_implementation_is_not_non_vacuity() {
    let (mut law, mut base) = membership_case();
    update_instances(&mut base, |nodes| {
        let mut empty = nodes[0].clone();
        empty.instance_id = "empty-app".to_owned();
        empty.imports.clear();
        empty.instances.clear();
        nodes.push(empty);
    });
    let mut declaration: norn_ir::LawIR =
        serde_json::from_value(serde_json::to_value(law.payload.to_json_value()).expect("wire"))
            .expect("law");
    declaration.scope.root = norn_ir::LawRoot::App("empty-app".to_owned());
    declaration.id = "toy.empty_scope".to_owned();
    law.artifact_id = "toy.empty_scope.law".to_owned();
    law.payload = norn_ir::ArtifactPayload::from_kind_json(
        &norn_ir::ArtifactKind::Law,
        serde_json::from_value(serde_json::to_value(declaration).expect("law")).expect("tree"),
    )
    .expect("payload");
    assert!(projected(&law, &base).is_empty());
    assert!(
        norn_checker::verification::retained_law_membership(
            &law,
            &base,
            Budget {
                max_steps: 0,
                ..RETAINED_TEST_BUDGET
            }
        )
        .is_err()
    );
}

const RETAINED_TEST_BUDGET: Budget = Budget {
    max_nodes: 128,
    max_edges: 1024,
    max_steps: 4096,
};

proptest::proptest! {
    #![proptest_config(proptest::test_runner::Config::with_cases(16))]
    #[test]
    fn law_contained_generated_orderings_preserve_effective_instances(reverse in proptest::bool::ANY, rotation in 0usize..12) {
        let (law, mut base) = membership_case(); let expected = projected(&law, &base);
        if reverse {base.reverse();} let count = rotation.checked_rem(base.len()).expect("nonempty"); base.rotate_left(count);
        proptest::prop_assert_eq!(projected(&law, &base), expected);
    }
}

fn entities() -> Vec<ArtifactEnvelope> {
    serde_json::from_str(include_str!("../../../fixtures/law-ir/entities.json")).expect("entities")
}
fn laws() -> Vec<Value> {
    serde_json::from_str(include_str!("../../../fixtures/law-ir/laws.json")).expect("laws")
}

fn interface_case(
    implemented: bool,
    expression: &str,
) -> (ArtifactEnvelope, Vec<ArtifactEnvelope>) {
    let mut base: Vec<ArtifactEnvelope> = [
        include_str!("fixtures/mapping/model/core.order.interface.json"),
        include_str!("fixtures/mapping/model/core.party.interface.json"),
        include_str!("fixtures/mapping/model/core.item.interface.json"),
    ]
    .into_iter()
    .map(|s| serde_json::from_str(s).expect("interface"))
    .collect();
    if implemented {
        base.extend(
            [
                include_str!("fixtures/mapping/model/commerce.sales-order.entity.json"),
                include_str!("fixtures/mapping/model/party.customer.entity.json"),
                include_str!("fixtures/mapping/model/fashion.style.entity.json"),
            ]
            .into_iter()
            .map(|s| serde_json::from_str::<ArtifactEnvelope>(s).expect("object")),
        );
    }
    let mut artifact: ArtifactEnvelope = serde_json::from_value(laws()[0].clone()).expect("law");
    let mut law: norn_ir::LawIR = serde_json::from_value(
        serde_json::to_value(artifact.payload.to_json_value()).expect("wire"),
    )
    .expect("law");
    law.subject = "core.Order".to_owned();
    law.scope.root = norn_ir::LawRoot::Company("org_demo".to_owned());
    law.scope.object_types = vec![law.subject.clone()];
    law.condition = None;
    law.modality = norn_ir::LawModality::Always {};
    law.response = norn_ir::LawResponse::Predicate {
        expression: norn_ir::ZenExpression(expression.to_owned()),
    };
    artifact.dependencies = vec![norn_ir::ArtifactDependency {
        artifact_id: "core.Order.interface".to_owned(),
        kind: norn_ir::ArtifactKind::Entity,
        version: Some("0.1.0".to_owned()),
        required: true,
    }];
    artifact.payload = norn_ir::ArtifactPayload::from_kind_json(
        &norn_ir::ArtifactKind::Law,
        serde_json::from_value(serde_json::to_value(law).expect("wire")).expect("tree"),
    )
    .expect("root");
    (artifact, base)
}

#[test]
fn law_interface_and_empty_validate_the_declared_contract_without_vacuous_evidence() {
    for implemented in [false, true] {
        let (artifact, base) = interface_case(implemented, "self.total_amount >= 0");
        let wire = serde_json::to_value(&artifact).expect("wire");
        assert!(run(vec![wire.clone()], &base).is_empty());
        assert_eq!(serde_json::to_value(&artifact).expect("original law"), wire);
        let (invalid, base) = interface_case(implemented, "self.implementation_only >= 0");
        assert!(
            run(vec![serde_json::to_value(invalid).expect("wire")], &base)
                .contains(&codes::PREDICATE_TYPE.to_owned())
        );
    }
}

#[test]
fn law_interface_missing_subject_and_unpinned_contract_refuse() {
    let (mut artifact, _) = interface_case(false, "self.total_amount >= 0");
    artifact.dependencies.clear();
    assert!(
        run(vec![serde_json::to_value(&artifact).expect("wire")], &[])
            .contains(&codes::UNKNOWN_BINDING.to_owned())
    );
    let (_, base) = interface_case(false, "self.total_amount >= 0");
    artifact.dependencies.clear();
    assert!(
        run(vec![serde_json::to_value(artifact).expect("wire")], &base)
            .contains(&codes::UNKNOWN_BINDING.to_owned())
    );
}

proptest::proptest! {
    #![proptest_config(proptest::test_runner::Config::with_cases(32))]
    #[test]
    fn law_empty_generated_undeclared_members_cannot_escape_declaration_check(name in "[a-z]{1,12}") {
        let expression = format!("self.{name}_missing >= 0");
        let (artifact, base) = interface_case(false, &expression);
        proptest::prop_assert!(run(vec![serde_json::to_value(artifact).expect("wire")], &base).contains(&codes::PREDICATE_TYPE.to_owned()));
    }
}
fn run(values: Vec<Value>, base: &[ArtifactEnvelope]) -> Vec<String> {
    let pool: Vec<ArtifactEnvelope> = values
        .into_iter()
        .map(|v| serde_json::from_value(v).expect("envelope"))
        .collect();
    check_artifact_set(
        &pool,
        &EntityArtifactSetContext {
            registry: &BTreeSet::new(),
            base,
            tenant: None,
        },
    )
    .into_iter()
    .map(|d| d.code.as_str().to_owned())
    .collect()
}
#[test]
fn erpnext_laws_admit_through_the_shared_set_checker() {
    let entity_findings = check_artifact_set(
        &entities(),
        &EntityArtifactSetContext {
            registry: &BTreeSet::new(),
            base: &[],
            tenant: None,
        },
    );
    assert!(entity_findings.is_empty(), "{entity_findings:?}");
    assert_eq!(run(laws(), &entities()), Vec::<String>::new());
}

#[test]
fn law_contained_pack_root_refuses_unknown_retained_instance() {
    let mut artifact: ArtifactEnvelope = serde_json::from_value(laws()[0].clone()).expect("law");
    let mut law: norn_ir::LawIR = serde_json::from_value(
        serde_json::to_value(artifact.payload.to_json_value()).expect("wire"),
    )
    .expect("law");
    law.scope.root = norn_ir::LawRoot::Pack("toy.unknown".to_owned());
    artifact.payload = norn_ir::ArtifactPayload::from_kind_json(
        &norn_ir::ArtifactKind::Law,
        serde_json::from_value(serde_json::to_value(law).expect("wire")).expect("tree"),
    )
    .expect("root");
    assert!(
        norn_checker::verification::check_law_set(&[artifact], &entities(), &BTreeSet::new())
            .iter()
            .any(|f| f.code == codes::UNRESOLVED_SCOPE)
    );
}
#[test]
fn law_types_and_absent_bindings_fail_closed() {
    for expr in [
        "self.missing == 1",
        "self.amount",
        "self.amount == 'wrong'",
        "self.amount == 9007199254740993.00000000000000000000000000001",
    ] {
        let mut law = laws()[1].clone();
        law["payload"]["response"]["expression"] = json!(expr);
        assert!(
            run(vec![law], &entities()).contains(&codes::PREDICATE_TYPE.to_owned()),
            "{expr}"
        );
    }
    let mut law = laws()[0].clone();
    law["dependencies"] = json!([]);
    assert!(run(vec![law], &entities()).contains(&codes::UNKNOWN_BINDING.to_owned()));
    let mut law = laws()[0].clone();
    law["payload"]["modality"] = json!({"kind":"next"});
    assert!(run(vec![law], &entities()).contains(&codes::INVALID_DOCUMENT.to_owned()));
}
#[test]
fn unsupported_temporal_combinations_are_stable_errors() {
    let mut law = laws()[0].clone();
    law["payload"]["timing"] = json!({"kind":"within","millis":"1"});
    assert!(run(vec![law], &entities()).contains(&codes::UNSUPPORTED_TEMPORAL.to_owned()));
    for millis in ["0", "01", "-1", "1.0", "18446744073709551616"] {
        let mut law = laws()[2].clone();
        law["payload"]["timing"]["millis"] = json!(millis);
        assert!(run(vec![law], &entities()).contains(&codes::INVALID_DOCUMENT.to_owned()));
    }
}
#[test]
fn existing_laws_survive_tightening_and_cannot_be_replaced() {
    let parent: ArtifactEnvelope = serde_json::from_value(laws()[0].clone()).expect("parent");
    let mut base = entities();
    base.push(parent);
    let mut child = laws()[0].clone();
    child["artifactId"] = json!("erpnext.child.law");
    child["payload"]["modality"] = json!({"kind":"always"});
    assert!(run(vec![child.clone()], &base).contains(&codes::WEAKENS_INHERITED.to_owned()));
    child["payload"]["id"] = json!("erpnext.additional_safety");
    assert!(run(vec![child], &base).is_empty());
}
#[test]
fn whole_set_closure_includes_hidden_new_writers_and_refuses_missing_dependencies() {
    let law: ArtifactEnvelope = serde_json::from_value(laws()[0].clone()).expect("law");
    let mut pool = entities();
    pool.push(law.clone());
    let budget = Budget {
        max_nodes: 100,
        max_edges: 100,
        max_steps: 1000,
    };
    let before = conservative_closure(&law, &pool, &BTreeSet::new(), budget).expect("closure");
    let mut hidden = serde_json::to_value(&pool[0]).expect("writer envelope");
    hidden["artifactId"] = json!("erpnext.hidden.rule");
    hidden["kind"] = json!("rule");
    hidden["payload"] = json!({"ruleId":"hidden", "ruleVersion":"2.0", "graph": {}});
    pool.push(serde_json::from_value(hidden).expect("lenient envelope"));
    let after = conservative_closure(&law, &pool, &BTreeSet::new(), budget).expect("closure");
    assert!(after.artifacts.is_superset(&before.artifacts));
    assert!(after.writers.contains(&LawWriter::Artifact {
        artifact_id: "erpnext.hidden.rule".into()
    }));
    assert!(after.writers.contains(&LawWriter::Transition {
        object_type: "erpnext.SalesOrder".into(),
        property: "status".into(),
        transition: "shipped".into()
    }));
    assert!(after.open_boundaries.contains("restore"));
    let mut edited = pool.clone();
    let mut value = serde_json::to_value(&edited[0]).expect("envelope");
    value["payload"]["objectType"]["description"] = json!("same-version changed meaning");
    edited[0] = serde_json::from_value(value).expect("modified envelope");
    let changed = conservative_closure(&law, &edited, &BTreeSet::new(), budget).expect("closure");
    assert_ne!(changed.content_digests, after.content_digests);
    pool[0].dependencies.push(norn_ir::ArtifactDependency {
        artifact_id: "absent".to_owned(),
        kind: norn_ir::ArtifactKind::Entity,
        version: Some("0.1.0".to_owned()),
        required: true,
    });
    assert!(conservative_closure(&law, &pool, &BTreeSet::new(), budget).is_err());
    assert!(
        conservative_closure(
            &law,
            &pool,
            &BTreeSet::new(),
            Budget {
                max_nodes: 0,
                ..budget
            }
        )
        .is_err()
    );
}

#[test]
fn local_law_diagnostics_keep_real_json_pointer_spans() {
    let mut law = laws()[0].clone();
    law["payload"]["timing"] = json!({"kind":"within", "millis":"1"});
    let findings = norn_checker::check_artifact_json(&law);
    let diagnostic = findings
        .iter()
        .find(|d| d.code.as_str() == codes::UNSUPPORTED_TEMPORAL)
        .expect("timing diagnostic");
    assert_eq!(
        diagnostic.span.as_ref().expect("span").pointer,
        "/payload/timing"
    );
}

#[test]
fn closure_lists_inherited_transitions_on_the_actual_subtype() {
    let law: ArtifactEnvelope = serde_json::from_value(laws()[0].clone()).expect("law");
    let mut pool = entities();
    let mut child = serde_json::to_value(&pool[0]).expect("parent");
    child["artifactId"] = json!("erpnext.SpecialOrder.entity");
    child["dependencies"] = json!([{"artifactId":"erpnext.SalesOrder.entity", "kind":"entity", "version":"0.1.0", "required":true}]);
    child["payload"]["objectType"] =
        json!({"name":"erpnext.SpecialOrder", "extends":"erpnext.SalesOrder"});
    pool.push(serde_json::from_value(child).expect("subtype"));
    pool.push(law.clone());
    let closure = conservative_closure(
        &law,
        &pool,
        &BTreeSet::new(),
        Budget {
            max_nodes: 100,
            max_edges: 100,
            max_steps: 1000,
        },
    )
    .expect("closure");
    assert!(closure.writers.contains(&LawWriter::Transition {
        object_type: "erpnext.SpecialOrder".into(),
        property: "status".into(),
        transition: "shipped".into()
    }));
}

#[test]
fn registry_typed_entity_keeps_its_transition_writers_and_refuses_without_registry() {
    let law: ArtifactEnvelope = serde_json::from_value(laws()[0].clone()).expect("law");
    let mut pool = entities();
    let mut entity = serde_json::to_value(&pool[0]).expect("entity");
    entity["payload"]["objectType"]["properties"]
        .as_array_mut()
        .expect("properties")
        .push(json!({"id":"currency", "type":{"kind":"ref", "name":"iso4217.currency_code"}}));
    pool[0] = serde_json::from_value(entity).expect("registry-typed entity");
    pool.push(law.clone());
    let registry = BTreeSet::from([norn_ir::PropertyTypeName("iso4217.currency_code".into())]);
    let budget = Budget {
        max_nodes: 100,
        max_edges: 100,
        max_steps: 1000,
    };
    let closure = conservative_closure(&law, &pool, &registry, budget).expect("registry closure");
    assert!(closure.writers.contains(&LawWriter::Transition {
        object_type: "erpnext.SalesOrder".into(),
        property: "status".into(),
        transition: "shipped".into(),
    }));
    assert!(norn_checker::verification::check_law_set(&pool, &[], &registry).is_empty());
    assert!(conservative_closure(&law, &pool, &BTreeSet::new(), budget).is_err());
    assert!(
        norn_checker::verification::check_law_set(&pool, &[], &BTreeSet::new())
            .iter()
            .any(|d| d.code.as_str() == codes::CLOSURE_REFUSED)
    );
}

#[test]
fn conditions_until_state_action_transition_and_deadline_slots_are_checked() {
    let mut law = laws()[0].clone();
    law["payload"]["condition"] = json!("self.missing == 0");
    assert!(run(vec![law], &entities()).contains(&codes::PREDICATE_TYPE.to_owned()));
    let mut law = laws()[3].clone();
    law["payload"]["modality"]["predicate"] = json!("self.status");
    assert!(run(vec![law], &entities()).contains(&codes::PREDICATE_TYPE.to_owned()));
    let mut law = laws()[0].clone();
    law["payload"]["scope"]["lifecycleStates"] =
        json!([{"objectType":"erpnext.SalesOrder", "property":"status", "states":["absent"]}]);
    assert!(run(vec![law], &entities()).contains(&codes::UNKNOWN_BINDING.to_owned()));
    let mut law = laws()[0].clone();
    law["payload"]["response"] = json!({"kind":"action", "artifactId":"absent.action"});
    assert!(run(vec![law], &entities()).contains(&codes::UNKNOWN_BINDING.to_owned()));
    let mut law = laws()[0].clone();
    law["payload"]["response"] = json!({"kind":"transition", "objectType":"erpnext.SalesOrder", "property":"status", "transitionId":"shipped"});
    assert!(run(vec![law.clone()], &entities()).is_empty());
    law["payload"]["response"]["transitionId"] = json!("absent");
    assert!(run(vec![law], &entities()).contains(&codes::UNKNOWN_BINDING.to_owned()));
    let mut law = laws()[2].clone();
    law["payload"]["timing"] = json!({"kind":"before_deadline", "property":"deadline"});
    assert!(run(vec![law.clone()], &entities()).contains(&codes::UNKNOWN_BINDING.to_owned()));
    let mut base = entities();
    let mut request = serde_json::to_value(&base[2]).expect("request");
    request["payload"]["objectType"]["properties"]
        .as_array_mut()
        .expect("properties")
        .push(json!({"id":"deadline", "type":{"kind":"timestamp", "timezone":"utc"}}));
    base[2] = serde_json::from_value(request).expect("deadline type");
    assert!(run(vec![law.clone()], &base).is_empty());
    let mut computed = serde_json::to_value(&base[2]).expect("request with deadline");
    computed["payload"]["objectType"]["properties"].as_array_mut().expect("properties").push(json!({"id":"computed_deadline", "type":{"kind":"timestamp", "timezone":"utc"}, "computed":"self.deadline"}));
    base[2] = serde_json::from_value(computed).expect("computed timestamp type");
    let entity_findings = check_artifact_set(
        &base,
        &EntityArtifactSetContext {
            registry: &BTreeSet::new(),
            base: &[],
            tenant: None,
        },
    );
    assert!(entity_findings.is_empty(), "{entity_findings:?}");
    law["payload"]["timing"]["property"] = json!("computed_deadline");
    assert!(run(vec![law], &base).contains(&codes::UNKNOWN_BINDING.to_owned()));
}

#[test]
fn composition_pack_member_kinds_follow_the_canonical_enum() {
    let mut values = laws();
    let mut pack: Value = serde_json::from_str(include_str!(
        "../../../fixtures/ir/retail-core.business_pack.json"
    ))
    .expect("pack");
    pack["artifactId"] = json!("erpnext.pack.core");
    pack["namespace"] = json!("erpnext");
    pack["payload"]["packId"] = json!("erpnext.pack.core");
    pack["payload"]["artifacts"] = Value::Array(
        values
            .iter()
            .map(|law| {
                json!({
                    "artifactId": law["artifactId"], "version": law["version"], "kind": "law"
                })
            })
            .collect(),
    );
    values.push(pack);
    assert!(run(values.clone(), &entities()).is_empty());
    for kind in [
        json!("derived"),
        json!("unknown"),
        json!({"law": null}),
        json!(null),
        json!(false),
        json!(0),
        json!([]),
    ] {
        let mut invalid = values.clone();
        invalid.last_mut().expect("pack")["payload"]["artifacts"][0]["kind"] = json!(kind);
        assert!(
            run(invalid, &entities())
                .iter()
                .any(|code| code == "business_pack.validation.invalid_payload")
        );
    }
    values.last_mut().expect("pack")["payload"]["artifacts"][0]
        .as_object_mut()
        .expect("member")
        .remove("kind");
    assert!(
        run(values, &entities())
            .iter()
            .any(|code| code == "business_pack.validation.invalid_payload")
    );
}

fn decimal_binding_findings(
    units: [Option<&str>; 3],
    scales: [u64; 3],
) -> Vec<norn_checker::Diagnostic> {
    let names = ["input", "output", "loss"];
    let interface: norn_ir::InterfaceDef = serde_json::from_value(json!({
        "name":"core.Conversion", "invariants":[{"id":"conserve","expression":"self.input == self.output + self.loss","message":"conserve"}], "properties": names.map(|id| json!({
            "id":id, "type":{"kind":"decimal-binding","group":"conversion"}
        }))
    }))
    .expect("bound contract");
    let object: norn_ir::ObjectTypeDef = serde_json::from_value(json!({
        "name":"toy.Melt", "implements":["core.Conversion"],
        "properties": names.into_iter().zip(scales).zip(units).map(|((id, scale), unit)| {
            let mut kind = json!({"kind":"decimal", "scale":scale});
            if let Some(unit) = unit { kind["unit"] = json!({"system":"ucum","code":unit}); }
            json!({"id":id, "type":kind})
        }).collect::<Vec<_>>()
    }))
    .expect("implementor");
    norn_checker::check_entity_model(
        &norn_checker::EntityModel {
            interfaces: &[interface],
            object_types: &[object],
            ..norn_checker::EntityModel::EMPTY
        },
        &norn_checker::EntityContext {
            catalog: &[],
            registry: &BTreeSet::new(),
            base: norn_checker::EntityModel::EMPTY,
            tenant: None,
        },
    )
}

#[test]
fn interface_decimal_binding_resolves_concrete_units_and_scales_without_defaults() {
    for unit in ["g", "kg"] {
        assert!(decimal_binding_findings([Some(unit); 3], [3; 3]).is_empty());
    }
    assert!(
        decimal_binding_findings([None; 3], [3; 3])
            .iter()
            .any(|d| d.code.as_str() == "core.values.unit_binding_required")
    );
    assert!(
        decimal_binding_findings([Some("g"); 3], [3, 2, 3])
            .iter()
            .any(|d| d.code.as_str() == "core.values.unit_mismatch")
    );
}

proptest::proptest! {
    #[test]
    fn interface_decimal_binding_generated_scales_are_identical_or_refused(a in 0_u64..9, b in 0_u64..9, c in 0_u64..9, same_unit in proptest::bool::ANY) {
        let findings = decimal_binding_findings([Some("g"),Some("g"),Some(if same_unit {"g"} else {"kg"})], [a,b,c]);
        proptest::prop_assert_eq!(findings.is_empty(), a == b && b == c && same_unit);
    }
}

#[test]
fn interface_decimal_binding_cannot_be_a_stored_value_type() {
    let object: norn_ir::ObjectTypeDef = serde_json::from_value(json!({
        "name":"toy.Unbound", "properties":[{"id":"quantity", "type":{"kind":"decimal-binding","group":"mass"}}]
    })).expect("syntax");
    assert!(
        norn_checker::check_object_type_local(&object)
            .iter()
            .any(|d| d.code.as_str() == "core.values.unit_binding_required")
    );
    assert!(
        decimal_binding_findings([Some("g"), Some("kg"), Some("g")], [3; 3])
            .iter()
            .any(|d| d.code.as_str() == "core.values.unit_mismatch")
    );
}

#[test]
fn interface_decimal_binding_inherited_contract_rechecks_effective_members() {
    let names = ["input", "output", "loss"];
    let requirements =
        names.map(|id| json!({"id":id,"type":{"kind":"decimal-binding","group":"conversion"}}));
    let concrete = names.map(|id| json!({"id":id,"type":{"kind":"decimal","scale":3,"unit":{"system":"ucum","code":"g"}}}));
    let interface =
        serde_json::from_value(json!({"name":"core.Conversion","properties":requirements}))
            .expect("interface");
    let parent = serde_json::from_value(
        json!({"name":"toy.Conversion","implements":["core.Conversion"],"properties":concrete}),
    )
    .expect("parent");
    let child = serde_json::from_value(
        json!({"name":"toy.Melt","extends":"toy.Conversion","properties":[
            {"id":"output","type":{"kind":"decimal","scale":2,"unit":{"system":"ucum","code":"g"}}}
        ]}),
    )
    .expect("child");
    let findings = norn_checker::check_entity_model(
        &norn_checker::EntityModel {
            interfaces: &[interface],
            object_types: &[parent, child],
            ..norn_checker::EntityModel::EMPTY
        },
        &norn_checker::EntityContext {
            catalog: &[],
            registry: &BTreeSet::new(),
            base: norn_checker::EntityModel::EMPTY,
            tenant: None,
        },
    );
    assert!(
        findings
            .iter()
            .any(|finding| finding.code.as_str() == "core.values.unit_mismatch")
    );
}

