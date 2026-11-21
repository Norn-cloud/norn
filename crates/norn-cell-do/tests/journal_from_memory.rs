//! INGEST-JOURNAL-MEMORY-1: a commit's export journal record is built from
//! the entries its writer appended, not from the ledger read back. On a real
//! record import, every journaled record must be byte-for-byte the record the
//! former path (the ledger span read back and proven) yields.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    reason = "equivalence test"
)]
use norn_api::host::DevHost;
use norn_cell::CellStore;
use norn_cell::engine::NativeEngine;
use norn_cell_do::seed;
use serde_json::{Value, json};

/// The `ERPNext` synthetic export with `lines` distinct sales-order lines.
fn export(lines: usize) -> Value {
    let export: Value = serde_json::from_str(include_str!(
        "../../../registry/packs/erpnext/synthetic.json"
    ))
    .unwrap();
    let rows = export["snapshot"].as_array().unwrap();
    let mut snapshot: Vec<Value> = rows
        .iter()
        .filter(|row| row["dataset"] != "Sales Order Item")
        .cloned()
        .collect();
    let order = rows
        .iter()
        .find(|row| row["dataset"] == "Sales Order Item")
        .unwrap();
    for n in 1..=lines {
        let mut line = order.clone();
        line["record"]["name"] = json!(format!("SYN-ERPNEXT-order-{n:06}"));
        line["record"]["custom_norn_ref"] = json!(format!("demo/order/{n:06}/line/1"));
        snapshot.push(line);
    }
    json!({ "profile": export["profile"], "snapshot": snapshot })
}

#[test]
fn journal_records_of_an_import_equal_the_ledger_read_back() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("cell.sqlite");
    let scope = seed::demo_scope();
    let engine = NativeEngine::open(&path, norn_cell::StoreOptions::default()).unwrap();
    let store = seed::provision(engine, &scope).unwrap();
    let model = store.model().clone();
    let host = DevHost::from_store(store, scope, |command| command.payload.clone()).unwrap();
    let recorded = norn_cell_do::recorded::Recorded::parse(&export(40)).unwrap();
    let rows = recorded.observations().unwrap();
    // The load, then the same load again (every row already landed).
    let first = host.ingest_within(&rows, None).unwrap();
    assert!(
        first
            .outcomes
            .iter()
            .any(|row| row.outcome == norn_cell::MappingOutcome::Landed)
    );
    host.ingest_within(&rows, None).unwrap();
    drop(host);

    let engine = NativeEngine::open(&path, norn_cell::StoreOptions::default()).unwrap();
    let store = CellStore::from_engine(engine, model).unwrap();
    let commits = store.export_commit_count().unwrap();
    assert!(commits > 2, "the load journals its commits ({commits})");
    let mut events = 0;
    for commit_seq in 1..=commits {
        // The exporter's read: hash-verified against the stored row.
        let journaled = store.export_record(commit_seq).unwrap().unwrap();
        // The former path: the commit's ledger span read back, every event
        // proven.
        let read_back: Vec<_> = store
            .read()
            .ledger(journaled.receipt.body.first_event_seq)
            .unwrap()
            .into_iter()
            .filter(|entry| entry.commit_seq == commit_seq)
            .collect();
        events += read_back.len();
        let former = norn_cell::export::ExportRecord {
            entries: read_back,
            ..journaled.clone()
        };
        assert_eq!(
            former.bytes().unwrap(),
            journaled.bytes().unwrap(),
            "commit {commit_seq}: journal bytes differ from the ledger read back"
        );
        assert_eq!(former.hash().unwrap(), journaled.hash().unwrap());
    }
    assert!(events > 40, "the load's events are journaled ({events})");
}
