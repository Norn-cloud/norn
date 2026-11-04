//! PROOF-LIFECYCLE-3: exhaustive conditional relation/native commit correspondence.
#![cfg(feature = "native")]
#![allow(
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    missing_docs,
    reason = "bounded native correspondence"
)]
use norn_cell::{CellStore, CommitBatch, Model, SqliteCell};
use norn_checker::obligations::lifecycle::{
    finite::{Bounds, Row},
    graph::Graph,
};
use norn_checker::obligations::{ApprovedContract, DefinitionSource};
use norn_ir::{ArtifactEnvelope, DecisionOutput, EntityRootV2, FactChange, ObjectTypeName, PropertyId, TransitionId};
use norn_kernel::cell_host::JsonObject;
use norn_kernel::{
    CellHost, DecisionRefusal, DecisionTurn, FactEvent, FactKey, LogicalTime, TypeCatalog, World,
    commit_decision, load_slice,
};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};

fn create(row: &Row) -> FactChange {
    FactChange::Create {
        object_type: ObjectTypeName("demo.Melt".into()),
        key: "synthetic".into(),
        values: row.clone(),
    }
}
fn native(
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
                facts: vec![create(row)],
                ..CommitBatch::default()
            })
            .expect("fixture pre-state");
    }
    let mut cell: SqliteCell<FactEvent, JsonObject, JsonObject> =
        SqliteCell::new(store, "org_demo/dev", |c: &norn_kernel::Command<JsonObject>| c.payload.clone()).expect("native cell");
    let prior = cell.store().read().dump().expect("before");
    let key = FactKey::new("demo.Melt", "synthetic");
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
            commands: vec![],
            timers: vec![],
        },
    ) {
        Ok(_) => true,
        Err(
            DecisionRefusal::NotFromState { .. }
            | DecisionRefusal::InvalidInitialState { .. }
            | DecisionRefusal::GuardFalse { .. }
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
    (serde_json::from_str(&source_text).expect("same native declarations"), bounds, graph)
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
    let mut compared = 0;
    for (from, row) in graph.inputs.nodes.iter().enumerate() {
        let (committed, after) = native(&model, &catalog, None, create(row));
        assert_eq!(
            committed,
            graph.initial.contains(&from),
            "initial row {from}"
        );
        assert_eq!(after.as_ref(), if committed { Some(row) } else { None });
        compared += 1;
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
            assert_eq!(
                after.as_ref(),
                Some(edge.map_or(row, |edge| &graph.inputs.nodes[edge.to]))
            );
            compared += 1;
        }
    }
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
