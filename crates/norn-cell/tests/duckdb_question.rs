#![allow(
    clippy::expect_used,
    clippy::indexing_slicing,
    missing_docs,
    reason = "QUERY-ENGINE-1 native correspondence"
)]
mod query_support;
use norn_cell::{
    CellError, CellStore, CommitBatch, FactWriteRow, Model,
    compute::{Engine, QueryContext},
};
use norn_checker::{
    EffectiveCatalog,
    metric::compute::{QuestionPlan, admit},
};
use norn_ir::{
    ArtifactEnvelope, ArtifactKind, ArtifactPayload, EntityIrVersion, EntityRootV2, PropertyType,
    WholeNumber,
};
use query_support::{CLOCK, TYPE, WORLD, fixture, load, put};

fn row(id: &str, input: i64, recovered: i64) -> FactWriteRow {
    put(id, input, recovered, "2026-10-23T12:00:00Z", "gold")
}

fn commit(store: &mut CellStore, writes: Vec<FactWriteRow>) {
    let batch = CommitBatch {
        world: WORLD.into(),
        scope: "synthetic".into(),
        logical_time: CLOCK,
        writes,
        ..CommitBatch::default()
    };
    store.commit(&batch).expect("native changes");
}

fn compare(store: &CellStore, plan: &QuestionPlan, context: QueryContext) {
    let exact = store
        .evaluate_question(plan, context.clone(), |row| Ok(row.id != "G0"))
        .expect("cell");
    let mut sql = store
        .evaluate_question_duckdb(plan, context, |row| Ok(row.id != "G0"))
        .expect("SQL");
    assert_eq!(sql.engine, Engine::Duckdb);
    sql.engine = Engine::Cell;
    assert_eq!(sql, exact);
}

fn rebind(
    store: &CellStore,
    plan: &QuestionPlan,
    scale: u64,
    sum: bool,
) -> (CellStore, QuestionPlan) {
    let mut object = store.model().object_types().next().expect("type").clone();
    for property in &mut object.properties {
        if let PropertyType::Decimal { scale: places, .. } = &mut property.property_type {
            *places = WholeNumber(scale);
        }
    }
    let mut artifacts: Vec<ArtifactEnvelope> =
        serde_json::from_str(include_str!("../../../fixtures/ir/metrics/ontology.json"))
            .expect("template");
    let artifact = &mut artifacts[0];
    artifact.artifact_id = "demo.Conversion.entity.v2".into();
    artifact.name = TYPE.into();
    artifact.namespace = "demo".into();
    let root = EntityRootV2 {
        entity_version: EntityIrVersion::V2_0,
        object_type: object.clone(),
        mutation_authority: None,
        extensions: None,
    };
    artifact.payload = ArtifactPayload::from_kind_json(
        &ArtifactKind::Entity,
        norn_ir::json_tree_from_value(serde_json::to_value(root).expect("typed root"))
            .expect("tree"),
    )
    .expect("payload");
    let catalog = EffectiveCatalog::from_artifacts(&artifacts[..1]);
    let mut question = plan.question().clone();
    if sum {
        question.metrics[0].metric = "demo.recovered".into();
    }
    let plan = admit(plan.definitions(), &question, &catalog, 100).expect("same admitted algebra");
    let model = Model::new(&norn_projections::SqliteDdlInput {
        object_types: &[object],
        property_types: &[],
    })
    .expect("model");
    (CellStore::open_in_memory(model).expect("cell"), plan)
}

#[test]
fn query_three_scrap_yield_sql_matches_exact_frontier_and_lineage_after_correction_and_retraction()
{
    let (mut store, plan, context) = fixture();
    load(&mut store);
    compare(&store, &plan, context.clone());
    for write in [
        row("SYN-MELT-15-1", 75_000, 72_000),
        FactWriteRow::Delete {
            object_type: TYPE.into(),
            key: "SYN-MELT-15-1".into(),
        },
    ] {
        commit(&mut store, vec![write]);
        compare(&store, &plan, context.clone());
    }
}

#[test]
fn query_sql_exact_rounding_signs_zero_and_large_coefficients() {
    for (input, recovered) in [
        (128, 1),
        (128, -1),
        (-128, 1),
        (-128, -1),
        (0, 0),
        (1, 9_000_000_000_000_000),
    ] {
        let (mut store, plan, context) = fixture();
        commit(
            &mut store,
            vec![row("quoted'); DROP TABLE input;--", input, recovered)],
        );
        compare(&store, &plan, context);
    }
}

#[test]
fn query_sql_unsupported_declared_precision_refuses_while_cell_supports_empty_population() {
    let (seed, plan, context) = fixture();
    let (store, plan) = rebind(&seed, &plan, 7, false);
    store
        .evaluate_question(&plan, context.clone(), |_| Ok(true))
        .expect("cell supports empty plan");
    assert!(matches!(
        store.evaluate_question_duckdb(&plan, context, |_| Ok(true)),
        Err(CellError::ComputeRefused("core.compute.unsupported_engine"))
    ));
}

proptest::proptest! {
    #![proptest_config(proptest::test_runner::Config::with_cases(16))]
    #[test]
    fn generated_sql_matches_exact_rows_and_ids_after_visibility_filtering(
        population in proptest::collection::vec((-1000i64..1000,-1000i64..1000),1..8), sum in proptest::bool::ANY
    ) {
        let (seed,base,context)=fixture();
        let (mut store,plan)=rebind(&seed,&base,3,sum);
        let writes=population.iter().enumerate().map(|(i,(input,recovered))|row(&format!("G{i}"),*input,*recovered)).collect();
        commit(&mut store,writes);
        compare(&store,&plan,context);
    }
}
