//! DATA-ASOF: T is the declared query window, K is receipt time plus frontier.
#![allow(
    clippy::expect_used,
    clippy::indexing_slicing,
    missing_docs,
    clippy::panic,
    reason = "native historical query conformance"
)]
mod query_support;
use norn_cell::{CommitBatch, KnowledgeCutoff};
use norn_kernel::LogicalTime;
use query_support::*;
#[test]
fn late_scrap_correction_preserves_the_original_bitemporal_answer() {
    let (mut store, plan, context) = fixture();
    load(&mut store);
    let k1 = KnowledgeCutoff {
        recorded_at: LogicalTime::from_millis(CLOCK),
        frontier: store.verified_head().expect("K1").commits,
    };
    let first = store
        .evaluate_question_known_at(&plan, context.clone(), k1, |_| Ok(true))
        .expect("T/K1");
    let bytes = serde_json::to_vec(&first).expect("old receipt-bound answer");
    let week = first.result.rows.get("\"2026-10-19\"").expect("showcase");
    assert_eq!(week.totals[0].canonical(), "294");
    assert_eq!(week.totals[1].canonical(), "300");
    assert_eq!(week.value.expect("ratio").canonical(), "0.98");
    let now = CLOCK.checked_add(86_400_000).expect("K2");
    store
        .commit(&CommitBatch {
            world: WORLD.into(),
            scope: "synthetic".into(),
            logical_time: now,
            writes: vec![put(
                "SYN-MELT-15-1",
                75_000,
                72_000,
                "2026-10-23T12:01:00Z",
                "gold",
            )],
            ..CommitBatch::default()
        })
        .expect("accepted source correction");
    let head = store.verified_head().expect("K2 frontier");
    let mut next = context.clone();
    next.now_ms = now;
    let watermark = next
        .watermarks
        .get_mut("erp")
        .expect("source")
        .get_mut("measurements")
        .expect("partition");
    watermark.heartbeat_ms = Some(now);
    watermark.observed = "k2/81".into();
    next.source_cursors
        .get_mut("erp")
        .expect("source")
        .insert("measurements".into(), "k2/81".into());
    let k2 = KnowledgeCutoff {
        recorded_at: LogicalTime::from_millis(now),
        frontier: head.commits,
    };
    let second = store
        .evaluate_question_known_at(&plan, next, k2, |_| Ok(true))
        .expect("T/K2");
    let week = second
        .result
        .rows
        .get("\"2026-10-19\"")
        .expect("corrected showcase");
    assert_eq!(week.totals[0].canonical(), "292.5");
    assert_eq!(week.totals[1].canonical(), "300");
    assert_eq!(week.value.expect("ratio").canonical(), "0.975");
    let old = store
        .evaluate_question_known_at(&plan, context, k1, |_| Ok(true))
        .expect("original cut still reproducible");
    assert_eq!(serde_json::to_vec(&old).expect("old answer"), bytes);
    assert_eq!(store.verified_head().expect("read-only"), head);
}


#[test]
fn asof_requires_matching_authenticated_cutoff_and_preserves_historical_pins() {
    use norn_cell::{
        CellError,
        compute::{Freshness, ivm::IncrementalQuestion},
    };
    let (mut store, base, mut context) = fixture();
    load(&mut store);
    let point = KnowledgeCutoff {
        recorded_at: LogicalTime::from_millis(CLOCK),
        frontier: store.verified_head().expect("head").commits,
    };
    let plan = asof_plan(&store, &base, "2026-10-26T00:00:00.000Z");
    assert!(matches!(
        store.evaluate_question(&plan, context.clone(), |_| panic!("no unpinned read")),
        Err(CellError::ComputeRefused("core.compute.unsupported_engine"))
    ));
    context
        .watermarks
        .get_mut("erp")
        .expect("source")
        .get_mut("measurements")
        .expect("partition")
        .heartbeat_ms = Some(CLOCK.checked_sub(60_000).expect("old heartbeat"));
    let result = store
        .evaluate_question_known_at(&plan, context.clone(), point, |_| Ok(true))
        .expect("AsOf accepted");
    assert_eq!(
        result.result.rows["\"2026-10-19\""].totals[0].canonical(),
        "294"
    );
    assert_eq!(result.result.freshness, Freshness::Exact);
    assert_eq!(result.knowledge_time_ms, CLOCK);
    assert_eq!(result.result.known_as_of, point.frontier);
    let maintained =
        IncrementalQuestion::rebuild_known_at(&store, &plan, context.clone(), point, |_| Ok(true))
            .expect("same historical IVM");
    assert_eq!(maintained.result().rows, result.result.rows);
    for at in ["2026-10-26T00:00:00.001Z", "2026-10-26T00:00:00.000000001Z"] {
        let mismatch = asof_plan(&store, &base, at);
        assert!(matches!(
            store.evaluate_question_known_at(&mismatch, context.clone(), point, |_| panic!(
                "mismatch must refuse before rows"
            )),
            Err(CellError::ComputeRefused("core.compute.stale_input"))
        ));
    }
}

#[test]
fn asof_incremental_advance_refuses_a_changed_cutoff_without_publishing() {
    use norn_cell::{CellError, compute::ivm::IncrementalQuestion};
    let (mut store, base, context) = fixture();
    load(&mut store);
    let point = KnowledgeCutoff {
        recorded_at: LogicalTime::from_millis(CLOCK),
        frontier: store.verified_head().expect("head").commits,
    };
    let plan = asof_plan(&store, &base, "2026-10-26T00:00:00.000Z");
    let mut maintained =
        IncrementalQuestion::rebuild_known_at(&store, &plan, context.clone(), point, |_| Ok(true))
            .expect("AsOf IVM");
    maintained
        .advance_known_at(&store, context.clone(), point, |_| Ok(true))
        .expect("matching redelivery");
    let before = maintained.result().clone();
    let later = CLOCK.checked_add(1).expect("later knowledge");
    store
        .commit(&CommitBatch {
            world: WORLD.into(),
            scope: "synthetic".into(),
            logical_time: later,
            writes: vec![put(
                "SYN-MELT-15-1",
                75_000,
                72_000,
                "2026-10-23T12:01:00Z",
                "gold",
            )],
            ..CommitBatch::default()
        })
        .expect("later correction");
    let mut next = context.clone();
    next.now_ms = later;
    let cutoff = KnowledgeCutoff {
        recorded_at: LogicalTime::from_millis(later),
        frontier: store.verified_head().expect("next head").commits,
    };
    assert!(matches!(
        maintained.advance_known_at(&store, next, cutoff, |_| panic!("no mismatched read")),
        Err(CellError::ComputeRefused("core.compute.stale_input"))
    ));
    assert_eq!(maintained.result(), &before);
    maintained
        .advance_known_at(&store, context, point, |_| Ok(true))
        .expect("original cutoff remains resumable");
    assert_eq!(maintained.result(), &before);
}
