#![allow(
    clippy::expect_used,
    clippy::indexing_slicing,
    dead_code,
    missing_docs,
    reason = "shared synthetic query fixture"
)]
use norn_cell::compute::{QueryContext, Watermark};
use norn_cell::{CellStore, CommitBatch, FactWriteRow, Model};
use norn_checker::{
    EffectiveCatalog,
    metric::compute::{QuestionPlan, admit},
};
use norn_ir::metric::{MetricAdditivity, MetricType, MetricTypeParams};
use norn_ir::{
    ArtifactEnvelope, Decimal, MetricDefinition, ObjectTypeDef, QuestionIR, RuntimeValue as V,
};
use serde_json::{Value, json};
use std::collections::BTreeMap;

pub(crate) const TYPE: &str = "demo.Conversion";
pub(crate) const WORLD: &str = "synthetic-k1";
pub(crate) const CLOCK: u64 = 1_792_972_800_000;
pub(crate) fn fixture() -> (CellStore, QuestionPlan, QueryContext) {
    let object: ObjectTypeDef = serde_json::from_value(json!({"name":TYPE,"properties":[
        {"id":"input","type":{"kind":"decimal","scale":3,"unit":{"system":"ucum","code":"g"}}},
        {"id":"recovered","type":{"kind":"decimal","scale":3,"unit":{"system":"ucum","code":"g"}}},
        {"id":"loss","type":{"kind":"decimal","scale":3,"unit":{"system":"ucum","code":"g"}}},
        {"id":"occurred_at","type":{"kind":"timestamp","timezone":"utc"}},
        {"id":"metal","type":{"kind":"text"}}
    ]}))
    .expect("typed model");
    let mut artifact: Value = serde_json::from_str::<Value>(include_str!(
        "../../../../fixtures/ir/metrics/ontology.json"
    ))
    .expect("artifact")[0]
        .clone();
    artifact["artifactId"] = json!("demo.Conversion.entity.v2");
    artifact["name"] = json!(TYPE);
    artifact["namespace"] = json!("demo");
    artifact["provenance"] = json!({"source":"import","sourcePath":"synthetic:scrap-yield-k1.csv"});
    artifact["payload"] = json!({"entityVersion":"2.0","objectType":object});
    let artifact: ArtifactEnvelope = serde_json::from_value(artifact).expect("typed artifact");
    let catalog = EffectiveCatalog::from_artifacts(&[artifact]);
    let model = Model::new(&norn_projections::SqliteDdlInput {
        object_types: &[object],
        property_types: &[],
    })
    .expect("model");
    let mut out: MetricDefinition = serde_json::from_str(include_str!(
        "../../../../fixtures/ir/metrics/net-sales.definition.json"
    ))
    .expect("metric");
    out.name = "demo.recovered".into();
    out.type_params.expr = Some("recovered".into());
    let aggregate = out
        .type_params
        .metric_aggregation_params
        .as_mut()
        .expect("aggregate");
    aggregate.semantic_model = norn_ir::ObjectTypeName(TYPE.into());
    aggregate.agg_time_dimension = "occurred_at".into();
    out.norn.currency = None;
    out.norn.default_filters.clear();
    out.norn.time_dimension =
        Some(serde_json::from_value(json!({"property":"occurred_at"})).expect("path"));
    let mut input = out.clone();
    input.name = "demo.input".into();
    input.type_params.expr = Some("input".into());
    let mut ratio = out.clone();
    ratio.name = "demo.yield".into();
    ratio.metric_type = MetricType::Ratio;
    ratio.type_params = MetricTypeParams {
        numerator: Some(out.name.clone()),
        denominator: Some(input.name.clone()),
        ..MetricTypeParams::default()
    };
    ratio.norn.additivity = MetricAdditivity::NonAdditive;
    let mut question: QuestionIR = serde_json::from_str(include_str!(
        "../../../../fixtures/ir/metrics/sales-by-store-day.question.json"
    ))
    .expect("question");
    question.name = "demo.yield_week".into();
    question.object_set = norn_ir::ObjectTypeName(TYPE.into());
    question.comparison = None;
    question.metrics = serde_json::from_value(json!([{"metric":"demo.yield"}])).expect("selection");
    question.dimensions =
        serde_json::from_value(json!([{"path":{"property":"occurred_at"},"grain":"week"}]))
            .expect("grain");
    question.filters = serde_json::from_value(
        json!([{"path":{"property":"metal"},"operator":"eq","value":"gold"}]),
    )
    .expect("predicate");
    question.time_window.start = norn_ir::UtcTimestamp("2026-07-06T00:00:00Z".into());
    question.time_window.end = norn_ir::UtcTimestamp("2026-10-26T00:00:00Z".into());
    question.freshness = norn_ir::metric::FreshnessPolicy::Live {
        max_age_ms: norn_ir::WholeNumber(5_000),
    };
    let plan = admit(&[out, input, ratio], &question, &catalog, 100).expect("admitted question");
    (
        CellStore::open_in_memory(model).expect("cell"),
        plan,
        context(),
    )
}

fn context() -> QueryContext {
    QueryContext {
        world: WORLD.into(),
        scope: "synthetic".into(),
        population_pin: "all-fixture-conversions".into(),
        authorization_epoch: 1,
        mapping_pin: "fixture:22cee20298906d6f1ecb8bd3eabc3d405409d7c3091c275cbfe0a8932694c28d"
            .into(),
        now_ms: CLOCK,
        source_cursors: BTreeMap::from([(
            "erp".into(),
            BTreeMap::from([("measurements".into(), "k1/80".into())]),
        )]),
        watermarks: BTreeMap::from([(
            "erp".into(),
            BTreeMap::from([(
                "measurements".into(),
                Watermark {
                    observed: "k1/80".into(),
                    heartbeat_ms: Some(CLOCK),
                    complete: true,
                },
            )]),
        )]),
        exclusions: BTreeMap::new(),
    }
}

pub(crate) fn put(key: &str, input: i64, recovered: i64, at: &str, metal: &str) -> FactWriteRow {
    FactWriteRow::Put {
        object_type: TYPE.into(),
        key: key.into(),
        values: BTreeMap::from([
            (
                "input".into(),
                V::from(Decimal::parse(&format!("{input}e-3")).expect("exact mass")),
            ),
            (
                "recovered".into(),
                V::from(Decimal::parse(&format!("{recovered}e-3")).expect("exact mass")),
            ),
            (
                "loss".into(),
                V::from(
                    Decimal::parse(&format!(
                        "{}e-3",
                        input.checked_sub(recovered).expect("bounded loss")
                    ))
                    .expect("exact loss"),
                ),
            ),
            ("occurred_at".into(), V::from(at)),
            ("metal".into(), V::from(metal)),
        ]),
    }
}
pub(crate) fn fixture_writes() -> Vec<FactWriteRow> {
    include_str!("../../../../fixtures/demo/scrap-yield-k1.csv")
        .lines()
        .skip(1)
        .map(|line| {
            let fields: Vec<_> = line.split(',').collect();
            put(
                fields[2],
                fields[4].parse().expect("input"),
                fields[5].parse().expect("recovered"),
                fields[8],
                fields[3],
            )
        })
        .collect()
}
pub(crate) fn signed_delta(before: &Decimal, after: &Decimal) -> Decimal {
    use norn_checker::metric::compute::contribution::coefficient;
    let difference = coefficient(after)
        .expect("bounded coefficient")
        .checked_sub(coefficient(before).expect("bounded coefficient"))
        .expect("bounded difference");
    Decimal::parse(&format!("{difference}e-6")).expect("exact signed difference")
}
pub(crate) fn load(store: &mut CellStore) {
    store
        .commit(&CommitBatch {
            world: WORLD.into(),
            scope: "synthetic".into(),
            logical_time: CLOCK,
            writes: fixture_writes(),
            ..CommitBatch::default()
        })
        .expect("native fixture commit");
}

pub(crate) fn loss_plan() -> QuestionPlan {
    let (store, base, _) = fixture();
    let mut artifact: Value = serde_json::from_str::<Value>(include_str!(
        "../../../../fixtures/ir/metrics/ontology.json"
    ))
    .expect("artifact")[0]
        .clone();
    artifact["artifactId"] = json!("demo.Conversion.entity.v2");
    artifact["name"] = json!(TYPE);
    artifact["namespace"] = json!("demo");
    artifact["payload"] = json!({"entityVersion":"2.0","objectType":store.model().object_types().next().expect("conversion")});
    let catalog =
        EffectiveCatalog::from_artifacts(&[serde_json::from_value(artifact).expect("typed model")]);
    let mut metric = base
        .definitions()
        .iter()
        .find(|d| d.name == "demo.recovered")
        .expect("sum")
        .clone();
    metric.name = "demo.loss".into();
    metric.type_params.expr = Some("loss".into());
    let mut question = base.question().clone();
    question.name = "demo.loss_week".into();
    question.metrics = serde_json::from_value(json!([{"metric":"demo.loss"}])).expect("selection");
    admit(&[metric], &question, &catalog, 100).expect("admitted exact loss")
}

pub(crate) type QualityHost = norn_cell::SqliteCell<
    norn_kernel::ObservationEvent,
    norn_kernel::cell_host::JsonObject,
    norn_kernel::cell_host::JsonObject,
>;
pub(crate) fn quality_fixture() -> (QualityHost, QuestionPlan, QueryContext) {
    let (base, plan, mut context) = fixture();
    let mut object = base
        .model()
        .object_types()
        .find(|o| o.name.as_str() == TYPE)
        .expect("conversion")
        .clone();
    let mut encoded = serde_json::to_value(&object).expect("object");
    encoded["invariants"] = json!([{"id":"recovery_bound","expression":"self.recovered <= self.input","message":"Recovered mass exceeds source input"}]);
    object = serde_json::from_value(encoded).expect("invariant");
    let model = Model::from_definitions(&[object.clone()], &[]).expect("model");
    let artifact = json!({"irVersion":"0.1","artifactId":"demo.Conversion.entity","kind":"entity","namespace":"demo","name":"Conversion","version":"0.1.0","lifecycle":"draft","scope":{"orgId":"org_demo","environmentId":"dev"},"dependencies":[],"provenance":{"source":"authoring_sdk"},"payload":{"entityVersion":"2.0","objectType":object}});
    let mut store = CellStore::open_in_memory(model.clone()).expect("store");
    store
        .install_definitions(
            model,
            norn_cell::InstalledDefinitionInput {
                canonical_set: serde_json::to_string(&vec![artifact]).expect("set"),
                registry: vec![],
                receipt_sha256: "synthetic-quality-v1".into(),
                admission_receipt: None,
                readmission: None,
                diff: json!({"added":{"count":1,"ids":["demo.Conversion.entity"]}}),
                installed_at: CLOCK,
                layout: None,
            },
            Some(0),
            "synthetic",
        )
        .expect("install");
    context.world = norn_cell::MAIN_WORLD.into();
    store
        .commit(&CommitBatch {
            world: context.world.clone(),
            scope: context.scope.clone(),
            logical_time: CLOCK,
            writes: fixture_writes(),
            ..CommitBatch::default()
        })
        .expect("load");
    (
        norn_cell::SqliteCell::new(store, "synthetic", |_| BTreeMap::new()).expect("host"),
        plan,
        context,
    )
}
pub(crate) fn capture_quality(host: &mut QualityHost, recovered: i64, delivery: &str) {
    use norn_kernel::snapshot_observe::{ObservedSnapshot, commit_observed_snapshot};
    use norn_kernel::{
        CellHost, DecisionTurn, FactKey, ObservationId, Observed, Provenance, TypeCatalog, World,
        load_slice,
    };
    let FactWriteRow::Put { values, .. } = put(
        "SYN-MELT-15-1",
        75_000,
        recovered,
        "2026-10-23T12:01:00Z",
        "gold",
    ) else {
        unreachable!()
    };
    let provenance: norn_ir::InstanceProvenance = serde_json::from_value(json!({"source":{"system":"synthetic-erpnext","record":"SYN-MELT-15-1"},"mappingVersion":"synthetic.measurements@1","recordedAt":"2026-10-26T00:00:00Z","sourceFields":["input","recovered"]})).expect("provenance");
    let observation = Observed {
        id: ObservationId(delivery.into()),
        provenance: Provenance {
            source: "synthetic-erpnext".into(),
            source_id: "SYN-MELT-15-1".into(),
            observed_at: host.now(),
            mapping_version: "synthetic.measurements@1".into(),
        },
        input: BTreeMap::from([
            ("values".into(), V::Object(values.clone())),
            (
                "provenance".into(),
                norn_ir::runtime::encode(&provenance).expect("encode"),
            ),
        ]),
    };
    host.inject(&observation).expect("inject");
    let pending = host.observe().expect("observe").expect("pending");
    let model = host.store().model().clone();
    let catalog = TypeCatalog::new(model.object_types());
    let key = FactKey::new(TYPE, "SYN-MELT-15-1");
    let slice = load_slice(host, [key.clone()]).expect("slice");
    let turn = DecisionTurn {
        id: delivery.into(),
        now: host.now(),
        world: World::Main,
        consumes: Some(pending.consumed()),
    };
    commit_observed_snapshot(
        host,
        &catalog,
        &slice,
        &turn,
        ObservedSnapshot {
            key: &key,
            values: &values,
            lifecycles: &[],
            provenance: &provenance,
        },
    )
    .expect("capture");
}
