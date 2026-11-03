//! DATA-INCREMENTAL / DATA-RESTATEMENT-DELTA: QUERY-ENGINE-1,
//! COMPUTE-DETERMINISTIC-1 and RESTATE-HISTORY-1 correspondence.
#![cfg(feature = "native")]
#![allow(
    clippy::expect_used,
    clippy::indexing_slicing,
    missing_docs,
    reason = "native acceptance assertions"
)]
mod query_support;

use norn_cell::compute::{Engine, QueryContext, QueryResult, ivm::IncrementalQuestion};
use norn_cell::{
    CellStore, CommitBatch, CommitOutcome, FactWriteRow, KnowledgeCutoff, Model, StoreOptions,
};
use norn_checker::metric::compute::QuestionPlan;
use norn_ir::{Decimal, FactRef};
use norn_kernel::LogicalTime;
use query_support::*;
use std::{
    collections::BTreeSet,
    path::{Path, PathBuf},
};

const WEEK: &str = "\"2026-10-19\"";
const G4: &str = "SYN-MELT-15-3";

struct Run {
    store: CellStore,
    path: PathBuf,
    model: Model,
    plans: [QuestionPlan; 2],
    context: QueryContext,
    live: [IncrementalQuestion; 2],
    history: [IncrementalQuestion; 2],
}

fn point(store: &CellStore, context: &QueryContext) -> KnowledgeCutoff {
    KnowledgeCutoff {
        recorded_at: LogicalTime::from_millis(context.now_ms),
        frontier: store.verified_head().expect("frontier").commits,
    }
}

fn progress(store: &CellStore, context: &mut QueryContext, at: u64) {
    context.now_ms = at;
    let cursor = format!(
        "synthetic-receipt/{}",
        store.verified_head().expect("head").commits
    );
    context
        .source_cursors
        .get_mut("erp")
        .expect("source")
        .insert("measurements".into(), cursor.clone());
    let mark = context
        .watermarks
        .get_mut("erp")
        .expect("source")
        .get_mut("measurements")
        .expect("partition");
    mark.observed = cursor;
    mark.heartbeat_ms = Some(at);
}

impl Run {
    fn new(path: &Path, writes: Vec<FactWriteRow>) -> Self {
        let (seed, plan, mut context) = fixture();
        let model = seed.model().clone();
        drop(seed);
        let mut store =
            CellStore::open(path, model.clone(), StoreOptions::default()).expect("open");
        store
            .commit(&CommitBatch {
                world: WORLD.into(),
                scope: context.scope.clone(),
                logical_time: CLOCK,
                writes,
                ..CommitBatch::default()
            })
            .expect("initial capture");
        progress(&store, &mut context, CLOCK);
        Self::rebuild(store, path.to_owned(), model, [plan, loss_plan()], context)
    }

    fn rebuild(
        store: CellStore,
        path: PathBuf,
        model: Model,
        plans: [QuestionPlan; 2],
        context: QueryContext,
    ) -> Self {
        let cutoff = point(&store, &context);
        let live = plans.each_ref().map(|plan| {
            IncrementalQuestion::rebuild(&store, plan, context.clone(), |_| Ok(true))
                .expect("complete live rebuild")
        });
        let history = plans.each_ref().map(|plan| {
            IncrementalQuestion::rebuild_known_at(&store, plan, context.clone(), cutoff, |_| {
                Ok(true)
            })
            .expect("complete historical rebuild")
        });
        assert!(
            live.iter()
                .chain(history.iter())
                .all(IncrementalQuestion::was_rebuilt)
        );
        let run = Self {
            store,
            path,
            model,
            plans,
            context,
            live,
            history,
        };
        run.assert_reference();
        run
    }

    fn assert_reference(&self) {
        let cutoff = point(&self.store, &self.context);
        for i in 0..2 {
            let mut exact = self
                .store
                .evaluate_question(&self.plans[i], self.context.clone(), |_| Ok(true))
                .expect("live full recompute");
            exact.engine = Engine::Ivm;
            assert_eq!(
                self.live[i].result(),
                &exact,
                "entire live envelope, rows and versioned contributions"
            );
            let mut historical = self
                .store
                .evaluate_question_known_at(&self.plans[i], self.context.clone(), cutoff, |_| {
                    Ok(true)
                })
                .expect("same T/K recompute")
                .result;
            historical.engine = Engine::Ivm;
            assert_eq!(
                self.history[i].result(),
                &historical,
                "entire historical envelope at the identical cutoff"
            );
            assert_eq!(self.live[i].result().rows, self.history[i].result().rows);
        }
    }

    fn advance(&mut self) {
        let cutoff = point(&self.store, &self.context);
        for i in 0..2 {
            self.live[i]
                .advance(&self.store, self.context.clone(), |_| Ok(true))
                .expect("receipt delta");
            self.history[i]
                .advance_known_at(&self.store, self.context.clone(), cutoff, |_| Ok(true))
                .expect("historical receipt delta");
            assert!(!self.live[i].was_rebuilt());
            assert!(!self.history[i].was_rebuilt());
        }
        self.assert_reference();
    }

    fn apply(&mut self, writes: Vec<FactWriteRow>, key: &str, at: u64) {
        let batch = CommitBatch {
            world: WORLD.into(),
            scope: self.context.scope.clone(),
            logical_time: at,
            idempotency_key: Some(key.into()),
            writes,
            ..CommitBatch::default()
        };
        assert!(matches!(
            self.store.commit(&batch).expect("change"),
            CommitOutcome::Committed(_)
        ));
        progress(&self.store, &mut self.context, at);
        self.advance();
        let head = self.store.verified_head().expect("head");
        let before = self.answers();
        assert!(matches!(
            self.store.commit(&batch).expect("redelivery"),
            CommitOutcome::Replayed(_)
        ));
        assert_eq!(self.store.verified_head().expect("no extra commit"), head);
        self.advance();
        assert_eq!(
            self.answers(),
            before,
            "redelivery and same-frontier delivery add no delta"
        );
    }

    fn answers(&self) -> [QueryResult; 4] {
        [
            self.live[0].result().clone(),
            self.live[1].result().clone(),
            self.history[0].result().clone(),
            self.history[1].result().clone(),
        ]
    }

    fn restart(self) -> Self {
        let before = self.answers();
        let Self {
            store,
            path,
            model,
            plans,
            context,
            live,
            history,
        } = self;
        drop(live);
        drop(history);
        drop(store);
        let store = CellStore::open(&path, model.clone(), StoreOptions::default())
            .expect("real SQLite reopen");
        let resumed = Self::rebuild(store, path, model, plans, context);
        assert_eq!(
            resumed.answers(),
            before,
            "restart preserves the complete pinned answers and contribution IDs"
        );
        resumed
    }
}

fn contribution_ids(result: &QueryResult) -> BTreeSet<String> {
    result
        .rows
        .values()
        .flat_map(|row| {
            row.members
                .iter()
                .map(|(id, member)| FactRef::new(TYPE, id, member.version).stable_id())
        })
        .collect()
}

#[test]
fn data_incremental_k1_k2_has_exact_signed_deltas_and_reproducible_ids() {
    let directory = tempfile::tempdir().expect("directory");
    let mut rows = fixture_writes();
    let index = rows
        .iter()
        .position(|row| matches!(row, FactWriteRow::Put {key, ..} if key == G4))
        .expect("G4");
    let fourth = rows.remove(index);
    let mut run = Run::new(&directory.path().join("acceptance.sqlite"), rows);
    let before = run.answers();
    assert_eq!(
        before[0].rows[WEEK].totals,
        vec![Decimal::parse("210.7").expect("sum"), Decimal::from(215i64)]
    );
    assert_eq!(before[0].rows[WEEK].members.len(), 3);
    run.apply(vec![fourth], "G4", CLOCK.checked_add(1).expect("clock"));
    let k1 = run.answers();
    assert_eq!(
        k1[0].rows[WEEK].totals,
        vec![Decimal::from(294i64), Decimal::from(300i64)]
    );
    assert_eq!(
        k1[0].rows[WEEK].value,
        Some(Decimal::parse("0.98").expect("ratio"))
    );
    assert_eq!(k1[1].rows[WEEK].totals, vec![Decimal::from(6i64)]);
    assert_eq!(
        signed_delta(&before[0].rows[WEEK].totals[0], &k1[0].rows[WEEK].totals[0]),
        Decimal::parse("83.3").expect("G4 recovered")
    );
    assert_eq!(
        signed_delta(&before[0].rows[WEEK].totals[1], &k1[0].rows[WEEK].totals[1]),
        Decimal::from(85i64)
    );
    let k1_context = run.context.clone();
    let k1_point = point(&run.store, &run.context);
    run.apply(
        vec![put(
            "SYN-MELT-15-1",
            75_000,
            72_000,
            "2026-10-23T12:01:00Z",
            "gold",
        )],
        "K2",
        CLOCK.checked_add(86_400_000).expect("K2"),
    );
    let k2 = run.answers();
    assert_eq!(
        k2[0].rows[WEEK].totals,
        vec![Decimal::parse("292.5").expect("sum"), Decimal::from(300i64)]
    );
    assert_eq!(
        k2[0].rows[WEEK].value,
        Some(Decimal::parse("0.975").expect("ratio"))
    );
    assert_eq!(
        k2[1].rows[WEEK].totals,
        vec![Decimal::parse("7.5").expect("loss")]
    );
    for (before, after, expected) in [
        (
            &k1[0].rows[WEEK].totals[0],
            &k2[0].rows[WEEK].totals[0],
            "-1.5",
        ),
        (
            &k1[0].rows[WEEK].totals[1],
            &k2[0].rows[WEEK].totals[1],
            "0",
        ),
        (
            &k1[1].rows[WEEK].totals[0],
            &k2[1].rows[WEEK].totals[0],
            "1.5",
        ),
    ] {
        assert_eq!(signed_delta(before, after).canonical(), expected);
    }
    assert_corrected_ids(&k1[0], &k2[0]);
    run = run.restart();
    for (i, expected) in [(0, &k1[2]), (1, &k1[3])] {
        let mut old = run
            .store
            .evaluate_question_known_at(&run.plans[i], k1_context.clone(), k1_point, |_| Ok(true))
            .expect("reproduce K1 after K2 and restart")
            .result;
        old.engine = Engine::Ivm;
        assert_eq!(&old, expected);
    }
}

fn assert_corrected_ids(k1: &QueryResult, k2: &QueryResult) {
    let original_ids = contribution_ids(k1);
    assert_eq!(
        original_ids.len(),
        64,
        "all sixteen weeks retain four gold contribution IDs"
    );
    assert_eq!(k2.rows[WEEK].members.len(), 4);
    assert_eq!(k2.rows[WEEK].members["SYN-MELT-15-1"].version, 2);
    assert_eq!(
        contribution_ids(k2)
            .difference(&original_ids)
            .cloned()
            .collect::<BTreeSet<_>>(),
        BTreeSet::from([FactRef::new(TYPE, "SYN-MELT-15-1", 2).stable_id()])
    );
    assert_eq!(
        original_ids
            .difference(&contribution_ids(k2))
            .cloned()
            .collect::<BTreeSet<_>>(),
        BTreeSet::from([FactRef::new(TYPE, "SYN-MELT-15-1", 1).stable_id()])
    );
}

fn assert_delivery_equivalence(left: &Run, right: &Run) {
    for i in 0..2 {
        assert_eq!(left.live[i].result().rows, right.live[i].result().rows);
        assert_eq!(
            contribution_ids(left.live[i].result()),
            contribution_ids(right.live[i].result())
        );
        assert_eq!(
            left.history[i].result().rows,
            right.history[i].result().rows
        );
        assert_eq!(
            left.live[i].result().context.source_cursors,
            right.live[i].result().context.source_cursors
        );
    }
    // Physical receipt hashes differ across delivery orders; each was checked against its own exact snapshot.
}

fn reordered_sequence(changes: Vec<(u8, u8, i64, i64, bool, bool)>) {
    let directory = tempfile::tempdir().expect("directory");
    let rows = fixture_writes();
    let mut left = Run::new(&directory.path().join("forward.sqlite"), rows.clone());
    let mut right = Run::new(
        &directory.path().join("reverse.sqlite"),
        rows.into_iter().rev().collect(),
    );
    assert_delivery_equivalence(&left, &right);
    let prefix = [
        (0, 0, 1_000, 900, true, false),
        (0, 0, 2_000, 1_700, true, false),
        (1, 0, 0, 0, true, false),
        (0, 0, 1_000, 800, true, true),
    ];
    for (step, (kind, id, input, recovered, in_week, restart)) in
        prefix.into_iter().chain(changes).enumerate()
    {
        let second = id
            .checked_add(1)
            .expect("bounded ID")
            .checked_rem(4)
            .expect("nonzero divisor");
        let writes: Vec<_> = [id, second]
            .into_iter()
            .map(|id| {
                let key = format!("independent/{id}");
                if kind == 1 {
                    FactWriteRow::Delete {
                        object_type: TYPE.into(),
                        key,
                    }
                } else {
                    put(
                        &key,
                        input,
                        recovered.min(input),
                        if in_week {
                            "2026-10-23T12:00:00Z"
                        } else {
                            "2026-10-16T12:00:00Z"
                        },
                        if kind == 2 { "silver" } else { "gold" },
                    )
                }
            })
            .collect();
        let at = CLOCK
            .checked_add(
                u64::try_from(step)
                    .expect("bounded step")
                    .checked_add(1)
                    .expect("bounded step"),
            )
            .expect("clock");
        let key = format!("sequence/{step}");
        left.apply(writes.clone(), &key, at);
        right.apply(writes.into_iter().rev().collect(), &key, at);
        assert_delivery_equivalence(&left, &right);
        if restart {
            left = left.restart();
            right = right.restart();
            assert_delivery_equivalence(&left, &right);
        }
    }
    left = left.restart();
    right = right.restart();
    assert_delivery_equivalence(&left, &right);
}

proptest::proptest! {
    #![proptest_config(proptest::test_runner::Config::with_cases(16))]
    #[test]
    fn generated_changes_match_recompute_through_reorder_duplicate_and_restart(
        changes in proptest::collection::vec((0u8..3, 0u8..4, 1i64..100_000, 0i64..100_000, proptest::bool::ANY, proptest::bool::ANY), 1..9)
    ) {
        reordered_sequence(changes);
    }
}
