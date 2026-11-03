#![allow(
    clippy::expect_used,
    clippy::indexing_slicing,
    missing_docs,
    reason = "native query conformance"
)]
mod query_support;
use norn_cell::{CellError, CommitBatch, compute::Freshness};
use query_support::*;

#[test]
fn scrap_yield_live_revisions_use_the_real_cell_evaluator_and_result_frontier() {
    let (mut store, plan, mut context) = fixture();
    load(&mut store);
    let first = store
        .evaluate_question(&plan, context.clone(), |_| Ok(true))
        .expect("cell query");
    assert_eq!(first.rows.len(), 16);
    let week = first.rows.get("\"2026-10-19\"").expect("showcase week");
    assert_eq!(week.totals[0].canonical(), "294");
    assert_eq!(week.totals[1].canonical(), "300");
    assert_eq!(week.value.expect("ratio").canonical(), "0.98");
    assert_eq!(week.members.len(), 4);
    assert_eq!(first.freshness, Freshness::Exact);
    assert!(first.complete);
    assert_eq!(first.revision, store.verified_head().expect("actual head"));
    assert_eq!(first.known_as_of, first.revision.commits);
    store
        .commit(&CommitBatch {
            world: WORLD.into(),
            scope: "synthetic".into(),
            logical_time: CLOCK.checked_add(86_400_000).expect("next day"),
            writes: vec![put(
                "SYN-MELT-15-1",
                75_000,
                72_000,
                "2026-10-23T12:01:00Z",
                "gold",
            )],
            ..CommitBatch::default()
        })
        .expect("correction appends");
    context.now_ms = CLOCK.checked_add(86_400_000).expect("K2 clock");
    context
        .source_cursors
        .get_mut("erp")
        .expect("source")
        .insert("measurements".into(), "k2/81".into());
    let watermark = context
        .watermarks
        .get_mut("erp")
        .expect("source")
        .get_mut("measurements")
        .expect("partition");
    watermark.observed = "k2/81".into();
    watermark.heartbeat_ms = Some(context.now_ms);
    let second = store
        .evaluate_question(&plan, context, |_| Ok(true))
        .expect("new revision");
    let corrected = second.rows.get("\"2026-10-19\"").expect("week");
    assert_eq!(corrected.value.expect("ratio").canonical(), "0.975");
    assert_eq!(corrected.totals[0].canonical(), "292.5");
    assert_eq!(week.value.expect("original").canonical(), "0.98");
    assert_ne!(first.lineage_ref, second.lineage_ref);
}

#[test]
fn query_eligibility_and_visibility_do_not_change_the_quarantine_authority() {
    let (mut store, plan, mut context) = fixture();
    load(&mut store);
    context
        .exclusions
        .insert("SYN-MELT-15-1".into(), "core07-finding".into());
    let result = store
        .evaluate_question(&plan, context.clone(), |_| Ok(true))
        .expect("excluded");
    assert_eq!(result.freshness, Freshness::Incomplete);
    assert!(!result.complete);
    assert_eq!(result.rows["\"2026-10-19\""].members.len(), 3);
    context.exclusions.clear();
    context.population_pin = "fixture-without-G2".into();
    context.authorization_epoch = 2;
    let hidden = store
        .evaluate_question(&plan, context, |row| Ok(row.id != "SYN-MELT-15-1"))
        .expect("authorized population");
    assert_eq!(hidden.rows["\"2026-10-19\""].members.len(), 3);
}

#[test]
fn query_zero_input_is_unavailable_and_even_empty_scope_refusal_writes_nothing() {
    let (mut store, plan, mut context) = fixture();
    store.bind("scope", "synthetic").expect("bound scope");
    let head = store.verified_head().expect("empty head");
    context.scope = "other".into();
    assert!(matches!(
        store.evaluate_question(&plan, context.clone(), |_| Ok(true)),
        Err(CellError::ComputeRefused("core.compute.stale_input"))
    ));
    assert_eq!(head, store.verified_head().expect("unchanged empty head"));
    context.scope = "synthetic".into();
    store
        .commit(&CommitBatch {
            world: WORLD.into(),
            scope: "synthetic".into(),
            logical_time: CLOCK,
            writes: vec![put("zero", 0, 0, "2026-10-23T12:00:00Z", "gold")],
            ..CommitBatch::default()
        })
        .expect("commit");
    let result = store
        .evaluate_question(&plan, context.clone(), |_| Ok(true))
        .expect("query");
    assert!(result.rows.values().next().expect("bucket").value.is_none());
    let head = store.verified_head().expect("head");
    context.scope = "other".into();
    assert!(matches!(
        store.evaluate_question(&plan, context, |_| Ok(true)),
        Err(CellError::ComputeRefused("core.compute.stale_input"))
    ));
    assert_eq!(head, store.verified_head().expect("same head"));
}

proptest::proptest! {
    #[test]
    fn query_staleness_follows_the_required_source_heartbeat(lag in 0u64..10_000) {
        let (store, plan, mut context) = fixture();
        context.now_ms = CLOCK.checked_add(lag).expect("bounded clock");
        let result = store.evaluate_question(&plan, context, |_| Ok(true)).expect("query");
        proptest::prop_assert_eq!(result.freshness,
            if lag == 0 { Freshness::Exact } else if lag <= 5_000 { Freshness::Fresh } else { Freshness::Stale });
    }
}

#[test]
fn typed_question_lowering_matches_the_native_scrap_yield_question() {
    let source =
        include_str!("../../../tools/norn-ts/fixtures/questions/scrap-yield.question.json");
    let questions: Vec<norn_ir::QuestionIR> =
        serde_json::from_str(source).expect("strict native QuestionIR");
    let [question] = questions.as_slice() else {
        panic!("one typed question")
    };
    let (mut store, plan, context) = fixture();
    assert_eq!(
        question,
        plan.question(),
        "all admitted semantics survive lowering"
    );
    load(&mut store);
    let result = store
        .evaluate_question(&plan, context, |_| Ok(true))
        .expect("the existing exact cell path");
    assert_eq!(result.rows.len(), 16);
    let week = result.rows.get("\"2026-10-19\"").expect("showcase week");
    assert_eq!(week.value.expect("ratio").canonical(), "0.98");
    assert_eq!(week.members.len(), 4);
}
