//! A committed cell at an older storage version (ENGINE-UPGRADE-1, #1494):
//! the scenario that writes it, and a lossless JSON image of its SQLite file.
//!
//! Only APIs that every storage version has are used here, so the recorder
//! (`tests/engine_fixture_record.rs`) builds against the release whose
//! storage version it records; on a release before the test door (#1485 S6)
//! drop the `TestCommit` import, as `CellStore::commit` was public there. The image holds the catalog in creation
//! order and every table's rows by rowid, so loading it rebuilds the same
//! logical database.

#![allow(
    clippy::arithmetic_side_effects,
    clippy::expect_used,
    clippy::format_collect,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::string_slice,
    clippy::wildcard_enum_match_arm,
    dead_code,
    unreachable_pub,
    reason = "test support shared by the upgrade test and its recorder"
)]

use std::collections::BTreeMap;
use std::path::Path;

use norn_cell::cell_identity::CellId;
use norn_cell::engine::{NativeEngine, SqlEngine, SqlValue};
use norn_cell::test_door::TestCommit as _;
use norn_cell::{CellStore, CommitBatch, FactWriteRow, MAIN_WORLD, Model, StoreOptions};
use norn_ir::{ObjectTypeDef, RuntimeValue, runtime_object};
use serde_json::{Value, json};

/// Encoding of the image.
pub const IMAGE_FORMAT: &str = "norn.cell.sqlite-image/1";
/// The scope the fixture cell is bound to.
pub const SCOPE: &str = "fixture/dev";
const OBJECT_TYPE: &str = "fixture.Item";
const CELL_ID: &str = "0f1e2d3c4b5a69788796a5b4c3d2e1f0";

/// The fixture's one object type: an integer with an invariant, a text with
/// a default.
pub fn object_type() -> ObjectTypeDef {
    serde_json::from_value(json!({
        "name": OBJECT_TYPE,
        "properties": [
            {"id": "qty", "type": {"kind": "integer"}},
            {"id": "label", "type": {"kind": "text"}, "default": "item"}
        ],
        "invariants": [
            {"id": "non_negative", "expression": "self.qty >= 0", "message": "qty is not negative"}
        ]
    }))
    .expect("object type")
}

/// The model the fixture cell was written under.
pub fn model() -> Model {
    Model::from_definitions(&[object_type()], &[]).expect("model")
}

fn put(key: &str, qty: i64) -> FactWriteRow {
    FactWriteRow::Put {
        object_type: OBJECT_TYPE.to_owned(),
        key: key.to_owned(),
        values: BTreeMap::from([("qty".to_owned(), RuntimeValue::from(qty))]),
    }
}

/// The recorded commits, in order: creates with a ledger event, an update,
/// a delete. Each carries an idempotency key, so replaying one after an
/// upgrade must return its recorded receipt.
pub fn batches() -> Vec<CommitBatch> {
    let batch = |time: u64, key: &str, writes: Vec<FactWriteRow>, events| CommitBatch {
        world: MAIN_WORLD.to_owned(),
        scope: SCOPE.to_owned(),
        logical_time: time,
        idempotency_key: Some(key.to_owned()),
        writes,
        events,
        ..CommitBatch::default()
    };
    vec![
        batch(
            1_000,
            "create",
            (0..5).map(|i| put(&format!("a{i}"), i)).collect(),
            vec![runtime_object! { "type" => "fixture.created", "count" => 5_i64 }],
        ),
        batch(2_000, "update", vec![put("a1", 10), put("b0", 7)], vec![]),
        batch(
            3_000,
            "delete",
            vec![FactWriteRow::Delete {
                object_type: OBJECT_TYPE.to_owned(),
                key: "a4".to_owned(),
            }],
            vec![runtime_object! { "type" => "fixture.deleted", "key" => "a4" }],
        ),
    ]
}

/// Write the fixture cell at `path` with whatever engine this build is.
pub fn write_cell(path: &Path) {
    let engine = NativeEngine::open(path, StoreOptions::default()).expect("engine");
    let mut store = CellStore::from_engine_with_identity_source(engine, model(), |_| {
        Ok(CellId::try_from(CELL_ID.to_owned()).expect("identity"))
    })
    .expect("new cell");
    store.bind("scope", SCOPE).expect("scope");
    for batch in batches() {
        store.commit(&batch).expect("recorded commit");
    }
}

fn encode(value: &SqlValue) -> Value {
    match value {
        SqlValue::Null => Value::Null,
        SqlValue::Integer(integer) => json!(integer),
        SqlValue::Real(real) => json!({ "real": real }),
        SqlValue::Text(text) => json!(text),
        SqlValue::Blob(bytes) => {
            json!({ "blob": bytes.iter().map(|byte| format!("{byte:02x}")).collect::<String>() })
        }
    }
}

fn decode(value: &Value) -> SqlValue {
    match value {
        Value::Null => SqlValue::Null,
        Value::Number(number) => SqlValue::Integer(number.as_i64().expect("integer")),
        Value::String(text) => SqlValue::Text(text.clone()),
        Value::Object(tagged) => {
            if let Some(real) = tagged.get("real") {
                SqlValue::Real(real.as_f64().expect("real"))
            } else {
                let hex = tagged["blob"].as_str().expect("blob");
                SqlValue::Blob(
                    (0..hex.len())
                        .step_by(2)
                        .map(|at| u8::from_str_radix(&hex[at..at + 2], 16).expect("hex"))
                        .collect(),
                )
            }
        }
        Value::Bool(_) | Value::Array(_) => panic!("not an image value: {value}"),
    }
}

fn quote(name: &str) -> String {
    format!("\"{}\"", name.replace('"', "\"\""))
}

fn text(value: &SqlValue) -> String {
    match value {
        SqlValue::Text(text) => text.clone(),
        other => panic!("expected text, found {other:?}"),
    }
}

/// The logical contents of `engine`'s database.
pub fn image(engine: &NativeEngine) -> Value {
    let catalog = engine
        .query(
            "SELECT type, name, tbl_name, sql FROM sqlite_master \
             WHERE sql IS NOT NULL ORDER BY rowid",
            &[],
        )
        .expect("catalog");
    let mut schema = Vec::new();
    let mut tables = serde_json::Map::new();
    for entry in catalog {
        let [kind, name, table, sql] = <[SqlValue; 4]>::try_from(entry).expect("catalog row");
        let (kind, name, table, sql) = (text(&kind), text(&name), text(&table), text(&sql));
        assert!(
            !name.starts_with("sqlite_"),
            "{name}: SQLite-internal tables are not imaged"
        );
        if kind == "table" {
            let mut columns = vec!["rowid".to_owned()];
            columns.extend(
                engine
                    .query(
                        "SELECT name FROM pragma_table_info(?1) ORDER BY cid",
                        &[SqlValue::from(name.as_str())],
                    )
                    .expect("columns")
                    .iter()
                    .map(|row| text(&row[0])),
            );
            let rows: Vec<Value> = engine
                .query(
                    &format!("SELECT rowid, * FROM {} ORDER BY rowid", quote(&name)),
                    &[],
                )
                .expect("rows")
                .iter()
                .map(|row| Value::Array(row.iter().map(encode).collect()))
                .collect();
            tables.insert(name.clone(), json!({ "columns": columns, "rows": rows }));
        }
        schema.push(json!({ "type": kind, "name": name, "table": table, "sql": sql }));
    }
    let storage = engine
        .query("SELECT value FROM \"_norn_meta\" WHERE key = 'engine'", &[])
        .ok()
        .and_then(|rows| rows.into_iter().next())
        // The stamp's storage version as written, or null for a stamp that
        // is not JSON; an unstamped store is storage version 1.
        .map_or(Value::from(1), |row| {
            serde_json::from_str::<Value>(&text(&row[0]))
                .map_or(Value::Null, |stamp| stamp["storage"].clone())
        });
    json!({ "format": IMAGE_FORMAT, "storage": storage, "schema": schema, "tables": tables })
}

/// Rebuild `image` into a new SQLite file at `path`.
pub fn load(image: &Value, path: &Path) {
    assert_eq!(image["format"], IMAGE_FORMAT, "image format");
    assert!(!path.exists(), "load writes a new file");
    let engine = NativeEngine::open(path, StoreOptions::default()).expect("engine");
    let schema = image["schema"].as_array().expect("schema");
    engine
        .transaction(|| {
            // Tables and indexes in catalog order, so the loaded catalog
            // lists them as the original does; views and triggers after the
            // rows, so no trigger fires on the load.
            let deferred =
                |entry: &Value| matches!(entry["type"].as_str(), Some("view" | "trigger"));
            for entry in schema.iter().filter(|entry| !deferred(entry)) {
                engine.execute_batch(entry["sql"].as_str().expect("sql"))?;
            }
            for entry in schema.iter().filter(|entry| entry["type"] == "table") {
                let name = entry["name"].as_str().expect("name");
                let table = &image["tables"][name];
                let columns: Vec<String> = table["columns"]
                    .as_array()
                    .expect("columns")
                    .iter()
                    .map(|column| quote(column.as_str().expect("column")))
                    .collect();
                let insert = format!(
                    "INSERT INTO {} ({}) VALUES ({})",
                    quote(name),
                    columns.join(", "),
                    (1..=columns.len())
                        .map(|at| format!("?{at}"))
                        .collect::<Vec<_>>()
                        .join(", ")
                );
                for row in table["rows"].as_array().expect("rows") {
                    let values: Vec<SqlValue> =
                        row.as_array().expect("row").iter().map(decode).collect();
                    engine.execute(&insert, &values)?;
                }
            }
            for entry in schema.iter().filter(|entry| deferred(entry)) {
                engine.execute_batch(entry["sql"].as_str().expect("sql"))?;
            }
            Ok::<_, norn_cell::CellError>(())
        })
        .expect("load image");
    engine.close().expect("close");
}

/// The image of the SQLite file at `path`, read through a fresh read-only
/// connection, so imaging a cell this engine refuses neither fails nor
/// writes.
pub fn image_of(path: &Path) -> Value {
    let engine = NativeEngine::open_read_only(path).expect("engine");
    let image = image(&engine);
    engine.close().expect("close");
    image
}
