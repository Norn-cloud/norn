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

use super::engine_fixture;

use std::cell::Cell;
use std::collections::BTreeMap;
use std::path::Path;

use super::quantity_support::{define, post, reserve};
use norn_cell::engine::{NativeEngine, ReadIsolation, SqlEngine, SqlValue};
use norn_cell::reactive::{BudgetedLinksRead, DecodeBudget, ReactiveLimits};
use norn_cell::test_door::TestCommit as _;
use norn_cell::{
    CellError, CellStore, CheckpointKey, CommitBatch, FactWriteRow, KnowledgeCutoff, LinkChange,
    MAIN_WORLD, Model, ScrubStatus, StoreOptions,
};
use norn_ir::quantity_ledger::QuantityRequest;
use norn_ir::{RuntimeValue, runtime_object};

/// Commits apart the engine records a checkpoint (`open_checkpoint::EVERY`).
const EVERY: u64 = 128;

/// This test process's checkpoint key: random, from SQLite's
/// `randomblob`, the same for every open in the process.
fn test_key_bytes() -> [u8; 32] {
    static KEY: std::sync::OnceLock<[u8; 32]> = std::sync::OnceLock::new();
    *KEY.get_or_init(|| {
        let engine = NativeEngine::open_in_memory().expect("engine");
        let rows = engine
            .query("SELECT randomblob(32)", &[])
            .expect("random bytes");
        let [SqlValue::Blob(bytes)] = &rows[0][..] else {
            panic!("random bytes {rows:?}");
        };
        bytes.clone().try_into().expect("32 bytes")
    })
}

/// The host's open options: the test key (#1551 round 4).
fn keyed() -> StoreOptions {
    StoreOptions {
        checkpoint_key: Some(norn_cell::CheckpointKey::from_bytes(test_key_bytes())),
        ..StoreOptions::default()
    }
}

fn open(path: &Path) -> Result<CellStore<NativeEngine>, CellError> {
    CellStore::open(path, engine_fixture::model(), keyed())
}

/// An open by a host that holds no key.
fn open_unkeyed(path: &Path) -> Result<CellStore<NativeEngine>, CellError> {
    CellStore::open(path, engine_fixture::model(), StoreOptions::default())
}

fn raw(path: &Path) -> NativeEngine {
    NativeEngine::open(path, StoreOptions::default()).expect("engine")
}

fn put(n: u64) -> CommitBatch {
    CommitBatch {
        world: MAIN_WORLD.to_owned(),
        scope: engine_fixture::SCOPE.to_owned(),
        logical_time: 10_000_u64.saturating_add(n),
        idempotency_key: Some(format!("put-{n}")),
        writes: vec![FactWriteRow::Put {
            object_type: "fixture.Item".to_owned(),
            key: format!("c{}", n.rem_euclid(7)),
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
    cell(&path, EVERY.saturating_sub(1));
    assert_eq!(
        checkpoint(&path),
        None,
        "no checkpoint before {EVERY} commits"
    );
    cell_more(
        &path,
        EVERY.saturating_sub(1),
        EVERY.saturating_mul(2).saturating_add(3),
    );
    let (commits, seq, hash, _root) = checkpoint(&path).expect("a checkpoint");
    assert_eq!(
        commits,
        EVERY.saturating_mul(2),
        "the last multiple of {EVERY}"
    );
    let store = open(&path).expect("open from the checkpoint");
    assert_eq!(store.scrub_to_end(), ScrubStatus::Idle);
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
        EVERY.saturating_mul(2).saturating_add(3)
    );
}

fn cell_more(path: &Path, from: u64, to: u64) {
    let mut store = open(path).expect("open");
    // The host's due-work clock runs the scrub between commits.
    assert_eq!(store.scrub_to_end(), ScrubStatus::Idle);
    for n in from..to {
        store.commit(&put(n)).expect("commit");
    }
}

#[test]
fn an_open_from_the_checkpoint_admits_the_same_cell_a_genesis_open_admits() {
    let dir = tempfile::tempdir().expect("directory");
    let path = dir.path().join("cell.sqlite");
    cell(&path, EVERY.saturating_add(10));
    assert!(checkpoint(&path).is_some());
    let from_checkpoint = open(&path).expect("open").verified_head().expect("head");
    execute(&path, "DELETE FROM _norn_open_checkpoint");
    let from_genesis = open(&path).expect("open").verified_head().expect("head");
    assert_eq!(from_checkpoint, from_genesis);
    // An open without a checkpoint verifies from genesis and writes nothing.
    assert_eq!(checkpoint(&path), None);
}

/// #1551 round 4: a checkpoint row changed by anyone without the key is
/// never trusted; the open verifies from genesis instead of refusing.
#[test]
fn a_checkpoint_row_changed_without_the_key_opens_from_genesis() {
    let dir = tempfile::tempdir().expect("directory");
    let path = dir.path().join("cell.sqlite");
    cell(&path, EVERY.saturating_add(10));
    execute(
        &path,
        "UPDATE _norn_open_checkpoint SET hash = '0000000000000000000000000000000000000000000000000000000000000000'",
    );
    let store = open(&path).expect("a genesis open");
    assert!(
        history_ready(&store),
        "the changed row was trusted: {:?}",
        store.scrub_status()
    );
}

#[test]
fn a_replaced_receipt_the_checkpoint_covers_refuses_the_open() {
    let dir = tempfile::tempdir().expect("directory");
    let path = dir.path().join("cell.sqlite");
    cell(&path, EVERY.saturating_add(10));
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
    cell(&path, EVERY.saturating_add(10));
    let fresh = dir.path().join("fresh.sqlite");
    std::fs::copy(&path, &fresh).expect("copy");
    let suffix = EVERY.saturating_add(5);
    // A ledger event after the checkpoint.
    execute(
        &path,
        &format!(
            "UPDATE _norn_ledger SET hash = 'ff' || substr(hash, 3) WHERE commit_seq = {suffix}"
        ),
    );
    refused(&path);
    // A receipt after the checkpoint.
    let receipt = dir.path().join("receipt.sqlite");
    std::fs::copy(&fresh, &receipt).expect("copy");
    execute(
        &receipt,
        &format!(
            "UPDATE _norn_receipt SET receipt_hash = 'ff' || substr(receipt_hash, 3) WHERE commit_seq = {suffix}"
        ),
    );
    refused(&receipt);
    // A value version after the checkpoint.
    execute(
        &fresh,
        &format!(
            "UPDATE _norn_fact_values SET hash = 'ff' || substr(hash, 3) WHERE commit_seq = {suffix}"
        ),
    );
    refused(&fresh);
}

#[test]
fn an_old_row_changed_in_place_is_refused_when_read_and_from_genesis() {
    let dir = tempfile::tempdir().expect("directory");
    let path = dir.path().join("cell.sqlite");
    cell(&path, EVERY.saturating_add(10));
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
            logical_time: 10_000_u64.saturating_add(n),
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
        n = n.saturating_add(1);
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
    let first_open = EVERY.saturating_add(10);
    let tampered = EVERY.saturating_mul(2).saturating_sub(3);
    cell(&path, first_open);
    let mut store = open(&path).expect("open");
    // The scrub verifies the history and seeds the checkpoint at the open's
    // head; commits advance it from there.
    assert_eq!(store.scrub_to_end(), ScrubStatus::Idle);
    for n in first_open..tampered {
        store.commit(&put(n)).expect("commit");
    }
    // A value version this handle wrote after its open, changed before the
    // next checkpoint is due. No later commit here reads its key.
    execute(
        &path,
        &format!(
            "UPDATE _norn_fact_values SET hash = 'ff' || substr(hash, 3) WHERE commit_seq = {tampered}"
        ),
    );
    for n in tampered..EVERY.saturating_mul(2).saturating_add(2) {
        store.commit(&put(n)).expect("commit");
    }
    drop(store);
    assert_eq!(
        checkpoint(&path).map(|(commits, ..)| commits),
        Some(first_open),
        "the checkpoint stays before the unverified record"
    );
    refused(&path);
}

#[test]
fn an_old_receipt_changed_in_place_is_never_served() {
    let dir = tempfile::tempdir().expect("directory");
    let path = dir.path().join("cell.sqlite");
    cell(&path, EVERY.saturating_add(10));
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
    cell(&path, EVERY.saturating_add(10));
    rewrite_receipt(&path, EVERY.saturating_add(5), |body| {
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
    let mut record = open_unkeyed(&path)
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
/// #1551 round 4: nothing advances until the scrub has verified the history
/// the handle opened over; the scrub finds the changed event and fences the
/// cell, which then commits nothing and drops its checkpoint. On 0030aa90b2
/// the checkpoint did not advance either, but the cell kept serving it.
#[test]
fn a_checkpoint_never_advances_over_an_event_changed_before_the_handle_opened_past_it() {
    let dir = tempfile::tempdir().expect("directory");
    let path = dir.path().join("cell.sqlite");
    let head_commits = EVERY.saturating_add(12);
    cell(&path, head_commits);
    assert_eq!(checkpoint(&path).map(|(commits, ..)| commits), Some(EVERY));
    let mut store = open(&path).expect("open at commit 140");
    // A generic event before the handle's head, after the durable
    // checkpoint, changed in place with its stored hash kept.
    execute(
        &path,
        &format!(
            "UPDATE _norn_ledger SET event = replace(event, 'fixture.put', 'fixture.pux') WHERE commit_seq = {}",
            EVERY.saturating_add(7)
        ),
    );
    assert!(matches!(
        store.commit(&put(head_commits)),
        Err(CellError::Verifying(_))
    ));
    assert!(matches!(store.scrub_to_end(), ScrubStatus::Fenced { .. }));
    assert!(matches!(
        store.commit(&put(head_commits)),
        Err(CellError::Fenced(_))
    ));
    drop(store);
    assert_eq!(checkpoint(&path), None, "the fence drops the checkpoint");
    refused(&path);
}

/// S4: an idempotency lookup compares the hashed body's own selectors with
/// the query, so a covered receipt whose SQL `world` changed is never
/// returned for another world.
#[test]
fn an_idempotency_lookup_never_trusts_a_changed_selector_column() {
    let dir = tempfile::tempdir().expect("directory");
    let path = dir.path().join("cell.sqlite");
    cell(&path, EVERY.saturating_add(10));
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
    for n in installed..EVERY.saturating_add(10) {
        store.commit(&put(n)).expect("commit");
    }
    drop(store);
    assert_eq!(checkpoint(&path).map(|(commits, ..)| commits), Some(EVERY));
    open(&path).expect("the untouched cell opens from its checkpoint");
    rusqlite::Connection::open(&path)
        .expect("sqlite")
        .execute(
            "UPDATE _norn_definition_version SET canonical_set = ?1 WHERE version = 1",
            rusqlite::params![install_input(&model, "0.2.0").canonical_set],
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
                "scope": {"orgId": "fixture", "environmentId": "dev"},
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
    cell(&path, EVERY.saturating_add(10));
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
        let after = u64::try_from(seq.saturating_sub(1)).expect("seq");
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
    for n in installed..EVERY.saturating_add(10) {
        store.commit(&put(n)).expect("commit");
    }
    drop(store);
    assert_eq!(checkpoint(&path).map(|(commits, ..)| commits), Some(EVERY));
    open(&path).expect("the untouched cell opens from its checkpoint");
    rusqlite::Connection::open(&path)
        .expect("sqlite")
        .execute(
            "UPDATE _norn_definition_version SET canonical_set = ?1 WHERE version = 1",
            rusqlite::params![install_input(&model, "0.2.0").canonical_set],
        )
        .expect("tamper");
    assert!(
        open(&path).is_err(),
        "a changed superseded definition set opened from the checkpoint"
    );
    execute(&path, "DELETE FROM _norn_open_checkpoint");
    refused(&path);
}

// ---------------------------------------------------------------------------
// #1551 round 4: the checkpoint is MAC'd under a key outside the file, a
// checkpointed open verifies live state, and the history scrub verifies the
// rest, holding back the reads that need it. Each test notes how it fails
// on 0030aa90b2.
// ---------------------------------------------------------------------------

/// Whether history reads are served (a genesis open, or a finished scrub).
fn history_ready<E: SqlEngine>(store: &CellStore<E>) -> bool {
    match store.scrub_status() {
        ScrubStatus::Idle => true,
        ScrubStatus::Verifying { history_ready, .. } => history_ready,
        ScrubStatus::Fenced { .. } => false,
    }
}

/// The RFC 6962 root over the first `commits` stored receipt digests, in the
/// checkpoint's lowercase hex: what anyone who can read the file computes.
fn receipts_root_at(path: &Path, commits: u64) -> String {
    let mut tree =
        ct_merkle::mem_backed_tree::MemoryBackedTree::<transparency_sha2::Sha256, String>::new();
    for row in raw(path)
        .query(
            &format!("SELECT receipt_hash FROM _norn_receipt ORDER BY commit_seq LIMIT {commits}"),
            &[],
        )
        .expect("digests")
    {
        let [SqlValue::Text(digest)] = &row[..] else {
            panic!("digest row {row:?}");
        };
        tree.push(digest.clone());
    }
    tree.root()
        .as_bytes()
        .iter()
        .fold(String::new(), |mut hex, byte| {
            use std::fmt::Write as _;
            let _infallible = write!(hex, "{byte:02x}");
            hex
        })
}

/// (a) S2: a covered receipt rewritten with its digest, together with the
/// checkpoint's receipt root, is refused with or without the key: without
/// it the open is a genesis open; with it the row's MAC no longer verifies,
/// so the open is a genesis open. On 0030aa90b2 the open admitted it from
/// the checkpoint.
#[test]
fn a_checkpoint_rewritten_with_the_history_it_covers_is_never_trusted() {
    let dir = tempfile::tempdir().expect("directory");
    let path = dir.path().join("cell.sqlite");
    cell(&path, EVERY.saturating_add(10));
    rewrite_receipt(&path, 7, |body| {
        body["logicalTime"] = serde_json::json!(1);
    });
    let root = receipts_root_at(&path, EVERY);
    execute(
        &path,
        &format!("UPDATE _norn_open_checkpoint SET receipts_root = '{root}'"),
    );
    assert!(
        open_unkeyed(&path).is_err(),
        "an unkeyed open admitted the rewritten history"
    );
    // Commit 7's journal row is a residue rebuilt from that receipt
    // (EXPORT-RESIDUE-1): the open refuses at its record before the ledger
    // pass reaches the time check.
    let error = refused(&path);
    assert_eq!(
        error, "ledger corrupt at seq 7: export journal record does not verify",
        "{error}"
    );
}

/// (b) A valid checkpoint row copied from another cell is never trusted:
/// the MAC binds the cell's identity. The other cell here is this cell's
/// history under another identity (every export record and the binding
/// rewritten), holding the row MAC'd for the first identity. The open is a
/// genesis open. On 0030aa90b2 the row was trusted and its export pin
/// refused the open; a row is now judged by its MAC alone.
#[test]
fn a_checkpoint_row_from_another_cell_is_never_trusted() {
    const OTHER: &str = "0123456789abcdef0123456789abcdef";
    let dir = tempfile::tempdir().expect("directory");
    let path = dir.path().join("cell.sqlite");
    let total = EVERY.saturating_add(10);
    cell(&path, total);
    let store = open_unkeyed(&path).expect("open");
    let records: Vec<_> = (1..=total)
        .filter_map(|commit| {
            store
                .export_record(commit)
                .expect("record")
                .map(|record| (commit, record))
        })
        .collect();
    drop(store);
    let engine = raw(&path);
    for (commit, mut record) in records {
        record
            .bindings
            .insert("cell_id".to_owned(), OTHER.to_owned());
        engine
            .execute(
                "UPDATE _norn_export_journal SET record = ?1, hash = ?2 WHERE commit_seq = ?3",
                &[
                    SqlValue::Text(serde_json::to_string(&record).expect("record")),
                    SqlValue::Text(record.hash().expect("hash")),
                    SqlValue::Integer(i64::try_from(commit).expect("commit")),
                ],
            )
            .expect("rewrite the record");
    }
    engine
        .execute(
            &format!("UPDATE _norn_meta SET value = '{OTHER}' WHERE key = 'binding:cell_id'"),
            &[],
        )
        .expect("rebind");
    drop(engine);
    let store = open(&path).expect("a genesis open of the other cell");
    assert!(
        history_ready(&store),
        "a row from another cell was trusted: {:?}",
        store.scrub_status()
    );
}

/// (c) S4: a covered receipt's idempotency selector changed so the lookup
/// misses it. Until the scrub has checked every covered receipt, a retry is
/// refused as retryable instead of committed twice; the scrub then finds
/// the changed selector and fences the cell, and the next open (from
/// genesis) refuses it. On 0030aa90b2 the retry committed a second time.
#[test]
fn a_hidden_idempotency_key_is_never_committed_twice() {
    let dir = tempfile::tempdir().expect("directory");
    let path = dir.path().join("cell.sqlite");
    cell(&path, EVERY.saturating_add(10));
    // put(n) is commit n + 1; this one is covered by the checkpoint at EVERY.
    let n = EVERY.saturating_sub(20);
    execute(
        &path,
        &format!(
            "UPDATE _norn_receipt SET idempotency_key = 'hidden' WHERE commit_seq = {}",
            n.saturating_add(1)
        ),
    );
    let mut store = open(&path).expect("the open verifies from the checkpoint");
    assert!(!history_ready(&store));
    // The client's retry, stamped with the host's current time.
    let retry = CommitBatch {
        logical_time: 10_000_u64.saturating_add(EVERY).saturating_add(20),
        ..put(n)
    };
    assert!(
        matches!(store.commit(&retry), Err(CellError::Verifying(_))),
        "a retry of a hidden key was not held back"
    );
    assert!(matches!(store.scrub_to_end(), ScrubStatus::Fenced { .. }));
    assert!(
        matches!(store.commit(&retry), Err(CellError::Fenced(_))),
        "a fenced cell committed"
    );
    drop(store);
    assert_eq!(checkpoint(&path), None, "the fence drops the checkpoint");
    refused(&path);
}

/// (c) S7: a covered value version deleted, so an as-of read's scan would
/// pick an older one. Until the scrub ends the read is refused as
/// retryable; the scrub finds the missing version and fences the cell. On
/// 0030aa90b2 the as-of read answered the older version.
#[test]
fn an_as_of_read_waits_for_the_scrub_over_a_deleted_version() {
    let dir = tempfile::tempdir().expect("directory");
    let path = dir.path().join("cell.sqlite");
    cell(&path, EVERY.saturating_add(10));
    let commit = EVERY.saturating_sub(30);
    let row = raw(&path)
        .query(
            &format!("SELECT object_type, id FROM _norn_fact_values WHERE commit_seq = {commit}"),
            &[],
        )
        .expect("row");
    let [SqlValue::Text(object_type), SqlValue::Text(id)] = &row[0][..] else {
        panic!("fact row {row:?}");
    };
    execute(
        &path,
        &format!("DELETE FROM _norn_fact_values WHERE commit_seq = {commit}"),
    );
    let store = open(&path).expect("the open verifies from the checkpoint");
    assert!(
        matches!(
            store
                .read()
                .version_as_of(MAIN_WORLD, object_type, id, commit),
            Err(CellError::Verifying(_))
        ),
        "an as-of read trusted the scan before the scrub"
    );
    assert!(matches!(store.scrub_to_end(), ScrubStatus::Fenced { .. }));
    assert!(matches!(
        store
            .read()
            .version_as_of(MAIN_WORLD, object_type, id, commit),
        Err(CellError::Fenced(_))
    ));
}

/// (c) S8: a covered ledger event changed and rehashed over its own
/// `prev_hash`, so the entry verifies alone but no longer chains. A ledger
/// page over it is refused until the scrub ends; the scrub finds the broken
/// chain and fences the cell. On 0030aa90b2 `ledger_page` served it.
#[test]
fn a_rehashed_ledger_event_is_never_served_before_the_scrub() {
    let dir = tempfile::tempdir().expect("directory");
    let path = dir.path().join("cell.sqlite");
    cell(&path, EVERY.saturating_add(10));
    let commit = EVERY.saturating_sub(40);
    let engine = raw(&path);
    let row = engine
        .query(
            &format!(
                "SELECT seq, world, event, prev_hash FROM _norn_ledger WHERE commit_seq = {commit} ORDER BY seq LIMIT 1"
            ),
            &[],
        )
        .expect("entry");
    let [
        SqlValue::Integer(seq),
        SqlValue::Text(world),
        SqlValue::Text(event),
        SqlValue::Text(prev),
    ] = &row[0][..]
    else {
        panic!("ledger row {row:?}");
    };
    let event: serde_json::Value =
        serde_json::from_str(&event.replace("fixture.put", "fixture.pux")).expect("event");
    let hash = norn1(&serde_json::json!({
        "seq": seq,
        "commitSeq": commit,
        "world": world,
        "event": event,
        "prev": prev,
    }));
    engine
        .execute(
            "UPDATE _norn_ledger SET event = ?1, hash = ?2 WHERE seq = ?3",
            &[
                SqlValue::Text(event.to_string()),
                SqlValue::Text(hash),
                SqlValue::Integer(*seq),
            ],
        )
        .expect("rehash");
    drop(engine);
    let after = u64::try_from(seq.saturating_sub(1)).expect("seq");
    let store = open(&path).expect("the open verifies from the checkpoint");
    assert!(
        matches!(
            store.read().ledger_page(after, 1),
            Err(CellError::Verifying(_))
        ),
        "ledger_page served a covered entry before the scrub"
    );
    // The suffix is verified: a page after the checkpoint is served.
    let (_, covered_seq, ..) = checkpoint(&path).expect("checkpoint");
    assert!(store.read().ledger_page(covered_seq, 1).is_ok());
    assert!(matches!(store.scrub_to_end(), ScrubStatus::Fenced { .. }));
}

/// The scrub of an untampered cell ends idle, serves history again, and
/// seeds the checkpoint at the head the open verified.
#[test]
fn the_scrub_of_an_untampered_cell_reseeds_the_checkpoint_at_the_open_head() {
    let dir = tempfile::tempdir().expect("directory");
    let path = dir.path().join("cell.sqlite");
    let total = EVERY.saturating_add(10);
    cell(&path, total);
    let store = open(&path).expect("open");
    assert!(!history_ready(&store));
    assert_eq!(store.scrub_to_end(), ScrubStatus::Idle);
    assert!(history_ready(&store));
    store
        .read()
        .ledger_page(0, 4)
        .expect("history is served after the scrub");
    drop(store);
    assert_eq!(
        checkpoint(&path).map(|(commits, ..)| commits),
        Some(total),
        "seeded at the open's head"
    );
}

/// (d) A restore that trimmed history keeps a checkpoint past its end: the
/// open verifies from genesis instead of refusing, the scrub re-seeds the
/// checkpoint at the restored head, and commits advance it from there. On
/// 0030aa90b2 the open refused ("fewer receipts than it covers").
#[test]
fn a_trimmed_restore_reseeds_the_checkpoint_through_the_scrub() {
    let dir = tempfile::tempdir().expect("directory");
    let path = dir.path().join("cell.sqlite");
    let restored = dir.path().join("restored.sqlite");
    let cut = EVERY.saturating_mul(2).saturating_add(72);
    let end = EVERY.saturating_mul(4).saturating_add(2);
    let next_cp = EVERY.saturating_mul(3);
    cell(&path, cut);
    std::fs::copy(&path, &restored).expect("snapshot at 328");
    cell_more(&path, cut, end);
    let row = raw(&path)
        .query("SELECT * FROM _norn_open_checkpoint", &[])
        .expect("the later row")
        .remove(0);
    let engine = raw(&restored);
    engine
        .execute("DELETE FROM _norn_open_checkpoint", &[])
        .expect("clear");
    engine
        .execute(
            "INSERT INTO _norn_open_checkpoint VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
            &row,
        )
        .expect("the row past the cut");
    drop(engine);
    let mut store = open(&restored).expect("a trimmed restore opens from genesis");
    assert!(history_ready(&store));
    assert_eq!(store.scrub_to_end(), ScrubStatus::Idle);
    assert_eq!(
        checkpoint(&restored).map(|(commits, ..)| commits),
        Some(cut),
        "re-seeded at the restored head"
    );
    for n in cut..=next_cp {
        store.commit(&put(n)).expect("commit");
    }
    drop(store);
    assert_eq!(
        checkpoint(&restored).map(|(commits, ..)| commits),
        Some(next_cp),
        "advanced from the re-seeded checkpoint"
    );
}

// ---------------------------------------------------------------------------
// #1551 parked round-4 blockers and shoulds (S2, S4, S7, S8, S10, S11, S12, S13)
// ---------------------------------------------------------------------------

/// S2: a malformed or unparseable checkpoint row (wrong column type,
/// half-null export pair, invalid MAC hex) falls back cleanly to a genesis
/// open rather than failing or trusting partial fields; tampered history
/// under a malformed row is still refused by the genesis open.
#[test]
fn malformed_checkpoint_row_opens_from_genesis() {
    let dir = tempfile::tempdir().expect("directory");
    let base = dir.path().join("base.sqlite");
    cell(&base, EVERY.saturating_add(10));
    for (name, sql) in [
        (
            "non-integer commits",
            "UPDATE _norn_open_checkpoint SET commits = 'corrupt'",
        ),
        (
            "out-of-range positive integer",
            "UPDATE _norn_open_checkpoint SET commits = 9223372036854775807",
        ),
        (
            "out-of-range negative integer",
            "UPDATE _norn_open_checkpoint SET commits = -9223372036854775808",
        ),
        (
            "float commits",
            "UPDATE _norn_open_checkpoint SET commits = 1.5",
        ),
        (
            "non-finite float commits",
            "UPDATE _norn_open_checkpoint SET commits = 9e999",
        ),
        (
            "invalid utf8 hash",
            "UPDATE _norn_open_checkpoint SET hash = CAST(X'fffe' AS TEXT)",
        ),
        (
            "blob commits",
            "UPDATE _norn_open_checkpoint SET commits = X'deadbeef'",
        ),
        (
            "half-null export pair",
            "UPDATE _norn_open_checkpoint SET export_seq = 1, export_hash = NULL",
        ),
        (
            "invalid mac hex",
            "UPDATE _norn_open_checkpoint SET mac = 'not-hex-zz'",
        ),
    ] {
        let copy = dir
            .path()
            .join(format!("{}.sqlite", name.replace(' ', "_")));
        std::fs::copy(&base, &copy).expect("copy");
        execute(&copy, sql);
        let store = open(&copy).expect("malformed row falls back to genesis open");
        assert!(
            history_ready(&store),
            "{name}: malformed row should open from genesis, got {:?}",
            store.scrub_status()
        );
    }
    // Tampered history beneath a malformed checkpoint row is refused by genesis open.
    let tampered = dir.path().join("tampered.sqlite");
    std::fs::copy(&base, &tampered).expect("copy");
    execute(
        &tampered,
        "UPDATE _norn_open_checkpoint SET commits = 'corrupt'",
    );
    execute(
        &tampered,
        "UPDATE _norn_fact_values SET hash = 'ff' || substr(hash, 3) WHERE commit_seq = 7",
    );
    assert!(
        open(&tampered).is_err(),
        "tampered history under a malformed checkpoint must still refuse"
    );
}

/// S4: `StoreReader::receipt_by_idempotency_key` refuses a miss with
/// `CellError::Verifying` until the receipts scrub finishes, while hits in
/// the verified suffix are served immediately.
#[test]
fn idempotency_lookup_miss_is_gated_until_receipts_scrub_finishes() {
    let dir = tempfile::tempdir().expect("directory");
    let path = dir.path().join("cell.sqlite");
    cell(&path, EVERY.saturating_add(10));
    let store = open(&path).expect("checkpointed open");
    assert!(!history_ready(&store));
    assert!(
        matches!(
            store
                .read()
                .receipt_by_idempotency_key(MAIN_WORLD, "never-committed"),
            Err(CellError::Verifying(_))
        ),
        "idempotency lookup miss must be gated until receipts scrub finishes"
    );
    let suffix_key = format!("put-{}", EVERY.saturating_add(5));
    assert!(
        matches!(
            store
                .read()
                .receipt_by_idempotency_key(MAIN_WORLD, &suffix_key),
            Ok(Some(_))
        ),
        "a hit in the verified suffix must be served"
    );
    // Advance until receipts phase finishes (before history completes or at Idle).
    loop {
        match store.scrub_step() {
            ScrubStatus::Verifying {
                phase: "receipts", ..
            } => {}
            ScrubStatus::Verifying { .. } | ScrubStatus::Idle => break,
            ScrubStatus::Fenced { finding } => panic!("unexpected fence: {finding}"),
        }
    }
    assert_eq!(
        store
            .read()
            .receipt_by_idempotency_key(MAIN_WORLD, "never-committed")
            .expect("miss allowed once receipts scrub finishes"),
        None
    );
}

/// S7: historical frames and knowledge-cutoff evidence are gated BEFORE
/// frontier lookup or population selection, even when covered history rows
/// have been deleted (empty population).
#[test]
fn historical_frames_are_gated_before_population_selection() {
    let dir = tempfile::tempdir().expect("directory");
    let clean = dir.path().join("clean.sqlite");
    cell(&clean, EVERY.saturating_add(10));
    let cutoff = KnowledgeCutoff {
        recorded_at: norn_kernel::LogicalTime::from_millis(10_000_000),
        frontier: 10,
    };
    let limits = ReactiveLimits::from_profile(&norn_ir::ResourceLimitProfile::norn_limits_v1());

    // Untampered: refused while scrubbing, served after scrub completes.
    {
        let store = open(&clean).expect("open");
        let mut budget = DecodeBudget::new(&limits);
        assert!(matches!(
            store
                .read()
                .historical_revision_with_budget(cutoff, &mut budget),
            Err(CellError::Verifying(_))
        ));
        assert!(matches!(
            store.read().fact_evidence_known_at_with_budget(
                MAIN_WORLD,
                "fixture.Item",
                "c1",
                cutoff,
                &mut budget
            ),
            Err(CellError::Verifying(_))
        ));
        assert_eq!(store.scrub_to_end(), ScrubStatus::Idle);
        let mut budget = DecodeBudget::new(&limits);
        assert!(
            store
                .read()
                .historical_revision_with_budget(cutoff, &mut budget)
                .expect("revision after scrub")
                .is_some()
        );
    }

    // Tampered: delete ALL covered fact values so population selection would be empty.
    let emptied = dir.path().join("emptied.sqlite");
    std::fs::copy(&clean, &emptied).expect("copy");
    execute(
        &emptied,
        &format!("DELETE FROM _norn_fact_values WHERE commit_seq <= {EVERY}"),
    );
    let store = open(&emptied).expect("checkpointed open");
    let mut budget = DecodeBudget::new(&limits);
    assert!(
        matches!(
            store
                .read()
                .historical_revision_with_budget(cutoff, &mut budget),
            Err(CellError::Verifying(_))
        ),
        "historical_revision_with_budget must refuse before population selection"
    );
    assert!(
        matches!(
            store.read().fact_evidence_known_at_with_budget(
                MAIN_WORLD,
                "fixture.Item",
                "c1",
                cutoff,
                &mut budget
            ),
            Err(CellError::Verifying(_))
        ),
        "fact_evidence_known_at_with_budget must refuse before population selection"
    );
    assert!(matches!(store.scrub_to_end(), ScrubStatus::Fenced { .. }));
    assert!(matches!(
        store
            .read()
            .historical_revision_with_budget(cutoff, &mut budget),
        Err(CellError::Fenced(_))
    ));
}

/// S8: `ledger()`, `export_commit_count()`, `export_record()`, and
/// transparency proof reads are all gated until the history scrub finishes.
#[test]
fn ledger_export_and_transparency_reads_are_gated_until_scrub_ends() {
    let dir = tempfile::tempdir().expect("directory");
    let path = dir.path().join("cell.sqlite");
    cell(&path, EVERY.saturating_add(10));
    let (_, covered_seq, ..) = checkpoint(&path).expect("checkpoint");
    let store = open(&path).expect("checkpointed open");
    assert!(matches!(
        store.read().ledger(0),
        Err(CellError::Verifying(_))
    ));
    assert!(matches!(
        store.export_commit_count(),
        Err(CellError::Verifying(_))
    ));
    assert!(matches!(
        store.export_record(1),
        Err(CellError::Verifying(_))
    ));
    assert!(matches!(
        store.transparency_checkpoint(),
        Err(CellError::Verifying(_))
    ));
    assert!(matches!(
        store.transparency_inclusion(0),
        Err(CellError::Verifying(_))
    ));
    assert!(matches!(
        store.transparency_consistency(1),
        Err(CellError::Verifying(_))
    ));
    // Verified suffix remains readable.
    assert!(store.read().ledger_page(covered_seq, 1).is_ok());

    assert_eq!(store.scrub_to_end(), ScrubStatus::Idle);
    assert!(
        store.read().ledger(0).expect("ledger after scrub").len()
            >= usize::try_from(EVERY).expect("count")
    );
    assert_eq!(
        store.export_commit_count().expect("export count"),
        EVERY.saturating_add(10)
    );
    assert!(store.export_record(1).expect("export record").is_some());
    assert!(store.transparency_checkpoint().is_ok());
    assert!(
        store
            .transparency_inclusion(0)
            .expect("inclusion")
            .is_some()
    );
    assert!(
        store
            .transparency_consistency(1)
            .expect("consistency")
            .is_some()
    );
}

/// S10: a seed step never regresses a newer authentic checkpoint written by
/// a concurrent handle inside the checkpoint transaction.
#[test]
fn checkpoint_seed_does_not_regress_newer_concurrent_checkpoint() {
    let dir = tempfile::tempdir().expect("directory");
    let path = dir.path().join("cell.sqlite");
    let first_open = EVERY.saturating_add(10);
    let advanced = EVERY.saturating_mul(2);
    cell(&path, first_open);

    // Handle A opens at `first_open` with a pending scrub that targets `first_open`.
    let handle_a = open(&path).expect("handle A");
    assert!(!history_ready(&handle_a));

    // Handle B opens, scrubs to Idle, and commits up to `2 * EVERY` so the
    // durable checkpoint advances to `2 * EVERY`.
    let mut handle_b = open(&path).expect("handle B");
    assert_eq!(handle_b.scrub_to_end(), ScrubStatus::Idle);
    for n in first_open..advanced {
        handle_b.commit(&put(n)).expect("commit on handle B");
    }
    drop(handle_b);
    assert_eq!(
        checkpoint(&path).map(|(commits, ..)| commits),
        Some(advanced)
    );

    // Handle A finishes its scrub; its Seed step must NOT overwrite the newer
    // authentic checkpoint at `2 * EVERY` with `first_open`.
    assert_eq!(handle_a.scrub_to_end(), ScrubStatus::Idle);
    drop(handle_a);
    assert_eq!(
        checkpoint(&path).map(|(commits, ..)| commits),
        Some(advanced),
        "Seed must not regress a newer authentic checkpoint"
    );
}

/// Wraps a `NativeEngine` and injects a single transient commit fault AFTER
/// the transaction callback has run and returned `Ok`, rolling back the
/// transaction when `fail_next` is armed (S11 / S12).
struct OneShotIoFault {
    inner: NativeEngine,
    fail_next: std::rc::Rc<Cell<bool>>,
    callbacks_rolled_back: std::rc::Rc<Cell<u32>>,
}

impl SqlEngine for OneShotIoFault {
    fn read_isolation(&self) -> ReadIsolation {
        self.inner.read_isolation()
    }
    fn checkpoint_key(&self) -> Option<CheckpointKey> {
        self.inner.checkpoint_key()
    }
    fn external_data_version(&self) -> Result<Option<u64>, CellError> {
        self.inner.external_data_version()
    }
    fn definition_writes<T>(
        &self,
        f: impl FnOnce() -> Result<T, CellError>,
    ) -> Result<T, CellError> {
        self.inner.definition_writes(f)
    }
    fn execute(&self, sql: &str, params: &[SqlValue]) -> Result<(), CellError> {
        self.inner.execute(sql, params)
    }
    fn query(&self, sql: &str, params: &[SqlValue]) -> Result<Vec<Vec<SqlValue>>, CellError> {
        self.inner.query(sql, params)
    }
    fn execute_batch(&self, sql: &str) -> Result<(), CellError> {
        self.inner.execute_batch(sql)
    }
    fn transaction<T, X: From<CellError>>(&self, f: impl FnOnce() -> Result<T, X>) -> Result<T, X> {
        if !self.fail_next.replace(false) {
            return self.inner.transaction(f);
        }
        let ran = Cell::new(false);
        let result: Result<T, X> = self.inner.transaction(|| {
            let _value = f()?;
            ran.set(true);
            Err(CellError::Engine("transient sqlite commit fault".to_owned()).into())
        });
        assert!(
            ran.get(),
            "transaction callback must have executed before injected commit rollback"
        );
        self.callbacks_rolled_back
            .set(self.callbacks_rolled_back.get().saturating_add(1));
        result
    }
}

/// S11: staged scrub progress applies to `Run` only after each step
/// transaction commits, so transient SQLite commit faults AFTER the callback
/// runs in Receipts, History, and Seed phases roll back cleanly without
/// corrupting accumulated state.
#[test]
fn staged_scrub_survives_transient_engine_io_fault() {
    let dir = tempfile::tempdir().expect("directory");
    let path = dir.path().join("cell.sqlite");
    let total = EVERY.saturating_add(10);
    cell(&path, total);

    let fail_next = std::rc::Rc::new(Cell::new(false));
    let rolled_back = std::rc::Rc::new(Cell::new(0));
    let engine = OneShotIoFault {
        inner: NativeEngine::open(&path, keyed()).expect("native engine"),
        fail_next: std::rc::Rc::clone(&fail_next),
        callbacks_rolled_back: std::rc::Rc::clone(&rolled_back),
    };
    let store = CellStore::from_engine(engine, engine_fixture::model()).expect("open");
    assert!(!history_ready(&store));

    // Inject transient COMMIT fault during Receipts phase (callback runs, then rolls back).
    fail_next.set(true);
    let status = store.scrub_step();
    assert!(
        matches!(
            status,
            ScrubStatus::Verifying {
                phase: "receipts",
                verified: 0,
                ..
            }
        ),
        "transient receipts commit fault must not mutate Run, got {status:?}"
    );
    assert_eq!(rolled_back.get(), 1);

    // Advance into History phase.
    loop {
        let status = store.scrub_step();
        if let ScrubStatus::Verifying { phase, .. } = status {
            if phase == "history" {
                break;
            }
            assert_eq!(phase, "receipts", "expected receipts or history phase");
        } else {
            panic!("expected history phase, got {status:?}");
        }
    }

    // Inject transient COMMIT fault during History phase (callback runs, then rolls back).
    fail_next.set(true);
    let status = store.scrub_step();
    assert!(
        matches!(
            status,
            ScrubStatus::Verifying {
                phase: "history",
                verified: 0,
                ..
            }
        ),
        "transient history commit fault must not mutate Run, got {status:?}"
    );
    assert_eq!(rolled_back.get(), 2);

    // Advance into Seed phase.
    loop {
        let status = store.scrub_step();
        if let ScrubStatus::Verifying { phase, .. } = status {
            if phase == "seed" {
                break;
            }
            assert_eq!(phase, "history", "expected history or seed phase");
        } else {
            panic!("expected seed phase, got {status:?}");
        }
    }

    // Inject transient COMMIT fault during Seed phase (checkpoint write rolls back).
    fail_next.set(true);
    let status = store.scrub_step();
    assert!(
        matches!(status, ScrubStatus::Verifying { phase: "seed", .. }),
        "transient seed commit fault must retry cleanly, got {status:?}"
    );
    assert_eq!(rolled_back.get(), 3);
    assert_eq!(
        checkpoint(&path).map(|(commits, ..)| commits),
        Some(EVERY),
        "rolled-back seed must not persist new checkpoint"
    );

    // Scrub finishes cleanly without corrupted walk/quantity/tree state.
    assert_eq!(store.scrub_to_end(), ScrubStatus::Idle);
    assert!(history_ready(&store));
    drop(store);
    assert_eq!(
        checkpoint(&path).map(|(commits, ..)| commits),
        Some(total),
        "staged scrub seeded the exact open head after transient commit retries"
    );
}

/// S12: when scrub detects a defect it drops `_norn_open_checkpoint` AND
/// persists `_norn_meta` key `open_checkpoint:fenced`, so sibling handles
/// (live reads, writes, `inject`, `fork` even with pending scrub, and
/// `scrub_status`/`scrub_step`) and subsequent reopens (keyed or unkeyed)
/// all fail closed with `Fenced`.
#[test]
fn durable_fence_persists_across_reopen_and_blocks_sibling_handles() {
    let dir = tempfile::tempdir().expect("directory");
    let path = dir.path().join("cell.sqlite");
    let total = EVERY.saturating_add(10);
    cell(&path, total);
    execute(&path, "DELETE FROM _norn_fact_values WHERE commit_seq = 10");

    let handle_a = open(&path).expect("handle A");
    let mut handle_b = open(&path).expect("handle B");
    let handle_b_head = handle_b.verified_head().expect("handle B head");
    assert!(!history_ready(&handle_b));

    assert!(matches!(
        handle_a.scrub_to_end(),
        ScrubStatus::Fenced { .. }
    ));
    // Sibling live reads refuse with Fenced.
    assert!(
        matches!(
            handle_b.read().object(MAIN_WORLD, "fixture.Item", "c0"),
            Err(CellError::Fenced(_))
        ),
        "sibling live object read must fail closed on durable fence"
    );
    assert!(
        matches!(
            handle_b.read().version(MAIN_WORLD, "fixture.Item", "c0"),
            Err(CellError::Fenced(_))
        ),
        "sibling live version read must fail closed on durable fence"
    );
    assert!(
        matches!(
            handle_b.read().list(MAIN_WORLD, "fixture.Item", 10),
            Err(CellError::Fenced(_))
        ),
        "sibling live list read must fail closed on durable fence"
    );
    assert!(
        matches!(handle_b.verified_head(), Err(CellError::Fenced(_))),
        "sibling verified_head must fail closed on durable fence"
    );
    // Sibling commit and receipt-less writes (`inject`) refuse with Fenced.
    assert!(
        matches!(
            handle_b.commit(&put(total.saturating_add(1))),
            Err(CellError::Fenced(_))
        ),
        "sibling commit must fail closed on durable fence"
    );
    assert!(
        matches!(
            handle_b.inject("obs-after-fence", &RuntimeValue::Null),
            Err(CellError::Fenced(_))
        ),
        "sibling inject must fail closed on durable fence"
    );
    // Sibling with pending scrub must return Fenced (NOT Verifying) on fork().
    assert!(
        matches!(
            handle_b.fork("what-if", &handle_b_head),
            Err(CellError::Fenced(_))
        ),
        "sibling fork with pending scrub must prefer Fenced over Verifying"
    );
    // Sibling scrub_status() and scrub_step() observe the durable fence.
    assert!(matches!(
        handle_b.scrub_status(),
        ScrubStatus::Fenced { .. }
    ));
    assert!(!handle_b.scrub_pending());
    assert!(matches!(handle_b.scrub_step(), ScrubStatus::Fenced { .. }));
    drop(handle_a);
    drop(handle_b);

    assert_eq!(checkpoint(&path), None, "fence dropped checkpoint");
    assert!(
        matches!(open(&path), Err(CellError::Fenced(_))),
        "keyed reopen must fail closed with Fenced"
    );
    assert!(
        matches!(open_unkeyed(&path), Err(CellError::Fenced(_))),
        "unkeyed reopen must also fail closed with Fenced"
    );
}

struct NthCommitFault {
    inner: NativeEngine,
    skip_txs: std::rc::Rc<Cell<u32>>,
    armed: std::rc::Rc<Cell<bool>>,
}

impl SqlEngine for NthCommitFault {
    fn read_isolation(&self) -> ReadIsolation {
        self.inner.read_isolation()
    }
    fn checkpoint_key(&self) -> Option<CheckpointKey> {
        self.inner.checkpoint_key()
    }
    fn external_data_version(&self) -> Result<Option<u64>, CellError> {
        self.inner.external_data_version()
    }
    fn definition_writes<T>(
        &self,
        f: impl FnOnce() -> Result<T, CellError>,
    ) -> Result<T, CellError> {
        self.inner.definition_writes(f)
    }
    fn execute(&self, sql: &str, params: &[SqlValue]) -> Result<(), CellError> {
        self.inner.execute(sql, params)
    }
    fn query(&self, sql: &str, params: &[SqlValue]) -> Result<Vec<Vec<SqlValue>>, CellError> {
        self.inner.query(sql, params)
    }
    fn execute_batch(&self, sql: &str) -> Result<(), CellError> {
        self.inner.execute_batch(sql)
    }
    fn transaction<T, X: From<CellError>>(&self, f: impl FnOnce() -> Result<T, X>) -> Result<T, X> {
        if !self.armed.get() {
            return self.inner.transaction(f);
        }
        let left = self.skip_txs.get();
        if left > 0 {
            self.skip_txs.set(left.saturating_sub(1));
            return self.inner.transaction(f);
        }
        self.armed.set(false);
        self.inner.transaction(|| {
            let _ok = f()?;
            Err(CellError::Engine("transient fence_durably commit fault".to_owned()).into())
        })
    }
}

/// S12 (#1551 round 5): if `fence_durably` fails transiently when scrub first
/// detects a defect, the handle keeps `fence_persisted = false` and retries
/// persisting the durable fence on subsequent calls until `_norn_meta` records
/// `open_checkpoint:fenced` and `_norn_open_checkpoint` is deleted.
#[test]
fn transient_fence_persistence_failure_retries_until_durable() {
    let dir = tempfile::tempdir().expect("directory");
    let path = dir.path().join("cell.sqlite");
    let total = EVERY.saturating_add(10);
    cell(&path, total);
    execute(&path, "DELETE FROM _norn_fact_values WHERE commit_seq = 10");

    // Use a 2nd-transaction commit fault: tx 0 is the failing history step
    // (returns LedgerCorrupt), tx 1 is `fence_durably` (returns Ok -> rolled back!).
    let skip_txs = std::rc::Rc::new(Cell::new(0));
    let armed = std::rc::Rc::new(Cell::new(false));
    let store = CellStore::from_engine(
        NthCommitFault {
            inner: NativeEngine::open(&path, keyed()).expect("native engine"),
            skip_txs: std::rc::Rc::clone(&skip_txs),
            armed: std::rc::Rc::clone(&armed),
        },
        engine_fixture::model(),
    )
    .expect("open");

    loop {
        let status = store.scrub_step();
        if matches!(
            status,
            ScrubStatus::Verifying {
                phase: "history",
                ..
            }
        ) {
            break;
        }
        assert!(
            matches!(
                status,
                ScrubStatus::Verifying {
                    phase: "receipts",
                    ..
                }
            ),
            "expected history phase, got {status:?}"
        );
    }
    // Skip tx 0 (history step returning LedgerCorrupt), fail tx 1 (fence_durably).
    skip_txs.set(1);
    armed.set(true);
    assert!(matches!(store.scrub_step(), ScrubStatus::Fenced { .. }));
    assert!(!armed.get(), "fence_durably commit fault must have fired");
    assert!(
        checkpoint(&path).is_some(),
        "transient fence_durably failure left checkpoint in place until retried"
    );

    // Next call retries `fence_durably`, deleting checkpoint and persisting fence.
    assert!(matches!(store.scrub_step(), ScrubStatus::Fenced { .. }));
    assert_eq!(checkpoint(&path), None, "retry deleted checkpoint");
    drop(store);
    assert!(
        matches!(open(&path), Err(CellError::Fenced(_))),
        "reopen fails closed after retried durable fence persistence"
    );
}

/// S10 (#1551 round 5): trimmed restores at `<= MAX_ADVANCE` (`<= 256` commits,
/// e.g. 100 and 200 commits) drop the stale past-end checkpoint immediately on
/// open and advance cleanly on subsequent periodic checkpoints.
#[test]
fn a_trimmed_restore_under_max_advance_drops_stale_checkpoint_and_advances() {
    let dir = tempfile::tempdir().expect("directory");
    let end = EVERY.saturating_mul(4).saturating_add(2);
    for (cut, next_cp) in [(100_u64, EVERY), (200_u64, EVERY.saturating_mul(2))] {
        let path = dir.path().join(format!("cell-{cut}.sqlite"));
        let restored = dir.path().join(format!("restored-{cut}.sqlite"));
        cell(&path, cut);
        std::fs::copy(&path, &restored).expect("snapshot");
        cell_more(&path, cut, end);
        let row = raw(&path)
            .query("SELECT * FROM _norn_open_checkpoint", &[])
            .expect("later checkpoint")
            .remove(0);
        let engine = raw(&restored);
        engine
            .execute("DELETE FROM _norn_open_checkpoint", &[])
            .expect("clear");
        engine
            .execute(
                "INSERT INTO _norn_open_checkpoint VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
                &row,
            )
            .expect("copy past-end checkpoint");
        drop(engine);

        let mut store = open(&restored).expect("trimmed restore opens from genesis");
        assert!(history_ready(&store));
        assert_eq!(
            checkpoint(&restored),
            None,
            "cut={cut}: stale past-end checkpoint must be dropped immediately on open"
        );
        for n in cut..=next_cp {
            store.commit(&put(n)).expect("commit");
        }
        drop(store);
        assert_eq!(
            checkpoint(&restored).map(|(commits, ..)| commits),
            Some(next_cp),
            "cut={cut}: periodic advancement must record the new checkpoint at {next_cp}"
        );
    }
}

/// S10 (#1551 round 5): if an external SQLite connection mutates an already-
/// scrubbed earlier row between scrub transactions, `PRAGMA data_version`
/// changes, forcing the scrub to restart from genesis instead of stamping a
/// checkpoint over tampered history; the restarted scrub detects the tamper
/// and fences the cell durably.
#[test]
fn external_mutation_between_scrub_steps_restarts_and_fences_cell() {
    let dir = tempfile::tempdir().expect("directory");
    let path = dir.path().join("cell.sqlite");
    let total = EVERY.saturating_add(10);
    cell(&path, total);

    let store = open(&path).expect("checkpointed open");
    assert!(!history_ready(&store));

    // Advance past Receipts AND the first History chunk (commits 1..=64 passed!).
    loop {
        let status = store.scrub_step();
        if matches!(
            status,
            ScrubStatus::Verifying {
                phase: "history",
                verified,
                ..
            } if verified >= 64
        ) {
            break;
        }
        assert!(
            matches!(status, ScrubStatus::Verifying { .. }),
            "expected history progress >= 64, got {status:?}"
        );
    }

    // Mutate an already-scrubbed earlier row (commit 10) from a second SQLite connection!
    execute(&path, "DELETE FROM _norn_fact_values WHERE commit_seq = 10");

    // Seed must NOT clear the gate or stamp a checkpoint; scrub restarts and fences!
    let final_status = store.scrub_to_end();
    assert!(
        matches!(final_status, ScrubStatus::Fenced { .. }),
        "external mid-scrub tamper must restart scrub and fence the cell, got {final_status:?}"
    );
    assert!(!history_ready(&store));
    drop(store);
    assert_eq!(checkpoint(&path), None, "fence dropped checkpoint");
    assert!(matches!(open(&path), Err(CellError::Fenced(_))));
}

fn measured_model() -> norn_cell::Model {
    use norn_ir::{
        ObjectTypeDef, ObjectTypeName, Property, PropertyConstraints, PropertyId, PropertyType,
    };
    use norn_projections::SqliteDdlInput;

    let object_type = ObjectTypeDef {
        immutable: false,
        name: ObjectTypeName("fixture.Measured".to_owned()),
        role: None,
        description: None,
        status: norn_ir::ResourceStatus::default(),
        extends: None,
        implements: vec![],
        properties: vec![
            Property {
                protected: false,
                origin: None,
                immutable_after_create: false,
                id: PropertyId("unit".to_owned()),
                description: None,
                property_type: PropertyType::Text { format: None },
                constraints: PropertyConstraints::default(),
                default: None,
                markings: vec![],
                computed: None,
            },
            Property {
                protected: false,
                origin: None,
                immutable_after_create: false,
                id: PropertyId("amount".to_owned()),
                description: None,
                property_type: PropertyType::Decimal {
                    additivity: norn_ir::PropertyAdditivity::default(),
                    scale: norn_ir::WholeNumber(2),
                    rounding: None,
                    unit: None,
                    currency_source: None,
                    per_unit: None,
                    unit_source: vec![PropertyId("unit".to_owned())],
                },
                constraints: PropertyConstraints::default(),
                default: None,
                markings: vec![],
                computed: None,
            },
        ],
        links: vec![],
        owned_collections: vec![],
        identity: vec![],
        invariants: vec![],
        lifecycles: vec![],
        markings: vec![],
        accounting_leg: None,
    };
    norn_cell::Model::new(&SqliteDdlInput {
        object_types: &[object_type],
        property_types: &[],
    })
    .expect("model")
}

/// S7/S8 (#1551 round 5): `source_disagreements` scans `_norn_fact_values` and
/// must be gated by `history_gate` until the scrub finishes.
#[test]
fn source_disagreements_are_gated_until_scrub_ends() {
    let model = measured_model();

    let dir = tempfile::tempdir().expect("directory");
    let path = dir.path().join("measured.sqlite");
    {
        let mut store = CellStore::open(&path, model.clone(), keyed()).expect("open");
        for n in 0..EVERY.saturating_add(5) {
            let batch = CommitBatch {
                world: MAIN_WORLD.to_owned(),
                scope: "s".to_owned(),
                logical_time: 10_000_u64.saturating_add(n),
                idempotency_key: Some(format!("m-{n}")),
                writes: vec![FactWriteRow::Put {
                    object_type: "fixture.Measured".to_owned(),
                    key: "m1".to_owned(),
                    values: BTreeMap::from([
                        (
                            "unit".to_owned(),
                            RuntimeValue::from(if n.is_multiple_of(2) { "kg" } else { "lb" }),
                        ),
                        ("amount".to_owned(), RuntimeValue::from("10.00")),
                    ]),
                }],
                events: vec![
                    runtime_object! { "type" => "fixture.measured", "n" => i64::try_from(n).expect("n") },
                ],
                ..CommitBatch::default()
            };
            store.commit(&batch).expect("commit");
        }
    }

    let store = CellStore::open(&path, model, keyed()).expect("checkpointed open");
    assert!(!history_ready(&store));
    assert!(
        matches!(
            store
                .read()
                .source_disagreements(MAIN_WORLD, "fixture.Measured", "m1", None),
            Err(CellError::Verifying(_))
        ),
        "source_disagreements must refuse with Verifying during incomplete scrub"
    );
    assert_eq!(store.scrub_to_end(), ScrubStatus::Idle);
    assert!(
        store
            .read()
            .source_disagreements(MAIN_WORLD, "fixture.Measured", "m1", None)
            .is_ok(),
        "source_disagreements succeeds once scrub completes"
    );
}

/// S7/S8 (#1551 round 5): `analytics_delta`, `dataset_manifest`,
/// `observation_is_quarantined`, `observation_findings`, and `analytics_source`
/// all scan covered history and must refuse with `Verifying` until scrub ends
/// and `Fenced` when fenced.
#[test]
fn analytics_delta_dataset_manifest_and_quarantine_reads_are_gated_until_scrub_ends() {
    let dir = tempfile::tempdir().expect("directory");
    let path = dir.path().join("cell.sqlite");
    cell(&path, EVERY.saturating_add(10));
    let tenant = norn_cell::analytics::TenantId::of_scope(engine_fixture::SCOPE);

    let store = open(&path).expect("checkpointed open");
    assert!(!history_ready(&store));
    let record = store
        .read()
        .object(MAIN_WORLD, "fixture.Item", "c0")
        .expect("live object")
        .expect("c0 exists");

    assert!(
        matches!(
            store.analytics_delta(&tenant, MAIN_WORLD, 0, 100),
            Err(CellError::Verifying(_))
        ),
        "analytics_delta must refuse with Verifying while scrub is incomplete"
    );
    assert!(
        matches!(
            store.dataset_manifest(MAIN_WORLD, "fixture.Item"),
            Err(CellError::Verifying(_))
        ),
        "dataset_manifest must refuse with Verifying while scrub is incomplete"
    );
    assert!(
        matches!(
            store.read().observation_is_quarantined(MAIN_WORLD, &record),
            Err(CellError::Verifying(_))
        ),
        "observation_is_quarantined must refuse with Verifying while scrub is incomplete"
    );
    assert!(
        matches!(
            store
                .read()
                .observation_findings(MAIN_WORLD, "fixture.Item", "c0"),
            Err(CellError::Verifying(_))
        ),
        "observation_findings must refuse with Verifying while scrub is incomplete"
    );
    assert!(
        matches!(
            store.analytics_source(MAIN_WORLD, engine_fixture::SCOPE, "fixture.Item"),
            Err(CellError::Verifying(_))
        ),
        "analytics_source must refuse with Verifying while scrub is incomplete"
    );

    assert_eq!(store.scrub_to_end(), ScrubStatus::Idle);
    assert!(store.analytics_delta(&tenant, MAIN_WORLD, 0, 100).is_ok());
    assert!(store.dataset_manifest(MAIN_WORLD, "fixture.Item").is_ok());
    assert!(
        store
            .read()
            .observation_is_quarantined(MAIN_WORLD, &record)
            .is_ok()
    );
    assert!(
        store
            .analytics_source(MAIN_WORLD, engine_fixture::SCOPE, "fixture.Item")
            .is_ok()
    );
    drop(store);

    // Tamper covered history -> scrub fences -> all refuse with Fenced.
    execute(&path, "DELETE FROM _norn_fact_values WHERE commit_seq = 10");
    let fenced = open(&path).expect("open before scrub");
    assert!(matches!(fenced.scrub_to_end(), ScrubStatus::Fenced { .. }));
    assert!(matches!(
        fenced.analytics_delta(&tenant, MAIN_WORLD, 0, 100),
        Err(CellError::Fenced(_))
    ));
    assert!(matches!(
        fenced.dataset_manifest(MAIN_WORLD, "fixture.Item"),
        Err(CellError::Fenced(_))
    ));
    assert!(matches!(
        fenced
            .read()
            .observation_is_quarantined(MAIN_WORLD, &record),
        Err(CellError::Fenced(_))
    ));
}

/// S13: `CellStore::fork` refuses with `Verifying` while the history scrub
/// is incomplete, refuses with `Fenced` when fenced, and succeeds once the
/// scrub reaches `Idle`.
#[test]
fn fork_is_refused_while_scrub_is_incomplete_or_fenced() {
    let dir = tempfile::tempdir().expect("directory");
    let path = dir.path().join("cell.sqlite");
    cell(&path, EVERY.saturating_add(10));

    let store = open(&path).expect("checkpointed open");
    let head = store.verified_head().expect("head");
    assert!(
        matches!(store.fork("what-if", &head), Err(CellError::Verifying(_))),
        "fork must refuse while scrub is incomplete"
    );
    assert_eq!(store.scrub_to_end(), ScrubStatus::Idle);
    assert!(
        store.fork("what-if", &head).is_ok(),
        "fork succeeds once scrub completes"
    );
    drop(store);

    // Fenced cell refuses fork with Fenced.
    execute(&path, "DELETE FROM _norn_fact_values WHERE commit_seq = 10");
    let fenced = open(&path).expect("open before scrub");
    let head = fenced.verified_head().expect("head");
    assert!(matches!(fenced.scrub_to_end(), ScrubStatus::Fenced { .. }));
    assert!(
        matches!(fenced.fork("what-if", &head), Err(CellError::Fenced(_))),
        "fork must refuse on a fenced cell"
    );
}

/// S10 (#1551 round 6): if a commit extends the ledger during scrub and an
/// external connection subsequently corrupts that suffix, the `data_version`
/// restart refreshes the scrub target to the live tail and fences the cell.
#[test]
fn external_suffix_corruption_after_mid_scrub_commit_restarts_and_fences() {
    let dir = tempfile::tempdir().expect("directory");
    let path = dir.path().join("cell.sqlite");
    let total = EVERY.saturating_add(10);
    cell(&path, total);

    let mut store = open(&path).expect("checkpointed open");
    assert!(!history_ready(&store));

    // Advance into History phase so the scrub has already completed Receipts.
    loop {
        let status = store.scrub_step();
        if matches!(
            status,
            ScrubStatus::Verifying {
                phase: "history",
                verified,
                ..
            } if verified >= 64
        ) {
            break;
        }
        assert!(
            matches!(status, ScrubStatus::Verifying { .. }),
            "expected history progress >= 64, got {status:?}"
        );
    }

    // Commit a new suffix during the active scrub (extends past original target).
    let suffix_commit = total.saturating_add(1);
    let outcome = store.commit(&put(suffix_commit)).expect("mid-scrub commit");
    assert_eq!(outcome.receipt().body.commit_seq, suffix_commit);

    // Corrupt that suffix externally via a second SQLite connection!
    execute(
        &path,
        &format!("DELETE FROM _norn_fact_values WHERE commit_seq = {suffix_commit}"),
    );

    // Restart must refresh target to `suffix_commit`, detect corruption, and fence!
    let final_status = store.scrub_to_end();
    assert!(
        matches!(final_status, ScrubStatus::Fenced { .. }),
        "external suffix tamper must restart through the suffix and fence, got {final_status:?}"
    );
    assert!(!history_ready(&store));
    drop(store);
    assert_eq!(checkpoint(&path), None, "fence dropped checkpoint");
    assert!(matches!(open(&path), Err(CellError::Fenced(_))));
}

/// S8/S12 (#1551 round 6): `dump_filtered`/`dump` refuse during incomplete scrub
/// and on a sibling durable fence; `flow_run`/`flow_runs`/`live_runs` refuse on
/// a sibling durable fence.
#[test]
fn dump_filtered_and_flow_run_respect_scrub_and_durable_fence() {
    let dir = tempfile::tempdir().expect("directory");
    let path = dir.path().join("cell.sqlite");
    cell(&path, EVERY.saturating_add(10));

    let store = open(&path).expect("checkpointed open");
    assert!(!history_ready(&store));
    assert!(matches!(
        store.read().dump_filtered(|_| true),
        Err(CellError::Verifying(_))
    ));
    assert!(matches!(store.read().dump(), Err(CellError::Verifying(_))));
    assert!(store.read().flow_run(MAIN_WORLD, "run-1").is_ok());
    assert_eq!(store.scrub_to_end(), ScrubStatus::Idle);
    assert!(store.read().dump_filtered(|_| true).is_ok());
    drop(store);

    // Sibling opens before first handle fences durably.
    execute(&path, "DELETE FROM _norn_fact_values WHERE commit_seq = 10");
    let first = open(&path).expect("first open");
    let sibling = open(&path).expect("sibling open");
    assert!(matches!(first.scrub_to_end(), ScrubStatus::Fenced { .. }));

    let filter = norn_cell::flow_run::RunFilter::default();
    assert!(matches!(
        sibling.read().dump_filtered(|_| true),
        Err(CellError::Fenced(_))
    ));
    assert!(matches!(
        sibling.read().flow_run(MAIN_WORLD, "run-1"),
        Err(CellError::Fenced(_))
    ));
    assert!(matches!(
        sibling.read().flow_runs(MAIN_WORLD, &filter),
        Err(CellError::Fenced(_))
    ));
    assert!(matches!(
        sibling.read().live_runs(MAIN_WORLD, "flow.one"),
        Err(CellError::Fenced(_))
    ));
}

struct FenceOnBeginEngine {
    inner: NativeEngine,
    path: std::path::PathBuf,
    armed: std::rc::Rc<Cell<bool>>,
}

impl SqlEngine for FenceOnBeginEngine {
    fn read_isolation(&self) -> ReadIsolation {
        self.inner.read_isolation()
    }
    fn checkpoint_key(&self) -> Option<CheckpointKey> {
        self.inner.checkpoint_key()
    }
    fn external_data_version(&self) -> Result<Option<u64>, CellError> {
        self.inner.external_data_version()
    }
    fn definition_writes<T>(
        &self,
        f: impl FnOnce() -> Result<T, CellError>,
    ) -> Result<T, CellError> {
        self.inner.definition_writes(f)
    }
    fn execute(&self, sql: &str, params: &[SqlValue]) -> Result<(), CellError> {
        self.inner.execute(sql, params)
    }
    fn query(&self, sql: &str, params: &[SqlValue]) -> Result<Vec<Vec<SqlValue>>, CellError> {
        self.inner.query(sql, params)
    }
    fn execute_batch(&self, sql: &str) -> Result<(), CellError> {
        self.inner.execute_batch(sql)
    }
    fn transaction<T, X: From<CellError>>(&self, f: impl FnOnce() -> Result<T, X>) -> Result<T, X> {
        if self.armed.replace(false) {
            execute(
                &self.path,
                "INSERT OR REPLACE INTO _norn_meta (key, value) VALUES ('open_checkpoint:fenced', 'sibling raced before bind tx')",
            );
        }
        self.inner.transaction(f)
    }
}

/// S12 (#1551 round 6): `CellStore::bind` checks the durable fence INSIDE its
/// write transaction, refusing if a sibling fences between pre-check and BEGIN.
#[test]
fn bind_refuses_when_sibling_fences_before_bind_transaction() {
    let dir = tempfile::tempdir().expect("directory");
    let path = dir.path().join("cell.sqlite");
    cell(&path, 10);

    let armed = std::rc::Rc::new(Cell::new(false));
    let mut store = CellStore::from_engine(
        FenceOnBeginEngine {
            inner: NativeEngine::open(&path, keyed()).expect("native engine"),
            path: path.clone(),
            armed: std::rc::Rc::clone(&armed),
        },
        engine_fixture::model(),
    )
    .expect("open");

    armed.set(true);
    let refused = store.bind("late_key", "late_value");
    assert!(
        matches!(refused, Err(CellError::Fenced(_))),
        "bind must fail closed inside its write transaction, got {refused:?}"
    );
    let rows = raw(&path)
        .query(
            "SELECT value FROM _norn_meta WHERE key = 'binding:late_key'",
            &[],
        )
        .expect("query meta");
    assert!(rows.is_empty(), "fenced bind must not persist a binding");
}

/// S12 (#1551 round 7): an already-acquired `StoreReader` observes a sibling
/// durable fence written after `store.read()` was called.
#[test]
fn existing_store_reader_refuses_when_sibling_fences_after_acquire() {
    let dir = tempfile::tempdir().expect("directory");
    let path = dir.path().join("cell.sqlite");
    cell(&path, EVERY.saturating_add(10));
    execute(&path, "DELETE FROM _norn_fact_values WHERE commit_seq = 10");

    let first = open(&path).expect("first open");
    let sibling = open(&path).expect("sibling open");
    // Acquire the reader BEFORE the sibling fences durably.
    let reader = sibling.read();
    let limits = ReactiveLimits::from_profile(&norn_ir::ResourceLimitProfile::norn_limits_v1());
    assert!(reader.flow_run(MAIN_WORLD, "run-1").is_ok());
    let c0_ver = reader
        .object(MAIN_WORLD, "fixture.Item", "c0")
        .expect("c0")
        .expect("present")
        .version;
    assert!(reader.head_commit_seq().is_ok());
    assert!(reader.database_revision().is_ok());
    assert!(
        reader
            .source_event_mapping_bundles("conn", "p0", "src", "s1", "e1")
            .is_ok()
    );
    assert!(
        reader
            .fact_version_evidence_with_budget(
                MAIN_WORLD,
                "fixture.Item",
                "c0",
                c0_ver,
                &mut DecodeBudget::new(&limits),
            )
            .expect("evidence before fence")
            .is_some()
    );

    assert!(matches!(first.scrub_to_end(), ScrubStatus::Fenced { .. }));

    // The already-acquired reader must fail closed via `PRAGMA data_version`.
    assert!(matches!(
        reader.flow_run(MAIN_WORLD, "run-1"),
        Err(CellError::Fenced(_))
    ));
    assert!(matches!(
        reader.object(MAIN_WORLD, "fixture.Item", "c0"),
        Err(CellError::Fenced(_))
    ));
    assert!(matches!(
        reader.head_commit_seq(),
        Err(CellError::Fenced(_))
    ));
    assert!(matches!(
        reader.database_revision(),
        Err(CellError::Fenced(_))
    ));
    assert!(matches!(
        reader.source_event_mapping_bundles("conn", "p0", "src", "s1", "e1"),
        Err(CellError::Fenced(_))
    ));
    assert_budgeted_fact_evidence_fenced(&reader, c0_ver, &limits);
    assert!(matches!(
        sibling
            .read()
            .source_event_mapping_bundles("conn", "p0", "src", "s1", "e1"),
        Err(CellError::Fenced(_))
    ));
    let model = engine_fixture::model();
    let candidate = install_input(&model, "0.2.0");
    assert!(matches!(
        sibling.check_definitions(&model, &candidate, None),
        Err(CellError::Fenced(_))
    ));
    assert!(matches!(sibling.cell_id(), Err(CellError::Fenced(_))));
    assert!(matches!(
        sibling.perform_fence_generation("cmd-1"),
        Err(CellError::Fenced(_))
    ));
    assert_budgeted_links_fenced(&limits);
}

const ZERO_LIMITS: ReactiveLimits = ReactiveLimits {
    max_records: 0,
    max_canonical_bytes: 0,
    max_fanout_per_commit: 0,
    max_scan: 0,
    max_candidate_bytes_per_commit: 0,
};

fn assert_budgeted_fact_evidence_fenced(
    reader: &norn_cell::StoreReader<'_, NativeEngine>,
    version: u64,
    limits: &ReactiveLimits,
) {
    assert!(
        matches!(
            reader.fact_version_evidence(MAIN_WORLD, "fixture.Item", "c0", version),
            Err(CellError::Fenced(_))
        ),
        "fact_version_evidence must refuse after sibling durable fence"
    );
    for active in [limits, &ZERO_LIMITS] {
        assert!(
            matches!(
                reader.fact_version_evidence_with_budget(
                    MAIN_WORLD,
                    "fixture.Item",
                    "c0",
                    version,
                    &mut DecodeBudget::new(active),
                ),
                Err(CellError::Fenced(_))
            ),
            "fact_version_evidence_with_budget must refuse (never Ok(None)) after sibling fence"
        );
        assert!(
            matches!(
                reader.receipt_for_fact_version_with_budget(
                    MAIN_WORLD,
                    "fixture.Item",
                    "c0",
                    version,
                    &mut DecodeBudget::new(active),
                ),
                Err(CellError::Fenced(_))
            ),
            "receipt_for_fact_version_with_budget must refuse after sibling fence"
        );
        assert!(
            matches!(
                reader.object_at_version_with_budget(
                    MAIN_WORLD,
                    "fixture.Item",
                    "c0",
                    version,
                    &mut DecodeBudget::new(active),
                ),
                Err(CellError::Fenced(_))
            ),
            "object_at_version_with_budget must refuse after sibling fence"
        );
    }
}

fn linked_model() -> Model {
    let def: norn_ir::ObjectTypeDef = serde_json::from_value(serde_json::json!({
        "name": "fixture.Item",
        "properties": [
            {"id": "qty", "type": {"kind": "integer"}},
            {"id": "label", "type": {"kind": "text"}, "default": "item"}
        ],
        "links": [{
            "id": "next",
            "target": "fixture.Item",
            "cardinality": {"to": "many", "from": "many"}
        }]
    }))
    .expect("linked item def");
    Model::from_definitions(&[def], &[]).expect("linked model")
}

fn seed_linked_cell(path: &Path, model: &Model) {
    let mut store = CellStore::open(path, model.clone(), keyed()).expect("open linked");
    store.bind("scope", engine_fixture::SCOPE).expect("bind");
    store
        .commit(&CommitBatch {
            world: MAIN_WORLD.to_owned(),
            scope: engine_fixture::SCOPE.to_owned(),
            logical_time: 1_000,
            idempotency_key: Some("seed-nodes".to_owned()),
            writes: vec![
                FactWriteRow::Put {
                    object_type: "fixture.Item".to_owned(),
                    key: "c0".to_owned(),
                    values: BTreeMap::from([("qty".to_owned(), RuntimeValue::from(1_i64))]),
                },
                FactWriteRow::Put {
                    object_type: "fixture.Item".to_owned(),
                    key: "c1".to_owned(),
                    values: BTreeMap::from([("qty".to_owned(), RuntimeValue::from(2_i64))]),
                },
            ],
            ..CommitBatch::default()
        })
        .expect("seed nodes");
    store
        .commit(&CommitBatch {
            world: MAIN_WORLD.to_owned(),
            scope: engine_fixture::SCOPE.to_owned(),
            logical_time: 1_001,
            idempotency_key: Some("seed-link".to_owned()),
            links: vec![LinkChange::Add {
                object_type: "fixture.Item".to_owned(),
                link: "next".to_owned(),
                source: "c0".to_owned(),
                target: "c1".to_owned(),
                values: BTreeMap::new(),
                target_type: None,
            }],
            ..CommitBatch::default()
        })
        .expect("seed link");
    for n in 2..EVERY.saturating_add(5) {
        store.commit(&put(n)).expect("put");
    }
}

fn assert_budgeted_links_fenced(limits: &ReactiveLimits) {
    let dir = tempfile::tempdir().expect("directory");
    let path = dir.path().join("linked.sqlite");
    let model = linked_model();
    seed_linked_cell(&path, &model);
    execute(&path, "DELETE FROM _norn_fact_values WHERE commit_seq = 10");

    let first = CellStore::open(&path, model.clone(), keyed()).expect("first linked");
    let sibling = CellStore::open(&path, model, keyed()).expect("sibling linked");
    let reader = sibling.read();
    assert_eq!(
        reader
            .link_targets_with_budget(
                MAIN_WORLD,
                "fixture.Item",
                "next",
                "c0",
                &mut DecodeBudget::new(limits),
            )
            .expect("targets before fence"),
        BudgetedLinksRead::Complete(vec!["c1".to_owned()])
    );
    assert_eq!(
        reader
            .link_sources_to_with_budget(
                MAIN_WORLD,
                "fixture.Item",
                "next",
                "c1",
                8,
                &mut DecodeBudget::new(limits),
            )
            .expect("sources before fence"),
        BudgetedLinksRead::Complete(vec!["c0".to_owned()])
    );

    assert!(matches!(first.scrub_to_end(), ScrubStatus::Fenced { .. }));

    for active in [limits, &ZERO_LIMITS] {
        assert!(
            matches!(
                reader.link_targets_with_budget(
                    MAIN_WORLD,
                    "fixture.Item",
                    "next",
                    "c0",
                    &mut DecodeBudget::new(active),
                ),
                Err(CellError::Fenced(_))
            ),
            "link_targets_with_budget must refuse after sibling durable fence"
        );
        assert!(
            matches!(
                reader.link_sources_to_with_budget(
                    MAIN_WORLD,
                    "fixture.Item",
                    "next",
                    "c1",
                    8,
                    &mut DecodeBudget::new(active),
                ),
                Err(CellError::Fenced(_))
            ),
            "link_sources_to_with_budget must refuse after sibling durable fence"
        );
    }
}

/// S10 (#1551 round 7): a quantity commit landing during scrub whose
/// `_norn_quantity_history` row is corrupted externally must be caught after
/// restart and fence the cell durably.
#[test]
fn restart_scrub_detects_corrupt_suffix_quantity_history_and_fences() {
    let dir = tempfile::tempdir().expect("directory");
    let path = dir.path().join("cell.sqlite");
    quantity_cell(&path);

    let mut store = open(&path).expect("checkpointed open");
    assert!(!history_ready(&store));
    loop {
        let status = store.scrub_step();
        if matches!(
            status,
            ScrubStatus::Verifying {
                phase: "history",
                verified,
                ..
            } if verified >= 64
        ) {
            break;
        }
        assert!(matches!(status, ScrubStatus::Verifying { .. }));
    }

    // Commit a suffix quantity transition mid-scrub, then corrupt its history row externally.
    let suffix_commit = EVERY.saturating_add(1);
    commit_quantity(&mut store, suffix_commit, &post(0, "mid-scrub", "fund", 5));
    execute(
        &path,
        &format!(
            "UPDATE _norn_quantity_history SET posted = '999999' WHERE commit_seq = {suffix_commit}"
        ),
    );

    let final_status = store.scrub_to_end();
    assert!(
        matches!(final_status, ScrubStatus::Fenced { .. }),
        "restart must verify suffix _norn_quantity_history and fence, got {final_status:?}"
    );
    assert!(!history_ready(&store));
    drop(store);
    assert_eq!(checkpoint(&path), None, "fence dropped checkpoint");
    assert!(matches!(open(&path), Err(CellError::Fenced(_))));
}

struct RowBoundedEngine {
    inner: NativeEngine,
    armed: std::rc::Rc<Cell<bool>>,
    wake_rows: std::rc::Rc<Cell<usize>>,
    max_wake_rows: std::rc::Rc<Cell<usize>>,
}

impl SqlEngine for RowBoundedEngine {
    fn read_isolation(&self) -> ReadIsolation {
        self.inner.read_isolation()
    }
    fn checkpoint_key(&self) -> Option<CheckpointKey> {
        self.inner.checkpoint_key()
    }
    fn external_data_version(&self) -> Result<Option<u64>, CellError> {
        self.inner.external_data_version()
    }
    fn definition_writes<T>(
        &self,
        f: impl FnOnce() -> Result<T, CellError>,
    ) -> Result<T, CellError> {
        self.inner.definition_writes(f)
    }
    fn execute(&self, sql: &str, params: &[SqlValue]) -> Result<(), CellError> {
        self.inner.execute(sql, params)
    }
    fn query(&self, sql: &str, params: &[SqlValue]) -> Result<Vec<Vec<SqlValue>>, CellError> {
        let rows = self.inner.query(sql, params)?;
        if self.armed.get() {
            let total = self.wake_rows.get().saturating_add(rows.len());
            self.wake_rows.set(total);
            if total > self.max_wake_rows.get() {
                self.max_wake_rows.set(total);
            }
        }
        Ok(rows)
    }
    fn execute_batch(&self, sql: &str) -> Result<(), CellError> {
        self.inner.execute_batch(sql)
    }
    fn transaction<T, X: From<CellError>>(&self, f: impl FnOnce() -> Result<T, X>) -> Result<T, X> {
        self.inner.transaction(f)
    }
}

fn scrub_measuring_total_wake_rows(
    store: &CellStore<RowBoundedEngine>,
    armed: &Cell<bool>,
    wake_rows: &Cell<usize>,
) -> ScrubStatus {
    armed.set(true);
    loop {
        wake_rows.set(0);
        let status = store.scrub_step();
        if !matches!(status, ScrubStatus::Verifying { .. }) {
            return status;
        }
    }
}

/// S10 (#1551 round 7/10): restarting scrub after a sibling adds > 512 commits
/// keeps the TOTAL SQL rows summed across all queries in each wake <= 512.
#[test]
fn restart_scrub_after_large_sibling_suffix_stays_within_512_row_bound() {
    let dir = tempfile::tempdir().expect("directory");
    let path = dir.path().join("cell.sqlite");
    let initial = EVERY.saturating_add(10);
    cell(&path, initial);

    let armed = std::rc::Rc::new(Cell::new(false));
    let wake_rows = std::rc::Rc::new(Cell::new(0));
    let max_wake_rows = std::rc::Rc::new(Cell::new(0));
    let store = CellStore::from_engine(
        RowBoundedEngine {
            inner: NativeEngine::open(&path, keyed()).expect("native engine"),
            armed: std::rc::Rc::clone(&armed),
            wake_rows: std::rc::Rc::clone(&wake_rows),
            max_wake_rows: std::rc::Rc::clone(&max_wake_rows),
        },
        engine_fixture::model(),
    )
    .expect("checkpointed open");
    assert!(!history_ready(&store));
    assert!(matches!(store.scrub_step(), ScrubStatus::Verifying { .. }));

    // Sibling adds 600 commits (> RECEIPT_STEP = 256) before the next wake.
    let final_commits = initial.saturating_add(600);
    let mut sibling = open(&path).expect("sibling open");
    for n in initial..final_commits {
        let mut batch = put(n);
        batch.idempotency_key = None;
        sibling.commit(&batch).expect("sibling commit");
    }
    drop(sibling);

    let status = scrub_measuring_total_wake_rows(&store, &armed, &wake_rows);
    assert_eq!(status, ScrubStatus::Idle);
    assert!(
        max_wake_rows.get() <= 512,
        "scrub restart exceeded 512-row total per-wake bound: saw {} total rows in one wake",
        max_wake_rows.get()
    );
    assert!(history_ready(&store));
    drop(store);
    assert_eq!(
        checkpoint(&path).map(|(commits, ..)| commits),
        Some(final_commits),
        "checkpoint advanced to the sibling-extended head"
    );
}

/// S10 (#1551 round 10): a > 256-commit quantity sibling suffix stays strictly
/// within 512 TOTAL SQL rows summed across all queries in every scrub wake.
#[test]
fn quantity_scrub_after_large_sibling_suffix_stays_within_512_total_rows_per_wake() {
    let dir = tempfile::tempdir().expect("directory");
    let path = dir.path().join("cell.sqlite");
    quantity_cell(&path);

    let armed = std::rc::Rc::new(Cell::new(false));
    let wake_rows = std::rc::Rc::new(Cell::new(0));
    let max_wake_rows = std::rc::Rc::new(Cell::new(0));
    let store = CellStore::from_engine(
        RowBoundedEngine {
            inner: NativeEngine::open(&path, keyed()).expect("native engine"),
            armed: std::rc::Rc::clone(&armed),
            wake_rows: std::rc::Rc::clone(&wake_rows),
            max_wake_rows: std::rc::Rc::clone(&max_wake_rows),
        },
        engine_fixture::model(),
    )
    .expect("checkpointed open");
    assert!(matches!(store.scrub_step(), ScrubStatus::Verifying { .. }));

    // 270 quantity commits (> 256) would exceed 512 total rows if selector + receipts
    // both used LIMIT 512 in a single wake. Advance the sibling past Phase::Receipts
    // so idempotent quantity commits are admitted while keeping history_ready() false
    // (so the sibling never advances _norn_open_checkpoint).
    let added = 270_u64;
    let final_commits = EVERY.saturating_add(added);
    let mut sibling = open(&path).expect("sibling open");
    while matches!(
        sibling.scrub_step(),
        ScrubStatus::Verifying {
            phase: "receipts",
            ..
        }
    ) {}
    assert!(!history_ready(&sibling));
    for n in EVERY..final_commits {
        commit_quantity(&mut sibling, n, &post(0, &format!("sib-{n}"), "fund", 1));
    }
    drop(sibling);

    let status = scrub_measuring_total_wake_rows(&store, &armed, &wake_rows);
    assert_eq!(status, ScrubStatus::Idle);
    assert!(
        max_wake_rows.get() <= 512,
        "quantity scrub exceeded 512-row total per-wake bound: saw {} total rows in one wake",
        max_wake_rows.get()
    );
    assert!(history_ready(&store));
    drop(store);
    assert_eq!(
        checkpoint(&path).map(|(commits, ..)| commits),
        Some(final_commits),
        "checkpoint advanced to the quantity sibling-extended head"
    );
}

fn multi_row_batch(n: u64, width: u64, logical_time: u64) -> CommitBatch {
    let writes = (0..width)
        .map(|slot| FactWriteRow::Put {
            object_type: "fixture.Item".to_owned(),
            key: format!("m-{n}-{slot}"),
            values: BTreeMap::from([(
                "qty".to_owned(),
                RuntimeValue::from(i64::try_from(slot).expect("qty")),
            )]),
        })
        .collect();
    let events = (0..width)
        .map(|slot| {
            runtime_object! {
                "type" => "fixture.multi",
                "n" => i64::try_from(n).expect("n"),
                "slot" => i64::try_from(slot).expect("slot")
            }
        })
        .collect();
    CommitBatch {
        world: MAIN_WORLD.to_owned(),
        scope: engine_fixture::SCOPE.to_owned(),
        logical_time,
        idempotency_key: Some(format!("multi-{n}")),
        writes,
        events,
        ..CommitBatch::default()
    }
}

fn put_at(n: u64, logical_time: u64) -> CommitBatch {
    CommitBatch {
        logical_time,
        ..put(n)
    }
}

/// S10 (#1551 round 11/12): multi-row commits (both 64+ commits x 10 events/writes
/// AND a single jumbo commit with 300 events + 300 writes = 600 rows > 512)
/// resume cleanly across wakes while staying strictly <= 512 total SQL rows per wake.
#[test]
fn multi_row_commits_history_scrub_stays_within_512_total_rows_per_wake() {
    let dir = tempfile::tempdir().expect("directory");
    let path = dir.path().join("multi.sqlite");
    engine_fixture::write_cell(&path);
    let mut seed = open(&path).expect("open");
    // Monotonic logical_time: 10_000+n (64 multi-row) < 20_000 (jumbo) < 30_000+n (tail).
    let jumbo_n = engine_fixture::COMMITS.saturating_add(64);
    for n in engine_fixture::COMMITS..jumbo_n {
        seed.commit(&multi_row_batch(n, 10, 10_000_u64.saturating_add(n)))
            .expect("multi commit");
    }
    seed.commit(&multi_row_batch(jumbo_n, 300, 20_000))
        .expect("jumbo commit");
    let total_commits = EVERY.saturating_add(4);
    for n in jumbo_n.saturating_add(1)..total_commits {
        seed.commit(&put_at(n, 30_000_u64.saturating_add(n)))
            .expect("tail put");
    }
    drop(seed);
    assert_eq!(checkpoint(&path).map(|(c, ..)| c), Some(EVERY));

    // Clean checkpointed open: every scrub wake must read <= 512 total SQL rows.
    let armed = std::rc::Rc::new(Cell::new(false));
    let wake_rows = std::rc::Rc::new(Cell::new(0));
    let max_wake_rows = std::rc::Rc::new(Cell::new(0));
    let store = CellStore::from_engine(
        RowBoundedEngine {
            inner: NativeEngine::open(&path, keyed()).expect("native engine"),
            armed: std::rc::Rc::clone(&armed),
            wake_rows: std::rc::Rc::clone(&wake_rows),
            max_wake_rows: std::rc::Rc::clone(&max_wake_rows),
        },
        engine_fixture::model(),
    )
    .expect("checkpointed open");
    assert!(!history_ready(&store));
    let status = scrub_measuring_total_wake_rows(&store, &armed, &wake_rows);
    assert_eq!(status, ScrubStatus::Idle);
    assert!(
        max_wake_rows.get() <= 512,
        "multi-row history scrub exceeded 512 total rows in one wake: saw {}",
        max_wake_rows.get()
    );
    assert!(history_ready(&store));
    drop(store);

    // Tamper one late snapshot (slot 295, verified on a resumed wake) of the jumbo commit.
    let tampered = dir.path().join("multi-tampered.sqlite");
    std::fs::copy(&path, &tampered).expect("copy");
    execute(
        &tampered,
        &format!("DELETE FROM _norn_fact_values WHERE id = 'm-{jumbo_n}-295'"),
    );
    let tampered_store = open(&tampered).expect("checkpointed open");
    assert!(
        matches!(tampered_store.scrub_to_end(), ScrubStatus::Fenced { .. }),
        "resumed multi-wake snapshot verification must catch late corrupt row in jumbo commit"
    );
}

struct SharedEngine(std::rc::Rc<NativeEngine>);

impl SqlEngine for SharedEngine {
    fn read_isolation(&self) -> ReadIsolation {
        self.0.read_isolation()
    }
    fn checkpoint_key(&self) -> Option<CheckpointKey> {
        self.0.checkpoint_key()
    }
    fn external_data_version(&self) -> Result<Option<u64>, CellError> {
        self.0.external_data_version()
    }
    fn definition_writes<T>(
        &self,
        f: impl FnOnce() -> Result<T, CellError>,
    ) -> Result<T, CellError> {
        self.0.definition_writes(f)
    }
    fn execute(&self, sql: &str, params: &[SqlValue]) -> Result<(), CellError> {
        self.0.execute(sql, params)
    }
    fn query(&self, sql: &str, params: &[SqlValue]) -> Result<Vec<Vec<SqlValue>>, CellError> {
        self.0.query(sql, params)
    }
    fn execute_batch(&self, sql: &str) -> Result<(), CellError> {
        self.0.execute_batch(sql)
    }
    fn transaction<T, X: From<CellError>>(&self, f: impl FnOnce() -> Result<T, X>) -> Result<T, X> {
        self.0.transaction(f)
    }
}

fn step_into_quantity_live<E: SqlEngine>(store: &CellStore<E>, tail_wakes: usize) {
    loop {
        let status = store.scrub_step();
        if matches!(
            status,
            ScrubStatus::Verifying {
                phase: "history",
                verified,
                target,
                ..
            } if verified == target
        ) {
            break;
        }
        assert!(matches!(status, ScrubStatus::Verifying { .. }));
    }
    for _ in 0..tail_wakes {
        let status = store.scrub_step();
        assert!(
            matches!(status, ScrubStatus::Verifying { .. }),
            "expected QuantityTail live stage to keep verifying, got {status:?}"
        );
    }
}

fn insert_rogue_quantity_history(engine: &NativeEngine, commit_seq: u64) {
    let seq = i64::try_from(commit_seq).expect("commit_seq");
    engine
        .execute(
            r#"INSERT INTO "_norn_quantity_history"
               ("world","ledger","account","unit","layer","posted","held","version","commit_seq")
               VALUES ('main','rogue','acct','USD','settled','1','0',1,?1)"#,
            &[seq.into()],
        )
        .expect("insert rogue quantity history");
}

/// S10 (#1551 round 10): ordinary-only commits arriving during `Live` stage
/// re-enter suffix verification so rogue append rows at that ordinary commit
/// fence the cell, and the final live stage repeats beyond-tail probes.
#[test]
fn ordinary_commit_during_quantity_live_stage_verifies_append_and_beyond_tail() {
    // 1. Clean ordinary commit arriving mid-Live completes cleanly.
    let dir = tempfile::tempdir().expect("directory");
    let clean = dir.path().join("clean.sqlite");
    quantity_cell(&clean);
    let mut clean_store = open(&clean).expect("open clean");
    // Wake 1 enters Live::Timers; wake 2 verifies Timers -> now at Live::Ledgers.
    step_into_quantity_live(&clean_store, 2);
    clean_store
        .commit(&put(EVERY.saturating_add(1)))
        .expect("ordinary commit during Live");
    assert_eq!(clean_store.scrub_to_end(), ScrubStatus::Idle);
    assert!(history_ready(&clean_store));

    // 2. Ordinary commit arriving mid-Live with a rogue `_norn_quantity_history`
    //    row on the SAME SQLite connection (no `data_version` restart) must fence.
    let rogue_ordinary = dir.path().join("rogue-ordinary.sqlite");
    quantity_cell(&rogue_ordinary);
    let conn = std::rc::Rc::new(NativeEngine::open(&rogue_ordinary, keyed()).expect("open"));
    let mut store = CellStore::from_engine(
        SharedEngine(std::rc::Rc::clone(&conn)),
        engine_fixture::model(),
    )
    .expect("from_engine");
    step_into_quantity_live(&store, 2);
    let ordinary_seq = EVERY.saturating_add(1);
    store
        .commit(&put(ordinary_seq))
        .expect("ordinary commit during Live");
    insert_rogue_quantity_history(&conn, ordinary_seq);
    assert!(
        matches!(store.scrub_to_end(), ScrubStatus::Fenced { .. }),
        "rogue quantity history row at ordinary commit arriving during Live stage must fence"
    );

    // 3. Beyond-tail rogue row injected right before final `Reservations` live
    //    stage on the SAME SQLite connection must be caught by the final probe.
    let rogue_beyond = dir.path().join("rogue-beyond.sqlite");
    quantity_cell(&rogue_beyond);
    let conn2 = std::rc::Rc::new(NativeEngine::open(&rogue_beyond, keyed()).expect("open"));
    let store2 = CellStore::from_engine(
        SharedEngine(std::rc::Rc::clone(&conn2)),
        engine_fixture::model(),
    )
    .expect("from_engine");
    // Wakes 1..=4: finish_suffix -> Timers -> Ledgers -> Balances -> sitting at Reservations.
    step_into_quantity_live(&store2, 4);
    insert_rogue_quantity_history(&conn2, EVERY.saturating_add(5));
    assert!(
        matches!(store2.scrub_to_end(), ScrubStatus::Fenced { .. }),
        "beyond-tail rogue quantity history row before final Live stage must fence"
    );
}

/// S10 (#1551 round 9): committing valid same-handle quantity postings between
/// `Phase::QuantityTail` suffix wakes must not falsely fence the cell.
#[test]
fn same_handle_quantity_commit_during_quantity_tail_completes_without_fencing() {
    let dir = tempfile::tempdir().expect("directory");
    let path = dir.path().join("cell.sqlite");
    quantity_cell(&path);

    let mut store = open(&path).expect("checkpointed open");
    assert!(!history_ready(&store));
    // Step through Receipts and all History chunks until entering QuantityTail.
    loop {
        let status = store.scrub_step();
        if matches!(
            status,
            ScrubStatus::Verifying {
                phase: "history",
                verified,
                target,
                ..
            } if verified == target
        ) {
            break;
        }
        assert!(matches!(status, ScrubStatus::Verifying { .. }));
    }

    // First valid same-handle quantity commit right as QuantityTail starts.
    commit_quantity(
        &mut store,
        EVERY.saturating_add(1),
        &post(0, "tail-1", "fund", 5),
    );
    let mid = store.scrub_step();
    assert!(
        matches!(mid, ScrubStatus::Verifying { .. }),
        "first QuantityTail wake should step suffix receipts cleanly, got {mid:?}"
    );

    // Second valid same-handle quantity commit between suffix/live wakes.
    commit_quantity(
        &mut store,
        EVERY.saturating_add(2),
        &post(0, "tail-2", "fund", 7),
    );
    assert_eq!(
        store.scrub_to_end(),
        ScrubStatus::Idle,
        "valid same-handle quantity commits between QuantityTail wakes must not falsely fence"
    );
    assert!(history_ready(&store));
}
