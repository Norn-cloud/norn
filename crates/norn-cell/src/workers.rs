//! DO SQLite implementation of SqlEngine; TXN-ATOMIC-1 includes typed refusals.
use std::cell::Cell;

use js_sys::{Array, Uint8Array};
use wasm_bindgen::{closure::ScopedClosure, prelude::*};

use crate::CellError;
use crate::engine::{
    MAX_SAFE_INTEGER, SqlEngine, SqlValue, Transact, run_transaction, validate_bindings,
};

#[wasm_bindgen(inline_js = r#"
export function rows(storage, sql, params) {
  return Array.from(storage.sql.exec(sql, ...params).raw());
}
export function transact(storage, callback) {
  return storage.transactionSync(callback);
}
"#)]
extern "C" {
    #[wasm_bindgen(catch)]
    fn rows(storage: &JsValue, sql: &str, params: &Array) -> Result<Array, JsValue>;
    #[wasm_bindgen(catch)]
    fn transact(storage: &JsValue, callback: &JsValue) -> Result<JsValue, JsValue>;
}

/// A trusted DO root storage handle; SQL joins any outer SQLite transaction.
/// Platform SQL authorization must reject transaction-control SQL, as DO does.
/// Neither this handle nor arbitrary SQL is a network operation.
#[derive(Debug)]
pub struct WorkersEngine {
    storage: JsValue,
    active: Cell<bool>,
    revision: crate::engine::WriteRevision,
}

impl WorkersEngine {
    /// Bind the root storage supplied by the DO constructor.
    #[must_use]
    pub const fn new(storage: JsValue) -> Self {
        Self {
            storage,
            active: Cell::new(false),
            revision: crate::engine::WriteRevision::new(),
        }
    }
}

fn backend(error: &JsValue) -> CellError {
    CellError::Engine(format!("DO SQLite: {error:?}"))
}

#[allow(
    clippy::as_conversions,
    clippy::cast_precision_loss,
    reason = "validate_bindings bounds integers to the exactly representable JS range"
)]
fn encode(value: &SqlValue) -> JsValue {
    match value {
        SqlValue::Null => JsValue::NULL,
        SqlValue::Integer(value) => JsValue::from_f64(*value as f64),
        SqlValue::Real(value) => JsValue::from_f64(*value),
        SqlValue::Text(value) => JsValue::from_str(value),
        SqlValue::Blob(value) => Uint8Array::from(value.as_slice()).into(),
    }
}

#[allow(
    clippy::as_conversions,
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    reason = "finite integral values are checked against the exact JS integer range before conversion"
)]
fn decode(value: &JsValue) -> Result<SqlValue, CellError> {
    if value.is_null() {
        return Ok(SqlValue::Null);
    }
    if let Some(text) = value.as_string() {
        return Ok(SqlValue::Text(text));
    }
    if let Some(number) = value.as_f64() {
        if !number.is_finite() {
            return Err(CellError::Engine("non-finite SQL value".into()));
        }
        if number.fract() != 0.0 {
            return Ok(SqlValue::Real(number));
        }
        if number.abs() > MAX_SAFE_INTEGER as f64 {
            return Err(CellError::OutOfRange("SQL integer"));
        }
        return Ok(SqlValue::Integer(number as i64));
    }
    if value.is_instance_of::<js_sys::ArrayBuffer>() {
        return Ok(SqlValue::Blob(Uint8Array::new(value).to_vec()));
    }
    if value.is_instance_of::<Uint8Array>() {
        return Ok(SqlValue::Blob(Uint8Array::new(value).to_vec()));
    }
    Err(CellError::Engine("unsupported DO SQL value".into()))
}

/// `storage.transactionSync`; the decision logic is `run_transaction`.
impl Transact for WorkersEngine {
    type Thrown = JsValue;

    fn transaction_sync<F: FnMut() -> Result<(), JsValue>>(
        &self,
        callback: &mut F,
    ) -> Result<(), JsValue> {
        // The closure type is named: a generic `F` alone does not pin its arity.
        let closure =
            ScopedClosure::<dyn FnMut() -> Result<(), JsValue> + '_>::borrow_mut_aborting(callback);
        transact(&self.storage, closure.as_js_value()).map(|_| ())
    }

    fn rollback_marker(&self) -> JsValue {
        JsValue::from_str("norn:rollback")
    }

    fn is_rollback_marker(&self, thrown: &JsValue) -> bool {
        thrown.as_string().as_deref() == Some("norn:rollback")
    }

    fn callback_reused(&self) -> JsValue {
        JsValue::from_str("transaction callback reused")
    }

    fn engine_error(&self, thrown: &JsValue) -> CellError {
        backend(thrown)
    }
}

impl SqlEngine for WorkersEngine {
    /// Tables the runtime keeps in the DO's own database, visible in
    /// `sqlite_master`: `_cf_*` and `__cf_*` (reserved by the Durable Objects
    /// storage API, `__cf_kv` among them), celld's Litestream replication
    /// tables, and Wrangler/Miniflare's local `__miniflare_do_name`.
    fn platform_table_globs(&self) -> &'static [&'static str] {
        &["_cf_*", "__cf_*", "_litestream_*", "__miniflare_*"]
    }

    /// A Durable Object's storage has one connection, this cell's, and the
    /// cell serves each request in one synchronous Wasm call with no await, so
    /// nothing writes between two reads of a call.
    fn read_isolation(&self) -> crate::engine::ReadIsolation {
        crate::engine::ReadIsolation::ExclusiveSynchronous
    }

    /// This connection is the database's only writer, so a count of the
    /// writes it ran is a revision ([`crate::engine::WriteRevision`]).
    fn read_cache_revision(&self) -> Result<Option<(u64, u64)>, CellError> {
        Ok(self.revision.current(self.active.get()))
    }

    fn execute(&self, sql: &str, params: &[SqlValue]) -> Result<(), CellError> {
        self.query(sql, params).map(|_| ())
    }

    fn query(&self, sql: &str, params: &[SqlValue]) -> Result<Vec<Vec<SqlValue>>, CellError> {
        validate_bindings(sql, params)?;
        self.revision.note(sql);
        let bindings = params.iter().map(encode).collect::<Array>();
        rows(&self.storage, sql, &bindings)
            .map_err(|error| backend(&error))?
            .iter()
            .map(|row| {
                if !Array::is_array(&row) {
                    return Err(CellError::Engine("invalid DO SQL row".into()));
                }
                Array::from(&row)
                    .iter()
                    .map(|value| decode(&value))
                    .collect()
            })
            .collect()
    }

    fn execute_batch(&self, sql: &str) -> Result<(), CellError> {
        self.revision.bump();
        self.execute(sql, &[])
    }

    fn transaction<T, X: From<CellError>>(&self, f: impl FnOnce() -> Result<T, X>) -> Result<T, X> {
        run_transaction(self, &self.active, f)
    }
}
