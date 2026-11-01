//! Definition epochs over the existing tenant cell store.

use std::sync::{Mutex, MutexGuard, RwLock, RwLockReadGuard};

use norn_cell::engine::SqlEngine;
use norn_cell::{CellStore, InstalledDefinitionInput, InstalledDefinitionSet};
use norn_kernel::LogicalTime;

use crate::{defs::Admitted, refusal::Refusal, scope::CellScope};

/// One tenant's admitted definitions and cell, shared by transport adapters.
pub(crate) type ActionCell<E> = norn_cell::SqliteCell<
    norn_kernel::FactEvent,
    norn_ir::RuntimeObject,
    norn_ir::RuntimeObject,
    E,
>;

/// Shared tenant host.
pub struct DevHost<E: SqlEngine> {
    scope: CellScope,
    pub(crate) authorizer: Box<dyn crate::authz::Authorizer>,
    definitions: RwLock<()>,
    cell: Mutex<ActionCell<E>>,
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
        let driver =
            ActionCell::new(cell, scope.key(), |c| c.payload.clone()).map_err(Refusal::host)?;
        Ok(Self {
            scope,
            authorizer: Box::new(crate::authz::DenyAll),
            definitions: RwLock::new(()),
            cell: Mutex::new(driver),
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
            self.cell()?.store_mut(),
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

    pub(crate) fn cell(&self) -> Result<MutexGuard<'_, ActionCell<E>>, Refusal> {
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
                .store()
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
                Arc::clone(&reads),
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
            assert_eq!(
                response
                    .headers()
                    .get(http::header::CACHE_CONTROL)
                    .expect("cache policy"),
                "no-store"
            );
            let bytes = axum::body::to_bytes(response.into_body(), 4096)
                .await
                .expect("body");
            let refusal: serde_json::Value = serde_json::from_slice(&bytes).expect("refusal");
            assert_eq!(refusal["code"], code);
        }
        assert_eq!(reads.load(Ordering::SeqCst), 0);
    }
    #[cfg(feature = "http-host")]
    #[tokio::test]
    async fn host_http_actor_and_unrepresentable_version_conformance() {
        use tower::ServiceExt as _;
        let (host, _, _) = host();
        let app = crate::routes::router(Arc::new(host.with_authorizer(Reader)));
        for (actor, suffix, status, key, value) in [
            ("reader", "", 200, "version", serde_json::json!(1)),
            ("other", "", 403, "code", serde_json::json!("authz.denied")),
            (
                "reader",
                "&version=18446744073709551615",
                404,
                "code",
                serde_json::json!("defs.version_not_found"),
            ),
            (
                "reader",
                "&version=9007199254740993",
                404,
                "code",
                serde_json::json!("defs.version_not_found"),
            ),
        ] {
            let request = http::Request::builder()
                .uri(format!(
                    "/v1/defs?orgId=org%2F%25x&environmentId=dev%2F%25x{suffix}"
                ))
                .header("x-norn-actor", actor)
                .body(axum::body::Body::empty())
                .expect("request");
            let response = app.clone().oneshot(request).await.expect("response");
            assert_eq!(response.status().as_u16(), status);
            assert_eq!(
                response
                    .headers()
                    .get(http::header::CACHE_CONTROL)
                    .expect("cache policy"),
                "no-store"
            );
            let bytes = axum::body::to_bytes(response.into_body(), 16384)
                .await
                .expect("body");
            let document: serde_json::Value = serde_json::from_slice(&bytes).expect("document");
            assert_eq!(document[key], value);
        }
    }

    #[test]
    fn host_definition_epoch_keeps_install_and_read_revisions_coherent() {
        let (host, _, mut query) = host();
        let host = Arc::new(host.with_authorizer(Reader));
        let epoch = host.definition_read().expect("epoch");
        std::thread::scope(|threads| {
            let (sent, recv) = std::sync::mpsc::channel();
            let writer = Arc::clone(&host);
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
        let source = r#"[{"irVersion":"0.1","artifactId":"test.numbers.setting","kind":"setting","namespace":"test","name":"Numbers","version":"0.1.0","lifecycle":"draft","scope":{},"dependencies":[],"provenance":{"source":"authoring_sdk"},"payload":{"settingVersion":"0.1","settingKey":"test.numbers","valueType":"json","value":{"large":9007199254740993,"tiny":0.0000000000000000000000000001,"boundary":79228162514264337593543950335,"cursor":null},"effectiveFrom":"2026-01-01T00:00:00Z","review":{"state":"approved"},"scopeRequired":["orgId","environmentId"]}}]"#;
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

#[cfg(all(test, feature = "native", feature = "http-host"))]
#[allow(
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::too_many_lines,
    missing_docs,
    reason = "CP1 surface correspondence scenario"
)]
mod cp1_surface {
    use super::*;
    use crate::{
        authz::{AccessRequest, Authorizer, DenyAll},
        routes::router,
    };
    use norn_cell::{CommitBatch, FactWriteRow, LinkChange, engine::NativeEngine};
    use norn_ir::quantity_ledger::{QUANTITY_CELL_VERSION, QuantityOperation, QuantityRequest};
    use norn_ir::{ArtifactEnvelope, ArtifactPayload, EntityIrVersion, EntityRootV2};
    use norn_kernel::{
        ActionAuthzRequest, ActionDenial, ActionReadAuthzRequest, ActionReadTypeAuthzRequest,
        DecisionTurn, FactKey, OWNED_ROWS_TYPE, TypeCatalog, World, owned_rows_manifest_key,
    };
    use serde_json::{Value, json};
    use std::collections::BTreeMap;
    use std::sync::Arc;
    use tower::ServiceExt;
    struct Demo;
    impl Authorizer for Demo {
        fn authorize(&self, r: &AccessRequest<'_>) -> Result<(), Refusal> {
            if r.actor == Some("demo") {
                Ok(())
            } else {
                DenyAll.authorize(r)
            }
        }
        fn action(&self, _: &ActionAuthzRequest<'_>) -> Result<(), ActionDenial> {
            Ok(())
        }
        fn action_read(&self, _: &ActionReadAuthzRequest<'_>) -> Result<(), ActionDenial> {
            Ok(())
        }
        fn action_read_type(&self, _: &ActionReadTypeAuthzRequest<'_>) -> Result<(), ActionDenial> {
            Ok(())
        }
    }
    fn host() -> DevHost<NativeEngine> {
        let mut artifacts: Vec<ArtifactEnvelope> = serde_json::from_str(include_str!(
            "../../../tools/norn-ts/fixtures/stock/stock.artifacts.json"
        ))
        .expect("authored stock");
        for ty in norn_kernel::stock_posting::stock_output_types() {
            let mut artifact = artifacts
                .iter()
                .find(|a| a.kind() == norn_ir::ArtifactKind::Entity)
                .expect("entity")
                .clone();
            artifact.artifact_id = format!("{}.entity", ty.name.as_str());
            artifact.name = ty.name.as_str().into();
            let root = EntityRootV2 {
                entity_version: EntityIrVersion::V2_0,
                object_type: ty,
                mutation_authority: None,
                extensions: None,
            };
            let payload = serde_json::to_value(root).expect("typed");
            artifact.payload =
                ArtifactPayload::Entity(serde_json::from_value(payload).expect("payload"));
            artifacts.retain(|a| a.artifact_id != artifact.artifact_id);
            artifacts.push(artifact);
        }
        let host = DevHost::new(
            NativeEngine::open_in_memory().expect("engine"),
            &serde_json::to_string(&artifacts).expect("source"),
            CellScope {
                org_id: "org_demo".into(),
                environment_id: "dev".into(),
            },
            LogicalTime::EPOCH,
        )
        .expect("admitted host")
        .with_authorizer(Demo);
        {
            let mut cell = host.cell().expect("cell");
            seed(cell.store_mut());
            let rules = norn_kernel::admit_stock_rules(&artifacts).expect("rules");
            let model = cell.store().model().clone();
            let catalog = TypeCatalog::new(model.object_types()).with_stock_rules(&rules);
            let rule = catalog.stock_action("stock.receipt.submit").expect("rule");
            let scope = BTreeMap::from([
                ("orgId".into(), "org_demo".into()),
                ("environmentId".into(), "dev".into()),
            ]);
            let turn = DecisionTurn {
                id: "prepare".into(),
                now: LogicalTime::EPOCH,
                world: World::Main,
                consumes: None,
            };
            let invocation = rule.call("pr", scope, "demo".into(), "prepare".into(), turn);
            let snapshot = norn_kernel::action_commit::stock::prepare_stock_snapshot(
                &*cell,
                &catalog,
                &invocation,
                &crate::actions::Policy(&Demo),
            )
            .expect("partition");
            cell.commit_quantity(
                &QuantityRequest {
                    version: QUANTITY_CELL_VERSION.into(),
                    ledger_id: format!("stock/{}", snapshot.partitions[0].witness.key),
                    balance_versions: vec![],
                    operation: QuantityOperation::Define {
                        account_limit: 3,
                        pack: norn_kernel::stock_posting::stock_quantity_pack("ea").expect("pack"),
                        unrestricted_accounts: vec!["external-source".into()],
                    },
                },
                "configure",
            )
            .expect("quantity configuration");
        }
        host
    }
    fn put(ty: &str, id: &str, values: Value) -> FactWriteRow {
        FactWriteRow::Put {
            object_type: ty.into(),
            key: id.into(),
            values: serde_json::from_value(values).expect("exact fixture values"),
        }
    }
    fn link(ty: &str, id: &str, name: &str, target_type: &str, target: &str) -> LinkChange {
        LinkChange::Add {
            object_type: ty.into(),
            source: id.into(),
            link: name.into(),
            target_type: Some(target_type.into()),
            target: target.into(),
            values: BTreeMap::new(),
        }
    }
    fn seed(store: &mut CellStore) {
        let mut writes = vec![
            put(
                "stock.Company",
                "c",
                json!({"id":"c","name":"Demo","currency":"USD","book_id":"book","graph_id":"graph","graph_version":1,"srbnb_account":"srbnb","cogs_account":"cogs"}),
            ),
            put(
                "stock.Warehouse",
                "w",
                json!({"id":"w","name":"Demo","stock_account":"stock"}),
            ),
            put(
                "stock.Item",
                "i",
                json!({"id":"i","name":"Demo","uom":"ea","valuation_method":"fifo"}),
            ),
            put(
                "stock.Party",
                "party",
                json!({"id":"party","name":"Demo","role":"supplier"}),
            ),
        ];
        let mut links = vec![link(
            "stock.Warehouse",
            "w",
            "company",
            "stock.Company",
            "c",
        )];
        for (ty, child, id, quantities) in [
            (
                "stock.PurchaseReceipt",
                "stock.ReceiptLine",
                "pr",
                vec![(2, 10), (3, 20)],
            ),
            (
                "stock.DeliveryNote",
                "stock.DeliveryLine",
                "dn",
                vec![(4, 0)],
            ),
            (
                "stock.DeliveryNote",
                "stock.DeliveryLine",
                "over",
                vec![(2, 0)],
            ),
        ] {
            writes.push(put(ty,id,json!({"id":id,"request_id":id,"effective_at":"2026-11-01T00:00:00Z","status":"draft"})));
            links.push(link(ty, id, "company", "stock.Company", "c"));
            links.push(link(
                ty,
                id,
                if id == "pr" { "supplier" } else { "customer" },
                "stock.Party",
                "party",
            ));
            let mut rows = Vec::new();
            for (index, (q, rate)) in quantities.into_iter().enumerate() {
                let row = format!("{id}-{index}");
                rows.push(row.clone());
                let mut values =
                    json!({"id":row,"line_number":index.saturating_add(1),"quantity":q});
                if id == "pr" {
                    values["rate"] = json!(rate);
                }
                writes.push(put(child, &row, values));
                links.extend([
                    link(ty, id, "lines", child, &row),
                    link(child, &row, "voucher", ty, id),
                    link(child, &row, "item", "stock.Item", "i"),
                    link(
                        child,
                        &row,
                        if id == "pr" { "destination" } else { "source" },
                        "stock.Warehouse",
                        "w",
                    ),
                ]);
            }
            let key =
                owned_rows_manifest_key(&FactKey::new(ty, id), "lines").expect("manifest identity");
            writes.push(put(
                OWNED_ROWS_TYPE,
                &key.key,
                json!({"parentType":ty,"parentKey":id,"collectionId":"lines","rowIds":rows}),
            ));
        }
        store
            .commit(&CommitBatch {
                world: "main".into(),
                scope: "org_demo/dev".into(),
                writes,
                links,
                ..CommitBatch::default()
            })
            .expect("seed drafts");
    }

    #[allow(
        clippy::disallowed_methods,
        reason = "conformance invokes the real CLI process"
    )]
    async fn invoke(
        app: &axum::Router,
        surface: &str,
        url: &str,
        actor: &str,
        call: [&str; 3],
    ) -> (Value, bool) {
        let [action, id, key] = call;
        let args = json!({"actionId":action,"input":{"id":id},"version":1,"idempotencyKey":key});
        if surface == "cli" {
            let url = format!("{url}/mcp");
            let actor = actor.to_owned();
            let key = key.to_owned();
            let result = tokio::task::spawn_blocking(move || {
                use std::io::Write;
                let mut child = std::process::Command::new(
                    std::env::var("NORN_CLI").expect("NORN_CLI compiled binary"),
                )
                .args([
                    "remote",
                    "--url",
                    &url,
                    "--org",
                    "org_demo",
                    "--env",
                    "dev",
                    "--dev-actor",
                    &actor,
                    "call",
                    "call_action",
                    "--idempotency-key",
                    &key,
                ])
                .stdin(std::process::Stdio::piped())
                .stdout(std::process::Stdio::piped())
                .spawn()
                .expect("CLI");
                child
                    .stdin
                    .take()
                    .expect("stdin")
                    .write_all(args.to_string().as_bytes())
                    .expect("write");
                let output = child.wait_with_output().expect("CLI exit");
                (
                    serde_json::from_slice::<Value>(&output.stdout).expect("CLI JSON"),
                    !output.status.success(),
                )
            })
            .await
            .expect("CLI process");
            return (result.0["result"]["structuredContent"].clone(), result.1);
        }
        let body = if surface == "api" {
            args
        } else {
            json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"call_action","arguments":args,"_meta":{"norn/idempotencyKey":key}}})
        };
        let request = http::Request::builder()
            .method("POST")
            .uri(if surface == "api" {
                "/v1/actions"
            } else {
                "/mcp"
            })
            .header("x-norn-org-id", "org_demo")
            .header("x-norn-environment-id", "dev")
            .header("x-norn-actor", actor)
            .body(axum::body::Body::from(body.to_string()))
            .expect("request");
        let response = app.clone().oneshot(request).await.expect("response");
        let failed = !response.status().is_success();
        let bytes = axum::body::to_bytes(response.into_body(), 1_000_000)
            .await
            .expect("response body");
        let value: Value = serde_json::from_slice(&bytes).expect("JSON");
        if surface == "api" || failed {
            (value, failed)
        } else {
            (
                value["result"]["structuredContent"].clone(),
                value["result"]["isError"] == true,
            )
        }
    }
    fn dump(host: &DevHost<NativeEngine>) -> BTreeMap<String, Vec<String>> {
        host.cell()
            .expect("cell")
            .store()
            .read()
            .dump()
            .expect("dump")
    }
    #[tokio::test]
    #[ignore = "requires the built remote CLI; run CP1-surface Boat command in README"]
    async fn surface_cp1_fifo_and_surface_deny_1() {
        let mut states = Vec::new();
        let mut refusals = Vec::new();
        for surface in ["api", "mcp", "cli"] {
            let host = Arc::new(host());
            let app = router(Arc::clone(&host));
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
                .await
                .expect("listener");
            let url = format!("http://{}", listener.local_addr().expect("address"));
            let server_app = app.clone();
            let server =
                tokio::spawn(
                    async move { axum::serve(listener, server_app).await.expect("serve") },
                );
            for (action, id, key) in [
                ("stock.receipt.submit", "pr", "receive"),
                ("stock.delivery.submit", "dn", "deliver"),
            ] {
                let (result, failed) = invoke(&app, surface, &url, "demo", [action, id, key]).await;
                assert!(!failed, "{surface}: {result}");
            }
            let before = dump(&host);
            let (refused, failed) = invoke(
                &app,
                surface,
                &url,
                "demo",
                ["stock.delivery.submit", "over", "over"],
            )
            .await;
            assert!(failed, "POST-OVERDELIVERY {surface}");
            assert!(
                refused["message"]
                    .as_str()
                    .expect("readable")
                    .contains("available"),
                "{refused}"
            );
            let (denied, failed) = invoke(
                &app,
                surface,
                &url,
                "forbidden",
                ["stock.receipt.submit", "pr", "denied"],
            )
            .await;
            assert!(failed, "SURFACE-DENY-1 {surface}");
            assert_eq!(denied["code"], "authz.denied");
            assert_eq!(before, dump(&host), "SURFACE-DENY-1/POST-NO-WRITES");
            let cell = host.cell().expect("cell");
            let partition = cell
                .store()
                .read()
                .list("main", "stock.Partition", 10)
                .expect("partition");
            assert_eq!(partition.len(), 1);
            assert_eq!(
                partition[0].values.get("quantity"),
                Some(&norn_ir::RuntimeValue::from_json(json!(1)).expect("quantity"))
            );
            assert_eq!(
                partition[0].values.get("value"),
                Some(&norn_ir::RuntimeValue::from_json(json!(20)).expect("value"))
            );
            let legs = cell
                .store()
                .read()
                .list("main", "stock.GLLeg", 10)
                .expect("GL");
            let mut amounts = BTreeMap::<String, i64>::new();
            for row in legs {
                let leg: norn_ir::posting::AccountingLegIr =
                    norn_ir::runtime::decode(row.values.get("leg").expect("leg"))
                        .expect("typed leg");
                *amounts.entry(leg.account.account_id).or_default() +=
                    leg.amount_unscaled.parse::<i64>().expect("cents");
            }
            assert_eq!(
                amounts,
                BTreeMap::from([
                    ("cogs".into(), 6000),
                    ("stock".into(), 2000),
                    ("srbnb".into(), -8000)
                ]),
                "POST-GL receipt 80 / COGS 60 / remaining 20"
            );
            states.push(before);
            refusals.push((refused, denied));
            server.abort();
        }
        assert!(
            states.windows(2).all(|s| s[0] == s[1]),
            "SURFACE-ACTION-1 committed state parity"
        );
        assert!(
            refusals.windows(2).all(|r| r[0] == r[1]),
            "SURFACE-ACTION-1 refusal parity"
        );
    }
}
