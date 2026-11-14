#![allow(
    clippy::expect_used,
    clippy::indexing_slicing,
    missing_docs,
    reason = "QUERY-FANOUT native conformance"
)]
mod query_support;
use norn_cell::compute::{Engine, ivm::IncrementalQuestion, lineage::LineageDisclosure};
use norn_checker::metric::compute::{AMBIGUOUS_GRAIN, UNSUPPORTED_ENGINE, admit};
use norn_ir::metric::{DimensionPath, QuestionDimension, SafeAggregation};
use norn_ir::{Decimal, LinkId, PropertyId};
use query_support::WORLD;
use query_support::fanout::{
    Fixture, HOLDING, fanout_batch as batch, fanout_fixture as fixture, holding, tags,
};

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
