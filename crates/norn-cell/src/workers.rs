//! DO SQLite implementation of SqlEngine; TXN-ATOMIC-1 includes typed refusals.
use std::cell::Cell;

use js_sys::{Array, Uint8Array};
use wasm_bindgen::{closure::ScopedClosure, prelude::*};

use crate::CellError;
use crate::engine::{MAX_SAFE_INTEGER, SqlEngine, SqlValue, validate_bindings};

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
}

impl WorkersEngine {
    /// Bind the root storage supplied by the DO constructor.
    #[must_use]
    pub const fn new(storage: JsValue) -> Self {
        Self {
            storage,
            active: Cell::new(false),
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

struct Active<'a>(&'a Cell<bool>);
impl Drop for Active<'_> {
    fn drop(&mut self) {
        self.0.set(false);
    }
}

impl SqlEngine for WorkersEngine {
    /// Tables the runtime keeps in the DO's own database, visible in
    /// `sqlite_master`: `_cf_*` (reserved by the Durable Objects storage API),
    /// celld's Litestream replication tables, and Wrangler/Miniflare's local
    /// `__miniflare_do_name`.
    fn platform_table_globs(&self) -> &'static [&'static str] {
        &["_cf_*", "_litestream_*", "__miniflare_*"]
    /// A Durable Object's storage has one connection, this cell's, and the
    /// cell serves each request in one synchronous Wasm call with no await, so
    /// nothing writes between two reads of a call.
    fn read_isolation(&self) -> crate::engine::ReadIsolation {
        crate::engine::ReadIsolation::ExclusiveSynchronous
    }

    fn execute(&self, sql: &str, params: &[SqlValue]) -> Result<(), CellError> {
        self.query(sql, params).map(|_| ())
    }

    fn query(&self, sql: &str, params: &[SqlValue]) -> Result<Vec<Vec<SqlValue>>, CellError> {
        validate_bindings(sql, params)?;
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
        self.execute(sql, &[])
    }

    fn transaction<T, X: From<CellError>>(&self, f: impl FnOnce() -> Result<T, X>) -> Result<T, X> {
        if self.active.replace(true) {
            return Err(CellError::Engine("nested transaction refused".into()).into());
        }
        let _active = Active(&self.active);
        let mut operation = Some(f);
        let mut output = None;
        let mut callback = || -> Result<(), JsValue> {
            let Some(f) = operation.take() else {
                return Err(JsValue::from_str("transaction callback reused"));
            };
            let result = f();
            let failed = result.is_err();
            output = Some(result);
            if failed {
                Err(JsValue::from_str("norn:rollback"))
            } else {
                Ok(())
            }
        };
        let result = {
            let closure = ScopedClosure::borrow_mut_aborting(&mut callback);
            transact(&self.storage, closure.as_js_value())
        };
        match (result, output) {
            (Err(error), Some(Err(refusal)))
                if error.as_string().as_deref() == Some("norn:rollback") =>
            {
                Err(refusal)
            }
            (Err(error), _) => Err(backend(&error).into()),
            (Ok(_), Some(Ok(value))) => Ok(value),
            (Ok(_), None | Some(Err(_))) => {
                Err(CellError::Engine("transaction callback/rollback missing".into()).into())
            }
        }
    }
}
