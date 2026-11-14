//! RESTATE-HISTORY-1: receipt-maintained deltas equal rebuilds per bitemporal slice.
#![allow(
    clippy::expect_used,
    clippy::indexing_slicing,
    missing_docs,
    reason = "native delta conformance"
)]
mod query_support;
use norn_cell::{
    CommitBatch, KnowledgeCutoff,
    compute::{Engine, QueryContext, ivm::IncrementalQuestion},
};
use norn_kernel::LogicalTime;
use query_support::*;
fn coordinate(store: &norn_cell::CellStore, context: &QueryContext) -> KnowledgeCutoff {
    KnowledgeCutoff {
        recorded_at: LogicalTime::from_millis(context.now_ms),
        frontier: store.verified_head().expect("frontier").commits,
    }
}
fn progress(context: &mut QueryContext, at: u64, frontier: u64) {
    context.now_ms = at;
    let cursor = format!("correction/{frontier}");
    context
        .source_cursors
        .get_mut("erp")
        .expect("source")
        .insert("measurements".into(), cursor.clone());
    let w = context
        .watermarks
        .get_mut("erp")
        .expect("source")
        .get_mut("measurements")
        .expect("partition");
    w.observed = cursor;
    w.heartbeat_ms = Some(at);
}
proptest::proptest! {
    #![proptest_config(proptest::test_runner::Config::with_cases(16))]
    #[test]
    fn restatement_delta_equals_rebuild_and_old_answers_reproduce(
        corrections in proptest::collection::vec((0i64..75_000,14u8..28,proptest::bool::ANY),1..6)
    ) {
        let (base,plan,original)=fixture();let model=base.model().clone();drop(base);
        let directory=tempfile::tempdir().expect("restart fixture");let path=directory.path().join("cell.sqlite");
        let mut store=norn_cell::CellStore::open(&path,model.clone(),norn_cell::StoreOptions::default()).expect("store");load(&mut store);
        let first=coordinate(&store,&original);
        let old=store.evaluate_question_known_at(&plan,original.clone(),first,|_|Ok(true)).expect("old slice");
        let old_bytes=serde_json::to_vec(&old).expect("old answer");
        let mut context=original.clone();
        let mut view=IncrementalQuestion::rebuild_known_at(&store,&plan,context.clone(),first,|_|Ok(true)).expect("historical rebuild");
        for (index,(amount,day,restart)) in corrections.into_iter().enumerate() {
            let at=CLOCK.checked_add(u64::try_from(index).expect("index").checked_add(1).expect("bounded")).expect("clock");
            store.commit(&CommitBatch{world:WORLD.into(),scope:"synthetic".into(),logical_time:at,writes:vec![put("SYN-MELT-15-1",75_000,amount,&format!("2026-10-{day:02}T12:01:00Z"),"gold")],..CommitBatch::default()}).expect("source correction");
            let cutoff=KnowledgeCutoff{recorded_at:LogicalTime::from_millis(at),frontier:store.verified_head().expect("head").commits};
            progress(&mut context,at,cutoff.frontier);
            view.advance_known_at(&store,context.clone(),cutoff,|_|Ok(true)).expect("receipt delta");
            let mut rebuilt=store.evaluate_question_known_at(&plan,context.clone(),cutoff,|_|Ok(true)).expect("full same-slice rebuild").result;
            rebuilt.engine=Engine::Ivm;
            proptest::prop_assert_eq!(view.result(),&rebuilt);
            proptest::prop_assert!(!view.was_rebuilt());
            let again=store.evaluate_question_known_at(&plan,original.clone(),first,|_|Ok(true)).expect("old answer after correction");
            proptest::prop_assert_eq!(serde_json::to_vec(&again).expect("old answer"),old_bytes.clone());
            if restart {
                drop(view);drop(store);
                store=norn_cell::CellStore::open(&path,model.clone(),norn_cell::StoreOptions::default()).expect("reopen");
                view=IncrementalQuestion::rebuild_known_at(&store,&plan,context.clone(),cutoff,|_|Ok(true)).expect("restart at the same slice");
                proptest::prop_assert!(view.was_rebuilt());proptest::prop_assert_eq!(view.result(),&rebuilt);
            }
        }
    }
}
#[test]
fn showcase_restatement_retracts_recovered_mass_and_refusal_keeps_the_old_generation() {
    let (mut store, plan, mut context) = fixture();
    load(&mut store);
    let first = coordinate(&store, &context);
    let mut view =
        IncrementalQuestion::rebuild_known_at(&store, &plan, context.clone(), first, |_| Ok(true))
            .expect("K1");
    let old = view.result().clone();
    let at = CLOCK.checked_add(86_400_000).expect("K2");
    store
        .commit(&CommitBatch {
            world: WORLD.into(),
            scope: "synthetic".into(),
            logical_time: at,
            writes: vec![put(
                "SYN-MELT-15-1",
                75_000,
                72_000,
                "2026-10-23T12:01:00Z",
                "gold",
            )],
            ..CommitBatch::default()
        })
        .expect("accepted correction");
    let next = KnowledgeCutoff {
        recorded_at: LogicalTime::from_millis(at),
        frontier: store.verified_head().expect("head").commits,
    };
    progress(&mut context, at, next.frontier);
    assert!(matches!(
        view.advance_known_at(&store, context.clone(), next, |_| Err(
            norn_cell::CellError::ComputeRefused("test.refusal")
        )),
        Err(norn_cell::CellError::ComputeRefused("test.refusal"))
    ));
    assert_eq!(view.result(), &old);
    view.advance_known_at(&store, context.clone(), next, |_| Ok(true))
        .expect("K2");
    let before = old.rows.get("\"2026-10-19\"").expect("old showcase");
    let after = view
        .result()
        .rows
        .get("\"2026-10-19\"")
        .expect("new showcase");
    assert_eq!(before.totals[0].canonical(), "294");
    assert_eq!(after.totals[0].canonical(), "292.5");
    assert_eq!(before.totals[1], after.totals[1]);
    assert_eq!(after.totals[1].canonical(), "300");
    let current = view.result().clone();
    assert!(matches!(
        view.advance_known_at(&store, context, first, |_| Ok(true)),
        Err(norn_cell::CellError::ComputeRefused(
            "core.world.invalid_restatement"
        ))
    ));
    assert_eq!(view.result(), &current);
}

#[test]
fn quarantined_restatement_rebuilds_from_result_context_and_rederives_finding_pins() {
    let (mut host, plan, context) = quality_fixture();
    capture_quality(&mut host, 80_000, "bad-source");
    let prior = coordinate(host.store(), &context);
    let old = host
        .store()
        .evaluate_question_known_at(&plan, context.clone(), prior, |_| Ok(true))
        .expect("quarantined historical answer");
    let wire = serde_json::to_value(&old.result).expect("answer wire");
    assert_eq!(
        serde_json::from_value::<norn_cell::compute::QueryResult>(wire).expect("answer roundtrip"),
        old.result
    );
    let pins = old.result.exclusions.clone();
    assert_eq!(pins.len(), 1);
    // Same result-context reuse as check_restatement_delta, with no manual clearing.
    let mut restated = IncrementalQuestion::rebuild_known_at(
        host.store(),
        &plan,
        old.result.context.clone(),
        prior,
        |_| Ok(true),
    )
    .expect("rebuild prior quarantined generation");
    assert_eq!(restated.result().exclusions, pins);
    capture_quality(&mut host, 73_500, "corrected-source");
    let next = coordinate(host.store(), &context);
    let delta = restated
        .advance_known_at(host.store(), context.clone(), next, |_| Ok(true))
        .expect("restate correction");
    let mut exact = host
        .store()
        .evaluate_question_known_at(&plan, context, next, |_| Ok(true))
        .expect("recompute restatement")
        .result;
    exact.engine = Engine::Ivm;
    assert_eq!(delta, &exact);
    assert_eq!(delta.exclusions.len(), 0);
    let historical = IncrementalQuestion::rebuild_known_at(
        host.store(),
        &plan,
        old.result.context,
        prior,
        |_| Ok(true),
    )
    .expect("rebuild historical frontier after restatement");
    assert_eq!(historical.result().exclusions, pins);
    assert_eq!(historical.result().rows, old.result.rows);
}
