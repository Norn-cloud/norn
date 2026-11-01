//! Definition epochs over the existing tenant cell store.

use std::sync::{Mutex, MutexGuard, RwLock, RwLockReadGuard};

use norn_cell::engine::SqlEngine;
use norn_cell::{CellStore, InstalledDefinitionInput, InstalledDefinitionSet};
use norn_kernel::LogicalTime;

use crate::{defs::Admitted, refusal::Refusal, scope::CellScope};

/// One tenant's admitted definitions and cell, shared by transport adapters.
pub struct DevHost<E: SqlEngine> {
    scope: CellScope,
    definitions: RwLock<()>,
    cell: Mutex<CellStore<E>>,
}

impl<E: SqlEngine> DevHost<E> {
    /// Install checked definitions in a fresh engine before publishing the host.
    ///
    /// # Errors
    /// Admission, scope binding, existing definitions, or store failures refuse.
    pub fn new(
        engine: E,
        source: &str,
        scope: CellScope,
        now: LogicalTime,
    ) -> Result<Self, Refusal> {
        let admitted = Admitted::from_source(source, &scope)?;
        let (model, input) = installation(admitted, now);
        let cell = CellStore::provision_definitions(engine, model, &input, &scope.key())
            .map_err(Refusal::host)?;
        Ok(Self {
            scope,
            definitions: RwLock::new(()),
            cell: Mutex::new(cell),
        })
    }

    /// Trusted host installation; no network caller gains installation authority.
    ///
    /// # Errors
    /// Failed admission or a stale version publishes no definition revision.
    pub fn install(
        &self,
        source: &str,
        expected_version: u64,
        now: LogicalTime,
    ) -> Result<InstalledDefinitionSet, Refusal> {
        let admitted = Admitted::from_source(source, &self.scope)?;
        let _epoch = self.definitions.write().map_err(Refusal::host)?;
        install(
            &mut *self.cell()?,
            admitted,
            Some(expected_version),
            &self.scope,
            now,
        )
    }

    /// Scope served by this host.
    #[must_use]
    pub const fn scope(&self) -> &CellScope {
        &self.scope
    }

    /// Hold the definition epoch across authorization and document projection.
    ///
    /// # Errors
    /// A poisoned epoch refuses closed.
    pub fn definition_read(&self) -> Result<RwLockReadGuard<'_, ()>, Refusal> {
        self.definitions.read().map_err(Refusal::host)
    }

    pub(crate) fn cell(&self) -> Result<MutexGuard<'_, CellStore<E>>, Refusal> {
        self.cell.lock().map_err(Refusal::host)
    }
}

fn install<E: SqlEngine>(
    cell: &mut CellStore<E>,
    admitted: Admitted,
    expected: Option<u64>,
    scope: &CellScope,
    now: LogicalTime,
) -> Result<InstalledDefinitionSet, Refusal> {
    let (model, input) = installation(admitted, now);
    cell.install_definitions(model, input, expected, &scope.key())
        .map_err(Refusal::host)
}

fn installation(
    admitted: Admitted,
    now: LogicalTime,
) -> (norn_cell::Model, InstalledDefinitionInput) {
    (
        admitted.model,
        InstalledDefinitionInput {
            canonical_set: admitted.canonical_set,
            registry: Vec::new(),
            receipt_sha256: admitted.receipt_sha256,
            admission_receipt: Some(admitted.receipt),
            readmission: None,
            diff: serde_json::json!({}),
            installed_at: now.as_millis(),
            layout: Some(admitted.layout),
        },
    )
}

#[cfg(all(test, not(target_arch = "wasm32")))]
#[allow(
    clippy::expect_used,
    missing_docs,
    reason = "host conformance assertions"
)]
mod tests {
    use super::*;
    use norn_cell::engine::NativeEngine;

    fn scope() -> CellScope {
        CellScope {
            org_id: "org_demo".into(),
            environment_id: "dev".into(),
        }
    }

    #[test]
    fn host_admission_failure_preserves_the_installed_revision_and_ledger() {
        let host = DevHost::new(
            NativeEngine::open_in_memory().expect("engine"),
            "[]",
            scope(),
            LogicalTime::from_millis(1),
        )
        .expect("admitted host");
        let read = || {
            host.cell()
                .expect("cell")
                .read()
                .installed_definitions(None)
                .expect("read")
        };
        let before = read().expect("installed");
        assert_eq!(before.version, 1);
        assert!(
            host.install("[{}]", 1, LogicalTime::from_millis(2))
                .is_err()
        );
        assert_eq!(read(), Some(before.clone()));
        let after = host
            .install("[]", 1, LogicalTime::from_millis(3))
            .expect("install");
        assert_eq!(after.version, 2);
        assert_eq!(after.commit_seq, before.commit_seq + 1);
        assert_eq!(after.receipt_sha256, before.receipt_sha256);
        assert!(host.install("[]", 1, LogicalTime::from_millis(4)).is_err());
        assert_eq!(read(), Some(after));
    }

    #[test]
    fn host_admission_refuses_unchecked_and_out_of_scope_sources() {
        for source in ["{}", "[{}]", "1e999999"] {
            let refusal = DevHost::new(
                NativeEngine::open_in_memory().expect("engine"),
                source,
                scope(),
                LogicalTime::EPOCH,
            )
            .err()
            .expect("unchecked source refuses");
            assert_eq!(refusal.code, "defs.not_admitted");
        }
        let source =
            include_str!("../../../fixtures/ir/entity-v2/artifact-set/retail-sku.entity.json");
        let mut other = scope();
        other.org_id = "other".into();
        let refused = Admitted::from_source(&format!("[{source}]"), &other)
            .err()
            .expect("scope refusal");
        assert_eq!(refused.code, "defs.not_admitted");
    }

    #[test]
    fn host_admission_refuses_residual_state_without_active_definitions() {
        for table in ["metadata", "ledger", "value_history"] {
            let engine = NativeEngine::open_in_memory().expect("engine");
            engine
                .execute_batch(&format!(
                    "CREATE TABLE {table} (value TEXT); INSERT INTO {table} VALUES ('retained')"
                ))
                .expect("residual state");
            let refusal = DevHost::new(engine, "[]", scope(), LogicalTime::EPOCH)
                .err()
                .expect("not fresh");
            assert_eq!(refusal.code, "host.error");
        }
    }
}
