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
        .map_err(|err| format!("{}: {err}", dir.display()))?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "json"))
        .collect();
    paths.sort();
    paths
        .iter()
        .map(|path| {
            let text =
                fs::read_to_string(path).map_err(|err| format!("{}: {err}", path.display()))?;
            serde_json::from_str(&text).map_err(|err| format!("{}: {err}", path.display()))
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
        let text = fs::read_to_string(core.join(name)).map_err(|err| format!("{name}: {err}"))?;
        serde_json::from_str(&text).map_err(|err| format!("{name}: {err}"))
    })
    .collect()
}

fn runtime_envelopes(artifacts: &[ArtifactEnvelope]) -> Result<Vec<RuntimeValue>, String> {
    artifacts
        .iter()
        .map(|artifact| {
            let wire = serde_json::to_value(artifact).map_err(|err| format!("{err}"))?;
            RuntimeValue::from_json(wire).map_err(|err| format!("{err:?}"))
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
        AdmittedDefinitions::from_envelopes(&pool).map_err(|err| format!("definitions: {err}"))?;
    let (sources, base_sources) = (runtime_envelopes(&candidate)?, runtime_envelopes(&base)?);
    let registry = BTreeSet::new();
    let checked = norn_checker::check_artifact_set_with_receipt(
        &norn_checker::ReceiptSources::new(&sources, &base_sources)
            .map_err(|err| format!("{err:?}"))?,
        &EntityArtifactSetContext {
            registry: &registry,
            base: &base,
            tenant: None,
        },
        &norn_ir::ResourceLimitProfile::norn_limits_v1(),
    )
    .map_err(|err| format!("receipt: {err:?}"))?;
    if !checked.receipt.admitted {
        return Err(format!("plan pack not admitted: {:?}", checked.diagnostics));
    }
    let plan_laws: BTreeSet<&str> = candidate
        .iter()
        .filter(|art| art.namespace == "plan" && art.kind() == norn_ir::ArtifactKind::Law)
        .map(|art| art.artifact_id.as_str())
        .collect();
    let law_diags: Vec<_> = checked
        .diagnostics
        .iter()
        .filter(|diag| {
            diag.artifact_id
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
                .map_err(|err| format!("plan.questions.json: {err}"))?;
        let questions: Vec<QuestionIR> =
            serde_json::from_str(&text).map_err(|err| format!("decode plan questions: {err}"))?;
        let question = questions
            .into_iter()
            .find(|item| item.name == name)
            .ok_or_else(|| format!("missing plan question {name}"))?;
        let registry = BTreeSet::new();
        let space = derive_metric_space(
            &ScopedView::uniform(&self.pool, &registry),
            &LawRoot::Entity(question.object_set.as_str().to_owned()),
        )
        .map_err(|err| format!("derive metric space for {name}: {err:?}"))?;
        space
            .admit(&question, 100)
            .map_err(|err| format!("admit {name}: {err:?}"))
    }

    fn transform_plan(
        &self,
        artifact_id: &str,
    ) -> Result<norn_checker::transform::TransformPlan, String> {
        let artifact = self
            .candidate
            .iter()
            .find(|item| item.artifact_id.as_str() == artifact_id)
            .ok_or_else(|| format!("missing transform {artifact_id}"))?;
        norn_checker::transform::admit(artifact, &self.pool, &BTreeSet::new())
            .map_err(|err| format!("admit transform {artifact_id}: {err:?}"))
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
        let dir = tempfile::tempdir().map_err(|err| format!("tempdir: {err}"))?;
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
        .map_err(|err| format!("open cell {file_name}: {err}"))?;
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
            serde_json::from_value(input).map_err(|err| format!("{action} input: {err}"))?;
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

    fn fact_state(&self, object_type: &str, id: &str) -> Result<FactState, String> {
        self.cell
            .load(&FactKey::new(object_type, id))
            .map_err(|err| format!("load {object_type}/{id}: {err}"))
    }

    fn fact_field(
        &self,
        object_type: &str,
        id: &str,
        field: &str,
    ) -> Result<Option<String>, String> {
        let state = self.fact_state(object_type, id)?;
        Ok(state.values.as_ref().and_then(|vals| row_str(vals, field)))
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
            .map_err(|err| format!("evaluate {name}: {err}"))
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
            .map_err(|err| format!("evaluate {name} known_at: {err}"))
    }

    fn last_cutoff(&self) -> Result<KnowledgeCutoff, String> {
        let receipt = self
            .cell
            .store()
            .read()
            .receipts()
            .map_err(|err| format!("receipts: {err}"))?
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
            .map_err(|err| format!("transform {artifact_id}: {err}"))
    }
}

fn scalar(value: &RuntimeValue) -> String {
    match value {
        RuntimeValue::String(text) => text.clone(),
        RuntimeValue::Number(dec) => dec.canonical(),
        RuntimeValue::Bool(flag) => flag.to_string(),
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
        let matched = parts.len() == expected.len()
            && parts
                .iter()
                .zip(expected.iter())
                .all(|(actual, want)| actual == *want);
        matched
            .then_some(row.value)
            .flatten()
            .map(|dec| dec.canonical())
    })
}

fn row_str(row: &BTreeMap<String, RuntimeValue>, col: &str) -> Option<String> {
    row.get(col).map(scalar)
}

/// Deterministic `TXN-PHANTOM` race wrapper: lets an outer action prepare its
/// predicate-fenced commit against the underlying `SqliteCell`, then injects a
/// competing commit right before forwarding `commit` or `commit_stamped`.
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
            inject(self.cell).map_err(|err| {
                CommitError::Host(CellError::DefinitionReceiptInvalid(format!(
                    "injected race commit failed: {err}"
                )))
            })?;
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
            inject(self.cell).map_err(|err| {
                CommitError::Host(CellError::DefinitionReceiptInvalid(format!(
                    "injected race stamped commit failed: {err}"
                )))
            })?;
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
    fn publish(&mut self, receipt: &CommitReceipt) -> Result<(), CellError> {
        self.cell.publish(receipt)
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
    for (sku_id, category, brand, shelf_days) in [
        ("SKU-DRY-1", "dry_goods", "nile_foods", 180),
        ("SKU-SPARE-1", "spares", "unplanned", 365),
    ] {
        book.ok(
            "supply.sku.register",
            "eng:dina",
            json!({
                "sku_id": sku_id, "category": category,
                "unit_of_measure": "each", "shelf_life_days": shelf_days
            }),
        )?;
        book.ok(
            "plan.sku_profile.register",
            "eng:dina",
            json!({"sku_id": sku_id, "brand": brand}),
        )?;
    }
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
            "effective_from": "2026-09-01T00:00:00Z", "effective_to": "2026-10-01T00:00:00Z"
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
            "planned_fine_metal_g": "0.000000",
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
        "plan.baseline.activate",
        "director:salma",
        json!({
            "slot_id": "SLOT-DRY-SEP", "plan_version_id": "PV-2026-V1",
            "plan_line_id": "PL-DRY-SEP", "entity_id": "LE-SOLE", "store_id": "ST-CFC",
            "category": "dry_goods", "unit": "each"
        }),
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

fn create_draft_po_with_destination(
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
        "plan.po_destination.assign",
        "planner:amal",
        json!({
            "po_id": po_id, "store_id": "ST-CFC", "entity_id": "LE-SOLE", "plan_month": "2026-09"
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
    )
}

fn otb_auth_input(binding_id: &str, envelope_id: &str, po_id: &str, line_id: &str) -> Value {
    json!({
        "binding_id": binding_id, "envelope_id": envelope_id,
        "po_id": po_id, "line_id": line_id,
        "expected_payment_date": "2026-10-25T00:00:00Z",
        "payment_terms_days": 45,
        "as_of": "2026-09-06T10:00:00Z"
    })
}

struct PartialReceipt<'a> {
    po_id: &'a str,
    line_id: &'a str,
    binding_id: &'a str,
    receipt_id: &'a str,
    delta_qty: &'a str,
    cumulative_qty: &'a str,
    received_at: &'a str,
    first: bool,
}

fn post_partial_receipt(book: &mut PlanBook, rc: &PartialReceipt<'_>) -> Result<(), String> {
    book.ok(
        "supply.receipt.record",
        "clerk:hana",
        json!({
            "receipt_id": rc.receipt_id, "supplier_id": "SUP-NILE", "warehouse_id": "WH-MAIN",
            "line_id": rc.line_id, "received_qty": rc.delta_qty, "damaged_qty": "0.000000",
            "unit": "each", "received_at": rc.received_at
        }),
    )?;
    book.ok(
        "supply.line.record_received",
        "clerk:hana",
        json!({
            "line_id": rc.line_id, "po_id": rc.po_id, "sku_id": "SKU-DRY-1",
            "promised_at": "2026-09-10T00:00:00Z", "ordered_qty": "100.000000",
            "received_qty": rc.cumulative_qty, "unit": "each",
            "unit_price": "60.0000", "currency": "EGP"
        }),
    )?;
    if rc.first {
        book.ok(
            "supply.po.receive_partial",
            "clerk:hana",
            json!({"po_id": rc.po_id}),
        )?;
        book.ok(
            "plan.otb.record_partial_receipt",
            "clerk:hana",
            json!({
                "binding_id": rc.binding_id, "line_id": rc.line_id,
                "receipt_id": rc.receipt_id, "as_of": rc.received_at
            }),
        )
    } else {
        book.ok(
            "plan.otb.update_partial_receipt",
            "clerk:hana",
            json!({
                "binding_id": rc.binding_id, "line_id": rc.line_id,
                "as_of": rc.received_at
            }),
        )
    }
}

fn build_eval_invocation(
    scope: &str,
    version_id: &str,
    sha: &str,
    deadline_millis: u64,
) -> FunctionInvocation {
    let cmd_id = "cmd-eval-plan-v1";
    FunctionInvocation {
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
    }
}

fn build_eval_receipt(
    scope: &str,
    version_id: &str,
    sha: &str,
    deadline_millis: u64,
) -> Result<(DispatchedCommand, FunctionReceipt), String> {
    let metric = Decimal::parse_wire("0.11").map_err(|err| format!("decimal: {err}"))?;
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
    let invocation = build_eval_invocation(scope, version_id, sha, deadline_millis);
    let dispatched = DispatchedCommand::pin(invocation, Digest(EVAL_PROFILE_DIGEST.to_owned()))
        .map_err(|err| format!("pin: {err}"))?;
    let output_manifest =
        output_manifest_digest(&[], &summary).map_err(|err| format!("manifest: {err}"))?;
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
            peak_memory_bytes: 16_777_216,
            output_bytes: 256,
            fuel: None,
        },
        cache: CacheOrigin::Miss {},
    };
    Ok((dispatched, receipt))
}

fn dispatch_eval_receipt(
    book: &mut PlanBook,
    dispatched: &DispatchedCommand,
    receipt: &FunctionReceipt,
) -> Result<String, String> {
    let cmd_id = dispatched.invocation.command_id.clone();
    let encoded = match norn_ir::runtime::encode(receipt).map_err(|err| format!("encode: {err}"))? {
        RuntimeValue::Object(obj) => obj,
        other => return Err(format!("encoded receipt not object: {other:?}")),
    };
    if let Ok(mut map) = book.receipt_answers.lock() {
        map.insert(cmd_id.clone(), encoded);
    }
    let state = norn_ir::runtime::encode(&CommandState::dispatched(dispatched))
        .map_err(|err| format!("encode state: {err}"))?;
    let inv_val = norn_ir::runtime::encode(&dispatched.invocation)
        .map_err(|err| format!("encode inv: {err}"))?;
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
        .map_err(|err| format!("enqueue eval: {err}"))?;
    let _performed = book
        .cell
        .perform()
        .map_err(|err| format!("perform: {err}"))?;
    let command = dispatched.command_id();
    let digest = book
        .cell
        .retain_function_receipt(
            dispatched,
            &Completion {
                receipt,
                command: &command,
                generation: dispatched.generation(),
                authenticated_executor: Some(EVAL_EXECUTOR),
            },
            &ResultChecks {
                authorizer: &EvaluatorContract,
                validator: &EvaluatorContract,
            },
        )
        .map_err(|err| format!("retain receipt: {err}"))?;
    Ok(digest.0)
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
    let digest = dispatch_eval_receipt(book, &dispatched, &receipt)?;
    book.ok(
        "ml.modelversion.accept_evaluation",
        "eng:dina",
        json!({"version_id": version_id, "evaluation_receipt": digest}),
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
        .find(|art| art.artifact_id.as_str() == "plan.OtbEnvelope.entity")
        .ok_or("missing plan.OtbEnvelope.entity")?;

    for forbidden in [
        "core.Commitment",
        "supply.SaleEvidence",
        "supply.CountEvidence",
        "supply.ReceiptEvidence",
    ] {
        let mut mutated = serde_json::to_value(envelope_entity).map_err(|err| format!("{err}"))?;
        let implements = mutated
            .get_mut("payload")
            .and_then(|val| val.get_mut("objectType"))
            .and_then(|val| val.get_mut("implements"))
            .ok_or("missing implements field")?;
        *implements = json!(["plan.PlanTarget", forbidden]);
        let mutated: ArtifactEnvelope =
            serde_json::from_value(mutated).map_err(|err| format!("{err}"))?;
        let mut set: Vec<ArtifactEnvelope> = pack
            .candidate
            .iter()
            .filter(|art| art.artifact_id.as_str() != "plan.OtbEnvelope.entity")
            .cloned()
            .collect();
        set.push(mutated);
        let findings = norn_checker::check_artifact_set(&set, &ctx);
        assert!(
            findings
                .iter()
                .any(|diag| diag.code == "interface.validation.disjoint_violation"),
            "PlanTarget + {forbidden} must refuse as disjoint: {findings:?}"
        );
    }
    Ok(())
}

fn record_dry_goods_plan_line(
    book: &mut PlanBook,
    plan_line_id: &str,
    plan_version_id: &str,
    sales_retail: &str,
    sales_cost: &str,
    sales_qty: &str,
) -> Result<(), String> {
    book.ok(
        "plan.line.record",
        "planner:amal",
        json!({
            "plan_line_id": plan_line_id, "plan_version_id": plan_version_id,
            "entity_id": "LE-SOLE", "store_id": "ST-CFC",
            "category": "dry_goods", "brand": "nile_foods", "plan_month": "2026-09",
            "period_start": "2026-09-01T00:00:00Z", "as_of": "2026-09-30T23:59:59Z",
            "currency": "EGP", "unit": "each",
            "planned_sales_retail": sales_retail, "planned_sales_cost": sales_cost,
            "planned_sales_qty": sales_qty, "planned_receipts_cost": "9000.000000",
            "planned_receipts_retail": "15000.000000", "planned_receipts_qty": "150.000000",
            "planned_markdowns_cost": "0.000000", "planned_markdowns_retail": "0.000000",
            "planned_fine_metal_g": "0.000000",
            "planned_eom_stock_cost": "3000.000000", "planned_eom_stock_retail": "5000.000000",
            "planned_eom_stock_qty": "50.000000"
        }),
    )
}

fn lock_plan_version(book: &mut PlanBook, plan_version_id: &str) -> Result<(), String> {
    book.ok(
        "plan.version.submit",
        "planner:amal",
        json!({"plan_version_id": plan_version_id}),
    )?;
    book.ok(
        "plan.version.approve",
        "director:salma",
        json!({"plan_version_id": plan_version_id}),
    )?;
    book.ok(
        "plan.version.lock",
        "director:salma",
        json!({"plan_version_id": plan_version_id}),
    )
}

#[test]
fn plan_version_draft_only_edits_refuse_post_submit_approve_and_lock() -> Result<(), String> {
    let mut book = PlanBook::open("org_plan_lc", "prod", "plan-draft-only.sqlite")?;
    seed_retail_base(&mut book, "LE-SOLE", "EGP", &["ST-CFC"])?;
    book.ok(
        "plan.version.create",
        "planner:amal",
        json!({
            "plan_version_id": "PV-1", "entity_id": "LE-SOLE",
            "plan_code": "FY2026-AUTUMN", "revision_number": 1, "currency": "EGP",
            "effective_from": "2026-09-01T00:00:00Z", "effective_to": "2026-10-01T00:00:00Z"
        }),
    )?;
    let edit_payload = json!({
        "plan_version_id": "PV-1",
        "effective_from": "2026-09-01T00:00:00Z", "effective_to": "2026-11-01T00:00:00Z"
    });
    book.ok("plan.version.update_draft", "planner:amal", edit_payload)?;
    record_dry_goods_plan_line(
        &mut book,
        "PL-V1",
        "PV-1",
        "15000.000000",
        "9000.000000",
        "150.000000",
    )?;
    book.ok(
        "plan.version.submit",
        "planner:amal",
        json!({"plan_version_id": "PV-1"}),
    )?;

    let blocked_edit = json!({
        "plan_version_id": "PV-1",
        "effective_from": "2026-09-01T00:00:00Z", "effective_to": "2026-12-01T00:00:00Z"
    });
    let (post_submit_code, _) = book.refused(
        "plan.version.update_draft",
        "planner:amal",
        blocked_edit.clone(),
    )?;
    assert_eq!(post_submit_code, "action.write_mode_failed");

    let (sod_code, _) = book.refused(
        "plan.version.approve",
        "planner:amal",
        json!({"plan_version_id": "PV-1"}),
    )?;
    assert_eq!(sod_code, "action.permission_denied");
    book.ok(
        "plan.version.approve",
        "director:salma",
        json!({"plan_version_id": "PV-1"}),
    )?;

    let (post_approve_code, _) = book.refused(
        "plan.version.update_draft",
        "planner:amal",
        blocked_edit.clone(),
    )?;
    assert_eq!(post_approve_code, "action.write_mode_failed");

    book.ok(
        "plan.version.lock",
        "director:salma",
        json!({"plan_version_id": "PV-1"}),
    )?;
    let (post_lock_code, _) =
        book.refused("plan.version.update_draft", "planner:amal", blocked_edit)?;
    assert_eq!(post_lock_code, "action.write_mode_failed");
    Ok(())
}

#[test]
fn plan_version_supersession_preserves_bitemporal_knowledge_cutoffs() -> Result<(), String> {
    let mut book = PlanBook::open("org_plan_sup", "prod", "plan-supersede.sqlite")?;
    seed_retail_base(&mut book, "LE-SOLE", "EGP", &["ST-CFC"])?;
    book.ok(
        "plan.version.create",
        "planner:amal",
        json!({
            "plan_version_id": "PV-1", "entity_id": "LE-SOLE",
            "plan_code": "FY2026-AUTUMN", "revision_number": 1, "currency": "EGP",
            "effective_from": "2026-09-01T00:00:00Z", "effective_to": "2026-11-01T00:00:00Z"
        }),
    )?;
    record_dry_goods_plan_line(
        &mut book,
        "PL-V1",
        "PV-1",
        "15000.000000",
        "9000.000000",
        "150.000000",
    )?;
    lock_plan_version(&mut book, "PV-1")?;

    let cutoff_v1 = book.last_cutoff()?;
    let sales_at_v1 = book.answer_known_at("plan.planned_sales_retail_monthly", cutoff_v1)?;
    let v1_bucket = [
        "PV-1",
        "LE-SOLE",
        "ST-CFC",
        "dry_goods",
        "nile_foods",
        "EGP",
        "2026-09-01",
    ];
    let v2_bucket = [
        "PV-2",
        "LE-SOLE",
        "ST-CFC",
        "dry_goods",
        "nile_foods",
        "EGP",
        "2026-09-01",
    ];
    assert_eq!(
        bucket_decimal(&sales_at_v1, &v1_bucket).as_deref(),
        Some("15000")
    );
    assert_eq!(bucket_decimal(&sales_at_v1, &v2_bucket), None);

    book.ok(
        "plan.version.create_revision",
        "planner:amal",
        json!({
            "plan_version_id": "PV-2", "entity_id": "LE-SOLE",
            "plan_code": "FY2026-AUTUMN", "revision_number": 2, "currency": "EGP",
            "effective_from": "2026-09-01T00:00:00Z", "effective_to": "2026-11-01T00:00:00Z",
            "supersedes_version_id": "PV-1"
        }),
    )?;
    record_dry_goods_plan_line(
        &mut book,
        "PL-V2",
        "PV-2",
        "21000.000000",
        "12600.000000",
        "210.000000",
    )?;
    lock_plan_version(&mut book, "PV-2")?;
    book.ok(
        "plan.version.supersede",
        "director:salma",
        json!({"plan_version_id": "PV-1", "successor_version_id": "PV-2"}),
    )?;
    assert_eq!(
        book.fact_field("plan.PlanVersion", "PV-1", "status")?
            .as_deref(),
        Some("superseded")
    );
    assert_eq!(
        book.answer_known_at("plan.planned_sales_retail_monthly", cutoff_v1)?,
        sales_at_v1
    );
    let cutoff_v2 = book.last_cutoff()?;
    let sales_at_v2 = book.answer_known_at("plan.planned_sales_retail_monthly", cutoff_v2)?;
    assert_eq!(
        bucket_decimal(&sales_at_v2, &v2_bucket).as_deref(),
        Some("21000")
    );
    Ok(())
}

struct GoldSpreadLine<'a> {
    plan_line_id: &'a str,
    store_id: &'a str,
    sales_retail: &'a str,
    sales_cost: &'a str,
    sales_qty: &'a str,
    receipts_retail: &'a str,
    receipts_qty: &'a str,
    markdowns_cost: &'a str,
    markdowns_retail: &'a str,
    fine_metal_g: &'a str,
    bucket_index: i64,
    remainder_quanta: i64,
    eom_cost: &'a str,
    eom_retail: &'a str,
    eom_qty: &'a str,
}

fn gold_spread_payload(line: &GoldSpreadLine<'_>) -> Value {
    json!({
        "plan_line_id": line.plan_line_id, "plan_version_id": "PV-SP",
        "spread_target_id": "SP-EXACT", "entity_id": "LE-SOLE", "store_id": line.store_id,
        "category": "gold_jewelry", "brand": "nile_gold", "plan_month": "2026-09",
        "period_start": "2026-09-01T00:00:00Z", "as_of": "2026-09-30T23:59:59Z",
        "currency": "EGP", "unit": "g",
        "planned_sales_retail": line.sales_retail, "planned_sales_cost": line.sales_cost,
        "planned_sales_qty": line.sales_qty, "planned_receipts_retail": line.receipts_retail,
        "planned_receipts_qty": line.receipts_qty, "planned_markdowns_cost": line.markdowns_cost,
        "planned_markdowns_retail": line.markdowns_retail, "planned_fine_metal_g": line.fine_metal_g,
        "spread_bucket_index": line.bucket_index, "spread_base_quanta": 500_000,
        "spread_remainder_quanta": line.remainder_quanta,
        "planned_eom_stock_cost": line.eom_cost, "planned_eom_stock_retail": line.eom_retail,
        "planned_eom_stock_qty": line.eom_qty
    })
}

fn seed_spread_version_and_target(book: &mut PlanBook) -> Result<(), String> {
    seed_retail_base(book, "LE-SOLE", "EGP", &["ST-CFC", "ST-MOE"])?;
    book.ok(
        "plan.version.create",
        "planner:amal",
        json!({
            "plan_version_id": "PV-SP", "entity_id": "LE-SOLE",
            "plan_code": "FY2026-GOLD", "revision_number": 1, "currency": "EGP",
            "effective_from": "2026-09-01T00:00:00Z", "effective_to": "2026-10-01T00:00:00Z"
        }),
    )?;
    book.ok(
        "plan.spread_target.create",
        "planner:amal",
        json!({
            "spread_target_id": "SP-EXACT", "plan_version_id": "PV-SP",
            "entity_id": "LE-SOLE", "category": "gold_jewelry", "brand": "nile_gold",
            "plan_month": "2026-09", "currency": "EGP",
            "rounding_rule": "largest_remainder", "bucket_count": 2,
            "allocation_quantum_cost": "0.010000", "target_remainder_quanta": 1,
            "target_sales_retail": "100000.000000", "target_sales_cost": "75000.000000",
            "target_receipts_cost": "10000.010000", "target_receipts_retail": "13500.000000",
            "target_markdowns_cost": "200.000000", "target_markdowns_retail": "300.000000",
            "target_eom_stock_cost": "30000.000000", "target_fine_metal_g": "150.750000"
        }),
    )
}

#[test]
fn plan_line_misaligned_eom_refuses_by_law() -> Result<(), String> {
    let mut book = PlanBook::open("org_plan_eom", "prod", "plan-eom.sqlite")?;
    seed_spread_version_and_target(&mut book)?;
    let (bad_eom_code, _) = book.refused(
        "plan.line.record",
        "planner:amal",
        json!({
            "plan_line_id": "PL-BAD-EOM", "plan_version_id": "PV-SP",
            "entity_id": "LE-SOLE", "store_id": "ST-CFC",
            "category": "dry_goods", "brand": "nile_foods", "plan_month": "2026-09",
            "period_start": "2026-09-01T00:00:00Z", "as_of": "2026-09-15T23:59:59Z",
            "currency": "EGP", "unit": "each",
            "planned_sales_retail": "1000.000000", "planned_sales_cost": "600.000000",
            "planned_sales_qty": "10.000000", "planned_receipts_cost": "600.000000",
            "planned_receipts_retail": "1000.000000", "planned_receipts_qty": "10.000000",
            "planned_markdowns_cost": "0.000000", "planned_markdowns_retail": "0.000000",
            "planned_fine_metal_g": "0.000000",
            "planned_eom_stock_cost": "200.000000", "planned_eom_stock_retail": "350.000000",
            "planned_eom_stock_qty": "3.000000"
        }),
    )?;
    assert_eq!(bad_eom_code, "core.laws.violated");
    Ok(())
}

#[test]
fn top_down_spread_conservation_deterministic_rounding_and_seal_before_approve()
-> Result<(), String> {
    let mut book = PlanBook::open("org_plan_sp", "prod", "plan-spread.sqlite")?;
    seed_spread_version_and_target(&mut book)?;

    let cfc_base = GoldSpreadLine {
        plan_line_id: "PL-BAD-REM",
        store_id: "ST-CFC",
        sales_retail: "60000.000000",
        sales_cost: "45000.000000",
        sales_qty: "90.000000",
        receipts_retail: "8100.000000",
        receipts_qty: "90.500000",
        markdowns_cost: "120.000000",
        markdowns_retail: "180.000000",
        fine_metal_g: "90.500000",
        bucket_index: 1,
        remainder_quanta: 2,
        eom_cost: "18000.000000",
        eom_retail: "24000.000000",
        eom_qty: "50.000000",
    };
    let (bad_rem_code, _) = book.refused(
        "plan.line.record_spread",
        "planner:amal",
        gold_spread_payload(&cfc_base),
    )?;
    assert_eq!(bad_rem_code, "core.laws.violated");

    book.ok(
        "plan.line.record_spread",
        "planner:amal",
        gold_spread_payload(&GoldSpreadLine {
            plan_line_id: "PL-GOLD-CFC",
            remainder_quanta: 1,
            ..cfc_base
        }),
    )?;
    let (drift_code, _) = book.refused(
        "plan.spread_target.seal",
        "planner:amal",
        json!({"spread_target_id": "SP-EXACT"}),
    )?;
    assert_eq!(drift_code, "action.precondition_failed");

    book.ok(
        "plan.line.record_spread",
        "planner:amal",
        gold_spread_payload(&GoldSpreadLine {
            plan_line_id: "PL-GOLD-MOE",
            store_id: "ST-MOE",
            sales_retail: "40000.000000",
            sales_cost: "30000.000000",
            sales_qty: "60.000000",
            receipts_retail: "5400.000000",
            receipts_qty: "60.250000",
            markdowns_cost: "80.000000",
            markdowns_retail: "120.000000",
            fine_metal_g: "60.250000",
            bucket_index: 2,
            remainder_quanta: 0,
            eom_cost: "12000.000000",
            eom_retail: "16000.000000",
            eom_qty: "30.000000",
        }),
    )?;
    book.ok(
        "plan.version.submit",
        "planner:amal",
        json!({"plan_version_id": "PV-SP"}),
    )?;
    let (unsealed_code, _) = book.refused(
        "plan.version.approve",
        "director:salma",
        json!({"plan_version_id": "PV-SP"}),
    )?;
    assert_eq!(unsealed_code, "action.precondition_failed");

    book.ok(
        "plan.spread_target.seal",
        "planner:amal",
        json!({"spread_target_id": "SP-EXACT"}),
    )?;
    book.ok(
        "plan.version.approve",
        "director:salma",
        json!({"plan_version_id": "PV-SP"}),
    )?;
    book.ok(
        "plan.version.lock",
        "director:salma",
        json!({"plan_version_id": "PV-SP"}),
    )
}

#[test]
fn otb_position_includes_zero_po_active_envelope_and_excludes_closed() -> Result<(), String> {
    let mut book = PlanBook::open("org_plan_pos", "prod", "plan-position.sqlite")?;
    seed_locked_plan_and_envelope(&mut book)?;

    // Freshly approved active envelope with ZERO POs MUST appear with full remaining OTB.
    let pos = book.transform("plan.otb_position")?;
    assert_eq!(pos.rows.len(), 1);
    let row = pos.rows.first().ok_or("missing zero-PO envelope row")?;
    assert_eq!(
        row_str(&row.values, "envelope_id").as_deref(),
        Some("ENV-DRY-SEP")
    );
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
        Some("0")
    );
    assert_eq!(
        row_str(&row.values, "remaining_otb_cost").as_deref(),
        Some("10000")
    );
    assert_eq!(row_str(&row.values, "po_line_count").as_deref(), Some("0"));

    // Draft / pending_override envelope MUST NOT appear.
    book.ok(
        "plan.otb_envelope.request_unplanned",
        "planner:amal",
        json!({
            "envelope_id": "ENV-DRAFT", "plan_version_id": "PV-2026-V1",
            "entity_id": "LE-SOLE", "store_id": "ST-CFC", "category": "spares",
            "brand": "unplanned", "plan_month": "2026-09",
            "period_start": "2026-09-01T00:00:00Z", "currency": "EGP",
            "approved_budget_cost": "1500.000000", "override_reason": "Pending review"
        }),
    )?;
    assert_eq!(book.transform("plan.otb_position")?.rows.len(), 1);

    // Closing ENV-DRY-SEP removes it from active `otb_position`.
    book.ok(
        "plan.otb_envelope.close",
        "director:salma",
        json!({"envelope_id": "ENV-DRY-SEP"}),
    )?;
    assert!(book.transform("plan.otb_position")?.rows.is_empty());
    Ok(())
}

fn assert_otb_position(
    book: &PlanBook,
    recognized: &str,
    unreceived: &str,
    cancelled: &str,
    remaining: &str,
) -> Result<(), String> {
    let pos = book.transform("plan.otb_position")?;
    let row = pos.rows.first().ok_or("missing otb_position row")?;
    assert_eq!(
        row_str(&row.values, "recognized_receipts_cost").as_deref(),
        Some(recognized)
    );
    assert_eq!(
        row_str(&row.values, "unreceived_authorized_po_cost").as_deref(),
        Some(unreceived)
    );
    assert_eq!(
        row_str(&row.values, "cancelled_released_cost").as_deref(),
        Some(cancelled)
    );
    assert_eq!(
        row_str(&row.values, "remaining_otb_cost").as_deref(),
        Some(remaining)
    );
    Ok(())
}

fn authorize_po_and_post_two_receipts(book: &mut PlanBook) -> Result<(), String> {
    create_draft_po_with_destination(book, "PO-1", "PO-1/1", "SKU-DRY-1", "100.000000", "60.0000")?;
    book.ok(
        "plan.otb.authorize_po_line",
        "ops:karim",
        otb_auth_input("OTB-1", "ENV-DRY-SEP", "PO-1", "PO-1/1"),
    )?;
    let (open_rel_code, _) = book.refused(
        "plan.otb.release_cancelled_po",
        "ops:karim",
        json!({"binding_id": "OTB-1", "po_id": "PO-1", "line_id": "PO-1/1", "as_of": "2026-09-07T10:00:00Z"}),
    )?;
    assert_eq!(open_rel_code, "action.precondition_failed");

    for (rc_id, delta, cum, ts, first) in [
        (
            "RC-1",
            "25.000000",
            "25.000000",
            "2026-09-08T10:00:00Z",
            true,
        ),
        (
            "RC-2",
            "15.000000",
            "40.000000",
            "2026-09-09T10:00:00Z",
            false,
        ),
    ] {
        post_partial_receipt(
            book,
            &PartialReceipt {
                po_id: "PO-1",
                line_id: "PO-1/1",
                binding_id: "OTB-1",
                receipt_id: rc_id,
                delta_qty: delta,
                cumulative_qty: cum,
                received_at: ts,
                first,
            },
        )?;
    }
    Ok(())
}

#[test]
fn otb_repeated_partial_receipts_strict_cancellation_release_and_cash_timing() -> Result<(), String>
{
    let mut book = PlanBook::open("org_plan_otb", "prod", "plan-otb.sqlite")?;
    seed_locked_plan_and_envelope(&mut book)?;
    authorize_po_and_post_two_receipts(&mut book)?;
    assert_otb_position(&book, "2400", "3600", "0", "4000")?;

    let rel_input = |as_of: &str| json!({"binding_id": "OTB-1", "po_id": "PO-1", "line_id": "PO-1/1", "as_of": as_of});
    let (partial_rel_code, _) = book.refused(
        "plan.otb.release_cancelled_po",
        "ops:karim",
        rel_input("2026-09-10T10:00:00Z"),
    )?;
    assert_eq!(partial_rel_code, "action.precondition_failed");

    book.ok("supply.po.cancel", "ops:karim", json!({"po_id": "PO-1"}))?;
    let (late_rc_code, _) = book.refused(
        "supply.receipt.record",
        "clerk:hana",
        json!({
            "receipt_id": "RC-AFTER-CANCEL", "supplier_id": "SUP-NILE", "warehouse_id": "WH-MAIN",
            "line_id": "PO-1/1", "received_qty": "10.000000", "damaged_qty": "0.000000",
            "unit": "each", "received_at": "2026-09-11T10:00:00Z"
        }),
    )?;
    assert_eq!(late_rc_code, "action.precondition_failed");

    book.ok(
        "plan.otb.release_cancelled_po",
        "ops:karim",
        rel_input("2026-09-12T10:00:00Z"),
    )?;
    assert_otb_position(&book, "2400", "0", "3600", "7600")?;

    create_draft_po_with_destination(
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
        otb_auth_input("OTB-2", "ENV-DRY-SEP", "PO-2", "PO-2/1"),
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
fn otb_atomic_fail_closed_guards_block_bypass_missing_envelope_and_wrong_category()
-> Result<(), String> {
    let mut book = PlanBook::open("org_plan_guard", "prod", "plan-guards.sqlite")?;
    seed_locked_plan_and_envelope(&mut book)?;
    create_draft_po_with_destination(
        &mut book,
        "PO-SP",
        "PO-SP/1",
        "SKU-SPARE-1",
        "20.000000",
        "100.0000",
    )?;

    // 1. Direct `supply.po.submit` bypass MUST refuse by pack law; PO stays `draft`.
    let (bypass_submit_code, _) =
        book.refused("supply.po.submit", "ops:karim", json!({"po_id": "PO-SP"}))?;
    assert_eq!(bypass_submit_code, "core.laws.violated");
    assert_eq!(
        book.fact_field("supply.PurchaseOrder", "PO-SP", "status")?
            .as_deref(),
        Some("draft")
    );

    // 2. Missing envelope refuses; PO stays `draft` and no binding is created.
    let (missing_code, _) = book.refused(
        "plan.otb.authorize_po_line",
        "ops:karim",
        otb_auth_input("OTB-NO-ENV", "ENV-MISSING", "PO-SP", "PO-SP/1"),
    )?;
    assert_eq!(missing_code, "action.slice_object_missing");
    assert_eq!(
        book.fact_field("supply.PurchaseOrder", "PO-SP", "status")?
            .as_deref(),
        Some("draft")
    );
    assert!(!book.fact_state("plan.OtbPoBinding", "OTB-NO-ENV")?.exists());

    // 3. Charging a `spares` PO against the `dry_goods` envelope refuses; PO stays `draft`.
    let (wrong_cat_code, _) = book.refused(
        "plan.otb.authorize_po_line",
        "ops:karim",
        otb_auth_input("OTB-WRONG-CAT", "ENV-DRY-SEP", "PO-SP", "PO-SP/1"),
    )?;
    assert_eq!(wrong_cat_code, "action.precondition_failed");
    assert_eq!(
        book.fact_field("supply.PurchaseOrder", "PO-SP", "status")?
            .as_deref(),
        Some("draft")
    );
    assert!(
        !book
            .fact_state("plan.OtbPoBinding", "OTB-WRONG-CAT")?
            .exists()
    );
    Ok(())
}

#[test]
fn unplanned_otb_override_requires_independent_approval_before_po_submit() -> Result<(), String> {
    let mut book = PlanBook::open("org_plan_ovr", "prod", "plan-override.sqlite")?;
    seed_locked_plan_and_envelope(&mut book)?;
    create_draft_po_with_destination(
        &mut book,
        "PO-SP",
        "PO-SP/1",
        "SKU-SPARE-1",
        "20.000000",
        "100.0000",
    )?;
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
    let (activate_bypass, _) = book.refused(
        "plan.otb_envelope.activate_planned",
        "planner:amal",
        json!({"envelope_id": "ENV-UNPLANNED-SPARES"}),
    )?;
    assert_eq!(activate_bypass, "action.precondition_failed");
    book.ok(
        "plan.otb_envelope.submit_override",
        "planner:amal",
        json!({"envelope_id": "ENV-UNPLANNED-SPARES"}),
    )?;
    let (pending_code, _) = book.refused(
        "plan.otb.authorize_po_line",
        "ops:karim",
        otb_auth_input("OTB-SP-PENDING", "ENV-UNPLANNED-SPARES", "PO-SP", "PO-SP/1"),
    )?;
    assert_eq!(pending_code, "action.precondition_failed");
    assert_eq!(
        book.fact_field("supply.PurchaseOrder", "PO-SP", "status")?
            .as_deref(),
        Some("draft")
    );

    let (self_approve, _) = book.refused(
        "plan.otb_envelope.approve_override",
        "planner:amal",
        json!({"envelope_id": "ENV-UNPLANNED-SPARES"}),
    )?;
    assert_eq!(self_approve, "action.permission_denied");
    book.ok(
        "plan.otb_envelope.approve_override",
        "director:salma",
        json!({"envelope_id": "ENV-UNPLANNED-SPARES"}),
    )?;
    book.ok(
        "plan.otb.authorize_po_line",
        "ops:karim",
        otb_auth_input("OTB-SP-OK", "ENV-UNPLANNED-SPARES", "PO-SP", "PO-SP/1"),
    )?;
    assert_eq!(
        book.fact_field("supply.PurchaseOrder", "PO-SP", "status")?
            .as_deref(),
        Some("submitted")
    );
    Ok(())
}

#[test]
fn otb_cap_breach_leaves_draft_po_unsubmitted_with_no_binding() -> Result<(), String> {
    let mut book = PlanBook::open("org_plan_cap", "prod", "plan-cap.sqlite")?;
    seed_locked_plan_and_envelope(&mut book)?;
    create_draft_po_with_destination(
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
        otb_auth_input("OTB-NV1", "ENV-DRY-SEP", "PO-NV1", "PO-NV1/1"),
    )?;
    assert_eq!(
        book.fact_field("supply.PurchaseOrder", "PO-NV1", "status")?
            .as_deref(),
        Some("submitted")
    );

    // Second draft PO (+5,000 -> 11,000 > 10,000) refuses, stays `draft`, creates NO binding!
    create_draft_po_with_destination(
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
        otb_auth_input("OTB-NV2", "ENV-DRY-SEP", "PO-NV2", "PO-NV2/1"),
    )?;
    assert_eq!(breach_code, "action.precondition_failed");
    assert_eq!(
        book.fact_field("supply.PurchaseOrder", "PO-NV2", "status")?
            .as_deref(),
        Some("draft")
    );
    assert!(!book.fact_state("plan.OtbPoBinding", "OTB-NV2")?.exists());
    Ok(())
}

#[test]
fn concurrent_otb_cap_consumption_refuses_phantom_breach() -> Result<(), String> {
    let mut book = PlanBook::open("org_plan_race", "prod", "plan-race.sqlite")?;
    seed_locked_plan_and_envelope(&mut book)?;
    for (po_id, line_id) in [("PO-A", "PO-A/1"), ("PO-B", "PO-B/1")] {
        create_draft_po_with_destination(
            &mut book,
            po_id,
            line_id,
            "SKU-DRY-1",
            "100.000000",
            "60.0000",
        )?;
    }
    let admitted = book
        .pack
        .definitions
        .action("plan.otb.authorize_po_line")
        .ok_or("missing authorize_po_line")?;
    let catalog = book.pack.definitions.catalog();
    let scope = BTreeMap::from([
        ("orgId".into(), book.org_id.clone()),
        ("environmentId".into(), book.env_id.clone()),
    ]);
    let now_a = LogicalTime::from_millis(CLOCK_MS.saturating_add(500_000));
    let now_b = LogicalTime::from_millis(CLOCK_MS.saturating_add(501_000));
    let world = book.cell.store().world();
    book.cell.advance_to(now_b);

    let make_call = |id: &str, now: LogicalTime, input: Value| -> Result<ActionCall<'_>, String> {
        let input_obj: JsonObject =
            serde_json::from_value(input).map_err(|err| format!("{id}: {err}"))?;
        Ok(ActionCall {
            action: &admitted.root,
            action_artifact_id: &admitted.artifact_id,
            action_version: &admitted.version,
            scope: scope.clone(),
            actor: "ops:karim".to_owned(),
            approved_intent: None,
            input: input_obj,
            idempotency_key: id.to_owned(),
            object_type: None,
            turn: DecisionTurn {
                id: id.to_owned(),
                now,
                world: world.clone(),
                consumes: None,
            },
            trace: None,
        })
    };
    let call_a = make_call(
        "race-po-a",
        now_a,
        otb_auth_input("OTB-A", "ENV-DRY-SEP", "PO-A", "PO-A/1"),
    )?;
    let call_b = make_call(
        "race-po-b",
        now_b,
        otb_auth_input("OTB-B", "ENV-DRY-SEP", "PO-B", "PO-B/1"),
    )?;

    let mut race = RaceHost {
        cell: &mut book.cell,
        injected: Some(|cell: &mut PlanCell| {
            commit_action(cell, &catalog, &call_a, &Allow)
                .map_err(|err| format!("call A commit failed: {err}"))?;
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
    assert_eq!(
        book.fact_field("supply.PurchaseOrder", "PO-A", "status")?
            .as_deref(),
        Some("submitted")
    );
    assert_eq!(
        book.fact_field("supply.PurchaseOrder", "PO-B", "status")?
            .as_deref(),
        Some("draft")
    );
    assert!(!book.fact_state("plan.OtbPoBinding", "OTB-B")?.exists());
    assert_otb_position(&book, "0", "6000", "0", "4000")
}

fn assert_actual_variance(
    book: &PlanBook,
    retail_var: &str,
    cost_var: &str,
    qty_var: &str,
) -> Result<(), String> {
    let var = book.transform("plan.actual_vs_plan_variance")?;
    let row = var.rows.first().ok_or("missing actual_vs_plan_variance")?;
    assert_eq!(
        row_str(&row.values, "plan_version_id").as_deref(),
        Some("PV-2026-V1")
    );
    assert_eq!(
        row_str(&row.values, "sp_brand").as_deref(),
        Some("nile_foods")
    );
    assert_eq!(
        row_str(&row.values, "sales_retail_variance").as_deref(),
        Some(retail_var)
    );
    assert_eq!(
        row_str(&row.values, "sales_cost_variance").as_deref(),
        Some(cost_var)
    );
    assert_eq!(
        row_str(&row.values, "sales_qty_variance").as_deref(),
        Some(qty_var)
    );
    Ok(())
}

#[test]
fn bitemporal_actual_vs_plan_variance_and_late_restatement_of_existing_sale() -> Result<(), String>
{
    let mut book = PlanBook::open("org_plan_act", "prod", "plan-actual.sqlite")?;
    seed_locked_plan_and_envelope(&mut book)?;
    book.ok(
        "supply.pos_sale.record",
        "clerk:hana",
        json!({
            "sale_id": "SALE-SEP-1", "store_id": "ST-CFC", "sku_id": "SKU-DRY-1",
            "sold_qty": "120.000000", "unit": "each", "net_revenue": "12000.000000",
            "applied_unit_cost": "60.0000", "cost_status": "costed", "costed_ref": "COST-DRY-1",
            "currency": "EGP", "sold_at": "2026-09-10T14:00:00Z"
        }),
    )?;
    let cutoff_k1 = book.last_cutoff()?;
    let rev_k1 = book.answer_known_at("plan.actual_pos_revenue_monthly", cutoff_k1)?;
    let bucket = ["LE-SOLE", "ST-CFC", "dry_goods", "EGP", "2026-09-01"];
    assert_eq!(bucket_decimal(&rev_k1, &bucket).as_deref(), Some("12000"));
    assert_actual_variance(&book, "-6000", "-3600", "-60")?;

    // Genuine late restatement updating the EXISTING contribution `SALE-SEP-1` in-place.
    book.ok(
        "plan.pos_sale.restate",
        "ops:karim",
        json!({
            "sale_id": "SALE-SEP-1", "sold_qty": "155.000000",
            "net_revenue": "15500.000000", "sold_at": "2026-09-10T14:00:00Z"
        }),
    )?;
    let cutoff_k2 = book.last_cutoff()?;
    assert_eq!(
        book.answer_known_at("plan.actual_pos_revenue_monthly", cutoff_k1)?,
        rev_k1
    );
    let rev_k2 = book.answer_known_at("plan.actual_pos_revenue_monthly", cutoff_k2)?;
    assert_eq!(bucket_decimal(&rev_k2, &bucket).as_deref(), Some("15500"));
    assert_actual_variance(&book, "-2500", "-1500", "-25")
}

fn seed_published_forecast(
    book: &mut PlanBook,
    forecast_id: &str,
    revision: &str,
    published_at: &str,
    sales_retail: &str,
    sales_cost: &str,
    sales_qty: &str,
) -> Result<(), String> {
    book.ok(
        "plan.forecast.record",
        "eng:dina",
        json!({
            "forecast_id": forecast_id, "plan_version_id": "PV-2026-V1",
            "plan_line_id": "PL-DRY-SEP", "model_version": "supply.demand_forecast@1",
            "forecast_revision": revision, "entity_id": "LE-SOLE", "store_id": "ST-CFC",
            "category": "dry_goods", "brand": "nile_foods", "plan_month": "2026-09",
            "period_start": "2026-09-01T00:00:00Z", "published_at": published_at,
            "currency": "EGP", "unit": "each",
            "forecast_sales_retail": sales_retail, "forecast_sales_cost": sales_cost,
            "forecast_sales_qty": sales_qty, "forecast_fine_metal_g": "0.000000"
        }),
    )?;
    book.ok(
        "plan.forecast.publish",
        "eng:dina",
        json!({"forecast_id": forecast_id}),
    )
}

#[test]
fn forecast_vs_plan_variance_preserves_pinned_revision_after_supersession() -> Result<(), String> {
    let mut book = PlanBook::open("org_plan_fc", "prod", "plan-forecast.sqlite")?;
    seed_locked_plan_and_envelope(&mut book)?;
    seed_production_model_version(&mut book, "supply.demand_forecast@1")?;
    seed_published_forecast(
        &mut book,
        "PF-DRY-R1",
        "rev-2026-09-w2",
        "2026-09-12T08:00:00Z",
        "19250.500000",
        "11400.000000",
        "192.000000",
    )?;
    let cutoff_r1 = book.last_cutoff()?;
    let fc_q_r1 = book.answer_known_at("plan.published_forecast_sales_monthly", cutoff_r1)?;

    seed_published_forecast(
        &mut book,
        "PF-DRY-R2",
        "rev-2026-09-w3",
        "2026-09-19T08:00:00Z",
        "20100.000000",
        "12000.000000",
        "201.000000",
    )?;
    book.ok(
        "plan.forecast.supersede",
        "eng:dina",
        json!({"forecast_id": "PF-DRY-R1"}),
    )?;

    let fc_var = book.transform("plan.forecast_vs_plan_variance")?;
    assert_eq!(fc_var.rows.len(), 2);
    let rev1 = fc_var
        .rows
        .iter()
        .find(|row| row_str(&row.values, "forecast_revision").as_deref() == Some("rev-2026-09-w2"))
        .ok_or("missing superseded forecast revision R1")?;
    assert_eq!(
        row_str(&rev1.values, "status").as_deref(),
        Some("superseded")
    );
    assert_eq!(
        row_str(&rev1.values, "sales_retail_variance").as_deref(),
        Some("1250.5")
    );
    let rev2 = fc_var
        .rows
        .iter()
        .find(|row| row_str(&row.values, "forecast_revision").as_deref() == Some("rev-2026-09-w3"))
        .ok_or("missing published forecast revision R2")?;
    assert_eq!(
        row_str(&rev2.values, "status").as_deref(),
        Some("published")
    );
    assert_eq!(
        row_str(&rev2.values, "sales_retail_variance").as_deref(),
        Some("2100")
    );
    assert_eq!(
        book.answer_known_at("plan.published_forecast_sales_monthly", cutoff_r1)?,
        fc_q_r1
    );
    let fc_live = book.answer("plan.published_forecast_sales_monthly")?;
    assert_eq!(
        bucket_decimal(
            &fc_live,
            &[
                "PV-2026-V1",
                "supply.demand_forecast@1",
                "rev-2026-09-w2",
                "LE-SOLE",
                "ST-CFC",
                "dry_goods",
                "nile_foods",
                "EGP",
                "2026-09-01"
            ]
        )
        .as_deref(),
        Some("19250.5")
    );
    Ok(())
}

#[test]
fn separate_entity_currency_and_ucum_fine_metal_partitioning() -> Result<(), String> {
    let mut usd_book = PlanBook::open("org_plan_partner", "prod", "plan-usd.sqlite")?;
    seed_retail_base(&mut usd_book, "LE-PARTNER", "USD", &["ST-LUX"])?;
    usd_book.ok(
        "plan.version.create",
        "planner:amal",
        json!({
            "plan_version_id": "PV-USD-V1", "entity_id": "LE-PARTNER",
            "plan_code": "FY2026-JEWELRY", "revision_number": 1, "currency": "USD",
            "effective_from": "2026-09-01T00:00:00Z", "effective_to": "2026-10-01T00:00:00Z"
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
            "planned_fine_metal_g": "248.750000",
            "planned_eom_stock_cost": "8000.000000", "planned_eom_stock_retail": "10500.000000",
            "planned_eom_stock_qty": "105.000000"
        }),
    )?;
    let fine_metal = usd_book.answer("plan.planned_fine_metal_monthly")?;
    assert_eq!(
        bucket_decimal(
            &fine_metal,
            &[
                "PV-USD-V1",
                "LE-PARTNER",
                "ST-LUX",
                "gold_jewelry",
                "nile_gold",
                "2026-09-01"
            ]
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
