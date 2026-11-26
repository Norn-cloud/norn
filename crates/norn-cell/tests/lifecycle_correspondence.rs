//! PROOF-LIFECYCLE-3: exhaustive conditional relation/native commit correspondence.
#![cfg(feature = "native")]
#![allow(
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    missing_docs,
    reason = "bounded native correspondence"
)]
use norn_cell::test_door::TestCommit as _;
use norn_cell::{CellStore, CommitBatch, Model, SqliteCell};
use norn_checker::obligations::lifecycle::{
    finite::{Bounds, Row},
    graph::Graph,
};
use norn_checker::obligations::{ApprovedContract, DefinitionSource};
use norn_ir::{
    ArtifactEnvelope, DecisionOutput, EntityInterfaceRootV2, EntityRootV2, FactChange,
    ObjectTypeName, PropertyId, TransitionId,
};
use norn_kernel::cell_host::JsonObject;
use norn_kernel::{
    CellHost, DecisionRefusal, DecisionTurn, FactEvent, FactKey, LogicalTime, TypeCatalog, World,
    commit_decision, load_slice,
};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};

fn create_for(object_type: &str, row: &Row) -> FactChange {
    FactChange::Create {
        object_type: ObjectTypeName(object_type.into()),
        key: "synthetic".into(),
        values: row.clone(),
    }
}
fn create(row: &Row) -> FactChange {
    create_for("demo.Melt", row)
}
fn native_for(
    object_type: &str,
    model: &Model,
    catalog: &TypeCatalog<'_>,
    before: Option<&Row>,
    change: FactChange,
) -> (bool, Option<Row>) {
    let mut store = CellStore::open_in_memory(model.clone()).expect("native model");
    // Trusted fixture seeding covers EVERY pre-state, including unreachable rows.
    // The candidate operation itself always uses the checked native commit path.
    if let Some(row) = before {
        store
            .commit(&CommitBatch {
                world: "main".into(),
                scope: "org_demo/dev".into(),
                facts: vec![create_for(object_type, row)],
                ..CommitBatch::default()
            })
            .expect("fixture pre-state");
    }
    let mut cell: SqliteCell<FactEvent, JsonObject, JsonObject> = SqliteCell::new(
        store,
        "org_demo/dev",
        |c: &norn_kernel::Command<JsonObject>| c.payload.clone(),
    )
    .expect("native cell");
    let prior = cell.store().read().dump().expect("before");
    let key = FactKey::new(object_type, "synthetic");
    let slice = load_slice(&cell, [key.clone()]).expect("read");
    let turn = DecisionTurn {
        id: "correspondence".into(),
        now: LogicalTime::from_millis(1),
        world: World::Main,
        consumes: None,
    };
    let accepted = match commit_decision(
        &mut cell,
        catalog,
        &slice,
        &turn,
        &DecisionOutput {
            facts: vec![change],
            links: vec![],
            commands: vec![],
            timers: vec![],
        },
    ) {
        Ok(_) => true,
        Err(
            DecisionRefusal::NotFromState { .. }
            | DecisionRefusal::InvalidInitialState { .. }
            | DecisionRefusal::GuardFalse { .. }
            | DecisionRefusal::InvariantViolation { .. }
            | DecisionRefusal::Law {
                reason: norn_kernel::governing_laws::LawEvaluationError::Violated(_),
                ..
            },
        ) => false,
        Err(error) => panic!("unexpected native error, not a correspondence refusal: {error:?}"),
    };
    if !accepted {
        assert_eq!(prior, cell.store().read().dump().expect("atomic refusal"));
    }
    (accepted, cell.load(&key).expect("after").values)
}
fn native(
    model: &Model,
    catalog: &TypeCatalog<'_>,
    before: Option<&Row>,
    change: FactChange,
) -> (bool, Option<Row>) {
    native_for("demo.Melt", model, catalog, before, change)
}
fn fixture(joint: bool, disabled: bool, input: u8) -> (Vec<ArtifactEnvelope>, Bounds, Graph) {
    let mut declarations: Value = serde_json::from_str(include_str!(
        "../../norn-checker/tests/fixtures/jewelry_demo.json"
    ))
    .expect("actual authored chart");
    // Separately attributed Entity-root declaration; the actual App pack stays Unknown.
    declarations[1]["payload"]["scope"]["root"] = json!({"kind":"entity","id":"demo.Melt"});
    if disabled {
        declarations[0]["payload"]["objectType"]["lifecycles"][0]["transitions"][3]["guards"] =
            json!(["false"]);
    }
    if joint {
        let mut law = declarations[1].clone();
        law["artifactId"] = json!("bounded_joint");
        law["payload"]["id"] = json!("bounded_joint");
        law["payload"]["condition"] = json!("true");
        law["payload"]["response"]["expression"] = json!("self.loss_fine_g == 1");
        declarations.as_array_mut().expect("source").push(law);
    }
    for artifact in declarations.as_array_mut().expect("artifacts") {
        artifact["provenance"]["sourcePath"] = json!("tests/lifecycle_correspondence");
        if artifact["kind"] == "law" {
            artifact["payload"]["provenance"]["sourcePath"] =
                json!("tests/lifecycle_correspondence");
        }
    }
    let base: Value = serde_json::from_str(include_str!(
        "../../../registry/core/artifacts/core.units.json"
    ))
    .expect("units");
    let source_text = serde_json::to_string(&declarations).expect("source");
    let source = DefinitionSource::parse(
        &source_text,
        &serde_json::to_string(&vec![base]).expect("base"),
    )
    .expect("exact source");
    let bounds = Bounds {
        object_type: "demo.Melt".into(),
        property: "status".into(),
        seed: 0,
        frozen: BTreeMap::from([
            ("id".into(), vec!["\"synthetic\"".into()]),
            ("input_fine_g".into(), vec![input.to_string()]),
            ("output_fine_g".into(), vec!["1".into()]),
            ("loss_fine_g".into(), vec!["0".into(), "1".into()]),
        ]),
    };
    let authority = ApprovedContract::approve(
        &source,
        &BTreeSet::new(),
        json!({"expr":"norn-zen/6"}),
        json!({"store":"one initialized object"}),
        json!({"level":"bounded"}),
    )
    .expect("admitted");
    let graph = authority
        .lifecycle_graph(&source, &BTreeSet::new(), &bounds)
        .expect("fresh inputs")
        .graph
        .expect("qualified graph");
    (
        serde_json::from_str(&source_text).expect("same native declarations"),
        bounds,
        graph,
    )
}
#[test]
fn every_bounded_initialization_and_transition_matches_native_commit() {
    for (joint, disabled, expected_edges) in [
        (false, false, 7),
        (true, false, 4),
        (false, true, 6),
        (true, true, 3),
    ] {
        let (source, _, graph) = fixture(joint, disabled, 2);
        assert_eq!(graph.inputs.laws.len(), if joint { 2 } else { 1 });
        assert_eq!(graph.initial.len(), if joint { 1 } else { 2 });
        assert_eq!(graph.edges.len(), expected_edges);
        compare(&source, &graph);
    }
}
fn compare(artifacts: &[ArtifactEnvelope], graph: &Graph) {
    let entity: EntityRootV2 = artifacts[0]
        .payload
        .to_json_value()
        .decode()
        .expect("entity");
    let definitions = [entity.object_type];
    let model = Model::from_definitions(&definitions, &[])
        .expect("projection")
        .with_governing_laws(artifacts)
        .expect("retained laws");
    let catalog = TypeCatalog::new(definitions.iter()).with_governing_laws(artifacts);
    assert_eq!(graph.inputs.nodes.len(), 10);
    let mut compared = 0_usize;
    let mut committed_steps = 0_usize;
    for (from, row) in graph.inputs.nodes.iter().enumerate() {
        let (committed, after) = native(&model, &catalog, None, create(row));
        assert_eq!(
            committed,
            graph.initial.contains(&from),
            "initial row {from}"
        );
        assert_eq!(after.as_ref(), if committed { Some(row) } else { None });
        compared = compared.checked_add(1).expect("bounded case count");
        for transition in &graph.inputs.machine.lifecycle.transitions {
            let edge = graph
                .edges
                .iter()
                .find(|edge| edge.from == from && edge.transition == transition.id.as_str());
            let change = FactChange::Transition {
                object_type: ObjectTypeName("demo.Melt".into()),
                key: "synthetic".into(),
                lifecycle: PropertyId("status".into()),
                transition: TransitionId(transition.id.as_str().into()),
            };
            let (committed, after) = native(&model, &catalog, Some(row), change);
            assert_eq!(
                committed,
                edge.is_some(),
                "pre-state {from}, transition {}",
                transition.id.as_str()
            );
            committed_steps = committed_steps
                .checked_add(usize::from(committed))
                .expect("bounded case count");
            assert_eq!(
                after.as_ref(),
                Some(edge.map_or(row, |edge| &graph.inputs.nodes[edge.to]))
            );
            compared = compared.checked_add(1).expect("bounded case count");
        }
    }
    assert_eq!(
        graph.edges.len(),
        committed_steps,
        "no extra or duplicate edges"
    );
    assert_eq!(
        graph.initial.iter().collect::<BTreeSet<_>>().len(),
        graph.initial.len(),
        "no duplicate initial rows"
    );
    assert_eq!(
        compared, 50,
        "10 initializations + every one of 10 × 4 transitions; no sampling"
    );
}

proptest::proptest! {
    #![proptest_config(proptest::test_runner::Config::with_cases(8))]
    #[test]
    fn generated_frozen_contexts_preserve_native_correspondence(input in 1_u8..6, joint in proptest::prelude::any::<bool>(), disabled in proptest::prelude::any::<bool>()) {
        let (source, _, graph) = fixture(joint, disabled, input);
        compare(&source, &graph);
    }
}

#[test]
fn invariants_nullables_and_exact_decimal_match_native_commit() {
    let interface = json!({
        "artifactId": "test.Measured.interface",
        "dependencies": [],
        "irVersion": "0.1",
        "kind": "entity",
        "lifecycle": "active",
        "name": "Measured",
        "namespace": "test",
        "payload": {
            "entityVersion": "2.0",
            "interface": {
                "name": "test.Measured",
                "properties": [
                    { "id": "quantity", "type": { "kind": "decimal", "scale": 2 } },
                    { "id": "unit", "type": { "kind": "text" }, "constraints": { "nullable": true } },
                    { "id": "currency", "type": { "kind": "text" }, "constraints": { "nullable": true } }
                ],
                "invariants": [
                    {
                        "id": "positive_quantity",
                        "expression": "self.quantity > 0",
                        "message": "quantity must be strictly positive"
                    },
                    {
                        "id": "single_measure",
                        "expression": "((self.unit != null) and (self.currency == null)) or ((self.unit == null) and (self.currency != null))",
                        "message": "must carry either unit or currency"
                    }
                ]
            }
        },
        "provenance": { "source": "authored", "sourcePath": "tests/lifecycle_correspondence" },
        "scope": {},
        "version": "0.1.0"
    });
    let object = json!({
        "artifactId": "test.ExactOrder.entity",
        "dependencies": [
            { "artifactId": "test.Measured.interface", "kind": "entity", "required": true, "version": "0.1.0" }
        ],
        "irVersion": "0.1",
        "kind": "entity",
        "lifecycle": "active",
        "name": "ExactOrder",
        "namespace": "test",
        "payload": {
            "entityVersion": "2.0",
            "objectType": {
                "name": "test.ExactOrder",
                "implements": ["test.Measured"],
                "properties": [
                    {
                        "id": "status",
                        "type": {
                            "kind": "enum",
                            "values": [
                                { "value": "draft" },
                                { "value": "submitted" },
                                { "value": "closed" }
                            ]
                        }
                    },
                    { "id": "quantity", "type": { "kind": "decimal", "scale": 2 } },
                    { "id": "unit", "type": { "kind": "text" }, "constraints": { "nullable": true } },
                    { "id": "currency", "type": { "kind": "text" }, "constraints": { "nullable": true } },
                    { "id": "a", "type": { "kind": "decimal", "scale": 10 } },
                    { "id": "b", "type": { "kind": "decimal", "scale": 10 } }
                ],
                "invariants": [
                    {
                        "id": "positive_product",
                        "expression": "self.a * self.b > 0",
                        "message": "exact decimal product must be positive"
                    },
                    {
                        "id": "close_requires_large_qty",
                        "expression": "self.status != 'closed' or self.quantity >= 10",
                        "message": "only orders with quantity >= 10 may close"
                    }
                ],
                "lifecycles": [{
                    "property": "status",
                    "initial": ["draft"],
                    "terminal": ["closed"],
                    "endings": [{ "state": "closed", "outcome": "completed" }],
                    "transitions": [
                        { "id": "submit", "from": ["draft"], "to": "submitted" },
                        { "id": "close", "from": ["submitted"], "to": "closed" }
                    ]
                }]
            }
        },
        "provenance": { "source": "authored", "sourcePath": "tests/lifecycle_correspondence" },
        "scope": {},
        "version": "0.1.0"
    });
    let declarations = json!([interface, object]);
    let source_text = serde_json::to_string(&declarations).expect("source");
    let source = DefinitionSource::parse(&source_text, "[]").expect("exact source");
    let authority = ApprovedContract::approve(
        &source,
        &BTreeSet::new(),
        json!({"expr":"norn-zen/7"}),
        json!({"store":"one initialized object"}),
        json!({"level":"bounded"}),
    )
    .expect("admitted");

    let envelopes: Vec<ArtifactEnvelope> = serde_json::from_str(&source_text).expect("envelopes");
    let iface_root: EntityInterfaceRootV2 = envelopes[0]
        .payload
        .to_json_value()
        .decode()
        .expect("interface");
    let obj_root: EntityRootV2 = envelopes[1]
        .payload
        .to_json_value()
        .decode()
        .expect("object");
    let interfaces = [iface_root.interface];
    let definitions = [obj_root.object_type];
    let model = Model::from_definitions_with_interfaces(&definitions, &[], &interfaces)
        .expect("native model");
    let catalog = TypeCatalog::new(definitions.iter()).with_interfaces(interfaces.iter());

    let check_bounds = |bounds: &Bounds, expected_initial: usize, expected_edges: usize| {
        let graph = authority
            .lifecycle_graph(&source, &BTreeSet::new(), bounds)
            .expect("fresh inputs")
            .graph
            .expect("qualified graph");
        assert_eq!(graph.inputs.invariants.len(), 4);
        assert_eq!(graph.initial.len(), expected_initial);
        assert_eq!(graph.edges.len(), expected_edges);

        let mut committed_steps = 0_usize;
        for (from, row) in graph.inputs.nodes.iter().enumerate() {
            let (committed, after) = native_for(
                "test.ExactOrder",
                &model,
                &catalog,
                None,
                create_for("test.ExactOrder", row),
            );
            assert_eq!(
                committed,
                graph.initial.contains(&from),
                "initial row {from} ({row:?})"
            );
            assert_eq!(after.as_ref(), if committed { Some(row) } else { None });
            for transition in &graph.inputs.machine.lifecycle.transitions {
                let edge = graph
                    .edges
                    .iter()
                    .find(|edge| edge.from == from && edge.transition == transition.id.as_str());
                let change = FactChange::Transition {
                    object_type: ObjectTypeName("test.ExactOrder".into()),
                    key: "synthetic".into(),
                    lifecycle: PropertyId("status".into()),
                    transition: TransitionId(transition.id.as_str().into()),
                };
                let (committed, after) =
                    native_for("test.ExactOrder", &model, &catalog, Some(row), change);
                assert_eq!(
                    committed,
                    edge.is_some(),
                    "pre-state {from} ({row:?}), transition {}",
                    transition.id.as_str()
                );
                committed_steps = committed_steps
                    .checked_add(usize::from(committed))
                    .expect("bounded case count");
                assert_eq!(
                    after.as_ref(),
                    Some(edge.map_or(row, |edge| &graph.inputs.nodes[edge.to]))
                );
            }
        }
        assert_eq!(graph.edges.len(), committed_steps);
    };

    // Case A: normal exact decimals + nullable unit/currency + inherited & lifecycle-blocking invariants.
    check_bounds(
        &Bounds {
            object_type: "test.ExactOrder".into(),
            property: "status".into(),
            seed: 0,
            frozen: BTreeMap::from([
                ("quantity".into(), vec!["5".into(), "10".into()]),
                ("unit".into(), vec!["\"each\"".into(), "null".into()]),
                ("currency".into(), vec!["null".into()]),
                ("a".into(), vec!["0.5".into()]),
                ("b".into(), vec!["0.5".into()]),
            ]),
        },
        2,
        3,
    );

    // Case B: exact-arithmetic scale overflow (scale 10 * scale 10 = 20 > 18)
    // must be refused identically by graph.rs (evaluate_prepared) and native commit_check.
    check_bounds(
        &Bounds {
            object_type: "test.ExactOrder".into(),
            property: "status".into(),
            seed: 0,
            frozen: BTreeMap::from([
                ("quantity".into(), vec!["10".into()]),
                ("unit".into(), vec!["\"each\"".into()]),
                ("currency".into(), vec!["null".into()]),
                ("a".into(), vec!["0.0000000001".into()]),
                ("b".into(), vec!["0.0000000001".into()]),
            ]),
        },
        0,
        0,
    );
}
