#![cfg(feature = "native")]
#![allow(clippy::expect_used, missing_docs, reason = "integration-test crate")]

mod common;

use norn_cell::engine::{MAX_SAFE_INTEGER, NativeEngine, SqlEngine, SqlValue};
use norn_cell::store::MAX_OBSERVATION_BYTES;
use norn_cell::{
    CellError, CellStore, CommitBatch, EffectRow, MAIN_WORLD, OutboxCommand, StoreOptions,
};
use norn_ir::RuntimeValue;

fn enqueue(id: &str, generation: u64) -> CommitBatch {
    CommitBatch {
        world: MAIN_WORLD.to_owned(),
        scope: "org_demo/dev".to_owned(),
        events: vec![RuntimeValue::String(id.to_owned())],
        effects: vec![EffectRow::Enqueue {
            command: OutboxCommand {
                command_id: id.to_owned(),
                generation,
                idempotency_key: id.to_owned(),
                deadline: None,
                command: RuntimeValue::Null,
            },
        }],
        ..CommitBatch::default()
    }
}

#[test]
fn queue_positions_at_the_exact_integer_limit_refuse_without_mutation() {
    let directory = tempfile::tempdir().expect("directory");
    let path = directory.path().join("cell.sqlite");
    let model = common::corpus_model();
    let mut store = CellStore::open(&path, model.clone(), StoreOptions::default()).expect("open");
    store.commit(&enqueue("seed", 0)).expect("seed command");
    store
        .inject("seed", &RuntimeValue::Null)
        .expect("seed inbox");
    drop(store);

    let engine = NativeEngine::open(&path, StoreOptions::default()).expect("engine");
    let maximum = MAX_SAFE_INTEGER.checked_sub(1).expect("below boundary");
    for table in ["_norn_inbox", "_norn_outbox"] {
        engine
            .execute(
                &format!("UPDATE \"{table}\" SET \"position\" = ?1"),
                &[SqlValue::Integer(maximum)],
            )
            .expect("seed exact high-water mark");
    }
    drop(engine);

    let mut store = CellStore::open(&path, model.clone(), StoreOptions::default()).expect("reopen");
    store
        .commit(&enqueue("last", 0))
        .expect("last exact position");
    assert!(
        store
            .inject("last", &RuntimeValue::Null)
            .expect("last inbox position")
    );
    let commands = store.read().pending_commands().expect("commands");
    let observations = store.read().pending_observations().expect("observations");
    let bound = u64::try_from(MAX_SAFE_INTEGER).expect("positive bound");
    assert_eq!(commands.last().expect("last command").position, bound);
    assert_eq!(
        observations.last().expect("last observation").position,
        bound
    );
    let ledger = store.read().ledger(0).expect("ledger");
    let head = store.read().verify_ledger().expect("head");

    assert!(matches!(
        store.commit(&enqueue("overflow", 0)),
        Err(CellError::OutOfRange("outbox position"))
    ));
    assert!(matches!(
        store.inject("overflow", &RuntimeValue::Null),
        Err(CellError::OutOfRange("inbox position"))
    ));
    assert_eq!(store.read().pending_commands().expect("commands"), commands);
    assert_eq!(
        store.read().pending_observations().expect("observations"),
        observations
    );
    assert_eq!(store.read().ledger(0).expect("ledger"), ledger);
    assert_eq!(store.read().verify_ledger().expect("head"), head);

    // Dedupe and pending generation replacement allocate no new position.
    assert!(
        !store
            .inject("seed", &RuntimeValue::Null)
            .expect("duplicate")
    );
    store
        .commit(&enqueue("seed", 1))
        .expect("replace pending generation");
    drop(store);
    let reopened = CellStore::open(&path, model, StoreOptions::default()).expect("reopen again");
    assert_eq!(
        reopened.read().pending_commands().expect("commands").len(),
        2
    );
    assert_eq!(
        reopened
            .read()
            .pending_observations()
            .expect("observations"),
        observations
    );
    reopened
        .read()
        .verify_ledger()
        .expect("valid after refusal");
}

#[test]
fn canonical_observation_bytes_are_bounded_before_inbox_persistence() {
    let mut store = CellStore::open_in_memory(common::corpus_model()).expect("store");
    // Canonical JSON string quotes contribute two bytes to the row payload.
    let content_bytes = MAX_OBSERVATION_BYTES.checked_sub(2).expect("quotes");
    let at_limit = RuntimeValue::String("x".repeat(content_bytes));
    assert!(store.inject("exact", &at_limit).expect("exact cap"));
    let before = store.read().pending_observations().expect("inbox");
    let above_limit = RuntimeValue::String("x".repeat(content_bytes.saturating_add(1)));
    assert!(matches!(
        store.inject("above", &above_limit),
        Err(CellError::OutOfRange("observation bytes"))
    ));
    assert_eq!(store.read().pending_observations().expect("inbox"), before);
    assert_eq!(store.read().verify_ledger().expect("ledger").commits, 0);
}
