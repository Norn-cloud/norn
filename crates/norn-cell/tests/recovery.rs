#![allow(
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::arithmetic_side_effects,
    missing_docs,
    reason = "integration-test crate"
)]

//! T2-ST-2 (#2541): the ledger, receipt, outbox, timer, and run tables; and
//! deterministic crash-mid-commit simulation over the tables: a crash at any
//! point of a commit leaves a file that reopens to exactly the state before
//! the commit (or, after `COMMIT`, exactly the state after it), with a ledger
//! hash chain that verifies.

mod common;

use std::collections::BTreeMap;
use std::path::Path;

use norn_cell::{
    CellError, CellStore, CommitBatch, CommitOutcome, CommitPoint, Consumption, EffectRow,
    FactWriteRow, JournalMode, LedgerHead, MAIN_WORLD, Model, OutboxCommand, StoreOptions,
    TimerEntry,
};
use norn_ir::{FactChange, RuntimeValue as Value};
use norn_projections::{PropertyColumn, ValueEncoding};
use serde_json::json;

fn employee(columns: &[PropertyColumn], key: &str, number: u64) -> FactChange {
    let values: BTreeMap<String, Value> = columns
        .iter()
        .map(|column| {
            let value = match column.encoding {
                ValueEncoding::Integer => rv(json!(number)),
                ValueEncoding::Decimal => rv(json!("1.00")),
                ValueEncoding::Boolean => rv(json!(false)),
                ValueEncoding::Json => rv(json!({})),
                ValueEncoding::Text => rv(json!(format!("{}-{number}", column.property.as_str()))),
            };
            (column.property.as_str().to_owned(), value)
        })
        .collect();
    serde_json::from_value(json!({
        "op": "create",
        "objectType": "hr.Employee",
        "key": key,
        "values": values,
    }))
    .expect("fact decodes")
}

/// A deterministic batch: `size` new employees, one event per employee, a
/// command, and a timer.
fn batch(round: u64, size: u64) -> CommitBatch {
    let columns = model()
        .schema()
        .object_table("hr.Employee")
        .expect("table")
        .columns
        .clone();
    let start = round * 1_000;
    CommitBatch {
        world: MAIN_WORLD.to_owned(),
        scope: "org_demo/dev".to_owned(),
        logical_time: 10_000 + round,
        idempotency_key: Some(format!("turn-{round}")),
        events: (start..start + size)
            .map(|n| rv(json!({ "type": "employee.hired", "n": n })))
            .collect(),
        facts: (start..start + size)
            .map(|n| employee(&columns, &format!("E{n}"), n))
            .collect(),
        effects: vec![
            EffectRow::Enqueue {
                command: OutboxCommand {
                    command_id: format!("notify-{round}"),
                    generation: 0,
                    idempotency_key: format!("notify-{round}"),
                    deadline: Some(20_000 + round),
                    command: rv(json!({ "kind": "notify", "round": round })),
                },
            },
            EffectRow::Schedule {
                timer: TimerEntry {
                    timer_id: format!("review-{round}"),
                    due: 50_000 + round,
                    timer: rv(json!({ "kind": "review", "round": round })),
                },
            },
        ],
        ..CommitBatch::default()
    }
}

/// An in-memory `json!` value as an exact runtime value.
fn rv(value: serde_json::Value) -> Value {
    Value::from_json(value).expect("runtime value")
}

fn model() -> Model {
    common::corpus_model()
}

#[test]
fn commits_chain_receipts_and_enqueue_atomically() {
    let mut store = CellStore::open_in_memory(model()).expect("opens");
    let first = store.commit(&batch(1, 3)).expect("commits");
    let second = store.commit(&batch(2, 2)).expect("commits");
    let CommitOutcome::Committed(first) = first else {
        panic!("committed")
    };
    let CommitOutcome::Committed(second) = second else {
        panic!("committed")
    };
    assert_eq!(first.body.commit_seq, 1);
    assert_eq!(first.body.first_event_seq, 1);
    assert_eq!(first.body.event_count, 3);
    assert_eq!(second.body.first_event_seq, 4);
    assert_eq!(second.body.enqueued, ["notify-2"]);

    let ledger = store.read().ledger(0).expect("ledger");
    assert_eq!(ledger.len(), 5);
    assert_eq!(ledger[0].prev_hash, norn_cell::system::GENESIS_HASH);
    for pair in ledger.windows(2) {
        assert_eq!(pair[1].prev_hash, pair[0].hash);
    }
    assert_eq!(second.body.ledger_head, ledger[4].hash);

    // Bounded pages (#2970): `seq > after`, at most `limit`.
    let page = store.read().ledger_page(1, 2).expect("page");
    assert_eq!(page, ledger[1..3]);
    assert_eq!(store.read().ledger_page(4, 10).expect("tail"), ledger[4..]);
    assert!(store.read().ledger_page(5, 10).expect("end").is_empty());
    assert!(store.read().ledger_page(0, 0).expect("empty").is_empty());
    assert!(
        store
            .read()
            .ledger_page(u64::MAX, 10)
            .expect("past the end")
            .is_empty()
    );
    assert!(
        store
            .read()
            .ledger_page(u64::try_from(i64::MAX).unwrap(), 10)
            .expect("at the bound")
            .is_empty()
    );
    let head = store.read().verify_ledger().expect("verifies");
    assert_eq!((head.seq, head.commits), (5, 2));

    let pending = store.read().pending_commands().expect("outbox");
    assert_eq!(
        pending
            .iter()
            .map(|row| (row.commit_seq, row.command.command_id.as_str()))
            .collect::<Vec<_>>(),
        [(1, "notify-1"), (2, "notify-2")]
    );
    let timers = store.read().pending_timers().expect("timers");
    assert_eq!(timers[0].timer.timer_id, "review-1", "earliest due first");
}

#[test]
fn an_idempotent_retry_replays_and_a_conflicting_one_is_refused() {
    let mut store = CellStore::open_in_memory(model()).expect("opens");
    let committed = store.commit(&batch(1, 2)).expect("commits");
    let before = store.read().dump().expect("dumps");
    let replayed = store.commit(&batch(1, 2)).expect("replays");
    assert!(matches!(replayed, CommitOutcome::Replayed(_)));
    assert_eq!(replayed.receipt(), committed.receipt());
    assert_eq!(
        store.read().dump().expect("dumps"),
        before,
        "a replay writes nothing"
    );

    let mut conflicting = batch(1, 2);
    conflicting.logical_time += 1;
    assert!(matches!(
        store.commit(&conflicting),
        Err(CellError::IdempotencyConflict(_))
    ));
}

#[test]
fn a_performed_result_is_recorded_once_and_consumed_atomically() {
    let mut store = CellStore::open_in_memory(model()).expect("opens");
    store.commit(&batch(1, 1)).expect("commits");
    assert!(
        store
            .record_performed("notify-1", 0, &rv(json!({ "ok": true })))
            .expect("records")
    );
    // A second recording (a redelivery) executes and records nothing.
    assert!(
        !store
            .record_performed("notify-1", 0, &rv(json!({ "ok": false })))
            .expect("records")
    );
    assert_eq!(store.read().executions().expect("runs"), 1);
    let head = store.read().head_command().expect("outbox").expect("head");
    assert_eq!(head.result, Some(rv(json!({ "ok": true }))));

    let consume = |consumes: Consumption| CommitBatch {
        world: MAIN_WORLD.to_owned(),
        scope: "org_demo/dev".to_owned(),
        logical_time: 60_000,
        consumes: Some(consumes),
        events: vec![rv(json!({ "type": "consumed" }))],
        ..CommitBatch::default()
    };
    store
        .commit(&consume(Consumption::Result {
            command_id: "notify-1".to_owned(),
            generation: 0,
        }))
        .expect("commits");
    store
        .commit(&consume(Consumption::Timer {
            timer_id: "review-1".to_owned(),
            incarnation: 1,
        }))
        .expect("commits");
    assert!(store.read().pending_commands().expect("outbox").is_empty());
    assert!(store.read().pending_timers().expect("timers").is_empty());
    let runs = store.read().runs("notify-1").expect("runs");
    assert_eq!(runs.len(), 1);
    assert_eq!((runs[0].attempt, runs[0].consumed_commit_seq), (1, Some(2)));

    // Consuming it again is refused, atomically.
    let before = store.read().dump().expect("dumps");
    assert!(matches!(
        store.commit(&consume(Consumption::Result {
            command_id: "notify-1".to_owned(),
            generation: 0,
        })),
        Err(CellError::NotPending(_))
    ));
    assert_eq!(store.read().dump().expect("dumps"), before);
}

#[test]
fn deletes_leave_tombstones_so_versions_never_repeat() {
    let mut store = CellStore::open_in_memory(model()).expect("opens");
    let columns = model()
        .schema()
        .object_table("hr.Employee")
        .expect("table")
        .columns
        .clone();
    let FactChange::Create { values, .. } = employee(&columns, "E1", 1) else {
        panic!("create")
    };
    let values: BTreeMap<String, Value> = values;
    let write = |row: FactWriteRow| CommitBatch {
        world: MAIN_WORLD.to_owned(),
        scope: "org_demo/dev".to_owned(),
        writes: vec![row],
        ..CommitBatch::default()
    };
    let put = || FactWriteRow::Put {
        object_type: "hr.Employee".to_owned(),
        key: "E1".to_owned(),
        values: values.clone(),
    };
    let delete = FactWriteRow::Delete {
        object_type: "hr.Employee".to_owned(),
        key: "E1".to_owned(),
    };
    let version = |store: &CellStore| {
        store
            .read()
            .version(MAIN_WORLD, "hr.Employee", "E1")
            .expect("version")
    };
    assert_eq!(version(&store), 0);
    let receipt = store.commit(&write(put())).expect("creates");
    assert_eq!(receipt.receipt().body.written[0].version, 1);
    store.commit(&write(put())).expect("replaces");
    assert_eq!(version(&store), 2);
    store.commit(&write(delete)).expect("deletes");
    assert_eq!(version(&store), 3, "the tombstone");
    assert_eq!(
        store
            .read()
            .object(MAIN_WORLD, "hr.Employee", "E1")
            .expect("reads"),
        None
    );
    store.commit(&write(put())).expect("recreates");
    assert_eq!(
        version(&store),
        4,
        "a recreate continues after the tombstone"
    );
}

/// A file store with rounds 1 (3 events) and 2 (2 events), tampered with
/// `sql` behind the store's back; reopening must refuse it.
#[expect(
    clippy::unwrap_in_result,
    reason = "This test fixture must panic on failed prerequisites rather than simulate a recoverable domain refusal"
)]
fn reopen_after(sql: &str) -> Result<CellStore, CellError> {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("cell.sqlite");
    let mut store = CellStore::open(&path, model(), StoreOptions::default()).expect("opens");
    store.commit(&batch(1, 3)).expect("commits");
    store.commit(&batch(2, 2)).expect("commits");
    drop(store);
    let raw = rusqlite::Connection::open(&path).expect("raw open");
    raw.execute(sql, []).expect("tampers");
    drop(raw);
    CellStore::open(&path, model(), StoreOptions::default())
}

#[test]
fn a_tampered_ledger_is_refused_on_reopen() {
    let reopened = reopen_after(
        "UPDATE \"_norn_ledger\" SET \"event\" = '{\"type\":\"forged\"}' WHERE \"seq\" = 2",
    );
    assert!(
        matches!(reopened, Err(CellError::LedgerCorrupt { seq: 2, .. })),
        "{reopened:?}"
    );
}

#[test]
fn the_ledger_verifies_incrementally_from_a_checkpoint() {
    // #2975: a reader that keeps the last verified head checks only the
    // suffix committed after it, and gets the same head a full pass would.
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("cell.sqlite");
    let mut store = CellStore::open(&path, model(), StoreOptions::default()).expect("opens");
    assert_eq!(
        store.verified_head().expect("verifies"),
        LedgerHead::genesis()
    );
    store.commit(&batch(1, 3)).expect("commits");
    let checkpoint = store.read().verify_ledger().expect("verifies");
    assert_eq!(store.verified_head().expect("verifies"), checkpoint);
    store.commit(&batch(2, 2)).expect("commits");
    store.commit(&batch(3, 1)).expect("commits");
    let full = store.read().verify_ledger().expect("verifies");
    assert_eq!((full.seq, full.commits), (6, 3));
    assert_eq!(
        store
            .read()
            .verify_ledger_from(&checkpoint)
            .expect("suffix"),
        full
    );
    assert_eq!(
        store
            .read()
            .verify_ledger_from(&full)
            .expect("empty suffix"),
        full
    );
    assert_eq!(
        store
            .read()
            .verify_ledger_from(&LedgerHead::genesis())
            .expect("from genesis"),
        full
    );
    assert_eq!(store.verified_head().expect("verifies"), full);

    // A checkpoint the file does not contain is refused.
    for forged in [
        LedgerHead {
            hash: "0".repeat(64),
            ..checkpoint.clone()
        },
        LedgerHead {
            commits: 2,
            ..checkpoint.clone()
        },
        LedgerHead {
            seq: full.seq + 1,
            ..full
        },
        LedgerHead {
            seq: 0,
            commits: 1,
            ..LedgerHead::genesis()
        },
    ] {
        let refused = store.read().verify_ledger_from(&forged);
        assert!(
            matches!(refused, Err(CellError::LedgerCorrupt { .. })),
            "{forged:?}: {refused:?}"
        );
    }

    // A head after commits that appended no events (genesis hash, commits
    // counted) is a checkpoint too.
    let mut quiet = CellStore::open_in_memory(model()).expect("opens");
    quiet.commit(&batch(1, 0)).expect("commits");
    let empty = quiet.read().verify_ledger().expect("verifies");
    assert_eq!((empty.seq, empty.commits), (0, 1));
    quiet.commit(&batch(2, 2)).expect("commits");
    assert_eq!(
        quiet.read().verify_ledger_from(&empty).expect("suffix"),
        quiet.read().verify_ledger().expect("verifies")
    );

    // Tampering after the checkpoint is caught by the suffix check; a
    // remembered head that no longer anchors the file falls back to a full
    // verification, which refuses it too.
    let raw = rusqlite::Connection::open(&path).expect("raw open");
    raw.execute(
        "UPDATE \"_norn_ledger\" SET \"event\" = '{\"type\":\"forged\"}' WHERE \"seq\" = 5",
        [],
    )
    .expect("tampers");
    let refused = store.read().verify_ledger_from(&checkpoint);
    assert!(
        matches!(refused, Err(CellError::LedgerCorrupt { seq: 5, .. })),
        "{refused:?}"
    );
    raw.execute(
        "UPDATE \"_norn_ledger\" SET \"hash\" = 'forged' WHERE \"seq\" = 6",
        [],
    )
    .expect("tampers");
    drop(raw);
    let refused = store.verified_head();
    assert!(
        matches!(refused, Err(CellError::LedgerCorrupt { .. })),
        "{refused:?}"
    );
}

#[test]
fn receipt_lookup_columns_must_match_the_hashed_receipt() {
    // A cleared idempotency key would let the same turn commit twice.
    let reopened = reopen_after(
        "UPDATE \"_norn_receipt\" SET \"idempotency_key\" = NULL WHERE \"commit_seq\" = 1",
    );
    assert!(
        matches!(reopened, Err(CellError::LedgerCorrupt { .. })),
        "{reopened:?}"
    );
    // A renumbered receipt would skew every later commit number.
    let reopened =
        reopen_after("UPDATE \"_norn_receipt\" SET \"commit_seq\" = 99 WHERE \"commit_seq\" = 2");
    assert!(
        matches!(reopened, Err(CellError::LedgerCorrupt { .. })),
        "{reopened:?}"
    );
}

#[test]
fn a_negatively_numbered_receipt_is_refused_on_reopen() {
    let reopened = reopen_after(
        "INSERT INTO \"_norn_receipt\" SELECT -1, \"world\", NULL, \"request_hash\", \"receipt\", \"receipt_hash\" FROM \"_norn_receipt\" WHERE \"commit_seq\" = 1",
    );
    assert!(reopened.is_err(), "{reopened:?}");
}

#[test]
fn a_receipt_below_the_exact_integer_range_is_refused_on_reopen() {
    let reopened = reopen_after(
        "INSERT INTO \"_norn_receipt\" SELECT -9007199254740993, \"world\", NULL, \"request_hash\", \"receipt\", \"receipt_hash\" FROM \"_norn_receipt\" WHERE \"commit_seq\" = 1",
    );
    assert!(reopened.is_err(), "{reopened:?}");
}

#[test]
fn orphan_ledger_events_are_refused_on_reopen() {
    // Dropping receipt 2 leaves events 4 and 5 without a receipt.
    let reopened = reopen_after("DELETE FROM \"_norn_receipt\" WHERE \"commit_seq\" = 2");
    assert!(
        matches!(reopened, Err(CellError::LedgerCorrupt { seq: 4, .. })),
        "{reopened:?}"
    );
    // Moving an event to another commit breaks its receipt's span.
    let reopened = reopen_after("UPDATE \"_norn_ledger\" SET \"commit_seq\" = 2 WHERE \"seq\" = 3");
    assert!(
        matches!(reopened, Err(CellError::LedgerCorrupt { .. })),
        "{reopened:?}"
    );
}

#[test]
fn versions_never_repeat_across_a_type_hierarchy() {
    let mut store = CellStore::open_in_memory(model()).expect("opens");
    let customer = |object_type: &str| {
        let values: BTreeMap<String, Value> = model()
            .schema()
            .object_table(object_type)
            .map(|table| table.columns.clone())
            .into_iter()
            .flatten()
            .chain(
                model()
                    .schema()
                    .object_table("crm.Customer")
                    .expect("table")
                    .columns
                    .clone(),
            )
            .map(|column| {
                let value = match column.encoding {
                    ValueEncoding::Integer => rv(json!(1)),
                    ValueEncoding::Decimal => rv(json!("1.00")),
                    ValueEncoding::Boolean => rv(json!(true)),
                    ValueEncoding::Json => rv(json!({})),
                    ValueEncoding::Text => rv(json!("x")),
                };
                (column.property.as_str().to_owned(), value)
            })
            .collect();
        values
    };
    let write = |row: FactWriteRow| CommitBatch {
        world: MAIN_WORLD.to_owned(),
        scope: "org_demo/dev".to_owned(),
        writes: vec![row],
        ..CommitBatch::default()
    };
    let put = |object_type: &str| FactWriteRow::Put {
        object_type: object_type.to_owned(),
        key: "K".to_owned(),
        values: customer(object_type),
    };
    let delete = |object_type: &str| FactWriteRow::Delete {
        object_type: object_type.to_owned(),
        key: "K".to_owned(),
    };
    let mut seen = vec![(0, 0)];
    let mut observe = |store: &CellStore| {
        let versions = (
            store
                .read()
                .version(MAIN_WORLD, "crm.KeyAccount", "K")
                .expect("v"),
            store
                .read()
                .version(MAIN_WORLD, "crm.Customer", "K")
                .expect("v"),
        );
        seen.push(versions);
    };
    for row in [
        put("crm.KeyAccount"),
        delete("crm.KeyAccount"),
        // Repeat legitimate absent deletes before recreating a live parent.
        // A subtype-addressed delete cannot advance an incompatible live row.
        delete("crm.KeyAccount"),
        delete("crm.KeyAccount"),
        put("crm.Customer"),
        put("crm.Customer"),
        delete("crm.Customer"),
        put("crm.Customer"),
        delete("crm.Customer"),
        put("crm.KeyAccount"),
    ] {
        store.commit(&write(row)).expect("commits");
        observe(&store);
    }
    // Each key's version only ever increases.
    for pair in seen.windows(2) {
        assert!(pair[1].0 > pair[0].0 && pair[1].1 > pair[0].1, "{seen:?}");
    }
    assert_eq!(seen.last(), Some(&(10, 10)), "{seen:?}");
}

/// A store with two committed rounds, in `dir`.
fn seeded(dir: &Path, options: StoreOptions) -> CellStore {
    let mut store = CellStore::open(&dir.join("cell.sqlite"), model(), options).expect("opens");
    store.commit(&batch(1, 5)).expect("commits");
    store.commit(&batch(2, 5)).expect("commits");
    store
}

/// DST crash-mid-commit on tables: for both journal modes, a page cache small
/// enough to spill mid-transaction, and every commit point, the on-disk image
/// a crash leaves reopens to the pre-commit state (before `COMMIT`) or the
/// post-commit state (after it), the hash chain verifies, and an idempotent
/// retry after an acknowledged-lost commit replays its receipt.
#[test]
fn a_crash_at_any_commit_point_recovers_to_a_committed_state() {
    for journal_mode in [JournalMode::Delete, JournalMode::Wal] {
        let options = StoreOptions {
            journal_mode,
            cache_pages: Some(10),
        };
        // The reference post-commit state.
        let reference_dir = tempfile::tempdir().expect("tempdir");
        let mut reference = seeded(reference_dir.path(), options);
        let pre = reference.read().dump().expect("dumps");
        let crashing = batch(3, 200);
        let expected_receipt = reference
            .commit(&crashing)
            .expect("commits")
            .receipt()
            .clone();
        let post = reference.read().dump().expect("dumps");
        let post_head = reference.read().verify_ledger().expect("verifies");
        assert_ne!(pre, post);

        for point in CommitPoint::ALL {
            let dir = tempfile::tempdir().expect("tempdir");
            let path = dir.path().join("cell.sqlite");
            let crash_dir = tempfile::tempdir().expect("tempdir");
            let crash_path = crash_dir.path().join("cell.sqlite");
            let mut store = seeded(dir.path(), options);
            let outcome = store.commit_probed(&crashing, &mut |at| {
                if at == point {
                    common::crash_image(&path, &crash_path);
                    return Err(CellError::InjectedCrash(at));
                }
                Ok(())
            });
            assert!(
                matches!(outcome, Err(CellError::InjectedCrash(at)) if at == point),
                "{journal_mode:?} {point:?}: {outcome:?}"
            );
            // The live store rolled back (or, at `Committed`, committed).
            let expected = if point == CommitPoint::Committed {
                &post
            } else {
                &pre
            };
            assert_eq!(
                &store.read().dump().expect("dumps"),
                expected,
                "{journal_mode:?} {point:?} live"
            );
            drop(store);

            // The crash image recovers to the same state.
            let mut recovered =
                CellStore::open(&crash_path, model(), options).expect("crash image reopens");
            let head = recovered
                .read()
                .verify_ledger()
                .expect("recovered ledger verifies");
            assert_eq!(
                &recovered.read().dump().expect("dumps"),
                expected,
                "{journal_mode:?} {point:?} recovered"
            );
            // Retrying the turn converges on the reference commit either way.
            let retried = recovered.commit(&crashing).expect("retry commits");
            assert_eq!(
                retried.receipt(),
                &expected_receipt,
                "{journal_mode:?} {point:?}"
            );
            if point == CommitPoint::Committed {
                assert_eq!(head, post_head);
                assert!(matches!(retried, CommitOutcome::Replayed(_)));
            } else {
                assert!(matches!(retried, CommitOutcome::Committed(_)));
            }
            assert_eq!(recovered.read().dump().expect("dumps"), post);
            assert_eq!(
                recovered.read().verify_ledger().expect("verifies"),
                post_head
            );
        }
    }
}

#[test]
fn reopen_recovers_the_ledger_hash_chain_identically() {
    let dir = tempfile::tempdir().expect("tempdir");
    let options = StoreOptions::default();
    let store = seeded(dir.path(), options);
    let head = store.read().verify_ledger().expect("verifies");
    let ledger = store.read().ledger(0).expect("ledger");
    let dump = store.read().dump().expect("dumps");
    drop(store);
    let reopened =
        CellStore::open(&dir.path().join("cell.sqlite"), model(), options).expect("reopens");
    assert_eq!(reopened.read().verify_ledger().expect("verifies"), head);
    assert_eq!(reopened.read().ledger(0).expect("ledger"), ledger);
    assert_eq!(reopened.read().dump().expect("dumps"), dump);
}

#[test]
fn a_store_binding_is_checked_on_every_later_bind() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("cell.sqlite");
    let mut store = CellStore::open(&path, model(), StoreOptions::default()).expect("opens");
    store.bind("scope", "org_demo/dev").expect("binds");
    store.bind("scope", "org_demo/dev").expect("same binding");
    drop(store);
    let mut store = CellStore::open(&path, model(), StoreOptions::default()).expect("reopens");
    assert!(matches!(
        store.bind("scope", "org_demo/prod"),
        Err(CellError::BindingMismatch { .. })
    ));
    store.bind("scope", "org_demo/dev").expect("same binding");
}
