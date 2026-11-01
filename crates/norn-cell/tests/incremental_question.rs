#![allow(
    clippy::expect_used,
    clippy::indexing_slicing,
    missing_docs,
    reason = "native query correspondence"
)]
mod query_support;
use norn_cell::compute::{Engine, ivm::IncrementalQuestion};
use norn_cell::{CellStore, CommitBatch, CommitOutcome, FactWriteRow, StoreOptions};
use query_support::*;

proptest::proptest! {
    #![proptest_config(proptest::test_runner::Config::with_cases(32))]
    #[test]
    fn data_delta_incremental_equals_recompute_with_redelivery_and_restart(
        changes in proptest::collection::vec((0u8..7, 0u8..6, 0i64..100_000, 12u8..27, proptest::bool::ANY), 1..32)
    ) {
        let (base, plan, mut context) = fixture();
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
            match kind {
                0..=3 => {
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
                4 => { context.exclusions.insert(key, "core07-finding".into()); }
                5 => { context.exclusions.remove(&key); }
                _ => {}
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
fn skipped_receipt_or_changed_population_refuses_without_partial_publication() {
    let (mut store, plan, mut context) = fixture();
    load(&mut store);
    let mut view = IncrementalQuestion::rebuild(&store, &plan, context.clone(), |_| Ok(true))
        .expect("rebuild");
    let before = view.result().clone();
    context.authorization_epoch = 2;
    assert!(view.advance(&store, context.clone(), |_| Ok(true)).is_err());
    assert_eq!(view.result(), &before);
    context.authorization_epoch = 1;
    for id in ["a", "b"] {
        store
            .commit(&CommitBatch {
                world: WORLD.into(),
                scope: "synthetic".into(),
                logical_time: CLOCK,
                writes: vec![put(id, 1_000, 980, "2026-10-23T00:00:00Z", "gold")],
                ..CommitBatch::default()
            })
            .expect("commit");
    }
    assert!(view.advance(&store, context, |_| Ok(true)).is_err());
    assert_eq!(view.result(), &before);
}
