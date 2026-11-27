//! Real-`SqliteCell` validation of the generic IR-driven MBT, Domain Model
//! Coverage Observer, `LAW-EXAMPLE-1` Runtime Parity, Static Structural Gap
//! Detector, and Seeded Multi-Entity DST (#1656).
//!
//! Exercises real `origin/main` registry packs (`ml`, `tracking`, `canon`,
//! `time`, `org`) plus an Order-to-Cash `SoD` & immutable-terminal pack against
//! file-backed [`ObservedSqliteCell`] with clean reopen and `verify_ledger()`.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use norn_cell::function_receipt::{FUNCTION_COMMAND_TYPE, FUNCTION_PAYLOAD_TYPE};
use norn_cell::{
    CellError, DomainCoverageRecorder, DomainCoverageSnapshot, Model, ObservedSqliteCell,
    SqliteCell, StoreOptions,
};
use norn_ir::function::{
    Authority, AuthorityKind, CacheOrigin, Digest, ExecutionProvenance, Fencing, FreshnessMode,
    FunctionInvocation, FunctionOutcome, FunctionReceipt, RelevantStateToken, ResourceUse,
    RetryCeiling, RunRef, SuccessOutcome, output_manifest_digest,
};
use norn_ir::{ArtifactEnvelope, Decimal, RuntimeObject, RuntimeValue};
use norn_kernel::cell_host::{
    FactKey, FactMutation, FactState, FactVersion, FactWrite, JsonObject,
};
use norn_kernel::pack_mbt_support::{LawExampleVerdict as Verdict, LawIR, law_content_pin};
use norn_kernel::receipt_binding::{ReceiptBinding, ReceiptBindingError, ReceiptReference};
use norn_kernel::result_reconciliation::{
    CommandState, Completion, DispatchedCommand, InvalidReceipt, ReceiptValidator,
    ResultAuthorizer, ResultAuthzRequest, ResultChecks, ResultDenial,
};
use norn_kernel::{
    BoundedFact, CellCommit, CellHost, Command, CommitError, CommitReceipt, DecisionRefusal,
    DueTimer, Effect, FactEvent, IdempotencyKey, LinkKey, LinkReadTarget, LinkState, LogicalTime,
    Observed, OwnedRowsCommitProof, Performed, PredicateObject, PredicateQuery,
};
use norn_testkit::pack_mbt::coverage::{
    DomainCoverage, DomainTransition, ImmutableTarget, PolicyRuleTarget,
};
use norn_testkit::pack_mbt::driver::run_pack_mbt;
use norn_testkit::pack_mbt::dst::{PackDstConfig, run_pack_dst};
use norn_testkit::pack_mbt::static_gaps::{StructuralGap, analyze_static_gaps};
use norn_testkit::pack_mbt::{
    AdmittedPack, EvidenceLevel, LawExampleCase, PackHost, action_call_object_type,
    function_command_object_type,
};
use serde_json::json;
use tempfile::TempDir;

type BaseCell = SqliteCell<FactEvent, JsonObject, JsonObject>;

const MBT_RECEIPT_EXECUTOR: &str = "pool.mbt-receipt";

struct MbtReceiptContract;

impl ResultAuthorizer for MbtReceiptContract {
    fn authorize(&self, _request: &ResultAuthzRequest<'_>) -> Result<(), ResultDenial> {
        Ok(())
    }
}

impl ReceiptValidator for MbtReceiptContract {
    fn validate(
        &self,
        _dispatched: &DispatchedCommand,
        receipt: &FunctionReceipt,
    ) -> Result<(), InvalidReceipt> {
        if matches!(receipt.outcome, FunctionOutcome::Success(_)) {
            Ok(())
        } else {
            Err(InvalidReceipt {
                reason: "expected success receipt".to_owned(),
            })
        }
    }
}

fn build_mbt_receipt(
    scope: &str,
    now_millis: u64,
    cmd_id: &str,
    target_key: &str,
    manifest_digest: &str,
    summary: JsonObject,
) -> Result<(DispatchedCommand, FunctionReceipt, JsonObject), String> {
    let profile = Digest(format!("sha256:{}", "7".repeat(64)));
    let runtime = Digest(format!("sha256:{}", "5".repeat(64)));
    let invocation = FunctionInvocation {
        tenant: scope.to_owned(),
        world: "main".to_owned(),
        authority: Authority {
            kind: AuthorityKind::Service,
            id: "svc.mbt-receipt".to_owned(),
        },
        command_id: cmd_id.to_owned(),
        run: RunRef {
            id: target_key.to_owned(),
            generation: 1,
        },
        manifest: Digest(manifest_digest.to_owned()),
        inputs: BTreeMap::from([(
            "target".to_owned(),
            RuntimeValue::String(target_key.to_owned()),
        )]),
        blobs: BTreeMap::new(),
        provenance_commit: 1,
        dependency: RelevantStateToken::default(),
        freshness: FreshnessMode::AppendOnly,
        destination: None,
        deadline_millis: now_millis.saturating_add(1_000_000),
        retry: RetryCeiling {
            max_attempts: 2,
            max_cost_units: 100,
        },
        fencing: Fencing {
            token: format!("fence-{cmd_id}"),
            cancellation_generation: 0,
        },
    };
    let dispatched = DispatchedCommand::pin(invocation, profile)
        .map_err(|err| format!("pin mbt receipt command: {err}"))?;
    let output_manifest = output_manifest_digest(&[], &summary)
        .map_err(|err| format!("output_manifest_digest: {err}"))?;
    let receipt = FunctionReceipt {
        command_id: dispatched.invocation.command_id.clone(),
        run: dispatched.invocation.run.clone(),
        attempt: 1,
        dispatch: dispatched.dispatch.clone(),
        manifest: dispatched.invocation.manifest.clone(),
        execution_profile: dispatched.execution_profile.clone(),
        input_identity: dispatched.input_identity.clone(),
        dependency_token: dispatched.dependency_token.clone(),
        outcome: FunctionOutcome::Success(SuccessOutcome {
            output_manifest,
            outputs: Vec::new(),
            summary,
        }),
        provenance: ExecutionProvenance {
            executor: MBT_RECEIPT_EXECUTOR.to_owned(),
            placement: "mbt/node-1".to_owned(),
            runtime,
        },
        resources: ResourceUse {
            cpu_millis: 1,
            wall_millis: 1,
            peak_memory_bytes: 0,
            output_bytes: 0,
            fuel: None,
        },
        cache: CacheOrigin::Miss {},
    };
    let encoded_receipt =
        match norn_ir::runtime::encode(&receipt).map_err(|err| format!("encode receipt: {err}"))? {
            RuntimeValue::Object(obj) => obj,
            other @ (RuntimeValue::Null
            | RuntimeValue::Bool(_)
            | RuntimeValue::Number(_)
            | RuntimeValue::String(_)
            | RuntimeValue::Array(_)) => {
                return Err(format!("encoded receipt was not object: {other:?}"));
            }
        };
    Ok((dispatched, receipt, encoded_receipt))
}

/// File-backed [`ObservedSqliteCell`] implementing [`PackHost`] with real
/// SQLite clean reopen and `verify_ledger()` integrity verification.
struct SqlitePackHost {
    _dir: TempDir,
    db_path: PathBuf,
    model: Model,
    scope: String,
    recorder: Arc<DomainCoverageRecorder>,
    receipt_answers: Arc<Mutex<BTreeMap<String, JsonObject>>>,
    receipt_seq: u64,
    cell: Option<ObservedSqliteCell<FactEvent, JsonObject, JsonObject>>,
}

impl SqlitePackHost {
    fn open(pack: &AdmittedPack, recorder: Arc<DomainCoverageRecorder>) -> Result<Self, String> {
        let dir = tempfile::tempdir().map_err(|err| format!("tempdir: {err}"))?;
        let db_path = dir.path().join("pack-mbt.sqlite");
        let mut all_types = pack.object_types.clone();
        all_types.push(action_call_object_type());
        if !all_types
            .iter()
            .any(|ty| ty.name.as_str() == FUNCTION_COMMAND_TYPE)
        {
            all_types.push(function_command_object_type());
        }
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
        let receipt_answers: Arc<Mutex<BTreeMap<String, JsonObject>>> =
            Arc::new(Mutex::new(BTreeMap::new()));
        let answers_for_perform = Arc::clone(&receipt_answers);
        let mut inner = BaseCell::open(
            &db_path,
            model.clone(),
            StoreOptions::default(),
            scope.clone(),
            move |cmd| {
                answers_for_perform
                    .lock()
                    .ok()
                    .and_then(|map| map.get(cmd.id.0.as_str()).cloned())
                    .unwrap_or_else(|| cmd.payload.clone())
            },
        )
        .map_err(|err| format!("SqliteCell::open: {err}"))?;
        inner.set_result_authorizer(Some(Box::new(MbtReceiptContract)));
        let cell = inner.into_observed(Arc::clone(&recorder));
        Ok(Self {
            _dir: dir,
            db_path,
            model,
            scope,
            recorder,
            receipt_answers,
            receipt_seq: 0,
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

    fn resolve_accepted_receipt(
        &self,
        reference: &ReceiptReference,
        manifest: &Digest,
    ) -> Result<Option<ReceiptBinding<JsonObject>>, ReceiptBindingError<CellError>> {
        self.cell_ref()
            .map_err(ReceiptBindingError::Store)?
            .resolve_accepted_receipt(reference, manifest)
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

    // ACTOR-STAMP-1: a governed create's creator stamp reaches the cell's admission.
    fn commit_stamped(
        &mut self,
        commit: CellCommit<FactEvent, JsonObject, JsonObject>,
        proof: norn_kernel::CreatorStampProof,
        quantity: Option<(&norn_ir::quantity_ledger::QuantityRequest, &str)>,
    ) -> Result<CommitReceipt, CommitError<CellError>> {
        self.cell_mut()
            .map_err(CommitError::Host)?
            .commit_stamped(commit, proof, quantity)
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

    fn reopen(&mut self) -> Result<(), String> {
        let prev_now = self.logical_now();
        self.cell = None;
        let answers_for_perform = Arc::clone(&self.receipt_answers);
        let mut reopened = BaseCell::open(
            &self.db_path,
            self.model.clone(),
            StoreOptions::default(),
            self.scope.clone(),
            move |cmd| {
                answers_for_perform
                    .lock()
                    .ok()
                    .and_then(|map| map.get(cmd.id.0.as_str()).cloned())
                    .unwrap_or_else(|| cmd.payload.clone())
            },
        )
        .map_err(|err| format!("clean reopen failed: {err}"))?;
        reopened.set_result_authorizer(Some(Box::new(MbtReceiptContract)));
        reopened.advance_to(prev_now);
        reopened
            .store()
            .read()
            .verify_ledger()
            .map_err(|err| format!("verify_ledger after reopen failed: {err}"))?;
        self.cell = Some(reopened.into_observed(Arc::clone(&self.recorder)));
        Ok(())
    }

    fn seed_accepted_receipt(
        &mut self,
        target_key: &str,
        manifest_digest: &str,
        summary: JsonObject,
    ) -> Result<Option<String>, String> {
        self.receipt_seq = self.receipt_seq.saturating_add(1);
        let cmd_id = format!("mbt-receipt-{target_key}-{}", self.receipt_seq);
        let (dispatched, receipt, encoded_receipt) = build_mbt_receipt(
            &self.scope,
            self.logical_now().as_millis(),
            &cmd_id,
            target_key,
            manifest_digest,
            summary,
        )?;
        if let Ok(mut answers) = self.receipt_answers.lock() {
            answers.insert(cmd_id.clone(), encoded_receipt);
        }
        let state = norn_ir::runtime::encode(&CommandState::dispatched(&dispatched))
            .map_err(|err| format!("encode command state: {err}"))?;
        let invocation_val = norn_ir::runtime::encode(&dispatched.invocation)
            .map_err(|err| format!("encode invocation: {err}"))?;
        let freshness = dispatched
            .freshness()
            .ok_or_else(|| "dispatched freshness missing".to_owned())?;
        let cell = self
            .cell_mut()
            .map_err(|err| format!("cell_mut: {err}"))?
            .inner_mut();
        cell.commit(CellCommit {
            writes: vec![FactWrite {
                key: FactKey::new(FUNCTION_COMMAND_TYPE, cmd_id.as_str()),
                expected: FactVersion::ABSENT,
                mutation: FactMutation::Put {
                    values: JsonObject::from([("state".to_owned(), state)]),
                },
            }],
            effects: vec![Effect::Perform(Command {
                id: dispatched.command_id(),
                generation: dispatched.generation(),
                idempotency_key: IdempotencyKey(format!("{cmd_id}/1")),
                freshness,
                deadline: None,
                payload: JsonObject::from([
                    (
                        "type".to_owned(),
                        RuntimeValue::String(FUNCTION_PAYLOAD_TYPE.to_owned()),
                    ),
                    ("invocation".to_owned(), invocation_val),
                ]),
            })],
            ..CellCommit::empty()
        })
        .map_err(|err| format!("enqueue mbt receipt command: {err}"))?;
        let Some(delivered) = cell
            .perform()
            .map_err(|err| format!("perform mbt receipt command: {err}"))?
        else {
            return Err(format!("mbt receipt command {cmd_id} did not perform"));
        };
        if delivered.command != dispatched.command_id()
            || delivered.generation != dispatched.generation()
        {
            return Err(format!(
                "unexpected outbox head {:?} (generation {:?}) while seeding mbt receipt command {cmd_id}",
                delivered.command, delivered.generation
            ));
        }
        let command = dispatched.command_id();
        let digest = cell
            .retain_function_receipt(
                &dispatched,
                &Completion {
                    receipt: &receipt,
                    command: &command,
                    generation: dispatched.generation(),
                    authenticated_executor: Some(MBT_RECEIPT_EXECUTOR),
                },
                &ResultChecks {
                    authorizer: &MbtReceiptContract,
                    validator: &MbtReceiptContract,
                },
            )
            .map_err(|err| format!("retain mbt receipt: {err}"))?;
        Ok(Some(digest.0))
    }

    fn record_decision_refusal(&self, refusal: &DecisionRefusal<Self::Error>) {
        self.recorder.record_decision_refusal(refusal);
    }

    fn read_observed_coverage(&self) -> Option<DomainCoverage> {
        Some(snapshot_to_domain_coverage(&self.recorder.snapshot()))
    }

    fn governing_law_pin(&self, object_type: &str, artifact_id: &str) -> Option<String> {
        let cell = self.cell.as_ref()?;
        let laws = cell
            .inner()
            .store()
            .model()
            .governing_laws(object_type)
            .ok()?;
        let env = laws.iter().find(|env| env.artifact_id == artifact_id)?;
        let text = serde_json::to_string(&env.payload.to_json_value()).ok()?;
        let law: LawIR = serde_json::from_str(&text).ok()?;
        law_content_pin(&law).ok()
    }
}

fn snapshot_to_domain_coverage(snap: &DomainCoverageSnapshot) -> DomainCoverage {
    DomainCoverage {
        transitions: snap
            .transitions
            .iter()
            .map(|(k, v)| {
                (
                    DomainTransition {
                        object_type: k.object_type.clone(),
                        lifecycle: k.lifecycle.clone(),
                        transition: k.transition.clone(),
                        from: k.from.clone(),
                        to: k.to.clone(),
                    },
                    *v,
                )
            })
            .collect(),
        disallowed_transitions: snap
            .disallowed_transitions
            .iter()
            .map(|(k, v)| {
                (
                    DomainTransition {
                        object_type: k.object_type.clone(),
                        lifecycle: k.lifecycle.clone(),
                        transition: k.transition.clone(),
                        from: k.from.clone(),
                        to: k.to.clone(),
                    },
                    *v,
                )
            })
            .collect(),
        immutable_refusals: snap
            .immutable_refusals
            .iter()
            .map(|(k, v)| {
                (
                    ImmutableTarget {
                        object_type: k.object_type.clone(),
                        lifecycle: k.lifecycle.clone(),
                        state: k.state.clone(),
                    },
                    *v,
                )
            })
            .collect(),
        law_holds: snap.law_holds.clone(),
        law_violations: snap.law_violations.clone(),
        cedar_allows: snap
            .cedar_allows
            .iter()
            .map(|(k, v)| {
                (
                    PolicyRuleTarget {
                        policy_id: k.policy_id.clone(),
                        rule_id: k.rule_id.clone(),
                    },
                    *v,
                )
            })
            .collect(),
        cedar_denies: snap
            .cedar_denies
            .iter()
            .map(|(k, v)| {
                (
                    PolicyRuleTarget {
                        policy_id: k.policy_id.clone(),
                        rule_id: k.rule_id.clone(),
                    },
                    *v,
                )
            })
            .collect(),
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
            if matches!(
                env.artifact_id.as_str(),
                "location.Region.entity" | "location.Space.entity" | "commerce.SalesOrder.entity"
            ) {
                continue;
            }
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

/// `supply-lab` with `tracking` and `ml`, and the registry value types its
/// `Norn.ref("core.quantity")` properties resolve against.
fn load_supply_lab_pack() -> Result<AdmittedPack, String> {
    let root = repo_root();
    let mut envelopes = Vec::new();
    for name in [
        "core.value_types.entity.json",
        "std.value_types.entity.json",
    ] {
        let path = root.join("registry/core/artifacts").join(name);
        let text = fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?;
        envelopes.push(
            serde_json::from_str::<ArtifactEnvelope>(&text)
                .map_err(|e| format!("{}: {e}", path.display()))?,
        );
    }
    for dir in ["tracking", "ml", "supply-lab"] {
        envelopes.extend(load_envelopes_from_dir(
            &root.join("registry/packs").join(dir).join("artifacts"),
        )?);
    }
    AdmittedPack::from_envelopes("supply-lab", &envelopes)
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

    // Promote and retire are separated from the registrant (`independentOf`), so
    // the static gap detector no longer asks whether they need SoD.
    let gaps = analyze_static_gaps(&pack);
    assert!(
        !gaps.questions().iter().any(|g| matches!(
            g,
            StructuralGap::HighImpactActionMissingSod { action_id, .. }
                if action_id == "ml.modelversion.promote" || action_id == "ml.modelversion.retire"
        )),
        "ml promote and retire declare independentOf:\n{gaps}"
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
                ("end_day_offset", RuntimeValue::Number(Decimal::from(0_i64))),
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
                ("end_day_offset", RuntimeValue::Number(Decimal::from(0_i64))),
            ]),
            expected: Verdict::Violated,
        },
        LawExampleCase {
            name: "overnight working window 22:00..06:00 (+1 day) holds".to_owned(),
            law_id: "time.WorkingWindow.valid_local_window".to_owned(),
            subject: runtime_obj(&[
                ("id", RuntimeValue::String("ww-night".to_owned())),
                ("weekday", RuntimeValue::String("sunday".to_owned())),
                ("start_local", RuntimeValue::String("22:00".to_owned())),
                ("end_local", RuntimeValue::String("06:00".to_owned())),
                ("end_day_offset", RuntimeValue::Number(Decimal::from(1_i64))),
            ]),
            expected: Verdict::Holds,
        },
    ]);
    let time_rec = Arc::new(DomainCoverageRecorder::new());
    let time_report = run_pack_mbt(&time_pack, |_| {
        SqlitePackHost::open(&time_pack, Arc::clone(&time_rec))
    })?;
    assert_eq!(time_report.law_examples_checked, 3);
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
    let has_canon_lifecycle_or_reversal_defect = gaps.defects().into_iter().any(|gap| match gap {
        StructuralGap::DeadEndNonTerminalState { object_type, .. }
        | StructuralGap::UnreachableState { object_type, .. }
        | StructuralGap::ImmutableFinancialMissingReversal { object_type, .. }
        | StructuralGap::ImmutableFinancialUnverifiedReversal { object_type, .. } => {
            object_type.starts_with("canon.")
        }
        // `canon.OrderLine` inherits the active `core.Commitment` lifecycle
        // (`promise`, `fulfil`, `cancel`, `breach`) while `canon` itself is a
        // pure observation pack with zero actions; any other `canon.*`
        // transition-without-action finding remains a defect.
        StructuralGap::TransitionWithoutAction {
            object_type,
            lifecycle,
            transition_id,
        } => {
            object_type.starts_with("canon.")
                && !(object_type == "canon.OrderLine"
                    && lifecycle == "status"
                    && matches!(
                        transition_id.as_str(),
                        "promise" | "fulfil" | "cancel" | "breach"
                    ))
        }
        StructuralGap::OrphanField { .. } | StructuralGap::HighImpactActionMissingSod { .. } => {
            false
        }
    });
    assert!(
        !has_canon_lifecycle_or_reversal_defect,
        "canon.* entities should have no lifecycle or missing-reversal defects:\n{gaps}"
    );
    // `canon` declares entities with invariants and zero actions, so OrphanField
    // must report invariant-referenced fields even when the writer set is empty.
    assert!(
        gaps.defects().iter().any(|gap| matches!(
            gap,
            StructuralGap::OrphanField { object_type, property, .. }
                if object_type == "canon.Delivery" && property == "quantity"
        )),
        "expected OrphanField on action-less canon.Delivery.quantity:\n{gaps}"
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

/// SUPPLY-LAB axis 8: the static gap detector, the generic MBT (lifecycles,
/// immutable refusals, the full Cedar role matrix) and the seeded DST on the
/// admitted `supply-lab` pack (with `ml` and `tracking`) against a real
/// file-backed cell. Types with a protected creator (`supply.PurchaseOrder`,
/// `ml.ModelVersion`) are seeded through their stamping create action
/// (ACTOR-STAMP-1), and mapped evidence types are written by their admitted
/// mappings, so they raise no `OrphanField`. Every phase must pass.
#[test]
fn real_supply_lab_pack_passes_sqlite_mbt_dst_and_static_gaps() -> Result<(), String> {
    let pack = load_supply_lab_pack()?;
    let gaps = analyze_static_gaps(&pack);
    let missing_sod = |id: &str| {
        gaps.questions().iter().any(|gap| {
            matches!(gap, StructuralGap::HighImpactActionMissingSod { action_id, .. } if action_id == id)
        })
    };
    for action in [
        "supply.po.submit",
        "ml.modelversion.promote",
        "ml.modelversion.retire",
    ] {
        assert!(
            !missing_sod(action),
            "{action} declares independentOf:\n{gaps}"
        );
    }
    assert!(
        gaps.defects().is_empty(),
        "supply-lab structural defects:\n{gaps}"
    );
    assert!(
        pack.mapping_writes
            .get("supply.WmsCount")
            .is_some_and(|fields| fields.contains("on_hand_qty")),
        "the WMS mapping writes its evidence type"
    );
    let recorder = Arc::new(DomainCoverageRecorder::new());
    let mbt = run_pack_mbt(&pack, |_| {
        SqlitePackHost::open(&pack, Arc::clone(&recorder))
    })?;
    mbt.coverage.check_transitions_covered()?;
    mbt.coverage.check_immutable_refusals_covered()?;
    mbt.coverage.check_cedar_covered()?;
    let config = PackDstConfig {
        cases: 12,
        max_steps: 18,
        ..PackDstConfig::default()
    };
    let dst = run_pack_dst(&pack, config, |_| {
        SqlitePackHost::open(&pack, Arc::clone(&recorder))
    })?;
    assert_eq!(dst.cases_run, 12);
    Ok(())
}

/// The structural defects each first-party registry pack is known to carry,
/// rendered as the report prints them: anything else fails. `canon` declares
/// action-less entities on purpose (its invariants still reference fields).
const KNOWN_PACK_DEFECTS: &[(&str, &str)] = &[
    (
        "canon",
        r#"OrphanField { object_type: "canon.Holding", property: "stock_cost", referenced_in: ["invariant:cost_has_currency"] }"#,
    ),
    (
        "canon",
        r#"OrphanField { object_type: "canon.Holding", property: "cost_currency", referenced_in: ["invariant:cost_has_currency"] }"#,
    ),
    // `org` and `time` declare their entities and laws, not their writers.
    (
        "org",
        r#"OrphanField { object_type: "org.Membership", property: "valid_from", referenced_in: ["law:org.Membership.valid_date_range:response"] }"#,
    ),
    (
        "org",
        r#"OrphanField { object_type: "org.Membership", property: "valid_to", referenced_in: ["law:org.Membership.valid_date_range:response"] }"#,
    ),
    (
        "time",
        r#"OrphanField { object_type: "time.WorkingWindow", property: "start_local", referenced_in: ["law:time.WorkingWindow.valid_local_window:response"] }"#,
    ),
    (
        "time",
        r#"OrphanField { object_type: "time.WorkingWindow", property: "end_local", referenced_in: ["law:time.WorkingWindow.valid_local_window:response"] }"#,
    ),
];

/// RC-4: the static gap detector over every first-party pack in
/// `registry/packs/` (each pack's own artifacts, with `ml` and `tracking` for
/// `supply-lab`): zero structural defects beyond the pinned known ones.
#[test]
fn every_registry_pack_has_no_unexpected_structural_defects() -> Result<(), String> {
    let root = repo_root().join("registry/packs");
    let mut dirs: Vec<String> = fs::read_dir(&root)
        .map_err(|e| format!("{}: {e}", root.display()))?
        .filter_map(Result::ok)
        .filter(|entry| entry.path().join("artifacts").is_dir())
        .filter_map(|entry| entry.file_name().into_string().ok())
        .collect();
    dirs.sort();
    assert!(dirs.iter().any(|dir| dir == "supply-lab"), "{dirs:?}");
    let mut unexpected = Vec::new();
    let mut seen = BTreeSet::new();
    for dir in &dirs {
        let with: Vec<&str> = if dir == "supply-lab" {
            vec!["tracking", "ml", "supply-lab"]
        } else {
            vec![dir.as_str()]
        };
        let pack = load_registry_pack(dir, &with, false)?;
        let gaps = analyze_static_gaps(&pack);
        for defect in gaps.defects() {
            let rendered = format!("{defect:?}");
            seen.insert((dir.clone(), rendered.clone()));
            if !KNOWN_PACK_DEFECTS
                .iter()
                .any(|(known_pack, known)| *known_pack == dir.as_str() && *known == rendered)
            {
                unexpected.push(format!("{dir}: {rendered}"));
            }
        }
    }
    assert!(
        unexpected.is_empty(),
        "unexpected structural defects:\n{}",
        unexpected.join("\n")
    );
    // The known list only shrinks: a defect fixed is removed from it.
    for (pack, known) in KNOWN_PACK_DEFECTS {
        assert!(
            seen.contains(&((*pack).to_owned(), (*known).to_owned())),
            "{pack}: {known} is fixed; remove it from KNOWN_PACK_DEFECTS"
        );
    }
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
                "condition": "self.status == 'submitted' or self.status == 'approved' or self.status == 'posted'",
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
    ]
}

fn o2c_reverse_and_policy() -> Vec<serde_json::Value> {
    vec![
        json!({
            "irVersion": "0.1", "artifactId": "o2c.invoice.reverse.action", "version": "1.0.0",
            "kind": "action", "namespace": "o2c", "name": "Reverse invoice", "lifecycle": "active",
            "scope": {}, "dependencies": [], "provenance": {"source": "system"},
            "payload": {
                "actionId": "o2c.invoice.reverse",
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
                    "transition": "cancel"
                },
                "writes": [{"entityKind": "o2c.Invoice", "change": "modified"}]
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
    raw_envelopes.extend(o2c_reverse_and_policy());
    let envelopes: Vec<ArtifactEnvelope> = raw_envelopes
        .into_iter()
        .map(|val| serde_json::from_value(val).map_err(|err| format!("envelope: {err}")))
        .collect::<Result<Vec<_>, _>>()?;
    let pack = AdmittedPack::from_envelopes("o2c_sod", &envelopes)?.with_law_examples(vec![
        LawExampleCase {
            name: "positive submitted invoice holds".to_owned(),
            law_id: "o2c.Invoice.positive_amount".to_owned(),
            subject: runtime_obj(&[
                ("invoice_id", RuntimeValue::String("INV-1".to_owned())),
                ("requester", RuntimeValue::String("alice".to_owned())),
                ("amount", dec("250.00")?),
                ("memo", RuntimeValue::String("Q4 services".to_owned())),
                ("status", RuntimeValue::String("submitted".to_owned())),
            ]),
            expected: Verdict::Holds,
        },
        LawExampleCase {
            name: "zero amount submitted invoice violates".to_owned(),
            law_id: "o2c.Invoice.positive_amount".to_owned(),
            subject: runtime_obj(&[
                ("invoice_id", RuntimeValue::String("INV-2".to_owned())),
                ("requester", RuntimeValue::String("alice".to_owned())),
                ("amount", dec("0.00")?),
                ("memo", RuntimeValue::String("invalid".to_owned())),
                ("status", RuntimeValue::String("submitted".to_owned())),
            ]),
            expected: Verdict::Violated,
        },
        LawExampleCase {
            name: "zero amount draft invoice is not applicable".to_owned(),
            law_id: "o2c.Invoice.positive_amount".to_owned(),
            subject: runtime_obj(&[
                ("invoice_id", RuntimeValue::String("INV-3".to_owned())),
                ("requester", RuntimeValue::String("alice".to_owned())),
                ("amount", dec("0.00")?),
                ("memo", RuntimeValue::String("draft".to_owned())),
                ("status", RuntimeValue::String("draft".to_owned())),
            ]),
            expected: Verdict::NotApplicable,
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
    assert_eq!(mbt.uncovered_guard_refusals, Vec::<String>::new());
    assert!(mbt.evidence.contains(&(
        "lifecycle_walks_and_refusals",
        EvidenceLevel::ExhaustiveFinite
    )));
    assert!(mbt.immutable_refusals_checked >= 1);
    assert!(mbt.cedar_checks_run >= 4);
    assert_eq!(mbt.law_examples_checked, 3);

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
        dst.reopens_verified >= 1,
        "expected at least one SQLite clean-reopen + verify_ledger cycle"
    );

    let gaps = analyze_static_gaps(&pack);
    assert!(
        !gaps.defects().iter().any(|g| matches!(
            g,
            StructuralGap::ImmutableFinancialMissingReversal { object_type, .. }
                if object_type == "o2c.Invoice"
        )),
        "o2c.Invoice has a resolved compensating action:\n{gaps}"
    );
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
            ("status", RuntimeValue::String("submitted".to_owned())),
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
