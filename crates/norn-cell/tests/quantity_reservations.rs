#![allow(
    clippy::expect_used,
    missing_docs,
    reason = "integration-test assertions and fixtures"
)]
//! Current normalized cell quantity authority, not the legacy engine shell.
mod quantity_support;

use std::collections::BTreeMap;

use norn_cell::{
    CellError, CellStore, CommitBatch, CommitOutcome, CommitPoint, Model, SqliteCell, StoreOptions,
};
use norn_ir::quantity_ledger::QuantityOperation;
use norn_kernel::quantity::{QuantityError, QuantityKey};
use norn_kernel::{CellCommit, FactKey, FactMutation, FactVersion, FactWrite, LogicalTime};
use norn_testkit::proptest::prelude::*;
use quantity_support::{define, parameters, post, request, reserve};

fn model() -> Model {
    let def = serde_json::from_value(serde_json::json!({"name":"fixture.Marker","properties":[{"id":"label","type":{"kind":"text"}}]})).expect("definition");
    Model::from_definitions(&[def], &[]).expect("model")
}
fn host() -> SqliteCell<String, String, String> {
    let store = CellStore::open_in_memory(model()).expect("store");
    SqliteCell::new(store, "tenant/dev", |_| "ok".into()).expect("host")
}
fn initialized(amount: i128) -> SqliteCell<String, String, String> {
    let mut cell = host();
    cell.commit_quantity_fenced(&define(), "define")
        .expect("define");
    cell.commit_quantity_fenced(&post(1, "fund", "fund", amount), "fund")
        .expect("fund");
    cell
}

// Byte-identical ordinary append fixture from pre-PR c290972a's
// Writer::append_events/write_receipt and CommitBatch encoding. No current
// commit/admission path is used to insert the legacy discriminator event.
fn append_pre_pr_event(path: &std::path::Path, event: &serde_json::Value) -> norn_cell::Receipt {
    use norn_cell::engine::{NativeEngine, SqlEngine};
    fn hash<T: serde::Serialize>(value: &T) -> String {
        norn_ir::canonical::HashProfile::Norn1
            .hash_canonical_bytes(&norn_ir::canonical::canonical_json_of(value).expect("canonical"))
    }
    fn text<T: serde::Serialize>(value: &T) -> String {
        String::from_utf8(norn_ir::canonical::canonical_json_of(value).expect("canonical"))
            .expect("UTF-8")
    }
    let engine = NativeEngine::open(path, StoreOptions::default()).expect("engine");
    engine.transaction(|| -> Result<norn_cell::Receipt, CellError> {
        let (seq, prev) = engine.query_optional(
            r#"SELECT "seq", "hash" FROM "_norn_ledger" ORDER BY "seq" DESC LIMIT 1"#, &[],
            |row| Ok((row.get::<i64>(0)?, row.get::<String>(1)?)),
         )?.map_or_else(|| (1, "0".repeat(64)), |(seq, hash)| (seq.checked_add(1).expect("fixture sequence"), hash));
        let commit_seq: i64 = engine.query_row(r#"SELECT COALESCE(MAX("commit_seq"), 0)+1 FROM "_norn_receipt""#, &[], |row| row.get(0))?;
        let batch = CommitBatch {
            world: "main".into(), scope: "tenant/dev".into(),
            idempotency_key: Some(format!("legacy-{commit_seq}")),
            events: vec![norn_ir::RuntimeValue::from_json(event.clone()).expect("runtime event")],
            ..CommitBatch::default()
        };
        // Explicit pre-PR batch fields: quantity did not exist in that contract.
        let original_batch = serde_json::json!({
            "world":"main", "scope":"tenant/dev", "logicalTime":0,
            "idempotencyKey":batch.idempotency_key, "writes":[], "facts":[],
            "links":[], "events":[event], "effects":[],
        });
        assert_eq!(text(&batch), text(&original_batch));
        let head = hash(&serde_json::json!({"seq":seq,"commitSeq":commit_seq,"world":"main","event":event,"prev":prev}));
        let body = norn_cell::ReceiptBody {
            commit_seq: u64::try_from(commit_seq).expect("commit"), definitions_version: 0,
            world: "main".into(), logical_time: 0, idempotency_key: batch.idempotency_key.clone(),
            request_hash: hash(&original_batch), quantity: None, quantity_result: None, quantity_intent: None, consumed: None, written: vec![], linked: vec![],
            first_event_seq: u64::try_from(seq).expect("sequence"), event_count: 1,
            ledger_head: head.clone(), enqueued: vec![], scheduled: vec![],
        };
        engine.execute(r#"INSERT INTO "_norn_ledger" ("seq","commit_seq","world","event","prev_hash","hash") VALUES (?1,?2,?3,?4,?5,?6)"#, &[seq.into(), commit_seq.into(), "main".into(), text(event).into(), prev.into(), head.into()])?;
        let receipt_hash = hash(&body);
        engine.execute(r#"INSERT INTO "_norn_receipt" ("commit_seq","world","idempotency_key","request_hash","receipt","receipt_hash") VALUES (?1,?2,?3,?4,?5,?6)"#, &[commit_seq.into(), "main".into(), batch.idempotency_key.into(), body.request_hash.clone().into(), text(&body).into(), receipt_hash.clone().into()])?;
        engine.execute(r#"UPDATE "_norn_clock" SET logical_time=0,commit_seq=?1,receipt_hash=?2 WHERE id=1"#, &[commit_seq.into(), receipt_hash.clone().into()])?;
        Ok(norn_cell::Receipt { body, hash: receipt_hash })
    }).expect("original ordinary append")
}

#[test]
fn pre_upgrade_discriminator_events_keep_original_meaning_after_quantity_upgrade() {
    use norn_cell::engine::{NativeEngine, SqlEngine};
    for event in [
        serde_json::json!({"type":"quantity.transition"}),
        serde_json::json!({"type":"quantity.transition","request":define()}),
        serde_json::json!({"type":"norn.cell.store/3"}),
        serde_json::json!({"type":"norn.cell.store/3","quantityBoundary":{"seq":1,"commitSeq":1}}),
        serde_json::json!({"type":"norn.cell.store/4"}),
        serde_json::json!({"type":"norn.cell.store/4","quantityBoundary":{"seq":1,"commitSeq":1}}),
        serde_json::json!({"type":"norn.cell.store/5"}),
        serde_json::json!({"type":"norn.cell.store/5","quantityBoundary":{"seq":1,"commitSeq":1}}),
        serde_json::json!({"type":"norn.cell.store/6"}),
        serde_json::json!({"type":"norn.cell.store/6","quantityBoundary":{"seq":1,"commitSeq":1}}),
    ] {
        let dir = tempfile::tempdir().expect("directory");
        let path = dir.path().join("cell.sqlite");
        let mut store =
            CellStore::open(&path, model(), StoreOptions::default()).expect("legacy store");
        store.bind("scope", "tenant/dev").expect("binding");
        drop(store);
        let original = append_pre_pr_event(&path, &event);
        // Both previously public names coexist in the same pre-PR cell.
        append_pre_pr_event(&path, &serde_json::json!({"type":"norn.cell.store/6"}));
        append_pre_pr_event(&path, &serde_json::json!({"type":"quantity.transition"}));
        let store =
            CellStore::open(&path, model(), StoreOptions::default()).expect("pre-upgrade reopen");
        assert_eq!(
            store.read().receipt_at(1).expect("receipt"),
            Some(original.clone())
        );
        let before = store.read().dump().expect("dump");
        let mut cell: SqliteCell<String, String, String> =
            SqliteCell::new(store, "tenant/dev", |_| "ok".into()).expect("host");
        let definition = cell
            .commit_quantity_fenced(&define(), "define")
            .expect("atomic upgrade");
        assert_eq!(
            definition.receipt(),
            cell.commit_quantity_fenced(&define(), "define")
                .expect("exact define retry")
                .receipt()
        );
        cell.commit_quantity_fenced(&post(1, "fund", "fund", 20), "fund")
            .expect("fund");
        assert_eq!(
            cell.store().read().receipt_at(1).expect("receipt"),
            Some(original)
        );
        assert_ne!(before, cell.store().read().dump().expect("upgraded"));
        drop(cell);
        let store =
            CellStore::open(&path, model(), StoreOptions::default()).expect("upgraded reopen");
        assert_eq!(
            store
                .read()
                .quantity_version("main", "stock")
                .expect("revision"),
            2
        );
        drop(store);
        let engine = NativeEngine::open(&path, StoreOptions::default()).expect("engine");
        let format: String = engine
            .query_row(
                r#"SELECT "value" FROM "_norn_meta" WHERE "key"='format'"#,
                &[],
                |row| row.get(0),
            )
            .expect("format");
        assert_eq!(format, "norn.cell.store/7");
        drop(engine);
        // Ordinary receipts retain ordinary meaning even after the boundary.
        append_pre_pr_event(&path, &serde_json::json!({"type":"quantity.transition"}));
        CellStore::open(&path, model(), StoreOptions::default()).expect("ordinary suffix reopen");
    }
}

#[test]
fn first_quantity_upgrade_rolls_back_at_every_write_seam_and_replays_lost_reply() {
    let dir = tempfile::tempdir().expect("directory");
    let path = dir.path().join("cell.sqlite");
    let mut store = CellStore::open(&path, model(), StoreOptions::default()).expect("legacy store");
    store.bind("scope", "tenant/dev").expect("binding");
    drop(store);
    append_pre_pr_event(&path, &serde_json::json!({"type":"quantity.transition"}));
    let mut store = CellStore::open(&path, model(), StoreOptions::default()).expect("reopen");
    let batch = CommitBatch {
        world: "main".into(),
        scope: "tenant/dev".into(),
        quantity: Some(define()),
        idempotency_key: Some("define".into()),
        ..CommitBatch::default()
    };
    let before = store.read().dump().expect("legacy dump");
    for seam in [
        CommitPoint::FactsApplied,
        CommitPoint::LinksApplied,
        CommitPoint::EventsAppended,
        CommitPoint::EffectsEnqueued,
        CommitPoint::ReceiptWritten,
    ] {
        assert!(
            store
                .commit_probed(&batch, &mut |point| {
                    if point == seam {
                        Err(CellError::Engine("interrupted upgrade".into()))
                    } else {
                        Ok(())
                    }
                })
                .is_err()
        );
        assert_eq!(before, store.read().dump().expect("atomic rollback"));
        store
            .read()
            .verify_ledger()
            .expect("legacy prefix still verifies");
    }
    assert!(
        store
            .commit_probed(&batch, &mut |point| {
                if point == CommitPoint::Committed {
                    Err(CellError::Engine("lost upgrade reply".into()))
                } else {
                    Ok(())
                }
            })
            .is_err()
    );
    let committed = store.read().dump().expect("committed upgrade");
    drop(store);
    let mut store =
        CellStore::open(&path, model(), StoreOptions::default()).expect("upgraded reopen");
    assert!(matches!(
        store.commit(&batch).expect("retry"),
        CommitOutcome::Replayed(_)
    ));
    assert_eq!(
        committed,
        store.read().dump().expect("exact replay without mutation")
    );
}

#[test]
fn quantity_boundary_projection_cannot_be_shifted_or_removed() {
    use norn_cell::engine::{NativeEngine, SqlEngine};
    for damage in [
        r#"UPDATE "_norn_meta" SET "value"='{"seq":2,"commitSeq":1}' WHERE "key"='quantity:boundary'"#,
        r#"DELETE FROM "_norn_meta" WHERE "key"='quantity:boundary'"#,
        r#"UPDATE "_norn_meta" SET "value"='norn.cell.store/4' WHERE "key"='format'"#,
    ] {
        let dir = tempfile::tempdir().expect("directory");
        let path = dir.path().join("cell.sqlite");
        let mut store = CellStore::open(&path, model(), StoreOptions::default()).expect("store");
        store.bind("scope", "tenant/dev").expect("binding");
        store
            .commit_fenced(&CommitBatch {
                world: "main".into(),
                scope: "tenant/dev".into(),
                quantity: Some(define()),
                idempotency_key: Some("define".into()),
                ..CommitBatch::default()
            })
            .expect("define");
        let engine = NativeEngine::open(&path, StoreOptions::default()).expect("engine");
        engine.execute(damage, &[]).expect("damage");
        let before = store.read().dump().expect("dump");
        assert!(
            store
                .commit_fenced(&CommitBatch {
                    world: "main".into(),
                    scope: "tenant/dev".into(),
                    quantity: Some(post(1, "fund", "fund", 20)),
                    idempotency_key: Some("fund".into()),
                    ..CommitBatch::default()
                })
                .is_err()
        );
        assert_eq!(before, store.read().dump().expect("no mutation"));
        drop(store);
        drop(engine);
        assert!(CellStore::open(&path, model(), StoreOptions::default()).is_err());
    }
}

#[test]
fn immutable_quantity_boundary_refuses_coordinated_legacy_downgrade_and_redefinition() {
    use norn_cell::engine::{NativeEngine, SqlEngine};
    for reopen_before_damage in [false, true] {
        let dir = tempfile::tempdir().expect("directory");
        let path = dir.path().join("cell.sqlite");
        let mut store = CellStore::open(&path, model(), StoreOptions::default()).expect("store");
        store.bind("scope", "tenant/dev").expect("binding");
        store
            .commit_fenced(&CommitBatch {
                world: "main".into(),
                scope: "tenant/dev".into(),
                quantity: Some(define()),
                idempotency_key: Some("define".into()),
                ..CommitBatch::default()
            })
            .expect("define");
        if reopen_before_damage {
            drop(store);
            store =
                CellStore::open(&path, model(), StoreOptions::default()).expect("intact reopen");
        }
        let events = store.read().ledger(0).expect("immutable events");
        let receipts = store.read().receipts().expect("immutable receipts");
        let export = store.export_record(1).expect("immutable export");
        let engine = NativeEngine::open(&path, StoreOptions::default()).expect("engine");
        for sql in [
            r#"UPDATE "_norn_meta" SET "value"='norn.cell.store/4' WHERE "key"='format'"#,
            r#"DELETE FROM "_norn_meta" WHERE "key"='quantity:boundary'"#,
            r#"DELETE FROM "_norn_quantity_ledger" WHERE "world"='main' AND "ledger"='stock'"#,
        ] {
            engine.execute(sql, &[]).expect("projection-only damage");
        }
        let before = store.read().dump().expect("damaged dump");
        assert!(matches!(
            CellStore::open(&path, model(), StoreOptions::default()),
            Err(CellError::StoreFormat { found, expected }) if found == "norn.cell.store/4" && expected == "norn.cell.store/7"
        ));
        let mut replacement = define();
        if let QuantityOperation::Define { pack, .. } = &mut replacement.operation {
            pack.pack_id = "fixture.replacement".into();
        }
        assert!(matches!(
            store.commit_fenced(&CommitBatch {
                world: "main".into(),
                scope: "tenant/dev".into(),
                quantity: Some(replacement),
                idempotency_key: Some("redefine-at-zero".into()),
                ..CommitBatch::default()
            }),
            Err(CellError::Quantity(QuantityError::Corrupt))
        ));
        assert!(store.read().verify_ledger().is_err());
        assert!(store.verified_head().is_err());
        assert_eq!(before, store.read().dump().expect("no mutation"));
        assert_eq!(events, store.read().ledger(0).expect("events unchanged"));
        assert_eq!(
            receipts,
            store.read().receipts().expect("receipts unchanged")
        );
        assert_eq!(export, store.export_record(1).expect("export unchanged"));
    }
}

#[test]
fn legacy_open_handle_observes_another_handles_immutable_quantity_upgrade() {
    let dir = tempfile::tempdir().expect("directory");
    let path = dir.path().join("cell.sqlite");
    let mut first = CellStore::open(&path, model(), StoreOptions::default()).expect("first");
    first.bind("scope", "tenant/dev").expect("binding");
    let mut second = CellStore::open(&path, model(), StoreOptions::default()).expect("second");
    first
        .commit_fenced(&CommitBatch {
            world: "main".into(),
            scope: "tenant/dev".into(),
            quantity: Some(define()),
            idempotency_key: Some("define".into()),
            ..CommitBatch::default()
        })
        .expect("upgrade through first handle");
    second
        .commit_fenced(&CommitBatch {
            world: "main".into(),
            scope: "tenant/dev".into(),
            quantity: Some(post(1, "fund", "fund", 20)),
            idempotency_key: Some("fund".into()),
            ..CommitBatch::default()
        })
        .expect("second handle observes immutable boundary");
    first
        .verified_head()
        .expect("verify suffix through upgrade");
    second
        .verified_head()
        .expect("verify suffix through upgrade");
    drop(first);
    drop(second);
    let store = CellStore::open(&path, model(), StoreOptions::default()).expect("reopen");
    assert_eq!(
        store
            .read()
            .quantity_version("main", "stock")
            .expect("version"),
        2
    );
}

#[test]
fn lowered_quantity_clock_refuses_reopen_retry_and_expired_hold_admission() {
    use norn_cell::engine::{NativeEngine, SqlEngine};
    let dir = tempfile::tempdir().expect("directory");
    let path = dir.path().join("cell.sqlite");
    let store = CellStore::open(&path, model(), StoreOptions::default()).expect("store");
    let mut cell: SqliteCell<String, String, String> =
        SqliteCell::new(store, "tenant/dev", |_| "ok".into()).expect("host");
    cell.commit_quantity_fenced(&define(), "define")
        .expect("define");
    cell.commit_quantity_fenced(&post(1, "fund", "fund", 20), "fund")
        .expect("fund");
    cell.advance_to(LogicalTime::from_millis(5));
    cell.commit_quantity_fenced(&reserve(2, "hold", 10, 100), "hold")
        .expect("hold");
    cell.advance_to(LogicalTime::from_millis(120));
    let later = post(3, "later", "fund", 1);
    let receipt = cell
        .commit_quantity_fenced(&later, "later")
        .expect("later posting");
    drop(cell);
    let store = CellStore::open(&path, model(), StoreOptions::default()).expect("intact reopen");
    let mut cell: SqliteCell<String, String, String> =
        SqliteCell::new(store, "tenant/dev", |_| "ok".into()).expect("restarted host");
    cell.advance_to(LogicalTime::from_millis(50));
    let resolution = request(
        4,
        QuantityOperation::PostReservation {
            reservation_id: "hold".into(),
            posting_id: "expired-post".into(),
            resolution: norn_ir::quantity_ledger::QuantityResolution::All,
        },
    );
    assert!(matches!(
        cell.commit_quantity_fenced(&resolution, "intact-expired"),
        Err(CellError::Quantity(QuantityError::Expired))
    ));
    assert_eq!(
        receipt.receipt(),
        cell.commit_quantity_fenced(&later, "later")
            .expect("exact original timestamp retry")
            .receipt()
    );
    let engine = NativeEngine::open(&path, StoreOptions::default()).expect("engine");
    engine
        .execute(
            r#"UPDATE "_norn_clock" SET "logical_time"=0 WHERE id=1"#,
            &[],
        )
        .expect("lower only clock");
    let before = cell.store().read().dump().expect("damaged dump");
    for (operation, id) in [(&resolution, "expired"), (&later, "later")] {
        assert!(matches!(
            cell.commit_quantity_fenced(operation, id),
            Err(CellError::Quantity(QuantityError::Corrupt))
        ));
        assert_eq!(before, cell.store().read().dump().expect("no mutation"));
    }
    drop(cell);
    drop(engine);
    assert!(matches!(
        CellStore::open(&path, model(), StoreOptions::default()),
        Err(CellError::Quantity(QuantityError::Corrupt))
    ));
}

#[test]
fn ordinary_event_cannot_claim_the_internal_quantity_transition_type() {
    let dir = tempfile::tempdir().expect("directory");
    let path = dir.path().join("cell.sqlite");
    let store = CellStore::open(&path, model(), StoreOptions::default()).expect("store");
    let mut cell: SqliteCell<serde_json::Value, String, String> =
        SqliteCell::new(store, "tenant/dev", |_| "ok".into()).expect("host");
    let before = cell.store().read().dump().expect("before");
    assert!(matches!(
        cell.commit_host_events_idempotently(
            "spoof",
            vec![serde_json::json!({"type":"quantity.transition"})],
        ),
        Err(CellError::Quantity(QuantityError::InvalidContract))
    ));
    assert_eq!(before, cell.store().read().dump().expect("unchanged"));
    drop(cell);
    CellStore::open(&path, model(), StoreOptions::default()).expect("reopen after refusal");
}

#[test]
fn missing_quantity_ledger_refuses_redefinition_and_reopen() {
    use norn_cell::engine::{NativeEngine, SqlEngine};
    let dir = tempfile::tempdir().expect("directory");
    let path = dir.path().join("cell.sqlite");
    let store = CellStore::open(&path, model(), StoreOptions::default()).expect("store");
    let mut cell: SqliteCell<String, String, String> =
        SqliteCell::new(store, "tenant/dev", |_| "ok".into()).expect("host");
    cell.commit_quantity_fenced(&define(), "define")
        .expect("define");
    cell.commit_quantity_fenced(&post(1, "fund", "fund", 20), "fund")
        .expect("fund");
    let engine = NativeEngine::open(&path, StoreOptions::default()).expect("engine");
    engine
        .execute(
            r#"DELETE FROM "_norn_quantity_ledger" WHERE "world"='main' AND "ledger"='stock'"#,
            &[],
        )
        .expect("fault injection");
    let before = cell.store().read().dump().expect("damaged state");
    assert!(matches!(
        cell.commit_quantity_fenced(&define(), "fresh-define"),
        Err(CellError::Quantity(QuantityError::Corrupt))
    ));
    assert_eq!(before, cell.store().read().dump().expect("unchanged"));
    drop(cell);
    drop(engine);
    assert!(matches!(
        CellStore::open(&path, model(), StoreOptions::default()),
        Err(CellError::Quantity(QuantityError::Corrupt))
    ));
}

#[test]
fn missing_reservation_projection_refuses_reopen() {
    use norn_cell::engine::{NativeEngine, SqlEngine};
    let dir = tempfile::tempdir().expect("directory");
    let path = dir.path().join("cell.sqlite");
    let store = CellStore::open(&path, model(), StoreOptions::default()).expect("store");
    let mut cell: SqliteCell<String, String, String> =
        SqliteCell::new(store, "tenant/dev", |_| "ok".into()).expect("host");
    cell.commit_quantity_fenced(&define(), "define")
        .expect("define");
    cell.commit_quantity_fenced(&post(1, "fund", "fund", 20), "fund")
        .expect("fund");
    cell.commit_quantity_fenced(&reserve(2, "hold", 10, 100), "hold")
        .expect("reserve");
    drop(cell);
    let engine = NativeEngine::open(&path, StoreOptions::default()).expect("engine");
    engine.execute(
        r#"DELETE FROM "_norn_quantity_reservation" WHERE "world"='main' AND "ledger"='stock' AND "id"='hold'"#,
        &[],
    ).expect("fault injection");
    drop(engine);
    assert!(matches!(
        CellStore::open(&path, model(), StoreOptions::default()),
        Err(CellError::Quantity(QuantityError::Corrupt))
    ));
}
fn key(account: &str) -> QuantityKey {
    QuantityKey {
        account: account.into(),
        unit: "item".into(),
        layer: "physical".into(),
    }
}
fn amounts(cell: &SqliteCell<String, String, String>, account: &str) -> (i128, i128) {
    let b = cell
        .store()
        .read()
        .quantity_balance("main", "stock", &key(account))
        .expect("balance");
    (
        b.posted.parse().expect("exact"),
        b.held.parse().expect("exact"),
    )
}
fn effects() -> Vec<norn_kernel::Effect<String, String>> {
    vec![
        norn_kernel::Effect::Perform(norn_kernel::Command {
            id: norn_kernel::CommandId("notify".into()),
            generation: norn_kernel::Generation(0),
            idempotency_key: norn_kernel::IdempotencyKey("notify".into()),
            freshness: norn_kernel::FreshnessPolicy::AppendOnly,
            deadline: None,
            payload: "notify".into(),
        }),
        norn_kernel::Effect::Schedule(norn_kernel::Timer {
            id: norn_kernel::TimerId("deadline".into()),
            due: LogicalTime::from_millis(10),
            payload: "deadline".into(),
        }),
    ]
}

fn refused(
    cell: &mut SqliteCell<String, String, String>,
    req: &norn_ir::quantity_ledger::QuantityRequest,
    error: &QuantityError,
) {
    let before = cell.store().read().dump().expect("dump");
    let refusal = cell.commit_quantity_fenced(req, "refused");
    assert!(
        matches!(&refusal, Err(CellError::Quantity(found)) if found == error),
        "expected {error:?}, got {refusal:?}"
    );
    assert_eq!(before, cell.store().read().dump().expect("unchanged"));
}

#[test]
fn reserve_full_post_and_in_transit_loss_are_conserved() {
    let mut cell = initialized(100);
    let r = reserve(2, "shipment", 70, 1000);
    cell.commit_quantity_fenced(&r, "reserve").expect("reserve");
    assert_eq!(amounts(&cell, "lot"), (100, 70));
    refused(
        &mut cell,
        &post(3, "competing", "ship", 31),
        &QuantityError::InsufficientAvailable {
            account: "lot".into(),
        },
    );
    let consume = request(
        3,
        QuantityOperation::PostReservation {
            reservation_id: "shipment".into(),
            posting_id: "ship".into(),
            resolution: norn_ir::quantity_ledger::QuantityResolution::All,
        },
    );
    cell.commit_quantity_fenced(&consume, "consume")
        .expect("post full vector");
    assert_eq!(amounts(&cell, "lot"), (30, 0));
    assert_eq!(amounts(&cell, "transit"), (70, 0));
    cell.commit_quantity_fenced(&post(4, "accepted", "receive", 65), "accept")
        .expect("receive");
    cell.commit_quantity_fenced(&post(5, "loss", "loss", 5), "loss")
        .expect("explicit conserved loss");
    assert_eq!(amounts(&cell, "transit"), (0, 0));
    assert_eq!(amounts(&cell, "received"), (65, 0));
    assert_eq!(amounts(&cell, "loss"), (5, 0));
    let total: i128 = ["issuance", "lot", "transit", "received", "loss"]
        .into_iter()
        .map(|a| amounts(&cell, a).0)
        .sum();
    assert_eq!(total, 0);
    assert_eq!(
        cell.store()
            .read()
            .quantity_reservation("main", "stock", "shipment")
            .expect("reservation")
            .expect("record")
            .status,
        "posted"
    );
    refused(
        &mut cell,
        &request(
            6,
            QuantityOperation::Void {
                reservation_id: "shipment".into(),
                resolution: norn_ir::quantity_ledger::QuantityResolution::All,
            },
        ),
        &QuantityError::Terminal,
    );
}

#[test]
fn void_and_expiry_use_exclusive_trusted_deadline_and_do_not_post() {
    let mut cell = initialized(50);
    cell.commit_quantity_fenced(&reserve(2, "a", 30, 10), "a")
        .expect("reserve");
    refused(
        &mut cell,
        &request(
            3,
            QuantityOperation::Expire {
                reservation_id: "a".into(),
            },
        ),
        &QuantityError::NotExpired,
    );
    cell.commit_quantity_fenced(
        &request(
            3,
            QuantityOperation::Void {
                reservation_id: "a".into(),
                resolution: norn_ir::quantity_ledger::QuantityResolution::All,
            },
        ),
        "void",
    )
    .expect("void");
    assert_eq!(amounts(&cell, "lot"), (50, 0));
    cell.commit_quantity_fenced(&reserve(4, "b", 30, 10), "b")
        .expect("reserve");
    cell.advance_to(LogicalTime::from_millis(10));
    refused(
        &mut cell,
        &request(
            5,
            QuantityOperation::PostReservation {
                reservation_id: "b".into(),
                posting_id: "b".into(),
                resolution: norn_ir::quantity_ledger::QuantityResolution::All,
            },
        ),
        &QuantityError::Expired,
    );
    refused(
        &mut cell,
        &request(
            5,
            QuantityOperation::Void {
                reservation_id: "b".into(),
                resolution: norn_ir::quantity_ledger::QuantityResolution::All,
            },
        ),
        &QuantityError::Expired,
    );
    cell.commit_quantity_fenced(
        &request(
            5,
            QuantityOperation::Expire {
                reservation_id: "b".into(),
            },
        ),
        "expire",
    )
    .expect("expire");
    assert_eq!(amounts(&cell, "lot"), (50, 0));
    assert!(
        cell.store()
            .read()
            .quantity_posting("main", "stock", "b")
            .expect("posting")
            .is_none()
    );
    assert!(serde_json::from_value::<QuantityOperation>(serde_json::json!({"kind":"post_reservation","reservation_id":"b","posting_id":"partial","parameters":parameters(1)})).is_err());
}

#[test]
fn linked_reversal_is_append_only_and_cannot_repeat_or_reverse_a_reversal() {
    let mut cell = initialized(50);
    cell.commit_quantity_fenced(&post(2, "move", "ship", 20), "move")
        .expect("ship");
    let original = cell
        .store()
        .read()
        .quantity_posting("main", "stock", "move")
        .expect("posting");
    cell.commit_quantity_fenced(
        &request(
            3,
            QuantityOperation::Reverse {
                original_id: "move".into(),
                posting_id: "undo".into(),
            },
        ),
        "undo",
    )
    .expect("reversal");
    assert_eq!(amounts(&cell, "lot"), (50, 0));
    assert_eq!(
        original,
        cell.store()
            .read()
            .quantity_posting("main", "stock", "move")
            .expect("immutable")
    );
    assert_eq!(
        cell.store()
            .read()
            .quantity_posting("main", "stock", "undo")
            .expect("posting")
            .expect("record")
            .reverses
            .as_deref(),
        Some("move")
    );
    for id in ["move", "undo"] {
        refused(
            &mut cell,
            &request(
                4,
                QuantityOperation::Reverse {
                    original_id: id.into(),
                    posting_id: "again".into(),
                },
            ),
            &QuantityError::InvalidReversal,
        );
    }
}

#[test]
fn caller_quantity_intent_cannot_hide_changed_combined_inputs() {
    use norn_cell::{Consumption, EffectRow, FactWriteRow, LinkChange};
    use norn_ir::RuntimeValue as Value;

    let mut store = CellStore::open_in_memory(model()).expect("store");
    store.bind("scope", "tenant/dev").expect("scope");
    let batch = CommitBatch {
        world: "main".into(),
        scope: "tenant/dev".into(),
        idempotency_key: Some("combined".into()),
        quantity: Some(define()),
        quantity_intent: Some("caller-controlled".into()),
        writes: vec![FactWriteRow::Put {
            object_type: "fixture.Marker".into(),
            key: "a".into(),
            values: BTreeMap::from([("label".into(), Value::String("first".into()))]),
        }],
        ..CommitBatch::default()
    };
    // Exercise the public wire boundary, including the caller-supplied intent.
    let batch: CommitBatch =
        serde_json::from_str(&serde_json::to_string(&batch).expect("serialize request"))
            .expect("deserialize request");
    let first = store.commit(&batch).expect("first commit");
    assert_ne!(first.receipt().body.quantity_intent, batch.quantity_intent);
    let before = store.read().dump().expect("dump");
    let mut changed_field = batch.clone();
    if let FactWriteRow::Put { values, .. } = changed_field.writes.first_mut().expect("write") {
        values.insert("label".into(), Value::String("changed".into()));
    }
    let mut changed_quantity = batch.clone();
    if let QuantityOperation::Define { account_limit, .. } = &mut changed_quantity
        .quantity
        .as_mut()
        .expect("quantity")
        .operation
    {
        *account_limit += 1;
    }
    let mut removed_quantity = batch.clone();
    removed_quantity.quantity = None;
    let mut changed_event = batch.clone();
    changed_event.events.push(Value::String("changed".into()));
    let mut changed_effect = batch.clone();
    changed_effect.effects.push(EffectRow::CancelTimer {
        timer_id: "changed".into(),
    });
    let mut changed_consumption = batch.clone();
    changed_consumption.consumes = Some(Consumption::Observation {
        observation_id: "changed".into(),
    });
    let mut changed_link = batch.clone();
    changed_link.links.push(LinkChange::Remove {
        object_type: "fixture.Marker".into(),
        link: "changed".into(),
        source: "a".into(),
        target: "b".into(),
        target_type: None,
    });
    for mut changed in [
        changed_field,
        changed_quantity,
        removed_quantity,
        changed_event,
        changed_effect,
        changed_consumption,
        changed_link,
    ] {
        // Even copying the kernel's fingerprint cannot authorize a changed input.
        changed
            .quantity_intent
            .clone_from(&first.receipt().body.quantity_intent);
        assert!(
            matches!(store.commit(&changed), Err(CellError::IdempotencyConflict(key)) if key == "combined")
        );
        assert_eq!(before, store.read().dump().expect("refusal is atomic"));
    }
    let replay = store.commit(&batch).expect("exact retry");
    assert!(matches!(replay, CommitOutcome::Replayed(_)));
    assert_eq!(first.receipt(), replay.receipt());
    assert_eq!(before, store.read().dump().expect("replay writes nothing"));
}

#[test]
fn exact_retry_survives_time_and_state_changes_but_changed_input_conflicts() {
    let mut cell = initialized(50);
    let r = reserve(2, "a", 20, 10);
    let first = cell.commit_quantity_fenced(&r, "a").expect("reserve");
    cell.advance_to(LogicalTime::from_millis(100));
    cell.commit_quantity_fenced(
        &request(
            3,
            QuantityOperation::Expire {
                reservation_id: "a".into(),
            },
        ),
        "expired",
    )
    .expect("expire");
    let retry = cell.commit_quantity_fenced(&r, "a").expect("stable retry");
    assert!(matches!(retry, CommitOutcome::Replayed(_)));
    assert_eq!(first.receipt(), retry.receipt());
    let before = cell.store().read().dump().expect("dump");
    assert!(matches!(
        cell.commit_quantity_fenced(&reserve(2, "a", 21, 10), "a"),
        Err(CellError::IdempotencyConflict(_))
    ));
    assert_eq!(before, cell.store().read().dump().expect("unchanged"));
    let mut stale = post(2, "stale", "ship", 1);
    stale.balance_versions = cell
        .store()
        .read()
        .quantity_balance_versions("main", &stale)
        .expect("posting read set");
    let original = &first
        .receipt()
        .body
        .quantity
        .as_ref()
        .expect("original request")
        .balance_versions;
    for fence in &mut stale.balance_versions {
        if let Some(old) = original.iter().find(|old| {
            old.account == fence.account && old.unit == fence.unit && old.layer == fence.layer
        }) {
            fence.version = old.version;
        }
    }
    refused(
        &mut cell,
        &stale,
        &QuantityError::StaleVersion {
            expected: 1,
            current: 3,
        },
    );
}

#[test]
fn balance_and_hold_overflow_refuse_exactly() {
    let mut cell = initialized(i128::MAX);
    refused(
        &mut cell,
        &post(2, "overflow", "fund", 1),
        &QuantityError::Overflow,
    );
    // Negative vectors are rejected by the pinned pack's exact input bounds.
    refused(
        &mut cell,
        &post(2, "min", "fund", i128::MIN),
        &QuantityError::InvalidContract,
    );
    cell.commit_quantity_fenced(&reserve(2, "all", i128::MAX, 100), "all")
        .expect("max hold");
    refused(
        &mut cell,
        &reserve(3, "more", 1, 100),
        &QuantityError::InsufficientAvailable {
            account: "lot".into(),
        },
    );
}

#[test]
fn quantity_failure_rolls_back_ordinary_writes_events_and_effects() {
    let mut cell = initialized(1);
    let mut commit = CellCommit::empty();
    commit.events.push("attempt".into());
    commit.effects = effects();
    commit.writes.push(FactWrite {
        key: FactKey::new("fixture.Marker", "a"),
        expected: FactVersion::ABSENT,
        mutation: FactMutation::Put {
            values: BTreeMap::from([(
                "label".into(),
                norn_ir::RuntimeValue::String("attempt".into()),
            )]),
        },
    });
    let before = cell.store().read().dump().expect("dump");
    assert!(
        cell.commit_with_quantity_fenced(commit, &post(2, "short", "ship", 2), "short")
            .is_err()
    );
    assert_eq!(before, cell.store().read().dump().expect("unchanged"));
}

#[tokio::test]
#[allow(
    clippy::too_many_lines,
    reason = "one fixture checks replay, restart, restore, scheduled expiry and clock monotonicity"
)]
async fn reopen_restore_replay_preserve_heads_holds_history_and_receipts() {
    let dir = tempfile::tempdir().expect("directory");
    let path = dir.path().join("cell.sqlite");
    let export = dir.path().join("export");
    let store = CellStore::open(&path, model(), StoreOptions::default()).expect("store");
    let mut cell: SqliteCell<String, String, String> =
        SqliteCell::new(store, "tenant/dev", |_| "ok".into()).expect("host");
    cell.commit_quantity_fenced(&define(), "define")
        .expect("define");
    cell.commit_quantity_fenced(&post(1, "fund", "fund", 40), "fund")
        .expect("fund");
    cell.advance_to(LogicalTime::from_millis(5));
    let r = reserve(2, "pending", 20, 10);
    let receipt = cell.commit_quantity_fenced(&r, "reserve").expect("reserve");
    let expected = cell.store().read().dump().expect("dump");
    let mut exporter = norn_cell::export::Exporter::local(&export).expect("exporter");
    for seq in 1..=3 {
        exporter
            .append(
                &cell
                    .store()
                    .export_record(seq)
                    .expect("journal")
                    .expect("record"),
            )
            .await
            .expect("export");
    }
    drop(cell);
    let reopened = CellStore::open(&path, model(), StoreOptions::default()).expect("reopen");
    assert_eq!(expected, reopened.read().dump().expect("reopened state"));
    assert_eq!(
        reopened.read().next_timer_due().expect("reopened alarm"),
        Some(10)
    );
    assert!(
        reopened
            .read()
            .due_quantity_timer(9)
            .expect("before deadline")
            .is_none()
    );
    drop(reopened);
    let restored_path = dir.path().join("restored.sqlite");
    CellStore::restore_from(
        &mut exporter,
        &restored_path,
        model(),
        "tenant/dev",
        &BTreeMap::from([("scope".into(), "tenant/dev".into())]),
        StoreOptions::default(),
    )
    .await
    .expect("restore");
    let mut restored =
        CellStore::open(&restored_path, model(), StoreOptions::default()).expect("restored");
    assert_eq!(expected, restored.read().dump().expect("restored state"));
    assert_eq!(
        restored.read().next_timer_due().expect("restored alarm"),
        Some(10)
    );
    assert!(
        restored
            .read()
            .due_timer(10)
            .expect("ordinary selector")
            .is_none()
    );
    let before_regression = restored.read().dump().expect("dump");
    let mut regressed = request(
        3,
        QuantityOperation::Void {
            reservation_id: "pending".into(),
            resolution: norn_ir::quantity_ledger::QuantityResolution::All,
        },
    );
    regressed.balance_versions = restored
        .read()
        .quantity_balance_versions("main", &regressed)
        .expect("fences");
    assert!(matches!(
        restored.commit(&CommitBatch {
            world: "main".into(),
            scope: "tenant/dev".into(),
            quantity: Some(regressed),
            idempotency_key: Some("regressed".into()),
            ..CommitBatch::default()
        }),
        Err(CellError::Quantity(QuantityError::ClockRegression))
    ));
    assert_eq!(
        before_regression,
        restored.read().dump().expect("unchanged")
    );
    let mut cell: SqliteCell<String, String, String> =
        SqliteCell::new(restored, "tenant/dev", |_| "ok".into()).expect("host");
    assert_eq!(
        receipt.receipt(),
        cell.commit_quantity_fenced(&r, "reserve")
            .expect("replay after restore")
            .receipt()
    );
    assert_eq!(cell.now().as_millis(), 5);
    cell.advance_to(LogicalTime::from_millis(10));
    assert!(
        cell.expire_quantity_due()
            .expect("scheduled expiry after restore")
    );
    assert!(!cell.expire_quantity_due().expect("expiry consumed once"));
    assert_eq!(
        cell.store().read().next_timer_due().expect("idle alarm"),
        None
    );
    assert_eq!(amounts(&cell, "lot"), (40, 0));
}

#[test]
fn crash_before_commit_rolls_back_and_lost_reply_replays_once() {
    let mut cell = initialized(20);
    let req = reserve(2, "a", 10, 100);
    let mut batch = CommitBatch {
        world: "main".into(),
        scope: "tenant/dev".into(),
        quantity: Some(req.clone()),
        idempotency_key: Some("fault".into()),
        ..CommitBatch::default()
    };
    // Use a store directly to exercise the established deterministic fault seams.
    let mut store = CellStore::open_in_memory(model()).expect("store");
    store.bind("scope", "tenant/dev").expect("bind");
    for (id, req) in [("define", define()), ("fund", post(1, "fund", "fund", 20))] {
        store
            .commit_fenced(&CommitBatch {
                quantity: Some(req),
                idempotency_key: Some(id.into()),
                ..batch.clone()
            })
            .expect("setup");
    }
    batch.quantity.as_mut().expect("quantity").balance_versions = store
        .read()
        .quantity_balance_versions("main", &req)
        .expect("fences");
    let before = store.read().dump().expect("dump");
    assert!(
        store
            .commit_probed(
                &batch,
                &mut |point| if point == CommitPoint::EffectsEnqueued {
                    Err(CellError::Engine("crash".into()))
                } else {
                    Ok(())
                }
            )
            .is_err()
    );
    assert_eq!(before, store.read().dump().expect("rollback"));
    assert!(
        store
            .commit_probed(&batch, &mut |point| if point == CommitPoint::Committed {
                Err(CellError::Engine("lost reply".into()))
            } else {
                Ok(())
            })
            .is_err()
    );
    assert!(matches!(
        store.commit(&batch).expect("reconcile"),
        CommitOutcome::Replayed(_)
    ));
    cell.commit_quantity_fenced(&req, "reserve")
        .expect("fixture remains usable");
}

proptest! {
    #[test]
    fn generated_reservation_sequences_match_independent_scalar_fold(funded in 0_i64..1_000_000, held in 0_i64..1_000_000, resolution in 0_u8..3) {
        let mut cell = initialized(i128::from(funded));
        let before = cell.store().read().dump().expect("dump");
        let result = cell.commit_quantity_fenced(&reserve(2, "r", i128::from(held), 10), "reserve");
        if held > funded {
            prop_assert!(matches!(result, Err(CellError::Quantity(QuantityError::InsufficientAvailable { .. }))), "insufficient available must refuse");
            prop_assert_eq!(before, cell.store().read().dump().expect("unchanged"));
        } else {
            result.expect("admit");
            let op = match resolution {
                0 => QuantityOperation::PostReservation { reservation_id: "r".into(), posting_id: "p".into(), resolution: norn_ir::quantity_ledger::QuantityResolution::All, },
                1 => QuantityOperation::Void { reservation_id: "r".into(), resolution: norn_ir::quantity_ledger::QuantityResolution::All, },
                2..=u8::MAX => { cell.advance_to(LogicalTime::from_millis(10)); QuantityOperation::Expire { reservation_id: "r".into() } },
            };
            cell.commit_quantity_fenced(&request(3, op), "resolve").expect("resolve");
            let posted = if resolution == 0 { i128::from(held) } else { 0 };
            prop_assert_eq!(amounts(&cell, "lot"), (i128::from(funded).checked_sub(posted).expect("bounded model"), 0));
            prop_assert_eq!(amounts(&cell, "transit"), (posted, 0));
            prop_assert_eq!(amounts(&cell, "issuance").0.checked_add(amounts(&cell, "lot").0).expect("bounded model").checked_add(amounts(&cell, "transit").0).expect("bounded model"), 0);
        }
    }
}

#[test]
fn quantity_and_ordinary_commit_succeed_and_retry_as_one_receipt() {
    let mut cell = initialized(20);
    let mut commit = CellCommit::empty();
    commit.events.push("shipped".into());
    commit.effects = effects();
    commit.writes.push(FactWrite {
        key: FactKey::new("fixture.Marker", "a"),
        expected: FactVersion::ABSENT,
        mutation: FactMutation::Put {
            values: BTreeMap::from([(
                "label".into(),
                norn_ir::RuntimeValue::String("shipped".into()),
            )]),
        },
    });
    let quantity = post(2, "ship", "ship", 10);
    let first = cell
        .commit_with_quantity_fenced(commit.clone(), &quantity, "atomic")
        .expect("atomic commit");
    assert_eq!(amounts(&cell, "lot"), (10, 0));
    assert_eq!(cell.pending_commands().expect("outbox").len(), 1);
    assert_eq!(cell.pending_timers().expect("timer").len(), 1);
    let before = cell.store().read().dump().expect("dump");
    assert_eq!(
        first,
        cell.commit_with_quantity_fenced(commit, &quantity, "atomic")
            .expect("replay receipt")
    );
    assert_eq!(before, cell.store().read().dump().expect("unchanged"));
}

#[test]
fn corrupted_touched_head_refuses_receipt_replay() {
    use norn_cell::engine::{NativeEngine, SqlEngine};
    let dir = tempfile::tempdir().expect("directory");
    let path = dir.path().join("cell.sqlite");
    let store = CellStore::open(&path, model(), StoreOptions::default()).expect("store");
    let mut cell: SqliteCell<String, String, String> =
        SqliteCell::new(store, "tenant/dev", |_| "ok".into()).expect("host");
    cell.commit_quantity_fenced(&define(), "define")
        .expect("define");
    let quantity = post(1, "fund", "fund", 20);
    cell.commit_quantity_fenced(&quantity, "fund")
        .expect("fund");
    // Keep the verified handle open to retain the original live-damage check.
    let engine = NativeEngine::open(&path, StoreOptions::default()).expect("engine");
    engine
        .execute(
            r#"UPDATE "_norn_quantity_balance" SET "posted"='21' WHERE "account"='lot'"#,
            &[],
        )
        .expect("fault injection");
    drop(engine);

    refused(
        &mut cell,
        &post(1, "other", "fund", 20),
        &QuantityError::Corrupt,
    );
    assert!(matches!(
        cell.commit_quantity_fenced(&quantity, "fund"),
        Err(CellError::Quantity(QuantityError::Corrupt))
    ));
    drop(cell);
    assert!(CellStore::open(&path, model(), StoreOptions::default()).is_err());
}

#[test]
fn quantity_scope_must_match_the_file_binding() {
    let mut store = CellStore::open_in_memory(model()).expect("store");
    store.bind("scope", "tenant/dev").expect("bind");
    let before = store.read().dump().expect("dump");
    let batch = CommitBatch {
        world: "main".into(),
        scope: "another/dev".into(),
        quantity: Some(define()),
        idempotency_key: Some("define".into()),
        ..CommitBatch::default()
    };
    assert!(matches!(
        store.commit(&batch),
        Err(CellError::Quantity(QuantityError::InvalidContract))
    ));
    assert_eq!(before, store.read().dump().expect("unchanged"));
    let mut admitted = batch.clone();
    admitted.scope = "tenant/dev".into();
    store.commit(&admitted).expect("scoped definition");
    let after = store.read().dump().expect("dump");
    assert!(matches!(
        store.commit(&batch),
        Err(CellError::Quantity(QuantityError::InvalidContract))
    ));
    assert_eq!(
        after,
        store.read().dump().expect("no cross-scope receipt replay")
    );
}

#[test]
fn pack_identity_corruption_cannot_reinterpret_existing_quantities() {
    use norn_cell::engine::{NativeEngine, SqlEngine};
    let dir = tempfile::tempdir().expect("directory");
    let path = dir.path().join("cell.sqlite");
    let store = CellStore::open(&path, model(), StoreOptions::default()).expect("store");
    let mut cell: SqliteCell<String, String, String> =
        SqliteCell::new(store, "tenant/dev", |_| "ok".into()).expect("host");
    cell.commit_quantity_fenced(&define(), "define")
        .expect("define");
    let quantity = post(1, "fund", "fund", 20);
    cell.commit_quantity_fenced(&quantity, "fund")
        .expect("fund");
    let engine = NativeEngine::open(&path, StoreOptions::default()).expect("engine");
    let mut changed = quantity_support::pack();
    changed.ontology_artifact.artifact_version = "2".into();
    engine
        .execute(
            r#"UPDATE "_norn_quantity_ledger" SET "pack"=?1 WHERE "ledger"='stock'"#,
            &[norn_cell::engine::SqlValue::from(
                serde_json::to_string(&changed).expect("encode"),
            )],
        )
        .expect("fault injection");
    drop(engine);
    assert!(matches!(
        cell.commit_quantity_fenced(&quantity, "fund"),
        Err(CellError::Quantity(QuantityError::Corrupt))
    ));
    refused(
        &mut cell,
        &post(2, "next", "ship", 1),
        &QuantityError::Corrupt,
    );
    drop(cell);
    assert!(matches!(
        CellStore::open(&path, model(), StoreOptions::default()),
        Err(CellError::Quantity(QuantityError::Corrupt))
    ));
}

#[test]
fn persisted_reservation_vector_deadline_and_status_remain_pinned() {
    use norn_cell::engine::{NativeEngine, SqlEngine, SqlValue};
    #[derive(Debug)]
    enum Damage {
        Vector,
        Deadline,
        Status,
    }
    let dir = tempfile::tempdir().expect("directory");
    for case in [Damage::Vector, Damage::Deadline, Damage::Status] {
        let path = dir.path().join(format!("{case:?}.sqlite"));
        let store = CellStore::open(&path, model(), StoreOptions::default()).expect("store");
        let mut cell: SqliteCell<String, String, String> =
            SqliteCell::new(store, "tenant/dev", |_| "ok".into()).expect("host");
        cell.commit_quantity_fenced(&define(), "define")
            .expect("define");
        cell.commit_quantity_fenced(&post(1, "fund", "fund", 20), "fund")
            .expect("fund");
        let original = reserve(2, "r", 10, 100);
        cell.commit_quantity_fenced(&original, "reserve")
            .expect("reserve");
        let mut record = cell
            .store()
            .read()
            .quantity_reservation("main", "stock", "r")
            .expect("read")
            .expect("reservation");
        match case {
            Damage::Vector => {
                for leg in &mut record.legs {
                    leg.amount_unscaled = if leg.leg_id == "out" { "-1" } else { "1" }.into();
                }
            }
            Damage::Deadline => record.expires_at = 101,
            Damage::Status => record.status = "posted".into(),
        }
        let engine = NativeEngine::open(&path, StoreOptions::default()).expect("engine");
        engine
            .execute(
                r#"UPDATE "_norn_quantity_reservation" SET "record"=?1 WHERE "id"='r'"#,
                &[SqlValue::from(
                    serde_json::to_string(&record).expect("encode"),
                )],
            )
            .expect("fault injection");
        drop(engine);
        refused(
            &mut cell,
            &request(
                3,
                QuantityOperation::PostReservation {
                    reservation_id: "r".into(),
                    posting_id: "post".into(),
                    resolution: norn_ir::quantity_ledger::QuantityResolution::All,
                },
            ),
            &QuantityError::Corrupt,
        );
        assert!(matches!(
            cell.commit_quantity_fenced(&original, "reserve"),
            Err(CellError::Quantity(QuantityError::Corrupt))
        ));
        drop(cell);
        assert!(matches!(
            CellStore::open(&path, model(), StoreOptions::default()),
            Err(CellError::Quantity(QuantityError::Corrupt))
        ));
    }
}

#[test]
fn reversal_refuses_a_balanced_but_corrupted_original_vector() {
    use norn_cell::engine::{NativeEngine, SqlEngine, SqlValue};
    let dir = tempfile::tempdir().expect("directory");
    let path = dir.path().join("cell.sqlite");
    let store = CellStore::open(&path, model(), StoreOptions::default()).expect("store");
    let mut cell: SqliteCell<String, String, String> =
        SqliteCell::new(store, "tenant/dev", |_| "ok".into()).expect("host");
    cell.commit_quantity_fenced(&define(), "define")
        .expect("define");
    cell.commit_quantity_fenced(&post(1, "fund", "fund", 20), "fund")
        .expect("fund");
    cell.commit_quantity_fenced(&post(2, "ship", "ship", 10), "ship")
        .expect("ship");
    let mut original = cell
        .store()
        .read()
        .quantity_posting("main", "stock", "ship")
        .expect("read")
        .expect("posting");
    for leg in &mut original.legs {
        leg.amount_unscaled = if leg.leg_id == "out" { "-1" } else { "1" }.into();
    }
    // Keep the verified handle open to retain the original live-damage check.
    let engine = NativeEngine::open(&path, StoreOptions::default()).expect("engine");
    engine
        .execute(
            r#"UPDATE "_norn_quantity_posting" SET "record"=?1 WHERE "id"='ship'"#,
            &[SqlValue::from(
                serde_json::to_string(&original).expect("encode"),
            )],
        )
        .expect("fault injection");
    drop(engine);
    refused(
        &mut cell,
        &request(
            3,
            QuantityOperation::Reverse {
                original_id: "ship".into(),
                posting_id: "undo".into(),
            },
        ),
        &QuantityError::Corrupt,
    );
    drop(cell);
    assert!(CellStore::open(&path, model(), StoreOptions::default()).is_err());
}

#[test]
fn reversal_projection_damage_refuses_reopen_replay_and_second_reversal() {
    let dir = tempfile::tempdir().expect("directory");
    for (case, damage) in [
        (
            "null_link",
            r#"UPDATE "_norn_quantity_posting" SET "reverses"=NULL WHERE "id"='u'"#,
        ),
        (
            "wrong_link",
            r#"UPDATE "_norn_quantity_posting" SET "reverses"='b' WHERE "id"='u'"#,
        ),
        (
            "missing_row",
            r#"DELETE FROM "_norn_quantity_posting" WHERE "id"='u'"#,
        ),
        (
            "paired_link",
            r#"UPDATE "_norn_quantity_posting" SET "reverses"='b', "record"=json_set("record", '$.reverses', 'b') WHERE "id"='u'"#,
        ),
    ] {
        let path = dir.path().join(format!("{case}.sqlite"));
        assert_reversal_projection_damage(&path, damage, case);
    }
}

fn assert_reversal_projection_damage(path: &std::path::Path, damage: &str, case: &str) {
    use norn_cell::engine::{NativeEngine, SqlEngine};
    let store = CellStore::open(path, model(), StoreOptions::default()).expect("store");
    let mut cell: SqliteCell<String, String, String> =
        SqliteCell::new(store, "tenant/dev", |_| "ok".into()).expect("host");
    cell.commit_quantity_fenced(&define(), "define")
        .expect("define");
    cell.commit_quantity_fenced(&post(1, "fund", "fund", 100), "fund")
        .expect("fund");
    cell.commit_quantity_fenced(&post(2, "a", "ship", 20), "a")
        .expect("post A");
    let reversal = request(
        3,
        QuantityOperation::Reverse {
            original_id: "a".into(),
            posting_id: "u".into(),
        },
    );
    let receipt = cell
        .commit_quantity_fenced(&reversal, "u")
        .expect("reverse A");
    cell.commit_quantity_fenced(&post(4, "b", "ship", 20), "b")
        .expect("post B");
    assert_eq!(amounts(&cell, "lot"), (80, 0));
    assert_eq!(amounts(&cell, "transit"), (20, 0));
    drop(cell);
    let store = CellStore::open(path, model(), StoreOptions::default()).expect("intact reopen");
    let mut cell: SqliteCell<String, String, String> =
        SqliteCell::new(store, "tenant/dev", |_| "ok".into()).expect("host");
    assert_eq!(
        cell.commit_quantity_fenced(&reversal, "u")
            .expect("intact replay")
            .receipt(),
        receipt.receipt()
    );
    let engine = NativeEngine::open(path, StoreOptions::default()).expect("engine");
    engine.execute(damage, &[]).expect("fault injection");
    let before = cell.store().read().dump().expect("damaged dump");
    assert!(
        matches!(
            cell.commit_quantity_fenced(&reversal, "u"),
            Err(CellError::Quantity(QuantityError::Corrupt))
        ),
        "damaged replay: {case}"
    );
    assert_eq!(
        before,
        cell.store().read().dump().expect("no replay mutation")
    );
    refused(
        &mut cell,
        &request(
            5,
            QuantityOperation::Reverse {
                original_id: "a".into(),
                posting_id: "again".into(),
            },
        ),
        &QuantityError::Corrupt,
    );
    assert!(
        matches!(
            cell.store().read().verify_ledger(),
            Err(CellError::Quantity(QuantityError::Corrupt))
        ),
        "damaged ledger verification: {case}"
    );
    drop(cell);
    assert!(
        matches!(
            CellStore::open(path, model(), StoreOptions::default()),
            Err(CellError::Quantity(QuantityError::Corrupt))
        ),
        "damaged reopen: {case}"
    );
    drop(engine);
}

#[test]
fn unicode_identity_cannot_change_between_sql_and_immutable_events() {
    let mut cell = host();
    let mut noncanonical = define();
    noncanonical.ledger_id = "e\u{301}".into();
    refused(&mut cell, &noncanonical, &QuantityError::InvalidContract);
    let mut canonical = noncanonical.clone();
    canonical.ledger_id = "é".into();
    let first = cell
        .commit_quantity_fenced(&canonical, "unicode")
        .expect("canonical identity");
    let before = cell.store().read().dump().expect("dump");
    assert!(matches!(
        cell.commit_quantity_fenced(&noncanonical, "unicode"),
        Err(CellError::Quantity(QuantityError::InvalidContract))
    ));
    assert_eq!(before, cell.store().read().dump().expect("unchanged"));
    assert_eq!(
        first.receipt(),
        cell.commit_quantity_fenced(&canonical, "unicode")
            .expect("exact canonical retry")
            .receipt()
    );
    let mut funding = post(1, "fund", "fund", 20);
    funding.ledger_id = "é".into();
    cell.commit_quantity_fenced(&funding, "fund")
        .expect("SQL/event identity agrees");
}

#[test]
fn deadline_must_be_reachable_by_the_admitted_clock() {
    let mut cell = initialized(20);
    refused(
        &mut cell,
        &reserve(2, "unreachable", 10, u64::MAX),
        &QuantityError::InvalidContract,
    );
}

#[test]
fn surplus_balance_and_history_cannot_create_unearned_funding_at_reopen() {
    use norn_cell::engine::{NativeEngine, SqlEngine};
    let dir = tempfile::tempdir().expect("directory");
    let path = dir.path().join("cell.sqlite");
    let store = CellStore::open(&path, model(), StoreOptions::default()).expect("store");
    let mut cell: SqliteCell<String, String, String> =
        SqliteCell::new(store, "tenant/dev", |_| "ok".into()).expect("host");
    cell.commit_quantity_fenced(&define(), "define")
        .expect("define");
    drop(cell);
    let engine = NativeEngine::open(&path, StoreOptions::default()).expect("engine");
    engine
        .execute_batch(
            r#"
        INSERT INTO "_norn_quantity_balance"
          (world,ledger,account,unit,layer,posted,held,version)
          VALUES ('main','stock','lot','item','physical','20','0',1);
        INSERT INTO "_norn_quantity_history"
          (world,ledger,account,unit,layer,posted,held,version,commit_seq)
          VALUES ('main','stock','lot','item','physical','20','0',1,1);
    "#,
        )
        .expect("r5 surplus counterexample");
    drop(engine);
    assert!(CellStore::open(&path, model(), StoreOptions::default()).is_err());
}

#[test]
fn reopen_requires_exact_quantity_projection_sets_and_all_historical_rows() {
    use norn_cell::engine::{NativeEngine, SqlEngine};
    // Each fixture retains the original immutable transitions and receipts.
    // Include coordinated deletion/change, historical-only damage, and surplus
    // identities in otherwise untouched worlds, ledgers and balance partitions.
    for damage in [
        r#"DELETE FROM "_norn_quantity_ledger""#,
        r#"UPDATE "_norn_quantity_ledger" SET version=99"#,
        r#"UPDATE "_norn_clock" SET logical_time=99"#,
        r#"UPDATE "_norn_quantity_ledger" SET defined_at=2"#,
        r#"INSERT INTO "_norn_quantity_ledger" SELECT 'other',ledger,version,pack,unrestricted,defined_at FROM "_norn_quantity_ledger""#,
        r#"DELETE FROM "_norn_quantity_balance"; DELETE FROM "_norn_quantity_history""#,
        r#"UPDATE "_norn_quantity_balance" SET posted='200'; UPDATE "_norn_quantity_history" SET posted='200'"#,
        r#"UPDATE "_norn_quantity_balance" SET held='0'"#,
        r#"UPDATE "_norn_quantity_balance" SET version=1"#,
        r#"INSERT INTO "_norn_quantity_balance" SELECT world,ledger,'extra',unit,layer,posted,held,version FROM "_norn_quantity_balance" WHERE account='lot'"#,
        r#"DELETE FROM "_norn_quantity_history" WHERE version=2"#,
        r#"UPDATE "_norn_quantity_history" SET posted='200' WHERE version=2"#,
        r#"UPDATE "_norn_quantity_history" SET commit_seq=99 WHERE version=2"#,
        r#"INSERT INTO "_norn_quantity_history" SELECT 'other',ledger,account,unit,layer,posted,held,version,commit_seq FROM "_norn_quantity_history""#,
        r#"DELETE FROM "_norn_quantity_reservation""#,
        r#"UPDATE "_norn_quantity_reservation" SET created_at=2"#,
        r#"UPDATE "_norn_quantity_reservation" SET record=json_set(record,'$.status','void')"#,
        r#"INSERT INTO "_norn_quantity_reservation" SELECT world,ledger,'extra',record,version,commit_seq,created_at FROM "_norn_quantity_reservation""#,
        r#"DELETE FROM "_norn_quantity_posting""#,
        r#"UPDATE "_norn_quantity_posting" SET record=json_set(record,'$.legs[0].amountUnscaled','999')"#,
        r#"INSERT INTO "_norn_quantity_posting" SELECT 'other',ledger,id,record,reverses,version,commit_seq FROM "_norn_quantity_posting""#,
    ] {
        let dir = tempfile::tempdir().expect("directory");
        let path = dir.path().join("cell.sqlite");
        let store = CellStore::open(&path, model(), StoreOptions::default()).expect("store");
        let mut cell: SqliteCell<String, String, String> =
            SqliteCell::new(store, "tenant/dev", |_| "ok".into()).expect("host");
        cell.commit_quantity_fenced(&define(), "define")
            .expect("define");
        cell.commit_quantity_fenced(&post(1, "fund", "fund", 20), "fund")
            .expect("fund");
        cell.commit_quantity_fenced(&reserve(2, "hold", 10, 100), "hold")
            .expect("reserve");
        drop(cell);
        CellStore::open(&path, model(), StoreOptions::default()).expect("intact fold");
        let engine = NativeEngine::open(&path, StoreOptions::default()).expect("engine");
        engine.execute_batch(damage).expect("projection damage");
        let before: Vec<_> = [
            "_norn_quantity_ledger",
            "_norn_quantity_balance",
            "_norn_quantity_history",
            "_norn_quantity_reservation",
            "_norn_quantity_posting",
            "_norn_ledger",
            "_norn_receipt",
            "_norn_export_journal",
        ]
        .iter()
        .map(|table| {
            engine
                .query(&format!("SELECT * FROM \"{table}\""), &[])
                .expect("snapshot")
        })
        .collect();
        assert!(
            CellStore::open(&path, model(), StoreOptions::default()).is_err(),
            "{damage}"
        );
        let after: Vec<_> = [
            "_norn_quantity_ledger",
            "_norn_quantity_balance",
            "_norn_quantity_history",
            "_norn_quantity_reservation",
            "_norn_quantity_posting",
            "_norn_ledger",
            "_norn_receipt",
            "_norn_export_journal",
        ]
        .iter()
        .map(|table| {
            engine
                .query(&format!("SELECT * FROM \"{table}\""), &[])
                .expect("snapshot")
        })
        .collect();
        assert_eq!(before, after, "refusal must not repair caches: {damage}");
    }
}

#[test]
fn sub_256_kib_pack_amplification_refuses_with_bounded_diagnostics_and_no_commit() {
    use norn_checker::quantity_ledger::{
        MAX_LEDGER_DIAGNOSTIC_BYTES, MAX_LEDGER_DIAGNOSTICS, compile_ledger_pack, diagnostic_codes,
    };
    let mut malicious = quantity_support::pack();
    malicious.pack_id = "x".repeat(131_072);
    malicious.accounts[0].allowed_unit_ids = vec!["item".into(); 16_000];
    let malformed = request(
        0,
        QuantityOperation::Define {
            account_limit: 128,
            pack: malicious,
            unrestricted_accounts: vec!["issuance".into()],
        },
    );
    let bytes = norn_ir::canonical::canonical_json_of(&malformed).expect("canonical request");
    assert!(
        bytes.len() < 256 * 1024,
        "regression must fit the request byte budget"
    );
    let QuantityOperation::Define { pack, .. } = &malformed.operation else {
        unreachable!("fixture defines a pack")
    };
    // Standalone checker callers also get a bounded, typed refusal. This is
    // safe to exercise: no complete diagnostic set is ever retained.
    let diagnostics = compile_ledger_pack(pack)
        .expect_err("bounded checker refusal")
        .diagnostics;
    assert_eq!(diagnostics.len(), 1);
    assert!(diagnostics.len() <= MAX_LEDGER_DIAGNOSTICS);
    assert!(
        serde_json::to_vec(&diagnostics)
            .expect("diagnostic JSON")
            .len()
            <= MAX_LEDGER_DIAGNOSTIC_BYTES
    );
    assert_eq!(diagnostics[0].code, diagnostic_codes::TOO_MANY_DIAGNOSTICS);
    let mut cell = host();
    assert!(matches!(
        cell.commit_quantity_fenced(&malformed, "malformed"),
        Err(CellError::Quantity(QuantityError::InvalidContract))
    ));
    cell.commit_quantity_fenced(&define(), "malformed")
        .expect("refusal leaves ledger and retry identity untouched");
}

#[test]
fn conserving_partial_post_and_void_preserve_identity_deadline_and_retry() {
    use norn_ir::quantity_ledger::{QuantityResolution, QuantitySlice};
    let mut cell = initialized(100);
    cell.commit_quantity_fenced(&reserve(2, "partial", 60, 100), "reserve-partial")
        .expect("reserve");
    let subset = |q: i128| QuantityResolution::Subset {
        legs: vec![
            QuantitySlice {
                leg_id: "out".into(),
                amount_unscaled: q
                    .checked_neg()
                    .expect("bounded negative amount")
                    .to_string(),
            },
            QuantitySlice {
                leg_id: "in".into(),
                amount_unscaled: q.to_string(),
            },
        ],
    };
    let posting = request(
        3,
        QuantityOperation::PostReservation {
            reservation_id: "partial".into(),
            posting_id: "partial-post".into(),
            resolution: subset(20),
        },
    );
    let first = cell
        .commit_quantity_fenced(&posting, "partial-post")
        .expect("partial post");
    let remaining = cell
        .store()
        .read()
        .quantity_reservation("main", "stock", "partial")
        .expect("read")
        .expect("reservation");
    assert_eq!(remaining.status, "open");
    assert_eq!(remaining.expires_at, 100);
    assert_eq!(
        remaining
            .remaining
            .first()
            .expect("remaining leg")
            .amount_unscaled,
        "-40"
    );
    cell.commit_quantity_fenced(
        &request(
            4,
            QuantityOperation::Void {
                reservation_id: "partial".into(),
                resolution: subset(10),
            },
        ),
        "partial-void",
    )
    .expect("partial void");
    assert_eq!(
        cell.commit_quantity_fenced(&posting, "partial-post")
            .expect("retry after resolution")
            .receipt(),
        first.receipt()
    );
    cell.commit_quantity_fenced(
        &request(
            5,
            QuantityOperation::PostReservation {
                reservation_id: "partial".into(),
                posting_id: "finish-partial".into(),
                resolution: QuantityResolution::All,
            },
        ),
        "finish-partial",
    )
    .expect("finish");
    cell.store().read().verify_ledger().expect("immutable fold");
}

trait QuantityFixture {
    fn commit_with_quantity_fenced(
        &mut self,
        commit: CellCommit<String, String, String>,
        request: &norn_ir::quantity_ledger::QuantityRequest,
        key: &str,
    ) -> Result<norn_kernel::CommitReceipt, norn_kernel::CommitError<CellError>>;

    fn commit_quantity_fenced(
        &mut self,
        request: &norn_ir::quantity_ledger::QuantityRequest,
        key: &str,
    ) -> Result<CommitOutcome, CellError>;
}
impl QuantityFixture for SqliteCell<String, String, String> {
    fn commit_with_quantity_fenced(
        &mut self,
        commit: CellCommit<String, String, String>,
        request: &norn_ir::quantity_ledger::QuantityRequest,
        key: &str,
    ) -> Result<norn_kernel::CommitReceipt, norn_kernel::CommitError<CellError>> {
        let mut request = request.clone();
        if let Some(prior) = self
            .store()
            .read()
            .receipt_by_idempotency_key("main", key)
            .map_err(norn_kernel::CommitError::Host)?
        {
            if let Some(original) = prior.body.quantity {
                request.balance_versions = original.balance_versions;
            }
        } else {
            request.balance_versions = self
                .store()
                .read()
                .quantity_balance_versions("main", &request)
                .map_err(norn_kernel::CommitError::Host)?;
        }
        self.commit_with_quantity(commit, &request, key)
    }

    fn commit_quantity_fenced(
        &mut self,
        request: &norn_ir::quantity_ledger::QuantityRequest,
        key: &str,
    ) -> Result<CommitOutcome, CellError> {
        let mut request = request.clone();
        if let Some(prior) = self
            .store()
            .read()
            .receipt_by_idempotency_key("main", key)?
        {
            if let Some(original) = prior.body.quantity {
                request.balance_versions = original.balance_versions;
            }
        } else if request.balance_versions.is_empty()
            && let Ok(fences) = self
                .store()
                .read()
                .quantity_balance_versions("main", &request)
        {
            request.balance_versions = fences;
        }
        self.commit_quantity(&request, key)
    }
}
trait QuantityStoreFixture {
    fn commit_fenced(&mut self, batch: &CommitBatch) -> Result<CommitOutcome, CellError>;
}
impl QuantityStoreFixture for CellStore {
    fn commit_fenced(&mut self, batch: &CommitBatch) -> Result<CommitOutcome, CellError> {
        let mut batch = batch.clone();
        if let Some(request) = &mut batch.quantity
            && let Ok(fences) = self.read().quantity_balance_versions(&batch.world, request)
        {
            request.balance_versions = fences;
        }
        self.commit(&batch)
    }
}

fn two_lot_definition() -> norn_ir::quantity_ledger::QuantityRequest {
    let mut definition = define();
    if let QuantityOperation::Define { pack, .. } = &mut definition.operation {
        for account in ["lot2", "transit2"] {
            let mut added = pack.accounts.get(1).expect("lot account").clone();
            added.account_id = account.into();
            pack.accounts.push(added);
        }
        for (template, source, destination) in
            [("fund2", "issuance", "lot2"), ("ship2", "lot2", "transit2")]
        {
            let mut added = pack.templates.first().expect("fund template").clone();
            added.template_id = template.into();
            added.legs.first_mut().expect("outgoing leg").account_id = source.into();
            added.legs.get_mut(1).expect("incoming leg").account_id = destination.into();
            pack.templates.push(added);
        }
    }
    definition
}

fn partial(q: i128) -> norn_ir::quantity_ledger::QuantityResolution {
    use norn_ir::quantity_ledger::{QuantityResolution, QuantitySlice};
    QuantityResolution::Subset {
        legs: vec![
            QuantitySlice {
                leg_id: "out".into(),
                amount_unscaled: q
                    .checked_neg()
                    .expect("bounded negative amount")
                    .to_string(),
            },
            QuantitySlice {
                leg_id: "in".into(),
                amount_unscaled: q.to_string(),
            },
        ],
    }
}

proptest! {
    #[test]
    fn partial_resolution_interleaved_lots_conserve_independent_scalar_fold(a in 2_i64..100_000, b in 2_i64..100_000, split in 1_i64..100_000) {
        let (a,b) = (i128::from(a), i128::from(b));
        let mut cell = host();
        cell.commit_quantity_fenced(&two_lot_definition(), "define").expect("define");
        cell.commit_quantity_fenced(&post(0,"fund1","fund",a), "fund1").expect("fund1");
        cell.commit_quantity_fenced(&post(0,"fund2","fund2",b), "fund2").expect("fund2");
        cell.commit_quantity_fenced(&reserve(0,"r1",a,100), "r1").expect("reserve1");
        let mut r2 = reserve(0,"r2",b,100);
        if let QuantityOperation::Reserve { template_id, .. } = &mut r2.operation { *template_id = "ship2".into(); }
        cell.commit_quantity_fenced(&r2, "r2").expect("reserve2");
        let shipped = i128::from(split).checked_rem(a.checked_sub(1).expect("bounded divisor")).and_then(|q| q.checked_add(1)).expect("bounded split");
        let voided = i128::from(split).checked_rem(b.checked_sub(1).expect("bounded divisor")).and_then(|q| q.checked_add(1)).expect("bounded split");
        cell.commit_quantity_fenced(&request(0,QuantityOperation::PostReservation { reservation_id:"r1".into(), posting_id:"p1".into(), resolution:partial(shipped) }),"p1").expect("partial post");
        cell.commit_quantity_fenced(&request(0,QuantityOperation::Void { reservation_id:"r2".into(), resolution:partial(voided) }),"v2").expect("partial void");
        prop_assert_eq!(amounts(&cell,"lot"),(a.checked_sub(shipped).expect("bounded remainder"),a.checked_sub(shipped).expect("bounded remainder")));
        prop_assert_eq!(amounts(&cell,"transit"),(shipped,0));
        prop_assert_eq!(amounts(&cell,"lot2"),(b,b.checked_sub(voided).expect("bounded remainder")));
        prop_assert_eq!(amounts(&cell,"transit2"),(0,0));
        cell.advance_to(LogicalTime::from_millis(100));
        prop_assert!(cell.expire_quantity_due().expect("expiry1"));
        prop_assert!(cell.expire_quantity_due().expect("expiry2"));
        prop_assert!(!cell.expire_quantity_due().expect("drained"));
        let sum: i128 = ["issuance","lot","transit","lot2","transit2"].iter().map(|account| amounts(&cell,account).0).sum();
        prop_assert_eq!(sum,0);
        prop_assert_eq!(amounts(&cell,"lot").1.checked_add(amounts(&cell,"lot2").1).expect("bounded holds"),0);
        cell.store().read().verify_ledger().expect("immutable history fold");
    }
}

#[test]
fn disjoint_lots_progress_from_shared_snapshot_same_lot_stale_fence_refuses() {
    let dir = tempfile::tempdir().expect("directory");
    let path = dir.path().join("concurrency.sqlite");
    let mut first: SqliteCell<String, String, String> = SqliteCell::new(
        CellStore::open(&path, model(), StoreOptions::default()).expect("store"),
        "tenant/dev",
        |_| "ok".into(),
    )
    .expect("host");
    first
        .commit_quantity_fenced(&two_lot_definition(), "define")
        .expect("define");
    first
        .commit_quantity_fenced(&post(0, "fund1", "fund", 100), "fund1")
        .expect("fund1");
    first
        .commit_quantity_fenced(&post(0, "fund2", "fund2", 100), "fund2")
        .expect("fund2");
    let mut second: SqliteCell<String, String, String> = SqliteCell::new(
        CellStore::open(&path, model(), StoreOptions::default()).expect("second store"),
        "tenant/dev",
        |_| "ok".into(),
    )
    .expect("second host");
    let mut a = post(0, "a", "ship", 10);
    let mut b = post(0, "b", "ship2", 10);
    a.balance_versions = first
        .store()
        .read()
        .quantity_balance_versions("main", &a)
        .expect("snapshot a");
    b.balance_versions = second
        .store()
        .read()
        .quantity_balance_versions("main", &b)
        .expect("snapshot b");
    let mut conflicting = a.clone();
    if let QuantityOperation::Post { posting_id, .. } = &mut conflicting.operation {
        *posting_id = "conflict".into();
    }
    first.commit_quantity(&a, "a").expect("first lot");
    second
        .commit_quantity(&b, "b")
        .expect("disjoint lot uses original fences");
    let before = second.store().read().dump().expect("dump");
    assert!(matches!(
        second.commit_quantity(&conflicting, "conflict"),
        Err(CellError::Quantity(QuantityError::StaleVersion { .. }))
    ));
    assert_eq!(before, second.store().read().dump().expect("unchanged"));
    second
        .store()
        .read()
        .verify_ledger()
        .expect("fold independent versions");
}

#[test]
fn lazy_deadline_refuses_post_and_scheduled_expiry_consumes_timer_atomically() {
    let mut cell = initialized(100);
    cell.commit_quantity_fenced(&reserve(0, "deadline", 60, 10), "deadline")
        .expect("reserve");
    cell.commit_quantity_fenced(
        &request(
            0,
            QuantityOperation::PostReservation {
                reservation_id: "deadline".into(),
                posting_id: "part".into(),
                resolution: partial(20),
            },
        ),
        "part",
    )
    .expect("partial post");
    let mut expired = request(
        0,
        QuantityOperation::PostReservation {
            reservation_id: "deadline".into(),
            posting_id: "late".into(),
            resolution: norn_ir::quantity_ledger::QuantityResolution::All,
        },
    );
    expired.balance_versions = cell
        .store()
        .read()
        .quantity_balance_versions("main", &expired)
        .expect("fences");
    cell.advance_to(LogicalTime::from_millis(10));
    let before = cell.store().read().dump().expect("dump");
    assert!(matches!(
        cell.commit_quantity(&expired, "late"),
        Err(CellError::Quantity(QuantityError::Expired))
    ));
    assert_eq!(
        before,
        cell.store()
            .read()
            .dump()
            .expect("lazy refusal has no mutation")
    );
    assert!(cell.expire_quantity_due().expect("runtime submits expiry"));
    assert_eq!(amounts(&cell, "lot"), (80, 0));
    assert_eq!(
        cell.store()
            .read()
            .quantity_reservation("main", "stock", "deadline")
            .expect("read")
            .expect("reservation")
            .status,
        "expired"
    );
    let receipt = cell
        .store()
        .read()
        .receipts()
        .expect("receipts")
        .pop()
        .expect("expiry receipt");
    assert!(matches!(
        receipt.body.consumed,
        Some(norn_cell::Consumption::Timer { .. })
    ));
    assert_eq!(receipt.body.logical_time, 10);
    assert!(!cell.expire_quantity_due().expect("once"));
    cell.store().read().verify_ledger().expect("timer fold");
}

#[test]
fn concurrent_retry_reconciles_transaction_timestamp_without_changing_identity() {
    let mut store = CellStore::open_in_memory(model()).expect("store");
    store.bind("scope", "tenant/dev").expect("scope");
    store
        .commit_fenced(&CommitBatch {
            world: "main".into(),
            scope: "tenant/dev".into(),
            quantity: Some(define()),
            idempotency_key: Some("define".into()),
            ..CommitBatch::default()
        })
        .expect("define");
    let mut quantity = post(0, "fund", "fund", 20);
    quantity.balance_versions = store
        .read()
        .quantity_balance_versions("main", &quantity)
        .expect("fences");
    let mut first = CommitBatch {
        world: "main".into(),
        scope: "tenant/dev".into(),
        quantity: Some(quantity),
        idempotency_key: Some("same".into()),
        logical_time: 5,
        ..CommitBatch::default()
    };
    let receipt = store.commit(&first).expect("first");
    first.logical_time = 6;
    assert_eq!(
        store.commit(&first).expect("racing time retries").receipt(),
        receipt.receipt()
    );
    let before = store.read().dump().expect("dump");
    if let QuantityOperation::Post { parameters, .. } =
        &mut first.quantity.as_mut().expect("quantity").operation
    {
        *parameters = quantity_support::parameters(21);
    }
    assert!(matches!(
        store.commit(&first),
        Err(CellError::IdempotencyConflict(_))
    ));
    assert_eq!(before, store.read().dump().expect("no mutation"));
}

#[test]
fn clock_floor_is_cell_wide_and_restart_and_fork_keep_independent_floors() {
    let dir = tempfile::tempdir().expect("directory");
    let path = dir.path().join("clock.sqlite");
    let fork = dir.path().join("fork.sqlite");
    let store = CellStore::open(&path, model(), StoreOptions::default()).expect("store");
    let mut cell: SqliteCell<String, String, String> =
        SqliteCell::new(store, "tenant/dev", |_| "ok".into()).expect("host");
    cell.advance_to(LogicalTime::from_millis(100));
    cell.commit_quantity_fenced(&define(), "define")
        .expect("quantity clock");
    cell.commit_host_events_idempotently("ordinary", vec!["ordinary".into()])
        .expect("ordinary commit");
    drop(cell);
    std::fs::copy(&path, &fork).expect("fork closed cell file");
    let mut reopened = CellStore::open(&path, model(), StoreOptions::default()).expect("restart");
    let mut other = define();
    other.ledger_id = "second".into();
    let before = reopened.read().dump().expect("dump");
    assert!(matches!(
        reopened.commit(&CommitBatch {
            world: "main".into(),
            scope: "tenant/dev".into(),
            logical_time: 99,
            quantity: Some(other.clone()),
            idempotency_key: Some("other".into()),
            ..CommitBatch::default()
        }),
        Err(CellError::Quantity(QuantityError::ClockRegression))
    ));
    assert_eq!(before, reopened.read().dump().expect("unchanged"));
    let mut cell: SqliteCell<String, String, String> =
        SqliteCell::new(reopened, "tenant/dev", |_| "ok".into()).expect("host");
    assert_eq!(cell.now().as_millis(), 100);
    cell.advance_to(LogicalTime::from_millis(200));
    cell.commit_quantity(&other, "other")
        .expect("second ledger");
    let mut forkcell: SqliteCell<String, String, String> = SqliteCell::new(
        CellStore::open(&fork, model(), StoreOptions::default()).expect("fork open"),
        "tenant/dev",
        |_| "ok".into(),
    )
    .expect("fork host");
    assert_eq!(forkcell.now().as_millis(), 100);
    forkcell.advance_to(LogicalTime::from_millis(150));
    forkcell
        .commit_host_events_idempotently("fork", vec!["fork".into()])
        .expect("fork ordinary advances floor");
    assert_eq!(cell.store().read().clock_floor().expect("floor"), 200);
    assert_eq!(
        forkcell.store().read().clock_floor().expect("fork floor"),
        150
    );
}

#[test]
fn funding_policy_matrix_requires_explicit_same_vector_netting_and_funds_remainder() {
    use norn_ir::quantity_ledger::LedgerFundingPolicy::{Gross, Net};
    for (lot_policy, transit_policy, admitted) in [
        (Gross, Gross, false),
        (Net, Gross, false),
        (Gross, Net, false),
        (Net, Net, true),
    ] {
        let mut definition = define();
        if let QuantityOperation::Define { pack, .. } = &mut definition.operation {
            pack.accounts.get_mut(1).expect("lot account").funding = lot_policy;
            pack.accounts.get_mut(2).expect("transit account").funding = transit_policy;
            let mut circle = pack.templates.get(1).expect("ship template").clone();
            circle.template_id = "circle".into();
            let mut returning = circle.legs.clone();
            returning.first_mut().expect("return outgoing").leg_id = "return-out".into();
            returning.first_mut().expect("return outgoing").account_id = "transit".into();
            returning.get_mut(1).expect("return incoming").leg_id = "return-in".into();
            returning.get_mut(1).expect("return incoming").account_id = "lot".into();
            circle.legs.extend(returning);
            pack.templates.push(circle);
        }
        let mut cell = host();
        cell.commit_quantity_fenced(&definition, "define")
            .expect("declared policies");
        let mut circle = reserve(0, "circle", 10, 100);
        if let QuantityOperation::Reserve { template_id, .. } = &mut circle.operation {
            *template_id = "circle".into();
        }
        let before = cell.store().read().dump().expect("dump");
        let outcome = cell.commit_quantity_fenced(&circle, "circle");
        if admitted {
            outcome.expect("explicit netting");
            assert_eq!(amounts(&cell, "lot"), (0, 0));
            assert_eq!(amounts(&cell, "transit"), (0, 0));
            let before = cell.store().read().dump().expect("dump");
            let incomplete = request(
                0,
                QuantityOperation::Void {
                    reservation_id: "circle".into(),
                    resolution: partial(10),
                },
            );
            assert!(matches!(
                cell.commit_quantity_fenced(&incomplete, "unfunded-remainder"),
                Err(CellError::Quantity(
                    QuantityError::InsufficientAvailable { .. }
                ))
            ));
            assert_eq!(
                before,
                cell.store()
                    .read()
                    .dump()
                    .expect("void cannot increase unfunded remainder")
            );
            cell.commit_quantity_fenced(
                &request(
                    0,
                    QuantityOperation::PostReservation {
                        reservation_id: "circle".into(),
                        posting_id: "finish".into(),
                        resolution: norn_ir::quantity_ledger::QuantityResolution::All,
                    },
                ),
                "finish",
            )
            .expect("full netted posting");
            cell.store().read().verify_ledger().expect("netted fold");
        } else {
            assert!(matches!(
                outcome,
                Err(CellError::Quantity(
                    QuantityError::InsufficientAvailable { .. }
                ))
            ));
            assert_eq!(before, cell.store().read().dump().expect("unchanged"));
        }
    }
    let mut cell = initialized(10);
    cell.commit_quantity_fenced(&reserve(0, "incoming", 10, 100), "incoming")
        .expect("reserve incoming transit");
    let mut spend = reserve(0, "spend-pending", 10, 100);
    if let QuantityOperation::Reserve { template_id, .. } = &mut spend.operation {
        *template_id = "receive".into();
    }
    assert!(matches!(
        cell.commit_quantity_fenced(&spend, "spend-pending"),
        Err(CellError::Quantity(
            QuantityError::InsufficientAvailable { .. }
        ))
    ));
    assert!(
        serde_json::from_str::<norn_ir::quantity_ledger::LedgerFundingPolicy>("\"implicit\"")
            .is_err()
    );
}

#[test]
fn configured_account_limit_admits_more_than_64_and_refuses_limit_overflow() {
    let mut definition = define();
    if let QuantityOperation::Define {
        pack,
        account_limit,
        ..
    } = &mut definition.operation
    {
        *account_limit = 100;
        for i in 5..100 {
            let mut account = pack.accounts.get(1).expect("lot account").clone();
            account.account_id = format!("declared-{i}");
            pack.accounts.push(account);
        }
    }
    let mut cell = host();
    cell.commit_quantity_fenced(&definition, "define")
        .expect("100 declared accounts");
    cell.commit_quantity_fenced(&post(0, "fund", "fund", 10), "fund")
        .expect("usable admitted pack");
    let mut small = host();
    if let QuantityOperation::Define { account_limit, .. } = &mut definition.operation {
        *account_limit = 99;
    }
    let before = small.store().read().dump().expect("dump");
    assert!(matches!(
        small.commit_quantity(&definition, "limit"),
        Err(CellError::Quantity(QuantityError::InvalidContract))
    ));
    assert_eq!(
        before,
        small
            .store()
            .read()
            .dump()
            .expect("limit refuses atomically")
    );
}

#[test]
fn sparse_vectors_and_shared_pending_incoming_do_not_fence_unrelated_lots() {
    let definition = sparse_definition_with_shared_incoming();
    let mut cell = host();
    cell.commit_quantity_fenced(&definition, "define")
        .expect("define");
    cell.commit_quantity_fenced(&post(0, "fund1", "fund", 100), "fund1")
        .expect("fund1");
    cell.commit_quantity_fenced(&post(0, "fund2", "fund2", 100), "fund2")
        .expect("fund2");
    let mut r1 = reserve(0, "r1", 20, 100);
    let mut r2 = reserve(0, "r2", 20, 100);
    if let QuantityOperation::Reserve { template_id, .. } = &mut r2.operation {
        *template_id = "ship2".into();
    }
    r1.balance_versions = cell
        .store()
        .read()
        .quantity_balance_versions("main", &r1)
        .expect("r1 snapshot");
    r2.balance_versions = cell
        .store()
        .read()
        .quantity_balance_versions("main", &r2)
        .expect("r2 snapshot");
    assert_eq!(r1.balance_versions.len(), 1);
    assert_eq!(r2.balance_versions.len(), 1);
    let reserved = cell.commit_quantity(&r1, "r1").expect("reserve1");
    cell.commit_quantity(&r2, "r2")
        .expect("shared incoming causes no conflict");
    assert_eq!(
        cell.store()
            .read()
            .quantity_balance("main", "stock", &key("transit"))
            .expect("untouched incoming")
            .version,
        0
    );
    assert_eq!(
        cell.commit_quantity(&r1, "r1")
            .expect("replay without an incoming head")
            .receipt(),
        reserved.receipt()
    );
    let mut a = post(0, "a", "sparse", 10);
    let mut b = post(0, "b", "sparse", 10);
    for (request, first, second) in [(&mut a, 10, 0), (&mut b, 0, 10)] {
        if let QuantityOperation::Post { parameters, .. } = &mut request.operation {
            *parameters = vec![
                norn_ir::quantity_ledger::LedgerParameterValue {
                    parameter_id: "q1".into(),
                    scale: 0,
                    unscaled: first.to_string(),
                },
                norn_ir::quantity_ledger::LedgerParameterValue {
                    parameter_id: "q2".into(),
                    scale: 0,
                    unscaled: second.to_string(),
                },
            ];
        }
        request.balance_versions = cell
            .store()
            .read()
            .quantity_balance_versions("main", request)
            .expect("sparse snapshot");
        assert_eq!(request.balance_versions.len(), 2);
    }
    cell.commit_quantity(&a, "a").expect("first sparse lot");
    cell.commit_quantity(&b, "b")
        .expect("zero legs cause no conflict");
    assert_eq!(amounts(&cell, "lot"), (90, 20));
    assert_eq!(amounts(&cell, "lot2"), (90, 20));
    cell.store()
        .read()
        .verify_ledger()
        .expect("sparse immutable fold");
}

fn sparse_definition_with_shared_incoming() -> norn_ir::quantity_ledger::QuantityRequest {
    let mut definition = two_lot_definition();
    if let QuantityOperation::Define { pack, .. } = &mut definition.operation {
        let mut sparse = pack
            .templates
            .iter()
            .find(|template| template.template_id == "ship")
            .expect("ship")
            .clone();
        sparse.template_id = "sparse".into();
        sparse
            .parameters
            .first_mut()
            .expect("parameter")
            .parameter_id = "q1".into();
        let mut second_parameter = sparse.parameters.first().expect("parameter").clone();
        second_parameter.parameter_id = "q2".into();
        sparse.parameters.push(second_parameter);
        for leg in &mut sparse.legs {
            leg.amount.terms.first_mut().expect("term").parameter_id = "q1".into();
        }
        let mut second_legs = pack
            .templates
            .iter()
            .find(|template| template.template_id == "ship2")
            .expect("ship2")
            .legs
            .clone();
        for leg in &mut second_legs {
            leg.leg_id.push('2');
            leg.amount.terms.first_mut().expect("term").parameter_id = "q2".into();
        }
        sparse.legs.extend(second_legs);
        pack.templates.push(sparse);
        // Both reservations name the same future incoming aggregate. Gross
        // incoming quantity creates no hold, balance write or funding read.
        pack.templates
            .iter_mut()
            .find(|template| template.template_id == "ship2")
            .expect("ship2")
            .legs
            .get_mut(1)
            .expect("incoming")
            .account_id = "transit".into();
    }
    definition
}
