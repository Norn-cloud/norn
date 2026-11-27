//! Lane C2 (#1923): SQLite state-machine conformance and negative mutant replay
//! for `verification/pull-cursor-atomicity-v1/models/pull-cursor-atomicity.qnt`
//! over the real `SOURCE-CAPTURE-1` implementation (`CellStore` + `DevHost`).
//!
//! Evidence label: bounded state-machine conformance and SQLite mutant replay
//! over real SQLite transactions (not a mechanized Rust-to-Quint refinement proof).

#[cfg(not(target_arch = "wasm32"))]
#[allow(
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::unwrap_used,
    reason = "state-machine conformance checks fail loudly on any invariant divergence"
)]
mod tests {
    use std::collections::{BTreeMap, BTreeSet};
    use std::path::{Path, PathBuf};
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

    use norn_api::host::{DevHost, SourceCaptureError};
    use norn_api::pull::{PullProfile, Row, Watermark, observation};
    use norn_api::scope::CellScope;
    use norn_cell::engine::{NativeEngine, ReadIsolation, SqlEngine, SqlValue};
    use norn_cell::mapping_bundle::MappingInput;
    use norn_cell::source_cursor::{CursorAdvanceSpec, SourceLease};
    use norn_cell::{
        CellError, CellStore, InstalledDefinitionInput, MappingOutcome, Model, StoreOptions,
    };
    use norn_checker::EntityArtifactSetContext;
    use norn_checker::receipt::ReceiptSources;
    use norn_ir::provenance::UtcTimestamp;
    use norn_ir::{ArtifactEnvelope, ArtifactKind, EntityRoot, ResourceLimitProfile, RuntimeValue};
    use norn_kernel::Observed;
    use norn_ui_checker::check_artifact_set_with_receipt;
    use serde_json::json;

    const RECORDED_AT: &str = "2026-11-12T12:00:00Z";
    const T0: u64 = 1_794_484_800_000;
    const TICK_MS: u64 = 10_000;
    const TTL_MS: u64 = 30_000;
    const CONNECTION_ID: &str = "conn-c2-atomicity";
    const DATASET: &str = "Party";
    const MAX_PAGES: u64 = 3;

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
                    "C2 mutant fault before _norn_source_cursor write".to_owned(),
                ));
            }
            if self.fail_ledger_write.load(Ordering::Acquire) && sql.contains("_norn_ledger") {
                return Err(CellError::Engine(
                    "C2 mutant fault during post-capture ledger write".to_owned(),
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
                "norn-c2-{label}-{}-{id}.sqlite",
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

    fn rest_artifacts() -> Vec<ArtifactEnvelope> {
        let mut set: Vec<ArtifactEnvelope> = serde_json::from_str(
            &std::fs::read_to_string(
                root().join("fixtures/demo/explorer/three-systems.definitions.json"),
            )
            .expect("retained catalog"),
        )
        .expect("artifacts");
        for kind in ["source", "mapping", "connector"] {
            set.push(load_artifact(&format!(
                "fixtures/packs/erpnext-rest/artifacts/erpnext.rest.{kind}.json"
            )));
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

    fn open_host(
        artifacts: &[ArtifactEnvelope],
        path: &Path,
        install: bool,
    ) -> (DevHost<FaultEngine>, Arc<AtomicBool>, Arc<AtomicBool>) {
        let (model, input) = admit_and_model(artifacts);
        let fail_cursor_write = Arc::new(AtomicBool::new(false));
        let fail_ledger_write = Arc::new(AtomicBool::new(false));
        let inner = NativeEngine::open(path, StoreOptions::default()).expect("sqlite file");
        let engine = FaultEngine {
            inner,
            fail_cursor_write: Arc::clone(&fail_cursor_write),
            fail_ledger_write: Arc::clone(&fail_ledger_write),
        };
        let mut store = CellStore::from_engine(engine, model.clone()).expect("store");
        let scope = cell_scope();
        if install {
            store
                .install_definitions(model, input, Some(0), &scope.key())
                .expect("install");
        }
        let host = DevHost::from_store(store, scope, |_| BTreeMap::new()).expect("host");
        (host, fail_cursor_write, fail_ledger_write)
    }

    /// Direct SQLite inspection of `_norn_inbox` row count so the oracle verifies
    /// zero leaked rows on rollback and zero duplicate rows on idempotent replay.
    fn sqlite_inbox_row_count(path: &Path) -> u64 {
        let engine = NativeEngine::open(path, StoreOptions::default()).expect("inspector sqlite");
        let rows = engine
            .query("SELECT COUNT(*) FROM \"_norn_inbox\"", &[])
            .expect("count _norn_inbox");
        match rows.first().and_then(|r| r.first()) {
            Some(SqlValue::Integer(count)) => u64::try_from(*count).expect("non-negative count"),
            other => panic!("unexpected COUNT(*) result: {other:?}"),
        }
    }

    fn erpnext_profile() -> PullProfile {
        let connector =
            load_artifact("fixtures/packs/erpnext-rest/artifacts/erpnext.rest.connector.json");
        let payload = serde_json::to_value(connector.payload.to_json_value()).expect("payload");
        PullProfile::from_connector(&payload)
            .expect("valid")
            .expect("pull profile")
    }

    fn page_cursor_json(page: u64) -> String {
        let wm = Watermark {
            version: format!("2026-11-12 12:00:0{page}.000000"),
            id: RuntimeValue::from(format!("CUST-C2-{page:02}")),
        };
        wm.cursor_json().expect("cursor json")
    }

    fn build_page_row(
        profile: &PullProfile,
        page: u64,
        tampered: bool,
    ) -> (Observed<RuntimeValue>, MappingInput) {
        let label = if tampered {
            format!("Tampered C2 Party {page}")
        } else {
            format!("C2 Party {page}")
        };
        let source_id = format!("CUST-C2-{page:02}");
        let version = format!("2026-11-12 12:00:0{page}.000000");
        let payload = RuntimeValue::from_json(json!({
            "name": source_id,
            "custom_norn_ref": format!("demo/party/c2-{page}"),
            "label": label,
            "role": "customer",
        }))
        .expect("payload rv");
        let row = Row {
            dataset: DATASET.to_owned(),
            source_id,
            version,
            payload,
        };
        let raw = observation(profile, &row, T0.saturating_add(page)).expect("observation");
        let input = MappingInput {
            mapping: "erpnext.rest.mapping".to_owned(),
            dataset: DATASET.to_owned(),
            connector_instance: "sandbox".to_owned(),
            partition: DATASET.to_owned(),
            recorded_at: UtcTimestamp(RECORDED_AT.to_owned()),
        };
        (raw, input)
    }

    fn advance_spec(lease: &SourceLease, page: u64, now_ms: u64) -> CursorAdvanceSpec {
        CursorAdvanceSpec {
            connection_id: CONNECTION_ID.to_owned(),
            dataset: DATASET.to_owned(),
            lease_id: lease.lease_id.clone(),
            generation: lease.generation,
            now_ms,
            expected_previous_cursor: (page > 1).then(|| page_cursor_json(page.saturating_sub(1))),
            next_cursor: Some(page_cursor_json(page)),
            renew_ttl_ms: None,
        }
    }

    /// Reference oracle mirroring the 6 Quint invariants of `pull-cursor-atomicity.qnt`
    /// and checking them against the real SQLite tables (`_norn_source_lease`,
    /// `_norn_source_cursor`, `_norn_source_pending`, `_norn_inbox`, and canonical
    /// `canon.Party`) after every single transition.
    struct Oracle {
        max_gen_seen: u64,
        expected_cursor_page: u64,
        expected_inbox_rows: u64,
        captured_records: BTreeMap<u64, Vec<String>>,
        landed_pages: BTreeSet<u64>,
    }

    impl Oracle {
        fn new() -> Self {
            Self {
                max_gen_seen: 0,
                expected_cursor_page: 0,
                expected_inbox_rows: 0,
                captured_records: BTreeMap::new(),
                landed_pages: BTreeSet::new(),
            }
        }

        fn check_invariants(&self, host: &DevHost<FaultEngine>, db_path: &Path) {
            // 1. leaseGenerationMonotonic
            let lease = host.source_lease(CONNECTION_ID).expect("lease read");
            if let Some(lease) = &lease {
                assert_eq!(
                    lease.generation, self.max_gen_seen,
                    "leaseGenerationMonotonic violated: db gen {} != max {}",
                    lease.generation, self.max_gen_seen
                );
                assert!(lease.generation >= 1);
            } else {
                assert_eq!(
                    self.max_gen_seen, 0,
                    "lease row disappeared after generation {}",
                    self.max_gen_seen
                );
            }

            // 2. cursorInboxAtomic & 3. noDoubleCapture against real _norn_inbox row count
            let actual_inbox = sqlite_inbox_row_count(db_path);
            assert_eq!(
                actual_inbox, self.expected_inbox_rows,
                "cursorInboxAtomic/noDoubleCapture violated: _norn_inbox has {actual_inbox} rows, expected {}",
                self.expected_inbox_rows
            );
            let cursors = host.source_cursors(CONNECTION_ID).expect("cursors");
            let current = cursors.get(DATASET);
            if self.expected_cursor_page == 0 {
                assert!(
                    current.is_none(),
                    "cursor advanced without admitted capture"
                );
            } else {
                let rec = current.expect("durable cursor must exist");
                assert_eq!(
                    rec.cursor_json,
                    page_cursor_json(self.expected_cursor_page),
                    "cursor mismatch"
                );
            }

            // 4. pendingRecoversAll: every captured page <= cursor is either
            // canonically landed in `canon.Party` or still present in `_norn_source_pending`.
            let pending = host
                .pending_source_captures(CONNECTION_ID)
                .expect("pending read");
            let pending_records: BTreeSet<String> = pending
                .iter()
                .flat_map(|p| p.records.iter().cloned())
                .collect();
            for page in 1..=self.expected_cursor_page {
                let recs = self
                    .captured_records
                    .get(&page)
                    .expect("captured page must have recorded observation ids");
                assert_eq!(recs.len(), 1, "noDoubleCapture: one record per page");
                if self.landed_pages.contains(&page) {
                    let obj = host
                        .host_object("canon.Party", &format!("demo/party/c2-{page}"))
                        .expect("canon.Party read");
                    assert!(
                        obj.is_some(),
                        "landed page {page} must exist in canonical canon.Party table"
                    );
                } else {
                    assert!(
                        pending_records.contains(&recs[0]),
                        "pendingRecoversAll violated: page {page} not landed and missing from _norn_source_pending"
                    );
                }
            }
        }
    }

    #[derive(Default)]
    struct CoverageStats {
        max_page_captured: u64,
        forward_faults_rolled_back: u64,
        ledger_faults_with_pending: u64,
        recoveries_landed: u64,
        idempotent_replays: u64,
        tampered_conflicts: u64,
        fenced_writes_refused: u64,
    }

    struct Lcg(u64);
    impl Lcg {
        fn next(&mut self) -> u64 {
            self.0 = self
                .0
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1);
            self.0 >> 32
        }
    }

    struct FsmState<'a> {
        artifacts: &'a [ArtifactEnvelope],
        profile: &'a PullProfile,
        db_path: &'a Path,
        host: DevHost<FaultEngine>,
        fail_cursor: Arc<AtomicBool>,
        fail_ledger: Arc<AtomicBool>,
        rng: Lcg,
        now: u64,
        workers: [Option<SourceLease>; 2],
        oracle: Oracle,
    }

    impl FsmState<'_> {
        fn step_capture(&mut self, w_idx: usize, stats: &mut CoverageStats) {
            let Some(held) = self.workers[w_idx].clone() else {
                return;
            };
            let next_page = self
                .oracle
                .expected_cursor_page
                .saturating_add(1)
                .min(MAX_PAGES);
            let try_replay =
                self.oracle.expected_cursor_page > 0 && self.rng.next().is_multiple_of(3);
            let target_page = if try_replay {
                self.oracle.expected_cursor_page
            } else {
                next_page
            };
            let inject_fault = !try_replay && self.rng.next().is_multiple_of(4);
            let tampered = try_replay && self.rng.next().is_multiple_of(2);
            let row = build_page_row(self.profile, target_page, tampered);
            let spec = advance_spec(&held, target_page, self.now);
            let db_lease = self.host.source_lease(CONNECTION_ID).expect("lease");
            let lease_valid = db_lease.as_ref().is_some_and(|l| {
                l.lease_id == held.lease_id
                    && l.generation == held.generation
                    && self.now < l.expires_at_ms
            });
            self.fail_cursor.store(inject_fault, Ordering::Release);
            let result = self.host.capture_source_page_atomic(&[row], &spec, 64);
            self.fail_cursor.store(false, Ordering::Release);

            if !lease_valid {
                assert!(
                    matches!(result, Err(SourceCaptureError::LeaseFenced(_))),
                    "noFencedWrite: stale/expired worker must be fenced"
                );
                stats.fenced_writes_refused = stats.fenced_writes_refused.saturating_add(1);
            } else if tampered {
                assert!(
                    matches!(result, Err(SourceCaptureError::CursorConflict(_))),
                    "tampered duplicate payload must conflict"
                );
                stats.tampered_conflicts = stats.tampered_conflicts.saturating_add(1);
            } else if target_page <= self.oracle.expected_cursor_page {
                let replayed = result.expect("idempotent replay must admit");
                assert_eq!(
                    &replayed.records,
                    self.oracle
                        .captured_records
                        .get(&target_page)
                        .expect("recorded"),
                    "noDoubleCapture: replay must return identical record ids"
                );
                stats.idempotent_replays = stats.idempotent_replays.saturating_add(1);
            } else if inject_fault {
                assert!(result.is_err(), "injected cursor fault must roll back");
                stats.forward_faults_rolled_back =
                    stats.forward_faults_rolled_back.saturating_add(1);
            } else {
                let captured = result.expect("valid forward capture must commit");
                let after_inbox = sqlite_inbox_row_count(self.db_path);
                assert!(
                    after_inbox > self.oracle.expected_inbox_rows,
                    "cursorInboxAtomic: admitted forward capture must persist _norn_inbox rows"
                );
                self.oracle.expected_cursor_page = target_page;
                self.oracle.expected_inbox_rows = after_inbox;
                self.oracle
                    .captured_records
                    .insert(target_page, captured.records);
                stats.max_page_captured = stats.max_page_captured.max(target_page);
            }
        }

        fn step_recover(&mut self, with_reopen: bool, stats: &mut CoverageStats) {
            let had_pending = !self
                .host
                .pending_source_captures(CONNECTION_ID)
                .expect("pending")
                .is_empty();
            let inject_ledger = with_reopen && had_pending && self.rng.next().is_multiple_of(2);
            self.fail_ledger.store(inject_ledger, Ordering::Release);
            let res = self.host.recover_pending_source_pages(CONNECTION_ID);
            self.fail_ledger.store(false, Ordering::Release);
            if inject_ledger {
                assert!(res.is_err(), "injected ledger fault must fail recovery");
                stats.ledger_faults_with_pending =
                    stats.ledger_faults_with_pending.saturating_add(1);
            } else {
                let ingested = res.expect("clean recovery must succeed");
                for item in &ingested {
                    assert!(
                        !matches!(item.outcome, MappingOutcome::Quarantined { .. }),
                        "valid C2 Party pages must land canonically, got {:?}",
                        item.outcome
                    );
                }
                if had_pending {
                    stats.recoveries_landed = stats.recoveries_landed.saturating_add(1);
                }
                for page in 1..=self.oracle.expected_cursor_page {
                    self.oracle.landed_pages.insert(page);
                }
            }
            if with_reopen {
                let (reopened, fc, fl) = open_host(self.artifacts, self.db_path, false);
                self.host = reopened;
                self.fail_cursor = fc;
                self.fail_ledger = fl;
            }
        }

        fn step_once(&mut self, stats: &mut CoverageStats) {
            let w_idx = usize::try_from(self.rng.next() % 2).expect("0 or 1");
            let lease_id = if w_idx == 0 { "worker-1" } else { "worker-2" };
            match self.rng.next() % 10 {
                0..=4 => self.step_capture(w_idx, stats),
                5..=6 => {
                    let active = self.host.source_lease(CONNECTION_ID).expect("lease");
                    let res =
                        self.host
                            .acquire_source_lease(CONNECTION_ID, lease_id, self.now, TTL_MS);
                    let active_other = active
                        .as_ref()
                        .is_some_and(|l| self.now < l.expires_at_ms && l.lease_id != lease_id);
                    if active_other {
                        assert!(matches!(res, Err(SourceCaptureError::LeaseFenced(_))));
                    } else {
                        let lease = res.expect("lease acquisition must succeed");
                        self.oracle.max_gen_seen = self.oracle.max_gen_seen.saturating_add(1);
                        assert_eq!(lease.generation, self.oracle.max_gen_seen);
                        self.workers[w_idx] = Some(lease);
                    }
                }
                7 => self.now = self.now.saturating_add(TICK_MS),
                8 => self.step_recover(true, stats),
                _ => {
                    if let Some(held) = &self.workers[w_idx] {
                        let released = self
                            .host
                            .release_source_lease(CONNECTION_ID, &held.lease_id, held.generation)
                            .expect("release");
                        let db_lease = self.host.source_lease(CONNECTION_ID).expect("lease");
                        if db_lease.as_ref().is_some_and(|l| {
                            l.lease_id == held.lease_id && l.generation == held.generation
                        }) {
                            assert!(released);
                        } else {
                            assert!(!released, "stale release must not succeed");
                        }
                    } else {
                        self.step_recover(false, stats);
                    }
                }
            }
            self.oracle.check_invariants(&self.host, self.db_path);
        }
    }

    #[test]
    fn sqlite_state_machine_preserves_all_pull_cursor_atomicity_invariants() {
        let artifacts = rest_artifacts();
        let profile = erpnext_profile();
        let mut stats = CoverageStats::default();

        for seed in 1_u64..=12 {
            let db = TempDb::new(&format!("fsm-{seed}"));
            let (host, fail_cursor, fail_ledger) = open_host(&artifacts, db.path(), true);
            let mut fsm = FsmState {
                artifacts: &artifacts,
                profile: &profile,
                db_path: db.path(),
                host,
                fail_cursor,
                fail_ledger,
                rng: Lcg(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15)),
                now: T0,
                workers: [None, None],
                oracle: Oracle::new(),
            };
            for _step in 0..40 {
                fsm.step_once(&mut stats);
            }
        }

        // Non-vacuity: verify the state-machine run actually exercised every fault
        // and multi-page path rather than staying stuck on page 1.
        assert!(
            stats.max_page_captured >= 2,
            "FSM must advance across multiple pages (max={})",
            stats.max_page_captured
        );
        assert!(stats.forward_faults_rolled_back >= 1);
        assert!(stats.ledger_faults_with_pending >= 1);
        assert!(stats.recoveries_landed >= 1);
        assert!(stats.idempotent_replays >= 1);
        assert!(stats.tampered_conflicts >= 1);
        assert!(stats.fenced_writes_refused >= 1);
    }

    #[test]
    fn sqlite_negative_mutants_are_refuted_by_engine_and_invariant_oracle() {
        let artifacts = rest_artifacts();
        let profile = erpnext_profile();
        let db = TempDb::new("mutants");
        let (host, fail_cursor, fail_ledger) = open_host(&artifacts, db.path(), true);

        // M1 (unfencedCapture) + M5 (staleRelease) + M6 (resetGenOnRelease):
        let lease1 = host
            .acquire_source_lease(CONNECTION_ID, "worker-1", T0, TTL_MS)
            .expect("lease 1");
        assert_eq!(lease1.generation, 1);
        assert!(
            host.release_source_lease(CONNECTION_ID, &lease1.lease_id, lease1.generation)
                .expect("release 1")
        );
        let lease2 = host
            .acquire_source_lease(CONNECTION_ID, "worker-2", T0.saturating_add(1_000), TTL_MS)
            .expect("lease 2");
        assert_eq!(lease2.generation, 2);
        assert!(
            !host
                .release_source_lease(CONNECTION_ID, &lease1.lease_id, lease1.generation)
                .expect("stale release")
        );
        let row1 = build_page_row(&profile, 1, false);
        let stale_spec = advance_spec(&lease1, 1, T0.saturating_add(1_000));
        assert!(matches!(
            host.capture_source_page_atomic(std::slice::from_ref(&row1), &stale_spec, 64),
            Err(SourceCaptureError::LeaseFenced(_))
        ));
        assert_eq!(sqlite_inbox_row_count(db.path()), 0);

        // M2 (splitCursorCommit): cursor fault rolls back _norn_inbox, pending, and cursor atomically.
        let valid_spec = advance_spec(&lease2, 1, T0.saturating_add(2_000));
        fail_cursor.store(true, Ordering::Release);
        assert!(
            host.capture_source_page_atomic(std::slice::from_ref(&row1), &valid_spec, 64)
                .is_err()
        );
        fail_cursor.store(false, Ordering::Release);
        assert_eq!(sqlite_inbox_row_count(db.path()), 0);
        assert!(
            host.source_cursors(CONNECTION_ID)
                .expect("cursors")
                .is_empty()
        );
        assert_eq!(
            host.pending_source_captures(CONNECTION_ID)
                .expect("pending"),
            Vec::new()
        );

        // M3 (noIdempotentReplay): exact duplicate writes zero new _norn_inbox rows; tampered conflicts.
        let captured = host
            .capture_source_page_atomic(std::slice::from_ref(&row1), &valid_spec, 64)
            .expect("page 1 commits");
        let inbox_after_capture = sqlite_inbox_row_count(db.path());
        assert!(inbox_after_capture >= 1);
        let replayed = host
            .capture_source_page_atomic(std::slice::from_ref(&row1), &valid_spec, 64)
            .expect("exact duplicate replays idempotently");
        assert_eq!(replayed.records, captured.records);
        assert_eq!(sqlite_inbox_row_count(db.path()), inbox_after_capture);
        let tampered_row1 = build_page_row(&profile, 1, true);
        assert!(matches!(
            host.capture_source_page_atomic(&[tampered_row1], &valid_spec, 64),
            Err(SourceCaptureError::CursorConflict(_))
        ));
        assert_eq!(sqlite_inbox_row_count(db.path()), inbox_after_capture);

        // M4 (dropPendingBeforeLand): ledger fault leaves pending open across reopen,
        // and clean recovery lands canon.Party and clears _norn_source_pending.
        fail_ledger.store(true, Ordering::Release);
        assert!(host.recover_pending_source_pages(CONNECTION_ID).is_err());
        fail_ledger.store(false, Ordering::Release);
        assert_eq!(
            host.pending_source_captures(CONNECTION_ID)
                .expect("pending stays open")
                .len(),
            1
        );
        drop(host);
        let (reopened, _, _) = open_host(&artifacts, db.path(), false);
        let recovered = reopened
            .recover_pending_source_pages(CONNECTION_ID)
            .expect("post-reopen recovery");
        assert_eq!(recovered.len(), 1);
        assert!(
            !matches!(recovered[0].outcome, MappingOutcome::Quarantined { .. }),
            "recovered row must land canonically"
        );
        assert!(
            reopened
                .host_object("canon.Party", "demo/party/c2-1")
                .expect("read canon.Party")
                .is_some()
        );
        assert_eq!(
            reopened
                .pending_source_captures(CONNECTION_ID)
                .expect("pending cleared after landing"),
            Vec::new()
        );
    }
}
