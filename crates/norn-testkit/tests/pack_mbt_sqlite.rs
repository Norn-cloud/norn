//! Real-`SqliteCell` validation of the generic IR-driven MBT, Domain Model
//! Coverage Observer, `LAW-EXAMPLE-1` Runtime Parity, Static Structural Gap
//! Detector, and Seeded Multi-Entity DST (#1656).
//!
//! Exercises real `origin/main` registry packs (`ml`, `tracking`, `canon`,
//! `time`, `org`) plus an Order-to-Cash SoD & immutable-terminal pack against
//! file-backed [`ObservedSqliteCell`] with crash/reopen and `verify_ledger()`.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use norn_cell::{
    CellError, DomainCoverageRecorder, Model, ObservedSqliteCell, SqliteCell, StoreOptions,
};
use norn_ir::{ArtifactEnvelope, Decimal, RuntimeObject, RuntimeValue};
use norn_kernel::cell_host::{FactKey, FactState, JsonObject};
use norn_kernel::norn_checker::law_examples::Verdict;
use norn_kernel::{
    BoundedFact, CellCommit, CellHost, CommitError, CommitReceipt, DueTimer, FactEvent, LinkKey,
    LinkReadTarget, LinkState, LogicalTime, Observed, OwnedRowsCommitProof, Performed,
    PredicateObject, PredicateQuery,
};
use norn_testkit::pack_mbt::driver::run_pack_mbt;
use norn_testkit::pack_mbt::dst::{PackDstConfig, run_pack_dst};
use norn_testkit::pack_mbt::static_gaps::{GapSeverity, StructuralGap, analyze_static_gaps};
use norn_testkit::pack_mbt::{AdmittedPack, LawExampleCase, PackHost, action_call_object_type};
use serde_json::json;
use tempfile::TempDir;

type BaseCell = SqliteCell<FactEvent, JsonObject, JsonObject>;

/// File-backed [`ObservedSqliteCell`] implementing [`PackHost`] with real
/// SQLite crash/reopen and `verify_ledger()` integrity verification.
struct SqlitePackHost {
    _dir: TempDir,
    db_path: PathBuf,
    model: Model,
    scope: String,
    recorder: Arc<DomainCoverageRecorder>,
    cell: Option<ObservedSqliteCell<FactEvent, JsonObject, JsonObject>>,
}

impl SqlitePackHost {
    fn open(pack: &AdmittedPack, recorder: Arc<DomainCoverageRecorder>) -> Result<Self, String> {
        let dir = tempfile::tempdir().map_err(|err| format!("tempdir: {err}"))?;
        let db_path = dir.path().join("pack-mbt.sqlite");
        let mut all_types = pack.object_types.clone();
        all_types.push(action_call_object_type());
        let base_model = Model::from_definitions_with_interfaces(
            &all_types,
            &pack.property_types,
            &pack.interfaces,
        )
        .map_err(|err| format!("Model::from_definitions_with_interfaces: {err}"))?;
        let model = base_model
            .with_governing_laws(&pack.law_envelopes)
            .map_err(|err| format!("Model::with_governing_laws: {err}"))?;
        let scope = "org-mbt/dev".to_owned();
        let inner = BaseCell::open(
            &db_path,
            model.clone(),
            StoreOptions::default(),
            scope.clone(),
            |cmd| cmd.payload.clone(),
        )
        .map_err(|err| format!("SqliteCell::open: {err}"))?;
        let cell = inner.into_observed(Arc::clone(&recorder));
        Ok(Self {
            _dir: dir,
            db_path,
            model,
            scope,
            recorder,
            cell: Some(cell),
        })
    }

    fn cell_ref(
        &self,
    ) -> Result<&ObservedSqliteCell<FactEvent, JsonObject, JsonObject>, CellError> {
        self.cell
            .as_ref()
            .ok_or_else(|| CellError::Engine("cell closed".to_owned()))
    }

    fn cell_mut(
        &mut self,
    ) -> Result<&mut ObservedSqliteCell<FactEvent, JsonObject, JsonObject>, CellError> {
        self.cell
            .as_mut()
            .ok_or_else(|| CellError::Engine("cell closed".to_owned()))
    }
}

impl CellHost for SqlitePackHost {
    type Event = FactEvent;
    type Command = JsonObject;
    type Input = JsonObject;
    type Error = CellError;

    fn load(&self, key: &FactKey) -> Result<FactState, CellError> {
        self.cell_ref()?.load(key)
    }

    fn supports_reference_reads(&self) -> bool {
        self.cell
            .as_ref()
            .is_some_and(CellHost::supports_reference_reads)
    }

    fn load_reference(
        &self,
        key: &FactKey,
        max_bytes: u64,
    ) -> Result<Option<BoundedFact>, CellError> {
        self.cell_ref()?.load_reference(key, max_bytes)
    }

    fn load_bounded(
        &self,
        key: &FactKey,
        max_bytes: u64,
    ) -> Result<Option<BoundedFact>, CellError> {
        self.cell_ref()?.load_bounded(key, max_bytes)
    }

    fn load_link(&self, key: &LinkKey) -> Result<LinkState, CellError> {
        self.cell_ref()?.load_link(key)
    }

    fn read_predicate(
        &self,
        predicate: &PredicateQuery,
        max_members: usize,
        max_bytes: u64,
    ) -> Result<Option<Vec<PredicateObject>>, CellError> {
        self.cell_ref()?
            .read_predicate(predicate, max_members, max_bytes)
    }

    fn read_link_targets(
        &self,
        source: &FactKey,
        link: &str,
        max_targets: usize,
        max_key_bytes: u64,
    ) -> Result<Option<Vec<LinkReadTarget>>, CellError> {
        self.cell_ref()?
            .read_link_targets(source, link, max_targets, max_key_bytes)
    }

    fn commit(
        &mut self,
        commit: CellCommit<FactEvent, JsonObject, JsonObject>,
    ) -> Result<CommitReceipt, CommitError<CellError>> {
        self.cell_mut().map_err(CommitError::Host)?.commit(commit)
    }

    fn commit_action_references(
        &mut self,
        commit: CellCommit<FactEvent, JsonObject, JsonObject>,
        proof: norn_kernel::action_commit::ActionReferenceReads,
        quantity: Option<(&norn_ir::quantity_ledger::QuantityRequest, &str)>,
    ) -> Result<CommitReceipt, CommitError<CellError>> {
        self.cell_mut()
            .map_err(CommitError::Host)?
            .commit_action_references(commit, proof, quantity)
    }

    fn commit_action_quantity(
        &mut self,
        commit: CellCommit<FactEvent, JsonObject, JsonObject>,
        requests: &[norn_ir::quantity_ledger::QuantityRequest],
        idempotency_key: &str,
    ) -> Result<CommitReceipt, CommitError<CellError>> {
        self.cell_mut()
            .map_err(CommitError::Host)?
            .commit_action_quantity(commit, requests, idempotency_key)
    }

    fn commit_owned_rows(
        &mut self,
        commit: CellCommit<FactEvent, JsonObject, JsonObject>,
        proof: OwnedRowsCommitProof,
    ) -> Result<CommitReceipt, CommitError<CellError>> {
        self.cell_mut()
            .map_err(CommitError::Host)?
            .commit_owned_rows(commit, proof)
    }

    fn perform(&mut self) -> Result<Option<Performed<JsonObject>>, CellError> {
        self.cell_mut()?.perform()
    }

    fn observe(&mut self) -> Result<Option<Observed<JsonObject>>, CellError> {
        self.cell_mut()?.observe()
    }

    fn clock(&mut self) -> Result<Option<DueTimer<JsonObject>>, CellError> {
        self.cell_mut()?.clock()
    }

    fn publish(&mut self, receipt: &CommitReceipt) -> Result<(), CellError> {
        self.cell_mut()?.publish(receipt)
    }
}

impl PackHost for SqlitePackHost {
    fn ledger_len(&self) -> usize {
        self.cell
            .as_ref()
            .and_then(|c| c.inner().store().read().receipts().ok())
            .map_or(0, |receipts| receipts.len())
    }

    fn logical_now(&self) -> LogicalTime {
        self.cell
            .as_ref()
            .map_or(LogicalTime::EPOCH, |c| c.inner().now())
    }

    fn advance_clock(&mut self, delta_millis: u64) {
        if let Some(cell) = self.cell.as_mut() {
            let next = cell.inner().now().as_millis().saturating_add(delta_millis);
            cell.inner_mut().advance_to(LogicalTime::from_millis(next));
        }
    }

    fn verify_ledger(&self) -> Result<(), String> {
        let cell = self.cell_ref().map_err(|err| format!("{err}"))?;
        cell.inner()
            .store()
            .read()
            .verify_ledger()
            .map(|_| ())
            .map_err(|err| format!("verify_ledger failed: {err}"))
    }

    fn crash_and_reopen(&mut self) -> Result<(), String> {
        let prev_now = self.logical_now();
        // Drop the live SqliteCell handle cleanly (closing SQLite) and reopen from disk.
        self.cell = None;
        let mut reopened = BaseCell::open(
            &self.db_path,
            self.model.clone(),
            StoreOptions::default(),
            self.scope.clone(),
            |cmd| cmd.payload.clone(),
        )
        .map_err(|err| format!("reopen after crash failed: {err}"))?;
        reopened.advance_to(prev_now);
        reopened
            .store()
            .read()
            .verify_ledger()
            .map_err(|err| format!("verify_ledger after reopen failed: {err}"))?;
        self.cell = Some(reopened.into_observed(Arc::clone(&self.recorder)));
        Ok(())
    }
}

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn load_envelopes_from_dir(dir: &Path) -> Result<Vec<ArtifactEnvelope>, String> {
    let mut paths: Vec<PathBuf> = fs::read_dir(dir)
        .map_err(|err| format!("read_dir {}: {err}", dir.display()))?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.extension().and_then(|ext| ext.to_str()) == Some("json"))
        .collect();
    paths.sort();
    let mut envelopes = Vec::with_capacity(paths.len());
    for path in paths {
        let raw =
            fs::read_to_string(&path).map_err(|err| format!("read {}: {err}", path.display()))?;
        let env: ArtifactEnvelope =
            serde_json::from_str(&raw).map_err(|err| format!("parse {}: {err}", path.display()))?;
        envelopes.push(env);
    }
    Ok(envelopes)
}

fn load_registry_pack(
    name: &str,
    pack_dirs: &[&str],
    include_core: bool,
) -> Result<AdmittedPack, String> {
    let root = repo_root();
    let mut envelopes = Vec::new();
    let mut seen_ids = BTreeSet::new();
    if include_core {
        for env in load_envelopes_from_dir(&root.join("registry/core/artifacts"))? {
            if seen_ids.insert(env.artifact_id.clone()) {
                envelopes.push(env);
            }
        }
    }
    for dir in pack_dirs {
        for env in
            load_envelopes_from_dir(&root.join("registry/packs").join(dir).join("artifacts"))?
        {
            if seen_ids.insert(env.artifact_id.clone()) {
                envelopes.push(env);
            }
        }
    }
    AdmittedPack::from_envelopes(name, &envelopes)
}

fn runtime_obj(pairs: &[(&str, RuntimeValue)]) -> RuntimeObject {
    pairs
        .iter()
        .map(|(k, v)| ((*k).to_owned(), v.clone()))
        .collect()
}

fn dec(text: &str) -> Result<RuntimeValue, String> {
    Decimal::parse_wire(text)
        .map(RuntimeValue::Number)
        .map_err(|err| format!("decimal {text}: {err}"))
}

#[test]
fn real_tracking_pack_passes_sqlite_mbt_and_dst() -> Result<(), String> {
    let pack = load_registry_pack("tracking", &["tracking"], false)?;
    let recorder = Arc::new(DomainCoverageRecorder::new());

    let mbt = run_pack_mbt(&pack, |_| {
        SqlitePackHost::open(&pack, Arc::clone(&recorder))
    })?;
    assert!(mbt.valid_transitions_run >= 3);
    assert!(mbt.disallowed_transitions_checked >= 9);
    mbt.coverage.check_transitions_covered()?;

    let dst = run_pack_dst(
        &pack,
        PackDstConfig {
            cases: 16,
            max_steps: 18,
            ..PackDstConfig::default()
        },
        |_| SqlitePackHost::open(&pack, Arc::clone(&recorder)),
    )?;
    assert_eq!(dst.cases_run, 16);
    assert!(dst.steps_run >= 16);

    let cell_snap = recorder.snapshot();
    assert!(!cell_snap.transitions.is_empty());

    let gaps = analyze_static_gaps(&pack);
    assert!(
        gaps.defects().is_empty(),
        "unexpected tracking defects:\n{gaps}"
    );
    Ok(())
}

#[test]
fn real_ml_pack_passes_sqlite_mbt_cedar_matrix_and_dst() -> Result<(), String> {
    let pack = load_registry_pack("ml", &["tracking", "ml"], false)?;
    let recorder = Arc::new(DomainCoverageRecorder::new());

    let mbt = run_pack_mbt(&pack, |_| {
        SqlitePackHost::open(&pack, Arc::clone(&recorder))
    })?;
    assert!(mbt.valid_transitions_run >= 6);
    assert!(mbt.cedar_checks_run >= 12);
    mbt.coverage.check_transitions_covered()?;
    mbt.coverage.check_cedar_covered()?;

    let dst = run_pack_dst(
        &pack,
        PackDstConfig {
            cases: 12,
            max_steps: 16,
            ..PackDstConfig::default()
        },
        |_| SqlitePackHost::open(&pack, Arc::clone(&recorder)),
    )?;
    assert_eq!(dst.cases_run, 12);

    // Static gap detector flags high-impact ML promote/retire lacking SoD as explicit Questions.
    let gaps = analyze_static_gaps(&pack);
    assert!(
        gaps.questions().iter().any(|g| matches!(
            g,
            StructuralGap::HighImpactActionMissingSod { action_id, .. }
                if action_id == "ml.modelversion.promote"
        )),
        "expected SoD question for ml.modelversion.promote:\n{gaps}"
    );
    Ok(())
}

#[test]
fn real_time_and_org_packs_pass_law_example_sqlite_runtime_parity() -> Result<(), String> {
    let time_pack = load_registry_pack("time", &["time"], true)?.with_law_examples(vec![
        LawExampleCase {
            name: "working window 09:00..17:00 holds".to_owned(),
            law_id: "time.WorkingWindow.valid_local_window".to_owned(),
            subject: runtime_obj(&[
                ("id", RuntimeValue::String("ww-1".to_owned())),
                ("weekday", RuntimeValue::String("monday".to_owned())),
                ("start_local", RuntimeValue::String("09:00".to_owned())),
                ("end_local", RuntimeValue::String("17:00".to_owned())),
            ]),
            expected: Verdict::Holds,
        },
        LawExampleCase {
            name: "working window 17:00..09:00 violates".to_owned(),
            law_id: "time.WorkingWindow.valid_local_window".to_owned(),
            subject: runtime_obj(&[
                ("id", RuntimeValue::String("ww-bad".to_owned())),
                ("weekday", RuntimeValue::String("monday".to_owned())),
                ("start_local", RuntimeValue::String("17:00".to_owned())),
                ("end_local", RuntimeValue::String("09:00".to_owned())),
            ]),
            expected: Verdict::Violated,
        },
    ]);
    let time_rec = Arc::new(DomainCoverageRecorder::new());
    let time_report = run_pack_mbt(&time_pack, |_| {
        SqlitePackHost::open(&time_pack, Arc::clone(&time_rec))
    })?;
    assert_eq!(time_report.law_examples_checked, 2);
    assert!(
        time_rec
            .snapshot()
            .law_holds
            .contains_key("time.WorkingWindow.valid_local_window")
    );
    assert!(
        time_rec
            .snapshot()
            .law_violations
            .contains_key("time.WorkingWindow.valid_local_window")
    );

    let org_pack = load_registry_pack("org", &["org"], true)?.with_law_examples(vec![
        LawExampleCase {
            name: "membership valid_from < valid_to holds".to_owned(),
            law_id: "org.Membership.valid_date_range".to_owned(),
            subject: runtime_obj(&[
                ("id", RuntimeValue::String("mem-1".to_owned())),
                ("role", RuntimeValue::String("engineer".to_owned())),
                ("valid_from", RuntimeValue::String("2026-01-01".to_owned())),
                ("valid_to", RuntimeValue::String("2026-12-31".to_owned())),
            ]),
            expected: Verdict::Holds,
        },
        LawExampleCase {
            name: "membership inverted date range violates".to_owned(),
            law_id: "org.Membership.valid_date_range".to_owned(),
            subject: runtime_obj(&[
                ("id", RuntimeValue::String("mem-bad".to_owned())),
                ("role", RuntimeValue::String("engineer".to_owned())),
                ("valid_from", RuntimeValue::String("2026-12-31".to_owned())),
                ("valid_to", RuntimeValue::String("2026-01-01".to_owned())),
            ]),
            expected: Verdict::Violated,
        },
    ]);
    let org_rec = Arc::new(DomainCoverageRecorder::new());
    let org_report = run_pack_mbt(&org_pack, |_| {
        SqlitePackHost::open(&org_pack, Arc::clone(&org_rec))
    })?;
    assert_eq!(org_report.law_examples_checked, 2);
    assert!(
        org_rec
            .snapshot()
            .law_holds
            .contains_key("org.Membership.valid_date_range")
    );
    assert!(
        org_rec
            .snapshot()
            .law_violations
            .contains_key("org.Membership.valid_date_range")
    );
    Ok(())
}

#[test]
fn real_canon_pack_runs_static_gap_and_mbt_cleanly() -> Result<(), String> {
    let canon_pack = load_registry_pack("canon", &["canon"], true)?;
    let gaps = analyze_static_gaps(&canon_pack);
    let canon_defects: Vec<_> = gaps
        .defects()
        .into_iter()
        .filter(|gap| match gap {
            StructuralGap::DeadEndNonTerminalState { object_type, .. }
            | StructuralGap::UnreachableState { object_type, .. }
            | StructuralGap::OrphanField { object_type, .. }
            | StructuralGap::ImmutableFinancialMissingReversal { object_type, .. }
            | StructuralGap::TransitionWithoutAction { object_type, .. } => {
                object_type.starts_with("canon.")
            }
            StructuralGap::HighImpactActionMissingSod { action_id, .. } => {
                action_id.starts_with("canon.")
            }
        })
        .collect();
    assert!(
        canon_defects.is_empty(),
        "canon.* entities should have no structural defects:\n{gaps}"
    );
    // Static Gap Detector genuinely flags real core.analytics.Restatement gaps.
    assert!(
        gaps.defects().iter().any(|gap| matches!(
            gap,
            StructuralGap::OrphanField { object_type, property, .. }
                if object_type == "core.analytics.Restatement" && property == "reason"
        )),
        "expected real OrphanField finding on core.analytics.Restatement:\n{gaps}"
    );
    assert!(
        gaps.questions().iter().any(|gap| matches!(
            gap,
            StructuralGap::HighImpactActionMissingSod { action_id, .. }
                if action_id == "core.analytics.restatement.approve"
        )),
        "expected SoD Question on core.analytics.restatement.approve:\n{gaps}"
    );
    let recorder = Arc::new(DomainCoverageRecorder::new());
    let report = run_pack_mbt(&canon_pack, |_| {
        SqlitePackHost::open(&canon_pack, Arc::clone(&recorder))
    })?;
    report.coverage.check_transitions_covered()?;
    report.coverage.check_immutable_refusals_covered()?;
    report.coverage.check_cedar_covered()?;
    Ok(())
}

fn o2c_invoice_entity_and_law() -> Vec<serde_json::Value> {
    vec![
        json!({
            "irVersion": "0.1", "artifactId": "o2c.Invoice.entity", "version": "1.0.0",
            "kind": "entity", "namespace": "o2c", "name": "Invoice", "lifecycle": "active",
            "scope": {}, "dependencies": [], "provenance": {"source": "system"},
            "payload": {
                "entityVersion": "2.0",
                "objectType": {
                    "name": "o2c.Invoice",
                    "identity": [{"name": "primary", "properties": ["invoice_id"]}],
                    "properties": [
                        {"id": "invoice_id", "type": {"kind": "text"}},
                        {"id": "requester", "type": {"kind": "text"}},
                        {"id": "amount", "type": {"kind": "decimal", "scale": 2}},
                        {"id": "memo", "type": {"kind": "text"}},
                        {
                            "id": "status",
                            "default": "draft",
                            "type": {
                                "kind": "enum",
                                "values": [
                                    {"value": "draft"},
                                    {"value": "submitted"},
                                    {"value": "approved"},
                                    {"value": "posted"},
                                    {"value": "cancelled"}
                                ]
                            }
                        }
                    ],
                    "lifecycles": [{
                        "property": "status",
                        "initial": ["draft"],
                        "terminal": ["posted", "cancelled"],
                        "immutable": ["posted"],
                        "transitions": [
                            {"id": "submit", "from": ["draft"], "to": "submitted", "guards": ["self.amount > 0"]},
                            {"id": "approve", "from": ["submitted"], "to": "approved"},
                            {"id": "post", "from": ["approved"], "to": "posted"},
                            {"id": "cancel", "from": ["draft", "submitted"], "to": "cancelled"}
                        ]
                    }]
                }
            }
        }),
        json!({
            "irVersion": "0.1", "artifactId": "o2c.Invoice.positive_amount.law", "version": "1.0.0",
            "kind": "law", "namespace": "o2c", "name": "Submitted/posted invoice amount positive", "lifecycle": "active",
            "scope": {}, "dependencies": [], "provenance": {"source": "system"},
            "payload": {
                "lawVersion": "0.1",
                "id": "o2c.Invoice.positive_amount",
                "owner": "o2c",
                "severity": "error",
                "rationale": "Submitted and posted invoices must carry a positive amount.",
                "provenance": {"source": "authoring_sdk"},
                "scope": {"root": {"kind": "entity", "id": "o2c.Invoice"}, "objectTypes": ["o2c.Invoice"]},
                "subject": "o2c.Invoice",
                "condition": "self.status != 'cancelled'",
                "modality": {"kind": "always"},
                "response": {"kind": "predicate", "expression": "self.amount > 0"}
            }
        }),
    ]
}

fn o2c_actions_and_policy() -> Vec<serde_json::Value> {
    vec![
        json!({
            "irVersion": "0.1", "artifactId": "o2c.invoice.create.action", "version": "1.0.0",
            "kind": "action", "namespace": "o2c", "name": "Create invoice", "lifecycle": "active",
            "scope": {}, "dependencies": [], "provenance": {"source": "system"},
            "payload": {
                "actionId": "o2c.invoice.create",
                "scopeRequired": ["orgId", "environmentId"],
                "idempotent": true,
                "ledgered": true,
                "inputSchema": {
                    "type": "object",
                    "additionalProperties": false,
                    "properties": {
                        "invoice_id": {"type": "string"},
                        "requester": {"type": "string"},
                        "amount": {"type": "number", "nornType": {"kind": "decimal", "scale": 2}},
                        "memo": {"type": "string"}
                    },
                    "required": ["invoice_id", "requester", "amount", "memo"]
                },
                "outputSchema": {"type": "object", "additionalProperties": false, "properties": {"invoice_id": {"type": "string"}}, "required": ["invoice_id"]},
                "effectProgram": {
                    "kind": "upsert_input_object",
                    "objectIdField": "invoice_id",
                    "objectKind": "o2c.Invoice",
                    "entityArtifactId": "o2c.Invoice.entity",
                    "entityArtifactVersion": "1.0.0",
                    "writeMode": {"kind": "create_only"}
                },
                "writes": [{"entityKind": "o2c.Invoice", "change": "created"}]
            }
        }),
        json!({
            "irVersion": "0.1", "artifactId": "o2c.invoice.approve.action", "version": "1.0.0",
            "kind": "action", "namespace": "o2c", "name": "Approve invoice", "lifecycle": "active",
            "scope": {}, "dependencies": [], "provenance": {"source": "system"},
            "payload": {
                "actionId": "o2c.invoice.approve",
                "scopeRequired": ["orgId", "environmentId"],
                "idempotent": true,
                "ledgered": true,
                "inputSchema": {
                    "type": "object",
                    "additionalProperties": false,
                    "properties": {"invoice_id": {"type": "string"}},
                    "required": ["invoice_id"]
                },
                "outputSchema": {"type": "object", "additionalProperties": false, "properties": {"invoice_id": {"type": "string"}}, "required": ["invoice_id"]},
                "effectProgram": {
                    "kind": "fire_transition",
                    "objectIdField": "invoice_id",
                    "objectKind": "o2c.Invoice",
                    "entityArtifactId": "o2c.Invoice.entity",
                    "entityArtifactVersion": "1.0.0",
                    "lifecycle": "status",
                    "transition": "approve"
                },
                "writes": [{"entityKind": "o2c.Invoice", "change": "modified"}],
                "compensation": {"actionId": "o2c.invoice.reverse"}
            }
        }),
        json!({
            "irVersion": "0.1", "artifactId": "o2c.access.policy", "version": "1.0.0",
            "kind": "policy", "namespace": "o2c", "name": "Invoice access & SoD", "lifecycle": "active",
            "scope": {}, "dependencies": [], "provenance": {"source": "system"},
            "payload": {
                "accessVersion": "0.1",
                "policyId": "o2c.access",
                "rules": [
                    {
                        "ruleId": "approve_with_sod",
                        "actions": ["o2c.invoice.approve"],
                        "resourceType": "o2c.Invoice",
                        "scope": {"dimensions": ["org", "environment"]},
                        "allOf": [
                            {"role": {"anyOf": ["finance_approver"]}},
                            {"resource": {"name": "requester", "op": "ne", "value": {"principalAttribute": "subject_id"}}}
                        ]
                    }
                ]
            }
        }),
    ]
}

fn build_o2c_sod_pack() -> Result<AdmittedPack, String> {
    let mut raw_envelopes = o2c_invoice_entity_and_law();
    raw_envelopes.extend(o2c_actions_and_policy());
    let envelopes: Vec<ArtifactEnvelope> = raw_envelopes
        .into_iter()
        .map(|val| serde_json::from_value(val).map_err(|err| format!("envelope: {err}")))
        .collect::<Result<Vec<_>, _>>()?;
    let pack = AdmittedPack::from_envelopes("o2c_sod", &envelopes)?.with_law_examples(vec![
        LawExampleCase {
            name: "positive invoice holds".to_owned(),
            law_id: "o2c.Invoice.positive_amount".to_owned(),
            subject: runtime_obj(&[
                ("invoice_id", RuntimeValue::String("INV-1".to_owned())),
                ("requester", RuntimeValue::String("alice".to_owned())),
                ("amount", dec("250.00")?),
                ("memo", RuntimeValue::String("Q4 services".to_owned())),
                ("status", RuntimeValue::String("draft".to_owned())),
            ]),
            expected: Verdict::Holds,
        },
        LawExampleCase {
            name: "zero amount invoice violates".to_owned(),
            law_id: "o2c.Invoice.positive_amount".to_owned(),
            subject: runtime_obj(&[
                ("invoice_id", RuntimeValue::String("INV-2".to_owned())),
                ("requester", RuntimeValue::String("alice".to_owned())),
                ("amount", dec("0.00")?),
                ("memo", RuntimeValue::String("invalid".to_owned())),
                ("status", RuntimeValue::String("draft".to_owned())),
            ]),
            expected: Verdict::Violated,
        },
    ]);
    Ok(pack)
}

#[test]
fn o2c_sod_pack_achieves_100_percent_domain_coverage_with_crash_reopen_dst() -> Result<(), String> {
    let pack = build_o2c_sod_pack()?;
    let recorder = Arc::new(DomainCoverageRecorder::new());

    let mbt = run_pack_mbt(&pack, |_| {
        SqlitePackHost::open(&pack, Arc::clone(&recorder))
    })?;
    // Every declared transition, immutable terminal refusal, Cedar/SoD rule, and law must be 100% covered!
    mbt.coverage.check_transitions_covered()?;
    mbt.coverage.check_immutable_refusals_covered()?;
    mbt.coverage.check_cedar_covered()?;
    mbt.coverage.check_laws_covered()?;
    assert!(mbt.immutable_refusals_checked >= 1);
    assert!(mbt.cedar_checks_run >= 4);
    assert_eq!(mbt.law_examples_checked, 2);

    // Seeded multi-entity DST with virtual clock steps and real SQLite crash/reopen + verify_ledger().
    let dst = run_pack_dst(
        &pack,
        PackDstConfig {
            seed: [0x42; 32],
            cases: 24,
            max_steps: 24,
            entities_per_type: 2,
        },
        |_| SqlitePackHost::open(&pack, Arc::clone(&recorder)),
    )?;
    assert_eq!(dst.cases_run, 24);
    assert!(
        dst.crashes_recovered >= 1,
        "expected at least one SQLite crash/reopen cycle"
    );

    // Verify Static Gap Detector confirms approve action DOES have SoD (not flagged).
    let gaps = analyze_static_gaps(&pack);
    assert!(
        !gaps.questions().iter().any(|g| matches!(
            g,
            StructuralGap::HighImpactActionMissingSod { action_id, .. }
                if action_id == "o2c.invoice.approve"
        )),
        "o2c.invoice.approve already has SoD policy:\n{gaps}"
    );
    Ok(())
}

fn build_gap_pack() -> Result<AdmittedPack, String> {
    let raw_envelopes = vec![
        json!({
            "irVersion": "0.1", "artifactId": "gap.JournalEntry.entity", "version": "1.0.0",
            "kind": "entity", "namespace": "gap", "name": "JournalEntry", "lifecycle": "active",
            "scope": {}, "dependencies": [], "provenance": {"source": "system"},
            "payload": {
                "entityVersion": "2.0",
                "objectType": {
                    "name": "gap.JournalEntry",
                    "immutable": true,
                    "properties": [
                        {"id": "entry_id", "type": {"kind": "text"}},
                        {"id": "orphan_flag", "type": {"kind": "boolean"}},
                        {
                            "id": "status",
                            "default": "open",
                            "type": {
                                "kind": "enum",
                                "values": [
                                    {"value": "open"},
                                    {"value": "stuck"},
                                    {"value": "ghost"},
                                    {"value": "closed"}
                                ]
                            }
                        }
                    ],
                    "lifecycles": [{
                        "property": "status",
                        "initial": ["open"],
                        "terminal": ["closed"],
                        "immutable": ["closed"],
                        "transitions": [
                            {"id": "stall", "from": ["open"], "to": "stuck", "guards": ["self.orphan_flag"]},
                            {"id": "close", "from": ["open"], "to": "closed"}
                        ]
                    }]
                }
            }
        }),
        json!({
            "irVersion": "0.1", "artifactId": "gap.journal.post.action", "version": "1.0.0",
            "kind": "action", "namespace": "gap", "name": "Post journal", "lifecycle": "active",
            "scope": {}, "dependencies": [], "provenance": {"source": "system"},
            "payload": {
                "actionId": "gap.journal.post",
                "scopeRequired": ["orgId"],
                "idempotent": true,
                "ledgered": true,
                "inputSchema": {
                    "type": "object",
                    "additionalProperties": false,
                    "properties": {"entry_id": {"type": "string"}},
                    "required": ["entry_id"]
                },
                "outputSchema": {"type": "object", "additionalProperties": false, "properties": {"entry_id": {"type": "string"}}, "required": ["entry_id"]},
                "effectProgram": {
                    "kind": "upsert_input_object",
                    "objectIdField": "entry_id",
                    "objectKind": "gap.JournalEntry",
                    "entityArtifactId": "gap.JournalEntry.entity",
                    "entityArtifactVersion": "1.0.0",
                    "writeMode": {"kind": "create_only"}
                },
                "writes": [{"entityKind": "gap.JournalEntry", "change": "created"}]
            }
        }),
    ];
    let envelopes: Vec<ArtifactEnvelope> = raw_envelopes
        .into_iter()
        .map(|val| serde_json::from_value(val).map_err(|err| format!("envelope: {err}")))
        .collect::<Result<Vec<_>, _>>()?;
    AdmittedPack::from_envelopes("gap_pack", &envelopes)
}

#[test]
fn static_gap_detector_catches_every_structural_gap_class() -> Result<(), String> {
    let pack = build_gap_pack()?;
    let report = analyze_static_gaps(&pack);

    assert!(report.gaps.iter().any(|g| matches!(
        g,
        StructuralGap::DeadEndNonTerminalState { state, .. } if state == "stuck"
    )));
    assert!(report.gaps.iter().any(|g| matches!(
        g,
        StructuralGap::UnreachableState { state, .. } if state == "ghost"
    )));
    assert!(report.gaps.iter().any(|g| matches!(
        g,
        StructuralGap::OrphanField { property, .. } if property == "orphan_flag"
    )));
    assert!(report.gaps.iter().any(|g| matches!(
        g,
        StructuralGap::ImmutableFinancialMissingReversal { object_type, .. }
            if object_type == "gap.JournalEntry"
    )));
    assert!(report.gaps.iter().any(|g| matches!(
        g,
        StructuralGap::HighImpactActionMissingSod { action_id, .. }
            if action_id == "gap.journal.post" && g.severity() == GapSeverity::Question
    )));
    assert!(report.gaps.iter().any(|g| matches!(
        g,
        StructuralGap::TransitionWithoutAction { transition_id, .. } if transition_id == "close"
    )));
    Ok(())
}

#[test]
fn law_example_parity_catches_wrong_expectation() -> Result<(), String> {
    let mut pack = build_o2c_sod_pack()?;
    pack.law_examples = vec![LawExampleCase {
        name: "deliberately wrong expectation".to_owned(),
        law_id: "o2c.Invoice.positive_amount".to_owned(),
        subject: runtime_obj(&[
            ("invoice_id", RuntimeValue::String("INV-BAD".to_owned())),
            ("requester", RuntimeValue::String("alice".to_owned())),
            ("amount", dec("0.00")?),
            ("memo", RuntimeValue::String("bad".to_owned())),
            ("status", RuntimeValue::String("draft".to_owned())),
        ]),
        expected: Verdict::Holds,
    }];
    let recorder = Arc::new(DomainCoverageRecorder::new());
    let err = run_pack_mbt(&pack, |_| {
        SqlitePackHost::open(&pack, Arc::clone(&recorder))
    });
    assert!(err.is_err(), "expected LAW-EXAMPLE-1 mismatch to fail");
    Ok(())
}
