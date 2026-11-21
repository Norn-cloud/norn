//! OPEN-CHECKPOINT-1 (#1533): an open verifies the history after the cell's
//! last verified checkpoint, and a tampered history is still refused: a
//! checkpoint that no longer anchors the ledger, a replaced receipt it
//! covers, or any change after it refuses the open, and an old row changed
//! in place is refused when it is read and by a verification from genesis.
//!
//! Live state is never exempted (#1551 round 1): a changed balance, hold or
//! timer, or a changed cell identity binding, refuses an open from a
//! checkpoint at the head; a checkpoint never advances over a record it has
//! not verified; and an old receipt changed in place is never served.
#![cfg(feature = "native")]
#![allow(
    clippy::arithmetic_side_effects,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    missing_docs,
    reason = "integration-test crate"
)]

#[path = "support/engine_fixture.rs"]
mod engine_fixture;
#[path = "quantity_support/mod.rs"]
mod quantity_support;

use std::collections::BTreeMap;
use std::path::Path;

use norn_cell::engine::{NativeEngine, SqlEngine, SqlValue};
use norn_cell::test_door::TestCommit as _;
use norn_cell::{CellError, CellStore, CommitBatch, FactWriteRow, MAIN_WORLD, StoreOptions};
use norn_ir::quantity_ledger::QuantityRequest;
use norn_ir::{RuntimeValue, runtime_object};
use quantity_support::{define, post, reserve};

/// Commits apart the engine records a checkpoint (`open_checkpoint::EVERY`).
const EVERY: u64 = 128;

fn open(path: &Path) -> Result<CellStore<NativeEngine>, CellError> {
    CellStore::open(path, engine_fixture::model(), StoreOptions::default())
}

fn raw(path: &Path) -> NativeEngine {
    NativeEngine::open(path, StoreOptions::default()).expect("engine")
}

fn put(n: u64) -> CommitBatch {
    CommitBatch {
        world: MAIN_WORLD.to_owned(),
        scope: engine_fixture::SCOPE.to_owned(),
        logical_time: 10_000 + n,
        idempotency_key: Some(format!("put-{n}")),
        writes: vec![FactWriteRow::Put {
            object_type: "fixture.Item".to_owned(),
            key: format!("c{}", n % 7),
            values: BTreeMap::from([(
                "qty".to_owned(),
                RuntimeValue::from(i64::try_from(n).expect("qty")),
            )]),
        }],
        events: vec![
            runtime_object! { "type" => "fixture.put", "n" => i64::try_from(n).expect("n") },
        ],
        ..CommitBatch::default()
    }
}

/// A cell of `commits` commits (the fixture's history, then puts).
fn cell(path: &Path, commits: u64) {
    engine_fixture::write_cell(path);
    let mut store = open(path).expect("open");
    for n in engine_fixture::COMMITS..commits {
        store.commit(&put(n)).expect("commit");
    }
    assert_eq!(
        store.read().verify_ledger().expect("ledger").commits,
        commits
    );
}

fn checkpoint(path: &Path) -> Option<(u64, u64, String, String)> {
    raw(path)
        .query(
            "SELECT commits, seq, hash, receipts_root FROM _norn_open_checkpoint WHERE id = 1",
            &[],
        )
        .expect("checkpoint")
        .into_iter()
        .next()
        .map(|row| match &row[..] {
            [
                SqlValue::Integer(commits),
                SqlValue::Integer(seq),
                SqlValue::Text(hash),
                SqlValue::Text(root),
            ] => (
                u64::try_from(*commits).expect("commits"),
                u64::try_from(*seq).expect("seq"),
                hash.clone(),
                root.clone(),
            ),
            other => panic!("checkpoint row {other:?}"),
        })
}

fn execute(path: &Path, sql: &str) {
    raw(path).execute(sql, &[]).expect("tamper");
}

fn refused(path: &Path) -> String {
    match open(path) {
        Ok(_) => panic!("a tampered history opened"),
        Err(error) => error.to_string(),
    }
}

#[test]
fn every_hundred_twenty_eighth_commit_records_the_verified_head() {
    let dir = tempfile::tempdir().expect("directory");
    let path = dir.path().join("cell.sqlite");
    cell(&path, EVERY - 1);
    assert_eq!(
        checkpoint(&path),
        None,
        "no checkpoint before {EVERY} commits"
    );
    cell_more(&path, EVERY - 1, 2 * EVERY + 3);
    let (commits, seq, hash, _root) = checkpoint(&path).expect("a checkpoint");
    assert_eq!(commits, 2 * EVERY, "the last multiple of {EVERY}");
    let store = open(&path).expect("open from the checkpoint");
    let entry = store
        .read()
        .ledger_page(seq.saturating_sub(1), 1)
        .expect("ledger");
    assert_eq!(
        entry[0].hash, hash,
        "it names the ledger head at its commit"
    );
    assert_eq!(
        store.read().verify_ledger().expect("ledger").commits,
        2 * EVERY + 3
    );
}

fn cell_more(path: &Path, from: u64, to: u64) {
    let mut store = open(path).expect("open");
    for n in from..to {
        store.commit(&put(n)).expect("commit");
    }
}

#[test]
fn an_open_from_the_checkpoint_admits_the_same_cell_a_genesis_open_admits() {
    let dir = tempfile::tempdir().expect("directory");
    let path = dir.path().join("cell.sqlite");
    cell(&path, EVERY + 10);
    assert!(checkpoint(&path).is_some());
    let from_checkpoint = open(&path).expect("open").verified_head().expect("head");
    execute(&path, "DELETE FROM _norn_open_checkpoint");
    let from_genesis = open(&path).expect("open").verified_head().expect("head");
    assert_eq!(from_checkpoint, from_genesis);
    // An open without a checkpoint verifies from genesis and writes nothing.
    assert_eq!(checkpoint(&path), None);
}

#[test]
fn a_checkpoint_that_no_longer_anchors_the_ledger_refuses_the_open() {
    let dir = tempfile::tempdir().expect("directory");
    let path = dir.path().join("cell.sqlite");
    cell(&path, EVERY + 10);
    // A head rewritten with its whole chain no longer matches the checkpoint.
    execute(
        &path,
        "UPDATE _norn_open_checkpoint SET hash = '0000000000000000000000000000000000000000000000000000000000000000'",
    );
    assert!(refused(&path).contains("checkpoint"), "{}", refused(&path));
}

#[test]
fn a_replaced_receipt_the_checkpoint_covers_refuses_the_open() {
    let dir = tempfile::tempdir().expect("directory");
    let path = dir.path().join("cell.sqlite");
    cell(&path, EVERY + 10);
    // Commit 2's receipt digest, replaced as if rewritten with its own hash.
    execute(
        &path,
        "UPDATE _norn_receipt SET receipt_hash = 'ff' || substr(receipt_hash, 3) WHERE commit_seq = 2",
    );
    assert!(refused(&path).contains("root"), "{}", refused(&path));
}

#[test]
fn any_change_after_the_checkpoint_refuses_the_open() {
    let dir = tempfile::tempdir().expect("directory");
    let path = dir.path().join("cell.sqlite");
    cell(&path, EVERY + 10);
    let fresh = dir.path().join("fresh.sqlite");
    std::fs::copy(&path, &fresh).expect("copy");
    // A ledger event after the checkpoint.
    execute(
        &path,
        &format!(
            "UPDATE _norn_ledger SET hash = 'ff' || substr(hash, 3) WHERE commit_seq = {}",
            EVERY + 5
        ),
    );
    refused(&path);
    // A receipt after the checkpoint.
    let receipt = dir.path().join("receipt.sqlite");
    std::fs::copy(&fresh, &receipt).expect("copy");
    execute(
        &receipt,
        &format!(
            "UPDATE _norn_receipt SET receipt_hash = 'ff' || substr(receipt_hash, 3) WHERE commit_seq = {}",
            EVERY + 5
        ),
    );
    refused(&receipt);
    // A value version after the checkpoint.
    execute(
        &fresh,
        &format!(
            "UPDATE _norn_fact_values SET hash = 'ff' || substr(hash, 3) WHERE commit_seq = {}",
            EVERY + 5
        ),
    );
    refused(&fresh);
}

#[test]
fn an_old_row_changed_in_place_is_refused_when_read_and_from_genesis() {
    let dir = tempfile::tempdir().expect("directory");
    let path = dir.path().join("cell.sqlite");
    cell(&path, EVERY + 10);
    let row = raw(&path)
        .query(
            "SELECT world, object_type, id, version FROM _norn_fact_values WHERE commit_seq = 7",
            &[],
        )
        .expect("row");
    let [
        SqlValue::Text(world),
        SqlValue::Text(object_type),
        SqlValue::Text(id),
        SqlValue::Integer(version),
    ] = &row[0][..]
    else {
        panic!("fact row {row:?}");
    };
    let version = u64::try_from(*version).expect("version");
    execute(
        &path,
        "UPDATE _norn_fact_values SET hash = 'ff' || substr(hash, 3) WHERE commit_seq = 7",
    );
    // The open verifies what came after the checkpoint; the old version it
    // does not re-read is refused when it is read ...
    let store = open(&path).expect("the open verifies from the checkpoint");
    assert!(
        store
            .read()
            .object_at_version(world, object_type, id, version)
            .is_err(),
        "a changed historical version is never served"
    );
    drop(store);
    // ... and a verification from genesis (an open with no checkpoint, as a
    // scrub runs it) refuses the cell.
    execute(&path, "DELETE FROM _norn_open_checkpoint");
    refused(&path);
}

/// Commit `request` as commit `n`, fenced at the balances it reads.
fn commit_quantity(store: &mut CellStore<NativeEngine>, n: u64, request: &QuantityRequest) {
    let mut request = request.clone();
    request.balance_versions = store
        .read()
        .quantity_balance_versions(MAIN_WORLD, &request)
        .expect("fences");
    store
        .commit(&CommitBatch {
            world: MAIN_WORLD.to_owned(),
            scope: engine_fixture::SCOPE.to_owned(),
            logical_time: 10_000 + n,
            idempotency_key: Some(format!("quantity-{n}")),
            quantity: Some(request.into()),
            ..CommitBatch::default()
        })
        .expect("quantity commit");
}

/// A cell exactly [`EVERY`] commits long, so its open checkpoint is its
/// head, with quantity history before it: a ledger, a funding posting and an
/// open hold (a held balance and an expiry timer).
fn quantity_cell(path: &Path) {
    engine_fixture::write_cell(path);
    let mut store = open(path).expect("open");
    let requests = [
        define(),
        post(0, "fund", "fund", 10),
        reserve(0, "hold", 3, 1_000_000),
    ];
    let mut n = engine_fixture::COMMITS;
    for request in &requests {
        commit_quantity(&mut store, n, request);
        n += 1;
    }
    for n in n..EVERY {
        store.commit(&put(n)).expect("commit");
    }
    drop(store);
    assert_eq!(
        checkpoint(path).map(|(commits, ..)| commits),
        Some(EVERY),
        "the checkpoint is the head"
    );
}

#[test]
fn a_changed_quantity_projection_refuses_an_open_from_a_head_checkpoint() {
    let dir = tempfile::tempdir().expect("directory");
    let path = dir.path().join("cell.sqlite");
    quantity_cell(&path);
    let held: i64 = raw(&path)
        .query_row(
            r#"SELECT count(*) FROM "_norn_quantity_balance" WHERE "held" <> '0'"#,
            &[],
            |row| row.get(0),
        )
        .expect("holds");
    assert!(held > 0, "the fixture holds a quantity");
    open(&path).expect("the untouched cell opens from its checkpoint");
    for (name, tamper) in [
        (
            "a current balance",
            r#"UPDATE "_norn_quantity_balance" SET "posted" = '11' WHERE "account" = 'lot'"#,
        ),
        (
            "a balance with its history",
            r#"UPDATE "_norn_quantity_balance" SET "posted" = '11' WHERE "account" = 'lot';
               UPDATE "_norn_quantity_history" SET "posted" = '11' WHERE "account" = 'lot'"#,
        ),
        (
            "a hold",
            r#"UPDATE "_norn_quantity_balance" SET "held" = '0';
               UPDATE "_norn_quantity_history" SET "held" = '0'"#,
        ),
        (
            "an expiry timer",
            r#"DELETE FROM "_norn_timer" WHERE "timer_id" >= 'norn.quantity.expiry/' AND "timer_id" < 'norn.quantity.expiry0'"#,
        ),
    ] {
        let copy = dir.path().join("tampered.sqlite");
        std::fs::copy(&path, &copy).expect("copy");
        raw(&copy).execute_batch(tamper).expect("tamper");
        assert!(
            open(&copy).is_err(),
            "{name} changed under the checkpoint opened"
        );
        // A verification from genesis refuses it too.
        execute(&copy, "DELETE FROM _norn_open_checkpoint");
        assert!(open(&copy).is_err(), "{name} changed opened from genesis");
    }
}

#[test]
fn a_changed_cell_identity_refuses_an_open_from_a_head_checkpoint() {
    let dir = tempfile::tempdir().expect("directory");
    let path = dir.path().join("cell.sqlite");
    cell(&path, EVERY);
    assert_eq!(checkpoint(&path).map(|(commits, ..)| commits), Some(EVERY));
    let records: i64 = raw(&path)
        .query_row("SELECT count(*) FROM _norn_export_journal", &[], |row| {
            row.get(0)
        })
        .expect("records");
    assert!(records > 0, "the history holds export records");
    execute(
        &path,
        "UPDATE _norn_meta SET value = '0123456789abcdef0123456789abcdef' WHERE key = 'binding:cell_id'",
    );
    assert!(
        matches!(open(&path), Err(CellError::BindingMismatch { .. })),
        "a changed identity opened from the head checkpoint"
    );
    execute(&path, "DELETE FROM _norn_open_checkpoint");
    assert!(
        matches!(open(&path), Err(CellError::BindingMismatch { .. })),
        "as a genesis open refuses it"
    );
}

#[test]
fn a_checkpoint_never_advances_over_a_record_it_has_not_verified() {
    let dir = tempfile::tempdir().expect("directory");
    let path = dir.path().join("cell.sqlite");
    cell(&path, EVERY + 10);
    let mut store = open(&path).expect("open");
    for n in EVERY + 10..2 * EVERY - 3 {
        store.commit(&put(n)).expect("commit");
    }
    // A value version this handle wrote after its open, changed before the
    // next checkpoint is due. No later commit here reads its key.
    execute(
        &path,
        &format!(
            "UPDATE _norn_fact_values SET hash = 'ff' || substr(hash, 3) WHERE commit_seq = {}",
            2 * EVERY - 3
        ),
    );
    for n in 2 * EVERY - 3..2 * EVERY + 2 {
        store.commit(&put(n)).expect("commit");
    }
    drop(store);
    assert_eq!(
        checkpoint(&path).map(|(commits, ..)| commits),
        Some(EVERY),
        "the checkpoint stays before the unverified record"
    );
    refused(&path);
}

#[test]
fn an_old_receipt_changed_in_place_is_never_served() {
    let dir = tempfile::tempdir().expect("directory");
    let path = dir.path().join("cell.sqlite");
    cell(&path, EVERY + 10);
    // A field no lookup column mirrors, its stored digest kept.
    execute(
        &path,
        "UPDATE _norn_receipt SET receipt = json_set(receipt, '$.logicalTime', 1) WHERE commit_seq = 7",
    );
    {
        let store = open(&path).expect("the open verifies from the checkpoint");
        let reader = store.read();
        assert!(reader.receipts().is_err(), "receipts() served it");
        assert!(
            reader.receipts_after(6, 1).is_err(),
            "receipts_after served it"
        );
        assert!(reader.receipt_at(7).is_err(), "receipt_at served it");
    }
    execute(&path, "DELETE FROM _norn_open_checkpoint");
    refused(&path);
}

// ---------------------------------------------------------------------------
// #1551 round 2 (genesis parity): every check a genesis open runs has a
// checkpoint-open equivalent (the table in `src/open_checkpoint.rs`). Each
// test below fails on 257baa27bb.
// ---------------------------------------------------------------------------

/// Norn1 hash of `value`, as the engine content-addresses its own data.
fn norn1<T: serde::Serialize>(value: &T) -> String {
    use norn_ir::canonical::{HashProfile, canonical_json_of};
    HashProfile::Norn1.hash_canonical_bytes(&canonical_json_of(value).expect("canonical"))
}

/// Rewrite receipt `commit` through `change`, together with its stored
/// digest, so its body still hashes to it.
fn rewrite_receipt(path: &Path, commit: u64, change: impl FnOnce(&mut serde_json::Value)) {
    let engine = raw(path);
    let text: String = engine
        .query_row(
            &format!("SELECT receipt FROM _norn_receipt WHERE commit_seq = {commit}"),
            &[],
            |row| row.get(0),
        )
        .expect("receipt");
    let mut body: serde_json::Value = serde_json::from_str(&text).expect("receipt body");
    change(&mut body);
    engine
        .execute(
            "UPDATE _norn_receipt SET receipt = ?1, receipt_hash = ?2 WHERE commit_seq = ?3",
            &[
                SqlValue::Text(body.to_string()),
                SqlValue::Text(norn1(&body)),
                SqlValue::Integer(i64::try_from(commit).expect("commit")),
            ],
        )
        .expect("rewrite receipt");
}

/// S1: an ordinary receipt after the checkpoint whose logical time goes
/// back, rewritten with its own digest, refuses the open, as from genesis.
#[test]
fn a_suffix_receipt_whose_time_goes_back_refuses_an_open_from_the_checkpoint() {
    let dir = tempfile::tempdir().expect("directory");
    let path = dir.path().join("cell.sqlite");
    cell(&path, EVERY + 10);
    rewrite_receipt(&path, EVERY + 5, |body| {
        body["logicalTime"] = serde_json::json!(1);
    });
    assert!(
        open(&path).is_err(),
        "a regressed suffix receipt opened from the checkpoint"
    );
    execute(&path, "DELETE FROM _norn_open_checkpoint");
    refused(&path);
}

/// S2: the cell identity changed together with the binding and stored
/// digest of the last export record a head checkpoint covers refuses the
/// open: the checkpoint pins that record's digest.
#[test]
fn a_changed_identity_and_its_last_covered_export_record_refuse_an_open() {
    const OTHER: &str = "0123456789abcdef0123456789abcdef";
    let dir = tempfile::tempdir().expect("directory");
    let path = dir.path().join("cell.sqlite");
    cell(&path, EVERY);
    assert_eq!(checkpoint(&path).map(|(commits, ..)| commits), Some(EVERY));
    let mut record = open(&path)
        .expect("open")
        .export_record(EVERY)
        .expect("record")
        .expect("the head commit has an export record");
    record
        .bindings
        .insert("cell_id".to_owned(), OTHER.to_owned());
    let engine = raw(&path);
    engine
        .execute(
            "UPDATE _norn_export_journal SET record = ?1, hash = ?2 WHERE commit_seq = ?3",
            &[
                SqlValue::Text(serde_json::to_string(&record).expect("record")),
                SqlValue::Text(record.hash().expect("hash")),
                SqlValue::Integer(i64::try_from(EVERY).expect("commit")),
            ],
        )
        .expect("rewrite the record");
    engine
        .execute(
            &format!("UPDATE _norn_meta SET value = '{OTHER}' WHERE key = 'binding:cell_id'"),
            &[],
        )
        .expect("rebind");
    drop(engine);
    assert!(
        open(&path).is_err(),
        "a changed identity and its last covered record opened from the checkpoint"
    );
    execute(&path, "DELETE FROM _norn_open_checkpoint");
    refused(&path);
}

/// S3: a handle opened past a durable checkpoint never advances the next
/// checkpoint over a ledger event that changed under it, between the two.
#[test]
fn a_checkpoint_never_advances_over_an_event_changed_before_the_handle_opened_past_it() {
    let dir = tempfile::tempdir().expect("directory");
    let path = dir.path().join("cell.sqlite");
    cell(&path, EVERY + 12);
    assert_eq!(checkpoint(&path).map(|(commits, ..)| commits), Some(EVERY));
    let mut store = open(&path).expect("open at commit 140");
    // A generic event before the handle's head, after the durable
    // checkpoint, changed in place with its stored hash kept.
    execute(
        &path,
        &format!(
            "UPDATE _norn_ledger SET event = replace(event, 'fixture.put', 'fixture.pux') WHERE commit_seq = {}",
            EVERY + 7
        ),
    );
    for n in EVERY + 12..2 * EVERY + 2 {
        store.commit(&put(n)).expect("commit");
    }
    drop(store);
    assert_eq!(
        checkpoint(&path).map(|(commits, ..)| commits),
        Some(EVERY),
        "the checkpoint advanced over an event it never verified"
    );
    refused(&path);
}

/// S4: an idempotency lookup compares the hashed body's own selectors with
/// the query, so a covered receipt whose SQL `world` changed is never
/// returned for another world.
#[test]
fn an_idempotency_lookup_never_trusts_a_changed_selector_column() {
    let dir = tempfile::tempdir().expect("directory");
    let path = dir.path().join("cell.sqlite");
    cell(&path, EVERY + 10);
    execute(
        &path,
        "UPDATE _norn_receipt SET world = 'shadow' WHERE commit_seq = 7",
    );
    let key: String = raw(&path)
        .query_row(
            "SELECT idempotency_key FROM _norn_receipt WHERE commit_seq = 7",
            &[],
            |row| row.get(0),
        )
        .expect("key");
    {
        let store = open(&path).expect("the open verifies from the checkpoint");
        assert!(
            !matches!(
                store.read().receipt_by_idempotency_key("shadow", &key),
                Ok(Some(_))
            ),
            "a lookup in another world returned the receipt"
        );
    }
    execute(&path, "DELETE FROM _norn_open_checkpoint");
    refused(&path);
}

/// S6: the retained canonical set of the active definitions, changed after
/// its install is covered, refuses the open: the active version's typed
/// install proof runs on every open.
#[test]
fn a_changed_active_definition_set_refuses_an_open_from_the_checkpoint() {
    let dir = tempfile::tempdir().expect("directory");
    let path = dir.path().join("cell.sqlite");
    engine_fixture::write_cell(&path);
    let model = engine_fixture::model();
    let mut store = open(&path).expect("open");
    store
        .install_definitions(
            model.clone(),
            install_input(&model, "0.1.0"),
            Some(0),
            engine_fixture::SCOPE,
        )
        .expect("install");
    let installed = store.read().verify_ledger().expect("ledger").commits;
    for n in installed..EVERY + 10 {
        store.commit(&put(n)).expect("commit");
    }
    drop(store);
    assert_eq!(checkpoint(&path).map(|(commits, ..)| commits), Some(EVERY));
    open(&path).expect("the untouched cell opens from its checkpoint");
    raw(&path)
        .execute(
            "UPDATE _norn_definition_version SET canonical_set = ?1 WHERE version = 1",
            &[SqlValue::Text(install_input(&model, "0.2.0").canonical_set)],
        )
        .expect("tamper");
    assert!(
        open(&path).is_err(),
        "a changed active definition set opened from the checkpoint"
    );
    execute(&path, "DELETE FROM _norn_open_checkpoint");
    refused(&path);
}

/// One installed definition set naming `model`'s object types at `version`.
fn install_input(model: &norn_cell::Model, version: &str) -> norn_cell::InstalledDefinitionInput {
    let artifacts: Vec<serde_json::Value> = model
        .object_types()
        .filter(|object_type| object_type.name.as_str() != norn_kernel::OWNED_ROWS_TYPE)
        .map(|object_type| {
            let (namespace, name) = object_type
                .name
                .as_str()
                .rsplit_once('.')
                .expect("qualified name");
            serde_json::json!({
                "irVersion": "0.1",
                "artifactId": format!("{}.entity", object_type.name.as_str()),
                "kind": "entity",
                "namespace": namespace,
                "name": name,
                "version": version,
                "lifecycle": "draft",
                "scope": {"orgId": "org_demo", "environmentId": "dev"},
                "dependencies": [],
                "provenance": {"source": "authoring_sdk"},
                "payload": {"entityVersion": "2.0", "objectType": object_type}
            })
        })
        .collect();
    norn_cell::InstalledDefinitionInput {
        canonical_set: String::from_utf8(
            RuntimeValue::from_json(serde_json::json!(artifacts))
                .expect("exact JSON")
                .canonical_json()
                .expect("canonical"),
        )
        .expect("UTF-8"),
        registry: Vec::new(),
        receipt_sha256: format!("receipt-{version}"),
        admission_receipt: None,
        readmission: None,
        diff: serde_json::json!({"added": {"count": 1, "ids": [version]}}),
        installed_at: 10_000,
        layout: None,
    }
}

/// A ledger event the checkpoint covers, changed in place with its stored
/// hash kept, is never served: every ledger read rehashes the entry.
#[test]
fn an_old_ledger_event_changed_in_place_is_never_served() {
    let dir = tempfile::tempdir().expect("directory");
    let path = dir.path().join("cell.sqlite");
    cell(&path, EVERY + 10);
    let seq: i64 = raw(&path)
        .query_row(
            "SELECT seq FROM _norn_ledger WHERE commit_seq = 7 ORDER BY seq LIMIT 1",
            &[],
            |row| row.get(0),
        )
        .expect("seq");
    execute(
        &path,
        "UPDATE _norn_ledger SET event = replace(event, 'fixture.put', 'fixture.pux') WHERE commit_seq = 7",
    );
    {
        let store = open(&path).expect("the open verifies from the checkpoint");
        let after = u64::try_from(seq - 1).expect("seq");
        assert!(
            store.read().ledger_page(after, 1).is_err(),
            "ledger_page served the changed event"
        );
    }
    execute(&path, "DELETE FROM _norn_open_checkpoint");
    refused(&path);
}

/// Beyond S6: a superseded definition version is read live too (historical
/// compute, a mapping bundle pinned to it), so its retained set, changed
/// after the checkpoint covers it, refuses the open: the checkpoint pins a
/// digest of every retained version it covers.
#[test]
fn a_changed_superseded_definition_set_refuses_an_open_from_the_checkpoint() {
    let dir = tempfile::tempdir().expect("directory");
    let path = dir.path().join("cell.sqlite");
    engine_fixture::write_cell(&path);
    let model = engine_fixture::model();
    let mut store = open(&path).expect("open");
    for (expected, version) in [(0, "0.1.0"), (1, "0.1.1")] {
        store
            .install_definitions(
                model.clone(),
                install_input(&model, version),
                Some(expected),
                engine_fixture::SCOPE,
            )
            .expect("install");
    }
    let installed = store.read().verify_ledger().expect("ledger").commits;
    for n in installed..EVERY + 10 {
        store.commit(&put(n)).expect("commit");
    }
    drop(store);
    assert_eq!(checkpoint(&path).map(|(commits, ..)| commits), Some(EVERY));
    open(&path).expect("the untouched cell opens from its checkpoint");
    raw(&path)
        .execute(
            "UPDATE _norn_definition_version SET canonical_set = ?1 WHERE version = 1",
            &[SqlValue::Text(install_input(&model, "0.2.0").canonical_set)],
        )
        .expect("tamper");
    assert!(
        open(&path).is_err(),
        "a changed superseded definition set opened from the checkpoint"
    );
    execute(&path, "DELETE FROM _norn_open_checkpoint");
    refused(&path);
}
