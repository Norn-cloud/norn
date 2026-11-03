//! Executable adapter correspondence on actual DO SQLite, not a JS storage mock.
#![cfg(target_arch = "wasm32")]
use norn_cell::{CellError, engine::{SqlEngine, SqlValue}, workers::WorkersEngine};
use wasm_bindgen::prelude::*;

/// Commit, rollback a typed refusal, reject nested/SQL transactions and read state.
/// # Errors
/// Any adapter mismatch or storage failure.
#[wasm_bindgen]
pub fn exercise(storage: JsValue) -> Result<String, JsValue> {
    run(storage).map_err(|e| JsValue::from_str(&e.to_string()))
}
fn run(storage: JsValue) -> Result<String, CellError> {
    let engine = WorkersEngine::new(storage);
    engine.execute_batch("CREATE TABLE IF NOT EXISTS probe (id INTEGER PRIMARY KEY, exact TEXT, bytes BLOB)")?;
    engine.transaction(|| engine.execute(
        "INSERT OR IGNORE INTO probe VALUES (?1,?2,?3)",
        &[SqlValue::Integer(1), SqlValue::Text("9007199254740993.0001".into()), SqlValue::Blob(vec![0,255])],
    ))?;
    let refusal = engine.transaction(|| {
        engine.execute("INSERT INTO probe VALUES (2,'refused',NULL)", &[])?;
        Err::<(), _>(CellError::Engine("typed refusal".into()))
    });
    if !matches!(refusal, Err(CellError::Engine(ref e)) if e == "typed refusal") {
        return Err(CellError::Engine("typed refusal lost".into()));
    }
    let nested = engine.transaction(|| {
        engine.execute("INSERT INTO probe VALUES (3,'nested',NULL)", &[])?;
        engine.transaction(|| Ok::<_,CellError>(()))
    });
    if nested.is_ok() || engine.execute("COMMIT", &[]).is_ok() {
        return Err(CellError::Engine("transaction control accepted".into()));
    }
    let rows = engine.query("SELECT id,exact,bytes FROM probe ORDER BY id", &[])?;
    let expected = vec![vec![SqlValue::Integer(1), SqlValue::Text("9007199254740993.0001".into()), SqlValue::Blob(vec![0,255])]];
    if rows != expected {
        return Err(CellError::Engine(format!("rollback/state mismatch: {rows:?}")));
    }
    // Positional projection must retain duplicate column names.
    if engine.query("SELECT 1 AS x,2 AS x", &[])? != vec![vec![SqlValue::Integer(1),SqlValue::Integer(2)]] {
        return Err(CellError::Engine("column order lost".into()));
    }
    Ok("commit,typed-refusal,nested,sql-control,exact,blob,columns:pass".into())
}
