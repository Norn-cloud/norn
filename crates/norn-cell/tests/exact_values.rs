#![allow(
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    missing_docs,
    reason = "integration-test crate"
)]

//! #2770 (T3-NUM-1): the store keeps runtime values exact. A decimal column
//! stores the canonical decimal text and reads back the exact number; JSON
//! payloads (ledger events, commands, timers) are parsed exactly; and a file
//! of the previous store format is refused with a clear error, not migrated.

mod common;

use std::collections::BTreeMap;

use norn_cell::{CellError, CellStore, CommitBatch, MAIN_WORLD, StoreOptions};
use norn_ir::{FactChange, RuntimeValue};

/// A ledger event with numbers beyond `f64`.
const EVENT: &str =
    r#"{"type":"priced","amount":9007199254740993,"rate":0.1000000000000000000000000001}"#;

fn exact(text: &str) -> RuntimeValue {
    RuntimeValue::from_json_text(text).expect("exact JSON")
}

/// A sales order line whose `unit_price` is the token `UNIT_PRICE`.
const LINE: &str = r#"{"op":"create","objectType":"commerce.SalesOrderLine","key":"SO-1/1","values":{"order_number":"SO-1","line_number":1,"unit_price":UNIT_PRICE,"currency":"EGP"}}"#;

fn line(unit_price: &str) -> FactChange {
    serde_json::from_str(&LINE.replace("UNIT_PRICE", unit_price)).expect("decodes exactly")
}

#[test]
fn decimals_and_payloads_round_trip_exactly() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("cell.sqlite");
    let mut store =
        CellStore::open(&path, common::corpus_model(), StoreOptions::default()).expect("opens");
    // Beyond f64: 20 integer digits and 9 fractional digits.
    let price = "12345678901234567890.123456789";
    let event = exact(EVENT);
    store
        .commit(&CommitBatch {
            world: MAIN_WORLD.to_owned(),
            scope: "org_demo/dev".to_owned(),
            facts: vec![line(price)],
            events: vec![event.clone()],
            ..CommitBatch::default()
        })
        .expect("commits");
    let check = |store: &CellStore| {
        let record = store
            .read()
            .object(MAIN_WORLD, "commerce.SalesOrderLine", "SO-1/1")
            .expect("reads")
            .expect("exists");
        assert_eq!(record.values["unit_price"], exact(price));
        assert_eq!(record.values["line_number"], exact("1"));
        let ledger = store.read().ledger(1).expect("ledger");
        assert_eq!(ledger[0].event, event);
        store
            .read()
            .verify_ledger()
            .expect("the hash chain verifies");
    };
    check(&store);
    drop(store);
    let reopened =
        CellStore::open(&path, common::corpus_model(), StoreOptions::default()).expect("reopens");
    check(&reopened);
}

#[test]
fn a_decimal_beyond_the_engine_range_is_refused_never_rounded() {
    let mut store = CellStore::open_in_memory(common::corpus_model()).expect("opens");
    let mut values = BTreeMap::new();
    values.insert("order_number".to_owned(), RuntimeValue::from("SO-1"));
    values.insert("line_number".to_owned(), exact("1"));
    values.insert("currency".to_owned(), RuntimeValue::from("EGP"));
    // 30 significant digits: not an exact 96-bit decimal.
    values.insert(
        "unit_price".to_owned(),
        RuntimeValue::from("123456789012345678901234567890.5"),
    );
    let fact = FactChange::Create {
        object_type: norn_ir::ObjectTypeName("commerce.SalesOrderLine".to_owned()),
        key: "SO-1/1".to_owned(),
        values,
    };
    let refused = store.commit(&CommitBatch {
        world: MAIN_WORLD.to_owned(),
        scope: "org_demo/dev".to_owned(),
        facts: vec![fact],
        ..CommitBatch::default()
    });
    assert!(
        matches!(refused, Err(CellError::ValueType { .. })),
        "{refused:?}"
    );
}

#[test]
fn a_store_of_the_previous_format_is_refused_with_a_clear_error() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("cell.sqlite");
    drop(CellStore::open(&path, common::corpus_model(), StoreOptions::default()).expect("opens"));
    rusqlite::Connection::open(&path)
        .expect("raw open")
        .execute(
            "UPDATE \"_norn_meta\" SET \"value\" = 'norn.cell.store/1' WHERE \"key\" = 'format'",
            [],
        )
        .expect("downgrades the format");
    let reopened = CellStore::open(&path, common::corpus_model(), StoreOptions::default());
    match reopened {
        Err(error @ CellError::StoreFormat { .. }) => {
            assert!(
                error
                    .to_string()
                    .contains("unsupported pre-alpha store version"),
                "{error}"
            );
        }
        other => panic!("expected StoreFormat, got {other:?}"),
    }
}

#[test]
fn old_pending_link_layout_is_refused_before_open_or_definition_reads() {
    use norn_cell::FactWriteRow;
    use norn_cell::engine::NativeEngine;
    let ty = serde_json::from_value(serde_json::json!({
        "name": "norn.PendingLinks",
        "properties": [
            {"id":"objectType","type":{"kind":"text"}},
            {"id":"key","type":{"kind":"text"}},
            {"id":"links","type":{"kind":"json-ext"}}
        ]
    }))
    .expect("pending store type");
    let model = common::model_of(&[ty]);
    for old_format in [
        "norn.cell.store/2",
        "norn.cell.store/3",
        "norn.cell.store/4",
        "norn.cell.store/5",
        "norn.cell.store/6",
    ] {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("old-pending.sqlite");
        let mut store =
            CellStore::open(&path, model.clone(), StoreOptions::default()).expect("fixture store");
        // The actual pre-rework shape: status and required are both absent.
        let links = exact(
            r#"[{"sourceType":"test.Source","sourceKey":"source","link":"owner","targetType":"test.Target","targetKey":"missing","at":"2026-10-01T00:00:00Z","source":"source:erpnext/Source/source","mappingVersion":"test.mapping@1","provenance":{"source":{"system":"erpnext","record":"Source/source"},"mappingVersion":"test.mapping@1","recordedAt":"2026-10-01T00:00:00Z"}}]"#,
        );
        store
            .commit(&CommitBatch {
                world: MAIN_WORLD.to_owned(),
                scope: "org_demo/dev".to_owned(),
                writes: vec![FactWriteRow::Put {
                    object_type: "norn.PendingLinks".to_owned(),
                    key: "test.Target/missing".to_owned(),
                    values: BTreeMap::from([
                        ("objectType".to_owned(), "test.Target".into()),
                        ("key".to_owned(), "missing".into()),
                        ("links".to_owned(), links),
                    ]),
                }],
                ..CommitBatch::default()
            })
            .expect("retained old-layout fixture");
        drop(store);
        let raw = rusqlite::Connection::open(&path).expect("fixture connection");
        raw.execute(
            "UPDATE _norn_meta SET value = ?1 WHERE key = 'format'",
            [old_format],
        )
        .expect("old format");
        // Remove the index to detect attempted DDL before format refusal.
        raw.execute_batch("DROP TABLE _norn_pending_link")
            .expect("old schema");
        drop(raw);
        let engine = NativeEngine::open(&path, StoreOptions::default()).expect("engine");
        let error = CellStore::<NativeEngine>::read_active_definitions(&engine)
            .expect_err("definition bootstrap refuses old cells");
        assert!(matches!(error, CellError::StoreFormat { .. }));
        assert!(
            error
                .to_string()
                .contains("unsupported pre-alpha store version")
        );
        assert!(error.to_string().contains("recreate the cell"));
        let Err(error) = CellStore::open(&path, model.clone(), StoreOptions::default()) else {
            panic!("old layout must refuse");
        };
        assert!(matches!(error, CellError::StoreFormat { .. }));
        let raw = rusqlite::Connection::open(&path).expect("unchanged fixture");
        let exists: i64 = raw
            .query_row(
                "SELECT count(*) FROM sqlite_master WHERE name = '_norn_pending_link'",
                [],
                |row| row.get(0),
            )
            .expect("index check");
        assert_eq!(exists, 0, "no DDL or migration runs before format refusal");
        let format: String = raw
            .query_row(
                "SELECT value FROM _norn_meta WHERE key = 'format'",
                [],
                |row| row.get(0),
            )
            .expect("format");
        assert_eq!(format, old_format);
    }
}
