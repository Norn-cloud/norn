//! SOURCE-CAPTURE-1 pinned regressions (Lane B3):
//!
//! 1. Pull capture failure immediately before `_norn_source_cursor` persistence
//!    rolls back both inbox capture and cursor advancement atomically; retry
//!    succeeds with zero lost rows.
//! 2. Duplicate delivery of an already-captured complete page (including multi-run
//!    split pages and later `extracted_at` timestamps) is idempotent with zero
//!    double-capture or cursor corruption, while tampered payloads (even with
//!    identical observation IDs) and conflicting cursors refuse.
//! 3. Empty terminal pages advance `_norn_source_cursor` atomically in SQLite.
//! 4. Mid-page framing failure rolls back without advancing cursor, while an
//!    unconditionally selected Odoo `product.product` mapping refusal is
//!    captured durably as quarantined evidence alongside cursor advancement.
//! 5. Expired leases racing a newer sync are fenced by monotonic generation on
//!    capture and stale release, and a fenced run never overwrites
//!    `source.Connection` written by the winning run.
//! 6. Legacy cell upgrade: a cell with populated `source.Connection.watermark`
//!    and empty `_norn_source_cursor` seeds legacy cursors under its active
//!    lease and advances cleanly without cursor conflict.
//! 7. Post-cursor-commit store fault + SQLite file reopen: rows stranded in
//!    `_norn_inbox` / `_norn_source_pending` after cursor persistence are
//!    recovered and landed into canonical tables on the next sync even when the
//!    remote source returns empty pages.

#[cfg(not(target_arch = "wasm32"))]
#[allow(
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    reason = "SOURCE-CAPTURE-1 atomic source capture regressions"
)]
mod tests {
    use std::collections::{BTreeMap, BTreeSet};
    use std::path::{Path, PathBuf};
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

    use norn_api::host::{DevHost, SourceCaptureError};
    use norn_api::pull::{Limits, Pull, PullProfile, Watermark, observation};
    use norn_api::scope::CellScope;
    use norn_api::sources::{RUN_LEASE_MS, Sources, Step, Trigger};
    use norn_cell::engine::{NativeEngine, ReadIsolation, SqlEngine, SqlValue};
    use norn_cell::mapping_bundle::MappingInput;
    use norn_cell::source_cursor::CursorAdvanceSpec;
    use norn_cell::{
        CellError, CellStore, InstalledDefinitionInput, MappingOutcome, Model, StoreOptions,
    };
    use norn_checker::EntityArtifactSetContext;
    use norn_checker::receipt::ReceiptSources;
    use norn_ir::{ArtifactEnvelope, ArtifactKind, EntityRoot, ResourceLimitProfile, RuntimeValue};
    use norn_ui_checker::check_artifact_set_with_receipt;
    use serde_json::{Value, json};

    const RECORDED_AT: &str = "2026-11-12T12:00:00Z";
    const T0: u64 = 1_794_484_800_000;
    const SANDBOX_HOSTS: &str = "erpnext-rest,odoo-rest,cursor-mock";

    /// Fault-injecting SQLite engine wrapper:
    /// - `fail_cursor_write`: fails `_norn_source_cursor` writes inside
    ///   `capture_source_page`'s `write_tx` (testing capture rollback).
    /// - `fail_ledger_write`: fails `_norn_ledger` writes during post-capture
    ///   canonical commit (testing post-cursor-commit recovery after reopen).
    struct FaultEngine {
        inner: NativeEngine,
        fail_cursor_write: Arc<AtomicBool>,
        fail_ledger_write: Arc<AtomicBool>,
    }

    impl SqlEngine for FaultEngine {
        fn execute(&self, sql: &str, params: &[SqlValue]) -> Result<(), CellError> {
            if self.fail_cursor_write.load(Ordering::Acquire) && sql.contains("_norn_source_cursor")
            {
                return Err(CellError::Engine(
                    "injected fault immediately before cursor persistence".to_owned(),
                ));
            }
            if self.fail_ledger_write.load(Ordering::Acquire) && sql.contains("_norn_ledger") {
                return Err(CellError::Engine(
                    "injected fault after cursor commit during canonical processing".to_owned(),
                ));
            }
            self.inner.execute(sql, params)
        }

        fn query(&self, sql: &str, params: &[SqlValue]) -> Result<Vec<Vec<SqlValue>>, CellError> {
            self.inner.query(sql, params)
        }

        fn execute_batch(&self, sql: &str) -> Result<(), CellError> {
            self.inner.execute_batch(sql)
        }

        fn transaction<T, X: From<CellError>>(
            &self,
            f: impl FnOnce() -> Result<T, X>,
        ) -> Result<T, X> {
            self.inner.transaction(f)
        }

        fn platform_table_globs(&self) -> &'static [&'static str] {
            self.inner.platform_table_globs()
        }

        fn read_isolation(&self) -> ReadIsolation {
            self.inner.read_isolation()
        }

        fn read_cache_revision(&self) -> Result<Option<(u64, u64)>, CellError> {
            self.inner.read_cache_revision()
        }

        fn external_data_version(&self) -> Result<Option<u64>, CellError> {
            self.inner.external_data_version()
        }

        fn admitted(&self) -> Result<(), CellError> {
            self.inner.admitted()
        }

        fn definition_writes<T>(
            &self,
            f: impl FnOnce() -> Result<T, CellError>,
        ) -> Result<T, CellError> {
            self.inner.definition_writes(f)
        }
    }

    struct TempDb(PathBuf);

    impl TempDb {
        fn new(label: &str) -> Self {
            static COUNTER: AtomicU64 = AtomicU64::new(0);
            let id = COUNTER.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "norn-b3-{label}-{}-{id}.sqlite",
                std::process::id()
            ));
            drop(std::fs::remove_file(&path));
            Self(path)
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TempDb {
        fn drop(&mut self) {
            drop(std::fs::remove_file(&self.0));
            drop(std::fs::remove_file(format!("{}-wal", self.0.display())));
            drop(std::fs::remove_file(format!("{}-shm", self.0.display())));
        }
    }

    fn root() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
    }

    fn load_artifact(path: &str) -> ArtifactEnvelope {
        serde_json::from_str(&std::fs::read_to_string(root().join(path)).expect(path)).expect(path)
    }

    fn cell_scope() -> CellScope {
        CellScope {
            org_id: "org_demo".into(),
            environment_id: "dev".into(),
        }
    }

    fn rest_artifacts(extra: &[ArtifactEnvelope]) -> Vec<ArtifactEnvelope> {
        let mut set: Vec<ArtifactEnvelope> = serde_json::from_str(
            &std::fs::read_to_string(
                root().join("fixtures/demo/explorer/three-systems.definitions.json"),
            )
            .expect("retained catalog"),
        )
        .expect("artifacts");
        for system in ["erpnext", "odoo"] {
            for kind in ["source", "mapping", "connector"] {
                set.push(load_artifact(&format!(
                    "fixtures/packs/{system}-rest/artifacts/{system}.rest.{kind}.json"
                )));
            }
        }
        for name in [
            "source.Synced.interface",
            "source.Connection.entity",
            "source.SyncRun.entity",
            "source.Upload.entity",
            "norn.connect.source",
            "norn.connect.mapping",
        ] {
            set.push(load_artifact(&format!(
                "registry/packs/sources/artifacts/{name}.json"
            )));
        }
        set.extend_from_slice(extra);
        set
    }

    fn admit_and_model(artifacts: &[ArtifactEnvelope]) -> (Model, InstalledDefinitionInput) {
        let (base, candidate): (Vec<_>, Vec<_>) = artifacts
            .iter()
            .cloned()
            .partition(|a| a.artifact_id.starts_with("core.") || a.artifact_id.starts_with("std."));
        let to_rv = |set: &[ArtifactEnvelope]| -> Vec<RuntimeValue> {
            set.iter()
                .map(|a| {
                    RuntimeValue::from_json(serde_json::to_value(a).expect("wire")).expect("rv")
                })
                .collect()
        };
        let sources = ReceiptSources::new(&to_rv(&candidate), &to_rv(&base)).expect("sources");
        let checked = check_artifact_set_with_receipt(
            &sources,
            &EntityArtifactSetContext {
                registry: &BTreeSet::new(),
                base: &base,
                tenant: None,
            },
            &ResourceLimitProfile::norn_limits_v1(),
        )
        .expect("checked");
        assert!(checked.receipt.admitted, "{:#?}", checked.diagnostics);
        let (mut objects, mut interfaces, mut properties) = (Vec::new(), Vec::new(), Vec::new());
        for artifact in artifacts
            .iter()
            .filter(|a| a.kind() == ArtifactKind::Entity)
        {
            let root: EntityRoot = serde_json::from_value(
                serde_json::to_value(artifact.payload.to_json_value()).expect("payload"),
            )
            .expect("entity root");
            match root {
                EntityRoot::V2(root) => objects.push(root.object_type),
                EntityRoot::V2Interface(root) => interfaces.push(root.interface),
                EntityRoot::V2PropertyTypes(root) => properties.extend(root.property_types),
                EntityRoot::V2Extension(_) | EntityRoot::V2Markings(_) | EntityRoot::V1(_) => {
                    panic!("unexpected entity root")
                }
            }
        }
        let model = Model::from_definitions_with_interfaces(&objects, &properties, &interfaces)
            .expect("model")
            .with_governing_laws(artifacts)
            .expect("laws");
        let input = InstalledDefinitionInput {
            canonical_set: String::from_utf8(
                RuntimeValue::from_json(serde_json::to_value(artifacts).expect("set"))
                    .expect("rv")
                    .canonical_json()
                    .expect("canonical"),
            )
            .expect("utf8"),
            registry: vec![],
            receipt_sha256: checked.receipt.content_sha256().expect("digest"),
            admission_receipt: Some(
                String::from_utf8(checked.receipt.canonical_json().expect("receipt"))
                    .expect("utf8"),
            ),
            readmission: None,
            diff: json!({}),
            installed_at: 1,
            layout: None,
        };
        (model, input)
    }

    fn open_faultable_host(
        artifacts: &[ArtifactEnvelope],
        path: Option<&Path>,
    ) -> (DevHost<FaultEngine>, Arc<AtomicBool>, Arc<AtomicBool>) {
        let (model, input) = admit_and_model(artifacts);
        let fail_cursor_write = Arc::new(AtomicBool::new(false));
        let fail_ledger_write = Arc::new(AtomicBool::new(false));
        let inner = match path {
            Some(path) => NativeEngine::open(path, StoreOptions::default()).expect("sqlite file"),
            None => NativeEngine::open_in_memory().expect("sqlite"),
        };
        let engine = FaultEngine {
            inner,
            fail_cursor_write: Arc::clone(&fail_cursor_write),
            fail_ledger_write: Arc::clone(&fail_ledger_write),
        };
        let mut store = CellStore::from_engine(engine, model.clone()).expect("store");
        let scope = cell_scope();
        store
            .install_definitions(model, input, Some(0), &scope.key())
            .expect("install");
        let host = DevHost::from_store(store, scope, |_| BTreeMap::new()).expect("host");
        (host, fail_cursor_write, fail_ledger_write)
    }

    fn reopen_native_host(artifacts: &[ArtifactEnvelope], path: &Path) -> DevHost<NativeEngine> {
        let (model, _input) = admit_and_model(artifacts);
        let engine = NativeEngine::open(path, StoreOptions::default()).expect("reopen sqlite");
        let store = CellStore::from_engine(engine, model).expect("reopen store");
        DevHost::from_store(store, cell_scope(), |_| BTreeMap::new()).expect("reopen host")
    }

    fn open_native_host(artifacts: &[ArtifactEnvelope]) -> DevHost<NativeEngine> {
        let (model, input) = admit_and_model(artifacts);
        let mut store = CellStore::open_in_memory(model.clone()).expect("store");
        let scope = cell_scope();
        store
            .install_definitions(model, input, Some(0), &scope.key())
            .expect("install");
        DevHost::from_store(store, scope, |_| BTreeMap::new()).expect("host")
    }

    fn profile_of(system: &str) -> PullProfile {
        let connector = load_artifact(&format!(
            "fixtures/packs/{system}-rest/artifacts/{system}.rest.connector.json"
        ));
        let payload = serde_json::to_value(connector.payload.to_json_value()).expect("payload");
        PullProfile::from_connector(&payload)
            .expect("valid")
            .expect("pull profile")
    }

    fn recorded_pages(system: &str) -> BTreeMap<String, (u16, Value)> {
        let doc: Value = serde_json::from_str(
            &std::fs::read_to_string(
                root().join(format!("fixtures/connectors/rest/{system}.responses.json")),
            )
            .expect("responses"),
        )
        .expect("json");
        doc["pages"]
            .as_array()
            .expect("pages")
            .iter()
            .map(|p| {
                (
                    p["dataset"].as_str().expect("dataset").to_owned(),
                    (
                        u16::try_from(p["status"].as_u64().expect("status")).expect("u16"),
                        p["body"].clone(),
                    ),
                )
            })
            .collect()
    }

    fn empty_page(profile: &PullProfile, dataset: &str) -> Value {
        let feed = profile
            .feeds
            .iter()
            .find(|f| f.dataset == dataset)
            .expect("feed");
        let mut body = json!({});
        let mut at = &mut body;
        let parts: Vec<&str> = feed.items_path.trim_start_matches('/').split('/').collect();
        for (i, part) in parts.iter().enumerate() {
            at[*part] = if i.saturating_add(1) == parts.len() {
                json!([])
            } else {
                json!({})
            };
            at = &mut at[*part];
        }
        body
    }

    fn rest_sources(config: &str) -> Sources {
        let secrets = BTreeMap::from([
            (
                "NORN_SECRET_ERPNEXT_REST_TOKEN".to_owned(),
                "key:secret".to_owned(),
            ),
            (
                "NORN_SECRET_ODOO_REST_API_KEY".to_owned(),
                "odoo-key".to_owned(),
            ),
        ]);
        Sources::with_allowed_hosts(&cell_scope(), config, &secrets, SANDBOX_HOSTS)
            .expect("sources config")
    }

    const ERPNEXT_CONFIG: &str = r#"[
      {"id": "erpnext.rest", "system": "erpnext", "label": "ERPNext REST",
       "connector": "erpnext.rest.connector", "mapping": "erpnext.rest.mapping",
       "instance": "sandbox", "baseUrl": "http://erpnext-rest:8000",
       "orgId": "org_demo", "environmentId": "dev", "secretRefs": ["erpnext_rest_token"],
       "headers": {"X-Frappe-Site-Name": "frontend"}},
      {"id": "odoo.rest", "system": "odoo", "label": "Odoo REST",
       "connector": "odoo.rest.connector", "mapping": "odoo.rest.mapping",
       "instance": "sandbox", "baseUrl": "http://odoo-rest:8069",
       "orgId": "org_demo", "environmentId": "dev", "secretRefs": ["odoo_rest_api_key"]}
    ]"#;

    fn sealed_page_for(
        system: &str,
        dataset: &str,
        extracted_at_ms: u64,
    ) -> (
        Vec<(norn_kernel::Observed<RuntimeValue>, MappingInput)>,
        Watermark,
    ) {
        let mut profile = profile_of(system);
        let feed = profile
            .feeds
            .iter()
            .find(|f| f.dataset == dataset)
            .cloned()
            .expect("requested dataset feed");
        profile.feeds = vec![feed];
        let mut pages = recorded_pages(system);
        let mut pull = Pull::new(profile.clone(), BTreeMap::new(), Limits::default());
        let req = pull.next_request().expect("request");
        let (status, body) = pages.remove(&req.dataset).expect("recorded page");
        let accepted = pull
            .accept(status, body.to_string().as_bytes())
            .expect("accepted");
        let next_wm = accepted.advance.next.clone().expect("keyset next");
        let rows = accepted
            .rows
            .iter()
            .map(|row| {
                let raw = observation(&profile, row, extracted_at_ms).expect("sealed");
                (
                    raw,
                    MappingInput {
                        mapping: format!("{system}.rest.mapping"),
                        dataset: row.dataset.clone(),
                        connector_instance: "sandbox".into(),
                        partition: row.dataset.clone(),
                        recorded_at: norn_ir::provenance::UtcTimestamp(RECORDED_AT.into()),
                    },
                )
            })
            .collect();
        (rows, next_wm)
    }

    fn run_erpnext_sync<E: SqlEngine>(
        host: &DevHost<E>,
        sources: &mut Sources,
        profile: &PullProfile,
        now_ms: u64,
        mut pages: BTreeMap<String, (u16, Value)>,
    ) -> norn_api::sources::RunSummary {
        let mut step = sources.step(host, "erpnext.rest", None, now_ms);
        loop {
            match step {
                Step::Done(summary) => return summary,
                Step::Continue => step = sources.step(host, "erpnext.rest", None, now_ms),
                Step::Request(req) => {
                    let feed = profile
                        .feeds
                        .iter()
                        .find(|f| req.url.contains(f.request.path.as_str()))
                        .expect("feed");
                    let (st, b) = pages
                        .remove(&feed.dataset)
                        .unwrap_or_else(|| (200, empty_page(profile, &feed.dataset)));
                    step = sources.step(
                        host,
                        "erpnext.rest",
                        Some((st, b.to_string().as_bytes())),
                        now_ms,
                    );
                }
            }
        }
    }

    /// Regression 1: pull capture failure immediately before `_norn_source_cursor`
    /// persistence rolls back BOTH inbox capture and cursor advancement; retrying
    /// after clearing the fault lands every row cleanly with zero loss.
    #[test]
    fn pull_capture_failure_immediately_before_cursor_persistence_rolls_back_and_recovers() {
        let artifacts = rest_artifacts(&[]);
        let (host, fail_cursor, _fail_ledger) = open_faultable_host(&artifacts, None);
        let profile = profile_of("erpnext");
        let mut sources = rest_sources(ERPNEXT_CONFIG);
        sources
            .claim("erpnext.rest", Trigger::Manual, "user:owner@demo", T0)
            .expect("claim");
        let Step::Request(first_req) = sources.step(&host, "erpnext.rest", None, T0) else {
            panic!("expected first request");
        };
        let feed = profile
            .feeds
            .iter()
            .find(|f| first_req.url.contains(f.request.path.as_str()))
            .expect("feed");
        let (status, body) = recorded_pages("erpnext")
            .remove(&feed.dataset)
            .expect("recorded page");

        // Arm fault immediately before `_norn_source_cursor` write inside `write_tx`.
        fail_cursor.store(true, Ordering::Release);
        let Step::Done(failed) = sources.step(
            &host,
            "erpnext.rest",
            Some((status, body.to_string().as_bytes())),
            T0.saturating_add(1),
        ) else {
            panic!("cursor persistence fault must fail the run");
        };
        assert_eq!(failed.outcome, "failed");
        assert_eq!(
            failed.error.as_ref().map(|(c, _)| c.as_str()),
            Some("source.ingest")
        );
        assert!(
            host.source_cursors("erpnext.rest")
                .expect("cursors")
                .is_empty(),
            "cursor must not advance when persistence faults"
        );
        assert!(
            host.pending_source_captures("erpnext.rest")
                .expect("pending")
                .is_empty(),
            "pending capture must roll back with cursor fault"
        );
        let conn = host
            .host_object("source.Connection", "erpnext.rest")
            .expect("read")
            .expect("connection");
        assert_eq!(
            conn.get("watermark"),
            Some(&RuntimeValue::String("{}".into()))
        );

        // Clear fault and re-sync from scratch with a fresh `Sources` instance.
        fail_cursor.store(false, Ordering::Release);
        let mut retry = rest_sources(ERPNEXT_CONFIG);
        let retry_t = T0.saturating_add(10);
        retry
            .claim("erpnext.rest", Trigger::Manual, "user:owner@demo", retry_t)
            .expect("re-claim");
        let summary = run_erpnext_sync(
            &host,
            &mut retry,
            &profile,
            retry_t,
            recorded_pages("erpnext"),
        );
        assert_eq!(summary.outcome, "ok", "{summary:#?}");
        assert!(summary.rows_read > 0);
        assert_eq!(summary.rows_landed, summary.rows_read);
        assert_eq!(
            host.source_cursors("erpnext.rest").expect("cursors").len(),
            profile.feeds.len()
        );
    }

    /// Regression 2: exact duplicate delivery of a complete (multi-run) page is
    /// idempotent even at a later `extracted_at` timestamp, while tampered row
    /// payloads (keeping identical observation IDs) and conflicting cursors
    /// refuse with `CursorConflict`.
    #[test]
    fn duplicate_page_delivery_is_idempotent_and_tampered_or_conflicting_refuse() {
        let host = open_native_host(&rest_artifacts(&[]));
        // ERPNext `Party` has 101 rows (> PAGE_ROWS=100), so it exercises
        // multi-run atomic capture and complete-page replay in one shot.
        let dataset = "Party".to_owned();
        let (rows_t0, next_wm) = sealed_page_for("erpnext", &dataset, T0);
        let (rows_later, _) = sealed_page_for("erpnext", &dataset, T0.saturating_add(5_000));
        assert!(rows_t0.len() > 100, "need split multi-run page");
        let lease = host
            .acquire_source_lease("erpnext.rest", "run-1", T0, RUN_LEASE_MS)
            .expect("lease");
        let advance = CursorAdvanceSpec {
            connection_id: "erpnext.rest".into(),
            dataset: dataset.clone(),
            lease_id: lease.lease_id.clone(),
            generation: lease.generation,
            now_ms: T0.saturating_add(1),
            expected_previous_cursor: None,
            next_cursor: Some(next_wm.cursor_json().expect("json")),
            renew_ttl_ms: Some(RUN_LEASE_MS),
        };
        let first = host
            .ingest_source_page(&rows_t0, &advance)
            .expect("first delivery");
        assert!(first.iter().all(|o| o.outcome == MappingOutcome::Landed));

        // Exact retry of the complete multi-run page at a later wall clock.
        let duplicate = host
            .ingest_source_page(&rows_later, &advance)
            .expect("duplicate delivery");
        assert_eq!(duplicate.len(), first.len());
        assert!(
            duplicate
                .iter()
                .all(|o| o.outcome == MappingOutcome::AlreadyLanded),
            "{duplicate:#?}"
        );
        assert_eq!(
            duplicate
                .iter()
                .map(|o| o.observation_id.as_str())
                .collect::<Vec<_>>(),
            first
                .iter()
                .map(|o| o.observation_id.as_str())
                .collect::<Vec<_>>(),
            "replay with later extracted_at must return original observation ids without creating new inbox rows"
        );
        let stored = host.source_cursors("erpnext.rest").expect("cursors");
        assert_eq!(
            stored[&dataset].cursor_json,
            next_wm.cursor_json().expect("json")
        );

        // Tamper a single row's payload while keeping the exact same observation IDs.
        let mut tampered = rows_t0.clone();
        if let RuntimeValue::Object(obj) = &mut tampered[0].0.input
            && let Some(RuntimeValue::Object(payload)) = obj.get_mut("payload")
        {
            payload.insert(
                "customer_name".into(),
                RuntimeValue::String("Tampered Owner".into()),
            );
        }
        assert!(
            matches!(
                host.ingest_source_page(&tampered, &advance),
                Err(SourceCaptureError::CursorConflict(_))
            ),
            "tampered payload with identical observation IDs must fail content-bound digest"
        );

        let conflicting = CursorAdvanceSpec {
            next_cursor: Some(
                Watermark {
                    version: "2099-01-01 00:00:00".into(),
                    id: norn_ir::RuntimeValue::from("CONFLICT"),
                }
                .cursor_json()
                .expect("json"),
            ),
            ..advance
        };
        assert!(matches!(
            host.ingest_source_page(&rows_t0, &conflicting),
            Err(SourceCaptureError::CursorConflict(_))
        ));
    }

    /// Regression 3: an empty terminal page on a cursor-paged feed atomically
    /// advances `_norn_source_cursor` in SQLite even with zero rows.
    #[test]
    fn empty_terminal_page_advances_cursor_atomically() {
        let mut cursor_doc = serde_json::to_value(load_artifact(
            "fixtures/packs/erpnext-rest/artifacts/erpnext.rest.connector.json",
        ))
        .expect("connector json");
        let mut profile = PullProfile::from_connector(&cursor_doc["payload"])
            .expect("valid")
            .expect("profile");
        let mut feed = profile.feeds[0].clone();
        feed.paging = norn_api::pull::Paging::Cursor {
            size: 2,
            next_cursor_path: "/pageInfo/endCursor".into(),
            has_next_path: "/pageInfo/hasNextPage".into(),
        };
        feed.request
            .query
            .insert("after".into(), "{{cursor}}".into());
        profile.feeds = vec![feed.clone()];
        cursor_doc["artifactId"] = json!("erpnext.cursor.connector");
        cursor_doc["name"] = json!("erpnext.cursor.connector");
        cursor_doc["payload"]["extensions"]["norn.pull/1"] =
            serde_json::to_value(&profile).expect("json");
        let cursor_connector: ArtifactEnvelope =
            serde_json::from_value(cursor_doc).expect("connector envelope");

        let host = open_native_host(&rest_artifacts(&[cursor_connector]));
        let config = serde_json::json!([{
            "id": "erpnext.cursor", "system": "erpnext", "label": "Cursor ERPNext",
            "connector": "erpnext.cursor.connector", "mapping": "erpnext.rest.mapping",
            "instance": "sandbox", "baseUrl": "http://cursor-mock:8000",
            "orgId": "org_demo", "environmentId": "dev", "secretRefs": ["erpnext_rest_token"]
        }])
        .to_string();
        let mut sources = rest_sources(&config);
        sources
            .claim("erpnext.cursor", Trigger::Manual, "user:owner@demo", T0)
            .expect("claim");
        let Step::Request(_) = sources.step(&host, "erpnext.cursor", None, T0) else {
            panic!("first request");
        };
        let (_, mut first_body) = recorded_pages("erpnext")
            .remove(&feed.dataset)
            .expect("page");
        let expected_greatest = first_body["data"]
            .as_array()
            .expect("rows")
            .iter()
            .filter_map(|r| r.pointer(&feed.version_path).and_then(Value::as_str))
            .max()
            .expect("greatest version")
            .to_owned();
        first_body["pageInfo"] = json!({"hasNextPage": true, "endCursor": "cur-1"});

        let mut step = sources.step(
            &host,
            "erpnext.cursor",
            Some((200, first_body.to_string().as_bytes())),
            T0.saturating_add(1),
        );
        while matches!(step, Step::Continue) {
            step = sources.step(&host, "erpnext.cursor", None, T0.saturating_add(1));
        }
        let Step::Request(page2_req) = step else {
            panic!("expected page 2 request, got {step:?}");
        };
        assert!(page2_req.url.contains("after=cur-1"), "{}", page2_req.url);
        assert!(
            host.source_cursors("erpnext.cursor")
                .expect("cursors")
                .is_empty(),
            "non-terminal cursor page must not advance durable cursor"
        );

        let terminal =
            json!({"data": [], "pageInfo": {"hasNextPage": false, "endCursor": "cur-2"}});
        let Step::Done(summary) = sources.step(
            &host,
            "erpnext.cursor",
            Some((200, terminal.to_string().as_bytes())),
            T0.saturating_add(2),
        ) else {
            panic!("empty terminal page must complete the run");
        };
        assert_eq!(summary.outcome, "ok", "{summary:#?}");
        let cursors = host.source_cursors("erpnext.cursor").expect("cursors");
        let wm: Watermark = serde_json::from_str(&cursors[&feed.dataset].cursor_json).expect("wm");
        assert_eq!(wm.version, expected_greatest);
    }

    /// Regression 4: intra-page framing failure rolls back without advancing
    /// cursor, while an unconditionally selected Odoo `product.product` mapping
    /// refusal is captured durably as quarantined evidence alongside atomic
    /// cursor advancement.
    #[test]
    fn mid_page_framing_failure_rolls_back_and_quarantined_rows_capture_with_cursor() {
        let host = open_native_host(&rest_artifacts(&[]));
        let dataset = "product.product".to_owned();
        let (rows, next_wm) = sealed_page_for("odoo", &dataset, T0);
        assert!(rows.len() >= 2, "need multi-row product.product page");
        let lease = host
            .acquire_source_lease("odoo.rest", "run-mid", T0, RUN_LEASE_MS)
            .expect("lease");
        let advance = CursorAdvanceSpec {
            connection_id: "odoo.rest".into(),
            dataset: dataset.clone(),
            lease_id: lease.lease_id.clone(),
            generation: lease.generation,
            now_ms: T0.saturating_add(1),
            expected_previous_cursor: None,
            next_cursor: Some(next_wm.cursor_json().expect("json")),
            renew_ttl_ms: Some(RUN_LEASE_MS),
        };

        let mut malformed = rows.clone();
        if let RuntimeValue::Object(map) = &mut malformed[1].0.input {
            map.remove("payload");
        }
        assert!(
            host.ingest_source_page(&malformed, &advance).is_err(),
            "mid-page framing error must fail"
        );
        assert!(
            host.source_cursors("odoo.rest")
                .expect("cursors")
                .is_empty(),
            "mid-page framing failure must not advance cursor"
        );

        let mut quarantined_row = rows[0].clone();
        if let RuntimeValue::Object(obj) = &mut quarantined_row.0.input {
            if let Some(RuntimeValue::Object(payload)) = obj.get_mut("payload") {
                payload.insert("x_norn_ref".into(), RuntimeValue::Null);
            }
            let digest = norn_ir::canonical::HashProfile::Norn1
                .hash_json(serde_json::to_string(&obj["payload"]).expect("json"))
                .expect("digest");
            obj.insert("payload_digest".into(), RuntimeValue::String(digest));
            obj.insert(
                "event_id".into(),
                RuntimeValue::String("quarantine-product-1".into()),
            );
        }
        let outcomes = host
            .ingest_source_page(&[quarantined_row], &advance)
            .expect("quarantine captured");
        assert!(
            matches!(
                outcomes[0].outcome,
                MappingOutcome::Quarantined(ref reasons) if !reasons.is_empty()
            ),
            "expected quarantined outcome, got {:?}",
            outcomes[0].outcome
        );
        assert!(
            host.source_cursors("odoo.rest")
                .expect("cursors")
                .contains_key(&dataset),
            "quarantined page must still advance durable cursor"
        );
    }

    /// Regression 5: an expired lease racing a newer sync is fenced by
    /// monotonic generation on capture and stale release, and a fenced run
    /// never overwrites `source.Connection` written by the winning run.
    #[test]
    fn expired_lease_racing_a_newer_sync_is_fenced_and_does_not_clobber_connection() {
        let host = open_native_host(&rest_artifacts(&[]));
        let profile = profile_of("erpnext");
        let mut stale_sources = rest_sources(ERPNEXT_CONFIG);
        stale_sources
            .claim("erpnext.rest", Trigger::Manual, "user:stale@demo", T0)
            .expect("claim A");
        let Step::Request(req_a) = stale_sources.step(&host, "erpnext.rest", None, T0) else {
            panic!("run A first request");
        };
        let lease_a = host
            .source_lease("erpnext.rest")
            .expect("lease")
            .expect("run A lease");
        assert_eq!(lease_a.generation, 1);
        assert!(matches!(
            host.acquire_source_lease(
                "erpnext.rest",
                "run-early",
                T0.saturating_add(1_000),
                RUN_LEASE_MS
            ),
            Err(SourceCaptureError::LeaseFenced(_))
        ));

        // After lease A expires, worker B takes over (generation 2) and completes.
        let t_b = T0.saturating_add(RUN_LEASE_MS);
        let mut winner_sources = rest_sources(ERPNEXT_CONFIG);
        winner_sources
            .claim("erpnext.rest", Trigger::Manual, "user:winner@demo", t_b)
            .expect("claim B");
        let summary_b = run_erpnext_sync(
            &host,
            &mut winner_sources,
            &profile,
            t_b,
            recorded_pages("erpnext"),
        );
        assert_eq!(summary_b.outcome, "ok", "{summary_b:#?}");
        let conn_after_b = host
            .host_object("source.Connection", "erpnext.rest")
            .expect("read")
            .expect("connection after B");
        assert_eq!(
            conn_after_b.get("status"),
            Some(&RuntimeValue::String("ok".into()))
        );
        let wm_after_b = conn_after_b.get("watermark").cloned();
        assert_ne!(wm_after_b, Some(RuntimeValue::String("{}".into())));

        // Stale run A now receives its old HTTP response and must be fenced
        // without clobbering worker B's `source.Connection` or cursors.
        let feed = profile
            .feeds
            .iter()
            .find(|f| req_a.url.contains(f.request.path.as_str()))
            .expect("feed");
        let (st, body) = recorded_pages("erpnext")
            .remove(&feed.dataset)
            .expect("page");
        let Step::Done(summary_a) = stale_sources.step(
            &host,
            "erpnext.rest",
            Some((st, body.to_string().as_bytes())),
            t_b.saturating_add(50),
        ) else {
            panic!("stale run A must fail fenced");
        };
        assert_eq!(summary_a.outcome, "failed");
        assert_eq!(
            summary_a.error.as_ref().map(|(c, _)| c.as_str()),
            Some("source.lease_fenced")
        );
        assert!(
            !host
                .release_source_lease("erpnext.rest", &lease_a.lease_id, lease_a.generation)
                .expect("stale release")
        );
        let conn_after_a = host
            .host_object("source.Connection", "erpnext.rest")
            .expect("read")
            .expect("connection after stale A");
        assert_eq!(
            conn_after_a.get("status"),
            Some(&RuntimeValue::String("ok".into())),
            "fenced stale run must not overwrite winning connection status"
        );
        assert_eq!(
            conn_after_a.get("watermark"),
            wm_after_b.as_ref(),
            "fenced stale run must not overwrite winning connection watermark"
        );
    }

    /// Regression 6: upgrading from a populated legacy cell whose
    /// `source.Connection.watermark` is populated while `_norn_source_cursor`
    /// is empty seeds legacy cursors under the active lease and advances cleanly
    /// without `source.cursor_conflict`.
    #[test]
    fn legacy_cell_watermark_seeds_source_cursor_without_conflict() {
        let host = open_native_host(&rest_artifacts(&[]));
        let profile = profile_of("erpnext");
        let legacy_wm = BTreeMap::from([(
            "Party".to_owned(),
            Watermark {
                version: "2026-01-01 00:00:00".into(),
                id: norn_ir::RuntimeValue::String("CUST-LEGACY".into()),
            },
        )]);
        let legacy_json = serde_json::to_string(&legacy_wm).expect("json");
        let connect_profile = PullProfile {
            source: "norn-connect".into(),
            pipeline: "norn-connect/1".into(),
            feeds: Vec::new(),
            token: None,
        };
        let legacy_conn_row = norn_api::pull::Row {
            dataset: "connection".into(),
            source_id: "erpnext.rest".into(),
            version: "legacy/end".into(),
            payload: norn_ir::runtime_object! {
                "id" => "erpnext.rest", "system" => "erpnext", "label" => "ERPNext REST",
                "target" => "http://erpnext-rest:8000", "auth" => "api_key", "status" => "ok",
                "last_sync_started_at" => RECORDED_AT, "last_sync_ended_at" => RECORDED_AT,
                "last_trigger" => "manual", "watermark" => legacy_json.as_str(),
                "rows_read" => 1_u64, "rows_landed" => 1_u64, "rows_quarantined" => 0_u64,
                "quarantine_reasons" => "", "last_error_code" => RuntimeValue::Null,
                "last_error_message" => RuntimeValue::Null,
                "connector_id" => "erpnext.rest.connector",
                "source_schema_id" => "erpnext.rest.source",
                "mapping_id" => "erpnext.rest.mapping",
            },
        };
        let raw = observation(&connect_profile, &legacy_conn_row, T0).expect("raw");
        host.ingest(&[(
            raw,
            MappingInput {
                mapping: "norn.connect.mapping".into(),
                dataset: "connection".into(),
                connector_instance: "cell".into(),
                partition: "connection".into(),
                recorded_at: norn_ir::provenance::UtcTimestamp(RECORDED_AT.into()),
            },
        )])
        .expect("seed legacy connection");
        assert!(
            host.source_cursors("erpnext.rest")
                .expect("cursors")
                .is_empty(),
            "legacy cell must start with empty _norn_source_cursor"
        );

        let mut sources = rest_sources(ERPNEXT_CONFIG);
        let upgrade_t = T0.saturating_add(1);
        sources
            .claim(
                "erpnext.rest",
                Trigger::Manual,
                "user:upgrade@demo",
                upgrade_t,
            )
            .expect("claim");
        let summary = run_erpnext_sync(
            &host,
            &mut sources,
            &profile,
            upgrade_t,
            recorded_pages("erpnext"),
        );
        assert_eq!(summary.outcome, "ok", "{summary:#?}");
        assert!(summary.rows_landed > 0);
        let cursors = host.source_cursors("erpnext.rest").expect("cursors");
        assert_eq!(cursors.len(), profile.feeds.len());
        let party_wm: Watermark = serde_json::from_str(&cursors["Party"].cursor_json).expect("wm");
        assert_ne!(party_wm.version, "2026-01-01 00:00:00");
    }

    /// Regression 7: a store fault / crash AFTER `_norn_source_cursor` commits
    /// (during post-capture canonical processing) leaves captured rows durable
    /// in `_norn_inbox` + `_norn_source_pending`; reopening the SQLite file and
    /// running the next sync (even with empty remote pages) lands every stranded
    /// row into canonical tables and clears `_norn_source_pending`.
    #[test]
    fn post_cursor_commit_fault_recovers_stranded_inbox_rows_after_reopen() {
        let artifacts = rest_artifacts(&[]);
        let profile = profile_of("erpnext");
        let db = TempDb::new("post-commit-recovery");

        let captured_rows = {
            let (host, _fail_cursor, fail_ledger) =
                open_faultable_host(&artifacts, Some(db.path()));
            let mut sources = rest_sources(ERPNEXT_CONFIG);
            sources
                .claim("erpnext.rest", Trigger::Manual, "user:owner@demo", T0)
                .expect("claim");
            let Step::Request(first_req) = sources.step(&host, "erpnext.rest", None, T0) else {
                panic!("first request");
            };
            let feed = profile
                .feeds
                .iter()
                .find(|f| first_req.url.contains(f.request.path.as_str()))
                .expect("feed");
            let (status, body) = recorded_pages("erpnext")
                .remove(&feed.dataset)
                .expect("recorded page");
            let count = body["data"].as_array().expect("rows").len();

            // Let `capture_source_page` commit `_norn_inbox` + `_norn_source_pending`
            // + `_norn_source_cursor`, then fault canonical `_norn_ledger` commit.
            fail_ledger.store(true, Ordering::Release);
            let Step::Done(failed) = sources.step(
                &host,
                "erpnext.rest",
                Some((status, body.to_string().as_bytes())),
                T0.saturating_add(1),
            ) else {
                panic!("post-capture canonical commit fault must fail the run");
            };
            assert_eq!(failed.outcome, "failed");
            assert!(
                host.source_cursors("erpnext.rest")
                    .expect("cursors")
                    .contains_key(&feed.dataset),
                "cursor already committed before post-capture fault"
            );
            assert!(
                !host
                    .pending_source_captures("erpnext.rest")
                    .expect("pending")
                    .is_empty(),
                "stranded capture must remain durable in _norn_source_pending"
            );
            assert!(
                host.host_object("canon.Party", "demo/party/owner")
                    .expect("read")
                    .is_none(),
                "canonical object must not yet have landed"
            );
            u64::try_from(count).expect("u64")
        };

        // Reopen the SQLite file on disk with a fresh host and fresh Sources;
        // serve EMPTY pages for every feed (keyset cursor already moved past them).
        let reopened = reopen_native_host(&artifacts, db.path());
        let mut recovery_sources = rest_sources(ERPNEXT_CONFIG);
        let t_recover = T0.saturating_add(100);
        recovery_sources
            .claim(
                "erpnext.rest",
                Trigger::Manual,
                "user:recover@demo",
                t_recover,
            )
            .expect("claim recovery");
        let summary = run_erpnext_sync(
            &reopened,
            &mut recovery_sources,
            &profile,
            t_recover,
            BTreeMap::new(),
        );
        assert_eq!(summary.outcome, "ok", "{summary:#?}");
        assert_eq!(summary.rows_landed, captured_rows);
        assert!(
            reopened
                .pending_source_captures("erpnext.rest")
                .expect("pending")
                .is_empty(),
            "recovery must drain _norn_source_pending"
        );
        assert!(
            reopened
                .host_object("canon.Party", "demo/party/owner")
                .expect("read")
                .is_some(),
            "stranded inbox row must land in canonical table after reopen"
        );
    }

    /// Regression 8: a multi-row source page spanning an `OBS-LINK-DISPUTE-1`
    /// conflict (a valid row followed by a payload-conflicting row for an
    /// already-captured event) falls back to lease-checked one-by-one delivery,
    /// retains the conflict's quarantine evidence, advances `_norn_source_cursor`
    /// with the complete page's content digest, and replays idempotently.
    #[test]
    fn page_spanning_dispute_falls_back_retains_quarantine_and_replays_idempotently() {
        let host = open_native_host(&rest_artifacts(&[]));
        let dataset = "Party".to_owned();
        let (all_rows, next_wm) = sealed_page_for("erpnext", &dataset, T0);
        assert!(all_rows.len() >= 3);

        // Pre-capture row 1 so a second observation with the same event identity
        // and different payload triggers a FEED-CONFLICT (`mapping-conflict:`).
        host.ingest(&[all_rows[1].clone()])
            .expect("pre-capture baseline row");

        let mut conflicting_row = all_rows[1].clone();
        if let RuntimeValue::Object(obj) = &mut conflicting_row.0.input {
            if let Some(RuntimeValue::Object(payload)) = obj.get_mut("payload") {
                payload.insert(
                    "customer_name".into(),
                    RuntimeValue::String("Conflicting Party Name".into()),
                );
            }
            let digest = norn_ir::canonical::HashProfile::Norn1
                .hash_json(serde_json::to_string(&obj["payload"]).expect("json"))
                .expect("digest");
            obj.insert("payload_digest".into(), RuntimeValue::String(digest));
        }

        let page = vec![all_rows[0].clone(), conflicting_row, all_rows[2].clone()];
        let lease = host
            .acquire_source_lease("erpnext.rest", "run-dispute", T0, RUN_LEASE_MS)
            .expect("lease");
        let advance = CursorAdvanceSpec {
            connection_id: "erpnext.rest".into(),
            dataset: dataset.clone(),
            lease_id: lease.lease_id.clone(),
            generation: lease.generation,
            now_ms: T0.saturating_add(1),
            expected_previous_cursor: None,
            next_cursor: Some(next_wm.cursor_json().expect("json")),
            renew_ttl_ms: Some(RUN_LEASE_MS),
        };
        let first = host
            .ingest_source_page(&page, &advance)
            .expect("dispute-spanning page must fall back and advance");
        assert_eq!(first.len(), 3);
        assert_eq!(first[0].outcome, MappingOutcome::Landed);
        assert!(matches!(
            first[1].outcome,
            MappingOutcome::Quarantined(ref reasons)
                if reasons.iter().any(|r| r.code == "core.boundary.import_finding")
        ));
        assert_eq!(first[2].outcome, MappingOutcome::Landed);
        assert!(
            host.source_cursors("erpnext.rest")
                .expect("cursors")
                .contains_key(&dataset)
        );

        let replay = host
            .ingest_source_page(&page, &advance)
            .expect("exact replay of dispute-spanning page must succeed idempotently");
        assert_eq!(replay.len(), 3);
        assert_eq!(replay[0].observation_id, first[0].observation_id);
        assert_eq!(replay[1].observation_id, first[1].observation_id);
        assert_eq!(replay[2].observation_id, first[2].observation_id);
    }

    /// Regression 8b: a nonterminal dispute-spanning page (`next_cursor = None`)
    /// retains a processed full-page replay marker so later-timestamp redelivery
    /// returns identical observation IDs without regrouping or inserting a new
    /// `mapping-page:` row.
    #[test]
    fn nonterminal_dispute_page_fallback_replays_idempotently() {
        let host = open_native_host(&rest_artifacts(&[]));
        let dataset = "Party".to_owned();
        let (rows_t0, _) = sealed_page_for("erpnext", &dataset, T0);
        let (rows_later, _) = sealed_page_for("erpnext", &dataset, T0.saturating_add(7_000));
        host.ingest(&[rows_t0[1].clone()])
            .expect("pre-capture baseline row");
        let mut conflicting_t0 = rows_t0[1].clone();
        let mut conflicting_later = rows_later[1].clone();
        for row in [&mut conflicting_t0, &mut conflicting_later] {
            if let RuntimeValue::Object(obj) = &mut row.0.input {
                if let Some(RuntimeValue::Object(payload)) = obj.get_mut("payload") {
                    payload.insert(
                        "customer_name".into(),
                        RuntimeValue::String("Conflicting Nonterminal".into()),
                    );
                }
                let digest = norn_ir::canonical::HashProfile::Norn1
                    .hash_json(serde_json::to_string(&obj["payload"]).expect("json"))
                    .expect("digest");
                obj.insert("payload_digest".into(), RuntimeValue::String(digest));
            }
        }
        let nonterm_page = vec![rows_t0[0].clone(), conflicting_t0];
        let nonterm_page_later = vec![rows_later[0].clone(), conflicting_later];
        let lease = host
            .acquire_source_lease("erpnext.rest", "run-nt-dispute", T0, RUN_LEASE_MS)
            .expect("lease");
        let nonterm_adv = CursorAdvanceSpec {
            connection_id: "erpnext.rest".into(),
            dataset,
            lease_id: lease.lease_id,
            generation: lease.generation,
            now_ms: T0.saturating_add(1),
            expected_previous_cursor: None,
            next_cursor: None,
            renew_ttl_ms: Some(RUN_LEASE_MS),
        };
        let nt_first = host
            .ingest_source_page(&nonterm_page, &nonterm_adv)
            .expect("nonterminal dispute fallback");
        let nt_replay = host
            .ingest_source_page(&nonterm_page_later, &nonterm_adv)
            .expect("nonterminal dispute replay");
        assert_eq!(
            nt_replay
                .iter()
                .map(|o| o.observation_id.as_str())
                .collect::<Vec<_>>(),
            nt_first
                .iter()
                .map(|o| o.observation_id.as_str())
                .collect::<Vec<_>>(),
            "nonterminal dispute replay must return exact original observation ids"
        );
    }

    /// Regression 9: replaying a nonterminal cursor page (`next_cursor = None`)
    /// at a later `extracted_at` timestamp hits `IdempotentReplay` via
    /// `_norn_source_pending` without creating fresh inbox rows or replacing the
    /// original pending entry, and processed replay markers are reaped once the
    /// terminal page commits `_norn_source_cursor`.
    #[test]
    fn nonterminal_cursor_page_replay_preserves_original_observation_ids() {
        let host = open_native_host(&rest_artifacts(&[]));
        let dataset = "Party".to_owned();
        let (rows_t0, next_wm) = sealed_page_for("erpnext", &dataset, T0);
        let (rows_later, _) = sealed_page_for("erpnext", &dataset, T0.saturating_add(9_000));
        let slice_t0 = rows_t0[..3].to_vec();
        let slice_later = rows_later[..3].to_vec();
        let lease = host
            .acquire_source_lease("erpnext.rest", "run-nonterm", T0, RUN_LEASE_MS)
            .expect("lease");
        let nonterminal = CursorAdvanceSpec {
            connection_id: "erpnext.rest".into(),
            dataset: dataset.clone(),
            lease_id: lease.lease_id.clone(),
            generation: lease.generation,
            now_ms: T0.saturating_add(1),
            expected_previous_cursor: None,
            next_cursor: None,
            renew_ttl_ms: Some(RUN_LEASE_MS),
        };
        let first = host
            .ingest_source_page(&slice_t0, &nonterminal)
            .expect("first nonterminal page");
        assert!(first.iter().all(|o| o.outcome == MappingOutcome::Landed));
        assert!(
            host.pending_source_captures("erpnext.rest")
                .expect("pending")
                .is_empty(),
            "processed nonterminal page must not reappear as unprocessed recovery work"
        );

        let replay = host
            .ingest_source_page(&slice_later, &nonterminal)
            .expect("nonterminal replay at later extracted_at");
        assert_eq!(
            replay
                .iter()
                .map(|o| o.observation_id.as_str())
                .collect::<Vec<_>>(),
            first
                .iter()
                .map(|o| o.observation_id.as_str())
                .collect::<Vec<_>>(),
            "nonterminal cursor replay must return exact original observation ids"
        );
        assert!(
            replay
                .iter()
                .all(|o| o.outcome == MappingOutcome::AlreadyLanded)
        );

        let terminal = CursorAdvanceSpec {
            next_cursor: Some(next_wm.cursor_json().expect("json")),
            now_ms: T0.saturating_add(2),
            ..nonterminal
        };
        let terminal_out = host
            .ingest_source_page(&[], &terminal)
            .expect("empty terminal page commits cursor");
        assert!(
            terminal_out.is_empty(),
            "empty terminal page must produce no ingested rows: {terminal_out:?}"
        );
        assert!(
            host.source_cursors("erpnext.rest")
                .expect("cursors")
                .contains_key(&dataset)
        );
    }

    fn seal_holding_rows(
        mutate: impl FnOnce(&mut Vec<Value>),
    ) -> (
        Vec<(norn_kernel::Observed<RuntimeValue>, MappingInput)>,
        Watermark,
    ) {
        let dataset = "Warehouse Lot Balance";
        let mut profile = profile_of("erpnext");
        let feed = profile
            .feeds
            .iter()
            .find(|f| f.dataset == dataset)
            .cloned()
            .expect("holding feed");
        profile.feeds = vec![feed];
        let (status, mut body) = recorded_pages("erpnext").remove(dataset).expect("page");
        let items = body["data"].as_array_mut().expect("rows");
        items.truncate(2);
        assert_eq!(items.len(), 2, "need two holding rows");
        mutate(items);
        let mut pull = Pull::new(profile.clone(), BTreeMap::new(), Limits::default());
        let _req = pull.next_request().expect("request");
        let accepted = pull
            .accept(status, body.to_string().as_bytes())
            .expect("accepted");
        let next_wm = accepted.advance.next.clone().expect("next wm");
        let rows = accepted
            .rows
            .iter()
            .map(|row| {
                (
                    observation(&profile, row, T0).expect("sealed"),
                    MappingInput {
                        mapping: "erpnext.rest.mapping".into(),
                        dataset: row.dataset.clone(),
                        connector_instance: "sandbox".into(),
                        partition: row.dataset.clone(),
                        recorded_at: norn_ir::provenance::UtcTimestamp(RECORDED_AT.into()),
                    },
                )
            })
            .collect();
        (rows, next_wm)
    }

    /// Regression 10: lowering `max_rows` to 1 while draining a single captured
    /// `mapping-page:` run whose members are commit-refused (disputed by a
    /// conflicting resend before promotion, so `check_mapping_bundle` refuses
    /// both with `UnsupportedMappingProfile` and leaves both pending in
    /// `_norn_inbox` as quarantined) terminates cleanly in bounded steps
    /// without retrying the same member forever.
    #[test]
    fn paced_drain_with_multiple_quarantined_rows_terminates_cleanly() {
        let host = open_native_host(&rest_artifacts(&[]));
        let dataset = "Warehouse Lot Balance".to_owned();
        let (two_holdings, next_wm) = seal_holding_rows(|_| {});
        let (conflicting_holdings, _) = seal_holding_rows(|items| {
            for item in items.iter_mut() {
                item["quantity"] = json!(999_999.0);
            }
        });
        let lease = host
            .acquire_source_lease("erpnext.rest", "run-q-pace", T0, RUN_LEASE_MS)
            .expect("lease");
        let next_cursor = Some(next_wm.cursor_json().expect("json"));
        let advance = CursorAdvanceSpec {
            connection_id: "erpnext.rest".into(),
            dataset: dataset.clone(),
            lease_id: lease.lease_id.clone(),
            generation: lease.generation,
            now_ms: T0.saturating_add(1),
            expected_previous_cursor: None,
            next_cursor: next_cursor.clone(),
            renew_ttl_ms: Some(RUN_LEASE_MS),
        };
        let mut pending = host
            .capture_source_page_atomic(&two_holdings, &advance, 10)
            .expect("captured");
        assert_eq!(
            pending.pages.len(),
            1,
            "both valid holding rows must enter one mapping-page: run to exercise paced page-member draining"
        );
        assert_eq!(pending.records.len(), 2);

        // Capture conflicting resends for both source events so commit-time
        // `check_mapping_bundle` quarantines both members while leaving them
        // pending in `_norn_inbox`.
        let conflict_advance = CursorAdvanceSpec {
            connection_id: "erpnext.rest".into(),
            dataset,
            lease_id: lease.lease_id,
            generation: lease.generation,
            now_ms: T0.saturating_add(2),
            expected_previous_cursor: next_cursor,
            next_cursor: None,
            renew_ttl_ms: Some(RUN_LEASE_MS),
        };
        let _conflict_pending = host
            .capture_source_page_atomic(&conflicting_holdings, &conflict_advance, 10)
            .expect("capture conflicting resends");

        let mut outcomes = BTreeMap::new();
        let mut seen = Vec::new();
        let mut steps = 0_usize;
        while !pending.pages.is_empty() || !pending.records.is_empty() {
            steps = steps.saturating_add(1);
            assert!(steps <= 4, "paced drain must not spin on quarantined rows");
            let step = host
                .process_source_page_step(&mut pending, &mut outcomes, 1)
                .expect("step");
            assert!(step.len() <= 1, "step must respect max_rows=1");
            seen.extend(step);
        }
        assert_eq!(seen.len(), 2);
        assert!(
            seen.iter()
                .all(|o| matches!(o.outcome, MappingOutcome::Quarantined(_))),
            "expected commit-refused quarantine outcomes, got {seen:#?}"
        );
    }

    /// Regression 11: paced `OBS-LINK-DISPUTE-1` fallback retains an uncaptured
    /// row on store error (never skipping it or advancing cursor prematurely)
    /// and enforces the fresh step `now_ms` against lease expiry on continuation.
    #[test]
    fn dispute_fallback_retains_row_on_error_and_checks_fresh_lease_clock() {
        let artifacts = rest_artifacts(&[]);
        let (host, fail_cursor, fail_ledger) = open_faultable_host(&artifacts, None);
        let dataset = "Warehouse Lot Balance".to_owned();
        // A non-NFC `name` on row 1 after valid row 0 emits a new
        // `mapping-identity:` finding and triggers `MappingPageSpansDispute`.
        let (rows, next_wm) = seal_holding_rows(|items| {
            items[1]["name"] = json!("WLB-cafe\u{0301}");
        });
        let lease = host
            .acquire_source_lease("erpnext.rest", "run-dispute", T0, RUN_LEASE_MS)
            .expect("lease");
        let advance = CursorAdvanceSpec {
            connection_id: "erpnext.rest".into(),
            dataset: dataset.clone(),
            lease_id: lease.lease_id.clone(),
            generation: lease.generation,
            now_ms: T0.saturating_add(1),
            expected_previous_cursor: None,
            next_cursor: Some(next_wm.cursor_json().expect("json")),
            renew_ttl_ms: Some(RUN_LEASE_MS),
        };
        let norn_api::host::SourceCapturePlan::DisputeFallback(mut fallback) = host
            .capture_source_page_planned(&rows, &advance, 10)
            .expect("plan")
        else {
            panic!("expected dispute fallback plan for mid-page identity finding");
        };

        // Transient store fault during row 0 retains row 0 without committing cursor.
        fail_ledger.store(true, Ordering::Release);
        assert!(
            host.step_source_dispute_fallback(&mut fallback, T0.saturating_add(2), 1)
                .is_err()
        );
        assert!(!fallback.is_complete());
        assert!(
            host.source_cursors("erpnext.rest")
                .expect("cursors")
                .is_empty(),
            "cursor must not advance on row 0 fault"
        );

        // Clearing the fault lets row 0 complete without skipping it.
        fail_ledger.store(false, Ordering::Release);
        let first_step = host
            .step_source_dispute_fallback(&mut fallback, T0.saturating_add(3), 1)
            .expect("retry row 0");
        assert_eq!(first_step.len(), 1);
        assert!(!fallback.is_complete());

        // Advancing the step clock past lease expiry fences row 1 even without takeover.
        let expired_ms = T0.saturating_add(3).saturating_add(RUN_LEASE_MS);
        assert!(matches!(
            host.step_source_dispute_fallback(&mut fallback, expired_ms, 1),
            Err(SourceCaptureError::LeaseFenced(_))
        ));
        assert!(!fallback.is_complete());

        // Final cursor-write fault after row 1 pops must leave `is_complete() == false`
        // until a retry commits `_norn_source_cursor`.
        fail_cursor.store(true, Ordering::Release);
        assert!(
            host.step_source_dispute_fallback(&mut fallback, T0.saturating_add(4), 1)
                .is_err()
        );
        assert!(!fallback.is_complete());
        assert!(
            host.source_cursors("erpnext.rest")
                .expect("cursors")
                .is_empty(),
            "cursor must remain uncommitted on final cursor write fault"
        );

        fail_cursor.store(false, Ordering::Release);
        let retry_commit = host
            .step_source_dispute_fallback(&mut fallback, T0.saturating_add(5), 1)
            .expect("retry final cursor commit");
        assert!(
            retry_commit.is_empty(),
            "retry after all rows processed commits cursor without re-ingesting: {retry_commit:?}"
        );
        assert!(fallback.is_complete());
        assert!(
            host.source_cursors("erpnext.rest")
                .expect("cursors")
                .contains_key(&dataset)
        );
    }

    /// Regression 12: stale mapping revisions are fenced inside `reset_source_cursors`,
    /// `bind_source_mapping`, and `seed_source_cursors` before any cursor/binding mutation.
    #[test]
    fn stale_mapping_revision_fences_reset_bind_and_seed() {
        let host = open_native_host(&rest_artifacts(&[]));
        let active = host
            .active_mapping_binding("erpnext.rest.mapping")
            .expect("active binding")
            .expect("mapping installed");
        let stale = "erpnext.rest.mapping@0.0.0#stale";
        assert_ne!(active, stale);
        let lease = host
            .acquire_source_lease("erpnext.rest", "run-stale-rev", T0, RUN_LEASE_MS)
            .expect("lease");
        let seed = BTreeMap::from([(
            "Party".to_owned(),
            r#"{"version":"2026-01-01 00:00:00","id":"CUST-1"}"#.to_owned(),
        )]);
        assert!(matches!(
            host.reset_source_cursors("erpnext.rest", ("erpnext.rest.mapping", stale), &lease, T0),
            Err(SourceCaptureError::LeaseFenced(_))
        ));
        assert!(matches!(
            host.bind_source_mapping(
                "erpnext.rest",
                ("erpnext.rest.mapping", stale),
                &lease,
                T0,
                &seed,
            ),
            Err(SourceCaptureError::LeaseFenced(_))
        ));
        assert!(matches!(
            host.seed_source_cursors(
                "erpnext.rest",
                &lease,
                T0,
                &seed,
                Some(("erpnext.rest.mapping", stale)),
            ),
            Err(SourceCaptureError::LeaseFenced(_))
        ));
        assert!(
            host.source_mapping_binding("erpnext.rest")
                .expect("binding")
                .is_none()
        );
        assert!(
            host.source_cursors("erpnext.rest")
                .expect("cursors")
                .is_empty()
        );
    }

    /// Regression 13: legacy binding and cursor seeding commit atomically in one
    /// SQLite transaction; a fault during cursor seeding rolls back the binding.
    #[test]
    fn bind_source_mapping_rolls_back_binding_on_seed_cursor_fault() {
        let (host, fail_cursor, _fail_ledger) = open_faultable_host(&rest_artifacts(&[]), None);
        let active = host
            .active_mapping_binding("erpnext.rest.mapping")
            .expect("active binding")
            .expect("mapping installed");
        let lease = host
            .acquire_source_lease("erpnext.rest", "run-atomic-seed", T0, RUN_LEASE_MS)
            .expect("lease");
        let seed = BTreeMap::from([(
            "Party".to_owned(),
            r#"{"version":"2026-01-01 00:00:00","id":"CUST-1"}"#.to_owned(),
        )]);
        fail_cursor.store(true, Ordering::Release);
        assert!(
            host.bind_source_mapping(
                "erpnext.rest",
                ("erpnext.rest.mapping", &active),
                &lease,
                T0,
                &seed,
            )
            .is_err()
        );
        assert!(
            host.source_mapping_binding("erpnext.rest")
                .expect("binding")
                .is_none(),
            "binding must roll back when legacy cursor seed faults"
        );
        assert!(
            host.source_cursors("erpnext.rest")
                .expect("cursors")
                .is_empty()
        );
        fail_cursor.store(false, Ordering::Release);
        host.bind_source_mapping(
            "erpnext.rest",
            ("erpnext.rest.mapping", &active),
            &lease,
            T0,
            &seed,
        )
        .expect("atomic bind+seed");
        assert_eq!(
            host.source_mapping_binding("erpnext.rest")
                .expect("binding")
                .as_deref(),
            Some(active.as_str())
        );
        assert!(
            host.source_cursors("erpnext.rest")
                .expect("cursors")
                .contains_key("Party")
        );
    }

    /// Regression 14: when the sources report pack is installed, a real store
    /// error during end-of-run `report()` fails the run instead of returning `ok`.
    #[test]
    fn report_write_fault_with_report_pack_installed_fails_finish() {
        let (host, _fail_cursor, fail_ledger) = open_faultable_host(&rest_artifacts(&[]), None);
        let profile = profile_of("erpnext");
        let mut sources = rest_sources(ERPNEXT_CONFIG);
        sources
            .claim("erpnext.rest", Trigger::Manual, "user:owner@demo", T0)
            .expect("claim");
        let mut step = sources.step(&host, "erpnext.rest", None, T0);
        let mut t = T0;
        let feed_count = profile.feeds.len();
        let mut seen = 0_usize;
        let summary = loop {
            match step {
                Step::Request(req) => {
                    seen += 1;
                    let feed = profile
                        .feeds
                        .iter()
                        .find(|f| req.url.contains(f.request.path.as_str()))
                        .expect("feed");
                    let body = empty_page(&profile, &feed.dataset).to_string();
                    if seen == feed_count {
                        // Fault ledger writes for the final `source.Connection` / `source.SyncRun` report.
                        fail_ledger.store(true, Ordering::Release);
                    }
                    t = t.saturating_add(1);
                    step = sources.step(&host, "erpnext.rest", Some((200, body.as_bytes())), t);
                }
                Step::Continue => {
                    t = t.saturating_add(1);
                    step = sources.step(&host, "erpnext.rest", None, t);
                }
                Step::Done(summary) => break summary,
            }
        };
        assert_eq!(summary.outcome, "failed", "{summary:#?}");
        assert!(summary.error.is_some());
    }

    /// Regression 15: a failed `source.Connection` report (e.g. auth failure)
    /// without `_norn_source_binding` never proves legacy cursor ownership for
    /// `legacy_cursors_match_mapping_binding`.
    #[test]
    fn failed_legacy_connection_report_does_not_prove_cursor_binding() {
        let host = open_native_host(&rest_artifacts(&[]));
        let active = host
            .active_mapping_binding("erpnext.rest.mapping")
            .expect("active binding")
            .expect("mapping installed");
        let legacy_wm = BTreeMap::from([(
            "Party".to_owned(),
            Watermark {
                version: "2026-01-01 00:00:00".into(),
                id: norn_ir::RuntimeValue::String("CUST-LEGACY".into()),
            },
        )]);
        let legacy_json = serde_json::to_string(&legacy_wm).expect("json");
        let connect_profile = PullProfile {
            source: "norn-connect".into(),
            pipeline: "norn-connect/1".into(),
            feeds: Vec::new(),
            token: None,
        };
        let failed_conn = norn_api::pull::Row {
            dataset: "connection".into(),
            source_id: "erpnext.rest".into(),
            version: "auth-fail/end".into(),
            payload: norn_ir::runtime_object! {
                "id" => "erpnext.rest", "system" => "erpnext", "label" => "ERPNext REST",
                "target" => "http://erpnext-rest:8000", "auth" => "api_key", "status" => "failed",
                "last_sync_started_at" => RECORDED_AT, "last_sync_ended_at" => RECORDED_AT,
                "last_trigger" => "manual", "watermark" => legacy_json.as_str(),
                "rows_read" => 0_u64, "rows_landed" => 0_u64, "rows_quarantined" => 0_u64,
                "quarantine_reasons" => "", "last_error_code" => "source.auth",
                "last_error_message" => "missing secret",
                "connector_id" => "erpnext.rest.connector",
                "source_schema_id" => "erpnext.rest.source",
                "mapping_id" => "erpnext.rest.mapping",
            },
        };
        let raw = observation(&connect_profile, &failed_conn, T0).expect("raw");
        host.ingest(&[(
            raw,
            MappingInput {
                mapping: "norn.connect.mapping".into(),
                dataset: "connection".into(),
                connector_instance: "cell".into(),
                partition: "connection".into(),
                recorded_at: norn_ir::provenance::UtcTimestamp(RECORDED_AT.into()),
            },
        )])
        .expect("record failed connection");
        assert!(
            !host
                .legacy_cursors_match_mapping_binding(
                    "erpnext.rest",
                    "erpnext.rest.mapping",
                    &active,
                )
                .expect("legacy check"),
            "failed report must never bless legacy cursors"
        );
    }

    /// Regression 16: a second `Sources` instance with missing auth secrets is
    /// fenced at lease acquisition before auth runs and never overwrites an
    /// active worker's `source.Connection`.
    #[test]
    fn preflight_auth_failure_requires_lease_and_cannot_clobber_active_worker() {
        let host = open_native_host(&rest_artifacts(&[]));
        let mut worker_a = rest_sources(ERPNEXT_CONFIG);
        worker_a
            .claim("erpnext.rest", Trigger::Manual, "user:worker-a@demo", T0)
            .expect("claim a");
        let Step::Request(_) = worker_a.step(&host, "erpnext.rest", None, T0) else {
            panic!("worker A must acquire lease and issue first request");
        };
        let conn_before = host
            .host_object("source.Connection", "erpnext.rest")
            .expect("read")
            .expect("worker A connection");
        assert_eq!(
            conn_before.get("status").and_then(RuntimeValue::as_str),
            Some("syncing")
        );

        let mut worker_b = Sources::with_allowed_hosts(
            &cell_scope(),
            ERPNEXT_CONFIG,
            &BTreeMap::new(),
            SANDBOX_HOSTS,
        )
        .expect("no secrets");
        worker_b
            .claim(
                "erpnext.rest",
                Trigger::Manual,
                "user:worker-b@demo",
                T0.saturating_add(1),
            )
            .expect("claim b");
        let Step::Done(summary_b) =
            worker_b.step(&host, "erpnext.rest", None, T0.saturating_add(1))
        else {
            panic!("worker B must fail immediately when lease is held");
        };
        assert_eq!(summary_b.outcome, "failed");
        assert_eq!(
            summary_b.error.as_ref().map(|(c, _)| c.as_str()),
            Some("source.lease_fenced"),
            "lease check must precede auth failure: {summary_b:#?}"
        );
        let conn_after = host
            .host_object("source.Connection", "erpnext.rest")
            .expect("read")
            .expect("connection after b");
        assert_eq!(
            conn_after.get("status").and_then(RuntimeValue::as_str),
            Some("syncing"),
            "fenced worker B must not overwrite worker A's syncing status"
        );
    }
}
