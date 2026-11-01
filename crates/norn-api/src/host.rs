//! Definition epochs over the existing tenant cell store.

use std::sync::{Mutex, MutexGuard, RwLock, RwLockReadGuard};

use norn_cell::engine::SqlEngine;
use norn_cell::{CellStore, InstalledDefinitionInput, InstalledDefinitionSet, Model};
use norn_kernel::LogicalTime;

use crate::{defs::Admitted, refusal::Refusal, scope::CellScope};

/// One tenant's admitted definitions and cell, shared by transport adapters.
pub struct DevHost<E: SqlEngine> {
    scope: CellScope,
    pub(crate) authorizer: Box<dyn crate::authz::Authorizer>,
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
        CellStore::require_fresh_engine(&engine)
            .map_err(|error| Refusal::invalid(error.to_string()))?;
        let mut cell = CellStore::from_engine(
            engine,
            Model::from_definitions(&[], &[]).map_err(Refusal::host)?,
        )
        .map_err(Refusal::host)?;
        cell.bind("scope", &scope.key()).map_err(Refusal::host)?;
        install(&mut cell, admitted, Some(0), &scope, now)?;
        Ok(Self {
            scope,
            authorizer: Box::new(crate::authz::DenyAll),
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

    /// Configure trusted server authorization before sharing this host.
    #[must_use]
    pub fn with_authorizer(mut self, authorizer: impl crate::authz::Authorizer + 'static) -> Self {
        self.authorizer = Box::new(authorizer);
        self
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
    cell.install_definitions(
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
        expected,
        &scope.key(),
    )
    .map_err(Refusal::host)
}

#[cfg(all(test, feature = "native"))]
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
            assert_eq!(refusal.code, "request.invalid");
            assert_eq!(refusal.status, 400);
        }
    }
}

#[cfg(all(test, feature = "native"))]
#[allow(
    clippy::expect_used,
    missing_docs,
    reason = "host conformance assertions"
)]
mod defs_read {
    use crate::{
        authz::{AccessRequest, Authorizer},
        host::DevHost,
        refusal::Refusal,
        routes::{DefinitionsQuery, get_defs},
        scope::CellScope,
    };
    use norn_cell::{
        CellError,
        engine::{NativeEngine, SqlEngine, SqlValue},
    };
    use norn_kernel::LogicalTime;
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };

    struct Spy(NativeEngine, Arc<AtomicUsize>);
    impl SqlEngine for Spy {
        fn query(&self, sql: &str, params: &[SqlValue]) -> Result<Vec<Vec<SqlValue>>, CellError> {
            self.1.fetch_add(1, Ordering::SeqCst);
            self.0.query(sql, params)
        }
        fn execute(&self, sql: &str, params: &[SqlValue]) -> Result<(), CellError> {
            self.0.execute(sql, params)
        }
        fn execute_batch(&self, sql: &str) -> Result<(), CellError> {
            self.0.execute_batch(sql)
        }
        fn transaction<T, X: From<CellError>>(
            &self,
            f: impl FnOnce() -> Result<T, X>,
        ) -> Result<T, X> {
            self.0.transaction(f)
        }
    }
    struct Reader;
    impl Authorizer for Reader {
        fn authorize(&self, request: &AccessRequest<'_>) -> Result<(), Refusal> {
            if request.actor == Some("reader") {
                Ok(())
            } else {
                crate::authz::DenyAll.authorize(request)
            }
        }
    }
    fn host() -> (DevHost<Spy>, Arc<AtomicUsize>, DefinitionsQuery) {
        let reads = Arc::new(AtomicUsize::new(0));
        let scope = CellScope {
            org_id: "org/%x".into(),
            environment_id: "dev/%x".into(),
        };
        let query = DefinitionsQuery {
            org_id: Some(scope.org_id.clone()),
            environment_id: Some(scope.environment_id.clone()),
            version: None,
        };
        let host = DevHost::new(
            Spy(
                NativeEngine::open_in_memory().expect("engine"),
                reads.clone(),
            ),
            "[]",
            scope,
            LogicalTime::EPOCH,
        )
        .expect("host");
        (host, reads, query)
    }
    #[cfg(feature = "http-host")]
    #[tokio::test]
    async fn host_scope_and_default_deny_precede_every_store_read() {
        use tower::ServiceExt as _;
        let (host, reads, query) = host();
        reads.store(0, Ordering::SeqCst);
        assert_eq!(
            get_defs(&host, &query, Some("reader"))
                .expect_err("default deny")
                .code,
            "authz.denied"
        );
        let app = crate::routes::router(Arc::new(host.with_authorizer(Reader)));
        for (query, code) in [
            ("orgId=org%2F%25x&environmentId=dev%2F%25x", "authz.denied"),
            ("orgId=org%2F%25x", "scope.missing"),
            ("environmentId=dev%2F%25x", "scope.missing"),
            ("orgId=%20&environmentId=dev%2F%25x", "scope.missing"),
            ("orgId=other&environmentId=dev%2F%25x", "scope.denied"),
            ("orgId=org%2F%25x&environmentId=other", "scope.denied"),
            ("version=invalid", "request.invalid"),
        ] {
            let request = http::Request::builder()
                .uri(format!("/v1/defs?{query}"))
                .body(axum::body::Body::empty())
                .expect("request");
            let response = app.clone().oneshot(request).await.expect("response");
            assert!(response.status().is_client_error());
            let bytes = axum::body::to_bytes(response.into_body(), 4096)
                .await
                .expect("body");
            let refusal: serde_json::Value = serde_json::from_slice(&bytes).expect("refusal");
            assert_eq!(refusal["code"], code);
        }
        assert_eq!(reads.load(Ordering::SeqCst), 0);
    }
    #[test]
    fn host_definition_epoch_keeps_install_and_read_revisions_coherent() {
        let (host, _, mut query) = host();
        let host = Arc::new(host.with_authorizer(Reader));
        let epoch = host.definition_read().expect("epoch");
        std::thread::scope(|threads| {
            let (sent, recv) = std::sync::mpsc::channel();
            let writer = host.clone();
            let installed = threads.spawn(move || {
                sent.send(writer.definitions.try_write().is_err())
                    .expect("ready");
                writer.install("[]", 1, LogicalTime::from_millis(2))
            });
            assert!(recv.recv().expect("writer blocked"));
            drop(epoch);
            assert_eq!(installed.join().expect("join").expect("install").version, 2);
        });
        let mut read_version = |version| {
            query.version = version;
            get_defs(&host, &query, Some("reader"))
                .expect("read")
                .version
        };
        assert_eq!(read_version(None), 2);
        assert_eq!(read_version(Some(1)), 1);
        query.version = Some(3);
        let refusal = get_defs(&host, &query, Some("reader")).expect_err("unavailable");
        assert_eq!(
            (refusal.code.as_str(), refusal.status),
            ("defs.version_not_found", 404)
        );
    }
    #[test]
    fn host_wire_preserves_exact_definition_literals_and_empty_collections() {
        let (host, _, query) = host();
        let host = host.with_authorizer(Reader);
        let empty = get_defs(&host, &query, Some("reader")).expect("empty");
        let json = serde_json::to_string(&empty).expect("wire");
        let golden = format!(
            r#"{{"admitted":true,"version":1,"layoutRevision":1,"layout":{{"layoutVersion":"0.1","artifacts":{{}}}},"layoutDiagnostics":{},"receipt":{},"receiptSha256":"{}","diagnostics":{},"registry":[],"baseArtifactIds":[],"artifacts":[]}}"#,
            empty.layout_diagnostics,
            empty.receipt.get(),
            empty.receipt_sha256,
            empty.diagnostics
        );
        assert_eq!(json, golden);
        let source = r#"[{"irVersion":"0.1","artifactId":"test.Item.entity","kind":"entity","namespace":"test","name":"Item","version":"0.1.0","lifecycle":"draft","scope":{},"dependencies":[],"provenance":{"source":"authoring_sdk"},"payload":{"entityVersion":"2.0","objectType":{"name":"test.Item","properties":[{"id":"data","type":{"kind":"json-ext"},"default":{"large":9007199254740993,"tiny":0.0000000000000000000000000001,"boundary":79228162514264337593543950335,"cursor":null}}]}}}]"#;
        host.install(source, 1, norn_kernel::LogicalTime::from_millis(2))
            .expect("exact install");
        let document = get_defs(&host, &query, Some("reader")).expect("exact read");
        let text = serde_json::to_string(&document).expect("wire");
        assert!(text.contains(r#"{"boundary":79228162514264337593543950335,"cursor":null,"large":9007199254740993,"tiny":0.0000000000000000000000000001}"#));
        let invalid = source.replace("9007199254740993", "79228162514264337593543950336");
        assert!(
            host.install(&invalid, 2, norn_kernel::LogicalTime::from_millis(3))
                .is_err()
        );
    }
}
