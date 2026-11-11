//! Deterministic fault injection for simulation testing (`fault-injection`
//! feature, enabled only through dev-dependencies).

use super::{CellError, ReadIsolation, SqlEngine, SqlValue};

/// What an injected engine fault does (deterministic simulation testing).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EngineFaultKind {
    /// The host dies at the faulted statement: its transaction never
    /// commits, and the engine refuses every later call. The caller reopens
    /// the store from durable state with a fresh engine.
    Crash,
    /// The faulted statement fails with an I/O error: its transaction rolls
    /// back and the engine stays usable.
    Io,
}

/// One scheduled fault: SQL statement `at_statement` (counting from 0) of
/// transaction `transaction` (counting from 0) begun after
/// [`FaultSwitch::arm`]. A faulted transaction never commits: when its
/// closure would commit with fewer statements, the fault fires at the commit
/// point instead. A closure that fails on its own (a refusal) rolls back
/// without the fault firing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EngineFault {
    /// What the fault does.
    pub kind: EngineFaultKind,
    /// Which transaction after arming fails, counting from 0.
    pub transaction: u32,
    /// Which statement of that transaction fails, counting from 0.
    pub at_statement: u32,
}

/// The fault schedule shared by every [`FaultEngine`] opened over one store,
/// so a test arms it once and it survives reopening the store. Counting is
/// deterministic: statements and transactions in call order, nothing else.
#[derive(Debug, Clone, Default)]
pub struct FaultSwitch(std::rc::Rc<FaultState>);

#[derive(Debug, Default)]
struct FaultState {
    armed: std::cell::Cell<Option<EngineFault>>,
    transactions: std::cell::Cell<u32>,
    statements: std::cell::Cell<u32>,
    targeting: std::cell::Cell<bool>,
    fired: std::cell::Cell<bool>,
}

impl FaultSwitch {
    /// Arms `fault`, counting transactions and statements afresh.
    pub fn arm(&self, fault: EngineFault) {
        self.0.armed.set(Some(fault));
        self.0.transactions.set(0);
        self.0.statements.set(0);
        self.0.targeting.set(false);
        self.0.fired.set(false);
    }

    /// Disarms the switch and returns whether the armed fault fired.
    #[must_use = "the caller must branch on whether the fault fired"]
    pub fn disarm(&self) -> bool {
        self.0.armed.set(None);
        self.0.targeting.set(false);
        self.0.fired.replace(false)
    }

    /// Whether the armed fault has fired.
    #[must_use]
    pub fn fired(&self) -> bool {
        self.0.fired.get()
    }
}

/// An engine that fails on the schedule of its [`FaultSwitch`] and otherwise
/// delegates to `S`. It guarantees that a transaction the fault reaches is
/// rolled back, even when the caller swallows the statement's error.
#[derive(Debug)]
pub struct FaultEngine<S> {
    inner: S,
    switch: FaultSwitch,
    crashed: std::cell::Cell<bool>,
}

impl<S: SqlEngine> FaultEngine<S> {
    /// `inner`, failing on the schedule of `switch`.
    #[must_use]
    pub const fn new(inner: S, switch: FaultSwitch) -> Self {
        Self {
            inner,
            switch,
            crashed: std::cell::Cell::new(false),
        }
    }

    /// The wrapped engine.
    #[must_use]
    pub const fn inner(&self) -> &S {
        &self.inner
    }

    fn error(kind: EngineFaultKind) -> CellError {
        CellError::Engine(match kind {
            EngineFaultKind::Crash => "injected fault: host crashed".to_owned(),
            EngineFaultKind::Io => "injected fault: disk I/O error".to_owned(),
        })
    }

    fn fire(&self, kind: EngineFaultKind) -> CellError {
        self.switch.0.fired.set(true);
        if kind == EngineFaultKind::Crash {
            self.crashed.set(true);
        }
        Self::error(kind)
    }

    fn live(&self) -> Result<(), CellError> {
        if self.crashed.get() {
            Err(Self::error(EngineFaultKind::Crash))
        } else {
            Ok(())
        }
    }

    /// Counts one statement and fails it when it is the armed one.
    fn statement(&self) -> Result<(), CellError> {
        self.live()?;
        let state = &self.switch.0;
        let Some(fault) = state.armed.get() else {
            return Ok(());
        };
        if !state.targeting.get() || state.fired.get() {
            return Ok(());
        }
        let index = state.statements.get();
        state.statements.set(index.saturating_add(1));
        if index == fault.at_statement {
            return Err(self.fire(fault.kind));
        }
        Ok(())
    }
}

impl<S: SqlEngine> SqlEngine for FaultEngine<S> {
    fn execute(&self, sql: &str, params: &[SqlValue]) -> Result<(), CellError> {
        self.statement()?;
        self.inner.execute(sql, params)
    }

    fn query(&self, sql: &str, params: &[SqlValue]) -> Result<Vec<Vec<SqlValue>>, CellError> {
        self.statement()?;
        self.inner.query(sql, params)
    }

    fn execute_batch(&self, sql: &str) -> Result<(), CellError> {
        self.statement()?;
        self.inner.execute_batch(sql)
    }

    fn transaction<T, X: From<CellError>>(&self, f: impl FnOnce() -> Result<T, X>) -> Result<T, X> {
        struct Targeting<'a>(&'a std::cell::Cell<bool>);
        impl Drop for Targeting<'_> {
            fn drop(&mut self) {
                self.0.set(false);
            }
        }
        self.live()?;
        let state = &self.switch.0;
        let Some(fault) = state.armed.get() else {
            return self.inner.transaction(f);
        };
        let index = state.transactions.get();
        state.transactions.set(index.saturating_add(1));
        if index != fault.transaction || state.fired.get() {
            return self.inner.transaction(f);
        }
        state.statements.set(0);
        state.targeting.set(true);
        let targeting = Targeting(&state.targeting);
        let result = self.inner.transaction(|| match f() {
            Err(error) => Err(error),
            // Never commit once the fault fired, even if the caller swallowed
            // the statement's error; otherwise this is the commit point.
            Ok(_) if state.fired.get() => Err(Self::error(fault.kind).into()),
            Ok(_) => Err(self.fire(fault.kind).into()),
        });
        drop(targeting);
        result
    }

    fn platform_table_globs(&self) -> &'static [&'static str] {
        self.inner.platform_table_globs()
    }

    fn read_isolation(&self) -> ReadIsolation {
        self.inner.read_isolation()
    }

    fn read_cache_revision(&self) -> Result<Option<(u64, u64)>, CellError> {
        self.live()?;
        self.inner.read_cache_revision()
    }
}
