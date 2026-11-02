//! Derived attempt identities deduplicate real SQLite effects across reopen.
#![allow(clippy::expect_used, missing_docs, reason = "integration assertions")]

use norn_cell::{CellStore, CommitBatch, CommitOutcome, Model, StoreOptions};
use norn_ir::RuntimeValue;
use norn_kernel::flow_identity::FlowStepIdentity;
use norn_testkit::proptest::prelude::*;

fn batch(attempt: u64) -> CommitBatch {
    let identity = FlowStepIdentity {
        scope: "tenant/dev",
        world: "main",
        flow: "test.retry",
        version: "1",
        instance: "run",
        step: "charge",
    };
    CommitBatch {
        world: "main".into(),
        scope: "tenant/dev".into(),
        logical_time: 10,
        idempotency_key: Some(identity.attempt_key(attempt).expect("key").0),
        events: vec![RuntimeValue::String(format!("attempt:{attempt}"))],
        ..CommitBatch::default()
    }
}

#[test]
fn reopen_replays_same_receipt_and_changed_payload_refuses() {
    let dir = tempfile::tempdir().expect("dir");
    let path = dir.path().join("cell.sqlite");
    let model = Model::from_definitions(&[], &[]).expect("model");
    let mut store = CellStore::open(&path, model.clone(), StoreOptions::default()).expect("store");
    store.bind("scope", "tenant/dev").expect("scope");
    let first = store.commit(&batch(0)).expect("commit");
    drop(store);
    let mut store = CellStore::open(&path, model, StoreOptions::default()).expect("reopen");
    let replay = store.commit(&batch(0)).expect("replay");
    assert!(matches!(replay, CommitOutcome::Replayed(_)));
    assert_eq!(first.receipt(), replay.receipt());
    let mut changed = batch(0);
    changed.events = vec![RuntimeValue::String("changed".into())];
    assert!(store.commit(&changed).is_err());
    assert_eq!(store.read().ledger(0).expect("ledger").len(), 1);
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(32))]
    #[test]
    fn generated_redeliveries_never_double_effect(attempts in prop::collection::vec(0_u64..8, 1..24)) {
        let mut store = CellStore::open_in_memory(Model::from_definitions(&[], &[]).expect("model")).expect("store");
        store.bind("scope", "tenant/dev").expect("scope");
        let mut seen = std::collections::BTreeSet::new();
        for attempt in attempts {
            let outcome = store.commit(&batch(attempt)).expect("commit");
            let fresh = seen.insert(attempt);
            prop_assert_eq!(matches!(outcome, CommitOutcome::Committed(_)), fresh);
        }
        prop_assert_eq!(store.read().ledger(0).expect("ledger").len(), seen.len());
    }
}
