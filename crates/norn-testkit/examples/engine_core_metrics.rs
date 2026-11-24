//! Deterministic single-thread micro-benchmark of core Norn engine operations.
//!
//! Emits `METRIC {...}` JSON lines consumed by `scripts/metrics/run_probes.py`:
//! - `cell.memory.rss_per_cell.bytes` (`VmRSS` delta per open in-memory cell on Linux)
//! - `cell.commit.throughput.cmd_s` (single-cell governed commits/s)
//! - `cell.commit.size.bytes` (marginal `SQLite` bytes/commit)
//! - `cell.read.throughput.ops_s` (governed object reads/s)
//! - `decision.eval.throughput.ops_s` (standalone `JDM` decision evaluations/s)
//! - `expr.eval.throughput.ops_s` (standalone `ZEN` expression evaluations/s)

use std::collections::BTreeMap;
use std::error::Error;
use std::ffi::OsString;
use std::hint::black_box;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use norn_cell::{Model, SqliteCell, StoreOptions};
use norn_decisions::{AdmittedDecision, DecisionBinding, evaluate};
use norn_expr::{EvaluationBudget, Expression, Number, Value};
use norn_ir::{ObjectTypeDef, RuleRoot, RuntimeValue};
use norn_kernel::cell_host::JsonObject;
use norn_kernel::{
    CellCommit, CellHost as _, Command, FactKey, FactMutation, FactVersion, FactWrite, LogicalTime,
};
use serde_json::json;

type BenchResult<T> = std::result::Result<T, Box<dyn Error>>;
type BenchCell = SqliteCell<RuntimeValue, JsonObject, JsonObject>;

const BENCH_SCOPE: &str = "org_bench/dev";
const ORDER_TYPE: &str = "bench.Order";

const RSS_CELL_COUNT: u32 = 32;
const RSS_COMMITS_PER_CELL: u32 = 16;

const COMMIT_WARMUP: u32 = 100;
const COMMIT_MEASURED: u32 = 500;

const READ_WARMUP: u32 = 200;
const READ_MEASURED: u32 = 10_000;

const DECISION_WARMUP: u32 = 500;
const DECISION_MEASURED: u32 = 5_000;

const EXPR_WARMUP: u32 = 1_000;
const EXPR_MEASURED: u32 = 20_000;

struct MetricLine<'a> {
    metric: &'a str,
    value: f64,
    unit: &'a str,
    statistic: &'a str,
    samples: u32,
    workload: serde_json::Value,
}

fn echo_command(command: &Command<JsonObject>) -> JsonObject {
    command.payload.clone()
}

fn round2(value: f64) -> f64 {
    (value * 100.0).round() / 100.0
}

fn rate_per_sec(ops: u32, elapsed: Duration) -> BenchResult<f64> {
    if elapsed.is_zero() {
        return Err("benchmark elapsed duration was zero".into());
    }
    Ok(round2(f64::from(ops) / elapsed.as_secs_f64()))
}

fn emit_metric(stdout: &mut std::io::StdoutLock<'_>, entry: &MetricLine<'_>) -> BenchResult<()> {
    let payload = json!({
        "metric": entry.metric,
        "value": entry.value,
        "unit": entry.unit,
        "statistic": entry.statistic,
        "samples": entry.samples,
        "workload": entry.workload,
    });
    writeln!(stdout, "METRIC {}", serde_json::to_string(&payload)?)?;
    Ok(())
}

fn bench_model() -> BenchResult<Model> {
    let object_type: ObjectTypeDef = serde_json::from_value(json!({
        "name": ORDER_TYPE,
        "properties": [
            { "id": "order_no", "type": { "kind": "text" } },
            { "id": "amount", "type": { "kind": "integer" } },
            {
                "id": "status",
                "type": {
                    "kind": "enum",
                    "values": [{ "value": "draft" }, { "value": "submitted" }]
                },
                "default": "draft"
            }
        ],
        "lifecycles": [{
            "property": "status",
            "initial": ["draft"],
            "terminal": ["submitted"],
            "transitions": [{
                "id": "submit",
                "from": ["draft"],
                "to": "submitted",
                "guards": ["self.amount > 0"]
            }]
        }]
    }))?;
    Ok(Model::from_definitions(&[object_type], &[])?)
}

fn commit_order(cell: &mut BenchCell, idx: u32) -> BenchResult<()> {
    let id = format!("order-{idx}");
    let ts_ms = u64::from(idx)
        .checked_add(1)
        .ok_or("logical clock overflow")?;
    let amount = i64::from(idx).checked_add(1).ok_or("amount overflow")?;
    cell.advance_to(LogicalTime::from_millis(ts_ms));
    let values: JsonObject = serde_json::from_value(json!({
        "order_no": &id,
        "amount": amount,
        "status": "draft",
    }))?;
    let event = RuntimeValue::from_json(json!({
        "kind": "bench.order.created",
        "id": &id,
        "amount": amount,
    }))?;
    let commit = CellCommit {
        writes: vec![FactWrite {
            key: FactKey::new(ORDER_TYPE, id),
            expected: FactVersion::ABSENT,
            mutation: FactMutation::Put { values },
        }],
        events: vec![event],
        ..CellCommit::empty()
    };
    let receipt = cell.commit(commit)?;
    black_box(receipt);
    Ok(())
}

fn read_linux_vm_rss_bytes() -> Option<u64> {
    let status = std::fs::read_to_string("/proc/self/status").ok()?;
    for line in status.lines() {
        if let Some(rest) = line.strip_prefix("VmRSS:") {
            let kib_text = rest.split_whitespace().next()?;
            let kib: u64 = kib_text.parse().ok()?;
            return kib.checked_mul(1_024);
        }
    }
    None
}

fn total_sqlite_bytes(db_path: &Path) -> BenchResult<u64> {
    let mut total = std::fs::metadata(db_path)?.len();
    for suffix in ["-wal", "-journal"] {
        let mut sidecar = OsString::from(db_path.as_os_str());
        sidecar.push(suffix);
        if let Ok(meta) = std::fs::metadata(PathBuf::from(sidecar)) {
            total = total
                .checked_add(meta.len())
                .ok_or("sqlite byte size overflow")?;
        }
    }
    Ok(total)
}

fn benchmark_memory_per_cell(
    model: &Model,
    stdout: &mut std::io::StdoutLock<'_>,
) -> BenchResult<()> {
    if read_linux_vm_rss_bytes().is_none() {
        return Ok(());
    }
    let mut warmup = BenchCell::open_in_memory(model.clone(), BENCH_SCOPE, echo_command)?;
    for idx in 0..RSS_COMMITS_PER_CELL {
        commit_order(&mut warmup, idx)?;
    }
    drop(warmup);

    let rss_before = read_linux_vm_rss_bytes().ok_or("failed to read VmRSS before cell open")?;
    let mut cells = Vec::with_capacity(usize::try_from(RSS_CELL_COUNT)?);
    for _ in 0..RSS_CELL_COUNT {
        let mut cell = BenchCell::open_in_memory(model.clone(), BENCH_SCOPE, echo_command)?;
        for idx in 0..RSS_COMMITS_PER_CELL {
            commit_order(&mut cell, idx)?;
        }
        cells.push(cell);
    }
    black_box(&cells);
    let rss_after = read_linux_vm_rss_bytes().ok_or("failed to read VmRSS after cell open")?;
    drop(cells);

    let delta = rss_after.saturating_sub(rss_before);
    let per_cell = round2(f64::from(u32::try_from(delta)?) / f64::from(RSS_CELL_COUNT));
    emit_metric(
        stdout,
        &MetricLine {
            metric: "cell.memory.rss_per_cell.bytes",
            value: per_cell,
            unit: "bytes",
            statistic: "mean",
            samples: RSS_CELL_COUNT,
            workload: json!({
                "cells": RSS_CELL_COUNT,
                "commits_per_cell": RSS_COMMITS_PER_CELL,
                "source": "proc_self_status_vmrss",
            }),
        },
    )
}

fn benchmark_commit_and_read(
    model: &Model,
    stdout: &mut std::io::StdoutLock<'_>,
) -> BenchResult<()> {
    let temp = tempfile::TempDir::new()?;
    let db_path = temp.path().join("bench.sqlite");
    let mut cell = BenchCell::open(
        &db_path,
        model.clone(),
        StoreOptions::default(),
        BENCH_SCOPE,
        echo_command,
    )?;

    for idx in 0..COMMIT_WARMUP {
        commit_order(&mut cell, idx)?;
    }

    let size_before = total_sqlite_bytes(&db_path)?;
    let commit_start = Instant::now();
    for offset in 0..COMMIT_MEASURED {
        let idx = COMMIT_WARMUP
            .checked_add(offset)
            .ok_or("commit index overflow")?;
        commit_order(&mut cell, idx)?;
    }
    let commit_elapsed = commit_start.elapsed();
    let size_after = total_sqlite_bytes(&db_path)?;

    let commit_cmd_s = rate_per_sec(COMMIT_MEASURED, commit_elapsed)?;
    let size_delta = size_after.saturating_sub(size_before);
    let bytes_per_commit =
        round2(f64::from(u32::try_from(size_delta)?) / f64::from(COMMIT_MEASURED));
    let commit_workload = json!({
        "warmup_commits": COMMIT_WARMUP,
        "measured_commits": COMMIT_MEASURED,
        "journal_mode": "delete",
    });

    emit_metric(
        stdout,
        &MetricLine {
            metric: "cell.commit.throughput.cmd_s",
            value: commit_cmd_s,
            unit: "per_s",
            statistic: "mean",
            samples: COMMIT_MEASURED,
            workload: commit_workload.clone(),
        },
    )?;
    emit_metric(
        stdout,
        &MetricLine {
            metric: "cell.commit.size.bytes",
            value: bytes_per_commit,
            unit: "bytes",
            statistic: "mean",
            samples: COMMIT_MEASURED,
            workload: commit_workload,
        },
    )?;

    let total_objects = COMMIT_WARMUP
        .checked_add(COMMIT_MEASURED)
        .ok_or("object count overflow")?;
    benchmark_reads(&cell, total_objects, stdout)
}

fn benchmark_reads(
    cell: &BenchCell,
    total_objects: u32,
    stdout: &mut std::io::StdoutLock<'_>,
) -> BenchResult<()> {
    let keys: Vec<FactKey> = (0..total_objects)
        .map(|idx| FactKey::new(ORDER_TYPE, format!("order-{idx}")))
        .collect();

    for step in 0..READ_WARMUP {
        let slot = usize::try_from(step.checked_rem(total_objects).ok_or("modulo zero")?)?;
        let key = keys.get(slot).ok_or("missing warmup key")?;
        let state = cell.load(key)?;
        if !state.exists() {
            return Err("warmup read returned absent object".into());
        }
        black_box(state);
    }

    let read_start = Instant::now();
    for step in 0..READ_MEASURED {
        let slot = usize::try_from(step.checked_rem(total_objects).ok_or("modulo zero")?)?;
        let key = keys.get(slot).ok_or("missing read key")?;
        let state = cell.load(key)?;
        if !state.exists() {
            return Err("measured read returned absent object".into());
        }
        black_box(state);
    }
    let read_elapsed = read_start.elapsed();
    let read_ops_s = rate_per_sec(READ_MEASURED, read_elapsed)?;

    emit_metric(
        stdout,
        &MetricLine {
            metric: "cell.read.throughput.ops_s",
            value: read_ops_s,
            unit: "per_s",
            statistic: "mean",
            samples: READ_MEASURED,
            workload: json!({
                "objects": total_objects,
                "warmup_reads": READ_WARMUP,
                "measured_reads": READ_MEASURED,
            }),
        },
    )
}

fn closed_schema(properties: &serde_json::Value, required: &[&str]) -> serde_json::Value {
    json!({"type": "object", "properties": properties, "required": required, "additionalProperties": false})
}

fn decision_rule() -> BenchResult<RuleRoot> {
    let text = json!({"type": "string"});
    let integer = json!({"type": "integer"});
    let fact = closed_schema(
        &json!({
            "op": {"type": "string", "enum": ["transition"]},
            "objectType": {"type": "string", "enum": [ORDER_TYPE]},
            "key": &text,
            "lifecycle": {"type": "string", "enum": ["status"]},
            "transition": {"type": "string", "enum": ["submit"]},
        }),
        &["op", "objectType", "key", "lifecycle", "transition"],
    );
    let facts_expr = "event.kind == 'submit' and slice.status == 'draft' \
        ? [{op: 'transition', objectType: 'bench.Order', key: event.key, lifecycle: 'status', transition: 'submit'}] \
        : []";
    let cmds_expr = "event.kind == 'submit' ? [{kind: 'notify', order: event.key}] : []";
    let rule = json!({
        "ruleId": "bench.rule.order_submit",
        "ruleVersion": "2.0",
        "contract": "decide",
        "inputSchema": closed_schema(&json!({
            "slice": closed_schema(&json!({"status": &text}), &["status"]),
            "event": closed_schema(&json!({"kind": &text, "key": &text, "amount": &integer}), &["kind", "key", "amount"]),
        }), &["slice", "event"]),
        "outputSchema": closed_schema(&json!({
            "facts": {"type": "array", "items": fact},
            "commands": {"type": "array", "items": closed_schema(&json!({"kind": &text, "order": &text}), &["kind", "order"])},
            "timers": {"type": "array", "items": closed_schema(&json!({"afterMillis": &integer, "kind": &text}), &["afterMillis", "kind"])},
        }), &["facts", "commands", "timers"]),
        "scopeRequired": [],
        "graph": {
            "nodes": [
                {"id": "in", "name": "in", "type": "inputNode"},
                {"id": "x", "name": "x", "type": "expressionNode", "content": {"expressions": [
                    {"id": "f", "key": "facts", "value": facts_expr},
                    {"id": "c", "key": "commands", "value": cmds_expr},
                    {"id": "t", "key": "timers", "value": "[]"},
                ]}},
                {"id": "out", "name": "out", "type": "outputNode"},
            ],
            "edges": [
                {"id": "e1", "sourceId": "in", "targetId": "x"},
                {"id": "e2", "sourceId": "x", "targetId": "out"},
            ],
        },
    });
    Ok(serde_json::from_value(rule)?)
}

fn benchmark_decision(stdout: &mut std::io::StdoutLock<'_>) -> BenchResult<()> {
    let rule = decision_rule()?;
    let binding = DecisionBinding {
        profile_id: "norn-zen/7".to_owned(),
        environment_hash: "sha256:bench".to_owned(),
    };
    let decision = AdmittedDecision::admit(&rule, &BTreeMap::new(), binding)?;
    let slice = decision.validate_slice(Value::object([("status", Value::from("draft"))]))?;
    let event = decision.type_event(Value::object([
        ("kind", Value::from("submit")),
        ("key", Value::from("order-1")),
        ("amount", Value::Number(Number::from(42))),
    ]))?;

    for _ in 0..DECISION_WARMUP {
        let evaluated = evaluate(&decision, &slice, &event, EvaluationBudget::default())?;
        black_box(evaluated);
    }

    let started = Instant::now();
    for _ in 0..DECISION_MEASURED {
        let evaluated = evaluate(&decision, &slice, &event, EvaluationBudget::default())?;
        if evaluated.output.commands.len() != 1 {
            return Err("unexpected decision output command count".into());
        }
        black_box(evaluated);
    }
    let elapsed = started.elapsed();
    let ops_s = rate_per_sec(DECISION_MEASURED, elapsed)?;

    emit_metric(
        stdout,
        &MetricLine {
            metric: "decision.eval.throughput.ops_s",
            value: ops_s,
            unit: "per_s",
            statistic: "mean",
            samples: DECISION_MEASURED,
            workload: json!({
                "warmup_evals": DECISION_WARMUP,
                "measured_evals": DECISION_MEASURED,
                "nodes": 3,
            }),
        },
    )
}

fn benchmark_expression(stdout: &mut std::io::StdoutLock<'_>) -> BenchResult<()> {
    let expr = Expression::compile("price * quantity - discount")?;
    let context = Value::object([
        ("price", Value::Number("12.50".parse()?)),
        ("quantity", Value::Number(Number::from(3))),
        ("discount", Value::Number("2.25".parse()?)),
    ]);
    let expected = Value::Number("35.25".parse()?);

    for _ in 0..EXPR_WARMUP {
        let evaluation = expr.evaluate(&context, EvaluationBudget::new(100))?;
        black_box(evaluation);
    }

    let started = Instant::now();
    for _ in 0..EXPR_MEASURED {
        let evaluation = expr.evaluate(&context, EvaluationBudget::new(100))?;
        if evaluation.value() != &expected {
            return Err("unexpected expression evaluation result".into());
        }
        black_box(evaluation);
    }
    let elapsed = started.elapsed();
    let ops_s = rate_per_sec(EXPR_MEASURED, elapsed)?;

    emit_metric(
        stdout,
        &MetricLine {
            metric: "expr.eval.throughput.ops_s",
            value: ops_s,
            unit: "per_s",
            statistic: "mean",
            samples: EXPR_MEASURED,
            workload: json!({
                "warmup_evals": EXPR_WARMUP,
                "measured_evals": EXPR_MEASURED,
                "expression": "price * quantity - discount",
            }),
        },
    )
}

fn main() -> BenchResult<()> {
    let model = bench_model()?;
    let mut stdout = std::io::stdout().lock();
    benchmark_memory_per_cell(&model, &mut stdout)?;
    benchmark_commit_and_read(&model, &mut stdout)?;
    benchmark_decision(&mut stdout)?;
    benchmark_expression(&mut stdout)?;
    Ok(())
}
