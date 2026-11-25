//! EXPORT-RESIDUE-1 (#1533 Part 2): a steady-state main-world journal row
//! stores a residue that rebuilds its record byte for byte (the stored Norn1
//! hash is the full record's), every other row keeps the packed encoding,
//! any change to a row a residue is rebuilt from refuses, and budgeted reads
//! charge every row they rebuild from.
#![cfg(feature = "native")]
#![allow(
    clippy::arithmetic_side_effects,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    missing_docs,
    reason = "integration-test crate"
)]

use std::collections::BTreeMap;

use norn_cell::engine::SqlEngine as _;
use norn_cell::export::ExportRecord;
use norn_cell::test_door::TestCommit as _;
use norn_cell::{
    CellError, CellStore, CommitBatch, FactWriteRow, InstalledDefinitionInput, LinkChange,
    MAIN_WORLD, Model, StoreOptions,
};
use norn_ir::{ObjectTypeDef, RuntimeValue};
use serde_json::json;

const SCOPE: &str = "org_demo/dev";
const RESIDUE: &str = "norn.cell.export-residue/1:";
const PACKED: &str = "norn.cell.payload-storage/1:";

fn item_type() -> ObjectTypeDef {
    serde_json::from_value(json!({
        "name": "residue.Item",
        "properties": [
            {"id": "qty", "type": {"kind": "integer"}},
            {"id": "label", "type": {"kind": "text"}, "default": "item"}
        ],
        "links": [{
            "id": "related",
            "target": "residue.Item",
            "cardinality": { "to": "many", "from": "many" }
        }]
    }))
    .expect("object type")
}

fn model() -> Model {
    Model::from_definitions(&[item_type()], &[]).expect("model")
}

fn batch(time: u64, key: &str) -> CommitBatch {
    CommitBatch {
        world: MAIN_WORLD.to_owned(),
        scope: SCOPE.to_owned(),
        logical_time: time,
        idempotency_key: Some(key.to_owned()),
        ..CommitBatch::default()
    }
}

fn put(key: &str, qty: i64, label: &str) -> FactWriteRow {
    FactWriteRow::Put {
        object_type: "residue.Item".to_owned(),
        key: key.to_owned(),
        values: BTreeMap::from([
            ("qty".to_owned(), RuntimeValue::from(qty)),
            ("label".to_owned(), RuntimeValue::String(label.to_owned())),
        ]),
    }
}

fn link(source: &str, target: &str) -> LinkChange {
    LinkChange::Add {
        object_type: "residue.Item".to_owned(),
        link: "related".to_owned(),
        source: source.to_owned(),
        target: target.to_owned(),
        values: BTreeMap::default(),
        target_type: None,
    }
}

fn event(kind: &str) -> RuntimeValue {
    norn_ir::runtime_object! { "type" => kind }
}

/// A bound in-memory cell with three steady-state commits after its first:
/// a multi-fact create, links between them, and a large text value.
fn steady_store() -> CellStore {
    let mut store = CellStore::open_in_memory(model()).expect("store");
    store.bind("scope", SCOPE).expect("scope");
    let mut first = batch(100, "first");
    first.events.push(event("residue.first"));
    store.commit(&first).expect("first commit");
    let mut facts = batch(200, "facts");
    facts.writes = vec![put("a", 1, "one"), put("b", 2, "two"), put("c", 3, "three")];
    facts.events.push(event("residue.created"));
    store.commit(&facts).expect("multi-fact commit");
    let mut links = batch(300, "links");
    links.links = vec![link("a", "b"), link("a", "c"), link("b", "c")];
    store.commit(&links).expect("link commit");
    let mut large = batch(400, "large");
    large.writes = vec![put("a", 4, &"large-label ".repeat(2_000))];
    store.commit(&large).expect("large commit");
    store
}

fn journal_row(store: &CellStore, seq: u64) -> (String, String) {
    store
        .conn
        .query_row(
            r#"SELECT "hash","record" FROM "_norn_export_journal" WHERE "commit_seq"=?1"#,
            &[norn_cell::engine::SqlValue::Integer(
                i64::try_from(seq).expect("seq"),
            )],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .expect("journal row")
}

fn count(store: &CellStore, table: &str) -> i64 {
    store
        .conn
        .query_row(&format!("SELECT COUNT(*) FROM \"{table}\""), &[], |row| {
            row.get(0)
        })
        .expect("count")
}

/// Read `seq`'s record and prove it is the record its commit hashed: the
/// stored hash is the Norn1 hash of the complete record's canonical bytes,
/// and its receipt and entries are the commit's own rows.
fn assert_exact(store: &CellStore, seq: u64, residue: bool) -> ExportRecord {
    let (hash, raw) = journal_row(store, seq);
    assert_eq!(raw.starts_with(RESIDUE), residue, "commit {seq}: {raw:.80}");
    assert_eq!(raw.starts_with(PACKED), !residue, "commit {seq}: {raw:.80}");
    let record = store
        .local_export_record(seq)
        .expect("record reads")
        .expect("record exists");
    let bytes = record.bytes().expect("canonical bytes");
    assert_eq!(
        norn_ir::canonical::HashProfile::Norn1.hash_canonical_bytes(&bytes),
        hash
    );
    assert_eq!(record.hash().expect("hash"), hash);
    let receipt = store.read().receipt_at(seq).expect("receipt").expect("row");
    assert_eq!(record.receipt, receipt);
    let entries: Vec<_> = store
        .read()
        .ledger(0)
        .expect("ledger")
        .into_iter()
        .filter(|entry| entry.commit_seq == seq)
        .collect();
    assert_eq!(record.entries, entries);
    if seq > 1 {
        let previous = store
            .local_export_record(seq.saturating_sub(1))
            .expect("previous reads")
            .expect("previous exists");
        assert_eq!(record.previous, previous.hash().expect("previous hash"));
    }
    record
}

#[test]
fn steady_main_commits_store_residues_that_rebuild_their_hashed_records() {
    let store = steady_store();
    let first = assert_exact(&store, 1, false);
    assert!(
        first.initial_model.is_some(),
        "the first commit keeps its model"
    );
    for seq in 2..=4 {
        let record = assert_exact(&store, seq, true);
        assert_eq!(record.ddl_sql, store.read().model().ddl_sql());
        assert_eq!(
            record.bindings.get("scope").map(String::as_str),
            Some(SCOPE)
        );
    }
    assert_eq!(assert_exact(&store, 3, true).batch.links.len(), 3);
    assert_eq!(count(&store, "_norn_export_context"), 1);
    // A residue references no stored body: the DDL is the context's.
    let referenced: i64 = store
        .conn
        .query_row(
            r#"SELECT COUNT(*) FROM "_norn_export_payload" WHERE "commit_seq" > 1"#,
            &[],
            |row| row.get(0),
        )
        .expect("references");
    assert_eq!(referenced, 0);
    store.read().verify_ledger().expect("ledger unchanged");
}

#[test]
fn a_batch_command_result_and_another_world_keep_the_packed_encoding() {
    let dir = tempfile::tempdir().expect("directory");
    let mut store = CellStore::open(
        &dir.path().join("cell.sqlite"),
        model(),
        StoreOptions::default(),
    )
    .expect("store");
    store.bind("scope", SCOPE).expect("scope");
    store.commit(&batch(100, "first")).expect("first");
    let mut result = batch(200, "result");
    result.events.push(norn_ir::runtime_object! {
        "type" => "command.result",
        "result" => norn_ir::runtime_object! { "body" => "r".repeat(4_000) },
    });
    store.commit(&result).expect("result event");
    assert_exact(&store, 2, false);
    let mut steady = batch(300, "steady");
    steady.writes = vec![put("a", 1, "one")];
    store.commit(&steady).expect("steady");
    assert_exact(&store, 3, true);

    let head = store.verified_head().expect("head");
    let mut fork = store.fork("what-if", &head).expect("fork");
    let mut what_if = batch(400, "what-if");
    what_if.world = "what-if".to_owned();
    what_if.writes = vec![put("a", 9, "nine")];
    fork.commit(&what_if).expect("fork commit");
    // The fork reads the inherited residue and writes its own world packed.
    assert_exact(&fork, 3, true);
    assert_exact(&fork, 4, false);
}

fn install_input() -> (Model, InstalledDefinitionInput) {
    let definition = json!({ "name": "install.Item", "properties": [{ "id": "code", "type": { "kind": "text" } }] });
    let typed: ObjectTypeDef = serde_json::from_value(definition.clone()).expect("typed");
    let installed = Model::from_definitions(&[typed], &[]).expect("model");
    let artifacts = json!([{
        "irVersion": "0.1", "artifactId": "install.Item.entity", "kind": "entity", "namespace": "install", "name": "Item", "version": "0.1.0", "lifecycle": "draft",
        "scope": { "orgId": "org_demo", "environmentId": "dev" }, "dependencies": [],
        "provenance": { "source": "authoring_sdk", "sourcePath": "test" },
        "payload": { "entityVersion": "2.0", "objectType": definition }
    }]);
    let canonical = RuntimeValue::from_json(artifacts)
        .expect("exact")
        .canonical_json()
        .expect("canonical");
    let input = InstalledDefinitionInput {
        canonical_set: String::from_utf8(canonical).expect("utf8"),
        registry: Vec::new(),
        receipt_sha256: "a".repeat(64),
        admission_receipt: None,
        readmission: None,
        installed_at: 250,
        diff: json!({ "added": [], "removed": [], "changed": [], "narrowing": [] }),
        layout: None,
    };
    (installed, input)
}

#[test]
fn records_before_and_after_an_install_rebuild_their_own_model() {
    let before = Model::from_definitions(&[], &[]).expect("empty model");
    let mut store = CellStore::open_in_memory(before.clone()).expect("store");
    store.bind("scope", SCOPE).expect("scope");
    let mut first = batch(100, "first");
    first.events.push(event("residue.first"));
    store.commit(&first).expect("first");
    let mut steady = batch(200, "before");
    steady.events.push(event("residue.before"));
    store.commit(&steady).expect("before install");
    let (after, input) = install_input();
    store
        .install_definitions(after.clone(), input, Some(0), SCOPE)
        .expect("install");
    let mut later = batch(300, "after");
    later.events.push(event("residue.after"));
    store.commit(&later).expect("after install");

    let historical = assert_exact(&store, 2, true);
    assert_eq!(historical.model_hash, before.model_hash());
    assert_eq!(historical.ddl_sql, before.ddl_sql());
    assert!(assert_exact(&store, 3, false).definition_install.is_some());
    let current = assert_exact(&store, 4, true);
    assert_eq!(current.model_hash, after.model_hash());
    assert_eq!(current.ddl_sql, after.ddl_sql());
    assert_eq!(count(&store, "_norn_export_context"), 2);
}

fn refuses(store: &CellStore, seq: u64, what: &str) {
    assert!(
        matches!(
            store.local_export_record(seq),
            Err(CellError::LedgerCorrupt { .. })
        ),
        "{what}: {:?}",
        store.local_export_record(seq)
    );
}

#[test]
fn a_changed_row_beneath_a_residue_refuses_its_record() {
    let tampers = [
        (
            "context DDL",
            r#"UPDATE "_norn_export_context" SET "ddl_sql" = "ddl_sql" || ' '"#,
        ),
        (
            "context bindings",
            r#"UPDATE "_norn_export_context" SET "bindings" = '{}'"#,
        ),
        ("missing context", r#"DELETE FROM "_norn_export_context""#),
        (
            "receipt hash",
            r#"UPDATE "_norn_receipt" SET "receipt_hash" = '00' WHERE "commit_seq" = 2"#,
        ),
        (
            "ledger event",
            r#"UPDATE "_norn_ledger" SET "event" = '{"type":"tampered"}' WHERE "commit_seq" = 2"#,
        ),
        (
            "ledger span",
            r#"DELETE FROM "_norn_ledger" WHERE "commit_seq" = 2"#,
        ),
        (
            "residue batch",
            r#"UPDATE "_norn_export_journal" SET "record" = replace("record", '"logicalTime":200', '"logicalTime":201') WHERE "commit_seq" = 2"#,
        ),
        (
            "residue of another commit",
            r#"UPDATE "_norn_export_journal" SET "record" = (SELECT "record" FROM "_norn_export_journal" WHERE "commit_seq" = 3) WHERE "commit_seq" = 2"#,
        ),
    ];
    for (what, tamper) in tampers {
        let store = steady_store();
        assert_exact(&store, 2, true);
        store.conn.execute(tamper, &[]).expect(what);
        refuses(&store, 2, what);
    }
}

#[test]
fn a_changed_context_row_refuses_the_next_commit_that_names_it() {
    let mut store = steady_store();
    store
        .conn
        .execute(
            r#"UPDATE "_norn_export_context" SET "model_hash" = 'tampered'"#,
            &[],
        )
        .expect("tamper");
    let mut next = batch(500, "next");
    next.events.push(event("residue.next"));
    assert!(matches!(
        store.commit(&next),
        Err(CellError::LedgerCorrupt { .. })
    ));
    assert_eq!(
        count(&store, "_norn_export_journal"),
        4,
        "nothing is written"
    );
}

#[test]
fn budgeted_reads_charge_every_row_a_residue_is_rebuilt_from() {
    use crate::payload_storage::ExportDecodeCursor;
    use crate::scrub::RowBudget;

    let store = steady_store();
    for seq in 1..=4 {
        let record = assert_exact(&store, seq, seq > 1);
        let raw = journal_row(&store, seq).1;
        let bound = norn_cell::export::record_decode_bytes(&store.conn, seq)
            .expect("bound")
            .expect("row");
        let canonical = u64::try_from(record.bytes().expect("bytes").len()).expect("length");
        assert!(bound >= canonical, "commit {seq}: {bound} < {canonical}");
        if seq > 1 {
            assert!(bound > u64::try_from(raw.len()).expect("length"));
        }
    }
    // The multi-fact commit's three entries take several four-row wakes.
    let expected = store.local_export_record(2).expect("reads").expect("row");
    let mut budget = RowBudget::new(4);
    let mut cursor = ExportDecodeCursor::open(&store.conn, 2, &mut budget)
        .expect("opens")
        .expect("row");
    let mut wakes = 1_u32;
    let record = loop {
        if let Some(record) = cursor.step(&store.conn, &mut budget).expect("step") {
            break record;
        }
        budget = RowBudget::new(4);
        wakes += 1;
        assert!(wakes < 64, "a residue read makes progress every wake");
    };
    assert_eq!(record, expected);
    assert!(wakes >= 3, "the rows were charged: {wakes} wakes");
    // An unbounded wake rebuilds it at once.
    let mut budget = RowBudget::new(usize::MAX);
    let mut cursor = ExportDecodeCursor::open(&store.conn, 2, &mut budget)
        .expect("opens")
        .expect("row");
    assert_eq!(
        cursor.step(&store.conn, &mut budget).expect("step"),
        Some(expected)
    );
    // A tampered rebuild refuses through the cursor too.
    store
        .conn
        .execute(
            r#"UPDATE "_norn_ledger" SET "event" = '{"type":"tampered"}' WHERE "commit_seq" = 2"#,
            &[],
        )
        .expect("tamper");
    let mut budget = RowBudget::new(usize::MAX);
    let mut cursor = ExportDecodeCursor::open(&store.conn, 2, &mut budget)
        .expect("opens")
        .expect("row");
    assert!(matches!(
        cursor.step(&store.conn, &mut budget),
        Err(CellError::LedgerCorrupt { .. })
    ));
}
