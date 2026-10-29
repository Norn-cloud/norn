#![allow(
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    missing_docs,
    reason = "integration-test crate"
)]

//! Export journal persistence and atomicity checks.

mod common;

use std::path::Path;

use norn_cell::{
    CellError, CellStore, CommitBatch, CommitOutcome, CommitPoint, Consumption, EffectRow,
    JournalMode, MAIN_WORLD, OutboxCommand, StoreOptions, TimerEntry,
};
use norn_ir::RuntimeValue as Value;
use serde_json::json;

fn store_options() -> StoreOptions {
    StoreOptions {
        journal_mode: JournalMode::Delete,
        cache_pages: None,
    }
}

/// An in-memory `json!` value as an exact runtime value.
fn rv(value: serde_json::Value) -> Value {
    Value::from_json(value).expect("runtime value")
}

fn empty_batch(key: &str, event: Value) -> CommitBatch {
    CommitBatch {
        world: MAIN_WORLD.to_owned(),
        scope: "org_demo/dev".to_owned(),
        logical_time: 100,
        idempotency_key: Some(key.to_owned()),
        events: vec![event],
        ..CommitBatch::default()
    }
}

#[test]
fn journal_rows_are_atomic_idempotent_and_resume_after_reopen() {
    let dir = tempfile::tempdir().expect("temporary directory");
    let path = dir.path().join("cell.sqlite");
    let mut store =
        CellStore::open(&path, common::corpus_model(), store_options()).expect("store opens");

    let first_batch = empty_batch("turn-1", rv(json!({"type": "first"})));
    let first_receipt = store.commit(&first_batch).expect("first commit");
    let first = store
        .export_record(1)
        .expect("record reads")
        .expect("record exists");
    assert_eq!(&first.receipt, first_receipt.receipt());
    assert_eq!(first.entries.len(), 1);
    assert_eq!(first.entries[0].seq, 1);

    let replay = store.commit(&first_batch).expect("idempotent replay");
    assert!(matches!(replay, CommitOutcome::Replayed(_)));
    assert_eq!(store.export_record(2).expect("record reads"), None);

    let second_batch = empty_batch("turn-2", rv(json!({"type": "second"})));
    store.commit(&second_batch).expect("second commit");
    let second = store
        .export_record(2)
        .expect("record reads")
        .expect("record exists");
    assert_eq!(second.previous, first.hash().expect("first export hash"));
    drop(store);

    let reopened =
        CellStore::open(&path, common::corpus_model(), store_options()).expect("store reopens");
    assert_eq!(
        reopened.export_record(1).expect("record reads"),
        Some(first)
    );
    assert_eq!(
        reopened.export_record(2).expect("record reads"),
        Some(second)
    );
    assert_eq!(
        reopened
            .read()
            .verify_ledger()
            .expect("ledger verifies")
            .commits,
        2
    );
}

#[test]
fn a_failed_commit_rolls_back_its_export_record() {
    let mut store = CellStore::open_in_memory(common::corpus_model()).expect("store opens");
    let batch = empty_batch("turn-1", rv(json!({"type": "rolled-back"})));
    let result = store.commit_probed(&batch, &mut |point| {
        if point == CommitPoint::ReceiptWritten {
            Err(CellError::InjectedCrash(point))
        } else {
            Ok(())
        }
    });
    assert!(matches!(
        result,
        Err(CellError::InjectedCrash(CommitPoint::ReceiptWritten))
    ));
    assert_eq!(store.export_record(1).expect("record reads"), None);
    let head = store.read().verify_ledger().expect("ledger verifies");
    assert_eq!((head.seq, head.commits), (0, 0));
}

fn store_with_pending_deliveries() -> CellStore {
    let mut store = CellStore::open_in_memory(common::corpus_model()).expect("store opens");
    let initial = CommitBatch {
        effects: vec![
            EffectRow::Enqueue {
                command: OutboxCommand {
                    command_id: "command-1".to_owned(),
                    generation: 0,
                    idempotency_key: "command-1".to_owned(),
                    deadline: None,
                    command: rv(json!({"kind": "test"})),
                },
            },
            EffectRow::Schedule {
                timer: TimerEntry {
                    timer_id: "timer-1".to_owned(),
                    due: 500,
                    timer: rv(json!({"kind": "timer-input"})),
                },
            },
        ],
        ..empty_batch("turn-1", rv(json!({"type": "initial"})))
    };
    store.commit(&initial).expect("initial commit");
    store
        .record_performed("command-1", 0, &rv(json!({"ok": true})))
        .expect("result is recorded");
    store
        .inject(
            "observation-1",
            &rv(json!({"provenance": "fixture", "value": 7})),
        )
        .expect("observation is ingested");

    store
}

#[test]
fn committed_exports_capture_the_consumed_observation_result_and_timer() {
    let mut store = store_with_pending_deliveries();
    let result_commit = CommitBatch {
        consumes: Some(Consumption::Result {
            command_id: "command-1".to_owned(),
            generation: 0,
        }),
        ..empty_batch("turn-2", rv(json!({"type": "result-consumed"})))
    };
    store.commit(&result_commit).expect("result commit");
    let result = store
        .export_record(2)
        .expect("record reads")
        .expect("record exists");
    assert_eq!(
        result
            .consumed_input
            .as_ref()
            .and_then(|value| value.get("kind")),
        Some(&rv(json!("result")))
    );
    assert_eq!(
        result
            .consumed_input
            .as_ref()
            .and_then(|value| value.get("result")),
        Some(&rv(json!({"ok": true})))
    );
    assert_eq!(
        result
            .consumed_input
            .as_ref()
            .and_then(|value| value.get("runs"))
            .and_then(|value| value.as_array())
            .map(Vec::len),
        Some(1)
    );

    let timer_commit = CommitBatch {
        consumes: Some(Consumption::Timer {
            timer_id: "timer-1".to_owned(),
            incarnation: 1,
        }),
        ..empty_batch("turn-3", rv(json!({"type": "timer-consumed"})))
    };
    store.commit(&timer_commit).expect("timer commit");
    let timer = store
        .export_record(3)
        .expect("record reads")
        .expect("record exists");
    assert_eq!(
        timer
            .consumed_input
            .as_ref()
            .and_then(|value| value.get("kind")),
        Some(&rv(json!("timer")))
    );
    assert_eq!(
        timer
            .consumed_input
            .as_ref()
            .and_then(|value| value.get("timer"))
            .and_then(|value| value.get("timer")),
        Some(&rv(json!({"kind": "timer-input"})))
    );

    let observation_commit = CommitBatch {
        consumes: Some(Consumption::Observation {
            observation_id: "observation-1".to_owned(),
        }),
        ..empty_batch("turn-4", rv(json!({"type": "observation-consumed"})))
    };
    store
        .commit(&observation_commit)
        .expect("observation commit");
    let observation = store
        .export_record(4)
        .expect("record reads")
        .expect("record exists");
    assert_eq!(
        observation
            .consumed_input
            .as_ref()
            .and_then(|value| value.get("kind")),
        Some(&rv(json!("observation")))
    );
    assert_eq!(
        observation
            .consumed_input
            .as_ref()
            .and_then(|value| value.get("observed")),
        Some(&rv(json!({"provenance": "fixture", "value": 7})))
    );
}

#[test]
fn export_journal_reports_a_missing_legacy_batch_instead_of_faking_it() {
    let dir = tempfile::tempdir().expect("temporary directory");
    let path = dir.path().join("legacy.sqlite");
    create_legacy_store_with_receipt(&path);
    let mut store = CellStore::open(&path, common::corpus_model(), store_options())
        .expect("legacy store opens");

    // The old receipt remains visible, but there is no batch payload to export.
    assert_eq!(store.export_record(1).expect("record reads"), None);
    let event = rv(json!({"type": "after-upgrade"}));
    store
        .commit(&empty_batch("turn-2", event))
        .expect("new writes remain available");
    let tail = store
        .export_record(2)
        .expect("record reads")
        .expect("new record exists");
    assert!(
        tail.previous.is_empty(),
        "the missing predecessor stays explicit"
    );
    assert_eq!(store.export_record(1).expect("record reads"), None);
}

fn create_legacy_store_with_receipt(path: &Path) {
    {
        let mut store =
            CellStore::open(path, common::corpus_model(), store_options()).expect("store opens");
        store
            .commit(&empty_batch("old-turn", rv(json!({"type": "legacy"}))))
            .expect("legacy commit");
    }
    rusqlite::Connection::open(path)
        .expect("legacy image opens")
        .execute_batch("DROP TABLE \"_norn_export_journal\"")
        .expect("remove journal for legacy image");
}

#[test]
fn unicode_export_preserves_nfc_equivalence_rather_than_original_spelling() {
    let mut store = CellStore::open_in_memory(common::corpus_model()).expect("store opens");
    let decomposed = rv(json!({"label":"e\u{301}"}));
    let composed = rv(json!({"label":"é"}));
    assert_ne!(decomposed, composed, "submitted spellings differ");
    store
        .commit(&empty_batch("unicode", decomposed.clone()))
        .expect("commit");
    let record = store
        .export_record(1)
        .expect("export reads")
        .expect("record");
    assert_eq!(record.format, norn_cell::export::EXPORT_FORMAT);
    assert_eq!(record.batch.events, vec![composed.clone()]);
    assert_eq!(record.entries[0].event, composed);
    let decoded: norn_cell::export::ExportRecord =
        serde_json::from_slice(&record.bytes().expect("bytes")).expect("decode");
    assert_eq!(decoded, record);
    let mut original_spelling = record.clone();
    original_spelling.batch.events[0] = decomposed;
    assert_eq!(
        original_spelling.hash().expect("hash"),
        record.hash().expect("hash")
    );
    assert_eq!(
        original_spelling.bytes().expect("bytes"),
        record.bytes().expect("bytes")
    );
}
