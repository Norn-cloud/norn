//! SUPPLY-LAB: the `supply-lab` registry pack (with `ml` and `tracking`) on a
//! real file-backed `SqliteCell`, across seven data-plane stress axes. Every
//! write goes through `commit_action` (the kernel path, with the pack's laws),
//! every answer through the admitted question plan. Nothing is mocked; where an
//! engine refuses a shape the spec asks for, the test pins the refusal and
//! `supply-lab-gaps` (the Phase 1 report) names it.
//!
//! Axis 8 (generic MBT, coverage observer and static gaps) runs in
//! `norn-testkit/tests/pack_mbt_sqlite.rs`.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use norn_cell::compute::ivm::IncrementalQuestion;
use norn_cell::compute::lineage::LineageDisclosure;
use norn_cell::compute::transform::RowAccess;
use norn_cell::compute::{Engine, QueryContext, QueryResult};
use norn_cell::function_receipt::{FUNCTION_COMMAND_TYPE, FUNCTION_PAYLOAD_TYPE};
use norn_cell::{AdmittedDefinitions, CellError, KnowledgeCutoff, Model, SqliteCell, StoreOptions};
use norn_checker::EntityArtifactSetContext;
use norn_checker::entity_scopes::ScopedView;
use norn_checker::function::{
    admit_function_manifest, check_function_invocation, check_function_receipt,
};
use norn_checker::metric::compute::QuestionPlan;
use norn_checker::metric::derive::derive_metric_space;
use norn_ir::function::{
    Authority, AuthorityKind, BlobRef, CacheOrigin, Digest, ExecutionProvenance, Fencing,
    FreshnessMode, FunctionInvocation, FunctionManifest, FunctionOutcome, FunctionReceipt,
    RelevantStateToken, ResourceUse, RetryCeiling, RunRef, SuccessOutcome, output_manifest_digest,
};
use norn_ir::{
    ArtifactEnvelope, Decimal, DecisionOutput, DiagnosticSeverity, FactChange, LawRoot, LinkChange,
    QuestionIR, RuntimeValue,
};
use norn_kernel::CellHost as _;
use norn_kernel::cell_host::{FactKey, FactMutation, FactVersion, FactWrite, JsonObject};
use norn_kernel::result_reconciliation::{
    CommandState, Completion, DispatchedCommand, InvalidReceipt, ReceiptValidator,
    ResultAuthorizer, ResultAuthzRequest, ResultChecks, ResultDenial,
};
use norn_kernel::{
    ActionAuthorizer, ActionAuthzRequest, ActionCall, ActionDenial, ActionOutcome,
    ActionReadAuthzRequest, ActionReadTypeAuthzRequest, CellCommit, Command, DecisionTurn, Effect,
    FactEvent, IdempotencyKey, LogicalTime, commit_action,
};
use norn_testkit::pack_mbt::AdmittedPack;
use serde_json::{Value, json};
use tempfile::TempDir;

type Cell = SqliteCell<FactEvent, JsonObject, JsonObject>;

const SCOPE: &str = "org_demo/dev";
/// 2026-10-26T00:00:00Z: every commit is recorded after the 16-week window.
const CLOCK: u64 = 1_792_972_800_000;
/// The Monday of each ISO week W1..W16.
const WEEKS: [&str; 16] = [
    "2026-07-06",
    "2026-07-13",
    "2026-07-20",
    "2026-07-27",
    "2026-08-03",
    "2026-08-10",
    "2026-08-17",
    "2026-08-24",
    "2026-08-31",
    "2026-09-07",
    "2026-09-14",
    "2026-09-21",
    "2026-09-28",
    "2026-10-05",
    "2026-10-12",
    "2026-10-19",
];
/// Weekly issues of water at WH-A (each).
const ISSUES: [&str; 16] = [
    "120", "110", "130", "125", "140", "135", "150", "145", "160", "155", "170", "165", "180",
    "175", "190", "185",
];
/// A port strike: 30% more of each week's issues, exactly.
const STRIKE: [&str; 16] = [
    "36", "33", "39", "37.5", "42", "40.5", "45", "43.5", "48", "46.5", "51", "49.5", "54", "52.5",
    "57", "55.5",
];
/// WH-A water counts: W3 has two (latest wins), W5 and W6 are quiet (carried).
const WATER_A: [(&str, &str); 15] = [
    ("2026-07-06T10:00:00Z", "1000"),
    ("2026-07-14T10:00:00Z", "950"),
    ("2026-07-21T10:00:00Z", "930"),
    ("2026-07-23T15:00:00Z", "900"),
    ("2026-07-28T10:00:00Z", "870"),
    ("2026-08-18T10:00:00Z", "800"),
    ("2026-08-25T10:00:00Z", "780"),
    ("2026-09-01T10:00:00Z", "760"),
    ("2026-09-08T10:00:00Z", "740"),
    ("2026-09-15T10:00:00Z", "720"),
    ("2026-09-22T10:00:00Z", "700"),
    ("2026-09-29T10:00:00Z", "680"),
    ("2026-10-06T10:00:00Z", "660"),
    ("2026-10-13T10:00:00Z", "640"),
    ("2026-10-20T10:00:00Z", "620"),
];
/// WH-B water counts before the audit: absent in W1; W10 miscounted at K1.
const WATER_B: [(&str, &str); 4] = [
    ("2026-07-14T09:00:00Z", "300"),
    ("2026-07-28T09:00:00Z", "350"),
    ("2026-09-08T09:00:00Z", "500"),
    ("2026-09-22T09:00:00Z", "450"),
];

fn repo() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn envelopes(dir: &Path) -> Result<Vec<ArtifactEnvelope>, String> {
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

/// The admitted pack: candidates (tracking, ml, supply-lab), the base value
/// types they resolve against, and their one projection for a cell.
struct Lab {
    candidate: Vec<ArtifactEnvelope>,
    /// The candidates with the base they resolve against (`core.quantity`):
    /// what questions and transforms are admitted over.
    pool: Vec<ArtifactEnvelope>,
    pack: AdmittedPack,
    /// INSTALL-SEAM-1: the model (with the kernel's record types), the actions
    /// and their transition guards, built together from the same artifacts.
    definitions: AdmittedDefinitions,
}

fn lab() -> Result<Lab, String> {
    let mut candidate = Vec::new();
    for dir in ["tracking", "ml", "supply-lab"] {
        candidate.extend(envelopes(
            &repo().join("registry/packs").join(dir).join("artifacts"),
        )?);
    }
    let mut all = base()?;
    all.extend(candidate.iter().cloned());
    let pack = AdmittedPack::from_envelopes("supply-lab", &all)?;
    let definitions =
        AdmittedDefinitions::from_envelopes(&all).map_err(|e| format!("definitions: {e}"))?;
    Ok(Lab {
        candidate,
        pool: all,
        pack,
        definitions,
    })
}

impl Lab {
    fn model(&self) -> &Model {
        self.definitions.model()
    }
}

fn base() -> Result<Vec<ArtifactEnvelope>, String> {
    let core = repo().join("registry/core/artifacts");
    [
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

/// What one governed call did.
#[derive(Debug, PartialEq, Eq)]
enum Outcome {
    Committed,
    Replayed,
    Refused(String, String),
}

const FORECAST_EVALUATOR_EXECUTOR: &str = "pool.ml-evaluator";
const FORECAST_EVALUATOR_AUTHORITY: &str = "svc:ml-evaluator";
const FORECAST_EVALUATOR_MANIFEST_DIGEST: &str =
    "sha256:727d22e10828fff4346a4c231533eda37216d4b74ae5d1264fb4d9b6fcf3fef0";
const FORECAST_EVALUATOR_PROFILE_DIGEST: &str =
    "sha256:a780163924782b48cc7ef188073bbc22d903bdd5a55825b7077c9e9eb9c7656e";

struct EvaluatorReceiptContract;

impl ResultAuthorizer for EvaluatorReceiptContract {
    fn authorize(&self, request: &ResultAuthzRequest<'_>) -> Result<(), ResultDenial> {
        if request.authority.id == FORECAST_EVALUATOR_AUTHORITY {
            Ok(())
        } else {
            Err(ResultDenial {
                reason: "evaluator authority denied".to_owned(),
            })
        }
    }
}

impl ReceiptValidator for EvaluatorReceiptContract {
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

/// One file-backed cell serving the lab, with its own logical clock.
struct Stress {
    lab: Lab,
    _dir: TempDir,
    path: PathBuf,
    receipt_answers: Arc<Mutex<BTreeMap<String, JsonObject>>>,
    cell: Cell,
    tick: u64,
    calls: u64,
    /// Every committed governed call, in order: what `norn seed` replays
    /// (`registry/packs/supply-lab/seed/supply.calls.json`).
    committed: Vec<Value>,
}

impl Stress {
    fn open() -> Result<Self, String> {
        let lab = lab()?;
        let dir = tempfile::tempdir().map_err(|e| format!("tempdir: {e}"))?;
        let path = dir.path().join("supply-lab.sqlite");
        let receipt_answers = Arc::new(Mutex::new(BTreeMap::new()));
        let cell = open_cell(&path, lab.model(), Arc::clone(&receipt_answers))?;
        Ok(Self {
            lab,
            _dir: dir,
            path,
            receipt_answers,
            cell,
            tick: 0,
            calls: 0,
            committed: Vec::new(),
        })
    }

    fn now(&self) -> LogicalTime {
        LogicalTime::from_millis(CLOCK.saturating_add(self.tick))
    }

    /// Call `action` as `actor` with an idempotency key of its own.
    fn act(&mut self, action: &str, actor: &str, input: Value) -> Result<Outcome, String> {
        self.calls = self.calls.saturating_add(1);
        let key = format!("call-{}", self.calls);
        self.act_keyed(action, actor, input, &key)
    }

    fn act_keyed(
        &mut self,
        action: &str,
        actor: &str,
        input: Value,
        key: &str,
    ) -> Result<Outcome, String> {
        self.tick = self.tick.saturating_add(1_000);
        let now = self.now();
        self.cell.advance_to(now);
        let world = self.cell.store().world();
        let admitted = self
            .lab
            .definitions
            .action(action)
            .ok_or_else(|| format!("no action {action}"))?;
        let sent = input.clone();
        let input: JsonObject =
            serde_json::from_value(input).map_err(|e| format!("{action} input: {e}"))?;
        // One catalog, guards bound by construction: no second model, no forgotten guard.
        let catalog = self.lab.definitions.catalog();
        let call = ActionCall {
            action: &admitted.root,
            action_artifact_id: &admitted.artifact_id,
            action_version: &admitted.version,
            scope: BTreeMap::from([
                ("orgId".into(), "org_demo".into()),
                ("environmentId".into(), "dev".into()),
            ]),
            actor: actor.to_owned(),
            approved_intent: None,
            input,
            idempotency_key: key.to_owned(),
            object_type: None,
            turn: DecisionTurn {
                id: key.to_owned(),
                now,
                world,
                consumes: None,
            },
            trace: None,
        };
        Ok(
            match commit_action(&mut self.cell, &catalog, &call, &Allow) {
                Ok(ActionOutcome::Committed { .. }) => {
                    self.committed.push(json!({"actor": actor, "action": action, "idempotencyKey": key, "input": sent}));
                    Outcome::Committed
                }
                Ok(ActionOutcome::Replayed { .. }) => Outcome::Replayed,
                Err(refusal) => Outcome::Refused(refusal.code().to_owned(), refusal.to_string()),
            },
        )
    }

    /// Commit one kernel decision: the relinks, unlinks and deletes no
    /// governed supply-lab action makes.
    fn decide(
        &mut self,
        step: &str,
        facts: Vec<FactChange>,
        links: Vec<LinkChange>,
    ) -> Result<(), String> {
        self.tick = self.tick.saturating_add(1_000);
        let now = self.now();
        self.cell.advance_to(now);
        let keys: Vec<FactKey> = facts
            .iter()
            .filter_map(|fact| match fact {
                FactChange::Delete { object_type, key } => {
                    Some(FactKey::new(object_type.as_str(), key))
                }
                FactChange::Create { .. }
                | FactChange::Set { .. }
                | FactChange::Transition { .. } => None,
            })
            // A link change reads both its endpoints.
            .chain(links.iter().flat_map(|link| {
                let (source_type, source_key, target_type, target_key) = match link {
                    LinkChange::Add {
                        source_type,
                        source_key,
                        target_type,
                        target_key,
                        ..
                    }
                    | LinkChange::Remove {
                        source_type,
                        source_key,
                        target_type,
                        target_key,
                        ..
                    } => (source_type, source_key, target_type, target_key),
                };
                [
                    FactKey::new(source_type.as_str(), source_key),
                    FactKey::new(target_type.as_str(), target_key),
                ]
            }))
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
        let slice =
            norn_kernel::load_slice(&self.cell, keys).map_err(|e| format!("{step}: {e}"))?;
        let turn = DecisionTurn {
            id: format!("decide-{}", self.tick),
            now,
            world: self.cell.store().world(),
            consumes: None,
        };
        let output = DecisionOutput {
            facts,
            links,
            commands: Vec::new(),
            timers: Vec::new(),
        };
        let catalog = self.lab.definitions.catalog();
        norn_kernel::commit_decision(&mut self.cell, &catalog, &slice, &turn, &output)
            .map(|_| ())
            .map_err(|e| format!("{step}: {e}"))
    }

    /// Commit or fail with the action and its refusal.
    fn ok(&mut self, action: &str, actor: &str, input: Value) -> Result<(), String> {
        match self.act(action, actor, input)? {
            Outcome::Committed => Ok(()),
            other @ (Outcome::Replayed | Outcome::Refused(..)) => {
                Err(format!("{action} by {actor}: {other:?}"))
            }
        }
    }

    /// A refused call writes nothing: the store dump is unchanged.
    fn refused(
        &mut self,
        action: &str,
        actor: &str,
        input: Value,
    ) -> Result<(String, String), String> {
        let before = self.dump()?;
        let outcome = self.act(action, actor, input)?;
        assert_eq!(
            before,
            self.dump()?,
            "{action}: a refusal leaves no partial write"
        );
        match outcome {
            Outcome::Refused(code, message) => Ok((code, message)),
            other @ (Outcome::Committed | Outcome::Replayed) => {
                Err(format!("{action} by {actor} was not refused: {other:?}"))
            }
        }
    }

    fn dump(&self) -> Result<BTreeMap<String, Vec<String>>, String> {
        self.cell
            .store()
            .read()
            .dump()
            .map_err(|e| format!("dump: {e}"))
    }

    fn head(&self) -> Result<norn_cell::LedgerHead, String> {
        self.cell
            .store()
            .verified_head()
            .map_err(|e| format!("head: {e}"))
    }

    /// Close the cell and reopen its file, verifying the ledger.
    fn reopen(&mut self) -> Result<(), String> {
        let now = self.now();
        let replacement = Cell::new(
            norn_cell::CellStore::open_in_memory(self.lab.model().clone())
                .map_err(|e| format!("{e}"))?,
            SCOPE,
            |c| c.payload.clone(),
        )
        .map_err(|e| format!("placeholder: {e}"))?;
        drop(std::mem::replace(&mut self.cell, replacement));
        self.cell = open_cell(
            &self.path,
            self.lab.model(),
            Arc::clone(&self.receipt_answers),
        )?;
        self.cell.advance_to(now);
        self.cell
            .store()
            .read()
            .verify_ledger()
            .map(|_| ())
            .map_err(|e| format!("verify_ledger: {e}"))
    }
}

fn open_cell(
    path: &Path,
    model: &Model,
    receipt_answers: Arc<Mutex<BTreeMap<String, JsonObject>>>,
) -> Result<Cell, String> {
    let mut cell = Cell::open(
        path,
        model.clone(),
        StoreOptions::default(),
        SCOPE,
        move |c| {
            receipt_answers
                .lock()
                .ok()
                .and_then(|map| map.get(c.id.0.as_str()).cloned())
                .unwrap_or_else(|| c.payload.clone())
        },
    )
    .map_err(|e| format!("open cell: {e}"))?;
    cell.set_result_authorizer(Some(Box::new(EvaluatorReceiptContract)));
    Ok(cell)
}

// ------------------------------------------------------------------ dataset

fn count(snapshot: &str, at: &str, qty: &str, source: &str) -> Value {
    let mut parts = snapshot.split('/');
    let (warehouse, sku, basis) = (parts.next(), parts.next(), parts.next());
    let unit = if sku == Some("SKU-RICE") {
        "kg"
    } else {
        "each"
    };
    json!({"snapshot_id": snapshot, "warehouse_id": warehouse, "sku_id": sku, "on_hand_qty": qty,
        "unit": unit, "quantity_basis": basis, "as_of": at, "count_source": source, "count_ref": format!("{snapshot}@{at}")})
}

fn catalog(stress: &mut Stress) -> Result<(), String> {
    for (id, region, lat) in [
        ("WH-A", "delta", "30.044420"),
        ("WH-B", "canal", "30.596500"),
    ] {
        stress.ok(
            "supply.warehouse.register",
            "eng:dina",
            json!({"warehouse_id": id, "name": format!("Warehouse {id}"),
            "region": region, "latitude": lat, "longitude": "31.235700"}),
        )?;
    }
    for (sku, category, unit) in [
        ("SKU-WATER", "dry_goods", "each"),
        ("SKU-RICE", "dry_goods", "kg"),
    ] {
        stress.ok(
            "supply.sku.register",
            "eng:dina",
            json!({"sku_id": sku, "category": category,
            "unit_of_measure": unit, "shelf_life_days": 365}),
        )?;
    }
    Ok(())
}

/// Positions and counts up to knowledge time K1 (WH-B's W10 miscount included).
fn counts(stress: &mut Stress) -> Result<(), String> {
    let positions = [
        ("WH-A/SKU-WATER/on_hand", &WATER_A[..]),
        ("WH-B/SKU-WATER/on_hand", &WATER_B[..]),
        (
            "WH-A/SKU-RICE/on_hand",
            &[
                ("2026-07-06T10:00:00Z", "2000.5"),
                ("2026-08-25T10:00:00Z", "1800.25"),
            ][..],
        ),
        (
            "WH-B/SKU-WATER/available",
            &[("2026-07-14T09:00:00Z", "280")][..],
        ),
    ];
    for (snapshot, history) in positions {
        for (index, (at, qty)) in history.iter().enumerate() {
            let action = if index == 0 {
                "supply.position.open"
            } else {
                "supply.position.record_count"
            };
            stress.ok(action, "clerk:hana", count(snapshot, at, qty, "wms_scan"))?;
        }
    }
    Ok(())
}

fn movements(stress: &mut Stress, prefix: &str, issues: &[&str]) -> Result<(), String> {
    for (week, qty) in WEEKS.iter().zip(issues) {
        stress.ok(
            "supply.movement.record",
            "clerk:hana",
            json!({"movement_id": format!("{prefix}-{week}"),
            "warehouse_id": "WH-A", "sku_id": "SKU-WATER", "qty": qty, "unit": "each",
            "movement_kind": "issue", "occurred_at": format!("{week}T12:00:00Z")}),
        )?;
    }
    Ok(())
}

fn line(id: &str, sku: &str, unit: &str, ordered: &str, received: &str, rate: &str) -> Value {
    json!({"line_id": id, "po_id": "PO-1", "sku_id": sku, "promised_at": "2026-07-15T00:00:00Z",
        "ordered_qty": ordered, "received_qty": received, "fill_rate": rate, "unit": unit,
        "unit_price": "2.5", "currency": "EGP"})
}

/// A line's cumulative receipt: `fill_rate` is derived by the action, never sent.
fn received_line(id: &str, sku: &str, unit: &str, ordered: &str, received: &str) -> Value {
    let mut value = line(id, sku, unit, ordered, received, "0");
    if let Some(fields) = value.as_object_mut() {
        fields.remove("fill_rate");
    }
    value
}

fn receipt(
    id: &str,
    line_id: Option<&str>,
    unit: &str,
    received: &str,
    damaged: &str,
    at: &str,
) -> Value {
    json!({"receipt_id": id, "supplier_id": "SUP-NILE", "warehouse_id": "WH-A", "line_id": line_id,
        "received_qty": received, "damaged_qty": damaged, "unit": unit, "received_at": at})
}

/// PO-1: three lines, split deliveries and inspections, and one blind receipt.
fn purchasing(stress: &mut Stress) -> Result<(), String> {
    stress.ok(
        "supply.po.create",
        "planner:amal",
        json!({"po_id": "PO-1", "supplier_id": "SUP-NILE",
        "promised_at": "2026-07-15T00:00:00Z", "currency": "EGP"}),
    )?;
    for (id, sku, unit, ordered) in [
        ("PO-1/1", "SKU-WATER", "each", "1000"),
        ("PO-1/2", "SKU-WATER", "each", "100"),
        ("PO-1/3", "SKU-RICE", "kg", "500"),
    ] {
        stress.ok(
            "supply.line.add",
            "planner:amal",
            line(id, sku, unit, ordered, "0", "0"),
        )?;
    }
    stress.ok("supply.po.submit", "ops:omar", json!({"po_id": "PO-1"}))?;
    stress.ok(
        "supply.po.receive_partial",
        "clerk:hana",
        json!({"po_id": "PO-1"}),
    )?;
    for (id, line_id, unit, received, damaged, at) in [
        ("RC-1", "PO-1/1", "each", "600", "0", "2026-07-14T08:00:00Z"),
        (
            "RC-2",
            "PO-1/1",
            "each",
            "300",
            "20",
            "2026-07-16T08:00:00Z",
        ),
        ("RC-3", "PO-1/2", "each", "100", "0", "2026-07-15T08:00:00Z"),
        ("RC-4", "PO-1/3", "kg", "250", "0", "2026-07-15T09:00:00Z"),
    ] {
        stress.ok(
            "supply.receipt.record",
            "clerk:hana",
            receipt(id, Some(line_id), unit, received, damaged, at),
        )?;
    }
    stress.ok(
        "supply.receipt.record_blind",
        "clerk:hana",
        receipt("RC-B1", None, "each", "40", "0", "2026-07-15T10:00:00Z"),
    )?;
    for (id, receipt_id, inspected, rejected) in [
        ("IN-1", "RC-1", "50", "2"),
        ("IN-2", "RC-1", "30", "1"),
        ("IN-3", "RC-2", "40", "20"),
    ] {
        stress.ok(
            "supply.inspection.record",
            "clerk:hana",
            json!({"inspection_id": id, "receipt_id": receipt_id,
            "inspected_qty": inspected, "rejected_qty": rejected, "unit": "each"}),
        )?;
    }
    stress.ok(
        "supply.inspection.record_orphan",
        "clerk:hana",
        json!({"inspection_id": "IN-4", "receipt_id": null,
        "inspected_qty": "10", "rejected_qty": "0", "unit": "each"}),
    )?;
    for (id, sku, unit, ordered, received) in [
        ("PO-1/1", "SKU-WATER", "each", "1000", "900"),
        ("PO-1/2", "SKU-WATER", "each", "100", "100"),
        ("PO-1/3", "SKU-RICE", "kg", "500", "250"),
    ] {
        stress.ok(
            "supply.line.record_received",
            "clerk:hana",
            received_line(id, sku, unit, ordered, received),
        )?;
    }
    Ok(())
}

fn seeded() -> Result<Stress, String> {
    let mut stress = Stress::open()?;
    catalog(&mut stress)?;
    counts(&mut stress)?;
    movements(&mut stress, "MV", &ISSUES)?;
    purchasing(&mut stress)?;
    Ok(stress)
}

/// The reference seeding, as the calls a dev cell replays: `norn init
/// --template supply-lab` copies this file into the project and `norn seed`
/// sends each call as its actor, a named local principal holding the role the
/// pack's access policy names (DEV-PRINCIPALS-1). Generated from `seeded()`,
/// never written by hand; `NORN_PACKS_UPDATE=1` regenerates it.
#[test]
fn seed_calls_are_the_reference_seeding() -> Result<(), String> {
    let stress = seeded()?;
    let document = json!({ "format": "norn-calls/1", "calls": stress.committed });
    let path = repo().join("registry/packs/supply-lab/seed/supply.calls.json");
    let text = format!(
        "{}\n",
        serde_json::to_string_pretty(&document).map_err(|e| format!("{e}"))?
    );
    if std::env::var("NORN_PACKS_UPDATE").as_deref() == Ok("1") {
        fs::create_dir_all(path.parent().ok_or("no parent")?).map_err(|e| format!("{e}"))?;
        fs::write(&path, &text).map_err(|e| format!("{}: {e}", path.display()))?;
    }
    let committed = fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?;
    assert_eq!(
        committed, text,
        "the seed is the reference seeding; regenerate with NORN_PACKS_UPDATE=1"
    );
    Ok(())
}

// ---------------------------------------------------------------- questions

fn context(now: u64) -> QueryContext {
    QueryContext {
        world: "main".into(),
        scope: SCOPE.into(),
        population_pin: "supply-lab:all".into(),
        authorization_epoch: 1,
        mapping_pin: "supply-lab:native".into(),
        now_ms: now,
        source_cursors: BTreeMap::new(),
        watermarks: BTreeMap::new(),
    }
}

fn golden(name: &str) -> Result<QuestionIR, String> {
    let path = repo().join("registry/packs/supply-lab/questions/supply.questions.json");
    let text = fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?;
    let questions: Vec<Value> =
        serde_json::from_str(&text).map_err(|e| format!("questions: {e}"))?;
    let question = questions
        .into_iter()
        .find(|q| q.get("name").and_then(Value::as_str) == Some(name))
        .ok_or_else(|| format!("no question {name}"))?;
    serde_json::from_value(question).map_err(|e| format!("{name}: {e}"))
}

/// Admit `question` against the metric space derived from its object set.
fn admit(lab: &Lab, question: &QuestionIR) -> Result<QuestionPlan, Vec<String>> {
    let view = ScopedView::uniform(&lab.pool, &BTreeSet::new());
    let root = LawRoot::Entity(question.object_set.as_str().to_owned());
    let space = derive_metric_space(&view, &root).map_err(|e| vec![format!("derive: {e:?}")])?;
    space.admit(question, 500).map_err(|findings| {
        findings
            .iter()
            .map(|d| format!("{}: {}", d.code, d.message))
            .collect()
    })
}

fn plan(lab: &Lab, name: &str) -> Result<QuestionPlan, String> {
    admit(lab, &golden(name)?).map_err(|codes| format!("{name} refused: {codes:?}"))
}

/// The metrics derived for `object_type`: (name, measured expression, linked time).
fn metrics(lab: &Lab, object_type: &str) -> Result<Vec<(String, Option<String>, bool)>, String> {
    let view = ScopedView::uniform(&lab.pool, &BTreeSet::new());
    let space = derive_metric_space(&view, &LawRoot::Entity(object_type.to_owned()))
        .map_err(|e| format!("derive: {e:?}"))?;
    Ok(space
        .candidates()
        .iter()
        .map(|c| {
            let linked = c
                .definition
                .norn
                .time_dimension
                .as_ref()
                .is_some_and(|time| !time.links.is_empty());
            (
                c.definition.name.clone(),
                c.definition.type_params.expr.clone(),
                linked,
            )
        })
        .collect())
}

fn answer(stress: &Stress, plan: &QuestionPlan) -> Result<QueryResult, String> {
    stress
        .cell
        .store()
        .evaluate_question(plan, context(CLOCK.saturating_add(10_000_000)), |_| {
            Ok(true)
        })
        .map_err(|e| format!("evaluate: {e}"))
}

/// Bucket key → (totals, value), each decimal canonical.
fn rows(result: &QueryResult) -> BTreeMap<String, (Vec<String>, Option<String>)> {
    result
        .rows
        .iter()
        .map(|(key, row)| {
            (
                key.clone(),
                (
                    row.totals.iter().map(norn_ir::Decimal::canonical).collect(),
                    row.value.as_ref().map(norn_ir::Decimal::canonical),
                ),
            )
        })
        .collect()
}

/// The first total of the one bucket whose key holds every fragment.
fn total(result: &QueryResult, fragments: &[&str]) -> Option<String> {
    let mut found = rows(result)
        .into_iter()
        .filter(|(key, _)| fragments.iter().all(|f| key.contains(f)));
    let (_, (totals, _)) = found.next()?;
    if found.next().is_some() {
        return None;
    }
    totals.into_iter().next()
}

fn admission_errors(lab: &Lab) -> Result<Vec<String>, String> {
    let base = base()?;
    let registry = BTreeSet::new();
    let diagnostics = norn_checker::check_artifact_set(
        &lab.candidate,
        &EntityArtifactSetContext {
            registry: &registry,
            base: &base,
            tenant: None,
        },
    );
    Ok(diagnostics
        .iter()
        .filter(|d| d.severity == DiagnosticSeverity::Error)
        .map(|d| format!("{} {:?}: {}", d.code, d.artifact_id, d.message))
        .collect())
}

// -------------------------------------------------------- axis 0: admission

/// The pack (with `ml` and `tracking`) is admitted by the Rust checker itself.
#[test]
fn supply_lab_pack_is_admitted_by_the_rust_checker() -> Result<(), String> {
    let lab = lab()?;
    let errors = admission_errors(&lab)?;
    assert!(errors.is_empty(), "{errors:#?}");
    assert!(
        lab.pack
            .actions
            .iter()
            .any(|a| a.artifact_id == "supply.po.submit.action")
    );
    Ok(())
}

/// Every dashboard tile's question admits against the derived metric space,
/// exactly as its `supply.questions.json` twin does.
#[test]
fn supply_lab_dashboard_tiles_admit_against_the_derived_space() -> Result<(), String> {
    let lab = lab()?;
    let path = repo().join("registry/packs/supply-lab/questions/supply.dashboard.json");
    let text = fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?;
    let dashboards: Vec<Value> = serde_json::from_str(&text).map_err(|e| format!("{e}"))?;
    let tiles = dashboards
        .first()
        .and_then(|dashboard| dashboard.get("tiles"))
        .and_then(Value::as_array)
        .ok_or("no tiles")?;
    assert_eq!(tiles.len(), 5);
    for tile in tiles {
        let question: QuestionIR = serde_json::from_value(
            tile.get("question")
                .cloned()
                .ok_or("tile without question")?,
        )
        .map_err(|e| format!("tile question: {e}"))?;
        let name = question.name.clone();
        assert_eq!(
            question,
            golden(&name)?,
            "{name}: the tile asks its golden question"
        );
        admit(&lab, &question).map_err(|codes| format!("{name} refused: {codes:?}"))?;
    }
    Ok(())
}

// ------------------------------------------------- axis 1: semi-additive

/// METRIC-SNAPSHOT-ASOF-1: ASOF-CARRY, ASOF-LAST-NOT-SUM and ASOF-ABSENT on the real cell.
#[test]
fn axis1_snapshots_carry_take_the_last_count_and_stay_absent_before_the_first() -> Result<(), String>
{
    let stress = seeded()?;
    let on_hand = plan(&stress.lab, "supply.on_hand_weekly")?;
    assert!(
        on_hand.as_of(),
        "a semi-additive measure answers as of each week's end"
    );
    let result = answer(&stress, &on_hand)?;
    let a = |week: &str| total(&result, &[week, "WH-A", "each"]);
    assert_eq!(
        a(WEEKS[2]).as_deref(),
        Some("900"),
        "ASOF-LAST-NOT-SUM: two W3 counts take the later, never 1830"
    );
    assert_eq!(
        a(WEEKS[4]).as_deref(),
        Some("870"),
        "ASOF-CARRY: quiet W5 carries W4"
    );
    assert_eq!(
        a(WEEKS[5]).as_deref(),
        Some("870"),
        "ASOF-CARRY: quiet W6 carries W4"
    );
    assert_eq!(
        total(&result, &[WEEKS[0], "WH-B"]),
        None,
        "ASOF-ABSENT: WH-B before its first count has no row, never 0"
    );
    assert_eq!(
        total(&result, &[WEEKS[1], "WH-B", "each"]).as_deref(),
        Some("300")
    );
    assert_eq!(
        total(&result, &[WEEKS[15], "WH-A", "kg"]).as_deref(),
        Some("1800.25"),
        "units partition, never summed"
    );
    Ok(())
}

/// METRIC-UNIT-PARTITION-1: a stock question that leaves unit and basis free is
/// partitioned by them (never summed across); at the group-by cap it refuses.
/// METRIC-ADDITIVITY: no additive-over-time metric is derived for a stock.
#[test]
fn axis1_unit_partition_and_no_naive_additive_stock() -> Result<(), String> {
    let stress = seeded()?;
    let mut wire =
        serde_json::to_value(golden("supply.on_hand_weekly")?).map_err(|e| format!("{e}"))?;
    wire["filter"] = json!([]);
    wire["groupBy"] = json!([{"path": {"property": "as_of"}, "grain": "week"}]);
    let free: QuestionIR = serde_json::from_value(wire.clone()).map_err(|e| format!("{e}"))?;
    let partitioned = admit(&stress.lab, &free)
        .map_err(|c| format!("auto-partitioned question refused: {c:?}"))?;
    let result = answer(&stress, &partitioned)?;
    // W2: on_hand each = 950 (WH-A) + 300 (WH-B); available each = 280; kg = 2000.5.
    assert_eq!(
        total(&result, &[WEEKS[1], "each", "on_hand"]).as_deref(),
        Some("1250"),
        "{:?}",
        rows(&result)
    );
    assert_eq!(
        total(&result, &[WEEKS[1], "each", "available"]).as_deref(),
        Some("280")
    );
    assert_eq!(total(&result, &[WEEKS[1], "kg"]).as_deref(), Some("2000.5"));
    // Three free group-bys leave room for one of the two unit sources only.
    wire["groupBy"] = json!([{"path": {"property": "as_of"}, "grain": "week"}, {"path": {"property": "warehouse_id"}},
        {"path": {"property": "sku_id"}}]);
    let saturated: QuestionIR = serde_json::from_value(wire).map_err(|e| format!("{e}"))?;
    // At the group-by cap the free unit source cannot be added: it refuses as
    // mixed units, never silently dropping a partition (METRIC-UNIT-PARTITION-1).
    let refused = admit(&stress.lab, &saturated).err().ok_or(
        "METRIC-UNIT-MIXED: a stock that cannot be partitioned by unit and basis must refuse",
    )?;
    assert!(
        refused
            .iter()
            .any(|finding| finding.starts_with("metric.validation.mixed_unit")),
        "{refused:?}"
    );
    let names: Vec<String> = metrics(&stress.lab, "supply.InventorySnapshot")?
        .into_iter()
        .map(|(n, _, _)| n)
        .collect();
    assert!(
        names.iter().any(|n| n.ends_with("on_hand_qty.as_of_as_of")),
        "{names:?}"
    );
    assert!(
        !names.iter().any(|n| n.contains("on_hand_qty.by_")),
        "no additive-over-time candidate: {names:?}"
    );
    Ok(())
}

/// Fill rate is sum(received)/sum(ordered) at line grain: not the mean of line ratios, no fan-out.
#[test]
fn axis2_fill_rate_is_a_ratio_of_sums_without_fan_out() -> Result<(), String> {
    let stress = seeded()?;
    let fill = plan(&stress.lab, "supply.fill_rate_weekly")?;
    let result = answer(&stress, &fill)?;
    let all = rows(&result);
    let (totals, value) = all
        .iter()
        .find(|(key, _)| key.contains("each") && key.contains(WEEKS[1]))
        .map(|(_, row)| row.clone())
        .ok_or_else(|| format!("no each/W2 fill row: {all:?}"))?;
    assert_eq!(
        totals,
        ["1000", "1100"],
        "ordered counted once per line, though PO-1/1 has two receipts"
    );
    let value = value.ok_or("a nonzero denominator has a value")?;
    assert!(value.starts_with("0.9090"), "1000/1100, got {value}");
    assert_ne!(value, "0.95", "never the mean of line ratios (0.9 and 1.0)");
    let kg = all
        .iter()
        .find(|(key, _)| key.contains("kg"))
        .map(|(_, row)| row.clone());
    assert_eq!(
        kg.map(|(t, _)| t),
        Some(vec!["250".to_owned(), "500".to_owned()])
    );
    // Chasm-trap-safe derived dataset: receipts summed to their line before the join.
    let fill_rows = line_fill(&stress)?;
    assert_eq!(
        fill_rows.get("PO-1/1").cloned(),
        Some(("1000".into(), "900".into(), "2".into())),
        "{fill_rows:?}"
    );
    Ok(())
}

/// METRIC-LINK-MOVES and relinks: IVM follows a linked-time correction, a
/// relink, an unlink and a delete of IN-5, then refuses a linked-time plan at
/// a knowledge cutoff (links are read live, never as of a cutoff).
fn ivm_follows_link_edits(
    stress: &mut Stress,
    views: &mut [(QuestionPlan, IncrementalQuestion)],
    ctx: &QueryContext,
) -> Result<(), String> {
    // METRIC-LINK-MOVES: correcting the linked receipt's time moves IN-5 to W3.
    stress.ok(
        "supply.receipt.correct_time",
        "ops:omar",
        json!({"receipt_id": "RC-3", "received_at": "2026-07-22T08:00:00Z"}),
    )?;
    advance_equals_recompute(stress, views, ctx, "correct RC-3's time")?;
    // Relink IN-5 to RC-1 (W2), unlink it, then delete it: no governed action
    // does these, so each is a kernel decision.
    let name = |text: &str| norn_ir::ObjectTypeName(text.to_owned());
    let (from, to) = (name("supply.Inspection"), name("supply.ShipmentReceipt"));
    let link = |add: bool, receipt: &str| {
        let (source_type, source_key) = (from.clone(), "IN-5".to_owned());
        let (link, target_type) = (norn_ir::LinkId("receipt".to_owned()), to.clone());
        let target_key = receipt.to_owned();
        if add {
            LinkChange::Add {
                source_type,
                source_key,
                link,
                target_type,
                target_key,
                values: norn_ir::RuntimeObject::new(),
            }
        } else {
            LinkChange::Remove {
                source_type,
                source_key,
                link,
                target_type,
                target_key,
            }
        }
    };
    let edits: [(&str, Vec<FactChange>, Vec<LinkChange>); 3] = [
        (
            "relink IN-5 to RC-1",
            Vec::new(),
            vec![link(false, "RC-3"), link(true, "RC-1")],
        ),
        ("unlink IN-5", Vec::new(), vec![link(false, "RC-1")]),
        (
            "delete IN-5",
            vec![FactChange::Delete {
                object_type: from.clone(),
                key: "IN-5".to_owned(),
            }],
            Vec::new(),
        ),
    ];
    for (step, facts, links) in edits {
        stress.decide(step, facts, links)?;
        advance_equals_recompute(stress, views, ctx, step)?;
    }
    let (_, linked) = views.get(1).ok_or("two views")?;
    assert_eq!(
        total(linked.result(), &[WEEKS[1]]).as_deref(),
        Some("120"),
        "IN-5 is gone; W2 holds IN-1..IN-3"
    );
    assert_eq!(linked.result().unlinked, 2, "IN-4 and IN-6");
    // A linked time is read live, never as of a cutoff: refused, not approximated.
    let cutoff = KnowledgeCutoff {
        recorded_at: stress.now(),
        frontier: stress.head()?.commits,
    };
    let refused = IncrementalQuestion::rebuild_known_at(
        stress.cell.store(),
        &views.get(1).ok_or("two views")?.0,
        ctx.clone(),
        cutoff,
        |_| Ok(true),
    );
    assert!(
        matches!(
            refused,
            Err(CellError::ComputeRefused("core.compute.unsupported_engine"))
        ),
        "{:?}",
        refused.err()
    );
    Ok(())
}

/// Advance every view one commit and require it equal recompute.
fn advance_equals_recompute(
    stress: &Stress,
    views: &mut [(QuestionPlan, IncrementalQuestion)],
    ctx: &QueryContext,
    step: &str,
) -> Result<(), String> {
    for (question, view) in views {
        let advanced = view
            .advance(stress.cell.store(), ctx.clone(), |_| Ok(true))
            .map_err(|e| format!("advance after {step}: {e}"))?
            .clone();
        let mut recompute = answer(stress, question)?;
        recompute.engine = Engine::Ivm;
        assert_eq!(advanced, recompute, "IVM = recompute after {step}");
        assert!(!view.was_rebuilt(), "maintained, not rebuilt");
    }
    Ok(())
}

/// The `supply.line_fill` transform: line → (ordered, received, deliveries).
fn line_fill(stress: &Stress) -> Result<BTreeMap<String, (String, String, String)>, String> {
    let artifact = stress
        .lab
        .candidate
        .iter()
        .find(|a| a.artifact_id.as_str() == "supply.line_fill")
        .ok_or("no supply.line_fill")?;
    let plan = norn_checker::transform::admit(artifact, &stress.lab.pool, &BTreeSet::new())
        .map_err(|d| format!("line_fill refused: {d:?}"))?;
    let result = stress
        .cell
        .store()
        .evaluate_transform(&plan, "main", SCOPE, |_| Ok(RowAccess::Read))
        .map_err(|e| format!("line_fill: {e}"))?;
    let text = |row: &norn_cell::compute::transform::TransformRow, column: &str| {
        row.values.get(column).map_or_default(scalar)
    };
    Ok(result
        .rows
        .iter()
        .map(|row| {
            (
                text(row, "line_id"),
                (
                    text(row, "ordered"),
                    text(row, "received"),
                    text(row, "deliveries"),
                ),
            )
        })
        .collect())
}

/// A transform cell as text: strings as themselves, numbers canonical.
fn scalar(value: &RuntimeValue) -> String {
    if let RuntimeValue::String(text) = value {
        text.clone()
    } else if let RuntimeValue::Number(number) = value {
        number.canonical()
    } else {
        format!("{value:?}")
    }
}

/// METRIC-GRAIN-LINK-1: inspections (no time of their own) are bucketed by their
/// receipt's week; an inspection with no receipt is `unlinked`, never bucketed.
/// Two inspections of one receipt count twice as inspections, never as receipts.
#[test]
fn axis2_linked_time_buckets_inspections_by_receipt_and_counts_unlinked() -> Result<(), String> {
    let stress = seeded()?;
    let inspection = "supply.Inspection";
    let metric = metrics(&stress.lab, inspection)?
        .into_iter()
        .find(|(_, expr, linked)| *linked && expr.as_deref() == Some("inspected_qty"))
        .map(|(name, _, _)| name)
        .ok_or("no linked-time inspection metric derived")?;
    let question: QuestionIR = serde_json::from_value(json!({
        "irVersion": "0.1", "name": "supply.inspected_by_receipt_week", "objectSet": inspection,
        "aggregate": [{"metric": metric}],
        "filter": [{"path": {"property": "unit"}, "operator": "eq", "value": "each"}],
        "groupBy": [{"path": {"links": ["receipt"], "property": "received_at"}, "grain": "week"}],
        "window": {"start": "2026-07-06T00:00:00Z", "end": "2026-10-26T00:00:00Z"},
        "freshness": {"kind": "live", "maxAgeMs": 60000},
    }))
    .map_err(|e| format!("{e}"))?;
    let linked =
        admit(&stress.lab, &question).map_err(|c| format!("linked question refused: {c:?}"))?;
    assert!(linked.time_link().is_some());
    let result = answer(&stress, &linked)?;
    assert_eq!(
        total(&result, &[WEEKS[1]]).as_deref(),
        Some("120"),
        "50 + 30 + 40 by the receipts' week: {:?}",
        rows(&result)
    );
    assert_eq!(
        result.unlinked, 1,
        "the orphan inspection is skipped and counted, never bucketed"
    );
    let direct = answer(&stress, &plan(&stress.lab, "supply.received_weekly")?)?;
    assert_eq!(
        total(&direct, &[WEEKS[1], "each"]).as_deref(),
        Some("1040"),
        "receipts count once each, whatever their inspections"
    );
    Ok(())
}

/// `supply.ShipmentReceipt`, hex-encoded as derived metric ids name it.
const RECEIPT_HEX: &str = "737570706c792e536869706d656e7452656365697074";

/// Spec boundary, not a gap (METRIC-LINK-OWN-FIRST, Core 03): a type with a time
/// of its own derives no linked-time metric, and a question cannot supply its own
/// metric definition (`MetricSpace::admit` admits only derived candidates). Receipts
/// by their line's promised week are a derived dataset: `supply.receipts_by_promised_week`.
#[test]
fn axis2_receipts_keep_their_own_time_and_bucket_by_promised_week_through_a_transform()
-> Result<(), String> {
    let lab = lab()?;
    let linked: Vec<String> = metrics(&lab, "supply.ShipmentReceipt")?
        .into_iter()
        .filter(|(name, _, linked)| *linked && name.contains(RECEIPT_HEX))
        .map(|(n, _, _)| n)
        .collect();
    assert!(
        linked.is_empty(),
        "METRIC-LINK-OWN-FIRST: an object with its own time derives no linked-time sum: {linked:?}"
    );
    let stress = seeded()?;
    let by_week = transform_rows(&stress, "supply.receipts_by_promised_week")?;
    // PO-1's lines are all promised 2026-07-15 (W2): 600 + 300 + 100 each, 250 kg.
    assert_eq!(
        by_week
            .get(&(WEEKS[1].to_owned(), "each".to_owned()))
            .map(String::as_str),
        Some("1000"),
        "{by_week:?}"
    );
    assert_eq!(
        by_week
            .get(&(WEEKS[1].to_owned(), "kg".to_owned()))
            .map(String::as_str),
        Some("250")
    );
    Ok(())
}

/// The rows of transform `id`: (promised week, unit) → received.
fn transform_rows(stress: &Stress, id: &str) -> Result<BTreeMap<(String, String), String>, String> {
    let artifact = stress
        .lab
        .candidate
        .iter()
        .find(|a| a.artifact_id.as_str() == id)
        .ok_or_else(|| format!("no {id}"))?;
    let plan = norn_checker::transform::admit(artifact, &stress.lab.pool, &BTreeSet::new())
        .map_err(|d| format!("{id} refused: {d:?}"))?;
    let result = stress
        .cell
        .store()
        .evaluate_transform(&plan, "main", SCOPE, |_| Ok(RowAccess::Read))
        .map_err(|e| format!("{id}: {e}"))?;
    let text = |row: &norn_cell::compute::transform::TransformRow, column: &str| {
        row.values.get(column).map_or_default(scalar)
    };
    Ok(result
        .rows
        .iter()
        .map(|row| {
            let week = text(row, "l_promised_at");
            (
                (week.chars().take(10).collect(), text(row, "unit")),
                text(row, "received"),
            )
        })
        .collect())
}

// ------------------------------------ axis 3: native refusals (quarantine below)

/// Native invalid proposals refuse with the governing law and write nothing.
#[test]
fn axis3_native_invalid_proposals_refuse_atomically() -> Result<(), String> {
    let mut stress = seeded()?;
    let (code, message) = stress.refused(
        "supply.position.record_count",
        "clerk:hana",
        count(
            "WH-A/SKU-WATER/on_hand",
            "2026-10-21T10:00:00Z",
            "-50",
            "wms_scan",
        ),
    )?;
    assert_eq!(code, "core.laws.violated", "{message}");
    assert!(message.contains("on_hand_non_negative"), "{message}");
    let (code, message) = stress.refused(
        "supply.receipt.record",
        "clerk:hana",
        receipt(
            "RC-X",
            Some("PO-1/1"),
            "each",
            "10",
            "12",
            "2026-07-17T08:00:00Z",
        ),
    )?;
    assert_eq!(code, "core.laws.violated", "{message}");
    assert!(message.contains("damaged_within_received"), "{message}");
    let (code, message) = stress.refused(
        "supply.receipt.record_blind",
        "clerk:hana",
        receipt("RC-U", None, "PALLET", "10", "0", "2026-07-17T08:00:00Z"),
    )?;
    assert!(
        code.starts_with("action.") && message.contains("unit"),
        "an unknown unit refuses at the input, before any law: {code} {message}"
    );
    let mut cross = count(
        "WH-A/SKU-WATER/on_hand",
        "2026-10-21T10:00:00Z",
        "5",
        "wms_scan",
    );
    cross["unit"] = json!("kg");
    stress.refused("supply.position.record_count", "clerk:hana", cross)?;
    stress.refused("supply.movement.record", "clerk:hana", json!({"movement_id": "MV-KG", "warehouse_id": "WH-A",
        "sku_id": "SKU-WATER", "qty": "5", "unit": "kg", "movement_kind": "issue", "occurred_at": "2026-10-20T12:00:00Z"}))?;
    let (_, message) = stress.refused("supply.movement.record", "clerk:hana", json!({"movement_id": "MV-NEG", "warehouse_id": "WH-A",
        "sku_id": "SKU-WATER", "qty": "-50", "unit": "each", "movement_kind": "issue", "occurred_at": "2026-10-20T12:00:00Z"}))?;
    assert!(message.contains("movement_positive"), "{message}");

    // Wave 0/1 cross-object guards & LAW-LINK-1 / LAW-LINK-MANY-1:
    // 1. Closing a warehouse that still holds live stock refuses.
    stress.refused(
        "supply.warehouse.close",
        "ops:omar",
        json!({"warehouse_id": "WH-A"}),
    )?;
    // Closing an empty warehouse succeeds.
    stress.ok(
        "supply.warehouse.register",
        "eng:dina",
        json!({"warehouse_id": "WH-EMPTY", "name": "Empty", "region": "red_sea", "latitude": "27.257900", "longitude": "33.811600"}),
    )?;
    stress.ok(
        "supply.warehouse.close",
        "ops:omar",
        json!({"warehouse_id": "WH-EMPTY"}),
    )?;
    // 2. Recording a receipt against a closed warehouse or an unsubmitted PO refuses.
    stress.refused(
        "supply.receipt.record",
        "clerk:hana",
        json!({"receipt_id": "RC-CLOSED", "supplier_id": "SUP-NILE", "warehouse_id": "WH-EMPTY", "line_id": "PO-1/1",
            "received_qty": "10", "damaged_qty": "0", "unit": "each", "received_at": "2026-07-17T08:00:00Z"}),
    )?;
    // 3. Over-receiving against an order line refuses via LAW-LINK-MANY-1 (OrderLine.receipts_consistent).
    let (code, message) = stress.refused(
        "supply.receipt.record",
        "clerk:hana",
        receipt(
            "RC-OVER",
            Some("PO-1/1"),
            "each",
            "200",
            "0",
            "2026-07-17T08:00:00Z",
        ),
    )?;
    assert_eq!(code, "core.laws.violated", "{message}");
    assert!(message.contains("receipts_consistent"), "{message}");
    // 4. Inspecting more than a receipt received refuses via LAW-LINK-1 (Inspection.within_receipt)
    // and cumulative over-inspection refuses via LAW-LINK-MANY-1 (ShipmentReceipt.inspections_within_received).
    let (code, message) = stress.refused(
        "supply.inspection.record",
        "clerk:hana",
        json!({"inspection_id": "IN-BIG", "receipt_id": "RC-2", "inspected_qty": "350", "rejected_qty": "0", "unit": "each"}),
    )?;
    assert_eq!(code, "core.laws.violated", "{message}");
    assert!(message.contains("within_receipt"), "{message}");
    let (code, message) = stress.refused(
        "supply.inspection.record",
        "clerk:hana",
        json!({"inspection_id": "IN-CUMUL", "receipt_id": "RC-2", "inspected_qty": "270", "rejected_qty": "0", "unit": "each"}),
    )?;
    assert_eq!(code, "core.laws.violated", "{message}");
    assert!(message.contains("inspections_within_received"), "{message}");
    Ok(())
}

/// PO lifecycle: `SoD` on submit, terminal immutability, and lines only on drafts.
#[test]
fn axis3_purchase_lifecycle_refuses_sod_and_terminal_rewrites() -> Result<(), String> {
    let mut stress = Stress::open()?;
    catalog(&mut stress)?;
    let order = json!({"po_id": "PO-9", "supplier_id": "SUP-DELTA", "promised_at": "2026-08-01T00:00:00Z", "currency": "EGP"});
    assert_eq!(
        stress.act_keyed(
            "supply.po.create",
            "planner:amal",
            order.clone(),
            "po-9-create"
        )?,
        Outcome::Committed
    );
    let (code, message) =
        stress.refused("supply.po.submit", "planner:amal", json!({"po_id": "PO-9"}))?;
    assert!(
        message.contains("ACTOR-INDEPENDENT-1"),
        "SoD: {code} {message}"
    );
    stress.ok("supply.po.submit", "ops:omar", json!({"po_id": "PO-9"}))?;
    stress.refused("supply.line.add", "planner:amal", json!({"line_id": "PO-9/1", "po_id": "PO-9", "sku_id": "SKU-WATER",
        "promised_at": "2026-08-01T00:00:00Z", "ordered_qty": "10", "received_qty": "0", "fill_rate": "0", "unit": "each",
        "unit_price": "1", "currency": "EGP"}))?;
    stress.ok("supply.po.fulfil", "clerk:hana", json!({"po_id": "PO-9"}))?;
    let (code, message) =
        stress.refused("supply.po.cancel", "ops:omar", json!({"po_id": "PO-9"}))?;
    assert!(
        !code.is_empty(),
        "a fulfilled order never cancels: {message}"
    );
    let replay = stress.act_keyed("supply.po.create", "planner:amal", order, "po-9-create")?;
    assert_eq!(
        replay,
        Outcome::Replayed,
        "the same call key replays, committing nothing"
    );
    Ok(())
}

// ------------------------------- axis 4: engine parity and incremental upkeep

/// IVM = recompute across inserts, redelivery, an update, restart and reversed order.
#[test]
fn axis4_incremental_equals_recompute_through_redelivery_and_restart() -> Result<(), String> {
    let mut stress = seeded()?;
    let volume = plan(&stress.lab, "supply.movement_volume_weekly")?;
    let ctx = context(CLOCK.saturating_add(10_000_000));
    let mut view =
        IncrementalQuestion::rebuild(stress.cell.store(), &volume, ctx.clone(), |_| Ok(true))
            .map_err(|e| format!("rebuild: {e}"))?;
    stress.ok("supply.movement.record", "clerk:hana", json!({"movement_id": "MV-LATE", "warehouse_id": "WH-A",
        "sku_id": "SKU-WATER", "qty": "30", "unit": "each", "movement_kind": "issue", "occurred_at": "2026-07-07T18:00:00Z"}))?;
    let replay = stress.act_keyed("supply.movement.record", "clerk:hana", json!({"movement_id": "MV-LATE", "warehouse_id": "WH-A",
        "sku_id": "SKU-WATER", "qty": "30", "unit": "each", "movement_kind": "issue", "occurred_at": "2026-07-07T18:00:00Z"}), &format!("call-{}", stress.calls))?;
    assert_eq!(replay, Outcome::Replayed, "redelivery commits nothing");
    let advanced = view
        .advance(stress.cell.store(), ctx.clone(), |_| Ok(true))
        .map_err(|e| format!("advance: {e}"))?
        .clone();
    let mut recompute = answer(&stress, &volume)?;
    recompute.engine = Engine::Ivm;
    assert_eq!(
        advanced, recompute,
        "incremental = recompute after insert and redelivery"
    );
    assert_eq!(
        total(&recompute, &[WEEKS[0], "each"]).as_deref(),
        Some("150"),
        "120 + the late 30"
    );
    // A ratio over an updated line: IVM retracts and replaces it.
    // Record the matching 50-unit physical receipt first so reconciling received_qty to 950
    // satisfies OrderLine.receipts_consistent and advances `ratio` by a single commit.
    stress.ok(
        "supply.receipt.record",
        "clerk:hana",
        receipt(
            "RC-5",
            Some("PO-1/1"),
            "each",
            "50",
            "0",
            "2026-07-17T08:00:00Z",
        ),
    )?;
    let fill = plan(&stress.lab, "supply.fill_rate_weekly")?;
    let mut ratio =
        IncrementalQuestion::rebuild(stress.cell.store(), &fill, ctx.clone(), |_| Ok(true))
            .map_err(|e| format!("ratio rebuild: {e}"))?;
    stress.ok(
        "supply.line.record_received",
        "clerk:hana",
        received_line("PO-1/1", "SKU-WATER", "each", "1000", "950"),
    )?;
    let updated = ratio
        .advance(stress.cell.store(), ctx.clone(), |_| Ok(true))
        .map_err(|e| format!("ratio advance: {e}"))?
        .clone();
    let mut fill_now = answer(&stress, &fill)?;
    fill_now.engine = Engine::Ivm;
    assert_eq!(updated, fill_now, "incremental = recompute after an update");
    stress.reopen()?;
    let rebuilt = IncrementalQuestion::rebuild(stress.cell.store(), &volume, ctx, |_| Ok(true))
        .map_err(|e| format!("restart: {e}"))?;
    let mut after = answer(&stress, &volume)?;
    after.engine = Engine::Ivm;
    assert!(rebuilt.was_rebuilt());
    assert_eq!(
        rebuilt.result(),
        &after,
        "a restarted cell rebuilds the same complete generation"
    );
    reversed_order_matches(&recompute)
}

/// The same movements committed in reverse order answer the same buckets.
fn reversed_order_matches(forward: &QueryResult) -> Result<(), String> {
    let mut stress = Stress::open()?;
    catalog(&mut stress)?;
    stress.ok("supply.movement.record", "clerk:hana", json!({"movement_id": "MV-LATE", "warehouse_id": "WH-A",
        "sku_id": "SKU-WATER", "qty": "30", "unit": "each", "movement_kind": "issue", "occurred_at": "2026-07-07T18:00:00Z"}))?;
    let pairs: Vec<(&str, &str)> = WEEKS.iter().copied().zip(ISSUES.iter().copied()).collect();
    for (week, qty) in pairs.iter().rev() {
        stress.ok(
            "supply.movement.record",
            "clerk:hana",
            json!({"movement_id": format!("MV-{week}"),
            "warehouse_id": "WH-A", "sku_id": "SKU-WATER", "qty": qty, "unit": "each",
            "movement_kind": "issue", "occurred_at": format!("{week}T12:00:00Z")}),
        )?;
    }
    let reversed = answer(
        &stress,
        &plan(&stress.lab, "supply.movement_volume_weekly")?,
    )?;
    assert_eq!(
        rows(&reversed),
        rows(forward),
        "commit order never changes an additive answer"
    );
    Ok(())
}

/// QUERY-ENGINE-1: IVM maintains a snapshot (as-of) plan and a linked-time plan,
/// each equal to recompute after every commit: a new count, a restatement, an
/// inspection linked to its receipt, and an unlinked one.
#[test]
fn axis4_ivm_maintains_snapshot_and_linked_time_plans_equal_to_recompute() -> Result<(), String> {
    let mut stress = seeded()?;
    let ctx = context(CLOCK.saturating_add(10_000_000));
    let mut views = Vec::new();
    for name in ["supply.on_hand_weekly", "supply.inspected_by_receipt_week"] {
        let question = plan(&stress.lab, name)?;
        let view =
            IncrementalQuestion::rebuild(stress.cell.store(), &question, ctx.clone(), |_| Ok(true))
                .map_err(|e| format!("rebuild {name}: {e}"))?;
        views.push((question, view));
    }
    let steps = [
        (
            "supply.position.record_count",
            "clerk:hana",
            count(
                "WH-A/SKU-WATER/on_hand",
                "2026-10-21T10:00:00Z",
                "610",
                "wms_scan",
            ),
        ),
        (
            "supply.position.restate",
            "ops:omar",
            count(
                "WH-B/SKU-WATER/on_hand",
                "2026-09-08T09:00:00Z",
                "420",
                "audit",
            ),
        ),
        (
            "supply.inspection.record",
            "clerk:hana",
            json!({"inspection_id": "IN-5", "receipt_id": "RC-3",
                "inspected_qty": "25", "rejected_qty": "0", "unit": "each"}),
        ),
        (
            "supply.inspection.record_orphan",
            "clerk:hana",
            json!({"inspection_id": "IN-6", "receipt_id": null,
                "inspected_qty": "5", "rejected_qty": "0", "unit": "each"}),
        ),
    ];
    for (action, actor, input) in steps {
        stress.ok(action, actor, input)?;
        advance_equals_recompute(&stress, &mut views, &ctx, action)?;
    }
    assert_eq!(
        total(views.get(1).ok_or("two views")?.1.result(), &[WEEKS[1]]).as_deref(),
        Some("145"),
        "120 + IN-5's 25 by RC-3's week"
    );
    ivm_follows_link_edits(&mut stress, &mut views, &ctx)?;
    let (_, snapshot) = views.first().ok_or("two views")?;
    assert_eq!(
        total(snapshot.result(), &[WEEKS[9], "WH-B", "each"]).as_deref(),
        Some("420"),
        "the restated W10 count"
    );
    Ok(())
}

// --------------------------------- axis 5: bitemporal knowledge and restatement

/// W10 at WH-B: 500 as known at K1, 420 at K2; K1 stays reproducible; lineage reconciles.
#[test]
fn axis5_restatement_keeps_k1_reproducible_and_lineage_reconciles() -> Result<(), String> {
    let mut stress = seeded()?;
    let on_hand = plan(&stress.lab, "supply.on_hand_weekly")?;
    let k1 = KnowledgeCutoff {
        recorded_at: stress.now(),
        frontier: stress.head()?.commits,
    };
    let before = answer(&stress, &on_hand)?;
    stress.ok(
        "supply.position.restate",
        "ops:omar",
        count(
            "WH-B/SKU-WATER/on_hand",
            "2026-09-08T09:00:00Z",
            "420",
            "audit",
        ),
    )?;
    let k2 = KnowledgeCutoff {
        recorded_at: stress.now(),
        frontier: stress.head()?.commits,
    };
    let now = answer(&stress, &on_hand)?;
    let w10 = [WEEKS[9], "WH-B", "each"];
    assert_eq!(total(&before, &w10).as_deref(), Some("500"));
    assert_eq!(total(&now, &w10).as_deref(), Some("420"), "(T=W10, K=K2)");
    assert_eq!(
        total(&now, &[WEEKS[11], "WH-B", "each"]).as_deref(),
        Some("450"),
        "W12's later count still wins"
    );
    let at = |cutoff: KnowledgeCutoff| {
        stress
            .cell
            .store()
            .evaluate_question_known_at(
                &on_hand,
                context(cutoff.recorded_at.as_millis()),
                cutoff,
                |_| Ok(true),
            )
            .map(|h| h.result)
            .map_err(|e| format!("known at: {e}"))
    };
    let known_k1 = at(k1)?;
    assert_eq!(
        total(&known_k1, &w10).as_deref(),
        Some("500"),
        "(T=W10, K=K1) is reproducible after the restatement"
    );
    assert_eq!(
        rows(&known_k1),
        rows(&before),
        "the old generation is retained, bucket for bucket"
    );
    assert_eq!(total(&at(k2)?, &w10).as_deref(), Some("420"));
    let delta = signed_delta(&before, &now, &w10)?;
    assert_eq!(delta, "-80", "the restatement retracts exactly 80 units");
    // Anti-leakage: a cutoff recorded before the frontier it names is refused.
    let leak = KnowledgeCutoff {
        recorded_at: k1.recorded_at,
        frontier: k2.frontier,
    };
    let refused = stress.cell.store().evaluate_question_known_at(
        &on_hand,
        context(k1.recorded_at.as_millis()),
        leak,
        |_| Ok(true),
    );
    assert!(
        matches!(refused, Err(CellError::TemporalLeakage)),
        "{:?}",
        refused.err().map(|e| e.to_string())
    );
    lineage_reconciles(&stress, &on_hand, &now)
}

/// `now - before` for one integral bucket.
fn signed_delta(
    before: &QueryResult,
    now: &QueryResult,
    bucket: &[&str],
) -> Result<String, String> {
    let read = |r: &QueryResult| -> Result<i64, String> {
        total(r, bucket)
            .ok_or_else(|| "no bucket".to_owned())?
            .parse::<i64>()
            .map_err(|e| format!("{e}"))
    };
    let (old, new) = (read(before)?, read(now)?);
    new.checked_sub(old)
        .map(|d| d.to_string())
        .ok_or_else(|| "overflow".to_owned())
}

/// DATA-LINEAGE: tracing a bucket at its answer's frontier names exactly its members.
fn lineage_reconciles(
    stress: &Stress,
    plan: &QuestionPlan,
    answer: &QueryResult,
) -> Result<(), String> {
    let (bucket, members) = answer
        .rows
        .iter()
        .find(|(key, _)| key.contains(WEEKS[9]) && key.contains("WH-B"))
        .map(|(key, row)| (key.clone(), row.members.len()))
        .ok_or("no W10/WH-B bucket")?;
    let lineage = answer.lineage_ref.clone();
    let traced = stress
        .cell
        .store()
        .trace_question_answered_matching(
            plan,
            context(CLOCK.saturating_add(10_000_000)),
            answer.known_as_of,
            &|handle| handle == lineage,
            &bucket,
            |_| Ok(true),
            LineageDisclosure {
                values: &mut |_| Ok(true),
                source: &mut |_| Ok(true),
            },
        )
        .map_err(|e| format!("trace: {e}"))?;
    assert_eq!(
        traced.contributions.len(),
        members,
        "every contribution of the bucket, and nothing else"
    );
    Ok(())
}

// --------------------------------------- axis 6: forecasting and promotion

fn run(stress: &mut Stress, id: &str) -> Result<(), String> {
    stress.ok("tracking.run.create", "eng:dina", json!({"run_id": id, "experiment_id": "exp-demand", "run_name": id,
        "user_id": "eng:dina", "start_time": 1, "artifact_uri": format!("s3://lab/{id}"), "tags": "[]"}))?;
    stress.ok("tracking.run.finish", "eng:dina", json!({"run_id": id}))
}

fn version(id: &str, pinned: &str) -> Value {
    json!({"version_id": id, "model_id": "supply.demand_forecast", "version": id, "training_run": "run-train",
        "evaluation_run": "run-eval", "artifact_uri": format!("s3://lab/{id}"), "artifact_sha256": "ab".repeat(32),
        "artifact_size": 4096, "input_type": "supply.DemandObservation", "input_revision": pinned,
        "pinned_dataset_revision": pinned})
}

fn forecast_evaluator_manifest_json() -> Value {
    json!({
        "abi": "norn.function-abi/1",
        "bounds": {
            "maxCpuMillis": 60000,
            "maxDecodedBytes": 67108864,
            "maxInputBytes": 16777216,
            "maxMemoryBytes": 536870912,
            "maxOutputBytes": 1048576,
            "maxWallMillis": 120000
        },
        "cache": "none",
        "capabilities": [],
        "dependencyClosure": {
            "digest": "sha256:7de35b8e20e7846ea24f9c06cf34311dcc32ec8302d1ae12ee28996167aacf6d",
            "entries": [
                {
                    "digest": "sha256:07d4824036af339fa1b6ef769cc99d9c10a141769538b52b3b597c84f7a7b535",
                    "kind": "base_image",
                    "name": "base-python-3.11-slim"
                },
                {
                    "digest": "sha256:20af5c3138a5a2f232783307c65f5737410cf993e92471c44e624acbf818f12c",
                    "kind": "library",
                    "name": "norn-evaluator-cli"
                },
                {
                    "digest": "sha256:208ad3e35c4bca5743b54ce9e01cd6e7a9b25eaeedddfd3ea6b8454e7d766ffd",
                    "kind": "asset",
                    "name": "norn-evaluator-dockerfile"
                },
                {
                    "digest": "sha256:f0b12ce3d5196e69c5ef12b2fc91961405cd54702cb6f6a0dbfc8b218c68de45",
                    "kind": "library",
                    "name": "norn-evaluator-init"
                },
                {
                    "digest": "sha256:0c791aae2c792800bf7fa950377c07f31be6db0c0d96457bdf5d22d36e608683",
                    "kind": "library",
                    "name": "norn-evaluator-receipt"
                }
            ]
        },
        "determinism": {
            "hermetic_container": {
                "credentials": false,
                "goldenRuns": [],
                "isolatedScratch": true,
                "locale": "C.UTF-8",
                "network": false,
                "qualified": false,
                "timezone": "UTC"
            }
        },
        "errorSchema": {
            "digest": "sha256:c9a2c5b53dd1a995abeda0f1ed3367e9d2517bdf4ca7ae1a9976c8721772736f",
            "id": "ml.forecast.evaluate.error",
            "version": 1
        },
        "functionId": "ml.forecast.evaluate",
        "implementation": {
            "digest": "sha256:949a59f1deae17acca5d42546b1543639d0dcde535f347af1a111110d7b125db",
            "entryPoint": "python -m norn_regression.evaluator_cli",
            "format": "oci_image",
            "locator": null
        },
        "inputSchema": {
            "digest": "sha256:833d56b2c89ec4920ca325fae281277f85072321886dc10c7b11ca6e9a23a3fc",
            "id": "ml.forecast.evaluate.input",
            "version": 1
        },
        "outputSchema": {
            "digest": "sha256:61bf3f4fe4992c7280a8f1e8c165e7996426524ee48971344387e0225e4f8328",
            "id": "ml.forecast.evaluate.output",
            "version": 1
        },
        "platform": {
            "arch": "amd64",
            "os": "linux"
        },
        "retention": "ledger",
        "runtime": {
            "digest": "sha256:07d4824036af339fa1b6ef769cc99d9c10a141769538b52b3b597c84f7a7b535",
            "id": "python-container",
            "version": "3.11-slim"
        },
        "steps": [],
        "version": 1
    })
}

fn evaluation_summary(
    evaluated_revision: &str,
    artifact_sha256: &str,
    artifact_size: i64,
    contracts_passed: bool,
) -> Result<JsonObject, String> {
    let metric_val = Decimal::parse_wire("0.12").map_err(|e| format!("decimal: {e}"))?;
    Ok(JsonObject::from([
        (
            "evaluation_revision".to_owned(),
            RuntimeValue::String(evaluated_revision.to_owned()),
        ),
        (
            "evaluated_artifact_sha256".to_owned(),
            RuntimeValue::String(artifact_sha256.to_owned()),
        ),
        (
            "evaluated_artifact_size".to_owned(),
            RuntimeValue::from(artifact_size),
        ),
        (
            "metric_name".to_owned(),
            RuntimeValue::String("wape".to_owned()),
        ),
        ("metric_value".to_owned(), RuntimeValue::Number(metric_val)),
        (
            "contracts_passed".to_owned(),
            RuntimeValue::Bool(contracts_passed),
        ),
        ("checks_passed".to_owned(), RuntimeValue::from(11_i64)),
        (
            "checks_failed".to_owned(),
            RuntimeValue::from(i64::from(!contracts_passed)),
        ),
        (
            "suite_version".to_owned(),
            RuntimeValue::String("1".to_owned()),
        ),
    ]))
}

fn evaluation_invocation(
    cmd_id: &str,
    version_id: &str,
    manifest_digest: &str,
    evaluated_revision: &str,
    artifact_sha256: &str,
    artifact_size: u64,
    now_millis: u64,
) -> FunctionInvocation {
    FunctionInvocation {
        tenant: SCOPE.to_owned(),
        world: "main".to_owned(),
        authority: Authority {
            kind: AuthorityKind::Service,
            id: FORECAST_EVALUATOR_AUTHORITY.to_owned(),
        },
        command_id: cmd_id.to_owned(),
        run: RunRef {
            id: version_id.to_owned(),
            generation: 1,
        },
        manifest: Digest(manifest_digest.to_owned()),
        inputs: BTreeMap::from([
            (
                "dataset_revision".to_owned(),
                RuntimeValue::String(evaluated_revision.to_owned()),
            ),
            (
                "evaluation_run".to_owned(),
                RuntimeValue::String("run-eval".to_owned()),
            ),
            (
                "features".to_owned(),
                RuntimeValue::String("qty".to_owned()),
            ),
            (
                "metric_name".to_owned(),
                RuntimeValue::String("wape".to_owned()),
            ),
            (
                "model_id".to_owned(),
                RuntimeValue::String("supply.demand_forecast".to_owned()),
            ),
            (
                "order".to_owned(),
                RuntimeValue::String("week_start".to_owned()),
            ),
            (
                "target".to_owned(),
                RuntimeValue::String("predicted_qty".to_owned()),
            ),
            (
                "version_id".to_owned(),
                RuntimeValue::String(version_id.to_owned()),
            ),
        ]),
        blobs: BTreeMap::from([
            (
                "dataset".to_owned(),
                BlobRef {
                    digest: Digest(format!("sha256:{}", "cd".repeat(32))),
                    version: Some(evaluated_revision.to_owned()),
                    size_bytes: 512,
                },
            ),
            (
                "model".to_owned(),
                BlobRef {
                    digest: Digest(format!("sha256:{artifact_sha256}")),
                    version: Some(version_id.to_owned()),
                    size_bytes: artifact_size,
                },
            ),
        ]),
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
    }
}

struct EvalReceiptSpec<'a> {
    cmd_id: &'a str,
    version_id: &'a str,
    manifest_digest: &'a str,
    evaluated_revision: &'a str,
    artifact_sha256: &'a str,
    artifact_size: u64,
    contracts_passed: bool,
}

fn build_evaluation_receipt(
    spec: &EvalReceiptSpec<'_>,
    now_millis: u64,
) -> Result<(DispatchedCommand, FunctionReceipt, JsonObject), String> {
    let summary = evaluation_summary(
        spec.evaluated_revision,
        spec.artifact_sha256,
        i64::try_from(spec.artifact_size).map_err(|e| format!("artifact size: {e}"))?,
        spec.contracts_passed,
    )?;
    let invocation = evaluation_invocation(
        spec.cmd_id,
        spec.version_id,
        spec.manifest_digest,
        spec.evaluated_revision,
        spec.artifact_sha256,
        spec.artifact_size,
        now_millis,
    );
    let dispatched = DispatchedCommand::pin(
        invocation,
        Digest(FORECAST_EVALUATOR_PROFILE_DIGEST.to_owned()),
    )
    .map_err(|e| format!("pin evaluator command: {e}"))?;
    let output_manifest =
        output_manifest_digest(&[], &summary).map_err(|e| format!("output_manifest: {e}"))?;
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
            executor: FORECAST_EVALUATOR_EXECUTOR.to_owned(),
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
    let encoded = match norn_ir::runtime::encode(&receipt).map_err(|e| format!("encode: {e}"))? {
        RuntimeValue::Object(obj) => obj,
        other => return Err(format!("encoded receipt was not object: {other:?}")),
    };
    Ok((dispatched, receipt, encoded))
}

fn retain_evaluation_receipt(
    stress: &mut Stress,
    spec: &EvalReceiptSpec<'_>,
) -> Result<String, String> {
    let (dispatched, receipt, encoded) = build_evaluation_receipt(spec, stress.now().as_millis())?;
    if let Ok(mut answers) = stress.receipt_answers.lock() {
        answers.insert(spec.cmd_id.to_owned(), encoded);
    }
    let state = norn_ir::runtime::encode(&CommandState::dispatched(&dispatched))
        .map_err(|e| format!("encode state: {e}"))?;
    let invocation = norn_ir::runtime::encode(&dispatched.invocation)
        .map_err(|e| format!("encode invocation: {e}"))?;
    let freshness = dispatched
        .freshness()
        .map_err(|e| format!("freshness: {e}"))?;
    stress
        .cell
        .commit(CellCommit {
            writes: vec![FactWrite {
                key: FactKey::new(FUNCTION_COMMAND_TYPE, spec.cmd_id),
                expected: FactVersion::ABSENT,
                mutation: FactMutation::Put {
                    values: JsonObject::from([("state".to_owned(), state)]),
                },
            }],
            effects: vec![Effect::Perform(Command {
                id: dispatched.command_id(),
                generation: dispatched.generation(),
                idempotency_key: IdempotencyKey(format!("{}/1", spec.cmd_id)),
                freshness,
                deadline: None,
                payload: JsonObject::from([
                    (
                        "type".to_owned(),
                        RuntimeValue::String(FUNCTION_PAYLOAD_TYPE.to_owned()),
                    ),
                    ("invocation".to_owned(), invocation),
                ]),
            })],
            ..CellCommit::empty()
        })
        .map_err(|e| format!("enqueue evaluation command: {e}"))?;
    let performed = stress
        .cell
        .perform()
        .map_err(|e| format!("perform evaluation command: {e}"))?;
    if performed.is_none() {
        return Err(format!(
            "evaluation command {} did not perform",
            spec.cmd_id
        ));
    }
    let command = dispatched.command_id();
    let digest = stress
        .cell
        .retain_function_receipt(
            &dispatched,
            &Completion {
                receipt: &receipt,
                command: &command,
                generation: dispatched.generation(),
                authenticated_executor: Some(FORECAST_EVALUATOR_EXECUTOR),
            },
            &ResultChecks {
                authorizer: &EvaluatorReceiptContract,
                validator: &EvaluatorReceiptContract,
            },
        )
        .map_err(|e| format!("retain evaluation receipt: {e}"))?;
    Ok(digest.0)
}

fn forecast(id: &str, model_version: &str) -> Value {
    json!({"forecast_id": id, "subject": "OBS-W13", "model_version": model_version, "sku_id": "SKU-WATER",
        "warehouse_id": "WH-A", "week_start": "2026-09-28T00:00:00Z", "predicted_qty": "182.5", "stockout_risk": "low"})
}

fn assert_forecast_evaluator_manifest_parity() -> Result<(), String> {
    let manifest: FunctionManifest = serde_json::from_value(forecast_evaluator_manifest_json())
        .map_err(|e| format!("manifest decode: {e}"))?;
    let admitted = admit_function_manifest(&manifest)
        .map_err(|diags| format!("manifest admission refused: {diags:?}"))?;
    assert_eq!(admitted.digest.0, FORECAST_EVALUATOR_MANIFEST_DIGEST);
    assert_eq!(
        admitted.execution_profile.0,
        FORECAST_EVALUATOR_PROFILE_DIGEST
    );
    let registered_sha = "ab".repeat(32);
    let (dispatched, receipt, _) = build_evaluation_receipt(
        &EvalReceiptSpec {
            cmd_id: "cmd-parity-check",
            version_id: "v1",
            manifest_digest: FORECAST_EVALUATOR_MANIFEST_DIGEST,
            evaluated_revision: "c1",
            artifact_sha256: &registered_sha,
            artifact_size: 4096,
            contracts_passed: true,
        },
        CLOCK,
    )?;
    let inv_diags = check_function_invocation(&dispatched.invocation, &admitted);
    assert_eq!(
        inv_diags,
        vec![],
        "worker-shaped invocation must be admitted by norn-checker"
    );
    let rec_diags = check_function_receipt(&receipt, &dispatched.invocation, &admitted);
    assert_eq!(
        rec_diags,
        vec![],
        "worker-shaped receipt must be admitted by norn-checker"
    );
    Ok(())
}

fn accept_authenticated_evaluations(
    stress: &mut Stress,
    k1: &str,
    k2: &str,
) -> Result<String, String> {
    // Unretained/forged receipt digest is refused before any transition.
    let (unretained_code, _) = stress.refused(
        "ml.modelversion.accept_evaluation",
        "eng:dina",
        json!({"version_id": "v1", "evaluation_receipt": format!("sha256:{}", "a".repeat(64))}),
    )?;
    assert_eq!(unretained_code, "action.receipt_unknown");

    let registered_sha = "ab".repeat(32);
    let mismatched_sha = "ef".repeat(32);
    // Wrong manifest digest is refused even when retained by the host.
    let wrong_manifest = format!("sha256:{}", "9".repeat(64));
    let wrong_manifest_receipt = retain_evaluation_receipt(
        stress,
        &EvalReceiptSpec {
            cmd_id: "cmd-eval-wrong-manifest",
            version_id: "v1",
            manifest_digest: &wrong_manifest,
            evaluated_revision: k1,
            artifact_sha256: &registered_sha,
            artifact_size: 4096,
            contracts_passed: true,
        },
    )?;
    let (wrong_manifest_code, _) = stress.refused(
        "ml.modelversion.accept_evaluation",
        "eng:dina",
        json!({"version_id": "v1", "evaluation_receipt": wrong_manifest_receipt}),
    )?;
    assert_eq!(wrong_manifest_code, "action.receipt_unknown");

    let r1 = retain_evaluation_receipt(
        stress,
        &EvalReceiptSpec {
            cmd_id: "cmd-eval-v1",
            version_id: "v1",
            manifest_digest: FORECAST_EVALUATOR_MANIFEST_DIGEST,
            evaluated_revision: k1,
            artifact_sha256: &registered_sha,
            artifact_size: 4096,
            contracts_passed: true,
        },
    )?;
    let r2 = retain_evaluation_receipt(
        stress,
        &EvalReceiptSpec {
            cmd_id: "cmd-eval-v2",
            version_id: "v2",
            manifest_digest: FORECAST_EVALUATOR_MANIFEST_DIGEST,
            evaluated_revision: k2,
            artifact_sha256: &registered_sha,
            artifact_size: 4096,
            contracts_passed: true,
        },
    )?;
    let r3 = retain_evaluation_receipt(
        stress,
        &EvalReceiptSpec {
            cmd_id: "cmd-eval-v3",
            version_id: "v3",
            manifest_digest: FORECAST_EVALUATOR_MANIFEST_DIGEST,
            evaluated_revision: k1,
            artifact_sha256: &registered_sha,
            artifact_size: 4096,
            contracts_passed: false,
        },
    )?;
    // Same-version receipt on K1 passing contracts, but scored on different model bytes.
    let r4 = retain_evaluation_receipt(
        stress,
        &EvalReceiptSpec {
            cmd_id: "cmd-eval-v4-wrong-artifact",
            version_id: "v4",
            manifest_digest: FORECAST_EVALUATOR_MANIFEST_DIGEST,
            evaluated_revision: k1,
            artifact_sha256: &mismatched_sha,
            artifact_size: 4096,
            contracts_passed: true,
        },
    )?;
    // RUN-BIND-1: a receipt dispatched for v2 cannot be accepted for v1.
    let (run_mismatch_code, _) = stress.refused(
        "ml.modelversion.accept_evaluation",
        "eng:dina",
        json!({"version_id": "v1", "evaluation_receipt": r2}),
    )?;
    assert_eq!(run_mismatch_code, "action.receipt_run_mismatch");

    for (id, receipt_ref) in [("v1", &r1), ("v2", &r2), ("v3", &r3), ("v4", &r4)] {
        stress.ok(
            "ml.modelversion.accept_evaluation",
            "eng:dina",
            json!({"version_id": id, "evaluation_receipt": receipt_ref}),
        )?;
    }
    // Replaying an already-consumed evaluation receipt is refused.
    let (replay_code, _) = stress.refused(
        "ml.modelversion.accept_evaluation",
        "eng:dina",
        json!({"version_id": "v0", "evaluation_receipt": r1}),
    )?;
    assert_eq!(replay_code, "action.receipt_replayed");
    Ok(r1)
}

/// K1 and K2 revisions, and four versions: v0 unevaluated, v1 authenticated on K1 passing,
/// v2 authenticated on K2, v3 authenticated on K1 failing a contract.
fn register_versions(stress: &mut Stress) -> Result<(String, String, String), String> {
    assert_forecast_evaluator_manifest_parity()?;
    let k1 = format!("c{}", stress.head()?.commits);
    stress.ok(
        "supply.position.restate",
        "ops:omar",
        count(
            "WH-B/SKU-WATER/on_hand",
            "2026-09-08T09:00:00Z",
            "420",
            "audit",
        ),
    )?;
    let k2 = format!("c{}", stress.head()?.commits);
    stress.ok(
        "tracking.experiment.create",
        "eng:dina",
        json!({"experiment_id": "exp-demand", "name": "demand",
        "artifact_location": "s3://lab", "creation_time": 1, "tags": "[]"}),
    )?;
    run(stress, "run-train")?;
    run(stress, "run-eval")?;
    // Caller-supplied evaluation evidence is rejected at registration (no self-attestation).
    let mut self_attested = version("v-forged", &k1);
    if let Some(obj) = self_attested.as_object_mut() {
        obj.insert("evaluation_revision".to_owned(), json!(k1));
        obj.insert("contracts_passed".to_owned(), json!(true));
    }
    let (forged_code, _) = stress.refused("ml.modelversion.register", "eng:dina", self_attested)?;
    assert_eq!(forged_code, "action.invalid_input");

    for id in ["v0", "v1", "v2", "v3", "v4"] {
        stress.ok("ml.modelversion.register", "eng:dina", version(id, &k1))?;
    }
    let r1 = accept_authenticated_evaluations(stress, &k1, &k2)?;
    Ok((k1, k2, r1))
}

fn assert_promoted_v1_evidence(stress: &Stress, r1: &str, k1: &str) -> Result<(), String> {
    let promoted = stress
        .cell
        .load(&FactKey::new("ml.ModelVersion", "v1"))
        .map_err(|e| format!("load v1: {e}"))?;
    let stored = promoted.values.ok_or("v1 must exist")?;
    assert_eq!(
        stored.get("evaluation_receipt"),
        Some(&RuntimeValue::String(r1.to_owned()))
    );
    assert_eq!(
        stored.get("evaluation_revision"),
        Some(&RuntimeValue::String(k1.to_owned()))
    );
    assert_eq!(
        stored.get("evaluated_artifact_sha256"),
        Some(&RuntimeValue::String("ab".repeat(32)))
    );
    assert_eq!(
        stored.get("evaluated_artifact_size"),
        Some(&RuntimeValue::from(4096_i64))
    );
    assert_eq!(
        stored.get("contracts_passed"),
        Some(&RuntimeValue::Bool(true))
    );
    Ok(())
}

/// Train on W1–W12 at K1; promotion only from an authenticated evaluation receipt on the pinned revision.
#[test]
fn axis6_promotion_is_pinned_to_k1_and_forecasts_need_production() -> Result<(), String> {
    let mut stress = seeded()?;
    for (week, qty) in WEEKS.iter().zip(ISSUES) {
        stress.ok("supply.observation.record", "eng:dina", json!({"observation_id": format!("OBS-{week}"), "sku_id": "SKU-WATER",
            "warehouse_id": "WH-A", "week_start": format!("{week}T00:00:00Z"), "week_index": 1, "issued_qty": qty,
            "unit": "each", "lead_time_days": 7, "promotion": false}))?;
    }
    let (k1, _k2, r1) = register_versions(&mut stress)?;
    // Unevaluated candidate (no accepted receipt) cannot be promoted.
    stress.refused(
        "ml.modelversion.promote",
        "ops:omar",
        json!({"version_id": "v0"}),
    )?;
    // ACTOR-INDEPENDENT-1: the principal that registered a version never promotes it.
    let (_, own) = stress.refused(
        "ml.modelversion.promote",
        "eng:dina",
        json!({"version_id": "v1"}),
    )?;
    assert!(own.contains("ACTOR-INDEPENDENT-1"), "{own}");
    // ANALYSIS-PUBLISH-1: evaluated on another revision than the pinned one.
    let (code, stale) = stress.refused(
        "ml.modelversion.promote",
        "ops:omar",
        json!({"version_id": "v2"}),
    )?;
    assert_eq!(code, "core.projection.publication_guard", "{stale}");
    let (_, failed) = stress.refused(
        "ml.modelversion.promote",
        "ops:omar",
        json!({"version_id": "v3"}),
    )?;
    assert!(!stale.is_empty() && !failed.is_empty());
    // Same-version passing receipt on K1 scored on DIFFERENT model artifact bytes refuses promotion.
    let (mismatch_code, mismatch_msg) = stress.refused(
        "ml.modelversion.promote",
        "ops:omar",
        json!({"version_id": "v4"}),
    )?;
    assert_eq!(
        mismatch_code, "core.projection.publication_guard",
        "{mismatch_msg}"
    );
    stress.refused(
        "supply.forecast.record",
        "eng:dina",
        forecast("F-cand", "v1"),
    )?;
    stress.ok(
        "ml.modelversion.promote",
        "ops:omar",
        json!({"version_id": "v1"}),
    )?;
    assert_promoted_v1_evidence(&stress, &r1, &k1)?;
    stress.ok("supply.forecast.record", "eng:dina", forecast("F-1", "v1"))?;
    let mut negative = forecast("F-neg", "v1");
    negative["predicted_qty"] = json!("-3");
    let (code, message) = stress.refused("supply.forecast.record", "eng:dina", negative)?;
    assert!(
        code == "core.laws.violated" && message.contains("forecast_non_negative"),
        "{code}: {message}"
    );
    stress.refused("supply.forecast.record", "eng:dina", forecast("F-2", "v2"))?;
    stress.refused(
        "ml.predictionbatch.record",
        "eng:dina",
        json!({"batch_id": "B-2", "model_version": "v2",
        "input_type": "supply.DemandObservation", "input_revision": k1, "rows": 4}),
    )?;
    stress.ok(
        "ml.predictionbatch.record",
        "eng:dina",
        json!({"batch_id": "B-1", "model_version": "v1",
        "input_type": "supply.DemandObservation", "input_revision": k1, "rows": 4}),
    )?;
    let (_, own) = stress.refused(
        "ml.modelversion.retire",
        "eng:dina",
        json!({"version_id": "v1"}),
    )?;
    assert!(own.contains("ACTOR-INDEPENDENT-1"), "{own}");
    stress.ok(
        "ml.modelversion.retire",
        "ops:omar",
        json!({"version_id": "v1"}),
    )?;
    // APPROVAL-1: a hosted cell also parks every promotion for an admin's decision.
    let gate: Value = serde_json::from_str(
        &fs::read_to_string(repo().join("registry/packs/ml/promote.approval.json"))
            .map_err(|e| format!("approval: {e}"))?,
    )
    .map_err(|e| format!("approval: {e}"))?;
    assert_eq!(gate["payload"]["action"], "ml.modelversion.promote");
    stress.refused("supply.forecast.record", "eng:dina", forecast("F-3", "v1"))?;
    Ok(())
}

// --------------------------------------------- axis 7: WORLD-FORK what-if

/// A port-strike fork: +14 days lead time, +30% demand; main is untouched and fork evidence never exports.
#[test]
fn axis7_port_strike_fork_never_interferes_with_main() -> Result<(), String> {
    let mut stress = seeded()?;
    let volume = plan(&stress.lab, "supply.movement_volume_weekly")?;
    let main_before = answer(&stress, &volume)?;
    let image = stress
        .cell
        .store()
        .image()
        .map_err(|e| format!("image: {e}"))?;
    let base = stress.head()?;
    let mut forked = stress
        .cell
        .store()
        .fork("port-strike", &base)
        .map_err(|e| format!("fork: {e}"))?;
    assert!(
        matches!(
            forked.record_performed("cmd-1", 0, &RuntimeValue::Null),
            Err(CellError::WorldPerformForbidden)
        ),
        "WORLD-ISOLATE-1: a fork never records a performed command"
    );
    let fork_cell =
        Cell::new(forked, SCOPE, |c| c.payload.clone()).map_err(|e| format!("fork cell: {e}"))?;
    let main_cell = std::mem::replace(&mut stress.cell, fork_cell);
    movements(&mut stress, "MV-STRIKE", &STRIKE)?;
    stress.ok(
        "supply.po.create",
        "planner:amal",
        json!({"po_id": "PO-STRIKE", "supplier_id": "SUP-NILE",
        "promised_at": "2026-07-29T00:00:00Z", "currency": "EGP"}),
    )?;
    let fork_answer = answer(&stress, &volume)?;
    assert_eq!(
        total(&fork_answer, &[WEEKS[0], "each"]).as_deref(),
        Some("156"),
        "120 + 30% in the fork"
    );
    assert!(
        matches!(stress.cell.perform(), Ok(None)),
        "an empty fork outbox has nothing to perform (the pack enqueues no commands)"
    );
    assert!(
        matches!(
            stress.cell.store().transparency_checkpoint(),
            Err(CellError::ForkEvidenceForbidden)
        ),
        "FORK-EVIDENCE-1"
    );
    let minted = stress.head()?.commits;
    assert!(matches!(
        stress.cell.store().export_record(minted),
        Err(CellError::ForkEvidenceForbidden)
    ));
    stress.cell = main_cell;
    assert_eq!(
        stress
            .cell
            .store()
            .image()
            .map_err(|e| format!("image: {e}"))?,
        image,
        "FORK-NONINTERFERE: main unchanged"
    );
    assert_eq!(
        stress.head()?,
        base,
        "main's ledger head is where the fork left it"
    );
    assert_eq!(
        rows(&answer(&stress, &volume)?),
        rows(&main_before),
        "main's metrics unchanged"
    );
    stress
        .cell
        .store()
        .read()
        .verify_ledger()
        .map_err(|e| format!("verify_ledger: {e}"))?;
    Ok(())
}

// ----------------------------------- axis 3: messy feeds and replay-stable quarantine

type MapCell =
    SqliteCell<norn_kernel::ObservationEvent, norn_ir::RuntimeObject, norn_ir::RuntimeObject>;

fn runtime(artifacts: &[ArtifactEnvelope]) -> Result<Vec<RuntimeValue>, String> {
    artifacts
        .iter()
        .map(|a| {
            let wire = serde_json::to_value(a).map_err(|e| format!("{e}"))?;
            RuntimeValue::from_json(wire).map_err(|e| format!("{e:?}"))
        })
        .collect()
}

/// A cell with the pack installed under a real admission receipt, so its inbox
/// recomputes mapping bundles against the retained definitions.
fn mapping_cell(lab: &Lab, dir: &Path) -> Result<MapCell, String> {
    let base = base()?;
    let (sources, base_sources) = (runtime(&lab.candidate)?, runtime(&base)?);
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
        return Err(format!("not admitted: {:?}", checked.diagnostics));
    }
    let wire = checked
        .receipt
        .to_canonical_value()
        .map_err(|e| format!("{e:?}"))?;
    let receipt =
        String::from_utf8(norn_ir::canonical::serialize(&wire).map_err(|e| format!("{e:?}"))?)
            .map_err(|e| format!("{e}"))?;
    // INSTALL-SEAM-1: the same model the governed cells serve installs as the
    // projection of exactly the checked set.
    let model = lab.model().clone();
    let mut cell = MapCell::open(
        &dir.join("map.sqlite"),
        model.clone(),
        StoreOptions::default(),
        SCOPE,
        |_| BTreeMap::new(),
    )
    .map_err(|e| format!("open: {e}"))?;
    let set: Vec<&ArtifactEnvelope> = lab.candidate.iter().chain(&base).collect();
    cell.install_definitions(
        model,
        norn_cell::InstalledDefinitionInput {
            canonical_set: serde_json::to_string(&set).map_err(|e| format!("{e}"))?,
            registry: Vec::new(),
            receipt_sha256: norn_checker::admission_receipt_sha256(&receipt)
                .map_err(|e| format!("{e:?}"))?,
            admission_receipt: Some(receipt),
            readmission: None,
            diff: json!({"suite": "supply-lab"}),
            installed_at: 1,
            layout: None,
        },
        Some(0),
    )
    .map_err(|e| format!("install: {e}"))?;
    Ok(cell)
}

/// One raw source row as the connector would capture it.
fn raw(
    source: &str,
    event: &str,
    record: &str,
    payload: &str,
) -> Result<norn_kernel::Observed<RuntimeValue>, String> {
    let value = RuntimeValue::from_json_text(payload).map_err(|e| format!("{e:?}"))?;
    let digest = norn_ir::canonical::HashProfile::Norn1
        .hash_json(payload)
        .map_err(|e| format!("{e:?}"))?;
    Ok(norn_kernel::Observed {
        id: norn_kernel::ObservationId(event.to_owned()),
        provenance: norn_kernel::Provenance {
            source: source.to_owned(),
            source_id: record.to_owned(),
            mapping_version: "pipeline/1".to_owned(),
            observed_at: LogicalTime::from_millis(CLOCK),
        },
        input: norn_ir::runtime_object! {"contract"=>"norn.raw_observation/v1", "source"=>source,
        "source_id"=>record, "event_id"=>event, "source_version"=>"3", "pipeline_version"=>"pipeline/1",
        "kind"=>"upsert", "extracted_at"=>CLOCK, "payload"=>value, "payload_digest"=>digest},
    })
}

fn scan_row(scan: &str, qty: &str, uom: &str, at: &str) -> String {
    format!(
        r#"{{"scan_id":"{scan}","site":"WH-A","item":"SKU-WATER","qty":{qty},"uom":"{uom}","basis":"on_hand","counted_at":"{at}"}}"#
    )
}

fn input(mapping: &str, dataset: &str) -> norn_cell::mapping_bundle::MappingInput {
    norn_cell::mapping_bundle::MappingInput {
        mapping: mapping.to_owned(),
        dataset: dataset.to_owned(),
        connector_instance: "supply-lab".to_owned(),
        partition: "WH-A".to_owned(),
        recorded_at: norn_ir::provenance::UtcTimestamp("2026-10-26T00:00:00Z".to_owned()),
    }
}

/// Capture then process one row; `(fresh capture, processed)`.
fn feed(
    cell: &mut MapCell,
    observed: &norn_kernel::Observed<RuntimeValue>,
    input: &norn_cell::mapping_bundle::MappingInput,
) -> Result<(bool, bool), String> {
    let (bundle, fresh) = cell
        .inject_mapping_bundle(observed, input)
        .map_err(|e| format!("capture {}: {e}", observed.id.0))?;
    let processed = cell
        .process_mapping_bundle(&bundle)
        .map_err(|e| format!("process {}: {e}", observed.id.0))?;
    Ok((fresh, processed))
}

/// (law ids, open) of the findings recorded against one evidence object.
fn findings(cell: &MapCell, object_type: &str, key: &str) -> Result<Vec<(String, bool)>, String> {
    let found = cell
        .store()
        .read()
        .observation_findings("main", object_type, key)
        .map_err(|e| format!("findings: {e}"))?;
    found
        .iter()
        .map(|f| {
            let rule = serde_json::to_string(&f.rule_ref).map_err(|e| format!("{e}"))?;
            Ok((
                rule,
                matches!(
                    f.status,
                    norn_ir::data_quality::DataQualityFindingStatus::Open
                ),
            ))
        })
        .collect()
}

/// Capture and process one WMS scan row.
fn scan(
    cell: &mut MapCell,
    event: &str,
    scan: &str,
    qty: &str,
    uom: &str,
    at: &str,
) -> Result<(bool, bool), String> {
    let wms = input("supply.wms_scan.mapping", "stock_count");
    feed(
        cell,
        &raw("synthetic-wms", event, scan, &scan_row(scan, qty, uom, at))?,
        &wms,
    )
}

/// Active findings against one WMS scan, and whether one cites `law`.
fn open_scan_findings(cell: &MapCell, scan: &str, law: &str) -> Result<(usize, bool), String> {
    let open: Vec<String> = findings(cell, "supply.WmsCount", scan)?
        .into_iter()
        .filter(|(_, open)| *open)
        .map(|(rule, _)| rule)
        .collect();
    let cited = open.iter().any(|rule| rule.contains(law));
    Ok((open.len(), cited))
}

/// DATA-QUARANTINE: negative, unknown-unit and over-damaged rows become law-linked
/// findings, never refusals; replay adds no finding; a valid correction recovers.
#[test]
fn axis3_messy_feeds_quarantine_replay_stably_and_recover() -> Result<(), String> {
    let lab = lab()?;
    let dir = tempfile::tempdir().map_err(|e| format!("{e}"))?;
    let mut cell = mapping_cell(&lab, dir.path())?;
    scan(&mut cell, "e-1", "S-1", "100", "EA", "2026-07-07T08:00:00Z")?;
    assert_eq!(
        open_scan_findings(&cell, "S-1", "")?.0,
        0,
        "a clean scan has no finding"
    );
    scan(&mut cell, "e-2", "S-2", "-50", "EA", "2026-07-07T09:00:00Z")?;
    assert_eq!(
        open_scan_findings(&cell, "S-2", "on_hand_non_negative")?,
        (1, true),
        "negative quantity quarantined"
    );
    // An unknown unit: the `unit` enum quarantines the row (MAP-ENUM-TEXT-1).
    let codes = quarantined(
        &mut cell,
        "e-3",
        &scan_row("S-3", "12", "PALLET", "2026-07-07T10:00:00Z"),
    )?;
    assert_eq!(
        codes,
        ["mapping.runtime.evaluation_failed"],
        "unknown unit quarantined"
    );
    // Redelivery of the same event, then the same row under a new event id.
    let (fresh, processed) = scan(&mut cell, "e-2", "S-2", "-50", "EA", "2026-07-07T09:00:00Z")?;
    assert!(!fresh && !processed, "an exact redelivery is captured once");
    scan(
        &mut cell,
        "e-2b",
        "S-2",
        "-50",
        "EA",
        "2026-07-07T09:00:00Z",
    )?;
    assert_eq!(
        open_scan_findings(&cell, "S-2", "on_hand_non_negative")?,
        (1, true),
        "replay never duplicates an active finding"
    );
    // Out of order, then the valid correction.
    scan(&mut cell, "e-4", "S-1", "90", "EA", "2026-07-06T08:00:00Z")?;
    scan(&mut cell, "e-5", "S-2", "40", "EA", "2026-07-07T09:30:00Z")?;
    assert_eq!(
        open_scan_findings(&cell, "S-2", "")?.0,
        0,
        "the valid correction clears the active finding"
    );
    // Bounded literal-table lookup maps all three units, including 'L' -> 'l' without hitting fuel limits.
    scan(&mut cell, "e-11", "S-11", "15", "L", "2026-07-07T08:30:00Z")?;
    let litre_scan = cell
        .store()
        .read()
        .object("main", "supply.WmsCount", "S-11")
        .map_err(|e| format!("{e}"))?;
    assert_eq!(
        litre_scan
            .as_ref()
            .and_then(|o| o.values.get("unit"))
            .map(scalar)
            .as_deref(),
        Some("l"),
        "WMS unit 'L' maps cleanly to 'l'"
    );
    supplier_feed(&mut cell)?;
    cell.store()
        .read()
        .verify_ledger()
        .map(|_| ())
        .map_err(|e| format!("verify_ledger: {e}"))
}

/// MAP-ENUM-TEXT-1: the WMS feed sends `basis` as free text (no declared code
/// set) into the `quantity_basis` enum. The mapping admits; a code outside the
/// enum lands quarantined at commit, and a known code lands.
#[test]
fn axis3_unknown_enum_codes_land_in_quarantine() -> Result<(), String> {
    let lab = lab()?;
    let dir = tempfile::tempdir().map_err(|e| format!("{e}"))?;
    let mut cell = mapping_cell(&lab, dir.path())?;
    let row = r#"{"scan_id":"S-9","site":"WH-A","item":"SKU-WATER","qty":5,"uom":"EA","basis":"damaged","counted_at":"2026-07-07T08:00:00Z"}"#;
    assert_eq!(
        quarantined(&mut cell, "e-9", row)?,
        ["mapping.runtime.evaluation_failed"],
        "an unknown basis is quarantined by the mapping"
    );
    let landed = |cell: &MapCell, id: &str| {
        cell.store()
            .read()
            .object("main", "supply.WmsCount", id)
            .map(|found| found.is_some())
            .map_err(|e| format!("{e}"))
    };
    assert!(!landed(&cell, "S-9")?, "an unknown basis never lands");
    scan(&mut cell, "e-10", "S-10", "5", "EA", "2026-07-07T08:00:00Z")?;
    assert!(landed(&cell, "S-10")?, "a known basis lands");
    Ok(())
}

/// Capture one WMS row and process it as a sync does; the codes it is
/// quarantined for (empty when it landed).
fn quarantined(cell: &mut MapCell, event: &str, row: &str) -> Result<Vec<String>, String> {
    let scan_id = serde_json::from_str::<Value>(row)
        .map_err(|e| format!("{e}"))?
        .get("scan_id")
        .and_then(Value::as_str)
        .map(str::to_owned)
        .ok_or("no scan_id")?;
    let (bundle, fresh) = cell
        .inject_mapping_bundle(
            &raw("synthetic-wms", event, &scan_id, row)?,
            &input("supply.wms_scan.mapping", "stock_count"),
        )
        .map_err(|e| format!("capture: {e}"))?;
    assert!(fresh, "{event} is captured once");
    match cell
        .process_mapping_outcome(&bundle)
        .map_err(|e| format!("process: {e}"))?
    {
        norn_cell::MappingOutcome::Quarantined(reasons) => {
            Ok(reasons.into_iter().map(|reason| reason.code).collect())
        }
        norn_cell::MappingOutcome::Landed | norn_cell::MappingOutcome::AlreadyLanded => {
            Ok(Vec::new())
        }
    }
}

/// The ERP feed: vendor codes map to canonical suppliers; an over-damaged row is quarantined.
fn supplier_feed(cell: &mut MapCell) -> Result<(), String> {
    let erp = input("supply.erp_supplier.mapping", "Purchase Receipt Item");
    let row = |name: &str, supplier: &str, received: &str, rejected: &str| {
        format!(
            r#"{{"name":"{name}","parent":"PR-1","supplier":"{supplier}","purchase_order_item":"PO-1/1","received_qty":{received},"rejected_qty":{rejected},"uom":"Nos","posting_datetime":"2026-07-14T08:00:00Z"}}"#
        )
    };
    feed(
        cell,
        &raw(
            "synthetic-erp",
            "p-1",
            "R-1",
            &row("R-1", "NILE-001", "60", "5"),
        )?,
        &erp,
    )?;
    let mapped = cell
        .store()
        .read()
        .object("main", "supply.SupplierReceiptLine", "R-1")
        .map_err(|e| format!("{e}"))?;
    let supplier = mapped
        .as_ref()
        .and_then(|o| o.values.get("supplier_id"))
        .map(scalar);
    assert_eq!(
        supplier.as_deref(),
        Some("SUP-NILE"),
        "the vendor code maps onto the canonical supplier"
    );
    feed(
        cell,
        &raw(
            "synthetic-erp",
            "p-2",
            "R-2",
            &row("R-2", "DELTA-07", "10", "12"),
        )?,
        &erp,
    )?;
    let found = findings(cell, "supply.SupplierReceiptLine", "R-2")?;
    assert!(
        found
            .iter()
            .any(|(rule, open)| *open && rule.contains("damaged_within_received")),
        "{found:?}"
    );
    // Bounded literal-table lookup maps 'Litre' -> 'l' (the third branch that previously overflowed decide fuel).
    let litre_row = r#"{"name":"R-3","parent":"PR-2","supplier":"NILE-001","purchase_order_item":"PO-1/1","received_qty":25,"rejected_qty":0,"uom":"Litre","posting_datetime":"2026-07-15T08:00:00Z"}"#;
    feed(cell, &raw("synthetic-erp", "p-3", "R-3", litre_row)?, &erp)?;
    let mapped_litre = cell
        .store()
        .read()
        .object("main", "supply.SupplierReceiptLine", "R-3")
        .map_err(|e| format!("{e}"))?;
    assert_eq!(
        mapped_litre
            .as_ref()
            .and_then(|o| o.values.get("unit"))
            .map(scalar)
            .as_deref(),
        Some("l"),
        "ERP unit 'Litre' maps cleanly to 'l'"
    );
    Ok(())
}

// ------------------------------ axis 4: three engines (QUERY-ENGINE-1, QUERY-THREE)

/// Every supply-lab question on the exact cell, `DuckDB` (D136) and `DataFusion`
/// (D137) at the same frontier: an engine answers with the exact envelope
/// (only the engine label differs) or refuses as an unsupported engine shape;
/// the additive sums and the ratio of sums are answered by both.
#[cfg(all(feature = "duckdb", feature = "datafusion"))]
mod olap {
    use super::{CLOCK, SCOPE, Stress, WEEKS, context, plan, seeded, total};
    use norn_cell::analytics::{EngineRefusal, TenantId};
    use norn_cell::compute::{Engine as Label, QueryResult};
    use norn_checker::metric::compute::QuestionPlan;

    const ANSWERED: [&str; 3] = [
        "supply.movement_volume_weekly",
        "supply.received_weekly",
        "supply.fill_rate_weekly",
    ];
    const SHAPED: [&str; 2] = ["supply.on_hand_weekly", "supply.inspected_by_receipt_week"];

    fn refusal(error: &EngineRefusal) -> String {
        format!("{}: {}", error.code, error.detail)
    }

    /// One engine's answer to `plan`, or its refusal code.
    macro_rules! ask {
        ($engine:expr, $stress:expr, $plan:expr) => {{
            let store = $stress.cell.store();
            let tenant = TenantId::of_scope(SCOPE);
            let mut frontier = $engine
                .frontier(
                    &store
                        .analytics_frontier(&tenant, "main")
                        .map_err(|e| format!("frontier: {e}"))?,
                )
                .map_err(|e| refusal(&e))?
                .frontier;
            loop {
                let batch = store
                    .analytics_delta(&tenant, "main", frontier, 64)
                    .map_err(|e| format!("delta: {e}"))?;
                frontier = $engine
                    .sync(&batch.request)
                    .map_err(|e| refusal(&e))?
                    .frontier;
                if batch.complete {
                    break;
                }
            }
            let prepared = store
                .analytics_prepare($plan, context(CLOCK.saturating_add(10_000_000)), &tenant)
                .map_err(|e| format!("prepare: {e}"))?;
            match $engine.evaluate(prepared.request()) {
                Ok(response) => store
                    .analytics_answer(prepared, response)
                    .map(Ok)
                    .map_err(|e| format!("answer: {e}"))?,
                Err(refused) => Err(refused.code),
            }
        }};
    }

    /// QUERY-ENGINE-1: equal envelopes, or the engine's honest refusal of the shape.
    fn agree(
        name: &str,
        exact: &QueryResult,
        answered: Result<QueryResult, String>,
        label: Label,
    ) -> Result<bool, String> {
        match answered {
            Ok(mut answered) => {
                assert_eq!(answered.engine, label, "{name}");
                answered.engine = exact.engine;
                assert_eq!(&answered, exact, "{name} on {label:?}");
                Ok(true)
            }
            Err(code) if code == "core.compute.unsupported_engine" => Ok(false),
            Err(other) => Err(format!("{name} on {label:?}: {other}")),
        }
    }

    #[test]
    fn supply_lab_questions_agree_on_the_cell_duckdb_and_datafusion() -> Result<(), String> {
        let stress: Stress = seeded()?;
        let dir = tempfile::tempdir().map_err(|e| format!("{e}"))?;
        let duck =
            norn_cell::analytics::engine::Engine::new(norn_cell::analytics::engine::Settings {
                root: dir.path().join("duckdb"),
                memory_limit: "256MB".into(),
                threads: 1,
            })
            .map_err(|e| format!("duckdb: {e}"))?;
        let fusion = norn_cell::analytics::datafusion::Engine::new(
            norn_cell::analytics::datafusion::Settings {
                root: dir.path().join("datafusion"),
                memory_limit: "256MB".into(),
                threads: 1,
            },
        )
        .map_err(|e| format!("datafusion: {e}"))?;
        for name in ANSWERED.iter().chain(SHAPED.iter()) {
            let question: QuestionPlan = plan(&stress.lab, name)?;
            let exact = super::answer(&stress, &question)?;
            let by_duck = agree(name, &exact, ask!(duck, stress, &question), Label::Duckdb)?;
            let by_fusion = agree(
                name,
                &exact,
                ask!(fusion, stress, &question),
                Label::DataFusion,
            )?;
            if ANSWERED.contains(name) {
                assert!(by_duck && by_fusion, "{name}: both engines answer a sum");
            }
        }
        let volume = super::answer(
            &stress,
            &plan(&stress.lab, "supply.movement_volume_weekly")?,
        )?;
        assert_eq!(total(&volume, &[WEEKS[0], "each"]).as_deref(), Some("120"));
        Ok(())
    }
}
