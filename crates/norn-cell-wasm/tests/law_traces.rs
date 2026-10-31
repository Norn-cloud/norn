#![allow(
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::too_many_lines,
    missing_docs,
    reason = "integration-test crate"
)]

//! The `CellHost` laws as JSON traces for `apps/cell-shim` (T4-CELL-4, #2678).
//!
//! The shim is TypeScript, so it cannot run `norn_testkit`'s
//! `proptest-state-machine` laws directly. Instead this test samples
//! operation sequences from the same law machine
//! (`CellHostMachine::sequential_strategy`, a fixed seed), runs them on the
//! reference `MemoryCell` (which the law machine itself checks against the
//! independent model, as it does `norn-cell`'s `SqliteCell`), and records
//! every verb's outcome plus the observable cell state after each operation.
//! The shim's tests replay the file and require the same outcomes and states.
//!
//! The recorded file is a checked-in golden
//! (`apps/cell-shim/test/fixtures/cell-host-laws.json`): this test fails when
//! it is stale. `NORN_CELL_TRACES_BLESS=1` rewrites it.
//!
//! Trace format (`norn-cell-shim/law-traces@3`; `@2` added links, #3013;
//! `@3` adds the cell's `model`, its object types, #3018): `model` and
//! `sequences[]` of `{initial, ops, ledger}`. `initial` is the empty cell's state; each op is
//! one of
//!
//! - `{op: "commit", commit, expect: {receipt} | {refusal}}`
//! - `{op: "perform", expect: {delivered, executed}}`
//! - `{op: "observe", expect}` / `{op: "clock", expect}`
//! - `{op: "advance", to}` (absolute host time, ms)
//! - `{op: "inject", observed, accepted}`
//!
//! plus `after`: the state fields that changed (`facts`, `links`, `clock`,
//! `observe`, `outbox`, `timers`, `ledgerLength`, `executions`). `ledger` is the full
//! ledger at the end. Kernel inputs are `{"timer": n}`, `{"result": n}`, or
//! `{"observed": n}`; the shim's test performer answers a command with
//! `{"result": <its payload>}` (the law responder).

use std::collections::BTreeSet;
use std::path::PathBuf;

use norn_cell_wasm::refusal_value;
use norn_kernel::cell_host::JsonObject;
use norn_kernel::{
    CellCommit, CellHost, Command, DueTimer, Effect, FactMutation, FactVersion, FactWrite,
    LinkMutation, LinkWrite, LogicalTime, Observed, Performed, Timer,
};
use norn_testkit::cell_host::{
    CELL_HOST_LABELS, CellHostMachine, CellOp, LAW_DECLARED_LINKS, LawCommand, LawEvent, LawInput,
    MemoryCell, law_fact_keys, law_key, law_link_key, law_object_types, law_observed,
    law_responder, law_values,
};
use norn_testkit::proptest::strategy::{Strategy, ValueTree};
use norn_testkit::proptest::test_runner::{Config, RngAlgorithm, TestRng, TestRunner};
use norn_testkit::proptest_state_machine::ReferenceStateMachine;
use serde_json::{Map, Value, json};

type Cell = MemoryCell<LawEvent, LawCommand, LawInput>;

const FORMAT: &str = "norn-cell-shim/law-traces@3";
const SEED: [u8; 32] = *b"norn-cell-shim/law-traces/seed/1";
// Result consumption is rarer than the other laws: a generated command must
// be performed before a later commit can consume its receipt. Keep a wide,
// fixed-seed sample so every required label is present in the portable trace.
const SEQUENCES: usize = 128;
const MIN_OPS: usize = 8;
const MAX_OPS: usize = 40;
const BLESS_ENV: &str = "NORN_CELL_TRACES_BLESS";

fn golden_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../apps/cell-shim/test/fixtures/cell-host-laws.json")
}

fn to_json(value: &impl serde::Serialize) -> Value {
    serde_json::to_value(value).expect("law values serialize")
}

/// `serde_json::Map` follows `BTreeMap` by default but preserves insertion
/// order when another workspace crate enables `preserve_order`. Keep the
/// checked-in trace independent of that Cargo feature unification.
fn sort_json_object_keys(value: &mut Value) {
    match value {
        Value::Array(values) => {
            for value in values {
                sort_json_object_keys(value);
            }
        }
        Value::Object(object) => {
            let mut entries = std::mem::take(object).into_iter().collect::<Vec<_>>();
            entries.sort_unstable_by(|(left, _), (right, _)| left.cmp(right));
            for (_, value) in &mut entries {
                sort_json_object_keys(value);
            }
            object.extend(entries);
        }
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => {}
    }
}

fn input(input: &LawInput) -> Value {
    match input {
        LawInput::Timer(n) => json!({ "timer": n }),
        LawInput::Result(n) => json!({ "result": n }),
        LawInput::Observed(n) => json!({ "observed": n }),
    }
}

fn timer(timer: &Timer<LawInput>) -> Timer<Value> {
    Timer {
        id: timer.id.clone(),
        due: timer.due,
        payload: input(&timer.payload),
    }
}

fn due(due: &DueTimer<LawInput>) -> Value {
    to_json(&DueTimer {
        timer: timer(&due.timer),
        incarnation: due.incarnation,
    })
}

fn observed(observed: &Observed<LawInput>) -> Value {
    to_json(&Observed {
        id: observed.id.clone(),
        provenance: observed.provenance.clone(),
        input: input(&observed.input),
    })
}

fn performed(performed: &Performed<LawInput>) -> Value {
    to_json(&Performed {
        command: performed.command.clone(),
        generation: performed.generation,
        input: input(&performed.input),
    })
}

fn commit(commit: &CellCommit<LawEvent, LawCommand, LawInput>) -> Value {
    to_json(&CellCommit::<LawEvent, LawCommand, Value> {
        consumes: commit.consumes.clone(),
        reads: commit.reads.clone(),
        writes: commit.writes.clone(),
        events: commit.events.clone(),
        effects: commit
            .effects
            .iter()
            .map(|effect| match effect {
                Effect::Perform(command) => Effect::Perform(command.clone()),
                Effect::Schedule(scheduled) => Effect::Schedule(timer(scheduled)),
                Effect::CancelTimer(id) => Effect::CancelTimer(id.clone()),
            })
            .collect(),
        links: commit.links.clone(),
        link_reads: commit.link_reads.clone(),
    })
}

/// The observable state the laws compare after every operation.
fn state(cell: &mut Cell) -> Map<String, Value> {
    let facts: Vec<Value> = law_fact_keys()
        .into_iter()
        .map(|key| {
            let Ok(state) = cell.load(&key);
            json!({ "key": key, "state": state })
        })
        .collect();
    let links: Vec<Value> = (0..LAW_DECLARED_LINKS)
        .map(|n| {
            let key = law_link_key(n);
            let Ok(state) = cell.load_link(&key);
            json!({ "key": key, "state": state })
        })
        .collect();
    let Ok(clock) = cell.clock();
    let Ok(observe) = cell.observe();
    let outbox: Vec<&Command<LawCommand>> = cell.outbox().iter().collect();
    let timers: Vec<Value> = cell.timers().values().map(due).collect();
    let mut state = Map::new();
    state.insert("facts".to_owned(), Value::Array(facts));
    state.insert("links".to_owned(), Value::Array(links));
    state.insert("clock".to_owned(), clock.as_ref().map_or(Value::Null, due));
    state.insert(
        "observe".to_owned(),
        observe.as_ref().map_or(Value::Null, observed),
    );
    state.insert("outbox".to_owned(), to_json(&outbox));
    state.insert("timers".to_owned(), Value::Array(timers));
    state.insert("ledgerLength".to_owned(), json!(cell.ledger().len()));
    state.insert("executions".to_owned(), json!(cell.executions()));
    state
}

fn ledger(cell: &Cell) -> Value {
    Value::Array(
        cell.ledger()
            .iter()
            .map(|entry| {
                json!({
                    "seq": entry.seq,
                    "consumed": entry.consumed,
                    "written": entry.written,
                    "linked": entry.linked,
                    "events": entry.events,
                })
            })
            .collect(),
    )
}

fn accepted_label(commit: &CellCommit<LawEvent, LawCommand, LawInput>) -> &'static str {
    match &commit.consumes {
        None => "commit.accepted",
        Some(norn_kernel::Consumed::Timer { .. }) => "commit.accepted.consumed_timer",
        Some(norn_kernel::Consumed::Result { .. }) => "commit.accepted.consumed_result",
        Some(norn_kernel::Consumed::Observation { .. }) => "commit.accepted.consumed_observation",
    }
}

/// Runs one sequence on a fresh reference cell and records it.
fn record(ops: &[CellOp], labels: &mut BTreeSet<String>) -> Value {
    let mut cell = Cell::new(law_responder).with_types(law_object_types());
    let mut now = 0_u64;
    let initial = state(&mut cell);
    let mut last = initial.clone();
    let mut steps = Vec::with_capacity(ops.len());
    for op in ops {
        let mut step = match op {
            CellOp::Commit(law_commit) => {
                let expect = match cell.commit(law_commit.clone()) {
                    Ok(receipt) => {
                        labels.insert(accepted_label(law_commit).to_owned());
                        if !receipt.linked.is_empty() {
                            labels.insert("commit.accepted.linked".to_owned());
                        }
                        let Ok(()) = cell.publish(&receipt);
                        json!({ "receipt": receipt })
                    }
                    Err(error) => {
                        labels.insert(error.code().to_owned());
                        json!({ "refusal": refusal_value(&error) })
                    }
                };
                json!({ "op": "commit", "commit": commit(law_commit), "expect": expect })
            }
            CellOp::Perform => {
                let before = cell.executions();
                let Ok(delivered) = cell.perform();
                let executed = cell.executions() != before;
                labels.insert(
                    match (&delivered, executed) {
                        (None, _) => "perform.empty",
                        (Some(_), true) => "perform.executed",
                        (Some(_), false) => "perform.redelivered",
                    }
                    .to_owned(),
                );
                json!({
                    "op": "perform",
                    "expect": {
                        "delivered": delivered.as_ref().map_or(Value::Null, performed),
                        "executed": executed,
                    },
                })
            }
            CellOp::Observe => {
                let Ok(delivered) = cell.observe();
                if delivered.is_some() {
                    labels.insert("observe.delivered".to_owned());
                }
                json!({ "op": "observe", "expect": delivered.as_ref().map_or(Value::Null, observed) })
            }
            CellOp::Clock => {
                let Ok(delivered) = cell.clock();
                if delivered.is_some() {
                    labels.insert("clock.due".to_owned());
                }
                json!({ "op": "clock", "expect": delivered.as_ref().map_or(Value::Null, due) })
            }
            CellOp::Advance(millis) => {
                now = now.saturating_add(*millis);
                cell.advance_to(LogicalTime::from_millis(now));
                json!({ "op": "advance", "to": now })
            }
            CellOp::Inject { id, payload } => {
                let arriving = law_observed(*id, *payload);
                let accepted = cell.inject(arriving.clone());
                json!({ "op": "inject", "observed": observed(&arriving), "accepted": accepted })
            }
        };
        let current = state(&mut cell);
        let changed: Map<String, Value> = current
            .iter()
            .filter(|(field, value)| last.get(*field) != Some(*value))
            .map(|(field, value)| (field.clone(), value.clone()))
            .collect();
        if !changed.is_empty() {
            step["after"] = Value::Object(changed);
        }
        last = current;
        steps.push(step);
    }
    json!({ "initial": initial, "ops": steps, "ledger": ledger(&cell) })
}

/// A pinned sequence for the rarest refusal, `commit.link_cardinality`: a
/// generated commit must first pass delivery, CAS, link CAS, and endpoint
/// checks, so random sampling seldom reaches it. Puts `k0` and `k1`, then
/// adds two `owner` (to-one) links from `k0` in one commit.
fn pinned_cardinality() -> Vec<CellOp> {
    let commit = |writes: Vec<FactWrite>, links: Vec<LinkWrite>| {
        CellOp::Commit(CellCommit {
            consumes: None,
            reads: Vec::new(),
            writes,
            events: Vec::new(),
            effects: Vec::new(),
            links,
            link_reads: Vec::new(),
        })
    };
    let put = |n: u8| FactWrite {
        key: law_key(n),
        expected: FactVersion::ABSENT,
        mutation: FactMutation::Put {
            values: law_values(n),
        },
    };
    let owner = |n: u8| LinkWrite {
        key: law_link_key(n),
        expected: FactVersion::ABSENT,
        mutation: LinkMutation::Add {
            values: JsonObject::new(),
        },
    };
    vec![
        commit(vec![put(0), put(1)], Vec::new()),
        // `owner` k0 -> k0 and k0 -> k1 (see `law_link_key`).
        commit(Vec::new(), vec![owner(9), owner(12)]),
    ]
}

/// The whole trace file, one sequence per line.
fn traces() -> (String, BTreeSet<String>) {
    let mut runner = TestRunner::new_with_rng(
        Config::default(),
        TestRng::from_seed(RngAlgorithm::ChaCha, &SEED),
    );
    let strategy = CellHostMachine::<Cell>::sequential_strategy(MIN_OPS..=MAX_OPS);
    let mut labels = BTreeSet::new();
    let mut lines = Vec::with_capacity(SEQUENCES);
    for _ in 0..SEQUENCES {
        let tree = strategy
            .new_tree(&mut runner)
            .expect("the law machine has no preconditions");
        let (_, ops, _) = tree.current();
        let mut sequence = record(&ops, &mut labels);
        sort_json_object_keys(&mut sequence);
        lines.push(serde_json::to_string(&sequence).expect("trace serializes"));
    }
    let mut pinned = record(&pinned_cardinality(), &mut labels);
    sort_json_object_keys(&mut pinned);
    lines.push(serde_json::to_string(&pinned).expect("trace serializes"));
    let regenerate = format!("{BLESS_ENV}=1 cargo test -p norn-cell-wasm --test law_traces");
    let model = serde_json::to_string(&law_object_types()).expect("law model serializes");
    let mut text = format!(
        "{{\"format\":{},\"generator\":{},\"regenerate\":{},\"model\":{model},\"sequences\":[\n",
        json!(FORMAT),
        json!("crates/norn-cell-wasm/tests/law_traces.rs"),
        json!(regenerate),
    );
    text.push_str(&lines.join(",\n"));
    text.push_str("\n]}\n");
    (text, labels)
}

#[test]
fn law_traces_cover_every_law_outcome() {
    let (_, labels) = traces();
    for label in CELL_HOST_LABELS {
        let label = label.trim_start_matches("cell:");
        assert!(
            labels.contains(label),
            "the recorded traces never hit `{label}`; raise SEQUENCES or MAX_OPS (hit: {labels:?})"
        );
    }
}

#[test]
fn law_traces_are_deterministic() {
    assert_eq!(
        traces().0,
        traces().0,
        "the trace generator is not deterministic"
    );
}

#[test]
fn checked_in_law_traces_are_current() {
    let (text, _) = traces();
    let path = golden_path();
    if std::env::var_os(BLESS_ENV).is_some() {
        std::fs::write(&path, &text)
            .unwrap_or_else(|error| panic!("write {}: {error}", path.display()));
        return;
    }
    let checked_in = std::fs::read_to_string(&path).unwrap_or_default();
    assert!(
        checked_in == text,
        "{} is stale ({} bytes checked in, {} generated); regenerate with \
         `{BLESS_ENV}=1 cargo test -p norn-cell-wasm --test law_traces` and commit it",
        path.display(),
        checked_in.len(),
        text.len()
    );
}
