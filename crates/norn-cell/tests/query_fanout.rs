#![allow(
    clippy::expect_used,
    clippy::indexing_slicing,
    missing_docs,
    reason = "QUERY-FANOUT native conformance"
)]
mod query_support;
use norn_cell::compute::{
    Engine, QueryContext, Watermark, ivm::IncrementalQuestion, lineage::LineageDisclosure,
};
use norn_cell::{CellStore, CommitBatch, FactWriteRow, LinkChange, Model};
use norn_checker::{
    EffectiveCatalog,
    metric::compute::{AMBIGUOUS_GRAIN, QuestionPlan, UNSUPPORTED_ENGINE, admit},
};
use norn_ir::metric::{DimensionPath, QuestionDimension, QuestionMetric, SafeAggregation};
use norn_ir::{
    ArtifactEnvelope, ArtifactKind, ArtifactPayload, Decimal, EntityIrVersion, EntityRootV2, Link,
    LinkCardinality, LinkId, MetricDefinition, Multiplicity, ObjectTypeDef, ObjectTypeName,
    PropertyId, PropertyType, RuntimeValue as V, WholeNumber,
};
use query_support::{CLOCK, WORLD};
use std::collections::BTreeMap;
const HOLDING: &str = "example.Holding";
const TAG: &str = "example.Tag";
struct Fixture {
    store: CellStore,
    plan: QuestionPlan,
    context: QueryContext,
    definitions: Vec<MetricDefinition>,
    catalog: EffectiveCatalog,
}
fn catalog(objects: &[ObjectTypeDef]) -> EffectiveCatalog {
    let template: Vec<ArtifactEnvelope> =
        serde_json::from_str(include_str!("../../../fixtures/ir/metrics/ontology.json"))
            .expect("artifacts");
    let artifacts: Vec<_> = objects
        .iter()
        .map(|object| {
            let mut artifact = template[0].clone();
            artifact.artifact_id = format!("{}.entity.v2", object.name.as_str());
            artifact.namespace = "example".into();
            artifact.name = object.name.as_str().into();
            artifact.provenance.source_path = Some("synthetic:query-fanout".into());
            let root = EntityRootV2 {
                entity_version: EntityIrVersion::V2_0,
                object_type: object.clone(),
                mutation_authority: None,
                extensions: None,
            };
            artifact.payload = ArtifactPayload::from_kind_json(
                &ArtifactKind::Entity,
                norn_ir::json_tree_from_value(serde_json::to_value(root).expect("typed root"))
                    .expect("IR tree"),
            )
            .expect("payload");
            artifact
        })
        .collect();
    EffectiveCatalog::from_artifacts(&artifacts)
}
fn fixture() -> Fixture {
    let (seed, seed_plan, mut context) = query_support::fixture();
    let original = seed.model().object_types().next().expect("seed").clone();
    let mut holding = original.clone();
    holding.name = ObjectTypeName(HOLDING.into());
    holding
        .properties
        .retain(|p| ["input", "occurred_at"].contains(&p.id.as_str()));
    let value = holding
        .properties
        .iter_mut()
        .find(|p| p.id.as_str() == "input")
        .expect("value");
    value.id = PropertyId("value".into());
    value.property_type = PropertyType::Decimal {
        scale: WholeNumber(3),
        unit: None,
        currency_source: None,
        per_unit: None,
        rounding: None,
    };
    holding.links = vec![Link {
        id: LinkId("tags".into()),
        description: None,
        target: ObjectTypeName(TAG.into()),
        cardinality: LinkCardinality {
            to: Multiplicity::Many,
            from: Multiplicity::Many,
            required: false,
        },
        acyclic: false,
        inverse: None,
        properties: vec![],
    }];
    let mut tag = original;
    tag.name = ObjectTypeName(TAG.into());
    tag.properties.retain(|p| p.id.as_str() == "metal");
    tag.properties[0].id = PropertyId("label".into());
    let objects = [holding, tag];
    let catalog = catalog(&objects);
    let model = Model::new(&norn_projections::SqliteDdlInput {
        object_types: &objects,
        property_types: &[],
    })
    .expect("model");
    let mut measure = seed_plan
        .definitions()
        .iter()
        .find(|d| d.name == "demo.input")
        .expect("measure")
        .clone();
    measure.name = "example.value".into();
    measure.type_params.expr = Some("value".into());
    measure
        .type_params
        .metric_aggregation_params
        .as_mut()
        .expect("aggregate")
        .semantic_model = ObjectTypeName(HOLDING.into());
    let definitions = vec![measure];
    let mut question = seed_plan.question().clone();
    question.name = "example.holding_value".into();
    question.object_set = ObjectTypeName(HOLDING.into());
    question.metrics = vec![QuestionMetric {
        metric: "example.value".into(),
        inline: None,
        reaggregate: None,
    }];
    question.dimensions.clear();
    question.filters.clear();
    let plan = admit(&definitions, &question, &catalog, 100).expect("holding grain");
    context.mapping_pin = "synthetic:holding-values/1".into();
    context.population_pin = "all-example-holdings".into();
    context.source_cursors = BTreeMap::from([(
        "fixture".into(),
        BTreeMap::from([("holdings".into(), "seed/1".into())]),
    )]);
    context.watermarks = BTreeMap::from([(
        "fixture".into(),
        BTreeMap::from([(
            "holdings".into(),
            Watermark {
                observed: "seed/1".into(),
                heartbeat_ms: Some(CLOCK),
                complete: true,
            },
        )]),
    )]);
    Fixture {
        store: CellStore::open_in_memory(model).expect("store"),
        plan,
        context,
        definitions,
        catalog,
    }
}
fn holding(id: &str, value: i64) -> FactWriteRow {
    FactWriteRow::Put {
        object_type: HOLDING.into(),
        key: id.into(),
        values: BTreeMap::from([
            (
                "value".into(),
                V::from(Decimal::parse(&value.to_string()).expect("exact")),
            ),
            ("occurred_at".into(), V::from("2026-10-23T00:00:00Z")),
        ]),
    }
}
fn tags(batch: &mut CommitBatch, id: &str, count: u8) {
    for n in 0..count {
        let tag = format!("{id}-tag-{n}");
        batch.writes.push(FactWriteRow::Put {
            object_type: TAG.into(),
            key: tag.clone(),
            values: BTreeMap::from([("label".into(), V::from(tag.clone()))]),
        });
        batch.links.push(LinkChange::Add {
            object_type: HOLDING.into(),
            link: "tags".into(),
            source: id.into(),
            target: tag,
            target_type: Some(TAG.into()),
            values: BTreeMap::new(),
        });
    }
}
fn batch() -> CommitBatch {
    CommitBatch {
        world: WORLD.into(),
        scope: "synthetic".into(),
        logical_time: CLOCK,
        ..CommitBatch::default()
    }
}

#[test]
fn query_fanout_holding_80_with_two_tags_counts_once_and_equal_values_keep_distinct_ids() {
    let Fixture {
        mut store,
        plan,
        context,
        ..
    } = fixture();
    let mut initial = batch();
    initial.writes.push(holding("H1", 80));
    tags(&mut initial, "H1", 2);
    store.commit(&initial).expect("real links");
    assert_eq!(
        store
            .read()
            .link_targets(WORLD, HOLDING, "tags", "H1", 10, 4096)
            .expect("links")
            .expect("bounded")
            .len(),
        2
    );
    let result = store
        .evaluate_question(&plan, context.clone(), |_| Ok(true))
        .expect("sum");
    assert_eq!(result.rows["null"].value.expect("80").canonical(), "80");
    assert_eq!(result.rows["null"].members.len(), 1);
    let trace = store
        .trace_question(
            &plan,
            context.clone(),
            &result.lineage_ref,
            "null",
            |_| Ok(true),
            LineageDisclosure {
                values: &mut |_| Ok(true),
                source: &mut |_| Ok(false),
            },
        )
        .expect("trace");
    assert_eq!(trace.contributions.len(), 1);
    assert_eq!(trace.contributions[0].fact.key, "H1");
    let mut second = batch();
    second.writes.push(holding("H2", 80));
    tags(&mut second, "H2", 2);
    store.commit(&second).expect("equal value distinct grain");
    let result = store
        .evaluate_question(&plan, context, |_| Ok(true))
        .expect("sum both");
    assert_eq!(result.rows["null"].value.expect("160").canonical(), "160");
    assert_eq!(result.rows["null"].members.len(), 2);
}

#[test]
fn query_fanout_unspecified_expanded_tag_grain_refuses_before_execution() {
    let Fixture {
        plan,
        mut definitions,
        catalog,
        ..
    } = fixture();
    let mut expanded = plan.question().clone();
    expanded.dimensions = vec![QuestionDimension {
        path: DimensionPath {
            links: vec![LinkId("tags".into())],
            property: PropertyId("label".into()),
        },
        grain: None,
    }];
    let findings = admit(&definitions, &expanded, &catalog, 100).expect_err("ambiguous fanout");
    assert!(findings.iter().any(|d| d.code == AMBIGUOUS_GRAIN));
    definitions[0].norn.safe_aggregation = Some(SafeAggregation::Symmetric);
    let findings =
        admit(&definitions, &expanded, &catalog, 100).expect_err("unqualified linked buckets");
    assert!(findings.iter().any(|d| d.code == UNSUPPORTED_ENGINE));
}

proptest::proptest! {
    #![proptest_config(proptest::test_runner::Config::with_cases(16))]
    #[test]
    fn catalog_link_multiplicity_preserves_numeric_membership_and_delta_equals_recompute(
        population in proptest::collection::vec((0i64..1000,0u8..6),1..7)
    ) {
        let Fixture {mut store,plan,context,..} = fixture();
        let mut initial=batch();
        initial.writes=population.iter().enumerate().map(|(i,(value,_))|holding(&format!("H{i}"),*value)).collect();
        store.commit(&initial).expect("seed");
        let mut ivm=IncrementalQuestion::rebuild(&store,&plan,context.clone(),|_|Ok(true)).expect("rebuild");
        let numeric=ivm.result().rows.clone();
        let mut enrichment=batch();
        for (i,(_,count)) in population.iter().enumerate() { tags(&mut enrichment,&format!("H{i}"),*count); }
        store.commit(&enrichment).expect("catalog links");
        ivm.advance(&store,context.clone(),|_|Ok(true)).expect("link-only delta");
        let mut exact=store.evaluate_question(&plan,context,|_|Ok(true)).expect("recompute");
        exact.engine=Engine::Ivm;
        proptest::prop_assert_eq!(ivm.result(),&exact);
        proptest::prop_assert_eq!(&exact.rows,&numeric);
        let expected:i64=population.iter().map(|(value,_)|value).sum();
        proptest::prop_assert_eq!(exact.rows["null"].value,Some(Decimal::parse(&expected.to_string()).expect("independent sum")));
        proptest::prop_assert_eq!(exact.rows["null"].members.len(),population.len());
    }
}
