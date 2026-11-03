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
        .insert("SYN-MELT-15-1".into(), "caller-forged-finding".into());
    assert!(matches!(
        store.evaluate_question(&plan, context.clone(), |_| Ok(true)),
        Err(CellError::ComputeRefused("core.compute.caller_exclusions"))
    ));
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

#[test]
fn data_quarantine_pins_real_findings_and_historical_recovery_equals_ivm() {
    use norn_cell::{
        KnowledgeCutoff,
        compute::{Engine, ivm::IncrementalQuestion},
    };
    let (mut host, plan, context) = quality_fixture();
    let mut view = IncrementalQuestion::rebuild(host.store(), &plan, context.clone(), |_| Ok(true))
        .expect("rebuild");
    let before = KnowledgeCutoff { recorded_at:norn_kernel::LogicalTime::from_millis(CLOCK), frontier:host.store().verified_head().expect("pre-quarantine").commits };
    capture_quality(&mut host, 80_000, "bad-source");
    let cutoff = KnowledgeCutoff {
        recorded_at: norn_kernel::LogicalTime::from_millis(CLOCK),
        frontier: host.store().verified_head().expect("head").commits,
    };
    let excluded = host
        .store()
        .evaluate_question(&plan, context.clone(), |_| Ok(true))
        .expect("excluded");
    let finding = host
        .store()
        .read()
        .observation_findings(&context.world, TYPE, "SYN-MELT-15-1")
        .expect("finding")
        .remove(0);
    assert_eq!(excluded.context.exclusions["SYN-MELT-15-1"], finding.id);
    assert_eq!(excluded.freshness, Freshness::Incomplete);
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../fixtures/demo/answers/scrap-yield.quarantine.finding.json");
    let text = format!("{}\n", serde_json::to_string_pretty(&finding).expect("finding document"));
    if std::env::var("NORN_BLESS").as_deref() == Ok("1") {
        std::fs::create_dir_all(std::path::Path::new(path).parent().expect("directory")).expect("fixture directory");
        std::fs::write(path, &text).expect("bless finding");
    }
    assert_eq!(std::fs::read_to_string(path).expect("retained finding"), text);
    assert!(host.store().evaluate_question_known_at(&plan, context.clone(), before, |_| Ok(true)).expect("before finding").result.context.exclusions.is_empty());
    let mut reference = excluded.clone();
    reference.engine = Engine::Ivm;
    assert_eq!(
        view.advance(host.store(), context.clone(), |_| Ok(true))
            .expect("delta"),
        &reference
    );
    capture_quality(&mut host, 73_500, "corrected-source");
    let corrected = host
        .store()
        .evaluate_question(&plan, context.clone(), |_| Ok(true))
        .expect("corrected");
    assert!(corrected.context.exclusions.is_empty());
    reference = corrected;
    reference.engine = Engine::Ivm;
    assert_eq!(
        view.advance(host.store(), context.clone(), |_| Ok(true))
            .expect("recovery delta"),
        &reference
    );
    let historical = host
        .store()
        .evaluate_question_known_at(&plan, context, cutoff, |_| Ok(true))
        .expect("historical");
    assert_eq!(
        historical.result.context.exclusions,
        excluded.context.exclusions
    );
    assert_eq!(historical.result.rows, excluded.rows);
}


proptest::proptest! {
    #[test]
    fn caller_exclusion_pins_never_confer_query_authority(pin in "[a-z0-9]{1,32}") {
        let (store, plan, mut context) = fixture();
        context.exclusions.insert("SYN-MELT-15-1".into(), pin);
        proptest::prop_assert!(matches!(store.evaluate_question(&plan, context.clone(), |_| Ok(true)), Err(CellError::ComputeRefused("core.compute.caller_exclusions"))));
        proptest::prop_assert!(matches!(norn_cell::compute::ivm::IncrementalQuestion::rebuild(&store, &plan, context, |_| Ok(true)), Err(CellError::ComputeRefused("core.compute.caller_exclusions"))));
    }
}
