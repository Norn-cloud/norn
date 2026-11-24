//! Seeded Deterministic Simulation Testing (DST) harness over Norn's four-verb
//! `CellHost` (`commit`, `perform`, `observe`, `clock`) and `SqlEngine` boundary
//! (Part of #1615, Related: #716).
//!
//! Evidence class: `sampled` and `runtime-enforced` seeded deterministic
//! simulation. Override the default seed batch with `NORN_DST_SEED=<u64>` to
//! replay any failure deterministically.
#![cfg(feature = "native")]

mod quantity_support;

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::{Debug, Display};
use std::path::{Path, PathBuf};

use norn_cell::cell_identity::CellId;
use norn_cell::engine::{
    DoEngine, DoStorageEmulator, EngineFault, EngineFaultKind, FaultEngine, FaultSwitch,
    NativeEngine, SqlEngine,
};
use norn_cell::{
    CellError, CellStore, CommitOutcome, LedgerHead, MAIN_WORLD, MarkingDeclassification, Model,
    PerformClaim, SqliteCell, StoreOptions,
};
use norn_ir::{ActionRoot, ObjectTypeDef};
use norn_kernel::cell_host::{JsonObject, JsonValue};
use norn_kernel::quantity::QuantityError;
use norn_kernel::{
    ActionAuthorizer, ActionAuthzRequest, ActionCall, ActionDenial, ActionOutcome,
    ActionReadAuthzRequest, ActionReadTypeAuthzRequest, ActionRefusal, CellCommit, CellHost,
    Command, CommandId, CommitError, CommitReceipt, Consumed, DecisionTurn, DueTimer, Effect,
    FactEvent, FactExpectation, FactKey, FactMutation, FactState, FactVersion, FactWrite,
    FreshnessPolicy, Generation, IdempotencyKey, LinkKey, LinkMutation, LinkReadExpectation,
    LinkState, LinkWrite, LogicalTime, ObservationId, Observed, OwnedRowsCommitProof, Performed,
    Provenance, Timer, TimerId, TypeCatalog, World, commit_action,
};
use serde::{Deserialize, Serialize};
use serde_json::json;

const SCOPE: &str = "org_dst/dev";
const ACCOUNT_TYPE: &str = "dst.Account";
const NOTE_TYPE: &str = "dst.Note";
const LINK_PARENT: &str = "parent";
const LINK_PEER: &str = "peer";
const SECRET_MARKING: &str = "pii.secret";

const MAIN_SEEDS: &[u64] = &[
    0x0001_6150_0001,
    0x0001_6150_0002,
    0x0001_6150_0003,
    0x0001_6150_0004,
];
const DO_SEEDS: &[u64] = &[0x0002_6150_0001, 0x0002_6150_0002, 0x0002_6150_0003];
const ACTION_SEEDS: &[u64] = &[0x0003_6150_0001, 0x0003_6150_0002, 0x0003_6150_0003];
const CONF_SEEDS: &[u64] = &[
    0x0004_0716_0001,
    0x0004_0716_0002,
    0x0004_0716_0003,
    0x0004_0716_0004,
];

type DstResult<T> = Result<T, String>;
type Dump = BTreeMap<String, Vec<String>>;
type DstCommit = CellCommit<DstEvent, DstCommand, DstInput>;
type DstCell<S> = SqliteCell<DstEvent, DstCommand, DstInput, S>;

// ------------------------------------------------------------------ PRNG & helpers

#[derive(Debug, Clone)]
struct SplitMix64 {
    state: u64,
}

impl SplitMix64 {
    const fn new(seed: u64) -> Self {
        Self { state: seed }
    }

    fn next_u64(&mut self) -> u64 {
        self.state = self.state.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.state;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    fn bound(&mut self, limit: u64) -> u64 {
        if limit <= 1 {
            0
        } else {
            self.next_u64().checked_rem(limit).unwrap_or(0)
        }
    }

    fn index(&mut self, limit: usize) -> usize {
        if limit <= 1 {
            0
        } else {
            let cap = u64::try_from(limit).unwrap_or(u64::MAX);
            usize::try_from(self.bound(cap)).unwrap_or(0)
        }
    }

    fn coin(&mut self, num: u64, den: u64) -> bool {
        self.bound(den) < num
    }
}

trait Ctx<T> {
    fn ctx(self, seed: u64, step: usize, what: &str) -> DstResult<T>;
}

impl<T, E: Display> Ctx<T> for Result<T, E> {
    fn ctx(self, seed: u64, step: usize, what: &str) -> DstResult<T> {
        self.map_err(|err| format!("NORN_DST_SEED={seed} step={step}: {what}: {err}"))
    }
}

struct SeedPlan {
    seeds: Vec<u64>,
    assert_batch_coverage: bool,
}

fn dst_seeds(defaults: &[u64]) -> DstResult<SeedPlan> {
    match std::env::var("NORN_DST_SEED") {
        Ok(raw) => raw
            .trim()
            .parse::<u64>()
            .map(|seed| SeedPlan {
                seeds: vec![seed],
                assert_batch_coverage: false,
            })
            .map_err(|err| format!("invalid NORN_DST_SEED={raw:?}: {err}")),
        Err(std::env::VarError::NotPresent) => Ok(SeedPlan {
            seeds: defaults.to_vec(),
            assert_batch_coverage: true,
        }),
        Err(std::env::VarError::NotUnicode(_)) => Err("NORN_DST_SEED is not UTF-8".to_owned()),
    }
}

fn deterministic_cell_id(seed: u64, step: usize, stream: u64) -> DstResult<CellId> {
    let mut rng = SplitMix64::new(seed ^ stream.wrapping_mul(0x9E37_79B9_7F4A_7C15));
    let hi = rng.next_u64();
    let lo = rng.next_u64();
    CellId::try_from(format!("{hi:016x}{lo:016x}"))
        .map_err(|err| fail(seed, step, format!("deterministic cell_id: {err}")))
}

fn assert_batch_counts(suite: &str, counters: &[(&str, u64)]) -> DstResult<()> {
    for &(name, count) in counters {
        if count == 0 {
            return Err(format!(
                "{suite}: default seed batch never exercised required branch `{name}`"
            ));
        }
    }
    Ok(())
}

fn fail(seed: u64, step: usize, msg: impl Display) -> String {
    format!("NORN_DST_SEED={seed} step={step}: {msg}")
}

fn ensure(seed: u64, step: usize, cond: bool, msg: impl Display) -> DstResult<()> {
    if cond {
        Ok(())
    } else {
        Err(fail(seed, step, msg))
    }
}

fn ensure_eq<T: PartialEq + Debug>(
    seed: u64,
    step: usize,
    label: &str,
    left: &T,
    right: &T,
) -> DstResult<()> {
    ensure(
        seed,
        step,
        left == right,
        format!("{label}: left={left:?}, right={right:?}"),
    )
}

fn simulator_dir(seed: u64, step: usize) -> DstResult<tempfile::TempDir> {
    let shm = Path::new("/dev/shm");
    if shm.is_dir()
        && let Ok(dir) = tempfile::tempdir_in(shm)
    {
        return Ok(dir);
    }
    tempfile::tempdir().ctx(seed, step, "tempdir")
}

fn dump_of<E, C, I, S: SqlEngine>(
    cell: &SqliteCell<E, C, I, S>,
    seed: u64,
    step: usize,
) -> DstResult<Dump> {
    cell.store().read().dump().ctx(seed, step, "dump")
}

fn head_of<E, C, I, S: SqlEngine>(
    cell: &SqliteCell<E, C, I, S>,
    seed: u64,
    step: usize,
) -> DstResult<LedgerHead> {
    cell.store()
        .verified_head()
        .ctx(seed, step, "verified_head")
}

// ------------------------------------------------------------------ domain model

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
enum DstEvent {
    AccountPut {
        key: String,
        balance: i64,
    },
    LinkMutated {
        source: String,
        target: String,
        added: bool,
    },
    CommandEnqueued {
        id: String,
        generation: u64,
    },
    CommandConsumed {
        id: String,
        generation: u64,
    },
    TimerConsumed {
        id: String,
        incarnation: u64,
    },
    ObservedConsumed {
        id: String,
    },
    NoteWritten {
        key: String,
        tag: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct DstCommand {
    op: String,
    account: String,
    amount: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
enum DstInput {
    CommandResult { ok: bool, amount: i64 },
    TimerTick { tag: u64 },
    ExternalObs { code: i64 },
}

fn dst_responder(cmd: &Command<DstCommand>) -> DstInput {
    DstInput::CommandResult {
        ok: cmd.payload.amount >= 0,
        amount: cmd.payload.amount,
    }
}

fn dst_object_types(seed: u64, step: usize) -> DstResult<Vec<ObjectTypeDef>> {
    let account: ObjectTypeDef = serde_json::from_value(json!({
        "name": ACCOUNT_TYPE,
        "identity": [{ "name": "primary", "properties": ["id"] }],
        "properties": [
            { "id": "id", "type": { "kind": "text" } },
            { "id": "balance", "type": { "kind": "integer" } },
            { "id": "status", "type": { "kind": "enum", "values": [
                { "value": "open" }, { "value": "locked" }
            ] }, "default": "open" },
            { "id": "secret_note", "type": { "kind": "text" }, "markings": [SECRET_MARKING] }
        ],
        "lifecycles": [{
            "property": "status", "initial": ["open"], "terminal": [],
            "transitions": [
                { "id": "lock", "from": ["open"], "to": "locked" },
                { "id": "unlock", "from": ["locked"], "to": "open" }
            ]
        }],
        "links": [
            { "id": LINK_PARENT, "target": ACCOUNT_TYPE, "cardinality": { "to": "one", "from": "many" } },
            { "id": LINK_PEER, "target": ACCOUNT_TYPE, "cardinality": { "to": "many", "from": "many" } }
        ]
    }))
    .ctx(seed, step, "account type")?;

    let note: ObjectTypeDef = serde_json::from_value(json!({
        "name": NOTE_TYPE,
        "identity": [{ "name": "primary", "properties": ["id"] }],
        "properties": [
            { "id": "id", "type": { "kind": "text" } },
            { "id": "public_tag", "type": { "kind": "text" } },
            { "id": "derived_note", "type": { "kind": "text" } }
        ]
    }))
    .ctx(seed, step, "note type")?;

    let mut fields: Vec<serde_json::Value> =
        ["actionId", "actor", "turn", "transition", "from", "to"]
            .iter()
            .map(|id| json!({"id": id, "type": {"kind": "text"}}))
            .collect();
    fields.extend(
        [
            "actionArtifactId",
            "actionVersion",
            "effectKind",
            "ruleBinding",
        ]
        .iter()
        .map(|id| json!({"id": id, "type": {"kind": "text"}, "constraints": {"nullable": true}})),
    );
    fields.extend(
        [
            "object",
            "input",
            "effectiveInput",
            "derivedValues",
            "preconditionOutcomes",
            "rowOperations",
        ]
        .iter()
        .map(
            |id| json!({"id": id, "type": {"kind": "json-ext"}, "constraints": {"nullable": true}}),
        ),
    );
    let action_call: ObjectTypeDef = serde_json::from_value(
        json!({"name": norn_kernel::ACTION_CALL_TYPE, "properties": fields}),
    )
    .ctx(seed, step, "action call type")?;
    Ok(vec![account, note, action_call])
}

fn dst_model(seed: u64, step: usize) -> DstResult<Model> {
    let defs = dst_object_types(seed, step)?;
    Model::from_definitions(&defs, &[]).ctx(seed, step, "model")
}

fn account_key(index: usize) -> FactKey {
    FactKey::new(ACCOUNT_TYPE, format!("acct-{index}"))
}

fn note_key(index: usize) -> FactKey {
    FactKey::new(NOTE_TYPE, format!("note-{index}"))
}

fn account_values(id: &str, balance: i64, status: &str, secret_note: &str) -> JsonObject {
    BTreeMap::from([
        ("id".to_owned(), JsonValue::String(id.to_owned())),
        ("balance".to_owned(), JsonValue::from(balance)),
        ("status".to_owned(), JsonValue::String(status.to_owned())),
        (
            "secret_note".to_owned(),
            JsonValue::String(secret_note.to_owned()),
        ),
    ])
}

fn note_values(id: &str, public_tag: &str, derived_note: &str) -> JsonObject {
    BTreeMap::from([
        ("id".to_owned(), JsonValue::String(id.to_owned())),
        (
            "public_tag".to_owned(),
            JsonValue::String(public_tag.to_owned()),
        ),
        (
            "derived_note".to_owned(),
            JsonValue::String(derived_note.to_owned()),
        ),
    ])
}

// ------------------------------------------------------------------ reference oracle

#[derive(Debug, Clone)]
struct PendingCmdRef {
    command: Command<DstCommand>,
    claimed: bool,
    recorded: Option<DstInput>,
}

#[derive(Debug, Clone)]
struct RefModel {
    head: LedgerHead,
    clock_ms: u64,
    executions: u64,
    facts: BTreeMap<FactKey, FactState>,
    links: BTreeMap<LinkKey, LinkState>,
    outbox: Vec<PendingCmdRef>,
    highest_gen: BTreeMap<CommandId, Generation>,
    timers: BTreeMap<TimerId, DueTimer<DstInput>>,
    seen_obs: BTreeSet<ObservationId>,
    pending_obs: Vec<Observed<DstInput>>,
    qty_keys: BTreeMap<
        String,
        (
            norn_ir::quantity_ledger::QuantityRequest,
            i128,
            norn_cell::Receipt,
        ),
    >,
}

impl RefModel {
    fn new() -> Self {
        Self {
            head: LedgerHead::genesis(),
            clock_ms: 0,
            executions: 0,
            facts: BTreeMap::new(),
            links: BTreeMap::new(),
            outbox: Vec::new(),
            highest_gen: BTreeMap::new(),
            timers: BTreeMap::new(),
            seen_obs: BTreeSet::new(),
            pending_obs: Vec::new(),
            qty_keys: BTreeMap::new(),
        }
    }

    fn fact(&self, key: &FactKey) -> FactState {
        self.facts.get(key).cloned().unwrap_or_default()
    }

    fn link(&self, key: &LinkKey) -> LinkState {
        self.links.get(key).cloned().unwrap_or_default()
    }

    fn apply_commit(&mut self, commit: &DstCommit, receipt: &CommitReceipt, head: LedgerHead) {
        self.head = head;
        if let Some(consumed) = &commit.consumes {
            match consumed {
                Consumed::Timer { id, incarnation } => {
                    if self
                        .timers
                        .get(id)
                        .is_some_and(|d| d.incarnation == *incarnation)
                    {
                        self.timers.remove(id);
                    }
                }
                Consumed::Result {
                    command,
                    generation,
                } => {
                    self.outbox.retain(|e| {
                        !(e.command.id == *command && e.command.generation == *generation)
                    });
                }
                Consumed::Observation { id } => self.pending_obs.retain(|obs| obs.id != *id),
            }
        }
        for (write, written) in commit.writes.iter().zip(receipt.written.iter()) {
            let values = match &write.mutation {
                FactMutation::Put { values } => Some(values.clone()),
                FactMutation::Delete => None,
            };
            self.facts.insert(
                written.key.clone(),
                FactState {
                    version: written.version,
                    values,
                },
            );
        }
        for (write, linked) in commit.links.iter().zip(receipt.linked.iter()) {
            let values = match &write.mutation {
                LinkMutation::Add { values } => Some(values.clone()),
                LinkMutation::Remove => None,
            };
            self.links.insert(
                linked.key.clone(),
                LinkState {
                    version: linked.version,
                    values,
                },
            );
        }
        self.apply_effects(&commit.effects, receipt.seq);
    }

    fn apply_effects(
        &mut self,
        effects: &[Effect<DstCommand, DstInput>],
        seq: norn_kernel::LedgerSeq,
    ) {
        for effect in effects {
            match effect {
                Effect::Perform(cmd) => {
                    if self
                        .highest_gen
                        .get(&cmd.id)
                        .is_some_and(|seen| cmd.generation <= *seen)
                    {
                        continue;
                    }
                    self.highest_gen.insert(cmd.id.clone(), cmd.generation);
                    if let Some(slot) = self.outbox.iter_mut().find(|e| e.command.id == cmd.id) {
                        slot.command = cmd.clone();
                        slot.claimed = false;
                        slot.recorded = None;
                    } else {
                        self.outbox.push(PendingCmdRef {
                            command: cmd.clone(),
                            claimed: false,
                            recorded: None,
                        });
                    }
                }
                Effect::Schedule(timer) => {
                    self.timers.insert(
                        timer.id.clone(),
                        DueTimer {
                            timer: timer.clone(),
                            incarnation: seq,
                        },
                    );
                }
                Effect::CancelTimer(id) => {
                    self.timers.remove(id);
                }
            }
        }
    }

    fn verify<S: SqlEngine>(
        &self,
        seed: u64,
        step: usize,
        cell: &DstCell<S>,
        accts: usize,
    ) -> DstResult<()> {
        let verified = cell
            .store()
            .read()
            .verify_ledger()
            .ctx(seed, step, "verify_ledger")?;
        let head = head_of(cell, seed, step)?;
        ensure_eq(
            seed,
            step,
            "verified_head vs verify_ledger",
            &head,
            &verified,
        )?;
        ensure_eq(seed, step, "ledger head matches model", &head, &self.head)?;
        let execs = cell.executions().ctx(seed, step, "executions")?;
        ensure_eq(seed, step, "executions count", &execs, &self.executions)?;
        for idx in 0..accts {
            let key = account_key(idx);
            let actual = cell.load(&key).ctx(seed, step, "load fact")?;
            ensure_eq(
                seed,
                step,
                "fact state equivalence",
                &actual,
                &self.fact(&key),
            )?;
        }
        for (link_key, expected) in &self.links {
            let actual = cell.load_link(link_key).ctx(seed, step, "load link")?;
            ensure_eq(seed, step, "link state equivalence", &actual, expected)?;
        }
        self.verify_queues(seed, step, cell)
    }

    fn verify_queues<S: SqlEngine>(
        &self,
        seed: u64,
        step: usize,
        cell: &DstCell<S>,
    ) -> DstResult<()> {
        let actual_cmds = cell
            .pending_commands()
            .ctx(seed, step, "pending_commands")?;
        let expected_cmds: Vec<_> = self.outbox.iter().map(|e| e.command.clone()).collect();
        ensure_eq(
            seed,
            step,
            "outbox equivalence",
            &actual_cmds,
            &expected_cmds,
        )?;
        let actual_timers = cell.pending_timers().ctx(seed, step, "pending_timers")?;
        let expected_timers: Vec<_> = self.timers.values().cloned().collect();
        ensure_eq(
            seed,
            step,
            "timers equivalence",
            &actual_timers,
            &expected_timers,
        )?;
        let actual_obs_rows =
            cell.store()
                .read()
                .pending_observations()
                .ctx(seed, step, "pending_observations")?;
        let mut actual_obs = Vec::with_capacity(actual_obs_rows.len());
        for row in actual_obs_rows {
            let decoded: Observed<DstInput> = norn_ir::runtime::decode(&row.observed).ctx(
                seed,
                step,
                "decode pending observation",
            )?;
            ensure_eq(
                seed,
                step,
                "pending observation id matches row",
                &row.observation_id,
                &decoded.id.0,
            )?;
            actual_obs.push(decoded);
        }
        ensure_eq(
            seed,
            step,
            "pending observations equivalence",
            &actual_obs,
            &self.pending_obs,
        )?;
        ensure(
            seed,
            step,
            self.pending_obs
                .iter()
                .all(|obs| self.seen_obs.contains(&obs.id)),
            "every pending observation must be tracked in seen_obs",
        )
    }
}

// ------------------------------------------------------------------ Part 1: Native CellHost DST

#[derive(Debug, Default, Clone)]
struct MainCoverage {
    object_conflict: u64,
    link_conflict: u64,
    link_read_conflict: u64,
    qty_stale_version: u64,
    qty_delayed_replay: u64,
    qty_idempotency_conflict: u64,
    outbox_claim_crash_unknown: u64,
    outbox_generation_supersession: u64,
    outbox_completed: u64,
    outbox_consumed: u64,
    timer_aba_reschedule_refusal: u64,
    observation_dedup_consume: u64,
    engine_fault_crash: u64,
    engine_fault_io: u64,
    sqlite_reopen: u64,
}

impl MainCoverage {
    fn merge(&mut self, other: &Self) {
        self.object_conflict = self.object_conflict.saturating_add(other.object_conflict);
        self.link_conflict = self.link_conflict.saturating_add(other.link_conflict);
        self.link_read_conflict = self
            .link_read_conflict
            .saturating_add(other.link_read_conflict);
        self.qty_stale_version = self
            .qty_stale_version
            .saturating_add(other.qty_stale_version);
        self.qty_delayed_replay = self
            .qty_delayed_replay
            .saturating_add(other.qty_delayed_replay);
        self.qty_idempotency_conflict = self
            .qty_idempotency_conflict
            .saturating_add(other.qty_idempotency_conflict);
        self.outbox_claim_crash_unknown = self
            .outbox_claim_crash_unknown
            .saturating_add(other.outbox_claim_crash_unknown);
        self.outbox_generation_supersession = self
            .outbox_generation_supersession
            .saturating_add(other.outbox_generation_supersession);
        self.outbox_completed = self.outbox_completed.saturating_add(other.outbox_completed);
        self.outbox_consumed = self.outbox_consumed.saturating_add(other.outbox_consumed);
        self.timer_aba_reschedule_refusal = self
            .timer_aba_reschedule_refusal
            .saturating_add(other.timer_aba_reschedule_refusal);
        self.observation_dedup_consume = self
            .observation_dedup_consume
            .saturating_add(other.observation_dedup_consume);
        self.engine_fault_crash = self
            .engine_fault_crash
            .saturating_add(other.engine_fault_crash);
        self.engine_fault_io = self.engine_fault_io.saturating_add(other.engine_fault_io);
        self.sqlite_reopen = self.sqlite_reopen.saturating_add(other.sqlite_reopen);
    }

    fn assert_covered(&self) -> DstResult<()> {
        assert_batch_counts(
            "dst_cell_host_adversarial_schedule_and_crash_recovery",
            &[
                ("object_conflict", self.object_conflict),
                ("link_conflict", self.link_conflict),
                ("link_read_conflict", self.link_read_conflict),
                ("qty_stale_version", self.qty_stale_version),
                ("qty_delayed_replay", self.qty_delayed_replay),
                ("qty_idempotency_conflict", self.qty_idempotency_conflict),
                (
                    "outbox_claim_crash_unknown",
                    self.outbox_claim_crash_unknown,
                ),
                (
                    "outbox_generation_supersession",
                    self.outbox_generation_supersession,
                ),
                ("outbox_completed", self.outbox_completed),
                ("outbox_consumed", self.outbox_consumed),
                (
                    "timer_aba_reschedule_refusal",
                    self.timer_aba_reschedule_refusal,
                ),
                ("observation_dedup_consume", self.observation_dedup_consume),
                ("engine_fault_crash", self.engine_fault_crash),
                ("engine_fault_io", self.engine_fault_io),
                ("sqlite_reopen", self.sqlite_reopen),
            ],
        )
    }
}

struct NativeDstHarness {
    seed: u64,
    rng: SplitMix64,
    _dir: tempfile::TempDir,
    path: PathBuf,
    faults: FaultSwitch,
    cell: DstCell<FaultEngine<NativeEngine>>,
    model: RefModel,
    accounts: usize,
    next_cmd: u64,
    next_obs: u64,
    next_qty: u64,
    stale_consumed: Vec<Consumed>,
    cov: MainCoverage,
}

impl NativeDstHarness {
    fn open(seed: u64, accounts: usize) -> DstResult<Self> {
        let dir = simulator_dir(seed, 0)?;
        let path = dir.path().join(format!("dst-{seed}.sqlite"));
        let faults = FaultSwitch::default();
        let cell = Self::open_cell(seed, 0, &path, &faults)?;
        let mut h = Self {
            seed,
            rng: SplitMix64::new(seed),
            _dir: dir,
            path,
            faults,
            cell,
            model: RefModel::new(),
            accounts,
            next_cmd: 1,
            next_obs: 1,
            next_qty: 1,
            stale_consumed: Vec::new(),
            cov: MainCoverage::default(),
        };
        h.bootstrap_quantity()?;
        Ok(h)
    }

    fn open_cell(
        seed: u64,
        step: usize,
        path: &Path,
        faults: &FaultSwitch,
    ) -> DstResult<DstCell<FaultEngine<NativeEngine>>> {
        let native = NativeEngine::open_unsynced(path, StoreOptions::default()).ctx(
            seed,
            step,
            "open_unsynced",
        )?;
        let cell_id = deterministic_cell_id(seed, step, 1)?;
        let store = CellStore::from_engine_with_identity_source(
            FaultEngine::new(native, faults.clone()),
            dst_model(seed, step)?,
            move |_| Ok(cell_id.clone()),
        )
        .ctx(seed, step, "store")?;
        SqliteCell::new(store, SCOPE, dst_responder).ctx(seed, step, "cell")
    }

    fn bootstrap_quantity(&mut self) -> DstResult<()> {
        let def = self
            .cell
            .commit_quantity(&quantity_support::define(), "qty-define")
            .ctx(self.seed, 0, "qty define")?;
        let mut fund = quantity_support::post(1, "qty-fund-0", "fund", 10_000);
        fund.balance_versions = self
            .cell
            .store()
            .read()
            .quantity_balance_versions(MAIN_WORLD, &fund)
            .ctx(self.seed, 0, "qty versions")?;
        let funded = self
            .cell
            .commit_quantity(&fund, "qty-fund")
            .ctx(self.seed, 0, "qty fund")?;
        ensure(
            self.seed,
            0,
            funded.receipt().body.commit_seq > def.receipt().body.commit_seq,
            "quantity bootstrap commit_seq must advance",
        )?;
        self.model.head = head_of(&self.cell, self.seed, 0)?;
        Ok(())
    }

    fn crash_and_reopen(&mut self, step: usize) -> DstResult<()> {
        let _fired = self.faults.disarm();
        let before_dump = dump_of(&self.cell, self.seed, step)?;
        let before_head = head_of(&self.cell, self.seed, step)?;
        let saved_now = self.cell.now();
        self.cell = Self::open_cell(self.seed, step, &self.path, &self.faults)?;
        self.cov.sqlite_reopen = self.cov.sqlite_reopen.saturating_add(1);
        ensure(
            self.seed,
            step,
            self.cell.now() <= saved_now,
            "reopened clock floor must not exceed prior logical clock",
        )?;
        self.cell.advance_to(saved_now);
        ensure_eq(
            self.seed,
            step,
            "reopen dump equivalence",
            &dump_of(&self.cell, self.seed, step)?,
            &before_dump,
        )?;
        ensure_eq(
            self.seed,
            step,
            "reopen head equivalence",
            &head_of(&self.cell, self.seed, step)?,
            &before_head,
        )?;
        self.model
            .verify(self.seed, step, &self.cell, self.accounts)
    }

    fn commit_ok(&mut self, step: usize, commit: &DstCommit) -> DstResult<()> {
        let before_seq = self.model.head.commits;
        let receipt = self
            .cell
            .commit(commit.clone())
            .ctx(self.seed, step, "commit")?;
        ensure_eq(
            self.seed,
            step,
            "commit_seq strictly increases by 1",
            &receipt.seq.0,
            &before_seq.saturating_add(1),
        )?;
        if let Some(consumed) = &commit.consumes {
            self.stale_consumed.push(consumed.clone());
        }
        let head = head_of(&self.cell, self.seed, step)?;
        self.model.apply_commit(commit, &receipt, head);
        Ok(())
    }

    fn commit_refused(
        &mut self,
        step: usize,
        commit: DstCommit,
        expected_label: &str,
        expected: impl FnOnce(&CommitError<CellError>) -> bool,
    ) -> DstResult<()> {
        let before_dump = dump_of(&self.cell, self.seed, step)?;
        let before_seq = self.model.head.commits;
        match self.cell.commit(commit) {
            Ok(receipt) => {
                return Err(fail(
                    self.seed,
                    step,
                    format!(
                        "expected {expected_label}, but commit succeeded at seq {}",
                        receipt.seq.0
                    ),
                ));
            }
            Err(err) => {
                ensure(
                    self.seed,
                    step,
                    expected(&err),
                    format!("expected {expected_label}, got {err:?}"),
                )?;
            }
        }
        ensure_eq(
            self.seed,
            step,
            "zero-effect refusal dump",
            &dump_of(&self.cell, self.seed, step)?,
            &before_dump,
        )?;
        ensure_eq(
            self.seed,
            step,
            "refusal leaves commit_seq unchanged",
            &head_of(&self.cell, self.seed, step)?.commits,
            &before_seq,
        )
    }

    fn step_account_contention(&mut self, step: usize) -> DstResult<()> {
        let idx = self.rng.index(self.accounts);
        let key = account_key(idx);
        let prev = self.model.fact(&key);
        let bal = i64::try_from(self.rng.bound(1_000)).unwrap_or(0);
        let commit = CellCommit {
            writes: vec![FactWrite {
                key: key.clone(),
                expected: prev.version,
                mutation: FactMutation::Put {
                    values: account_values(&key.key, bal, "open", &format!("sec-{idx}-{step}")),
                },
            }],
            events: vec![DstEvent::AccountPut {
                key: key.key.clone(),
                balance: bal,
            }],
            ..CellCommit::empty()
        };
        self.commit_ok(step, &commit)?;
        // Contended writer with pre-commit version must refuse with zero side effects.
        let stale = CellCommit {
            writes: vec![FactWrite {
                key: key.clone(),
                expected: prev.version,
                mutation: FactMutation::Put {
                    values: account_values(&key.key, bal.saturating_add(1), "open", "stale"),
                },
            }],
            events: vec![DstEvent::AccountPut {
                key: key.key,
                balance: bal.saturating_add(1),
            }],
            effects: vec![Effect::Schedule(Timer {
                id: TimerId(format!("refused-{step}")),
                due: LogicalTime::from_millis(self.model.clock_ms),
                payload: DstInput::TimerTick { tag: 999 },
            })],
            ..CellCommit::empty()
        };
        self.commit_refused(step, stale, "CommitError::Conflict", |err| {
            matches!(err, CommitError::Conflict { .. })
        })?;
        self.cov.object_conflict = self.cov.object_conflict.saturating_add(1);
        Ok(())
    }

    fn step_link_contention(&mut self, step: usize) -> DstResult<()> {
        let src = account_key(self.rng.index(self.accounts));
        let dst = account_key(self.rng.index(self.accounts));
        if !self.model.fact(&src).exists() || !self.model.fact(&dst).exists() {
            return self.step_account_contention(step);
        }
        let targets = self
            .cell
            .read_link_targets(&src, LINK_PEER, 32, 65_536)
            .ctx(self.seed, step, "read_link_targets")?
            .ok_or_else(|| fail(self.seed, step, "bounded link read unavailable"))?;
        let lk = LinkKey::new(src.clone(), LINK_PEER, dst.clone());
        let state = self.model.link(&lk);
        let adding = !state.exists();
        let mutation = if adding {
            LinkMutation::Add {
                values: JsonObject::new(),
            }
        } else {
            LinkMutation::Remove
        };
        self.commit_ok(
            step,
            &CellCommit {
                links: vec![LinkWrite {
                    key: lk.clone(),
                    expected: state.version,
                    mutation: mutation.clone(),
                }],
                link_reads: vec![LinkReadExpectation {
                    source: src.clone(),
                    link: LINK_PEER.to_owned(),
                    targets: targets.clone(),
                }],
                events: vec![DstEvent::LinkMutated {
                    source: src.key.clone(),
                    target: dst.key.clone(),
                    added: adding,
                }],
                ..CellCommit::empty()
            },
        )?;
        self.commit_refused(
            step,
            CellCommit {
                links: vec![LinkWrite {
                    key: lk,
                    expected: state.version,
                    mutation,
                }],
                events: vec![DstEvent::LinkMutated {
                    source: src.key.clone(),
                    target: dst.key,
                    added: adding,
                }],
                ..CellCommit::empty()
            },
            "CommitError::LinkConflict",
            |err| matches!(err, CommitError::LinkConflict { .. }),
        )?;
        self.cov.link_conflict = self.cov.link_conflict.saturating_add(1);
        self.commit_refused(
            step,
            CellCommit {
                link_reads: vec![LinkReadExpectation {
                    source: src,
                    link: LINK_PEER.to_owned(),
                    targets,
                }],
                events: vec![DstEvent::AccountPut {
                    key: "phantom".to_owned(),
                    balance: 0,
                }],
                ..CellCommit::empty()
            },
            "CommitError::LinkReadConflict",
            |err| matches!(err, CommitError::LinkReadConflict { .. }),
        )?;
        self.cov.link_read_conflict = self.cov.link_read_conflict.saturating_add(1);
        Ok(())
    }

    fn step_outbox(&mut self, step: usize) -> DstResult<()> {
        let Some(head) = self.model.outbox.first().cloned() else {
            let id = format!("cmd-{}", self.next_cmd);
            self.next_cmd = self.next_cmd.saturating_add(1);
            let cmd = Command {
                id: CommandId(id.clone()),
                generation: Generation(0),
                idempotency_key: IdempotencyKey(format!("{id}-g0")),
                freshness: FreshnessPolicy::AppendOnly,
                deadline: Some(LogicalTime::from_millis(
                    self.model.clock_ms.saturating_add(500),
                )),
                payload: DstCommand {
                    op: "settle".to_owned(),
                    account: format!("acct-{}", self.rng.index(self.accounts)),
                    amount: i64::try_from(self.rng.bound(500)).unwrap_or(10),
                },
            };
            return self.commit_ok(
                step,
                &CellCommit {
                    effects: vec![Effect::Perform(cmd)],
                    events: vec![DstEvent::CommandEnqueued { id, generation: 0 }],
                    ..CellCommit::empty()
                },
            );
        };
        if !head.claimed {
            return self.step_claim_outbox(step, &head);
        }
        if head.recorded.is_none() {
            return self.step_complete_or_retry_outbox(step, &head);
        }
        let delivered = self
            .cell
            .perform()
            .ctx(self.seed, step, "perform")?
            .ok_or_else(|| fail(self.seed, step, "recorded result missing"))?;
        let consumed = delivered.consumed();
        self.commit_ok(
            step,
            &CellCommit {
                consumes: Some(consumed.clone()),
                events: vec![DstEvent::CommandConsumed {
                    id: head.command.id.0,
                    generation: head.command.generation.0,
                }],
                ..CellCommit::empty()
            },
        )?;
        self.cov.outbox_consumed = self.cov.outbox_consumed.saturating_add(1);
        self.commit_refused(
            step,
            CellCommit::empty().consuming(consumed),
            "CommitError::NotPending",
            |err| matches!(err, CommitError::NotPending { .. }),
        )
    }

    fn step_claim_outbox(&mut self, step: usize, head: &PendingCmdRef) -> DstResult<()> {
        let claimed = self
            .cell
            .claim_perform()
            .ctx(self.seed, step, "claim_perform")?;
        ensure_eq(
            self.seed,
            step,
            "fresh perform claim",
            &claimed,
            &Some(PerformClaim::Fresh(head.command.clone())),
        )?;
        self.model.head = head_of(&self.cell, self.seed, step)?;
        if let Some(slot) = self.model.outbox.first_mut() {
            slot.claimed = true;
        }
        if self.rng.coin(1, 2) {
            self.crash_and_reopen(step)?;
            let after = self
                .cell
                .claim_perform()
                .ctx(self.seed, step, "claim after crash")?;
            ensure_eq(
                self.seed,
                step,
                "crash after claim surfaces OutcomeUnknown",
                &after,
                &Some(PerformClaim::OutcomeUnknown(head.command.clone())),
            )?;
            let err = self.cell.perform();
            ensure(
                self.seed,
                step,
                matches!(err, Err(CellError::PerformOutcomeUnknown { .. })),
                format!("perform must return PerformOutcomeUnknown, got {err:?}"),
            )?;
            self.cov.outbox_claim_crash_unknown =
                self.cov.outbox_claim_crash_unknown.saturating_add(1);
        }
        Ok(())
    }

    fn step_complete_or_retry_outbox(
        &mut self,
        step: usize,
        head: &PendingCmdRef,
    ) -> DstResult<()> {
        if head.command.generation.0 == 0 && self.rng.coin(1, 2) {
            let next_gen = Generation(head.command.generation.0.saturating_add(1));
            let retry = Command {
                id: head.command.id.clone(),
                generation: next_gen,
                idempotency_key: IdempotencyKey(format!("{}-g{}", head.command.id.0, next_gen.0)),
                freshness: FreshnessPolicy::AppendOnly,
                deadline: None,
                payload: head.command.payload.clone(),
            };
            self.commit_ok(
                step,
                &CellCommit {
                    effects: vec![Effect::Perform(retry)],
                    events: vec![DstEvent::CommandEnqueued {
                        id: head.command.id.0.clone(),
                        generation: next_gen.0,
                    }],
                    ..CellCommit::empty()
                },
            )?;
            let stale = self
                .cell
                .complete_perform(&head.command, dst_responder(&head.command))
                .ctx(self.seed, step, "stale complete")?;
            ensure_eq(
                self.seed,
                step,
                "stale generation completion ignored",
                &stale,
                &None,
            )?;
            self.cov.outbox_generation_supersession =
                self.cov.outbox_generation_supersession.saturating_add(1);
            return Ok(());
        }
        let ans = dst_responder(&head.command);
        let rec = self
            .cell
            .complete_perform(&head.command, ans.clone())
            .ctx(self.seed, step, "complete")?;
        ensure(
            self.seed,
            step,
            rec.is_some(),
            "claimed command must record",
        )?;
        self.model.executions = self.model.executions.saturating_add(1);
        if let Some(slot) = self.model.outbox.first_mut() {
            slot.recorded = Some(ans.clone());
        }
        let dup =
            self.cell
                .complete_perform(&head.command, ans)
                .ctx(self.seed, step, "dup complete")?;
        ensure_eq(self.seed, step, "duplicate completion ignored", &dup, &None)?;
        self.cov.outbox_completed = self.cov.outbox_completed.saturating_add(1);
        Ok(())
    }

    fn step_clock(&mut self, step: usize) -> DstResult<()> {
        let next_ms = self.model.clock_ms.saturating_add(self.rng.bound(250));
        self.cell.advance_to(LogicalTime::from_millis(next_ms));
        self.model.clock_ms = next_ms;
        self.cell
            .advance_to(LogicalTime::from_millis(next_ms.saturating_sub(50)));
        ensure_eq(
            self.seed,
            step,
            "monotonic clock",
            &self.cell.now().as_millis(),
            &self.model.clock_ms,
        )?;

        if let Some(due) = self.cell.clock().ctx(self.seed, step, "clock")? {
            if self.rng.coin(1, 3) {
                let rescheduled = Timer {
                    id: due.timer.id.clone(),
                    due: LogicalTime::from_millis(self.model.clock_ms.saturating_add(100)),
                    payload: DstInput::TimerTick {
                        tag: u64::try_from(step).unwrap_or(0),
                    },
                };
                self.commit_ok(
                    step,
                    &CellCommit {
                        effects: vec![Effect::Schedule(rescheduled)],
                        ..CellCommit::empty()
                    },
                )?;
                self.commit_refused(
                    step,
                    CellCommit::empty().consuming(due.consumed()),
                    "CommitError::NotPending",
                    |err| matches!(err, CommitError::NotPending { .. }),
                )?;
                self.cov.timer_aba_reschedule_refusal =
                    self.cov.timer_aba_reschedule_refusal.saturating_add(1);
                return Ok(());
            }
            return self.commit_ok(
                step,
                &CellCommit {
                    consumes: Some(due.consumed()),
                    events: vec![DstEvent::TimerConsumed {
                        id: due.timer.id.0,
                        incarnation: due.incarnation.0,
                    }],
                    ..CellCommit::empty()
                },
            );
        }
        let timer = Timer {
            id: TimerId(format!("t-{}", self.rng.index(3))),
            due: LogicalTime::from_millis(self.model.clock_ms.saturating_add(self.rng.bound(120))),
            payload: DstInput::TimerTick {
                tag: u64::try_from(step).unwrap_or(0),
            },
        };
        self.commit_ok(
            step,
            &CellCommit {
                effects: vec![Effect::Schedule(timer)],
                ..CellCommit::empty()
            },
        )
    }

    fn step_observe(&mut self, step: usize) -> DstResult<()> {
        if let Some(obs) = self.cell.observe().ctx(self.seed, step, "observe")? {
            ensure(
                self.seed,
                step,
                self.model.seen_obs.contains(&obs.id),
                "observed inbox row must be tracked in seen_obs",
            )?;
            ensure(
                self.seed,
                step,
                !self.cell.inject(&obs).ctx(self.seed, step, "dup inject")?,
                "pending obs deduplicates",
            )?;
            self.commit_ok(
                step,
                &CellCommit {
                    consumes: Some(obs.consumed()),
                    events: vec![DstEvent::ObservedConsumed {
                        id: obs.id.0.clone(),
                    }],
                    ..CellCommit::empty()
                },
            )?;
            ensure(
                self.seed,
                step,
                !self.cell.inject(&obs).ctx(self.seed, step, "post-dup")?,
                "consumed obs stays seen",
            )?;
            self.cov.observation_dedup_consume =
                self.cov.observation_dedup_consume.saturating_add(1);
            return Ok(());
        }
        let id = ObservationId(format!("obs-{}", self.next_obs));
        self.next_obs = self.next_obs.saturating_add(1);
        ensure(
            self.seed,
            step,
            !self.model.seen_obs.contains(&id),
            "fresh obs id must not already be in seen_obs",
        )?;
        let obs = Observed {
            id: id.clone(),
            provenance: Provenance {
                source: "dst-connector".to_owned(),
                source_id: format!("src-{}", id.0),
                observed_at: self.cell.now(),
                mapping_version: "1.0.0".to_owned(),
            },
            input: DstInput::ExternalObs {
                code: i64::try_from(step).unwrap_or(0),
            },
        };
        ensure(
            self.seed,
            step,
            self.cell.inject(&obs).ctx(self.seed, step, "inject")?,
            "fresh obs accepted",
        )?;
        self.model.seen_obs.insert(id);
        self.model.pending_obs.push(obs);
        Ok(())
    }

    fn step_quantity_idempotency(&mut self, step: usize) -> DstResult<()> {
        if !self.model.qty_keys.is_empty() && self.rng.coin(1, 2) {
            let pick = self.rng.index(self.model.qty_keys.len());
            if let Some((key, (orig_req, amt, orig_receipt))) = self.model.qty_keys.iter().nth(pick)
            {
                let (key, orig_req, amt, orig_receipt) =
                    (key.clone(), orig_req.clone(), *amt, orig_receipt.clone());
                let before = dump_of(&self.cell, self.seed, step)?;
                // Delayed exact replay must still return the original receipt with zero writes.
                let replayed = self.cell.commit_quantity(&orig_req, &key).ctx(
                    self.seed,
                    step,
                    "delayed qty replay",
                )?;
                ensure(
                    self.seed,
                    step,
                    matches!(replayed, CommitOutcome::Replayed(_)),
                    "delayed exact qty retry must replay",
                )?;
                ensure_eq(
                    self.seed,
                    step,
                    "delayed replayed qty receipt",
                    replayed.receipt(),
                    &orig_receipt,
                )?;
                self.cov.qty_delayed_replay = self.cov.qty_delayed_replay.saturating_add(1);
                // Mutated payload under the same idempotency key must refuse IdempotencyConflict.
                let mut mutated = quantity_support::post(
                    1,
                    &format!("{key}-mut-{step}"),
                    "ship",
                    amt.saturating_add(1),
                );
                mutated.balance_versions = orig_req.balance_versions;
                let res = self.cell.commit_quantity(&mutated, &key);
                ensure(
                    self.seed,
                    step,
                    matches!(res, Err(CellError::IdempotencyConflict(_))),
                    format!("mutated qty key must conflict, got {res:?}"),
                )?;
                self.cov.qty_idempotency_conflict =
                    self.cov.qty_idempotency_conflict.saturating_add(1);
                return ensure_eq(
                    self.seed,
                    step,
                    "qty replay+conflict zero effect",
                    &dump_of(&self.cell, self.seed, step)?,
                    &before,
                );
            }
        }
        self.step_quantity_commit_and_replay(step)
    }

    fn step_quantity_commit_and_replay(&mut self, step: usize) -> DstResult<()> {
        let key = format!("qty-ship-{}", self.next_qty);
        self.next_qty = self.next_qty.saturating_add(1);
        let amt = i128::from(self.rng.bound(5).saturating_add(1));
        let mut req = quantity_support::post(1, &key, "ship", amt);
        req.balance_versions = self
            .cell
            .store()
            .read()
            .quantity_balance_versions(MAIN_WORLD, &req)
            .ctx(self.seed, step, "qty versions")?;
        if let Some(first_ver) = req.balance_versions.first() {
            let mut stale = req.clone();
            if let Some(entry) = stale.balance_versions.first_mut() {
                entry.version = first_ver.version.saturating_add(99);
            }
            let before = dump_of(&self.cell, self.seed, step)?;
            let stale_res = self.cell.commit_quantity(&stale, &format!("{key}-stale"));
            ensure(
                self.seed,
                step,
                matches!(
                    stale_res,
                    Err(CellError::Quantity(QuantityError::StaleVersion { .. }))
                ),
                format!("stale balance_versions must refuse StaleVersion, got {stale_res:?}"),
            )?;
            ensure_eq(
                self.seed,
                step,
                "stale qty zero effect",
                &dump_of(&self.cell, self.seed, step)?,
                &before,
            )?;
            self.cov.qty_stale_version = self.cov.qty_stale_version.saturating_add(1);
        }
        let committed =
            self.cell
                .commit_quantity(&req, &key)
                .ctx(self.seed, step, "commit_quantity")?;
        let orig_receipt = match &committed {
            CommitOutcome::Committed(r) => r.clone(),
            CommitOutcome::Replayed(_) => {
                return Err(fail(self.seed, step, "fresh qty key replayed"));
            }
        };
        self.model.head = head_of(&self.cell, self.seed, step)?;
        self.model
            .qty_keys
            .insert(key.clone(), (req.clone(), amt, orig_receipt));
        let before_replay = dump_of(&self.cell, self.seed, step)?;
        let replayed = self
            .cell
            .commit_quantity(&req, &key)
            .ctx(self.seed, step, "replay qty")?;
        ensure(
            self.seed,
            step,
            matches!(replayed, CommitOutcome::Replayed(_)),
            "exact qty retry replays",
        )?;
        ensure_eq(
            self.seed,
            step,
            "replayed qty receipt",
            replayed.receipt(),
            committed.receipt(),
        )?;
        ensure_eq(
            self.seed,
            step,
            "qty replay zero diff",
            &dump_of(&self.cell, self.seed, step)?,
            &before_replay,
        )
    }

    fn step_fault_injected_commit(&mut self, step: usize) -> DstResult<()> {
        let before_dump = dump_of(&self.cell, self.seed, step)?;
        let before_head = head_of(&self.cell, self.seed, step)?;
        let key = account_key(self.rng.index(self.accounts));
        let state = self.model.fact(&key);
        let kind = if self.rng.coin(1, 2) {
            EngineFaultKind::Crash
        } else {
            EngineFaultKind::Io
        };
        self.faults.arm(EngineFault {
            kind,
            transaction: 0,
            at_statement: u32::try_from(self.rng.bound(7)).unwrap_or(0),
        });
        let commit = CellCommit {
            writes: vec![FactWrite {
                key: key.clone(),
                expected: state.version,
                mutation: FactMutation::Put {
                    values: account_values(&key.key, 777, "open", "faulted"),
                },
            }],
            events: vec![DstEvent::AccountPut {
                key: key.key,
                balance: 777,
            }],
            effects: vec![Effect::Schedule(Timer {
                id: TimerId(format!("fault-{step}")),
                due: LogicalTime::from_millis(self.model.clock_ms),
                payload: DstInput::TimerTick { tag: 0 },
            })],
            ..CellCommit::empty()
        };
        let res = self.cell.commit(commit);
        let fired = self.faults.disarm();
        ensure(
            self.seed,
            step,
            fired && res.is_err(),
            "armed engine fault must fire and fail",
        )?;
        if kind == EngineFaultKind::Io {
            self.cov.engine_fault_io = self.cov.engine_fault_io.saturating_add(1);
            ensure_eq(
                self.seed,
                step,
                "io fault live rollback dump",
                &dump_of(&self.cell, self.seed, step)?,
                &before_dump,
            )?;
            ensure_eq(
                self.seed,
                step,
                "io fault live rollback head",
                &head_of(&self.cell, self.seed, step)?,
                &before_head,
            )?;
            self.model
                .verify(self.seed, step, &self.cell, self.accounts)?;
        } else {
            self.cov.engine_fault_crash = self.cov.engine_fault_crash.saturating_add(1);
        }
        let saved_now = self.cell.now();
        self.cell = Self::open_cell(self.seed, step, &self.path, &self.faults)?;
        self.cov.sqlite_reopen = self.cov.sqlite_reopen.saturating_add(1);
        self.cell.advance_to(saved_now);
        ensure_eq(
            self.seed,
            step,
            "fault rollback dump",
            &dump_of(&self.cell, self.seed, step)?,
            &before_dump,
        )?;
        ensure_eq(
            self.seed,
            step,
            "fault rollback head",
            &head_of(&self.cell, self.seed, step)?,
            &before_head,
        )
    }

    fn run_steps(&mut self, steps: usize) -> DstResult<MainCoverage> {
        for step in 1..=steps {
            let roll = self.rng.bound(100);
            if roll < 19 {
                self.step_account_contention(step)?;
            } else if roll < 33 {
                self.step_link_contention(step)?;
            } else if roll < 56 {
                self.step_outbox(step)?;
            } else if roll < 70 {
                self.step_clock(step)?;
            } else if roll < 80 {
                self.step_observe(step)?;
            } else if roll < 90 {
                self.step_quantity_idempotency(step)?;
            } else {
                self.step_fault_injected_commit(step)?;
            }
            if !self.stale_consumed.is_empty() && self.rng.coin(1, 5) {
                let pick = self.rng.index(self.stale_consumed.len());
                if let Some(stale) = self.stale_consumed.get(pick).cloned() {
                    self.commit_refused(
                        step,
                        CellCommit::empty().consuming(stale),
                        "CommitError::NotPending",
                        |err| matches!(err, CommitError::NotPending { .. }),
                    )?;
                }
            }
            if self.rng.coin(1, 7) {
                self.crash_and_reopen(step)?;
            }
            self.model
                .verify(self.seed, step, &self.cell, self.accounts)?;
        }
        Ok(self.cov.clone())
    }
}

// ------------------------------------------------------------------ Part 2: DO vs Native equivalence

#[derive(Debug, Default, Clone)]
struct DoCoverage {
    commits: u64,
    stale_refusals: u64,
    commit_faults: u64,
    reopens: u64,
}

impl DoCoverage {
    fn merge(&mut self, other: &Self) {
        self.commits = self.commits.saturating_add(other.commits);
        self.stale_refusals = self.stale_refusals.saturating_add(other.stale_refusals);
        self.commit_faults = self.commit_faults.saturating_add(other.commit_faults);
        self.reopens = self.reopens.saturating_add(other.reopens);
    }

    fn assert_covered(&self) -> DstResult<()> {
        assert_batch_counts(
            "dst_do_engine_commit_fault_and_native_equivalence",
            &[
                ("commits", self.commits),
                ("stale_refusals", self.stale_refusals),
                ("commit_faults", self.commit_faults),
                ("reopens", self.reopens),
            ],
        )
    }
}

fn open_native_cell(
    seed: u64,
    step: usize,
    stream: u64,
    path: &Path,
) -> DstResult<DstCell<NativeEngine>> {
    let eng = NativeEngine::open_unsynced(path, StoreOptions::default()).ctx(seed, step, "open")?;
    let cell_id = deterministic_cell_id(seed, step, stream)?;
    let store =
        CellStore::from_engine_with_identity_source(eng, dst_model(seed, step)?, move |_| {
            Ok(cell_id.clone())
        })
        .ctx(seed, step, "store")?;
    SqliteCell::new(store, SCOPE, dst_responder).ctx(seed, step, "cell")
}

fn open_do_cell(
    seed: u64,
    step: usize,
    stream: u64,
    path: &Path,
    faults: &FaultSwitch,
) -> DstResult<DstCell<FaultEngine<DoEngine<DoStorageEmulator>>>> {
    let eng = NativeEngine::open_unsynced(path, StoreOptions::default()).ctx(seed, step, "open")?;
    let cell_id = deterministic_cell_id(seed, step, stream)?;
    let storage = DoStorageEmulator::new(eng).with_faults(faults.clone());
    let store = CellStore::from_engine_with_identity_source(
        FaultEngine::new(DoEngine::new(storage), faults.clone()),
        dst_model(seed, step)?,
        move |_| Ok(cell_id.clone()),
    )
    .ctx(seed, step, "do store")?;
    SqliteCell::new(store, SCOPE, dst_responder).ctx(seed, step, "do cell")
}

fn assert_native_do_eq(
    seed: u64,
    step: usize,
    phase: &str,
    native_cell: &DstCell<NativeEngine>,
    do_cell: &DstCell<FaultEngine<DoEngine<DoStorageEmulator>>>,
) -> DstResult<()> {
    ensure_eq(
        seed,
        step,
        &format!("Native vs DO head ({phase})"),
        &head_of(native_cell, seed, step)?,
        &head_of(do_cell, seed, step)?,
    )?;
    ensure_eq(
        seed,
        step,
        &format!("Native vs DO full dump ({phase})"),
        &dump_of(native_cell, seed, step)?,
        &dump_of(do_cell, seed, step)?,
    )
}

fn run_do_vs_native_seed(seed: u64, steps: usize) -> DstResult<DoCoverage> {
    let mut rng = SplitMix64::new(seed);
    let mut cov = DoCoverage::default();
    let dir = simulator_dir(seed, 0)?;
    let (native_path, do_path) = (
        dir.path().join("native.sqlite"),
        dir.path().join("do.sqlite"),
    );
    let faults = FaultSwitch::default();
    let mut native_cell = open_native_cell(seed, 0, 2, &native_path)?;
    let mut do_cell = open_do_cell(seed, 0, 2, &do_path, &faults)?;
    assert_native_do_eq(seed, 0, "initial", &native_cell, &do_cell)?;

    for step in 1..=steps {
        let key = account_key(rng.index(3));
        let cur = native_cell.load(&key).ctx(seed, step, "load")?;
        let stale = rng.coin(1, 4);
        let expected = if stale {
            FactVersion(cur.version.0.saturating_add(7))
        } else {
            cur.version
        };
        let bal = i64::try_from(rng.bound(500)).unwrap_or(0);
        let commit = CellCommit {
            writes: vec![FactWrite {
                key: key.clone(),
                expected,
                mutation: FactMutation::Put {
                    values: account_values(&key.key, bal, "open", "same"),
                },
            }],
            events: vec![DstEvent::AccountPut {
                key: key.key,
                balance: bal,
            }],
            ..CellCommit::empty()
        };
        if !stale && rng.coin(1, 5) {
            let before = dump_of(&do_cell, seed, step)?;
            faults.arm(EngineFault {
                kind: EngineFaultKind::Commit,
                transaction: 0,
                at_statement: 0,
            });
            let faulted = do_cell.commit(commit.clone());
            ensure(
                seed,
                step,
                faults.disarm() && faulted.is_err(),
                "DO commit fault must fail",
            )?;
            ensure_eq(
                seed,
                step,
                "DO commit fault rollback",
                &dump_of(&do_cell, seed, step)?,
                &before,
            )?;
            assert_native_do_eq(seed, step, "after DO commit fault", &native_cell, &do_cell)?;
            cov.commit_faults = cov.commit_faults.saturating_add(1);
        }
        let r_nat = native_cell.commit(commit.clone()).map_err(|err| err.code());
        let r_do = do_cell.commit(commit).map_err(|err| err.code());
        ensure_eq(seed, step, "Native vs DO commit outcome", &r_nat, &r_do)?;
        if r_nat.is_ok() {
            cov.commits = cov.commits.saturating_add(1);
        } else {
            cov.stale_refusals = cov.stale_refusals.saturating_add(1);
        }
        assert_native_do_eq(seed, step, "after commit", &native_cell, &do_cell)?;
        if rng.coin(1, 5) {
            native_cell = open_native_cell(seed, step, 2, &native_path)?;
            do_cell = open_do_cell(seed, step, 2, &do_path, &faults)?;
            cov.reopens = cov.reopens.saturating_add(1);
            assert_native_do_eq(seed, step, "after reopen", &native_cell, &do_cell)?;
        }
    }
    Ok(cov)
}

// ------------------------------------------------------------------ Part 3: ActionCommit DST

#[derive(Debug, Default, Clone)]
struct ActionCoverage {
    commits: u64,
    stale_precondition_reads: u64,
    replays: u64,
    idempotency_conflicts: u64,
    reopens: u64,
}

impl ActionCoverage {
    fn merge(&mut self, other: &Self) {
        self.commits = self.commits.saturating_add(other.commits);
        self.stale_precondition_reads = self
            .stale_precondition_reads
            .saturating_add(other.stale_precondition_reads);
        self.replays = self.replays.saturating_add(other.replays);
        self.idempotency_conflicts = self
            .idempotency_conflicts
            .saturating_add(other.idempotency_conflicts);
        self.reopens = self.reopens.saturating_add(other.reopens);
    }

    fn assert_covered(&self) -> DstResult<()> {
        assert_batch_counts(
            "dst_action_commit_idempotency_and_stale_precondition",
            &[
                ("commits", self.commits),
                ("stale_precondition_reads", self.stale_precondition_reads),
                ("replays", self.replays),
                ("idempotency_conflicts", self.idempotency_conflicts),
                ("reopens", self.reopens),
            ],
        )
    }
}

struct AllowAll;

impl ActionAuthorizer for AllowAll {
    fn authorize(&self, _req: &ActionAuthzRequest<'_>) -> Result<(), ActionDenial> {
        Ok(())
    }
    fn authorize_read(&self, _req: &ActionReadAuthzRequest<'_>) -> Result<(), ActionDenial> {
        Ok(())
    }
    fn authorize_read_type(
        &self,
        _req: &ActionReadTypeAuthzRequest<'_>,
    ) -> Result<(), ActionDenial> {
        Ok(())
    }
}

fn guarded_toggle_action(seed: u64, step: usize, transition: &str) -> DstResult<ActionRoot> {
    serde_json::from_value(json!({
        "actionId": format!("dst.account.{transition}"),
        "title": format!("Account {transition}"),
        "scopeRequired": ["orgId", "environmentId"],
        "idempotent": true,
        "ledgered": true,
        "inputSchema": {
            "type": "object",
            "properties": { "id": { "type": "string" }, "guard_id": { "type": "string" } },
            "required": ["id", "guard_id"],
            "additionalProperties": false
        },
        "outputSchema": { "type": "object", "properties": {}, "additionalProperties": false },
        "slice": [{
            "name": "guard",
            "objectType": ACCOUNT_TYPE,
            "key": { "property": { "from": "input", "property": "guard_id" } }
        }],
        "preconditions": ["slice.guard.balance >= 0"],
        "writes": [{ "entityKind": ACCOUNT_TYPE, "change": "modified" }],
        "effectProgram": {
            "kind": "fire_transition",
            "objectIdField": "id",
            "objectKind": ACCOUNT_TYPE,
            "entityArtifactId": "dst.Account.entity",
            "entityArtifactVersion": "1.0.0",
            "lifecycle": "status",
            "transition": transition
        }
    }))
    .ctx(seed, step, "toggle action")
}

struct PreconditionRaceHost<'a> {
    cell: &'a mut SqliteCell<FactEvent, JsonObject, JsonObject>,
    interleave_guard: Option<FactKey>,
    fired: bool,
}

impl PreconditionRaceHost<'_> {
    fn maybe_race(&mut self) -> Result<(), CellError> {
        if self.fired {
            return Ok(());
        }
        let Some(guard_key) = self.interleave_guard.take() else {
            return Ok(());
        };
        self.fired = true;
        let current = self.cell.load(&guard_key)?;
        let Some(mut vals) = current.values else {
            return Ok(());
        };
        let bal = vals
            .get("balance")
            .and_then(JsonValue::as_i64)
            .unwrap_or(0)
            .saturating_add(1);
        vals.insert("balance".to_owned(), JsonValue::from(bal));
        let race = CellCommit {
            writes: vec![FactWrite {
                key: guard_key,
                expected: current.version,
                mutation: FactMutation::Put { values: vals },
            }],
            ..CellCommit::empty()
        };
        let _receipt = self
            .cell
            .commit(race)
            .map_err(|err| CellError::Engine(err.to_string()))?;
        Ok(())
    }
}

impl CellHost for PreconditionRaceHost<'_> {
    type Event = FactEvent;
    type Command = JsonObject;
    type Input = JsonObject;
    type Error = CellError;

    fn load(&self, key: &FactKey) -> Result<FactState, CellError> {
        self.cell.load(key)
    }
    fn supports_reference_reads(&self) -> bool {
        self.cell.supports_reference_reads()
    }
    fn load_reference(
        &self,
        key: &FactKey,
        max: u64,
    ) -> Result<Option<norn_kernel::BoundedFact>, CellError> {
        self.cell.load_reference(key, max)
    }
    fn load_bounded(
        &self,
        key: &FactKey,
        max: u64,
    ) -> Result<Option<norn_kernel::BoundedFact>, CellError> {
        self.cell.load_bounded(key, max)
    }
    fn load_link(&self, key: &LinkKey) -> Result<LinkState, CellError> {
        self.cell.load_link(key)
    }
    fn commit(
        &mut self,
        c: CellCommit<FactEvent, JsonObject, JsonObject>,
    ) -> Result<CommitReceipt, CommitError<CellError>> {
        self.maybe_race().map_err(CommitError::Host)?;
        self.cell.commit(c)
    }
    fn commit_action_references(
        &mut self,
        c: CellCommit<FactEvent, JsonObject, JsonObject>,
        proof: norn_kernel::action_commit::ActionReferenceReads,
        qty: Option<(&norn_ir::quantity_ledger::QuantityRequest, &str)>,
    ) -> Result<CommitReceipt, CommitError<CellError>> {
        self.maybe_race().map_err(CommitError::Host)?;
        self.cell.commit_action_references(c, proof, qty)
    }
    fn commit_owned_rows(
        &mut self,
        c: CellCommit<FactEvent, JsonObject, JsonObject>,
        proof: OwnedRowsCommitProof,
    ) -> Result<CommitReceipt, CommitError<CellError>> {
        self.cell.commit_owned_rows(c, proof)
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
}

fn action_call<'a>(
    action: &'a ActionRoot,
    id: &str,
    guard_id: &str,
    actor: &str,
    idem: &str,
) -> ActionCall<'a> {
    ActionCall {
        action,
        action_artifact_id: "dst.account.action",
        action_version: "1.0.0",
        scope: BTreeMap::from([
            ("orgId".to_owned(), "org_dst".to_owned()),
            ("environmentId".to_owned(), "dev".to_owned()),
        ]),
        actor: actor.to_owned(),
        accountable_person: None,
        approved_intent: None,
        input: BTreeMap::from([
            ("id".to_owned(), JsonValue::String(id.to_owned())),
            (
                "guard_id".to_owned(),
                JsonValue::String(guard_id.to_owned()),
            ),
        ]),
        idempotency_key: idem.to_owned(),
        object_type: None,
        turn: DecisionTurn {
            id: idem.to_owned(),
            now: LogicalTime::from_millis(0),
            world: World::Main,
            consumes: None,
        },
        trace: None,
    }
}

fn open_action_cell(
    seed: u64,
    step: usize,
    path: &Path,
) -> DstResult<SqliteCell<FactEvent, JsonObject, JsonObject>> {
    let eng = NativeEngine::open_unsynced(path, StoreOptions::default()).ctx(seed, step, "open")?;
    let cell_id = deterministic_cell_id(seed, step, 3)?;
    let store =
        CellStore::from_engine_with_identity_source(eng, dst_model(seed, step)?, move |_| {
            Ok(cell_id.clone())
        })
        .ctx(seed, step, "store")?;
    SqliteCell::new(store, SCOPE, |cmd: &Command<JsonObject>| {
        cmd.payload.clone()
    })
    .ctx(seed, step, "cell")
}

struct ActionDstHarness {
    seed: u64,
    rng: SplitMix64,
    _dir: tempfile::TempDir,
    path: PathBuf,
    cell: SqliteCell<FactEvent, JsonObject, JsonObject>,
    committed: Vec<(bool, String, String, String)>,
    next_key: u64,
    cov: ActionCoverage,
}

impl ActionDstHarness {
    fn open(seed: u64) -> DstResult<Self> {
        let dir = simulator_dir(seed, 0)?;
        let path = dir.path().join("action-dst.sqlite");
        let mut cell = open_action_cell(seed, 0, &path)?;
        let writes = (0..4)
            .map(|i| {
                let key = account_key(i);
                FactWrite {
                    key: key.clone(),
                    expected: FactVersion::ABSENT,
                    mutation: FactMutation::Put {
                        values: account_values(&key.key, 100, "open", "seed"),
                    },
                }
            })
            .collect();
        cell.commit(CellCommit {
            writes,
            ..CellCommit::empty()
        })
        .ctx(seed, 0, "seed")?;
        Ok(Self {
            seed,
            rng: SplitMix64::new(seed),
            _dir: dir,
            path,
            cell,
            committed: Vec::new(),
            next_key: 1,
            cov: ActionCoverage::default(),
        })
    }

    fn verify_replay_and_conflict(
        &mut self,
        step: usize,
        catalog: &TypeCatalog<'_>,
        act: &ActionRoot,
        entry: &(String, String, String),
    ) -> DstResult<()> {
        let (target_id, actor, idem) = entry;
        let before_dump = dump_of(&self.cell, self.seed, step)?;
        let before_head = head_of(&self.cell, self.seed, step)?;
        let replay_req = action_call(act, target_id, "acct-0", actor, idem);
        let replay = commit_action(&mut self.cell, catalog, &replay_req, &AllowAll)
            .ctx(self.seed, step, "replay")?;
        ensure(
            self.seed,
            step,
            matches!(replay, ActionOutcome::Replayed { .. }),
            "exact action retry must replay",
        )?;
        self.cov.replays = self.cov.replays.saturating_add(1);
        let mutated = action_call(act, target_id, "acct-0", "other-actor", idem);
        let conflict = commit_action(&mut self.cell, catalog, &mutated, &AllowAll);
        ensure(
            self.seed,
            step,
            matches!(conflict, Err(ActionRefusal::IdempotencyConflict { .. })),
            format!("mutated action key must conflict, got {conflict:?}"),
        )?;
        self.cov.idempotency_conflicts = self.cov.idempotency_conflicts.saturating_add(1);
        ensure_eq(
            self.seed,
            step,
            "replay+conflict zero diff",
            &dump_of(&self.cell, self.seed, step)?,
            &before_dump,
        )?;
        ensure_eq(
            self.seed,
            step,
            "replay+conflict head unchanged",
            &head_of(&self.cell, self.seed, step)?,
            &before_head,
        )
    }

    fn verify_raced_precondition(
        &mut self,
        step: usize,
        catalog: &TypeCatalog<'_>,
        req: &ActionCall<'_>,
        target_key: &FactKey,
        cur: &FactState,
    ) -> DstResult<()> {
        let before = head_of(&self.cell, self.seed, step)?;
        let mut raced = PreconditionRaceHost {
            cell: &mut self.cell,
            interleave_guard: Some(account_key(0)),
            fired: false,
        };
        let res = commit_action(&mut raced, catalog, req, &AllowAll);
        ensure(
            self.seed,
            step,
            matches!(res, Err(ActionRefusal::StalePreconditionRead { .. })),
            format!("expected StalePreconditionRead, got {res:?}"),
        )?;
        self.cov.stale_precondition_reads = self.cov.stale_precondition_reads.saturating_add(1);
        ensure_eq(
            self.seed,
            step,
            "raced action leaves target unchanged",
            &self.cell.load(target_key).ctx(self.seed, step, "reload")?,
            cur,
        )?;
        ensure_eq(
            self.seed,
            step,
            "only guard race committed",
            &head_of(&self.cell, self.seed, step)?.commits,
            &before.commits.saturating_add(1),
        )
    }

    fn run(&mut self, steps: usize) -> DstResult<ActionCoverage> {
        let defs = dst_object_types(self.seed, 0)?;
        let catalog = TypeCatalog::new(&defs);
        let (lock_act, unlock_act) = (
            guarded_toggle_action(self.seed, 0, "lock")?,
            guarded_toggle_action(self.seed, 0, "unlock")?,
        );
        for step in 1..=steps {
            if !self.committed.is_empty() && self.rng.coin(1, 3) {
                let pick = self.rng.index(self.committed.len());
                if let Some((is_lock, target, actor, idem)) = self.committed.get(pick).cloned() {
                    let act = if is_lock { &lock_act } else { &unlock_act };
                    self.verify_replay_and_conflict(step, &catalog, act, &(target, actor, idem))?;
                    continue;
                }
            }
            let target_key = account_key(self.rng.index(3).saturating_add(1));
            let cur = self
                .cell
                .load(&target_key)
                .ctx(self.seed, step, "load target")?;
            let is_open = cur
                .values
                .as_ref()
                .and_then(|v| v.get("status"))
                .and_then(JsonValue::as_str)
                == Some("open");
            let act = if is_open { &lock_act } else { &unlock_act };
            let idem = format!("act-{}", self.next_key);
            self.next_key = self.next_key.saturating_add(1);
            let actor = format!("user-{}", self.rng.index(3));
            let req = action_call(act, &target_key.key, "acct-0", &actor, &idem);
            if self.rng.coin(1, 3) {
                self.verify_raced_precondition(step, &catalog, &req, &target_key, &cur)?;
            } else {
                let out = commit_action(&mut self.cell, &catalog, &req, &AllowAll).ctx(
                    self.seed,
                    step,
                    "commit_action",
                )?;
                ensure(
                    self.seed,
                    step,
                    matches!(out, ActionOutcome::Committed { .. }),
                    "fresh action must commit",
                )?;
                self.cov.commits = self.cov.commits.saturating_add(1);
                self.committed.push((is_open, target_key.key, actor, idem));
            }
            if self.rng.coin(1, 5) {
                let before = dump_of(&self.cell, self.seed, step)?;
                self.cell = open_action_cell(self.seed, step, &self.path)?;
                self.cov.reopens = self.cov.reopens.saturating_add(1);
                ensure_eq(
                    self.seed,
                    step,
                    "action reopen equivalence",
                    &dump_of(&self.cell, self.seed, step)?,
                    &before,
                )?;
            }
        }
        Ok(self.cov.clone())
    }
}

fn run_action_dst_seed(seed: u64, steps: usize) -> DstResult<ActionCoverage> {
    ActionDstHarness::open(seed)?.run(steps)
}

// ------------------------------------------------------------------ Part 4: Two-Execution Confidentiality DST (#716)

#[derive(Debug, Default, Clone)]
struct ConfidentialityCoverage {
    secret_writes: u64,
    stale_version_refusals: u64,
    tainted_note_writes: u64,
    paired_links: u64,
    reopens: u64,
    declassifications: u64,
}

impl ConfidentialityCoverage {
    fn merge(&mut self, other: &Self) {
        self.secret_writes = self.secret_writes.saturating_add(other.secret_writes);
        self.stale_version_refusals = self
            .stale_version_refusals
            .saturating_add(other.stale_version_refusals);
        self.tainted_note_writes = self
            .tainted_note_writes
            .saturating_add(other.tainted_note_writes);
        self.paired_links = self.paired_links.saturating_add(other.paired_links);
        self.reopens = self.reopens.saturating_add(other.reopens);
        self.declassifications = self
            .declassifications
            .saturating_add(other.declassifications);
    }

    fn assert_covered(&self) -> DstResult<()> {
        assert_batch_counts(
            "dst_two_execution_confidentiality_noninterference",
            &[
                ("secret_writes", self.secret_writes),
                ("stale_version_refusals", self.stale_version_refusals),
                ("tainted_note_writes", self.tainted_note_writes),
                ("paired_links", self.paired_links),
                ("reopens", self.reopens),
                ("declassifications", self.declassifications),
            ],
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct PublicObservation {
    visible_accounts: Vec<MaskedObject>,
    visible_notes: Vec<MaskedObject>,
    account_count: usize,
    note_count: usize,
    refusal_codes: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct MaskedObject {
    id: String,
    version: u64,
    visible_fields: BTreeMap<String, String>,
    redacted_fields: Vec<String>,
}

fn read_masked_object(
    seed: u64,
    step: usize,
    reader: &norn_cell::StoreReader<'_, NativeEngine>,
    object_type: &str,
    id: &str,
    clearances: &BTreeSet<&str>,
) -> DstResult<Option<MaskedObject>> {
    let Some(record) = reader
        .object(MAIN_WORLD, object_type, id)
        .ctx(seed, step, "read")?
    else {
        return Ok(None);
    };
    let Some(occurrence) = reader
        .fact_occurrence_markings(MAIN_WORLD, object_type, id, None)
        .ctx(seed, step, "occurrence markings")?
    else {
        return Ok(None);
    };
    if occurrence.iter().any(|m| !clearances.contains(m.as_str())) {
        return Ok(None);
    }
    let Some(prop_markings) = reader
        .effective_markings(MAIN_WORLD, object_type, id, None)
        .ctx(seed, step, "effective markings")?
    else {
        return Ok(None);
    };
    let mut visible_fields = BTreeMap::new();
    let mut redacted_fields = Vec::new();
    for (prop, value) in &record.values {
        let allowed = prop_markings
            .get(prop)
            .is_some_and(|labels| labels.iter().all(|l| clearances.contains(l.as_str())));
        if allowed {
            let encoded = serde_json::to_string(value).ctx(seed, step, "encode")?;
            visible_fields.insert(prop.clone(), encoded);
        } else {
            redacted_fields.push(prop.clone());
        }
    }
    Ok(Some(MaskedObject {
        id: record.id,
        version: record.version,
        visible_fields,
        redacted_fields,
    }))
}

fn observe_world_struct(
    seed: u64,
    step: usize,
    cell: &DstCell<NativeEngine>,
    clearances: &BTreeSet<&str>,
    accounts: usize,
    notes: usize,
    refusals: &[String],
) -> DstResult<PublicObservation> {
    let reader = cell.store().read();
    let mut visible_accounts = Vec::new();
    for idx in 0..accounts {
        if let Some(m) = read_masked_object(
            seed,
            step,
            &reader,
            ACCOUNT_TYPE,
            &format!("acct-{idx}"),
            clearances,
        )? {
            visible_accounts.push(m);
        }
    }
    let mut visible_notes = Vec::new();
    for idx in 0..notes {
        if let Some(m) = read_masked_object(
            seed,
            step,
            &reader,
            NOTE_TYPE,
            &format!("note-{idx}"),
            clearances,
        )? {
            visible_notes.push(m);
        }
    }
    Ok(PublicObservation {
        account_count: visible_accounts.len(),
        note_count: visible_notes.len(),
        visible_accounts,
        visible_notes,
        refusal_codes: refusals.to_vec(),
    })
}

fn encode_observation(seed: u64, step: usize, obs: &PublicObservation) -> DstResult<Vec<u8>> {
    serde_json::to_vec(obs).ctx(seed, step, "serialize observation")
}

struct ConfidentialityPair {
    seed: u64,
    rng: SplitMix64,
    _dir: tempfile::TempDir,
    path_a: PathBuf,
    path_b: PathBuf,
    world_a: DstCell<NativeEngine>,
    world_b: DstCell<NativeEngine>,
    refusals_a: Vec<String>,
    refusals_b: Vec<String>,
    accounts: usize,
    notes: usize,
    cov: ConfidentialityCoverage,
}

impl ConfidentialityPair {
    fn new(seed: u64) -> DstResult<Self> {
        let dir = simulator_dir(seed, 0)?;
        let (path_a, path_b) = (
            dir.path().join("world-a.sqlite"),
            dir.path().join("world-b.sqlite"),
        );
        let mut pair = Self {
            seed,
            rng: SplitMix64::new(seed),
            _dir: dir,
            world_a: open_native_cell(seed, 0, 4, &path_a)?,
            world_b: open_native_cell(seed, 0, 4, &path_b)?,
            path_a,
            path_b,
            refusals_a: Vec::new(),
            refusals_b: Vec::new(),
            accounts: 4,
            notes: 4,
            cov: ConfidentialityCoverage::default(),
        };
        let key = account_key(0);
        let mk = |sec: &str| CellCommit {
            writes: vec![FactWrite {
                key: key.clone(),
                expected: FactVersion::ABSENT,
                mutation: FactMutation::Put {
                    values: account_values(&key.key, 10, "open", sec),
                },
            }],
            ..CellCommit::empty()
        };
        pair.world_a
            .commit(mk("seed-secret-A"))
            .ctx(seed, 0, "seed A")?;
        pair.world_b
            .commit(mk("seed-secret-B"))
            .ctx(seed, 0, "seed B")?;
        Ok(pair)
    }

    fn step_secret_write(&mut self, step: usize) -> DstResult<()> {
        let key = account_key(self.rng.index(self.accounts));
        let ver_a = self
            .world_a
            .load(&key)
            .ctx(self.seed, step, "load A")?
            .version;
        let ver_b = self
            .world_b
            .load(&key)
            .ctx(self.seed, step, "load B")?
            .version;
        ensure_eq(self.seed, step, "paired account version", &ver_a, &ver_b)?;
        let bal = i64::try_from(self.rng.bound(10_000)).unwrap_or(0);
        let stale = self.rng.coin(1, 5);
        let expected = if stale {
            FactVersion(ver_a.0.saturating_add(5))
        } else {
            ver_a
        };
        let (sec_a, sec_b) = (
            format!("alpha-{step}-{}", self.rng.next_u64()),
            format!("beta-{step}-{}", self.rng.next_u64()),
        );
        let mk = |sec: String| CellCommit {
            writes: vec![FactWrite {
                key: key.clone(),
                expected,
                mutation: FactMutation::Put {
                    values: account_values(&key.key, bal, "open", &sec),
                },
            }],
            events: vec![DstEvent::AccountPut {
                key: key.key.clone(),
                balance: bal,
            }],
            ..CellCommit::empty()
        };
        let res_a = self.world_a.commit(mk(sec_a));
        let res_b = self.world_b.commit(mk(sec_b));
        if stale {
            match (res_a, res_b) {
                (Err(ea), Err(eb)) => {
                    ensure(
                        self.seed,
                        step,
                        matches!(ea, CommitError::Conflict { .. })
                            && matches!(eb, CommitError::Conflict { .. }),
                        format!("stale secret write must refuse Conflict, got {ea:?} vs {eb:?}"),
                    )?;
                    self.refusals_a.push(ea.code().to_owned());
                    self.refusals_b.push(eb.code().to_owned());
                    self.cov.stale_version_refusals =
                        self.cov.stale_version_refusals.saturating_add(1);
                }
                (a, b) => {
                    return Err(fail(
                        self.seed,
                        step,
                        format!("stale secret write expected both Err, got A={a:?} B={b:?}"),
                    ));
                }
            }
        } else {
            let _a = res_a.ctx(self.seed, step, "secret write A")?;
            let _b = res_b.ctx(self.seed, step, "secret write B")?;
            self.cov.secret_writes = self.cov.secret_writes.saturating_add(1);
        }
        Ok(())
    }

    fn step_tainted_note(&mut self, step: usize) -> DstResult<()> {
        let mut src_key = account_key(self.rng.index(self.accounts));
        let dst_key = note_key(self.rng.index(self.notes));
        let mut src_a = self.world_a.load(&src_key).ctx(self.seed, step, "src A")?;
        let mut src_b = self.world_b.load(&src_key).ctx(self.seed, step, "src B")?;
        ensure_eq(
            self.seed,
            step,
            "tainted note src version",
            &src_a.version,
            &src_b.version,
        )?;
        if !src_a.exists() {
            src_key = account_key(0);
            src_a = self.world_a.load(&src_key).ctx(self.seed, step, "src0 A")?;
            src_b = self.world_b.load(&src_key).ctx(self.seed, step, "src0 B")?;
            ensure_eq(
                self.seed,
                step,
                "tainted note fallback src version",
                &src_a.version,
                &src_b.version,
            )?;
        }
        let dst_ver_a = self
            .world_a
            .load(&dst_key)
            .ctx(self.seed, step, "dst A")?
            .version;
        let dst_ver_b = self
            .world_b
            .load(&dst_key)
            .ctx(self.seed, step, "dst B")?
            .version;
        ensure_eq(
            self.seed,
            step,
            "tainted note dst version",
            &dst_ver_a,
            &dst_ver_b,
        )?;
        let tag = format!("tag-{step}");
        let (der_a, der_b) = (
            format!("A-{step}-{}", self.rng.next_u64()),
            format!("B-{step}-{}", self.rng.next_u64()),
        );
        let mk = |derived: String| CellCommit {
            reads: vec![FactExpectation {
                key: src_key.clone(),
                expected: src_a.version,
            }],
            writes: vec![FactWrite {
                key: dst_key.clone(),
                expected: dst_ver_a,
                mutation: FactMutation::Put {
                    values: note_values(&dst_key.key, &tag, &derived),
                },
            }],
            events: vec![DstEvent::NoteWritten {
                key: dst_key.key.clone(),
                tag: tag.clone(),
            }],
            ..CellCommit::empty()
        };
        let _a = self
            .world_a
            .commit(mk(der_a))
            .ctx(self.seed, step, "tainted note A")?;
        let _b = self
            .world_b
            .commit(mk(der_b))
            .ctx(self.seed, step, "tainted note B")?;
        self.cov.tainted_note_writes = self.cov.tainted_note_writes.saturating_add(1);
        Ok(())
    }

    fn step_paired_link(&mut self, step: usize) -> DstResult<()> {
        let lk = LinkKey::new(
            account_key(self.rng.index(self.accounts)),
            LINK_PARENT,
            account_key(self.rng.index(self.accounts)),
        );
        let state_a = self
            .world_a
            .load_link(&lk)
            .ctx(self.seed, step, "load link A")?;
        let state_b = self
            .world_b
            .load_link(&lk)
            .ctx(self.seed, step, "load link B")?;
        ensure_eq(
            self.seed,
            step,
            "paired link version",
            &state_a.version,
            &state_b.version,
        )?;
        let mutation = if state_a.exists() {
            LinkMutation::Remove
        } else {
            LinkMutation::Add {
                values: JsonObject::new(),
            }
        };
        let commit = CellCommit {
            links: vec![LinkWrite {
                key: lk,
                expected: state_a.version,
                mutation,
            }],
            ..CellCommit::empty()
        };
        match (
            self.world_a.commit(commit.clone()),
            self.world_b.commit(commit),
        ) {
            (Ok(_), Ok(_)) => {
                self.cov.paired_links = self.cov.paired_links.saturating_add(1);
                Ok(())
            }
            (Err(ea), Err(eb)) => {
                self.refusals_a.push(ea.code().to_owned());
                self.refusals_b.push(eb.code().to_owned());
                Ok(())
            }
            (a, b) => Err(fail(
                self.seed,
                step,
                format!("paired link outcome diverged: A={a:?} B={b:?}"),
            )),
        }
    }

    fn verify_non_vacuity(&self, step: usize) -> DstResult<()> {
        let public = BTreeSet::new();
        let privileged = BTreeSet::from([SECRET_MARKING]);
        let pub_a = observe_world_struct(
            self.seed,
            step,
            &self.world_a,
            &public,
            self.accounts,
            self.notes,
            &self.refusals_a,
        )?;
        let pub_b = observe_world_struct(
            self.seed,
            step,
            &self.world_b,
            &public,
            self.accounts,
            self.notes,
            &self.refusals_b,
        )?;
        let priv_a = observe_world_struct(
            self.seed,
            step,
            &self.world_a,
            &privileged,
            self.accounts,
            self.notes,
            &self.refusals_a,
        )?;
        let priv_b = observe_world_struct(
            self.seed,
            step,
            &self.world_b,
            &privileged,
            self.accounts,
            self.notes,
            &self.refusals_b,
        )?;
        ensure(
            self.seed,
            step,
            priv_a != priv_b && priv_a.visible_accounts != priv_b.visible_accounts,
            "non-vacuity: privileged worlds and accounts must diverge",
        )?;
        ensure(
            self.seed,
            step,
            !priv_a.visible_notes.is_empty()
                && !priv_b.visible_notes.is_empty()
                && priv_a.visible_notes != priv_b.visible_notes,
            "non-vacuity: privileged visible_notes must be non-empty and diverge",
        )?;
        ensure_eq(
            self.seed,
            step,
            "unprivileged visible_notes must match",
            &pub_a.visible_notes,
            &pub_b.visible_notes,
        )?;
        ensure(
            self.seed,
            step,
            !pub_a.visible_notes.is_empty(),
            "non-vacuity: unprivileged visible_notes must be non-empty",
        )?;
        for note in &pub_a.visible_notes {
            ensure(
                self.seed,
                step,
                note.redacted_fields.iter().any(|f| f == "derived_note")
                    && !note.visible_fields.contains_key("derived_note"),
                format!(
                    "derived_note must be redacted for public reader on {}",
                    note.id
                ),
            )?;
        }
        Ok(())
    }

    fn verify_declassification(&mut self, step: usize) -> DstResult<()> {
        let key = account_key(0);
        let hist_a = self
            .world_a
            .load(&key)
            .ctx(self.seed, step, "load hist A")?
            .version;
        let hist_b = self
            .world_b
            .load(&key)
            .ctx(self.seed, step, "load hist B")?
            .version;
        ensure_eq(
            self.seed,
            step,
            "declassification historical version",
            &hist_a,
            &hist_b,
        )?;
        let released = account_values(&key.key, 42, "open", "approved-for-release");
        let mut new_versions = Vec::with_capacity(2);
        for world in [&mut self.world_a, &mut self.world_b] {
            let cur = world.load(&key).ctx(self.seed, step, "load cur")?;
            world
                .commit(CellCommit {
                    writes: vec![FactWrite {
                        key: key.clone(),
                        expected: cur.version,
                        mutation: FactMutation::Put {
                            values: released.clone(),
                        },
                    }],
                    ..CellCommit::empty()
                })
                .ctx(self.seed, step, "prep declassify")?;
            let new_ver = world.load(&key).ctx(self.seed, step, "new ver")?.version;
            new_versions.push(new_ver);
            let change = MarkingDeclassification {
                object_type: ACCOUNT_TYPE.to_owned(),
                key: key.key.clone(),
                property: "secret_note".to_owned(),
                expected_version: new_ver.0,
                markings: vec![SECRET_MARKING.to_owned()],
                actor: "privacy-officer".to_owned(),
                reason: "explicit-dst-release".to_owned(),
            };
            world.declassify_markings("declassify-acct-0", change).ctx(
                self.seed,
                step,
                "declassify",
            )?;
            let hist = world
                .store()
                .read()
                .effective_markings(MAIN_WORLD, ACCOUNT_TYPE, &key.key, Some(hist_a.0))
                .ctx(self.seed, step, "hist markings")?;
            ensure(
                self.seed,
                step,
                hist.and_then(|m| m.get("secret_note").cloned())
                    .is_some_and(|labels| labels.iter().any(|l| l == SECRET_MARKING)),
                "historical version must retain pii.secret after declassification",
            )?;
        }
        ensure_eq(
            self.seed,
            step,
            "declassification new version",
            &new_versions.first().copied(),
            &new_versions.get(1).copied(),
        )?;
        self.cov.declassifications = self.cov.declassifications.saturating_add(1);
        self.verify_public_release(step, &key)
    }

    fn verify_public_release(&self, step: usize, key: &FactKey) -> DstResult<()> {
        let public = BTreeSet::new();
        let final_a = observe_world_struct(
            self.seed,
            step,
            &self.world_a,
            &public,
            self.accounts,
            self.notes,
            &self.refusals_a,
        )?;
        let final_b = observe_world_struct(
            self.seed,
            step,
            &self.world_b,
            &public,
            self.accounts,
            self.notes,
            &self.refusals_b,
        )?;
        for (label, obs) in [("world A", &final_a), ("world B", &final_b)] {
            let acct = obs
                .visible_accounts
                .iter()
                .find(|a| a.id == key.key)
                .ok_or_else(|| fail(self.seed, step, format!("{label}: acct-0 missing")))?;
            ensure_eq(
                self.seed,
                step,
                &format!("{label}: public reader observes declassified secret_note"),
                &acct.visible_fields.get("secret_note").map(String::as_str),
                &Some("\"approved-for-release\""),
            )?;
            ensure(
                self.seed,
                step,
                !acct.redacted_fields.iter().any(|f| f == "secret_note"),
                format!("{label}: declassified secret_note must not be redacted"),
            )?;
        }
        ensure_eq(
            self.seed,
            step,
            "post-declassification noninterference",
            &encode_observation(self.seed, step, &final_a)?,
            &encode_observation(self.seed, step, &final_b)?,
        )
    }

    fn run(&mut self, steps: usize) -> DstResult<ConfidentialityCoverage> {
        let public = BTreeSet::new();
        for step in 1..=steps {
            let roll = self.rng.bound(100);
            if roll < 45 {
                self.step_secret_write(step)?;
            } else if roll < 75 {
                self.step_tainted_note(step)?;
            } else if roll < 90 {
                self.step_paired_link(step)?;
            } else {
                self.world_a = open_native_cell(self.seed, step, 4, &self.path_a)?;
                self.world_b = open_native_cell(self.seed, step, 4, &self.path_b)?;
                self.cov.reopens = self.cov.reopens.saturating_add(1);
            }
            let obs_a = observe_world_struct(
                self.seed,
                step,
                &self.world_a,
                &public,
                self.accounts,
                self.notes,
                &self.refusals_a,
            )?;
            let obs_b = observe_world_struct(
                self.seed,
                step,
                &self.world_b,
                &public,
                self.accounts,
                self.notes,
                &self.refusals_b,
            )?;
            ensure_eq(
                self.seed,
                step,
                "two-execution noninterference",
                &encode_observation(self.seed, step, &obs_a)?,
                &encode_observation(self.seed, step, &obs_b)?,
            )?;
        }
        if self.cov.tainted_note_writes == 0 {
            self.step_tainted_note(steps.saturating_add(1))?;
        }
        self.verify_non_vacuity(steps)?;
        self.verify_declassification(steps)?;
        Ok(self.cov.clone())
    }
}

// ------------------------------------------------------------------ tests

#[test]
fn dst_cell_host_adversarial_schedule_and_crash_recovery() -> DstResult<()> {
    let plan = dst_seeds(MAIN_SEEDS)?;
    let mut cov = MainCoverage::default();
    for seed in plan.seeds {
        let run = NativeDstHarness::open(seed, 4)?.run_steps(30)?;
        cov.merge(&run);
    }
    if plan.assert_batch_coverage {
        cov.assert_covered()?;
    }
    Ok(())
}

#[test]
fn dst_do_engine_commit_fault_and_native_equivalence() -> DstResult<()> {
    let plan = dst_seeds(DO_SEEDS)?;
    let mut cov = DoCoverage::default();
    for seed in plan.seeds {
        let run = run_do_vs_native_seed(seed, 20)?;
        cov.merge(&run);
    }
    if plan.assert_batch_coverage {
        cov.assert_covered()?;
    }
    Ok(())
}

#[test]
fn dst_action_commit_idempotency_and_stale_precondition() -> DstResult<()> {
    let plan = dst_seeds(ACTION_SEEDS)?;
    let mut cov = ActionCoverage::default();
    for seed in plan.seeds {
        let run = run_action_dst_seed(seed, 20)?;
        cov.merge(&run);
    }
    if plan.assert_batch_coverage {
        cov.assert_covered()?;
    }
    Ok(())
}

#[test]
fn dst_two_execution_confidentiality_noninterference() -> DstResult<()> {
    let plan = dst_seeds(CONF_SEEDS)?;
    let mut cov = ConfidentialityCoverage::default();
    for seed in plan.seeds {
        let run = ConfidentialityPair::new(seed)?.run(24)?;
        cov.merge(&run);
    }
    if plan.assert_batch_coverage {
        cov.assert_covered()?;
    }
    Ok(())
}
