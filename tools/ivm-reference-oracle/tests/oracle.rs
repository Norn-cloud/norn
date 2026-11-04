//! COMPUTE-DETERMINISTIC-1 (#714): Differential Dataflow as an independent reference oracle.
//! For generated signed changes, Norn's receipt-maintained question must equal
//! both Norn's full recompute and a Differential Dataflow circuit over the same source rows.
//! The circuit is select (metal, window) -> project (Monday, mass) -> grouped
//! sum, the operators `certificate::question` certifies. Joins are not
//! admitted by `QuestionPlan`, so no join correspondence is claimed here.
#![allow(
    clippy::expect_used,
    clippy::indexing_slicing,
    missing_docs,
    reason = "test-only differential oracle"
)]
#[path = "../../../crates/norn-cell/tests/query_support/mod.rs"]
mod query_support;

use chrono::NaiveDate;
use differential_dataflow::input::InputSession;
use norn_cell::compute::{Engine, QueryResult, ivm::IncrementalQuestion};
use norn_cell::{CellStore, CommitBatch, CommitOutcome, FactWriteRow};
use norn_checker::metric::compute::certificate::{self, Algebraic};
use norn_ir::Decimal;
use query_support::{CLOCK, TYPE, WORLD, fixture, load, put};
use std::collections::{BTreeMap, BTreeSet};
use std::{cell::RefCell, rc::Rc, sync::mpsc, thread};

/// Source row: (id, effective epoch day, gold, input mg, recovered mg).
type Row = (u64, i32, bool, i64, i64);

/// Monday epoch day -> (recovered, input) in milligrams.
type View = BTreeMap<String, (Decimal, Decimal)>;

fn epoch_day(timestamp: &str) -> i32 {
    let date = NaiveDate::parse_from_str(&timestamp[..10], "%Y-%m-%d").expect("date");
    let epoch = NaiveDate::from_ymd_opt(1970, 1, 1).expect("epoch");
    i32::try_from((date - epoch).num_days()).expect("day")
}

/// Independent Differential Dataflow model of the admitted plan; it never calls Norn arithmetic.
struct Oracle {
    pending: Vec<(Row, isize)>,
    send: Option<mpsc::Sender<Vec<(Row, isize)>>>,
    receive: mpsc::Receiver<BTreeMap<i32, (i64, i64)>>,
    worker: Option<thread::JoinHandle<()>>,
    ids: BTreeMap<String, u64>,
    rows: BTreeMap<String, Row>,
    excluded: BTreeSet<String>,
}

impl Oracle {
    fn new() -> Self {
        let (start, end) = (epoch_day("2026-07-06"), epoch_day("2026-10-26"));
        let (send, updates) = mpsc::channel::<Vec<(Row, isize)>>();
        let (results, receive) = mpsc::channel();
        let worker = thread::spawn(move || {
            timely::execute_directly(move |worker| {
                let mut input = InputSession::<u64, Row, isize>::new();
                let output = Rc::new(RefCell::new(BTreeMap::new()));
                let sink = output.clone();
                let probe = worker.dataflow::<u64, _, _>(|scope| {
                    input
                        .to_collection(scope)
                        .filter(move |r| r.2 && r.1 >= start && r.1 < end)
                        // 1970-01-01 is Thursday: subtract days since Monday.
                        .map(|r| (r.1 - (r.1 + 3).rem_euclid(7), (r.3, r.4)))
                        .reduce(|_, rows, out| {
                            let input = rows.iter().map(|(v, w)| v.0 * *w as i64).sum::<i64>();
                            let recovered = rows.iter().map(|(v, w)| v.1 * *w as i64).sum::<i64>();
                            out.push(((recovered, input), 1_isize));
                        })
                        .inspect(move |((week, totals), _, diff)| {
                            let mut output = sink.borrow_mut();
                            let weight = output.entry((*week, *totals)).or_insert(0_isize);
                            *weight += diff;
                            if *weight == 0 {
                                output.remove(&(*week, *totals));
                            }
                        })
                        .probe()
                        .0
                });
                while let Ok(changes) = updates.recv() {
                    for (row, weight) in changes {
                        input.update(row, weight);
                    }
                    input.advance_to(input.time() + 1);
                    input.flush();
                    worker.step_while(|| probe.less_than(input.time()));
                    let snapshot = output
                        .borrow()
                        .iter()
                        .map(|((week, totals), weight)| {
                            assert_eq!(*weight, 1);
                            (*week, *totals)
                        })
                        .collect();
                    if results.send(snapshot).is_err() {
                        break;
                    }
                }
            })
        });
        Self {
            pending: Vec::new(),
            send: Some(send),
            receive,
            worker: Some(worker),
            ids: BTreeMap::new(),
            rows: BTreeMap::new(),
            excluded: BTreeSet::new(),
        }
    }

    fn effective(&self, key: &str) -> Option<Row> {
        (!self.excluded.contains(key))
            .then(|| self.rows.get(key).cloned())
            .flatten()
    }

    /// Apply one source change as a signed delta, then run one Differential Dataflow transaction.
    fn change(&mut self, key: &str, edit: impl FnOnce(&mut Self)) {
        if let Some(old) = self.effective(key) {
            self.pending.push((old, -1));
        }
        edit(self);
        if let Some(new) = self.effective(key) {
            self.pending.push((new, 1));
        }
    }

    fn put(&mut self, key: &str, day: i32, gold: bool, input: i64, recovered: i64) {
        let next = u64::try_from(self.ids.len()).expect("id");
        let id = *self.ids.entry(key.to_owned()).or_insert(next);
        self.change(key, |o| {
            o.rows
                .insert(key.to_owned(), (id, day, gold, input, recovered));
        });
    }

    fn view(&mut self) -> View {
        self.send
            .as_ref()
            .expect("worker")
            .send(std::mem::take(&mut self.pending))
            .expect("updates");
        let totals = self.receive.recv().expect("dataflow results");
        let epoch = NaiveDate::from_ymd_opt(1970, 1, 1).expect("epoch");
        let milli = |n: Option<&i64>| {
            Decimal::parse(&format!("{}e-3", n.copied().unwrap_or(0))).expect("mass")
        };
        totals
            .into_iter()
            .map(|(week, (recovered, mass))| {
                let monday = epoch + chrono::Days::new(u64::try_from(week).expect("post-epoch"));
                (
                    monday.format("%Y-%m-%d").to_string(),
                    (milli(Some(&recovered)), milli(Some(&mass))),
                )
            })
            .collect()
    }
}

impl Drop for Oracle {
    fn drop(&mut self) {
        drop(self.send.take());
        self.worker
            .take()
            .expect("worker")
            .join()
            .expect("dataflow worker");
    }
}

fn norn_view(result: &QueryResult) -> View {
    result
        .rows
        .values()
        .map(|row| {
            let bucket = row.bucket.as_str().expect("week bucket").to_owned();
            (bucket, (row.totals[0], row.totals[1]))
        })
        .collect()
}

/// One generated step: writes in one commit, or an exclusion toggle.
#[derive(Debug, Clone)]
enum Step {
    /// (week, batch, delete, gold, input mg, recovered mg, October day).
    Commit(Vec<(u8, u8, bool, bool, i64, i64, u8)>),
    Exclude(u8, u8),
    Restart,
}

fn step() -> impl proptest::strategy::Strategy<Value = Step> {
    use proptest::prelude::*;
    let write = (
        0u8..16,
        0u8..6,
        any::<bool>(),
        any::<bool>(),
        1i64..200_000,
        0i64..200_000,
        1u8..29,
    );
    prop_oneof![
        6 => proptest::collection::vec(write, 1..4).prop_map(Step::Commit),
        2 => (0u8..16, 0u8..6).prop_map(|(w, b)| Step::Exclude(w, b)),
        1 => Just(Step::Restart),
    ]
}

proptest::proptest! {
    #![proptest_config(proptest::test_runner::Config::with_cases(64))]
    #[test]
    fn norn_incremental_equals_reference_and_recompute(steps in proptest::collection::vec(step(), 1..24)) {
        let (base, plan, mut context) = fixture();
        let certificate = certificate::question(&plan);
        proptest::prop_assert!(certificate.algebraic.iter().all(|r| *r == Algebraic::ProvedAlgebraic));
        let model = base.model().clone();
        drop(base);
        let directory = tempfile::tempdir().expect("test directory");
        let path = directory.path().join("oracle.db");
        let mut store = CellStore::open(&path, model.clone(), norn_cell::StoreOptions::default()).expect("open");
        load(&mut store);
        let mut oracle = Oracle::new();
        for line in include_str!("../../../fixtures/demo/scrap-yield-k1.csv").lines().skip(1) {
            let f: Vec<_> = line.split(',').collect();
            oracle.put(f[2], epoch_day(f[8]), f[3] == "gold", f[4].parse().expect("input"), f[5].parse().expect("recovered"));
        }
        let mut view = IncrementalQuestion::rebuild(&store, &plan, context.clone(), |_| Ok(true)).expect("rebuild");
        proptest::prop_assert_eq!(norn_view(view.result()), oracle.view());
        for (index, step) in steps.into_iter().chain(std::iter::once(Step::Restart)).enumerate() {
            match step {
                Step::Commit(writes) => {
                    let mut seen = BTreeSet::new();
                    let mut batch = Vec::new();
                    for (w, b, delete, gold, input, recovered, day) in writes {
                        let key = format!("SYN-MELT-{w:02}-{b}");
                        if !seen.insert(key.clone()) { continue; }
                        if delete {
                            batch.push(FactWriteRow::Delete { object_type: TYPE.into(), key: key.clone() });
                            oracle.change(&key, |o| { o.rows.remove(&key); });
                        } else {
                            let at = format!("2026-10-{day:02}T12:00:00Z");
                            batch.push(put(&key, input, recovered, &at, if gold { "gold" } else { "silver" }));
                            oracle.put(&key, epoch_day(&at), gold, input, recovered);
                        }
                    }
                    let commit = CommitBatch { world: WORLD.into(), scope: "synthetic".into(), logical_time: CLOCK,
                        idempotency_key: Some(format!("oracle-{index}")), writes: batch, ..CommitBatch::default() };
                    store.commit(&commit).expect("change");
                    // Duplicate delivery is a receipt replay, never a second delta.
                    proptest::prop_assert!(matches!(store.commit(&commit).expect("redelivery"), CommitOutcome::Replayed(_)));
                }
                Step::Exclude(w, b) => {
                    let key = format!("SYN-MELT-{w:02}-{b}");
                    let on = !context.exclusions.contains_key(&key);
                    if on { context.exclusions.insert(key.clone(), "core07-finding".into()); } else { context.exclusions.remove(&key); }
                    oracle.change(&key, |o| { if on { o.excluded.insert(key.clone()); } else { o.excluded.remove(&key); } });
                }
                Step::Restart => {
                    drop(view);
                    drop(store);
                    store = CellStore::open(&path, model.clone(), norn_cell::StoreOptions::default()).expect("reopen");
                    view = IncrementalQuestion::rebuild(&store, &plan, context.clone(), |_| Ok(true)).expect("rebuild");
                }
            }
            let cursor = format!("fixture/{}", store.verified_head().expect("head").commits);
            context.source_cursors.get_mut("erp").expect("source").insert("measurements".into(), cursor.clone());
            context.watermarks.get_mut("erp").expect("source").get_mut("measurements").expect("partition").observed = cursor;
            view.advance(&store, context.clone(), |_| Ok(true)).expect("delta");
            let mut recompute = store.evaluate_question(&plan, context.clone(), |_| Ok(true)).expect("recompute");
            recompute.engine = Engine::Ivm;
            proptest::prop_assert_eq!(view.result(), &recompute);
            proptest::prop_assert_eq!(norn_view(view.result()), oracle.view());
        }
    }
}

/// The oracle must be able to fail: a dropped retraction diverges from Norn.
#[test]
fn oracle_detects_a_missing_retraction() {
    let mut oracle = Oracle::new();
    oracle.put("a", epoch_day("2026-10-13"), true, 1_000, 900);
    let before = oracle.view();
    // Update without retracting the old row: the Z-set double counts.
    oracle
        .pending
        .push(((0, epoch_day("2026-10-13"), true, 1_000, 950), 1));
    assert_ne!(oracle.view(), before);
    let expected = Decimal::parse("1.85").expect("double count");
    assert_eq!(oracle.view()["2026-10-12"].0, expected);
}
