#![allow(
    clippy::expect_used,
    clippy::indexing_slicing,
    missing_docs,
    reason = "native query correspondence"
)]
mod query_support;
use norn_cell::compute::{Engine, ivm::IncrementalQuestion};
use norn_cell::engine::{NativeEngine, SqlEngine, SqlValue};
use norn_cell::{CellError, CellStore, CommitBatch, CommitOutcome, FactWriteRow, StoreOptions};
use query_support::*;
use std::{cell::Cell, rc::Rc, time::Instant};

struct CountedEngine(NativeEngine, Rc<Cell<(usize, usize)>>);
impl SqlEngine for CountedEngine {
    fn execute(&self, sql: &str, p: &[SqlValue]) -> Result<(), CellError> {
        self.0.execute(sql, p)
    }
    fn execute_batch(&self, sql: &str) -> Result<(), CellError> {
        self.0.execute_batch(sql)
    }
    fn transaction<T, X: From<CellError>>(&self, f: impl FnOnce() -> Result<T, X>) -> Result<T, X> {
        self.0.transaction(f)
    }
    fn read_cache_revision(&self) -> Result<Option<(u64, u64)>, CellError> {
        self.0.read_cache_revision()
    }
    fn query(&self, sql: &str, p: &[SqlValue]) -> Result<Vec<Vec<SqlValue>>, CellError> {
        let (queries, findings) = self.1.get();
        self.1.set((
            queries.checked_add(1).expect("bounded queries"),
            findings
                .checked_add(usize::from(sql.contains("$.fact.subjects[0].objectType")))
                .expect("bounded lookups"),
        ));
        self.0.query(sql, p)
    }
}

// QUERY-ENGINE-1: eager advance only; seed, commit and canonical oracle are untimed.
// In-memory, single-threaded, at most 80 members (plan cap 100), no source disputes.
// Member cloning/lineage and rechecking the source index remain unoptimized costs.
#[allow(clippy::print_stdout, reason = "explicit bounded benchmark output")]
fn measure_advance(members: usize, changed: bool) -> usize {
    let (base, plan, context) = fixture();
    let counts = Rc::new(Cell::new((0, 0)));
    let mut store = CellStore::from_engine(
        CountedEngine(
            NativeEngine::open_in_memory().expect("engine"),
            Rc::clone(&counts),
        ),
        base.model().clone(),
    )
    .expect("store");
    store
        .commit(&CommitBatch {
            world: WORLD.into(),
            scope: "synthetic".into(),
            logical_time: CLOCK,
            writes: fixture_writes().into_iter().take(members).collect(),
            ..CommitBatch::default()
        })
        .expect("seed");
    let mut view = IncrementalQuestion::rebuild(&store, &plan, context.clone(), |_| Ok(true))
        .expect("rebuild");
    let writes = if changed {
        vec![put(
            "SYN-MELT-00-0",
            50_000,
            48_999,
            "2026-07-10T12:00:00Z",
            "gold",
        )]
    } else {
        vec![]
    };
    store
        .commit(&CommitBatch {
            world: WORLD.into(),
            scope: "synthetic".into(),
            logical_time: CLOCK,
            writes,
            // An unrelated unpinned finding must remain only an invalidation hint.
            events: if changed { vec![] } else { vec![norn_ir::runtime::encode(&serde_json::json!({"op":"data_quality_finding","fact":{"subjects":[{"objectType":TYPE,"key":"unrelated"}]}})).expect("unrelated event")] },
            ..CommitBatch::default()
        })
        .expect("receipt");
    assert_eq!(
        store
            .verified_head()
            .expect("head")
            .commits
            .checked_sub(view.result().revision.commits),
        Some(1)
    );
    counts.set((0, 0));
    let mut evaluated = 0usize;
    let start = Instant::now();
    std::hint::black_box(
        view.advance(&store, context.clone(), |_| {
            evaluated = evaluated.checked_add(1).expect("bounded members");
            Ok(true)
        })
        .expect("delta"),
    );
    let elapsed = start.elapsed();
    let (queries, findings) = counts.get();
    let mut exact = store
        .evaluate_question(&plan, context, |_| Ok(true))
        .expect("oracle");
    exact.engine = Engine::Ivm;
    assert_eq!(view.result(), &exact);
    println!(
        "members={members} changed={changed} elapsed_us={} evaluated={evaluated} queries={queries} finding_lookups={findings} successful_receipts=1 errors=0",
        elapsed.as_micros()
    );
    assert_eq!(evaluated, findings);
    findings
}

#[test]
fn finding_lookups_are_limited_to_changed_receipt_members() {
    assert_eq!(measure_advance(80, false), 0);
    assert_eq!(measure_advance(80, true), 1);
}

#[test]
#[ignore = "bounded release benchmark; run explicitly, compare identical harness on base"]
fn bounded_finding_lookup_benchmark() {
    for members in [8, 80] {
        for changed in [false, true] {
            measure_advance(members, changed);
        }
    }
}

#[test]
fn unchanged_receipt_preserves_quarantine_without_rechecking_members() {
    use norn_kernel::{CellHost, cell_host::CellCommit};
    let (mut host, plan, context) = quality_fixture();
    capture_quality(&mut host, 80_000, "invalid");
    let mut view = IncrementalQuestion::rebuild(host.store(), &plan, context.clone(), |_| Ok(true))
        .expect("quarantined generation");
    assert_eq!(view.result().exclusions.len(), 1);
    host.commit(CellCommit::empty()).expect("unrelated receipt");
    assert!(host.store().verified_head().expect("head").commits > view.result().revision.commits);
    view.advance(host.store(), context.clone(), |_| {
        Err(CellError::ComputeRefused("test.unchanged_member_evaluated"))
    })
    .expect("unchanged generation");
    let mut exact = host
        .store()
        .evaluate_question(&plan, context, |_| Ok(true))
        .expect("oracle");
    exact.engine = Engine::Ivm;
    assert_eq!(view.result(), &exact);
}

#[test]
fn same_frontier_untrusted_source_evidence_refuses_atomically_like_recompute() {
    let (mut store, plan, context) = fixture();
    load(&mut store);
    let mut view = IncrementalQuestion::rebuild(&store, &plan, context.clone(), |_| Ok(true))
        .expect("generation");
    let before = view.result().clone();
    let frontier = store.verified_head().expect("head");
    store
        .inject("mapping-identity:malformed", &norn_ir::RuntimeValue::Null)
        .expect("inbox only");
    assert_eq!(store.verified_head().expect("same frontier"), frontier);
    let exact = store
        .evaluate_question(&plan, context.clone(), |_| Ok(true))
        .expect_err("untrusted source evidence");
    let delta = view
        .advance(&store, context, |_| Ok(true))
        .expect_err("same source refusal");
    assert_eq!(delta.to_string(), exact.to_string());
    assert_eq!(view.result(), &before);
}

proptest::proptest! {
    #![proptest_config(proptest::test_runner::Config::with_cases(32))]
    #[test]
    fn data_delta_incremental_equals_recompute_with_redelivery_and_restart(
        changes in proptest::collection::vec((0u8..7, 0u8..6, 0i64..100_000, 12u8..27, proptest::bool::ANY), 1..32)
    ) {
        let (base, plan, mut context) = fixture();
        assert!(norn_checker::metric::compute::certificate::question(&plan).algebraic.iter().all(|result| *result == norn_checker::metric::compute::certificate::Algebraic::ProvedAlgebraic));
        let model = base.model().clone();
        drop(base);
        let directory = tempfile::tempdir().expect("test directory");
        let path = directory.path().join("query.db");
        let mut store = CellStore::open(&path, model.clone(), StoreOptions::default()).expect("open");
        load(&mut store);
        let mut view = IncrementalQuestion::rebuild(&store, &plan, context.clone(), |_| Ok(true)).expect("initial complete rebuild");
        proptest::prop_assert!(view.was_rebuilt());
        for (step, (kind, id, amount, day, restart)) in changes.into_iter().enumerate() {
            let key = format!("SYN-MELT-15-{id}");
            if kind <= 3 {
                let write = if kind == 3 {
                    FactWriteRow::Delete { object_type: TYPE.into(), key }
                } else {
                    put(&key, 75_000, amount, &format!("2026-10-{day:02}T12:00:00Z"), if kind == 2 { "silver" } else { "gold" })
                };
                let batch = CommitBatch { world: WORLD.into(), scope: "synthetic".into(), logical_time: CLOCK,
                    idempotency_key: Some(format!("generated-{step}")), writes: vec![write], ..CommitBatch::default() };
                store.commit(&batch).expect("change");
                proptest::prop_assert!(matches!(store.commit(&batch).expect("redelivery"), CommitOutcome::Replayed(_)));
            }
            let cursor = format!("fixture/{}", store.verified_head().expect("head").commits);
            context.source_cursors.get_mut("erp").expect("source").insert("measurements".into(), cursor.clone());
            context.watermarks.get_mut("erp").expect("source").get_mut("measurements").expect("partition").observed = cursor;
            view.advance(&store, context.clone(), |_| Ok(true)).expect("delta");
            let mut reference = store.evaluate_question(&plan, context.clone(), |_| Ok(true)).expect("full recompute");
            reference.engine = Engine::Ivm;
            proptest::prop_assert_eq!(view.result(), &reference);
            let redelivered = view.advance(&store, context.clone(), |_| Ok(true)).expect("same frontier");
            proptest::prop_assert_eq!(redelivered, &reference);
            if restart {
                drop(view);
                drop(store);
                store = CellStore::open(&path, model.clone(), StoreOptions::default()).expect("reopen");
                view = IncrementalQuestion::rebuild(&store, &plan, context.clone(), |_| Ok(true)).expect("labelled complete rebuild");
                proptest::prop_assert!(view.was_rebuilt());
                proptest::prop_assert_eq!(view.result(), &reference);
            }
        }
        drop(view);
        drop(store);
        let reopened = CellStore::open(&path, model, StoreOptions::default()).expect("mandatory restart");
        let rebuilt = IncrementalQuestion::rebuild(&reopened, &plan, context.clone(), |_| Ok(true)).expect("restart");
        let mut reference = reopened.evaluate_question(&plan, context, |_| Ok(true)).expect("recompute");
        reference.engine = Engine::Ivm;
        proptest::prop_assert_eq!(rebuilt.result(), &reference);
    }
}

#[test]
fn empty_generation_refuses_a_new_foreign_scope_binding() {
    let (mut store, plan, context) = fixture();
    let mut view = IncrementalQuestion::rebuild(&store, &plan, context.clone(), |_| Ok(true))
        .expect("rebuild");
    let before = view.result().clone();
    store.bind("scope", "other").expect("bound scope");
    assert!(matches!(
        view.advance(&store, context, |_| Ok(true)),
        Err(CellError::ComputeRefused("core.compute.stale_input"))
    ));
    assert_eq!(view.result(), &before);
}
#[test]
fn skipped_receipt_or_changed_population_refuses_without_partial_publication() {
    let (mut store, plan, mut context) = fixture();
    load(&mut store);
    let mut view = IncrementalQuestion::rebuild(&store, &plan, context.clone(), |_| Ok(true))
        .expect("rebuild");
    let before = view.result().clone();
    context.authorization_epoch = 2;
    assert!(matches!(
        view.advance(&store, context.clone(), |_| Ok(true)),
        Err(CellError::ComputeRefused("core.compute.stale_input"))
    ));
    assert_eq!(view.result(), &before);
    context.authorization_epoch = 1;
    let at = "2026-10-23T00:00:00Z";
    store
        .commit(&CommitBatch {
            world: WORLD.into(),
            scope: "synthetic".into(),
            logical_time: CLOCK,
            writes: vec![put("SYN-MELT-15-0", 75_000, 70_000, at, "gold")],
            ..CommitBatch::default()
        })
        .expect("one correction");
    assert!(matches!(
        view.advance(&store, context.clone(), |_| Err(CellError::ComputeRefused(
            "test.refusal"
        ))),
        Err(CellError::ComputeRefused("test.refusal"))
    ));
    assert_eq!(view.result(), &before);
    view.advance(&store, context.clone(), |_| Ok(true))
        .expect("retry the same receipt");
    let before = view.result().clone();
    for id in ["a", "b"] {
        store
            .commit(&CommitBatch {
                world: WORLD.into(),
                scope: "synthetic".into(),
                logical_time: CLOCK,
                writes: vec![put(id, 1_000, 980, at, "gold")],
                ..CommitBatch::default()
            })
            .expect("commit");
    }
    assert!(matches!(
        view.advance(&store, context, |_| Ok(true)),
        Err(CellError::ComputeRefused("core.compute.stale_input"))
    ));
    assert_eq!(view.result(), &before);
}
