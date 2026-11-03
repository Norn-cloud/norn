//! Studio's answer documents are the evaluator's exact output, retained as bytes.
//! `NORN_BLESS=1` rewrites them; review the diff.
#![allow(
    clippy::expect_used,
    missing_docs,
    reason = "retained engine answer documents"
)]
mod query_support;
use norn_cell::{CommitBatch, KnowledgeCutoff};
use norn_kernel::LogicalTime;
use query_support::*;

const DIRECTORY: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../fixtures/demo/answers");

fn retained(name: &str, document: &impl serde::Serialize) {
    let path = format!("{DIRECTORY}/{name}");
    let mut text = serde_json::to_string_pretty(document).expect("exact JSON text");
    text.push('\n');
    if std::env::var("NORN_BLESS").as_deref() == Ok("1") {
        std::fs::write(&path, &text).expect("bless retained document");
    }
    let retained = std::fs::read_to_string(&path).expect("retained document");
    assert!(
        retained == text,
        "{name} differs from the evaluator's output; review it and rerun with NORN_BLESS=1"
    );
}

#[test]
fn scrap_yield_answers_are_the_evaluators_exact_documents() {
    let (mut store, plan, context) = fixture();
    load(&mut store);
    retained("scrap-yield.question.json", plan.question());
    let k1 = KnowledgeCutoff {
        recorded_at: LogicalTime::from_millis(CLOCK),
        frontier: store.verified_head().expect("K1").commits,
    };
    let live = store
        .evaluate_question(&plan, context.clone(), |_| Ok(true))
        .expect("K1 answer");
    retained("scrap-yield.k1.answer.json", &live);
    let (mut host, quarantine_plan, quarantine_context) = quality_fixture();
    capture_quality(&mut host, 80_000, "bad-source");
    let excluded = host
        .store()
        .evaluate_question(&quarantine_plan, quarantine_context.clone(), |_| Ok(true))
        .expect("engine-owned quarantine answer");
    let finding = host
        .store()
        .read()
        .observation_findings(&quarantine_context.world, TYPE, "SYN-MELT-15-1")
        .expect("finding")
        .remove(0);
    assert_eq!(excluded.exclusions.get("SYN-MELT-15-1"), Some(&finding.id));
    retained("scrap-yield.k1-quarantined.answer.json", &excluded);
    retained("scrap-yield.quarantine.finding.json", &finding);

    let now = CLOCK.checked_add(86_400_000).expect("K2 clock");
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
        .expect("source correction");
    let mut corrected = context.clone();
    corrected.now_ms = now;
    for cursor in corrected.source_cursors.values_mut() {
        cursor.insert("measurements".into(), "k2/81".into());
    }
    for watermark in corrected
        .watermarks
        .values_mut()
        .flat_map(|source| source.values_mut())
    {
        watermark.observed = "k2/81".into();
        watermark.heartbeat_ms = Some(now);
    }
    let k2 = store
        .evaluate_question(&plan, corrected, |_| Ok(true))
        .expect("K2 answer");
    retained("scrap-yield.k2.answer.json", &k2);
    let as_of = store
        .evaluate_question_known_at(&plan, context, k1, |_| Ok(true))
        .expect("K1 as-of answer");
    retained("scrap-yield.k1-as-of.answer.json", &as_of);
}
