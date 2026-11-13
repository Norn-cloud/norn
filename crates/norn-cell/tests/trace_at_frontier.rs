//! "See the records" at the answer's frontier: an answer traced with its
//! `knownAsOf` and `nowMs` reaches every member of the bucket it named, even
//! after a later commit has moved the current answer. One row per metric
//! kind: additive here (a sum and a ratio of sums), semi-additive in
//! `snapshot_question.rs`.
#![allow(
    clippy::expect_used,
    missing_docs,
    reason = "trace-at-frontier conformance"
)]
mod query_support;

use norn_cell::compute::lineage::LineageDisclosure;
use norn_cell::compute::{QueryContext, QueryResult};
use norn_cell::{CellStore, CommitBatch};
use norn_checker::metric::compute::QuestionPlan;
use query_support::{CLOCK, WORLD, fixture, load, loss_plan, put};

/// Trace `bucket` of `answer` at the frontier and clock it was given at.
fn trace_at(
    store: &CellStore,
    plan: &QuestionPlan,
    mut context: QueryContext,
    answer: &QueryResult,
    bucket: &str,
) -> usize {
    context.now_ms = answer.context.now_ms;
    let lineage = answer.lineage_ref.clone();
    store
        .trace_question_answered_matching(
            plan,
            context,
            answer.known_as_of,
            &|handle| handle == lineage,
            bucket,
            |_| Ok(true),
            LineageDisclosure {
                values: &mut |_| Ok(true),
                source: &mut |_| Ok(true),
            },
        )
        .expect("traced at its frontier")
        .contributions
        .len()
}

#[test]
fn an_additive_and_a_ratio_card_trace_at_their_answers_frontier() {
    let (_, ratio, _) = fixture();
    for (kind, plan) in [("additive", loss_plan()), ("ratio", ratio)] {
        let (mut store, _, context) = fixture();
        load(&mut store);
        let answer = store
            .evaluate_question(&plan, context.clone(), |_| Ok(true))
            .expect("answered");
        let (bucket, members) = answer
            .rows
            .iter()
            .find(|(_, row)| !row.members.is_empty())
            .map(|(bucket, row)| (bucket.clone(), row.members.len()))
            .expect("a bucket with members");
        // A later commit moves the current answer; the trace is of the given one.
        store
            .commit(&CommitBatch {
                world: WORLD.into(),
                scope: "synthetic".into(),
                logical_time: CLOCK.checked_add(60_000).expect("clock"),
                writes: vec![put(
                    "SYN-LATE-1",
                    10_000,
                    9_000,
                    "2026-10-20T12:00:00Z",
                    "gold",
                )],
                ..CommitBatch::default()
            })
            .expect("a later commit");
        assert_eq!(
            trace_at(&store, &plan, context, &answer, &bucket),
            members,
            "{kind}: every member of the bucket"
        );
    }
}
