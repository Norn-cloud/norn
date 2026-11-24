//! Shared transaction glue of callback-style hosts (`Transact`, TXN-ATOMIC-1).
//!
//! Crate-private: the Workers engine uses it; only the test-only
//! `fault-injection` feature publishes it, for the native harness and its
//! mutants. Compiled only where it is used: on Workers (wasm32) and with
//! that feature.

use crate::CellError;

/// A host's callback-style transaction primitive, such as a Durable Object's
/// `storage.transactionSync` (TXN-ATOMIC-1). [`run_transaction`] holds the
/// decision logic every such host shares; an implementation supplies only
/// the primitive and its host-specific thrown values.
pub trait Transact {
    /// What the primitive and its callback throw (a `JsValue` on Workers).
    type Thrown;

    /// Runs `callback` synchronously inside one transaction: commits when it
    /// returns `Ok`, rolls back and rethrows when it returns `Err`, and
    /// throws its own value when the engine fails.
    ///
    /// # Errors
    /// The callback's thrown value, or the engine's.
    fn transaction_sync<F: FnMut() -> Result<(), Self::Thrown>>(
        &self,
        callback: &mut F,
    ) -> Result<(), Self::Thrown>;

    /// The value the callback throws to roll back after the transaction body
    /// failed (the rollback marker).
    fn rollback_marker(&self) -> Self::Thrown;

    /// Whether `thrown` is the rollback marker, rethrown unchanged.
    fn is_rollback_marker(&self, thrown: &Self::Thrown) -> bool;

    /// The value the callback throws if the primitive calls it twice.
    fn callback_reused(&self) -> Self::Thrown;

    /// The engine error for any other thrown value.
    fn engine_error(&self, thrown: &Self::Thrown) -> CellError;
}

struct Active<'a>(&'a std::cell::Cell<bool>);

impl Drop for Active<'_> {
    fn drop(&mut self) {
        self.0.set(false);
    }
}

/// [`SqlEngine::transaction`](super::SqlEngine::transaction) over a
/// [`Transact`] primitive (TXN-ATOMIC-1).
///
/// A transaction already open on `active` is refused here, not by the
/// engine. Otherwise `f` runs once inside the primitive's transaction; its
/// `Err` makes the callback throw the rollback marker, so the primitive rolls
/// back, and the caller gets `f`'s own (typed) error. Any other throw is an
/// engine error, and a commit without `f`'s success is reported as one.
///
/// # Errors
/// `f`'s error after rollback, a nested transaction, or an engine error.
pub fn run_transaction<H: Transact, T, X: From<CellError>>(
    host: &H,
    active: &std::cell::Cell<bool>,
    f: impl FnOnce() -> Result<T, X>,
) -> Result<T, X> {
    if active.replace(true) {
        return Err(CellError::Engine("nested transaction refused".into()).into());
    }
    let _active = Active(active);
    let mut operation = Some(f);
    let mut output = None;
    let mut callback = || -> Result<(), H::Thrown> {
        let Some(f) = operation.take() else {
            return Err(host.callback_reused());
        };
        let result = f();
        let failed = result.is_err();
        output = Some(result);
        if failed {
            Err(host.rollback_marker())
        } else {
            Ok(())
        }
    };
    let result = host.transaction_sync(&mut callback);
    match (result, output) {
        (Err(thrown), Some(Err(refusal))) if host.is_rollback_marker(&thrown) => Err(refusal),
        (Err(thrown), _) => Err(host.engine_error(&thrown).into()),
        (Ok(()), Some(Ok(value))) => Ok(value),
        (Ok(()), None | Some(Err(_))) => {
            Err(CellError::Engine("transaction callback/rollback missing".into()).into())
        }
    }
}
