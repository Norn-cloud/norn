//! Resource Planning Lab (S2 — Versioned Plan Pack & Open-to-Buy, #1942):
//!
//! End-to-end acceptance suite for `registry/packs/plan/` composed with
//! `registry/packs/supply-lab/`, `registry/packs/ml/`, and
//! `registry/packs/tracking/` on real file-backed `SqliteCell`s — zero domain
//! opcodes.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

use norn_cell::compute::transform::{RowAccess, TransformResult};
use norn_cell::compute::{QueryContext, QueryResult};
use norn_cell::function_receipt::{FUNCTION_COMMAND_TYPE, FUNCTION_PAYLOAD_TYPE};
use norn_cell::{AdmittedDefinitions, CellError, KnowledgeCutoff, Model, SqliteCell, StoreOptions};
use norn_checker::EntityArtifactSetContext;
use norn_checker::entity_scopes::ScopedView;
use norn_checker::metric::compute::QuestionPlan;
use norn_checker::metric::derive::derive_metric_space;
use norn_ir::function::{
    Authority, AuthorityKind, BlobRef, CacheOrigin, Digest, ExecutionProvenance, Fencing,
    FreshnessMode, FunctionInvocation, FunctionOutcome, FunctionReceipt, RelevantStateToken,
    ResourceUse, RetryCeiling, RunRef, SuccessOutcome, output_manifest_digest,
};
use norn_ir::{ArtifactEnvelope, Decimal, LawRoot, QuestionIR, RuntimeValue};
use norn_kernel::action_commit::ActionReferenceReads;
use norn_kernel::cell_host::{
    BoundedFact, CellCommit, CellHost, CommitError, CommitReceipt, DueTimer, FactKey, FactMutation,
    FactState, FactVersion, FactWrite, JsonObject, LinkKey, LinkReadTarget, LinkState, Observed,
    Performed, PredicateActionRead, PredicateObject, PredicateQuery, SerializationLane,
};
use norn_kernel::result_reconciliation::{
    CommandState, Completion, DispatchedCommand, InvalidReceipt, ReceiptValidator,
    ResultAuthorizer, ResultAuthzRequest, ResultChecks, ResultDenial,
};
use norn_kernel::{
    ActionAuthorizer, ActionAuthzRequest, ActionCall, ActionDenial, ActionOutcome,
    ActionReadAuthzRequest, ActionReadTypeAuthzRequest, Command, CreatorStampProof, DecisionTurn,
    Effect, FactEvent, IdempotencyKey, LogicalTime, commit_action,
};
use serde_json::{Value, json};
use tempfile::TempDir;

type PlanCell = SqliteCell<FactEvent, JsonObject, JsonObject>;

const CLOCK_MS: u64 = 1_789_905_600_000;
const EVAL_EXECUTOR: &str = "pool.ml-evaluator";
const EVAL_AUTHORITY: &str = "svc:ml-evaluator";
const EVAL_MANIFEST_DIGEST: &str =
    "sha256:9e9458beea3d97fc34784e42d6d98ae53343ed38b6f6c5211c71a9a4c7c890c6";
const EVAL_PROFILE_DIGEST: &str =
    "sha256:a780163924782b48cc7ef188073bbc22d903bdd5a55825b7077c9e9eb9c7656e";

fn repo() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn read_envelopes(dir: &Path) -> Result<Vec<ArtifactEnvelope>, String> {
    let mut paths: Vec<PathBuf> = fs::read_dir(dir)
        .map_err(|e| format!("{}: {e}", dir.display()))?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "json"))
        .collect();
    paths.sort();
    paths
        .iter()
        .map(|path| {
            let text = fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
            serde_json::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))
        })
        .collect()
}

fn core_base() -> Result<Vec<ArtifactEnvelope>, String> {
    let core = repo().join("registry/core/artifacts");
    [
        "core.Agent.interface.json",
        "core.AgentRole.interface.json",
        "core.Resource.interface.json",
        "core.Commitment.interface.json",
        "core.Hold.interface.json",
        "core.EconomicEvent.interface.json",
        "core.LedgerEffect.interface.json",
        "core.Claim.interface.json",
        "core.value_types.entity.json",
        "std.value_types.entity.json",
        "core.units.json",
        "core.time.json",
    ]
    .iter()
    .map(|name| {
        let text = fs::read_to_string(core.join(name)).map_err(|e| format!("{name}: {e}"))?;
        serde_json::from_str(&text).map_err(|e| format!("{name}: {e}"))
    })
    .collect()
}

fn runtime_envelopes(artifacts: &[ArtifactEnvelope]) -> Result<Vec<RuntimeValue>, String> {
    artifacts
        .iter()
        .map(|artifact| {
            let wire = serde_json::to_value(artifact).map_err(|e| format!("{e}"))?;
            RuntimeValue::from_json(wire).map_err(|e| format!("{e:?}"))
        })
        .collect()
}

struct PlanPack {
    candidate: Vec<ArtifactEnvelope>,
    base: Vec<ArtifactEnvelope>,
    pool: Vec<ArtifactEnvelope>,
    definitions: AdmittedDefinitions,
}

fn build_plan_pack() -> Result<PlanPack, String> {
    let mut candidate = Vec::new();
    for dir in ["tracking", "ml", "supply-lab", "plan"] {
        candidate.extend(read_envelopes(
            &repo().join("registry/packs").join(dir).join("artifacts"),
        )?);
    }
    let base = core_base()?;
    let mut pool = base.clone();
    pool.extend_from_slice(&candidate);
    let definitions =
        AdmittedDefinitions::from_envelopes(&pool).map_err(|e| format!("definitions: {e}"))?;
    let (sources, base_sources) = (runtime_envelopes(&candidate)?, runtime_envelopes(&base)?);
    let registry = BTreeSet::new();
    let checked = norn_checker::check_artifact_set_with_receipt(
        &norn_checker::ReceiptSources::new(&sources, &base_sources)
            .map_err(|e| format!("{e:?}"))?,
        &EntityArtifactSetContext {
            registry: &registry,
            base: &base,
            tenant: None,
        },
        &norn_ir::ResourceLimitProfile::norn_limits_v1(),
    )
    .map_err(|e| format!("receipt: {e:?}"))?;
    if !checked.receipt.admitted {
        return Err(format!("plan pack not admitted: {:?}", checked.diagnostics));
    }
    let plan_laws: BTreeSet<&str> = candidate
        .iter()
        .filter(|a| a.namespace == "plan" && a.kind == norn_ir::ArtifactKind::Law)
        .map(|a| a.artifact_id.as_str())
        .collect();
    let law_diags: Vec<_> = checked
        .diagnostics
        .iter()
        .filter(|d| {
            d.artifact_id
                .as_deref()
                .is_some_and(|id| plan_laws.contains(id))
        })
        .collect();
    if !law_diags.is_empty() {
        return Err(format!("unexpected plan law diagnostics: {law_diags:?}"));
    }
    Ok(PlanPack {
        candidate,
        base,
        pool,
        definitions,
    })
}

fn plan_pack() -> Result<&'static PlanPack, String> {
    static PACK: OnceLock<Result<PlanPack, String>> = OnceLock::new();
    PACK.get_or_init(build_plan_pack)
        .as_ref()
        .map_err(Clone::clone)
}

impl PlanPack {
    fn model(&self) -> &Model {
        self.definitions.model()
    }

    fn plan_question(&self, name: &str) -> Result<QuestionPlan, String> {
        let text =
            fs::read_to_string(repo().join("registry/packs/plan/questions/plan.questions.json"))
                .map_err(|e| format!("plan.questions.json: {e}"))?;
        let questions: Vec<QuestionIR> =
            serde_json::from_str(&text).map_err(|e| format!("decode plan questions: {e}"))?;
        let question = questions
            .into_iter()
            .find(|q| q.name == name)
            .ok_or_else(|| format!("missing plan question {name}"))?;
        let registry = BTreeSet::new();
        let space = derive_metric_space(
            &ScopedView::uniform(&self.pool, &registry),
            &LawRoot::Entity(question.object_set.as_str().to_owned()),
        )
        .map_err(|e| format!("derive metric space for {name}: {e:?}"))?;
        space
            .admit(&question, 100)
            .map_err(|e| format!("admit {name}: {e:?}"))
    }

    fn transform_plan(
        &self,
        artifact_id: &str,
    ) -> Result<norn_checker::transform::TransformPlan, String> {
        let artifact = self
            .candidate
            .iter()
            .find(|a| a.artifact_id.as_str() == artifact_id)
            .ok_or_else(|| format!("missing transform {artifact_id}"))?;
        norn_checker::transform::admit(artifact, &self.pool, &BTreeSet::new())
            .map_err(|e| format!("admit transform {artifact_id}: {e:?}"))
    }
}

struct Allow;
impl ActionAuthorizer for Allow {
    fn authorize_read(&self, _: &ActionReadAuthzRequest<'_>) -> Result<(), ActionDenial> {
        Ok(())
    }
    fn authorize_read_type(&self, _: &ActionReadTypeAuthzRequest<'_>) -> Result<(), ActionDenial> {
        Ok(())
    }
    fn authorize(&self, _: &ActionAuthzRequest<'_>) -> Result<(), ActionDenial> {
        Ok(())
    }
}

struct EvaluatorContract;
impl ResultAuthorizer for EvaluatorContract {
    fn authorize(&self, request: &ResultAuthzRequest<'_>) -> Result<(), ResultDenial> {
        if request.authority.id == EVAL_AUTHORITY {
            Ok(())
        } else {
            Err(ResultDenial {
                reason: "evaluator authority denied".to_owned(),
            })
        }
    }
}
impl ReceiptValidator for EvaluatorContract {
    fn validate(
        &self,
        _dispatched: &DispatchedCommand,
        receipt: &FunctionReceipt,
    ) -> Result<(), InvalidReceipt> {
        if receipt.attempt == 1 && matches!(receipt.outcome, FunctionOutcome::Success(_)) {
            Ok(())
        } else {
            Err(InvalidReceipt {
                reason: "expected attempt-1 success receipt".to_owned(),
            })
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
enum Outcome {
    Committed,
    Replayed,
    Refused(String, String),
}

struct PlanBook {
    pack: &'static PlanPack,
    _dir: TempDir,
    scope: String,
    org_id: String,
    env_id: String,
    receipt_answers: Arc<Mutex<BTreeMap<String, JsonObject>>>,
    cell: PlanCell,
    tick: u64,
    calls: u64,
}

impl PlanBook {
    fn open(org_id: &str, env_id: &str, file_name: &str) -> Result<Self, String> {
        let pack = plan_pack()?;
        let dir = tempfile::tempdir().map_err(|e| format!("tempdir: {e}"))?;
        let path = dir.path().join(file_name);
        let scope = format!("{org_id}/{env_id}");
        let receipt_answers: Arc<Mutex<BTreeMap<String, JsonObject>>> =
            Arc::new(Mutex::new(BTreeMap::new()));
        let answers_ref = Arc::clone(&receipt_answers);
        let mut cell = PlanCell::open(
            &path,
            pack.model().clone(),
            StoreOptions::default(),
            scope.clone(),
            move |cmd| {
                answers_ref
                    .lock()
                    .ok()
                    .and_then(|map| map.get(cmd.id.0.as_str()).cloned())
                    .unwrap_or_else(|| cmd.payload.clone())
            },
        )
        .map_err(|e| format!("open cell {file_name}: {e}"))?;
        cell.set_result_authorizer(Some(Box::new(EvaluatorContract)));
        Ok(Self {
            pack,
            _dir: dir,
            scope,
            org_id: org_id.to_owned(),
            env_id: env_id.to_owned(),
            receipt_answers,
            cell,
            tick: 0,
            calls: 0,
        })
    }

    fn now(&self) -> LogicalTime {
        LogicalTime::from_millis(CLOCK_MS.saturating_add(self.tick))
    }

    fn act(&mut self, action: &str, actor: &str, input: Value) -> Result<Outcome, String> {
        self.calls = self.calls.saturating_add(1);
        let key = format!("{}-call-{}", self.org_id, self.calls);
        self.tick = self.tick.saturating_add(1_000);
        let now = self.now();
        self.cell.advance_to(now);
        let world = self.cell.store().world();
        let admitted = self
            .pack
            .definitions
            .action(action)
            .ok_or_else(|| format!("no action {action}"))?;
        let input_obj: JsonObject =
            serde_json::from_value(input).map_err(|e| format!("{action} input: {e}"))?;
        let catalog = self.pack.definitions.catalog();
        let call = ActionCall {
            action: &admitted.root,
            action_artifact_id: &admitted.artifact_id,
            action_version: &admitted.version,
            scope: BTreeMap::from([
                ("orgId".into(), self.org_id.clone()),
                ("environmentId".into(), self.env_id.clone()),
            ]),
            actor: actor.to_owned(),
            approved_intent: None,
            input: input_obj,
            idempotency_key: key.clone(),
            object_type: None,
            turn: DecisionTurn {
                id: key,
                now,
                world,
                consumes: None,
            },
            trace: None,
        };
        Ok(
            match commit_action(&mut self.cell, &catalog, &call, &Allow) {
                Ok(ActionOutcome::Committed { .. }) => Outcome::Committed,
                Ok(ActionOutcome::Replayed { .. }) => Outcome::Replayed,
                Err(refusal) => Outcome::Refused(refusal.code().to_owned(), refusal.to_string()),
            },
        )
    }

    fn ok(&mut self, action: &str, actor: &str, input: Value) -> Result<(), String> {
        match self.act(action, actor, input)? {
            Outcome::Committed => Ok(()),
            Outcome::Replayed => Err(format!("{action} expected Committed, got Replayed")),
            Outcome::Refused(code, msg) => Err(format!(
                "{action} expected Committed, got Refused({code}, {msg})"
            )),
        }
    }

    fn refused(
        &mut self,
        action: &str,
        actor: &str,
        input: Value,
    ) -> Result<(String, String), String> {
        match self.act(action, actor, input)? {
            Outcome::Refused(code, msg) => Ok((code, msg)),
            Outcome::Committed => Err(format!("{action} expected Refused, got Committed")),
            Outcome::Replayed => Err(format!("{action} expected Refused, got Replayed")),
        }
    }

    fn query_context(&self, now_ms: u64) -> QueryContext {
        QueryContext {
            world: "main".to_owned(),
            scope: self.scope.clone(),
            population_pin: "all".to_owned(),
            authorization_epoch: 1,
            mapping_pin: "plan-otb/1".to_owned(),
            now_ms,
            source_cursors: BTreeMap::new(),
            watermarks: BTreeMap::new(),
        }
    }

    fn answer(&self, name: &str) -> Result<QueryResult, String> {
        let plan = self.pack.plan_question(name)?;
        self.cell
            .store()
            .evaluate_question(&plan, self.query_context(self.now().as_millis()), |_| {
                Ok(true)
            })
            .map_err(|e| format!("evaluate {name}: {e}"))
    }

    fn answer_known_at(&self, name: &str, cutoff: KnowledgeCutoff) -> Result<QueryResult, String> {
        let plan = self.pack.plan_question(name)?;
        self.cell
            .store()
            .evaluate_question_known_at(
                &plan,
                self.query_context(cutoff.recorded_at.as_millis()),
                cutoff,
                |_| Ok(true),
            )
            .map(|res| res.result)
            .map_err(|e| format!("evaluate {name} known_at: {e}"))
    }

    fn last_cutoff(&self) -> Result<KnowledgeCutoff, String> {
        let receipt = self
            .cell
            .store()
            .read()
            .receipts()
            .map_err(|e| format!("receipts: {e}"))?
            .pop()
            .ok_or_else(|| "no receipts yet".to_owned())?;
        Ok(KnowledgeCutoff {
            recorded_at: LogicalTime::from_millis(receipt.body.logical_time),
            frontier: receipt.body.commit_seq,
        })
    }

    fn transform(&self, artifact_id: &str) -> Result<TransformResult, String> {
        let plan = self.pack.transform_plan(artifact_id)?;
        self.cell
            .store()
            .evaluate_transform(&plan, "main", &self.scope, |_| Ok(RowAccess::Read))
            .map_err(|e| format!("transform {artifact_id}: {e}"))
    }
}

fn scalar(value: &RuntimeValue) -> String {
    match value {
        RuntimeValue::String(s) => s.clone(),
        RuntimeValue::Number(d) => d.canonical(),
        RuntimeValue::Bool(b) => b.to_string(),
        RuntimeValue::Null => "null".to_owned(),
        RuntimeValue::Array(items) => format!("{items:?}"),
        RuntimeValue::Object(obj) => format!("{obj:?}"),
    }
}

fn bucket_decimal(result: &QueryResult, expected: &[&str]) -> Option<String> {
    result.rows.values().find_map(|row| {
        let parts: Vec<String> = match &row.bucket {
            RuntimeValue::Array(items) => items.iter().map(scalar).collect(),
            other => vec![scalar(other)],
        };
        let ok = parts.len() == expected.len()
            && parts
                .iter()
                .zip(expected.iter())
                .all(|(actual, want)| actual == *want);
        ok.then_some(row.value).flatten().map(|d| d.canonical())
    })
}

fn row_str(row: &BTreeMap<String, RuntimeValue>, col: &str) -> Option<String> {
    row.get(col).map(scalar)
}

/// Deterministic `TXN-PHANTOM` race wrapper: lets an outer action prepare its
/// predicate-fenced commit against the underlying `SqliteCell`, then injects a
/// competing commit right before forwarding `commit_stamped` to SQLite.
struct RaceHost<'a, F> {
    cell: &'a mut PlanCell,
    injected: Option<F>,
}

impl<F: FnOnce(&mut PlanCell) -> Result<(), String>> CellHost for RaceHost<'_, F> {
    type Event = FactEvent;
    type Command = JsonObject;
    type Input = JsonObject;
    type Error = CellError;

    fn load(&self, key: &FactKey) -> Result<FactState, CellError> {
        self.cell.load(key)
    }
    fn load_bounded(
        &self,
        key: &FactKey,
        max_bytes: u64,
    ) -> Result<Option<BoundedFact>, CellError> {
        self.cell.load_bounded(key, max_bytes)
    }
    fn load_link(&self, key: &LinkKey) -> Result<LinkState, CellError> {
        self.cell.load_link(key)
    }
    fn read_link_targets(
        &self,
        source: &FactKey,
        link: &str,
        max_targets: usize,
        max_key_bytes: u64,
    ) -> Result<Option<Vec<LinkReadTarget>>, CellError> {
        self.cell
            .read_link_targets(source, link, max_targets, max_key_bytes)
    }
    fn read_predicate(
        &self,
        predicate: &PredicateQuery,
        max_members: usize,
        max_bytes: u64,
    ) -> Result<Option<Vec<PredicateObject>>, CellError> {
        self.cell.read_predicate(predicate, max_members, max_bytes)
    }
    fn read_predicate_for_action(
        &self,
        predicate: &PredicateQuery,
        max_members: usize,
        max_bytes: u64,
        authorize: &mut dyn FnMut(&PredicateObject) -> bool,
    ) -> Result<PredicateActionRead, CellError> {
        self.cell
            .read_predicate_for_action(predicate, max_members, max_bytes, authorize)
    }
    fn commit(
        &mut self,
        commit: CellCommit<FactEvent, JsonObject, JsonObject>,
    ) -> Result<CommitReceipt, CommitError<CellError>> {
        if let Some(inject) = self.injected.take() {
            inject(self.cell).map_err(|_| CommitError::ObservationAuthorization)?;
        }
        self.cell.commit(commit)
    }
    fn commit_stamped(
        &mut self,
        commit: CellCommit<FactEvent, JsonObject, JsonObject>,
        proof: CreatorStampProof,
        references: Option<ActionReferenceReads>,
        quantity: Option<(&norn_ir::quantity_ledger::QuantityRequest, &str)>,
    ) -> Result<CommitReceipt, CommitError<CellError>> {
        if let Some(inject) = self.injected.take() {
            inject(self.cell).map_err(|_| CommitError::ObservationAuthorization)?;
        }
        self.cell
            .commit_stamped(commit, proof, references, quantity)
    }
    fn perform(&mut self) -> Result<Option<Performed<JsonObject>>, CellError> {
        self.cell.perform()
    }
    fn observe(&mut self) -> Result<Option<Observed<JsonObject>>, CellError> {
        self.cell.observe()
    }
    fn clock(&mut self) -> Result<Option<DueTimer<JsonObject>>, CellError> {
        self.cell.clock()
    }
    fn serialize_by(&self, key: &FactKey) -> SerializationLane {
        self.cell.serialize_by(key)
    }
}

fn seed_retail_base(
    book: &mut PlanBook,
    entity_id: &str,
    currency: &str,
    store_ids: &[&str],
) -> Result<(), String> {
    book.ok(
        "supply.legal_entity.register",
        "eng:dina",
        json!({
            "entity_id": entity_id, "legal_name": format!("Entity {entity_id}"),
            "entity_kind": "sole_proprietorship", "book_code": format!("BK-{entity_id}"),
            "functional_currency": currency
        }),
    )?;
    for store_id in store_ids {
        book.ok(
            "supply.store.register",
            "eng:dina",
            json!({
                "store_id": store_id, "name": format!("Store {store_id}"),
                "entity_id": entity_id, "custodian_id": "CUST-MALL",
                "location_kind": "mall_store", "mall_code": "MALL-CFC", "currency": currency
            }),
        )?;
    }
    book.ok(
        "supply.warehouse.register",
        "eng:dina",
        json!({
            "warehouse_id": "WH-MAIN", "name": "Main DC", "region": "delta",
            "latitude": "30.044400", "longitude": "31.235700"
        }),
    )?;
    book.ok(
        "supply.sku.register",
        "eng:dina",
        json!({"sku_id": "SKU-DRY-1", "category": "dry_goods", "unit_of_measure": "each", "shelf_life_days": 180}),
    )?;
    book.ok(
        "supply.sku.register",
        "eng:dina",
        json!({"sku_id": "SKU-SPARE-1", "category": "spares", "unit_of_measure": "each", "shelf_life_days": 365}),
    )?;
    book.ok(
        "supply.unit_cost.record",
        "eng:dina",
        json!({
            "sku_id": "SKU-DRY-1", "cost_ref": "COST-DRY-1", "owner_entity_id": entity_id,
            "supplier_id": "SUP-NILE", "unit_cost": "60.0000", "currency": currency,
            "effective_from": "2026-08-01T00:00:00Z"
        }),
    )
}

fn seed_locked_plan_and_envelope(book: &mut PlanBook) -> Result<(), String> {
    seed_retail_base(book, "LE-SOLE", "EGP", &["ST-CFC"])?;
    book.ok(
        "plan.version.create",
        "planner:amal",
        json!({
            "plan_version_id": "PV-2026-V1", "entity_id": "LE-SOLE",
            "plan_code": "FY2026-AUTUMN", "revision_number": 1, "currency": "EGP",
            "effective_from": "2026-09-01T00:00:00Z", "effective_to": "2026-10-01T00:00:00Z",
            "recorded_at": "2026-08-25T09:00:00Z"
        }),
    )?;
    book.ok(
        "plan.line.record",
        "planner:amal",
        json!({
            "plan_line_id": "PL-DRY-SEP", "plan_version_id": "PV-2026-V1",
            "entity_id": "LE-SOLE", "store_id": "ST-CFC",
            "category": "dry_goods", "brand": "nile_foods", "plan_month": "2026-09",
            "period_start": "2026-09-01T00:00:00Z", "as_of": "2026-09-30T23:59:59Z",
            "currency": "EGP", "unit": "each",
            "planned_sales_retail": "18000.000000", "planned_sales_cost": "10800.000000",
            "planned_sales_qty": "180.000000", "planned_receipts_cost": "10000.000000",
            "planned_receipts_retail": "16500.000000", "planned_receipts_qty": "166.000000",
            "planned_markdowns_cost": "300.000000", "planned_markdowns_retail": "500.000000",
            "planned_fine_metal_g": "0.000000", "spread_remainder_cost": "0.000000",
            "planned_eom_stock_cost": "4200.000000", "planned_eom_stock_retail": "7000.000000",
            "planned_eom_stock_qty": "70.000000"
        }),
    )?;
    book.ok(
        "plan.version.submit",
        "planner:amal",
        json!({"plan_version_id": "PV-2026-V1"}),
    )?;
    book.ok(
        "plan.version.approve",
        "director:salma",
        json!({"plan_version_id": "PV-2026-V1"}),
    )?;
    book.ok(
        "plan.version.lock",
        "director:salma",
        json!({"plan_version_id": "PV-2026-V1"}),
    )?;
    book.ok(
        "plan.otb_envelope.create_planned",
        "planner:amal",
        json!({
            "envelope_id": "ENV-DRY-SEP", "plan_version_id": "PV-2026-V1",
            "plan_line_id": "PL-DRY-SEP", "entity_id": "LE-SOLE", "store_id": "ST-CFC",
            "category": "dry_goods", "brand": "nile_foods", "plan_month": "2026-09",
            "period_start": "2026-09-01T00:00:00Z", "currency": "EGP"
        }),
    )?;
    book.ok(
        "plan.otb_envelope.activate_planned",
        "planner:amal",
        json!({"envelope_id": "ENV-DRY-SEP"}),
    )
}

fn create_submitted_po(
    book: &mut PlanBook,
    po_id: &str,
    line_id: &str,
    sku_id: &str,
    qty: &str,
    unit_price: &str,
) -> Result<(), String> {
    book.ok(
        "supply.po.create",
        "planner:amal",
        json!({
            "po_id": po_id, "supplier_id": "SUP-NILE",
            "promised_at": "2026-09-10T00:00:00Z", "currency": "EGP"
        }),
    )?;
    book.ok(
        "supply.line.add",
        "planner:amal",
        json!({
            "line_id": line_id, "po_id": po_id, "sku_id": sku_id,
            "promised_at": "2026-09-10T00:00:00Z", "ordered_qty": qty,
            "received_qty": "0.000000", "fill_rate": "0.000000",
            "unit": "each", "unit_price": unit_price, "currency": "EGP"
        }),
    )?;
    book.ok("supply.po.submit", "ops:karim", json!({"po_id": po_id}))
}

fn otb_auth_input(
    binding_id: &str,
    envelope_id: &str,
    po_id: &str,
    line_id: &str,
    unit_cost: &str,
) -> Value {
    json!({
        "binding_id": binding_id, "envelope_id": envelope_id, "plan_version_id": "PV-2026-V1",
        "entity_id": "LE-SOLE", "store_id": "ST-CFC", "category": "dry_goods",
        "brand": "nile_foods", "plan_month": "2026-09", "po_id": po_id, "line_id": line_id,
        "currency": "EGP", "unit": "each", "unit_cost": unit_cost,
        "expected_payment_date": "2026-10-25T00:00:00Z", "payment_terms_days": 45,
        "as_of": "2026-09-06T10:00:00Z"
    })
}

fn build_eval_receipt(
    scope: &str,
    version_id: &str,
    sha: &str,
    deadline_millis: u64,
) -> Result<(DispatchedCommand, FunctionReceipt), String> {
    let metric = Decimal::parse_wire("0.11").map_err(|e| format!("decimal: {e}"))?;
    let summary = JsonObject::from([
        (
            "evaluation_revision".into(),
            RuntimeValue::String("c1".into()),
        ),
        (
            "evaluated_artifact_sha256".into(),
            RuntimeValue::String(sha.to_owned()),
        ),
        (
            "evaluated_artifact_size".into(),
            RuntimeValue::from(4096_i64),
        ),
        ("metric_name".into(), RuntimeValue::String("wape".into())),
        ("metric_value".into(), RuntimeValue::Number(metric)),
        ("contracts_passed".into(), RuntimeValue::Bool(true)),
        ("checks_passed".into(), RuntimeValue::from(11_i64)),
        ("checks_failed".into(), RuntimeValue::from(0_i64)),
        ("suite_version".into(), RuntimeValue::String("1".into())),
    ]);
    let cmd_id = "cmd-eval-plan-v1";
    let invocation = FunctionInvocation {
        tenant: scope.to_owned(),
        world: "main".to_owned(),
        authority: Authority {
            kind: AuthorityKind::Service,
            id: EVAL_AUTHORITY.to_owned(),
        },
        command_id: cmd_id.to_owned(),
        run: RunRef {
            id: version_id.to_owned(),
            generation: 1,
        },
        manifest: Digest(EVAL_MANIFEST_DIGEST.to_owned()),
        inputs: BTreeMap::from([
            ("dataset_revision".into(), RuntimeValue::String("c1".into())),
            (
                "evaluation_run".into(),
                RuntimeValue::String("run-eval".into()),
            ),
            ("features".into(), RuntimeValue::String("qty".into())),
            ("metric_name".into(), RuntimeValue::String("wape".into())),
            (
                "model_id".into(),
                RuntimeValue::String("supply.demand_forecast".into()),
            ),
            ("order".into(), RuntimeValue::String("week_start".into())),
            (
                "target".into(),
                RuntimeValue::String("predicted_qty".into()),
            ),
            ("version_id".into(), RuntimeValue::String(version_id.into())),
        ]),
        blobs: BTreeMap::from([
            (
                "dataset".into(),
                BlobRef {
                    digest: Digest(format!("sha256:{}", "cd".repeat(32))),
                    version: Some("c1".into()),
                    size_bytes: 512,
                },
            ),
            (
                "model".into(),
                BlobRef {
                    digest: Digest(format!("sha256:{sha}")),
                    version: Some(version_id.into()),
                    size_bytes: 4096,
                },
            ),
        ]),
        provenance_commit: 1,
        dependency: RelevantStateToken::default(),
        freshness: FreshnessMode::AppendOnly,
        destination: None,
        deadline_millis,
        retry: RetryCeiling {
            max_attempts: 2,
            max_cost_units: 100,
        },
        fencing: Fencing {
            token: format!("fence-{cmd_id}"),
            cancellation_generation: 0,
        },
    };
    let dispatched = DispatchedCommand::pin(invocation, Digest(EVAL_PROFILE_DIGEST.to_owned()))
        .map_err(|e| format!("pin: {e}"))?;
    let output_manifest =
        output_manifest_digest(&[], &summary).map_err(|e| format!("manifest: {e}"))?;
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
            executor: EVAL_EXECUTOR.to_owned(),
            placement: "ml-evaluator/node-1".to_owned(),
            runtime: Digest(
                "sha256:07d4824036af339fa1b6ef769cc99d9c10a141769538b52b3b597c84f7a7b535"
                    .to_owned(),
            ),
        },
        resources: ResourceUse {
            cpu_millis: 10,
            wall_millis: 25,
            peak_memory_bytes: 16 * 1024 * 1024,
            output_bytes: 256,
            fuel: None,
        },
        cache: CacheOrigin::Miss {},
    };
    Ok((dispatched, receipt))
}

fn seed_production_model_version(book: &mut PlanBook, version_id: &str) -> Result<(), String> {
    let sha = "ab".repeat(32);
    book.ok(
        "tracking.experiment.create",
        "eng:dina",
        json!({"experiment_id": "exp-plan", "name": "plan-demand", "artifact_location": "s3://lab/exp", "creation_time": 1, "tags": "[]"}),
    )?;
    for run_id in ["run-train", "run-eval"] {
        book.ok(
            "tracking.run.create",
            "eng:dina",
            json!({
                "run_id": run_id, "experiment_id": "exp-plan", "run_name": run_id,
                "user_id": "eng:dina", "start_time": 1,
                "artifact_uri": format!("s3://lab/{run_id}"), "tags": "[]"
            }),
        )?;
        book.ok("tracking.run.finish", "eng:dina", json!({"run_id": run_id}))?;
    }
    book.ok(
        "ml.modelversion.register",
        "eng:dina",
        json!({
            "version_id": version_id, "model_id": "supply.demand_forecast",
            "version": version_id, "training_run": "run-train", "evaluation_run": "run-eval",
            "artifact_uri": format!("s3://lab/{version_id}"), "artifact_sha256": sha,
            "artifact_size": 4096, "input_type": "supply.DemandObservation",
            "input_revision": "c1", "pinned_dataset_revision": "c1"
        }),
    )?;
    let (dispatched, receipt) = build_eval_receipt(
        &book.scope,
        version_id,
        &sha,
        book.now().as_millis().saturating_add(1_000_000),
    )?;
    let cmd_id = dispatched.invocation.command_id.clone();
    let encoded = match norn_ir::runtime::encode(&receipt).map_err(|e| format!("encode: {e}"))? {
        RuntimeValue::Object(obj) => obj,
        other => return Err(format!("encoded receipt not object: {other:?}")),
    };
    if let Ok(mut map) = book.receipt_answers.lock() {
        map.insert(cmd_id.clone(), encoded);
    }
    let state = norn_ir::runtime::encode(&CommandState::dispatched(&dispatched))
        .map_err(|e| format!("encode state: {e}"))?;
    let inv_val =
        norn_ir::runtime::encode(&dispatched.invocation).map_err(|e| format!("encode inv: {e}"))?;
    let freshness = dispatched
        .freshness()
        .ok_or_else(|| "missing freshness".to_owned())?;
    book.cell
        .commit(CellCommit {
            writes: vec![FactWrite {
                key: FactKey::new(FUNCTION_COMMAND_TYPE, cmd_id.clone()),
                expected: FactVersion::ABSENT,
                mutation: FactMutation::Put {
                    values: JsonObject::from([("state".into(), state)]),
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
                        "type".into(),
                        RuntimeValue::String(FUNCTION_PAYLOAD_TYPE.to_owned()),
                    ),
                    ("invocation".into(), inv_val),
                ]),
            })],
            ..CellCommit::empty()
        })
        .map_err(|e| format!("enqueue eval: {e}"))?;
    let _performed = book.cell.perform().map_err(|e| format!("perform: {e}"))?;
    let command = dispatched.command_id();
    let digest = book
        .cell
        .retain_function_receipt(
            &dispatched,
            &Completion {
                receipt: &receipt,
                command: &command,
                generation: dispatched.generation(),
                authenticated_executor: Some(EVAL_EXECUTOR),
            },
            &ResultChecks {
                authorizer: &EvaluatorContract,
                validator: &EvaluatorContract,
            },
        )
        .map_err(|e| format!("retain receipt: {e}"))?;
    book.ok(
        "ml.modelversion.accept_evaluation",
        "eng:dina",
        json!({"version_id": version_id, "evaluation_receipt": digest.0}),
    )?;
    book.ok(
        "ml.modelversion.promote",
        "director:salma",
        json!({"version_id": version_id}),
    )
}

#[test]
fn pack_admits_cleanly_and_enforces_disjointness_from_commitment_and_actuals() -> Result<(), String>
{
    let pack = plan_pack()?;
    let registry = BTreeSet::new();
    let ctx = EntityArtifactSetContext {
        registry: &registry,
        base: &pack.base,
        tenant: None,
    };
    let envelope_entity = pack
        .candidate
        .iter()
        .find(|a| a.artifact_id.as_str() == "plan.OtbEnvelope.entity")
        .ok_or("missing plan.OtbEnvelope.entity")?;

    for forbidden in [
        "core.Commitment",
        "supply.SaleEvidence",
        "supply.CountEvidence",
        "supply.ReceiptEvidence",
    ] {
        let mut mutated = serde_json::to_value(envelope_entity).map_err(|e| format!("{e}"))?;
        let implements = mutated
            .get_mut("payload")
            .and_then(|p| p.get_mut("objectType"))
            .and_then(|o| o.get_mut("implements"))
            .ok_or("missing implements field")?;
        *implements = json!(["plan.PlanTarget", forbidden]);
        let mutated: ArtifactEnvelope =
            serde_json::from_value(mutated).map_err(|e| format!("{e}"))?;
        let mut set: Vec<ArtifactEnvelope> = pack
            .candidate
            .iter()
            .filter(|a| a.artifact_id.as_str() != "plan.OtbEnvelope.entity")
            .cloned()
            .collect();
        set.push(mutated);
        let findings = norn_checker::check_artifact_set(&set, &ctx);
        assert!(
            findings
                .iter()
                .any(|d| d.code == "interface.validation.disjoint_violation"),
            "PlanTarget + {forbidden} must refuse as disjoint: {findings:?}"
        );
    }
    Ok(())
}

#[test]
fn plan_version_bitemporal_lifecycle_locked_immutability_and_supersession() -> Result<(), String> {
    let mut book = PlanBook::open("org_plan_lc", "prod", "plan-lifecycle.sqlite")?;
    seed_retail_base(&mut book, "LE-SOLE", "EGP", &["ST-CFC"])?;

    book.ok(
        "plan.version.create",
        "planner:amal",
        json!({
            "plan_version_id": "PV-1", "entity_id": "LE-SOLE",
            "plan_code": "FY2026-AUTUMN", "revision_number": 1, "currency": "EGP",
            "effective_from": "2026-09-01T00:00:00Z", "effective_to": "2026-10-01T00:00:00Z",
            "recorded_at": "2026-08-20T09:00:00Z"
        }),
    )?;
    book.ok(
        "plan.version.update_draft",
        "planner:amal",
        json!({
            "plan_version_id": "PV-1",
            "effective_from": "2026-09-01T00:00:00Z", "effective_to": "2026-11-01T00:00:00Z",
            "recorded_at": "2026-08-21T09:00:00Z"
        }),
    )?;
    book.ok(
        "plan.version.submit",
        "planner:amal",
        json!({"plan_version_id": "PV-1"}),
    )?;
    let (self_approve_code, _) = book.refused(
        "plan.version.approve",
        "planner:amal",
        json!({"plan_version_id": "PV-1"}),
    )?;
    assert_eq!(self_approve_code, "action.permission_denied");
    book.ok(
        "plan.version.approve",
        "director:salma",
        json!({"plan_version_id": "PV-1"}),
    )?;
    book.ok(
        "plan.version.lock",
        "director:salma",
        json!({"plan_version_id": "PV-1"}),
    )?;

    // Post-lock mutation MUST refuse (`lifecycle.validation.immutable_state_write`).
    let (immutable_code, _) = book.refused(
        "plan.version.update_draft",
        "planner:amal",
        json!({
            "plan_version_id": "PV-1",
            "effective_from": "2026-09-01T00:00:00Z", "effective_to": "2026-12-01T00:00:00Z",
            "recorded_at": "2026-08-22T09:00:00Z"
        }),
    )?;
    assert_eq!(immutable_code, "lifecycle.validation.immutable_state_write");

    // Adding a PlanLine to a locked PlanVersion refuses.
    let (locked_line_code, _) = book.refused(
        "plan.line.record",
        "planner:amal",
        json!({
            "plan_line_id": "PL-LATE", "plan_version_id": "PV-1",
            "entity_id": "LE-SOLE", "store_id": "ST-CFC",
            "category": "dry_goods", "brand": "nile_foods", "plan_month": "2026-09",
            "period_start": "2026-09-01T00:00:00Z", "as_of": "2026-09-30T23:59:59Z",
            "currency": "EGP", "unit": "each",
            "planned_sales_retail": "1000.000000", "planned_sales_cost": "600.000000",
            "planned_sales_qty": "10.000000", "planned_receipts_cost": "600.000000",
            "planned_receipts_retail": "1000.000000", "planned_receipts_qty": "10.000000",
            "planned_markdowns_cost": "0.000000", "planned_markdowns_retail": "0.000000",
            "planned_fine_metal_g": "0.000000", "spread_remainder_cost": "0.000000",
            "planned_eom_stock_cost": "200.000000", "planned_eom_stock_retail": "350.000000",
            "planned_eom_stock_qty": "3.000000"
        }),
    )?;
    assert_eq!(locked_line_code, "action.precondition_failed");

    // Supersede cleanly without mutating locked history.
    book.ok(
        "plan.version.create_revision",
        "planner:amal",
        json!({
            "plan_version_id": "PV-2", "entity_id": "LE-SOLE",
            "plan_code": "FY2026-AUTUMN", "revision_number": 2, "currency": "EGP",
            "effective_from": "2026-09-01T00:00:00Z", "effective_to": "2026-11-01T00:00:00Z",
            "recorded_at": "2026-09-02T09:00:00Z", "supersedes_version_id": "PV-1"
        }),
    )?;
    book.ok(
        "plan.version.submit",
        "planner:amal",
        json!({"plan_version_id": "PV-2"}),
    )?;
    book.ok(
        "plan.version.approve",
        "director:salma",
        json!({"plan_version_id": "PV-2"}),
    )?;
    book.ok(
        "plan.version.lock",
        "director:salma",
        json!({"plan_version_id": "PV-2"}),
    )?;
    book.ok(
        "plan.version.supersede",
        "director:salma",
        json!({"plan_version_id": "PV-1", "successor_version_id": "PV-2"}),
    )?;
    let pv1 = book
        .cell
        .load(&FactKey::new("plan.PlanVersion", "PV-1"))
        .map_err(|e| format!("load PV-1: {e}"))?;
    assert_eq!(
        pv1.values
            .as_ref()
            .and_then(|v| row_str(v, "status"))
            .as_deref(),
        Some("superseded")
    );
    Ok(())
}

#[test]
fn top_down_spread_conservation_exact_and_refused_drift() -> Result<(), String> {
    let mut book = PlanBook::open("org_plan_sp", "prod", "plan-spread.sqlite")?;
    seed_retail_base(&mut book, "LE-SOLE", "EGP", &["ST-CFC", "ST-MOE"])?;
    book.ok(
        "plan.version.create",
        "planner:amal",
        json!({
            "plan_version_id": "PV-SP", "entity_id": "LE-SOLE",
            "plan_code": "FY2026-GOLD", "revision_number": 1, "currency": "EGP",
            "effective_from": "2026-09-01T00:00:00Z", "effective_to": "2026-10-01T00:00:00Z",
            "recorded_at": "2026-08-25T09:00:00Z"
        }),
    )?;

    // Case 1: 0.01 EGP rounding drift MUST refuse to seal.
    book.ok(
        "plan.spread_target.create",
        "planner:amal",
        json!({
            "spread_target_id": "SP-DRIFT", "plan_version_id": "PV-SP",
            "entity_id": "LE-SOLE", "category": "dry_goods", "brand": "nile_foods",
            "plan_month": "2026-09", "currency": "EGP", "rounding_rule": "largest_remainder",
            "target_sales_retail": "10000.000000", "target_sales_cost": "6000.000000",
            "target_receipts_cost": "1000.010000", "target_receipts_retail": "1600.000000",
            "target_markdowns_cost": "100.000000", "target_markdowns_retail": "150.000000",
            "target_eom_stock_cost": "2000.000000", "target_fine_metal_g": "0.000000",
            "target_rounding_remainder_cost": "0.010000"
        }),
    )?;
    for (line_id, store_id) in [("PL-DRIFT-1", "ST-CFC"), ("PL-DRIFT-2", "ST-MOE")] {
        book.ok(
            "plan.line.record_spread",
            "planner:amal",
            json!({
                "plan_line_id": line_id, "plan_version_id": "PV-SP",
                "spread_target_id": "SP-DRIFT", "entity_id": "LE-SOLE", "store_id": store_id,
                "category": "dry_goods", "brand": "nile_foods", "plan_month": "2026-09",
                "period_start": "2026-09-01T00:00:00Z", "as_of": "2026-09-30T23:59:59Z",
                "currency": "EGP", "unit": "each",
                "planned_sales_retail": "5000.000000", "planned_sales_cost": "3000.000000",
                "planned_sales_qty": "50.000000", "planned_receipts_cost": "500.000000",
                "planned_receipts_retail": "800.000000", "planned_receipts_qty": "10.000000",
                "planned_markdowns_cost": "50.000000", "planned_markdowns_retail": "75.000000",
                "planned_fine_metal_g": "0.000000", "spread_remainder_cost": "0.000000",
                "planned_eom_stock_cost": "1000.000000", "planned_eom_stock_retail": "1600.000000",
                "planned_eom_stock_qty": "20.000000"
            }),
        )?;
    }
    let (drift_code, _) = book.refused(
        "plan.spread_target.seal",
        "planner:amal",
        json!({"spread_target_id": "SP-DRIFT"}),
    )?;
    assert_eq!(drift_code, "action.precondition_failed");

    // Case 2: Exact spread with explicit 0.01 EGP remainder + UCUM fine-metal grams seals cleanly.
    book.ok(
        "plan.spread_target.create",
        "planner:amal",
        json!({
            "spread_target_id": "SP-EXACT", "plan_version_id": "PV-SP",
            "entity_id": "LE-SOLE", "category": "gold_jewelry", "brand": "nile_gold",
            "plan_month": "2026-09", "currency": "EGP", "rounding_rule": "largest_remainder",
            "target_sales_retail": "100000.000000", "target_sales_cost": "75000.000000",
            "target_receipts_cost": "10000.010000", "target_receipts_retail": "13500.000000",
            "target_markdowns_cost": "200.000000", "target_markdowns_retail": "300.000000",
            "target_eom_stock_cost": "30000.000000", "target_fine_metal_g": "150.750000",
            "target_rounding_remainder_cost": "0.010000"
        }),
    )?;
    for (line_id, store_id, sr, sc, sq, rc, rr, rq, mc, mr, fg, rem, ec, er, eq) in [
        (
            "PL-GOLD-CFC",
            "ST-CFC",
            "60000.000000",
            "45000.000000",
            "90.000000",
            "5000.010000",
            "8100.000000",
            "90.500000",
            "120.000000",
            "180.000000",
            "90.500000",
            "0.010000",
            "18000.000000",
            "24000.000000",
            "50.000000",
        ),
        (
            "PL-GOLD-MOE",
            "ST-MOE",
            "40000.000000",
            "30000.000000",
            "60.000000",
            "5000.000000",
            "5400.000000",
            "60.250000",
            "80.000000",
            "120.000000",
            "60.250000",
            "0.000000",
            "12000.000000",
            "16000.000000",
            "30.000000",
        ),
    ] {
        book.ok(
            "plan.line.record_spread",
            "planner:amal",
            json!({
                "plan_line_id": line_id, "plan_version_id": "PV-SP",
                "spread_target_id": "SP-EXACT", "entity_id": "LE-SOLE", "store_id": store_id,
                "category": "gold_jewelry", "brand": "nile_gold", "plan_month": "2026-09",
                "period_start": "2026-09-01T00:00:00Z", "as_of": "2026-09-30T23:59:59Z",
                "currency": "EGP", "unit": "g",
                "planned_sales_retail": sr, "planned_sales_cost": sc, "planned_sales_qty": sq,
                "planned_receipts_cost": rc, "planned_receipts_retail": rr, "planned_receipts_qty": rq,
                "planned_markdowns_cost": mc, "planned_markdowns_retail": mr,
                "planned_fine_metal_g": fg, "spread_remainder_cost": rem,
                "planned_eom_stock_cost": ec, "planned_eom_stock_retail": er, "planned_eom_stock_qty": eq
            }),
        )?;
    }
    book.ok(
        "plan.spread_target.seal",
        "planner:amal",
        json!({"spread_target_id": "SP-EXACT"}),
    )
}

#[test]
fn otb_partial_receipt_cancellation_release_and_separate_cash_timing() -> Result<(), String> {
    let mut book = PlanBook::open("org_plan_otb", "prod", "plan-otb.sqlite")?;
    seed_locked_plan_and_envelope(&mut book)?;
    create_submitted_po(
        &mut book,
        "PO-1",
        "PO-1/1",
        "SKU-DRY-1",
        "100.000000",
        "60.0000",
    )?;

    // Authorize PO-1/1 (6,000 EGP against 10,000 EGP OTB), cash due in October.
    book.ok(
        "plan.otb.authorize_po_line",
        "ops:karim",
        otb_auth_input("OTB-1", "ENV-DRY-SEP", "PO-1", "PO-1/1", "60.0000"),
    )?;
    let pos_after_auth = book.transform("plan.otb_position")?;
    let row = pos_after_auth
        .rows
        .first()
        .ok_or("missing otb_position row")?;
    assert_eq!(
        row_str(&row.values, "approved_budget_cost").as_deref(),
        Some("10000")
    );
    assert_eq!(
        row_str(&row.values, "recognized_receipts_cost").as_deref(),
        Some("0")
    );
    assert_eq!(
        row_str(&row.values, "unreceived_authorized_po_cost").as_deref(),
        Some("6000")
    );
    assert_eq!(
        row_str(&row.values, "remaining_otb_cost").as_deref(),
        Some("4000")
    );

    // Partial receipt of 40 units (2,400 EGP): net remaining OTB stays 4,000 EGP.
    book.ok(
        "supply.receipt.record",
        "clerk:hana",
        json!({
            "receipt_id": "RC-1", "supplier_id": "SUP-NILE", "warehouse_id": "WH-MAIN",
            "line_id": "PO-1/1", "received_qty": "40.000000", "damaged_qty": "0.000000",
            "unit": "each", "received_at": "2026-09-08T10:00:00Z"
        }),
    )?;
    book.ok(
        "supply.line.record_received",
        "clerk:hana",
        json!({
            "line_id": "PO-1/1", "po_id": "PO-1", "sku_id": "SKU-DRY-1",
            "promised_at": "2026-09-10T00:00:00Z", "ordered_qty": "100.000000",
            "received_qty": "40.000000", "unit": "each", "unit_price": "60.0000", "currency": "EGP"
        }),
    )?;
    book.ok(
        "supply.po.receive_partial",
        "clerk:hana",
        json!({"po_id": "PO-1"}),
    )?;
    book.ok(
        "plan.otb.record_partial_receipt",
        "clerk:hana",
        json!({"binding_id": "OTB-1", "line_id": "PO-1/1", "receipt_id": "RC-1", "as_of": "2026-09-08T11:00:00Z"}),
    )?;
    let pos_after_partial = book.transform("plan.otb_position")?;
    let row_p = pos_after_partial
        .rows
        .first()
        .ok_or("missing partial otb row")?;
    assert_eq!(
        row_str(&row_p.values, "recognized_receipts_cost").as_deref(),
        Some("2400")
    );
    assert_eq!(
        row_str(&row_p.values, "unreceived_authorized_po_cost").as_deref(),
        Some("3600")
    );
    assert_eq!(
        row_str(&row_p.values, "remaining_otb_cost").as_deref(),
        Some("4000")
    );

    // Cancellation releases 3,600 EGP unreceived -> remaining OTB rises 4,000 -> 7,600 EGP.
    book.ok(
        "plan.otb.release_cancelled_po",
        "ops:karim",
        json!({"binding_id": "OTB-1", "po_id": "PO-1", "as_of": "2026-09-12T10:00:00Z"}),
    )?;
    let pos_after_cancel = book.transform("plan.otb_position")?;
    let row_c = pos_after_cancel
        .rows
        .first()
        .ok_or("missing cancel otb row")?;
    assert_eq!(
        row_str(&row_c.values, "recognized_receipts_cost").as_deref(),
        Some("2400")
    );
    assert_eq!(
        row_str(&row_c.values, "unreceived_authorized_po_cost").as_deref(),
        Some("0")
    );
    assert_eq!(
        row_str(&row_c.values, "cancelled_released_cost").as_deref(),
        Some("3600")
    );
    assert_eq!(
        row_str(&row_c.values, "remaining_otb_cost").as_deref(),
        Some("7600")
    );

    // New PO for 7,000 EGP now fits inside 7,600 EGP remaining OTB; cash due in October.
    create_submitted_po(
        &mut book,
        "PO-2",
        "PO-2/1",
        "SKU-DRY-1",
        "100.000000",
        "70.0000",
    )?;
    book.ok(
        "plan.otb.authorize_po_line",
        "ops:karim",
        otb_auth_input("OTB-2", "ENV-DRY-SEP", "PO-2", "PO-2/1", "70.0000"),
    )?;
    let cash = book.transform("plan.expected_cash_outflow_monthly")?;
    let cash_row = cash.rows.first().ok_or("missing cash outflow row")?;
    assert_eq!(
        row_str(&cash_row.values, "expected_payment_date").as_deref(),
        Some("2026-10-01")
    );
    assert_eq!(
        row_str(&cash_row.values, "expected_cash_outflow_cost").as_deref(),
        Some("9400")
    );
    Ok(())
}

#[test]
fn otb_fail_closed_guards_no_envelope_unplanned_override_and_non_vacuity() -> Result<(), String> {
    let mut book = PlanBook::open("org_plan_guard", "prod", "plan-guards.sqlite")?;
    seed_locked_plan_and_envelope(&mut book)?;

    // 1. No-envelope category (`spares`) refuses fail-closed (`action.slice_object_missing`).
    create_submitted_po(
        &mut book,
        "PO-SP",
        "PO-SP/1",
        "SKU-SPARE-1",
        "20.000000",
        "100.0000",
    )?;
    let spares_auth = |binding_id: &str, envelope_id: &str| {
        json!({
            "binding_id": binding_id, "envelope_id": envelope_id, "plan_version_id": "PV-2026-V1",
            "entity_id": "LE-SOLE", "store_id": "ST-CFC", "category": "spares",
            "brand": "unplanned", "plan_month": "2026-09", "po_id": "PO-SP", "line_id": "PO-SP/1",
            "currency": "EGP", "unit": "each", "unit_cost": "100.0000",
            "expected_payment_date": "2026-10-20T00:00:00Z", "payment_terms_days": 30,
            "as_of": "2026-09-05T10:00:00Z"
        })
    };
    let (missing_env_code, _) = book.refused(
        "plan.otb.authorize_po_line",
        "ops:karim",
        spares_auth("OTB-NO-ENV", "ENV-MISSING"),
    )?;
    assert_eq!(missing_env_code, "action.slice_object_missing");

    // 2. Explicit `unplanned` envelope refuses until submitted & independently approved.
    book.ok(
        "plan.otb_envelope.request_unplanned",
        "planner:amal",
        json!({
            "envelope_id": "ENV-UNPLANNED-SPARES", "plan_version_id": "PV-2026-V1",
            "entity_id": "LE-SOLE", "store_id": "ST-CFC", "category": "spares",
            "brand": "unplanned", "plan_month": "2026-09",
            "period_start": "2026-09-01T00:00:00Z", "currency": "EGP",
            "approved_budget_cost": "2500.000000", "override_reason": "Emergency chiller spares"
        }),
    )?;
    // Unplanned envelope cannot bypass override via activate_planned.
    let (bypass_code, _) = book.refused(
        "plan.otb_envelope.activate_planned",
        "planner:amal",
        json!({"envelope_id": "ENV-UNPLANNED-SPARES"}),
    )?;
    assert_eq!(bypass_code, "action.precondition_failed");
    book.ok(
        "plan.otb_envelope.submit_override",
        "planner:amal",
        json!({"envelope_id": "ENV-UNPLANNED-SPARES"}),
    )?;
    let (pending_code, _) = book.refused(
        "plan.otb.authorize_po_line",
        "ops:karim",
        spares_auth("OTB-SP-PENDING", "ENV-UNPLANNED-SPARES"),
    )?;
    assert_eq!(pending_code, "action.precondition_failed");
    let (self_override_code, _) = book.refused(
        "plan.otb_envelope.approve_override",
        "planner:amal",
        json!({"envelope_id": "ENV-UNPLANNED-SPARES"}),
    )?;
    assert_eq!(self_override_code, "action.permission_denied");
    book.ok(
        "plan.otb_envelope.approve_override",
        "director:salma",
        json!({"envelope_id": "ENV-UNPLANNED-SPARES"}),
    )?;
    book.ok(
        "plan.otb.authorize_po_line",
        "ops:karim",
        spares_auth("OTB-SP-OK", "ENV-UNPLANNED-SPARES"),
    )?;

    // 3. Non-vacuity: allowed PO (6,000 <= 10,000) commits; second PO (+5,000 -> 11,000) refuses.
    create_submitted_po(
        &mut book,
        "PO-NV1",
        "PO-NV1/1",
        "SKU-DRY-1",
        "100.000000",
        "60.0000",
    )?;
    book.ok(
        "plan.otb.authorize_po_line",
        "ops:karim",
        otb_auth_input("OTB-NV1", "ENV-DRY-SEP", "PO-NV1", "PO-NV1/1", "60.0000"),
    )?;
    create_submitted_po(
        &mut book,
        "PO-NV2",
        "PO-NV2/1",
        "SKU-DRY-1",
        "100.000000",
        "50.0000",
    )?;
    let (breach_code, _) = book.refused(
        "plan.otb.authorize_po_line",
        "ops:karim",
        otb_auth_input("OTB-NV2", "ENV-DRY-SEP", "PO-NV2", "PO-NV2/1", "50.0000"),
    )?;
    assert_eq!(breach_code, "action.precondition_failed");
    Ok(())
}

#[test]
fn concurrent_otb_cap_consumption_refuses_phantom_breach() -> Result<(), String> {
    let mut book = PlanBook::open("org_plan_race", "prod", "plan-race.sqlite")?;
    seed_locked_plan_and_envelope(&mut book)?;
    create_submitted_po(
        &mut book,
        "PO-A",
        "PO-A/1",
        "SKU-DRY-1",
        "100.000000",
        "60.0000",
    )?;
    create_submitted_po(
        &mut book,
        "PO-B",
        "PO-B/1",
        "SKU-DRY-1",
        "100.000000",
        "60.0000",
    )?;

    let admitted = book
        .pack
        .definitions
        .action("plan.otb.authorize_po_line")
        .ok_or("missing authorize_po_line")?;
    let catalog = book.pack.definitions.catalog();
    let input_a: JsonObject = serde_json::from_value(otb_auth_input(
        "OTB-A",
        "ENV-DRY-SEP",
        "PO-A",
        "PO-A/1",
        "60.0000",
    ))
    .map_err(|e| format!("input A: {e}"))?;
    let input_b: JsonObject = serde_json::from_value(otb_auth_input(
        "OTB-B",
        "ENV-DRY-SEP",
        "PO-B",
        "PO-B/1",
        "60.0000",
    ))
    .map_err(|e| format!("input B: {e}"))?;

    let scope = BTreeMap::from([
        ("orgId".into(), book.org_id.clone()),
        ("environmentId".into(), book.env_id.clone()),
    ]);
    let now_a = LogicalTime::from_millis(CLOCK_MS.saturating_add(500_000));
    let now_b = LogicalTime::from_millis(CLOCK_MS.saturating_add(501_000));
    let world = book.cell.store().world();
    book.cell.advance_to(now_b);

    let make_call = |id: &str, now: LogicalTime, input: JsonObject| ActionCall {
        action: &admitted.root,
        action_artifact_id: &admitted.artifact_id,
        action_version: &admitted.version,
        scope: scope.clone(),
        actor: "ops:karim".to_owned(),
        approved_intent: None,
        input,
        idempotency_key: id.to_owned(),
        object_type: None,
        turn: DecisionTurn {
            id: id.to_owned(),
            now,
            world: world.clone(),
            consumes: None,
        },
        trace: None,
    };
    let call_a = make_call("race-po-a", now_a, input_a);
    let call_b = make_call("race-po-b", now_b, input_b);

    // Call B prepares against empty `OtbPoBinding` (0 + 6,000 <= 10,000); right
    // before B commits, Call A commits 6,000 into ENV-DRY-SEP -> B MUST refuse
    // on TXN-PHANTOM predicate-read conflict (`action.aggregate_stale_read`).
    let mut race = RaceHost {
        cell: &mut book.cell,
        injected: Some(|cell: &mut PlanCell| {
            commit_action(cell, &catalog, &call_a, &Allow)
                .map_err(|e| format!("call A commit failed: {e}"))?;
            Ok(())
        }),
    };
    let race_err = match commit_action(&mut race, &catalog, &call_b, &Allow) {
        Ok(outcome) => return Err(format!("expected TXN-PHANTOM refusal, got {outcome:?}")),
        Err(err) => err,
    };
    assert_eq!(race_err.code(), "action.aggregate_stale_read");

    let retry_err = match commit_action(&mut book.cell, &catalog, &call_b, &Allow) {
        Ok(outcome) => return Err(format!("expected OTB cap refusal, got {outcome:?}")),
        Err(err) => err,
    };
    assert_eq!(retry_err.code(), "action.precondition_failed");

    let pos = book.transform("plan.otb_position")?;
    let row = pos.rows.first().ok_or("missing post-race OTB row")?;
    assert_eq!(
        row_str(&row.values, "unreceived_authorized_po_cost").as_deref(),
        Some("6000")
    );
    assert_eq!(
        row_str(&row.values, "remaining_otb_cost").as_deref(),
        Some("4000")
    );
    Ok(())
}

#[test]
fn bitemporal_actual_and_forecast_variance_with_asof_restatement_and_unit_partitioning()
-> Result<(), String> {
    let mut egp_book = PlanBook::open("org_plan_sole", "prod", "plan-egp.sqlite")?;
    seed_locked_plan_and_envelope(&mut egp_book)?;

    // Initial September POS sale (12,000 EGP, 120 each) -> snapshot K1 cutoff.
    egp_book.ok(
        "supply.pos_sale.record",
        "clerk:hana",
        json!({
            "sale_id": "SALE-SEP-1", "store_id": "ST-CFC", "sku_id": "SKU-DRY-1",
            "sold_qty": "120.000000", "unit": "each", "net_revenue": "12000.000000",
            "applied_unit_cost": "60.0000", "cost_status": "costed", "costed_ref": "COST-DRY-1",
            "currency": "EGP", "sold_at": "2026-09-10T14:00:00Z"
        }),
    )?;
    let k1 = egp_book.last_cutoff()?;
    let rev_k1_before = egp_book.answer_known_at("plan.actual_pos_revenue_monthly", k1)?;
    assert_eq!(
        bucket_decimal(&rev_k1_before, &["ST-CFC", "EGP", "2026-09-01"]).as_deref(),
        Some("12000")
    );

    // Late correction / restatement arrives at K2 (+3,500 EGP, +35 each).
    egp_book.ok(
        "supply.pos_sale.record",
        "clerk:hana",
        json!({
            "sale_id": "SALE-SEP-LATE", "store_id": "ST-CFC", "sku_id": "SKU-DRY-1",
            "sold_qty": "35.000000", "unit": "each", "net_revenue": "3500.000000",
            "applied_unit_cost": "60.0000", "cost_status": "costed", "costed_ref": "COST-DRY-1",
            "currency": "EGP", "sold_at": "2026-09-18T16:00:00Z"
        }),
    )?;

    // AsOfReader (#1888): K1 historical answer stays 12,000 while live restated answer is 15,500.
    assert_eq!(
        egp_book.answer_known_at("plan.actual_pos_revenue_monthly", k1)?,
        rev_k1_before
    );
    let rev_live = egp_book.answer("plan.actual_pos_revenue_monthly")?;
    assert_eq!(
        bucket_decimal(&rev_live, &["ST-CFC", "EGP", "2026-09-01"]).as_deref(),
        Some("15500")
    );
    let plan_sales = egp_book.answer("plan.planned_sales_retail_monthly")?;
    assert_eq!(
        bucket_decimal(
            &plan_sales,
            &["PV-2026-V1", "ST-CFC", "dry_goods", "EGP", "2026-09-01"]
        )
        .as_deref(),
        Some("18000")
    );
    let actual_grain = egp_book.transform("plan.actual_sales_at_plan_grain")?;
    let ag_row = actual_grain
        .rows
        .first()
        .ok_or("missing actual_sales_at_plan_grain")?;
    assert_eq!(
        row_str(&ag_row.values, "actual_sales_retail").as_deref(),
        Some("15500")
    );
    assert_eq!(
        row_str(&ag_row.values, "actual_sales_qty").as_deref(),
        Some("155")
    );

    // Forecast - Plan variance pinning PlanVersion + production ml.ModelVersion + forecast_revision.
    seed_production_model_version(&mut egp_book, "supply.demand_forecast@1")?;
    egp_book.ok(
        "plan.forecast.record",
        "eng:dina",
        json!({
            "forecast_id": "PF-DRY-R1", "plan_version_id": "PV-2026-V1",
            "plan_line_id": "PL-DRY-SEP", "model_version": "supply.demand_forecast@1",
            "forecast_revision": "rev-2026-09-w2", "entity_id": "LE-SOLE", "store_id": "ST-CFC",
            "category": "dry_goods", "brand": "nile_foods", "plan_month": "2026-09",
            "period_start": "2026-09-01T00:00:00Z", "published_at": "2026-09-12T08:00:00Z",
            "currency": "EGP", "unit": "each",
            "forecast_sales_retail": "19250.500000", "forecast_sales_cost": "11400.000000",
            "forecast_sales_qty": "192.000000", "forecast_fine_metal_g": "0.000000"
        }),
    )?;
    egp_book.ok(
        "plan.forecast.publish",
        "eng:dina",
        json!({"forecast_id": "PF-DRY-R1"}),
    )?;
    let fc_var = egp_book.transform("plan.forecast_vs_plan_variance")?;
    let fv_row = fc_var
        .rows
        .first()
        .ok_or("missing forecast_vs_plan_variance")?;
    assert_eq!(
        row_str(&fv_row.values, "plan_version_id").as_deref(),
        Some("PV-2026-V1")
    );
    assert_eq!(
        row_str(&fv_row.values, "model_version").as_deref(),
        Some("supply.demand_forecast@1")
    );
    assert_eq!(
        row_str(&fv_row.values, "forecast_revision").as_deref(),
        Some("rev-2026-09-w2")
    );
    assert_eq!(
        row_str(&fv_row.values, "sales_retail_variance").as_deref(),
        Some("1250.5")
    );
    assert_eq!(
        row_str(&fv_row.values, "sales_cost_variance").as_deref(),
        Some("600")
    );
    assert_eq!(
        row_str(&fv_row.values, "sales_qty_variance").as_deref(),
        Some("12")
    );

    // Separate USD entity cell with UCUM fine-metal grams (`g`) proves currency & unit partitioning.
    let mut usd_book = PlanBook::open("org_plan_partner", "prod", "plan-usd.sqlite")?;
    seed_retail_base(&mut usd_book, "LE-PARTNER", "USD", &["ST-LUX"])?;
    usd_book.ok(
        "plan.version.create",
        "planner:amal",
        json!({
            "plan_version_id": "PV-USD-V1", "entity_id": "LE-PARTNER",
            "plan_code": "FY2026-JEWELRY", "revision_number": 1, "currency": "USD",
            "effective_from": "2026-09-01T00:00:00Z", "effective_to": "2026-10-01T00:00:00Z",
            "recorded_at": "2026-08-25T09:00:00Z"
        }),
    )?;
    usd_book.ok(
        "plan.line.record",
        "planner:amal",
        json!({
            "plan_line_id": "PL-GOLD-USD", "plan_version_id": "PV-USD-V1",
            "entity_id": "LE-PARTNER", "store_id": "ST-LUX",
            "category": "gold_jewelry", "brand": "nile_gold", "plan_month": "2026-09",
            "period_start": "2026-09-01T00:00:00Z", "as_of": "2026-09-30T23:59:59Z",
            "currency": "USD", "unit": "g",
            "planned_sales_retail": "25000.000000", "planned_sales_cost": "19000.000000",
            "planned_sales_qty": "250.000000", "planned_receipts_cost": "20000.000000",
            "planned_receipts_retail": "26000.000000", "planned_receipts_qty": "260.000000",
            "planned_markdowns_cost": "100.000000", "planned_markdowns_retail": "150.000000",
            "planned_fine_metal_g": "248.750000", "spread_remainder_cost": "0.000000",
            "planned_eom_stock_cost": "8000.000000", "planned_eom_stock_retail": "10500.000000",
            "planned_eom_stock_qty": "105.000000"
        }),
    )?;
    let fine_metal = usd_book.answer("plan.planned_fine_metal_monthly")?;
    assert_eq!(
        bucket_decimal(
            &fine_metal,
            &["PV-USD-V1", "ST-LUX", "gold_jewelry", "2026-09-01"]
        )
        .as_deref(),
        Some("248.75")
    );
    let eom_usd = usd_book.answer("plan.planned_eom_stock_cost_monthly")?;
    assert_eq!(
        bucket_decimal(&eom_usd, &["PV-USD-V1", "ST-LUX", "USD", "2026-09-01"]).as_deref(),
        Some("8000")
    );
    Ok(())
}
