//! QUERY-ENGINE-1 / COMPUTE-DETERMINISTIC-1: exact embedded SQL correspondence.
#![allow(
    clippy::expect_used,
    clippy::indexing_slicing,
    missing_docs,
    reason = "QUERY-ENGINE-1 native correspondence"
)]
mod query_support;
use norn_cell::{
    CellError, CellStore, CommitBatch, FactWriteRow, Model,
    compute::{Engine, QueryContext, QueryResult},
};
use norn_checker::{
    EffectiveCatalog,
    metric::compute::{QuestionPlan, admit},
};
use norn_ir::{
    ArtifactEnvelope, ArtifactKind, ArtifactPayload, Decimal, EntityIrVersion, EntityRootV2,
    PropertyType, WholeNumber,
};
use proptest::strategy::Strategy;
use query_support::{CLOCK, TYPE, WORLD, fixture, load};

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

fn compare(store: &CellStore, plan: &QuestionPlan, context: QueryContext) -> QueryResult {
    let visible = |row: &norn_cell::ObjectRecord| Ok(row.id != "G0");
    let exact = store
        .evaluate_question(plan, context.clone(), visible)
        .expect("cell");
    let mut sql = store
        .evaluate_question_duckdb(plan, context, visible)
        .expect("SQL");
    assert_eq!(sql.engine, Engine::Duckdb);
    sql.engine = Engine::Cell;
    assert_eq!(sql, exact);
    exact
}

fn scaled_fixture(scales: [u64; 2], sum: bool) -> (CellStore, QuestionPlan, QueryContext) {
    let (store, plan, context) = fixture();
    let mut object = store.model().object_types().next().expect("type").clone();
    for property in &mut object.properties {
        if let PropertyType::Decimal { scale: places, .. } = &mut property.property_type {
            *places = WholeNumber(if property.id.as_str() == "recovered" {
                scales[1]
            } else {
                scales[0]
            });
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
    question.filters.clear();
    question.dimensions[0].path.property = norn_ir::PropertyId("metal".into());
    question.dimensions[0].grain = None;
    if sum {
        question.metrics[0].metric = "demo.recovered".into();
    }
    let plan = admit(plan.definitions(), &question, &catalog, 100).expect("same admitted algebra");
    let model = Model::new(&norn_projections::SqliteDdlInput {
        object_types: &[object],
        property_types: &[],
    })
    .expect("model");
    let store = CellStore::open_in_memory(model).expect("cell");
    (store, plan, context)
}

#[test]
fn query_three_scrap_yield_sql_matches_exact_frontier_and_lineage_after_correction_and_retraction()
{
    let (mut store, plan, context) = fixture();
    load(&mut store);
    compare(&store, &plan, context.clone());
    for write in [
        query_support::put(
            "SYN-MELT-15-1",
            75_000,
            72_000,
            "2026-10-23T12:00:00Z",
            "gold",
        ),
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
fn query_sql_unsupported_declared_precision_refuses_while_cell_supports_empty_population() {
    let (store, plan, context) = scaled_fixture([7, 7], false);
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
        let (mut store,plan,context)=scaled_fixture([3,3],sum);
        let writes=population.iter().enumerate().map(|(i,(input,recovered))|scaled_row(&format!("G{i}"),*input,*recovered,[3,3])).collect();
        commit(&mut store,writes);
        compare(&store,&plan,context);
    }
}

fn scaled_row(id: &str, input: i64, recovered: i64, scales: [u64; 2]) -> FactWriteRow {
    let exact =
        |value, scale| Decimal::parse(&format!("{value}e-{scale}")).expect("declared scale");
    FactWriteRow::Put {
        object_type: TYPE.into(),
        key: id.into(),
        values: norn_ir::runtime_object! {
            "input" => exact(input, scales[0]), "recovered" => exact(recovered, scales[1]),
            "loss" => Decimal::from(0i64), "occurred_at" => "2026-10-23T12:00:00Z", "metal" => "e\u{301}",
        }.as_object().expect("object literal").clone(),
    }
}

#[test]
fn rounding_scales_and_non_nfc_buckets_preserve_envelopes_after_correction_and_retraction() {
    let id = "quoted'); DROP TABLE input;--";
    for (scales, input, recovered) in [
        ([3, 3], 128, 1i64),
        ([3, 3], 128, -1),
        ([3, 3], -128, 1),
        ([3, 3], -128, -1),
        ([3, 3], 0, 0),
        ([3, 3], 1, 9_000_000_000_000_000),
        ([0, 3], 128, 1),
        ([6, 0], 3, 1),
        ([0, 6], 1, -1),
        ([6, 0], 0, 1),
    ] {
        let (mut store, plan, context) = scaled_fixture(scales, false);
        for recovered in [recovered, recovered.checked_neg().expect("bounded sign")] {
            commit(&mut store, vec![scaled_row(id, input, recovered, scales)]);
            assert_eq!(compare(&store, &plan, context.clone()).rows.len(), 1);
        }
        commit(
            &mut store,
            vec![FactWriteRow::Delete {
                object_type: TYPE.into(),
                key: id.into(),
            }],
        );
        assert!(compare(&store, &plan, context).rows.is_empty());
    }
}

proptest::proptest! {
    #![proptest_config(proptest::test_runner::Config::with_cases(16))]
    #[test]
    fn generated_unequal_scales_match_exact_envelopes(
        scales in (0u64..7, 0u64..7).prop_filter("unequal declared scales", |(a,b)| a != b),
        population in proptest::collection::vec((-1000i64..1000,-1000i64..1000),2..8), sum in proptest::bool::ANY
    ) {
        let scales=[scales.0,scales.1];
        let (mut store,plan,context)=scaled_fixture(scales,sum);
        let writes=population.iter().enumerate().map(|(i,(input,recovered))|scaled_row(&format!("G{i}"),*input,*recovered,scales)).collect();
        commit(&mut store,writes);
        compare(&store,&plan,context);
    }
}

/// The grain labels the exact engine buckets by equal `DuckDB`'s `date_trunc`
/// in UTC, cast to a date: at every month, quarter and year boundary of 2024
/// (one second either side), around the epoch, on a leap day, across an ISO
/// week that spans a year end, and at instants swept through 2024.
#[test]
#[allow(clippy::arithmetic_side_effects, reason = "instant sweep fixture")]
fn grain_labels_equal_duckdb_date_trunc() {
    use norn_checker::metric::compute::contribution::period_start;
    use norn_ir::metric::TimeGrain;
    let conn = duckdb::Connection::open_in_memory().expect("duckdb");
    let edges = [1704067199, 1704067200, 1706745599, 1706745600, 1709251199, 1709251200, 1711929599, 1711929600, 1714521599, 1714521600, 1717199999, 1717200000, 1719791999, 1719792000, 1722470399, 1722470400, 1725148799, 1725148800, 1727740799, 1727740800, 1730419199, 1730419200, 1733011199, 1733011200, 1735689599, 1735689600, -86401, -1, 0, 951782400, 951868799, 4102444799, 1735516800, 1735516799];
    let sweep = (0..1_500_i64).map(|step| 1_703_462_400 + step * 21_599);
    for (grain, name) in [
        (TimeGrain::Day, "day"),
        (TimeGrain::Week, "week"),
        (TimeGrain::Month, "month"),
        (TimeGrain::Quarter, "quarter"),
        (TimeGrain::Year, "year"),
    ] {
        let sql = format!(
            "SELECT CAST(CAST(date_trunc('{name}', make_timestamp(CAST(? AS BIGINT) * 1000000)) AS DATE) AS VARCHAR)"
        );
        for seconds in edges.into_iter().chain(sweep.clone()) {
            let expected: String = conn
                .query_row(&sql, [seconds], |row| row.get(0))
                .expect("date_trunc");
            assert_eq!(
                period_start(seconds, grain).expect("label"),
                expected,
                "{name} at {seconds}"
            );
        }
    }
}
