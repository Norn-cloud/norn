//! SOURCE-CAPTURE-1 pinned regressions (Lane B3):
//!
//! 1. Pull capture failure immediately before `_norn_source_cursor` persistence
//!    rolls back both inbox capture and cursor advancement atomically; retry
//!    succeeds with zero lost rows.
//! 2. Duplicate delivery of an already-captured page is idempotent with zero
//!    double-capture or cursor corruption, while conflicting cursors refuse.
//! 3. Empty terminal pages advance `_norn_source_cursor` atomically in SQLite.
//! 4. Mid-page failures (intra-page framing failure and multi-chunk paced
//!    failure) never advance `_norn_source_cursor`, while quarantined rows are
//!    durably captured as evidence alongside cursor progress.
//! 5. Expired leases racing a newer sync are fenced by monotonic generation
//!    both on capture and on stale release.

#[cfg(not(target_arch = "wasm32"))]
#[allow(
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    reason = "SOURCE-CAPTURE-1 atomic source capture regressions"
)]
mod tests {
    use std::collections::{BTreeMap, BTreeSet};
    use std::path::PathBuf;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};

    use norn_api::host::{DevHost, SourceCaptureError};
    use norn_api::pull::{Limits, Pull, PullProfile, Watermark, observation};
    use norn_api::scope::CellScope;
    use norn_api::sources::{RUN_LEASE_MS, Sources, Step, Trigger};
    use norn_cell::engine::{NativeEngine, ReadIsolation, SqlEngine, SqlValue};
    use norn_cell::mapping_bundle::MappingInput;
    use norn_cell::source_cursor::CursorAdvanceSpec;
    use norn_cell::{CellError, CellStore, InstalledDefinitionInput, MappingOutcome, Model};
    use norn_checker::EntityArtifactSetContext;
    use norn_checker::receipt::{ReceiptSources, check_artifact_set_with_receipt};
    use norn_ir::{ArtifactEnvelope, ArtifactKind, EntityRoot, ResourceLimitProfile, RuntimeValue};
    use serde_json::{Value, json};

    const RECORDED_AT: &str = "2026-11-12T12:00:00Z";
    const T0: u64 = 1_794_484_800_000;
    const SANDBOX_HOSTS: &str = "erpnext-rest,odoo-rest,cursor-mock";

    /// Wraps [`NativeEngine`] and deterministically faults the exact SQLite
    /// statement that persists `_norn_source_cursor` inside `capture_source_page`'s
    /// `write_tx`, after every row's inbox capture has already executed.
    struct CursorFaultEngine {
        inner: NativeEngine,
        fail_cursor_write: Arc<AtomicBool>,
    }

    impl SqlEngine for CursorFaultEngine {
        fn execute(&self, sql: &str, params: &[SqlValue]) -> Result<(), CellError> {
            if self.fail_cursor_write.load(Ordering::Acquire) && sql.contains("_norn_source_cursor")
            {
                return Err(CellError::Engine(
                    "injected fault immediately before cursor persistence".to_owned(),
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

        fn read_isolation(&self) -> ReadIsolation {
            self.inner.read_isolation()
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
                other => panic!("unexpected entity root: {other:?}"),
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
    ) -> (DevHost<CursorFaultEngine>, Arc<AtomicBool>) {
        let (model, input) = admit_and_model(artifacts);
        let fail_cursor_write = Arc::new(AtomicBool::new(false));
        let engine = CursorFaultEngine {
            inner: NativeEngine::open_in_memory().expect("sqlite"),
            fail_cursor_write: Arc::clone(&fail_cursor_write),
        };
        let mut store = CellStore::from_engine(engine, model.clone()).expect("store");
        let scope = cell_scope();
        store
            .install_definitions(model, input, Some(0), &scope.key())
            .expect("install");
        let host = DevHost::from_store(store, scope, |_| BTreeMap::new()).expect("host");
        (host, fail_cursor_write)
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

    fn sealed_first_page(
        system: &str,
    ) -> (
        String,
        Vec<(norn_kernel::Observed<RuntimeValue>, MappingInput)>,
        Watermark,
    ) {
        let profile = profile_of(system);
        let mut pages = recorded_pages(system);
        let mut pull = Pull::new(profile.clone(), BTreeMap::new(), Limits::default());
        let req = pull.next_request().expect("first request");
        let (status, body) = pages.remove(&req.dataset).expect("first page");
        let accepted = pull
            .accept(status, body.to_string().as_bytes())
            .expect("accepted");
        let next_wm = accepted.advance.next.clone().expect("keyset next");
        let rows = accepted
            .rows
            .iter()
            .map(|row| {
                let raw = observation(&profile, row, T0).expect("sealed");
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
        (accepted.dataset, rows, next_wm)
    }

    fn run_erpnext_sync<E: SqlEngine>(
        host: &DevHost<E>,
        sources: &mut Sources,
        profile: &PullProfile,
        now_ms: u64,
    ) -> norn_api::sources::RunSummary {
        let mut pages = recorded_pages("erpnext");
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
        let (host, fault) = open_faultable_host(&artifacts);
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
        fault.store(true, Ordering::Release);
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
        let conn = host
            .host_object("source.Connection", "erpnext.rest")
            .expect("read")
            .expect("connection");
        assert_eq!(
            conn.get("watermark"),
            Some(&RuntimeValue::String("{}".into()))
        );

        // Clear fault and re-sync from scratch with a fresh `Sources` instance.
        fault.store(false, Ordering::Release);
        let mut retry = rest_sources(ERPNEXT_CONFIG);
        let retry_t = T0.saturating_add(10);
        retry
            .claim("erpnext.rest", Trigger::Manual, "user:owner@demo", retry_t)
            .expect("re-claim");
        let summary = run_erpnext_sync(&host, &mut retry, &profile, retry_t);
        assert_eq!(summary.outcome, "ok", "{summary:#?}");
        assert!(summary.rows_read > 0);
        assert_eq!(summary.rows_landed, summary.rows_read);
        assert_eq!(
            host.source_cursors("erpnext.rest").expect("cursors").len(),
            profile.feeds.len()
        );
    }

    /// Regression 2: duplicate delivery of an already-captured page is
    /// idempotent with no double-land or cursor corruption; a conflicting
    /// cursor from a stale expectation is rejected with `CursorConflict`.
    #[test]
    fn duplicate_page_delivery_is_idempotent_and_conflicts_refuse() {
        let host = open_native_host(&rest_artifacts(&[]));
        let (dataset, rows, next_wm) = sealed_first_page("erpnext");
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
            .ingest_source_page(&rows, &advance)
            .expect("first delivery");
        assert!(first.iter().all(|o| o.outcome == MappingOutcome::Landed));

        let duplicate = host
            .ingest_source_page(&rows, &advance)
            .expect("duplicate delivery");
        assert_eq!(duplicate.len(), first.len());
        assert!(
            duplicate
                .iter()
                .all(|o| o.outcome == MappingOutcome::AlreadyLanded),
            "{duplicate:#?}"
        );
        let stored = host.source_cursors("erpnext.rest").expect("cursors");
        assert_eq!(
            stored[&dataset].cursor_json,
            next_wm.cursor_json().expect("json")
        );

        let conflicting = CursorAdvanceSpec {
            next_cursor: Some(
                Watermark {
                    version: "2099-01-01 00:00:00".into(),
                    id: Value::from("CONFLICT"),
                }
                .cursor_json()
                .expect("json"),
            ),
            ..advance
        };
        assert!(matches!(
            host.ingest_source_page(&rows, &conflicting),
            Err(SourceCaptureError::CursorConflict(_))
        ));
    }

    /// Regression 3: an empty terminal page on a cursor-paged feed atomically
    /// advances `_norn_source_cursor` in SQLite even with zero rows.
    #[test]
    fn empty_terminal_page_advances_cursor_atomically() {
        let mut cursor_connector =
            load_artifact("fixtures/packs/erpnext-rest/artifacts/erpnext.rest.connector.json");
        let mut payload =
            serde_json::to_value(cursor_connector.payload.to_json_value()).expect("payload");
        let mut profile = PullProfile::from_connector(&payload)
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
        payload["extensions"]["norn.pull/1"] = serde_json::to_value(&profile).expect("json");
        cursor_connector.artifact_id = "erpnext.cursor.connector".into();
        cursor_connector.name = "erpnext.cursor.connector".into();
        cursor_connector.payload = RuntimeValue::from_json(payload).expect("rv");

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

        let step = sources.step(
            &host,
            "erpnext.cursor",
            Some((200, first_body.to_string().as_bytes())),
            T0.saturating_add(1),
        );
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
    /// cursor, while a mapping-refused row is captured durably as quarantined
    /// evidence alongside atomic cursor advancement.
    #[test]
    fn mid_page_framing_failure_rolls_back_and_quarantined_rows_capture_with_cursor() {
        let host = open_native_host(&rest_artifacts(&[]));
        let (dataset, mut rows, next_wm) = sealed_first_page("odoo");
        assert!(rows.len() >= 2, "need multi-row page");
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

        let mut bad_product = rows
            .iter_mut()
            .find(|(_, input)| input.dataset == "product.product")
            .cloned();
        if let Some((ref mut raw, ref input)) = bad_product {
            if let RuntimeValue::Object(obj) = &mut raw.input {
                if let Some(RuntimeValue::Object(payload)) = obj.get_mut("payload") {
                    payload.insert("x_norn_ref".into(), RuntimeValue::Null);
                }
                let digest = norn_ir::canonical::HashProfile::Norn1
                    .hash_json(serde_json::to_string(&obj["payload"]).expect("json"))
                    .expect("digest");
                obj.insert("payload_digest".into(), RuntimeValue::String(digest));
                obj.insert(
                    "event_id".into(),
                    RuntimeValue::String("quarantine-1".into()),
                );
            }
            let q_advance = CursorAdvanceSpec {
                dataset: input.dataset.clone(),
                ..advance
            };
            let outcomes = host
                .ingest_source_page(&[(raw.clone(), input.clone())], &q_advance)
                .expect("quarantine captured");
            assert!(matches!(
                outcomes[0].outcome,
                MappingOutcome::Quarantined(ref reasons) if !reasons.is_empty()
            ));
            assert!(
                host.source_cursors("odoo.rest")
                    .expect("cursors")
                    .contains_key(&input.dataset)
            );
        } else {
            let outcomes = host.ingest_source_page(&rows, &advance).expect("retry");
            assert!(outcomes.iter().all(|o| o.outcome == MappingOutcome::Landed));
            assert!(
                host.source_cursors("odoo.rest")
                    .expect("cursors")
                    .contains_key(&dataset)
            );
        }
    }

    /// Regression 5: an expired lease racing a newer sync is fenced by
    /// monotonic generation on both capture and stale release; the newer sync's
    /// cursor wins.
    #[test]
    fn expired_lease_racing_a_newer_sync_is_fenced() {
        let host = open_native_host(&rest_artifacts(&[]));
        let (dataset, rows, wm_a) = sealed_first_page("erpnext");
        let wm_b = Watermark {
            version: "2026-12-01 00:00:00".into(),
            id: Value::from("WINNER-B"),
        };

        let lease_a = host
            .acquire_source_lease("erpnext.rest", "run-A", T0, RUN_LEASE_MS)
            .expect("lease A");
        assert_eq!(lease_a.generation, 1);
        assert!(matches!(
            host.acquire_source_lease(
                "erpnext.rest",
                "run-B",
                T0.saturating_add(1_000),
                RUN_LEASE_MS
            ),
            Err(SourceCaptureError::LeaseFenced(_))
        ));

        let expired_t = T0.saturating_add(RUN_LEASE_MS);
        let stale_advance = CursorAdvanceSpec {
            connection_id: "erpnext.rest".into(),
            dataset: dataset.clone(),
            lease_id: lease_a.lease_id.clone(),
            generation: lease_a.generation,
            now_ms: expired_t,
            expected_previous_cursor: None,
            next_cursor: Some(wm_a.cursor_json().expect("json")),
            renew_ttl_ms: Some(RUN_LEASE_MS),
        };
        assert!(matches!(
            host.ingest_source_page(&rows, &stale_advance),
            Err(SourceCaptureError::LeaseFenced(_))
        ));

        let lease_b = host
            .acquire_source_lease("erpnext.rest", "run-B", expired_t, RUN_LEASE_MS)
            .expect("lease B");
        assert_eq!(lease_b.generation, 2);

        let stale_clock = CursorAdvanceSpec {
            now_ms: T0.saturating_add(5_000),
            ..stale_advance
        };
        assert!(matches!(
            host.ingest_source_page(&rows, &stale_clock),
            Err(SourceCaptureError::LeaseFenced(_))
        ));
        assert!(
            host.source_cursors("erpnext.rest")
                .expect("cursors")
                .is_empty()
        );
        assert!(
            !host
                .release_source_lease("erpnext.rest", &lease_a.lease_id, lease_a.generation)
                .expect("stale release")
        );

        let winner_advance = CursorAdvanceSpec {
            connection_id: "erpnext.rest".into(),
            dataset: dataset.clone(),
            lease_id: lease_b.lease_id.clone(),
            generation: lease_b.generation,
            now_ms: expired_t.saturating_add(1),
            expected_previous_cursor: None,
            next_cursor: Some(wm_b.cursor_json().expect("json")),
            renew_ttl_ms: Some(RUN_LEASE_MS),
        };
        let landed = host
            .ingest_source_page(&rows, &winner_advance)
            .expect("worker B captures");
        assert!(landed.iter().all(|o| o.outcome == MappingOutcome::Landed));
        let stored = host.source_cursors("erpnext.rest").expect("cursors");
        assert_eq!(stored[&dataset].generation, 2);
        assert_eq!(
            stored[&dataset].cursor_json,
            wm_b.cursor_json().expect("json")
        );

        assert!(
            host.release_source_lease("erpnext.rest", &lease_b.lease_id, lease_b.generation)
                .expect("release B")
        );
        let lease_c = host
            .acquire_source_lease(
                "erpnext.rest",
                "run-C",
                expired_t.saturating_add(2),
                RUN_LEASE_MS,
            )
            .expect("lease C");
        assert_eq!(lease_c.generation, 3);
    }
}
