//! ENGINE-UPGRADE-1 (#1494) on Rivet's embedded SQLite: the storage-version
//! check lives in `norn-cell` below the host seam, so the Rivet host runs it
//! with no host code. This probe proves it on a real actor database.
//!
//! - The storage version 1 and 2 fixtures (`crates/norn-cell/tests/fixtures`,
//!   written by those engines) are loaded into a fresh actor's database
//!   through [`RivetEngine`]. Each opens through `CellStore`, upgrades in
//!   place to this engine's storage version, and keeps its ledger and receipts
//!   row for row. A reopen writes nothing, and the cell commits again.
//! - A cell stamped by a newer engine, or with a stamp this engine cannot
//!   read, refuses with the registered error and stays identical.

use std::fmt::Write as _;

use norn_cell::engine::{SqlEngine, SqlValue};
use norn_cell::engine_release::{
    self, Admission, EngineStamp, STAMP_FORMAT, STAMP_KEY, STORAGE_VERSION,
};
use norn_cell::{CellError, CellStore, Model};
use serde_json::{Value, json};
use sha2::{Digest as _, Sha256};

use crate::engine::RivetEngine;

const STORAGE_V1: &str = include_str!("../../../norn-cell/tests/fixtures/engine-storage-v1.json");
const STORAGE_V2: &str = include_str!("../../../norn-cell/tests/fixtures/engine-storage-v2.json");
const IMAGE_FORMAT: &str = "norn.cell.sqlite-image/1";

/// Which check the probe runs on its (fresh) actor database.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Case {
    /// Load the storage version 1 fixture and open it.
    UpgradeV1,
    /// Load the storage version 2 fixture and open it.
    UpgradeV2,
    /// Upgrade the v1 fixture, then stamp it as a newer engine would.
    Downgrade,
    /// Upgrade the v1 fixture, then give it a stamp this engine cannot read.
    UnknownStamp,
}

impl Case {
    /// The case a probe's `case` parameter names.
    #[must_use]
    pub fn parse(name: &str) -> Option<Self> {
        Some(match name {
            "v1" => Self::UpgradeV1,
            "v2" => Self::UpgradeV2,
            "downgrade" => Self::Downgrade,
            "unknown" => Self::UnknownStamp,
            _ => return None,
        })
    }
}

/// The model the fixture cells were written under
/// (`tests/support/engine_fixture.rs`).
fn model() -> Result<Model, CellError> {
    let object_type = serde_json::from_value(json!({
        "name": "fixture.Item",
        "properties": [
            {"id": "qty", "type": {"kind": "integer"}},
            {"id": "label", "type": {"kind": "text"}, "default": "item"}
        ],
        "invariants": [
            {"id": "non_negative", "expression": "self.qty >= 0", "message": "qty is not negative"}
        ]
    }))
    .map_err(|error| CellError::Engine(format!("fixture object type: {error}")))?;
    Model::from_definitions(&[object_type], &[])
        .map_err(|error| CellError::Engine(format!("fixture model: {error}")))
}

fn fail(message: impl Into<String>) -> CellError {
    CellError::Engine(message.into())
}

fn quote(name: &str) -> String {
    format!("\"{}\"", name.replace('"', "\"\""))
}

fn decode(value: &Value) -> Result<SqlValue, CellError> {
    Ok(match value {
        Value::Null => SqlValue::Null,
        Value::Number(number) => {
            SqlValue::Integer(number.as_i64().ok_or_else(|| fail("image integer"))?)
        }
        Value::String(text) => SqlValue::Text(text.clone()),
        Value::Object(tagged) => {
            if let Some(real) = tagged.get("real") {
                SqlValue::Real(real.as_f64().ok_or_else(|| fail("image real"))?)
            } else {
                let hex = tagged
                    .get("blob")
                    .and_then(Value::as_str)
                    .ok_or_else(|| fail("image blob"))?;
                let bytes = (0..hex.len())
                    .step_by(2)
                    .map(|at| {
                        hex.get(at..at + 2)
                            .and_then(|pair| u8::from_str_radix(pair, 16).ok())
                            .ok_or_else(|| fail("image hex"))
                    })
                    .collect::<Result<Vec<u8>, CellError>>()?;
                SqlValue::Blob(bytes)
            }
        }
        Value::Bool(_) | Value::Array(_) => return Err(fail("not an image value")),
    })
}

fn text<'a>(image: &'a Value, field: &str) -> Result<&'a str, CellError> {
    image
        .get(field)
        .and_then(Value::as_str)
        .ok_or_else(|| fail(format!("image {field}")))
}

/// `engine_fixture::load`, through any engine: tables and indexes in
/// catalog order, the rows, then views and triggers, in one transaction.
fn load<E: SqlEngine>(engine: &E, image: &Value) -> Result<(), CellError> {
    if image.get("format").and_then(Value::as_str) != Some(IMAGE_FORMAT) {
        return Err(fail("image format"));
    }
    let schema = image
        .get("schema")
        .and_then(Value::as_array)
        .ok_or_else(|| fail("image schema"))?;
    let deferred = |entry: &Value| {
        matches!(
            entry.get("type").and_then(Value::as_str),
            Some("view" | "trigger")
        )
    };
    engine.transaction(|| {
        for entry in schema.iter().filter(|entry| !deferred(entry)) {
            engine.execute_batch(text(entry, "sql")?)?;
        }
        for entry in schema
            .iter()
            .filter(|entry| entry.get("type").and_then(Value::as_str) == Some("table"))
        {
            let name = text(entry, "name")?;
            let table = image
                .get("tables")
                .and_then(|tables| tables.get(name))
                .ok_or_else(|| fail(format!("image rows of {name}")))?;
            let columns = table
                .get("columns")
                .and_then(Value::as_array)
                .ok_or_else(|| fail("image columns"))?
                .iter()
                .map(|column| column.as_str().map(quote).ok_or_else(|| fail("column")))
                .collect::<Result<Vec<_>, _>>()?;
            let insert = format!(
                "INSERT INTO {} ({}) VALUES ({})",
                quote(name),
                columns.join(", "),
                (1..=columns.len())
                    .map(|at| format!("?{at}"))
                    .collect::<Vec<_>>()
                    .join(", ")
            );
            for row in table
                .get("rows")
                .and_then(Value::as_array)
                .ok_or_else(|| fail("image rows"))?
            {
                let values = row
                    .as_array()
                    .ok_or_else(|| fail("image row"))?
                    .iter()
                    .map(decode)
                    .collect::<Result<Vec<_>, _>>()?;
                engine.execute(&insert, &values)?;
            }
        }
        for entry in schema.iter().filter(|entry| deferred(entry)) {
            engine.execute_batch(text(entry, "sql")?)?;
        }
        Ok(())
    })
}

fn is_platform(name: &str, globs: &[&str]) -> bool {
    globs.iter().any(|glob| {
        glob.strip_suffix('*')
            .map_or(*glob == name, |prefix| name.starts_with(prefix))
    })
}

fn render(value: &SqlValue, out: &mut String) {
    // Tagged, so `1` and `'1'` digest differently.
    let _ = match value {
        SqlValue::Null => write!(out, "n;"),
        SqlValue::Integer(integer) => write!(out, "i{integer};"),
        SqlValue::Real(real) => write!(out, "r{};", real.to_bits()),
        SqlValue::Text(text) => write!(out, "t{}:{text};", text.len()),
        SqlValue::Blob(bytes) => write!(out, "b{}:{bytes:02x?};", bytes.len()),
    };
}

/// A digest of every cell object (catalog entries and all rows by rowid),
/// the platform's own tables excluded, and of `only` tables when given.
fn digest<E: SqlEngine>(engine: &E, only: Option<&[&str]>) -> Result<String, CellError> {
    let globs = engine.platform_table_globs();
    let mut out = String::new();
    let catalog = engine.query(
        "SELECT type, name, tbl_name, sql FROM sqlite_master \
         WHERE sql IS NOT NULL AND name NOT LIKE 'sqlite_%' ORDER BY type, name",
        &[],
    )?;
    for entry in catalog {
        let [kind, name, table, sql] =
            <[SqlValue; 4]>::try_from(entry).map_err(|_| fail("catalog row"))?;
        let (SqlValue::Text(kind), SqlValue::Text(name), SqlValue::Text(table)) =
            (kind, name, table)
        else {
            return Err(fail("catalog text"));
        };
        if is_platform(&table, globs) || only.is_some_and(|only| !only.contains(&table.as_str()))
        {
            continue;
        }
        let _ = write!(out, "{kind} {name} on {table}: ");
        render(&sql, &mut out);
        out.push('\n');
        if kind == "table" {
            for row in engine.query(
                &format!("SELECT rowid, * FROM {} ORDER BY rowid", quote(&name)),
                &[],
            )? {
                row.iter().for_each(|value| render(value, &mut out));
                out.push('\n');
            }
        }
    }
    Ok(format!("{:x}", Sha256::digest(out.as_bytes())))
}

fn set_stamp<E: SqlEngine>(engine: &E, stamp: &str) -> Result<(), CellError> {
    engine.transaction(|| {
        engine.execute(
            "INSERT INTO \"_norn_meta\" (\"key\", \"value\") VALUES (?1, ?2) \
             ON CONFLICT (\"key\") DO UPDATE SET \"value\" = excluded.\"value\"",
            &[SqlValue::from(STAMP_KEY), SqlValue::from(stamp)],
        )
    })
}

/// The ledger head (commit count and hash) and receipt count after a full
/// verification, as JSON.
fn opened(store: &CellStore<RivetEngine>) -> Result<Value, CellError> {
    let head = store.read().verify_ledger()?;
    let receipts = store.read().receipts()?;
    Ok(json!({"commits": head.commits, "hash": head.hash, "receipts": receipts.len()}))
}

const EVIDENCE: &[&str] = &["_norn_ledger", "_norn_receipt"];

/// Load `fixture`, open it on this engine, and check what ENGINE-UPGRADE-1
/// promises. `fresh` builds another engine over the same actor database.
fn upgrade(
    fresh: &dyn Fn() -> RivetEngine,
    fixture: &str,
    from: u32,
) -> Result<Value, CellError> {
    let image: Value = serde_json::from_str(fixture).map_err(|error| fail(error.to_string()))?;
    let raw = fresh();
    if raw
        .query_optional(
            "SELECT 1 FROM sqlite_master WHERE name = '_norn_meta'",
            &[],
        )?
        .is_some()
    {
        return Err(fail("the probe needs a fresh actor (pass a new object=)"));
    }
    load(&raw, &image)?;
    let admitted = engine_release::admission(&raw)?;
    if admitted != (Admission::Upgrade { from }) {
        return Err(fail(format!("admission {admitted:?}, expected upgrade from {from}")));
    }
    let evidence = digest(&raw, Some(EVIDENCE))?;
    drop(raw);

    let store = CellStore::from_engine(fresh(), model()?)?;
    let head = opened(&store)?;
    drop(store);
    let raw = fresh();
    let after = engine_release::admission(&raw)?;
    let stamp = raw
        .query_optional(
            "SELECT \"value\" FROM \"_norn_meta\" WHERE \"key\" = ?1",
            &[SqlValue::from(STAMP_KEY)],
        )?
        .and_then(|row| row.into_iter().next());
    let current = serde_json::to_string(&EngineStamp::current())
        .map_err(|error| fail(error.to_string()))?;
    let evidence_kept = digest(&raw, Some(EVIDENCE))? == evidence;
    let upgraded = digest(&raw, None)?;
    drop(raw);

    // A current open migrates nothing and writes nothing.
    let store = CellStore::from_engine(fresh(), model()?)?;
    let reopened = opened(&store)?;
    drop(store);
    let unchanged = digest(&fresh(), None)? == upgraded;

    let ok = after == Admission::Current
        && stamp == Some(SqlValue::Text(current))
        && evidence_kept
        && unchanged
        && reopened == head;
    Ok(json!({
        "ok": ok,
        "from": from,
        "to": STORAGE_VERSION,
        "admissionAfter": format!("{after:?}"),
        "ledgerAndReceiptsUnchanged": evidence_kept,
        "reopenWritesNothing": unchanged,
        "head": head,
    }))
}

/// Upgrade the v1 fixture, write `stamp` below the engine, and check that
/// the open refuses with `expected` and leaves every cell object identical.
fn refusal(
    fresh: &dyn Fn() -> RivetEngine,
    stamp: &str,
    expected: fn(&CellError) -> bool,
) -> Result<Value, CellError> {
    let upgraded = upgrade(fresh, STORAGE_V1, 1)?;
    if upgraded.get("ok") != Some(&Value::Bool(true)) {
        return Ok(json!({"ok": false, "upgrade": upgraded}));
    }
    set_stamp(&fresh(), stamp)?;
    let before = digest(&fresh(), None)?;
    let refused = match CellStore::from_engine(fresh(), model()?) {
        Ok(_) => None,
        Err(error) => Some(error),
    };
    let unchanged = digest(&fresh(), None)? == before;
    let matched = refused.as_ref().is_some_and(expected);
    Ok(json!({
        "ok": matched && unchanged,
        "refusal": refused.map(|error| error.to_string()),
        "unchanged": unchanged,
    }))
}

/// Run `case` over engines that `fresh` builds on one actor database.
///
/// # Errors
///
/// A step the probe could not run (as opposed to a check that failed, which
/// answers `ok: false`).
pub fn run(case: Case, fresh: &dyn Fn() -> RivetEngine) -> Result<Value, CellError> {
    match case {
        Case::UpgradeV1 => upgrade(fresh, STORAGE_V1, 1),
        Case::UpgradeV2 => upgrade(fresh, STORAGE_V2, 2),
        Case::Downgrade => refusal(
            fresh,
            &format!(
                r#"{{"format":"{STAMP_FORMAT}","storage":{},"writer":"drop-99"}}"#,
                STORAGE_VERSION + 1
            ),
            |error| {
                matches!(
                    error,
                    CellError::EngineDowngrade { cell, writer, engine }
                        if *cell == STORAGE_VERSION + 1 && writer == "drop-99" && *engine == STORAGE_VERSION
                )
            },
        ),
        Case::UnknownStamp => refusal(
            fresh,
            r#"{"format":"norn.cell.engine/99","storage":3,"writer":"drop-99"}"#,
            |error| matches!(error, CellError::EngineUnknownVersion { .. }),
        ),
    }
}
