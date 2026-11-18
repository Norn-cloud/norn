//! Commit-path work counters and the receipt replay gate (#1417 T0, #1421).
//!
//! `commit_counters_by_size` prints one `COMMIT-COUNTERS` JSON line per size:
//! the deterministic work one checked commit does, at 10, 100 and 1,000 rows
//! per commit and at 10, 100 and 1,000 installed laws.
//!
//! `recorded_commit_sequence_replays_identical_receipts` re-runs one recorded
//! commit sequence (puts, updates, a refused write, a delete) and compares every
//! kernel receipt, refusal, stored receipt and ledger entry byte for byte with
//! `fixtures/commit-replay.json`. A behaviour-preserving change must leave that
//! file unchanged; `NORN_RECORD_COMMIT_REPLAY=1` rewrites it.
#![cfg(feature = "native")]
#![allow(
    clippy::arithmetic_side_effects,
    clippy::expect_used,
    clippy::panic,
    clippy::print_stdout,
    missing_docs,
    reason = "measurement and replay harness"
)]

use norn_cell::{CellStore, Model, SqliteCell};
use norn_ir::counters::{self, Counts};
use norn_ir::{ArtifactEnvelope, ObjectTypeDef};
use norn_kernel::cell_host::JsonObject;
use norn_kernel::{CellCommit, CellHost, FactEvent, FactKey, FactMutation, FactVersion, FactWrite};
use serde_json::{Value, json};

type Cell = SqliteCell<FactEvent, JsonObject, JsonObject>;

const OBJECT_TYPE: &str = "bench.Item";
const INVARIANTS: usize = 4;

fn object_type(invariants: usize) -> ObjectTypeDef {
    let invariants: Vec<Value> = (1..=invariants)
        .map(|i| {
            json!({
                "id": format!("floor_{i}"),
                "expression": format!("self.qty >= -{i}"),
                "message": format!("qty is at least -{i}")
            })
        })
        .collect();
    serde_json::from_value(json!({
        "name": OBJECT_TYPE,
        "properties": [
            {"id": "qty", "type": {"kind": "integer"}},
            {"id": "label", "type": {"kind": "text"}, "default": "item"}
        ],
        "invariants": invariants
    }))
    .expect("object type")
}

fn laws(count: usize) -> Vec<ArtifactEnvelope> {
    let template: Vec<Value> =
        serde_json::from_str(include_str!("../../../fixtures/stress/purchase.json"))
            .expect("law template set");
    let template = template
        .into_iter()
        .find(|a| a["artifactId"] == "stress.purchase_positive.law")
        .expect("law template");
    (0..count)
        .map(|i| {
            let mut law = template.clone();
            law["artifactId"] = json!(format!("bench.ceiling_{i:04}.law"));
            law["namespace"] = json!("bench");
            law["dependencies"][0]["artifactId"] = json!("bench.Item.entity");
            let payload = &mut law["payload"];
            payload["id"] = json!(format!("bench.ceiling_{i:04}"));
            payload["owner"] = json!("bench");
            payload["subject"] = json!(OBJECT_TYPE);
            payload["scope"] = json!({"objectTypes": [OBJECT_TYPE], "root": {"id": OBJECT_TYPE, "kind": "entity"}});
            payload["response"]["expression"] = json!(format!("self.qty <= {}", 1_000_000 + i));
            serde_json::from_value(law).expect("law artifact")
        })
        .collect()
}

fn cell(laws: usize) -> Cell {
    let model = Model::from_definitions(&[object_type(INVARIANTS)], &[])
        .expect("model")
        .with_governing_laws(&self::laws(laws))
        .expect("laws");
    Cell::new(
        CellStore::open_in_memory(model).expect("SQLite"),
        "org_demo/dev",
        |command| command.payload.clone(),
    )
    .expect("cell")
}

fn values(qty: i64) -> JsonObject {
    serde_json::from_value(json!({"qty": qty})).expect("values")
}

fn put(key: &str, expected: FactVersion, qty: i64) -> FactWrite<JsonObject> {
    FactWrite {
        key: FactKey::new(OBJECT_TYPE, key),
        expected,
        mutation: FactMutation::Put {
            values: values(qty),
        },
    }
}

fn creates(prefix: &str, rows: usize) -> CellCommit<FactEvent, JsonObject, JsonObject> {
    CellCommit {
        writes: (0..rows)
            .map(|i| put(&format!("{prefix}{i:04}"), FactVersion::ABSENT, 1))
            .collect(),
        ..CellCommit::empty()
    }
}

fn json_counts(counts: &Counts) -> Value {
    counts
        .entries()
        .into_iter()
        .map(|(name, value)| (name.to_owned(), json!(value)))
        .collect::<serde_json::Map<_, _>>()
        .into()
}

/// The counts of one checked commit of `rows` fresh objects, after one warm-up
/// commit on the same cell.
fn measure(laws: usize, rows: usize) -> Counts {
    let mut cell = cell(laws);
    cell.commit(creates("warm", 1)).expect("warm-up commit");
    let _ = counters::take();
    cell.commit(creates("row", rows)).expect("measured commit");
    counters::take()
}

#[test]
fn commit_counters_by_size() {
    assert!(
        counters::ENABLED,
        "norn-cell tests enable the counters feature"
    );
    let mut lines = Vec::new();
    for rows in [10, 100, 1_000] {
        let counts = measure(4, rows);
        lines.push(json!({"axis": "rows", "rows": rows, "laws": 4, "invariants": INVARIANTS, "counts": json_counts(&counts)}));
    }
    for laws in [10, 100, 1_000] {
        let counts = measure(laws, 10);
        lines.push(json!({"axis": "laws", "rows": 10, "laws": laws, "invariants": INVARIANTS, "counts": json_counts(&counts)}));
    }
    // P5: definition-derived work is done once per commit, not once per row.
    for name in [
        "expression_compiles",
        "expression_environment_builds",
        "law_decodes",
    ] {
        let by_rows: Vec<_> = lines
            .iter()
            .filter(|line| line["axis"] == "rows")
            .map(|line| line["counts"][name].clone())
            .collect();
        assert!(
            by_rows.windows(2).all(|pair| pair[0] == pair[1]),
            "{name} grows with rows: {by_rows:?}"
        );
    }
    for line in &lines {
        println!("COMMIT-COUNTERS {line}");
        let counts = &line["counts"];
        for name in [
            "sql_statements",
            "rows_written",
            "bytes_written",
            "hashes",
            "expression_compiles",
        ] {
            assert!(
                counts[name].as_u64().is_some_and(|n| n > 0),
                "{name} counted: {line}"
            );
        }
    }
}

/// One recorded commit sequence: every kernel result, then every stored
/// receipt and ledger entry, as one JSON document.
fn replay_transcript() -> Value {
    let mut cell = cell(3);
    let mut steps = Vec::new();
    let mut record = |cell: &mut Cell, commit| {
        let result = match cell.commit(commit) {
            Ok(receipt) => json!({"ok": receipt}),
            Err(error) => json!({"refused": error.code(), "message": error.to_string()}),
        };
        steps.push(result);
    };
    record(&mut cell, creates("a", 10));
    let updates = (0_i64..5)
        .map(|i| {
            let key = FactKey::new(OBJECT_TYPE, format!("a{i:04}"));
            let version = cell.load(&key).expect("load").version;
            put(&key.key, version, 2 + i)
        })
        .collect();
    record(
        &mut cell,
        CellCommit {
            writes: updates,
            ..CellCommit::empty()
        },
    );
    record(
        &mut cell,
        CellCommit {
            writes: vec![put("bad", FactVersion::ABSENT, -100)],
            ..CellCommit::empty()
        },
    );
    record(
        &mut cell,
        CellCommit {
            writes: vec![put("over", FactVersion::ABSENT, 2_000_000)],
            ..CellCommit::empty()
        },
    );
    let doomed = FactKey::new(OBJECT_TYPE, "a0009");
    let version = cell.load(&doomed).expect("load").version;
    record(
        &mut cell,
        CellCommit {
            writes: vec![FactWrite {
                key: doomed,
                expected: version,
                mutation: FactMutation::Delete,
            }],
            ..CellCommit::empty()
        },
    );
    let mut mixed = creates("b", 3);
    let key = FactKey::new(OBJECT_TYPE, "a0005");
    let version = cell.load(&key).expect("load").version;
    mixed.writes.push(put("a0005", version, 7));
    record(&mut cell, mixed);
    record(
        &mut cell,
        CellCommit {
            writes: vec![put("a0001", FactVersion::ABSENT, 1)],
            ..CellCommit::empty()
        },
    );
    let reader = cell.store().read();
    let receipts: Vec<Value> = reader
        .receipts()
        .expect("receipts")
        .into_iter()
        .map(|receipt| json!({"hash": receipt.hash, "body": receipt.body}))
        .collect();
    let ledger: Vec<Value> = reader
        .ledger(0)
        .expect("ledger")
        .into_iter()
        .map(|entry| {
            json!({"seq": entry.seq, "commitSeq": entry.commit_seq, "world": entry.world,
                "event": entry.event, "prevHash": entry.prev_hash, "hash": entry.hash})
        })
        .collect();
    json!({"steps": steps, "receipts": receipts, "ledger": ledger})
}

#[test]
fn recorded_commit_sequence_replays_identical_receipts() {
    let transcript = serde_json::to_string_pretty(&replay_transcript()).expect("transcript") + "\n";
    assert_eq!(
        transcript,
        serde_json::to_string_pretty(&replay_transcript()).expect("transcript") + "\n",
        "one sequence gives one transcript"
    );
    let path =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/commit-replay.json");
    if std::env::var_os("NORN_RECORD_COMMIT_REPLAY").is_some() {
        std::fs::write(&path, &transcript).expect("record transcript");
    }
    let recorded = std::fs::read_to_string(&path).expect("recorded transcript");
    assert!(
        recorded == transcript,
        "commit receipts differ from the recorded sequence ({}); a behaviour-preserving change must not alter them",
        path.display()
    );
}
