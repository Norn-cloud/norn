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
    clippy::indexing_slicing,
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

fn cell(invariants: usize, laws: usize) -> Cell {
    let model = Model::from_definitions(&[object_type(invariants)], &[])
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

fn put(key: &str, expected: FactVersion, qty: i64) -> FactWrite {
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

/// `creates`, with the `Created` ledger event of each object.
fn creates_with_events(prefix: &str, rows: usize) -> CellCommit<FactEvent, JsonObject, JsonObject> {
    let mut commit = creates(prefix, rows);
    commit.events = commit
        .writes
        .iter()
        .map(|write| FactEvent::Created {
            key: write.key.clone(),
            values: serde_json::from_value(json!({"qty": 1, "label": "item"})).expect("values"),
        })
        .collect();
    commit
}

fn json_counts(counts: &Counts) -> Value {
    counts
        .entries()
        .into_iter()
        .map(|(name, value)| (name.to_owned(), json!(value)))
        .collect::<serde_json::Map<_, _>>()
        .into()
}

/// The counts of the first checked commit on a cell (one object), and of
/// the next one (`rows` fresh objects) on the same cell.
fn measure(invariants: usize, laws: usize, rows: usize) -> (Counts, Counts) {
    let mut cell = cell(invariants, laws);
    let _open = counters::take();
    cell.commit(creates("warm", 1)).expect("first commit");
    let first = counters::take();
    cell.commit(creates("row", rows)).expect("measured commit");
    (first, counters::take())
}

#[test]
fn commit_counters_by_size() {
    const {
        assert!(
            counters::ENABLED,
            "norn-cell tests enable the counters feature"
        );
    }
    let mut lines = Vec::new();
    let mut sample = |axis: &str, invariants: usize, laws: usize, rows: usize| {
        let (first, counts) = measure(invariants, laws, rows);
        lines.push(json!({"axis": axis, "rows": rows, "laws": laws, "invariants": invariants, "first": json_counts(&first), "counts": json_counts(&counts)}));
    };
    for rows in [10, 100, 1_000] {
        sample("rows", INVARIANTS, 4, rows);
    }
    for laws in [10, 100, 1_000] {
        sample("laws", INVARIANTS, laws, 10);
    }
    for invariants in [10, 100, 1_000] {
        sample("invariants", invariants, 4, 10);
    }
    // P5 and ADMIT-CACHE-1 (#1533): definition-derived work is done once
    // per model, not per row or per commit. The first commit on a model
    // compiles each declared invariant and law once and builds one typed
    // environment; every later commit reuses them and compiles nothing,
    // whatever its row count. Laws are decoded once per commit's catalog.
    for line in &lines {
        let declared = |name: &str| line[name].as_u64().expect("size");
        let (first, counts) = (&line["first"], &line["counts"]);
        assert_eq!(
            first["expression_compiles"],
            json!(declared("invariants") + declared("laws")),
            "first compiles: {line}"
        );
        assert_eq!(
            first["expression_environment_builds"],
            json!(1),
            "first environments: {line}"
        );
        assert_eq!(counts["expression_compiles"], json!(0), "compiles: {line}");
        assert_eq!(
            counts["expression_environment_builds"],
            json!(0),
            "environments: {line}"
        );
        assert_eq!(
            counts["effective_model_builds"],
            json!(0),
            "effective models: {line}"
        );
        for counted in [first, counts] {
            assert_eq!(
                counted["law_decodes"],
                json!(declared("laws")),
                "decodes: {line}"
            );
        }
    }
    for line in &lines {
        println!("COMMIT-COUNTERS {line}");
        let counts = &line["counts"];
        for name in ["sql_statements", "rows_written", "bytes_written", "hashes"] {
            assert!(
                counts[name].as_u64().is_some_and(|n| n > 0),
                "{name} counted: {line}"
            );
        }
    }
}

/// A type whose invariant reads a declared decimal, so it runs on the exact
/// profile: typed check and lowering, now prepared once per catalog.
fn decimal_cell() -> Cell {
    let object_type: ObjectTypeDef = serde_json::from_value(json!({
        "name": "bench.Priced",
        "properties": [{"id": "amount", "type": {"kind": "decimal", "scale": 2, "unit": {"system": "ucum", "code": "1"}}}],
        "invariants": [{"id": "nonnegative", "expression": "self.amount >= 0", "message": "amount is not negative"}]
    }))
    .expect("decimal type");
    let model = Model::from_definitions(&[object_type], &[]).expect("model");
    Cell::new(
        CellStore::open_in_memory(model).expect("SQLite"),
        "org_demo/dev",
        |command| command.payload.clone(),
    )
    .expect("cell")
}

fn decimal_creates(prefix: &str, rows: usize) -> CellCommit<FactEvent, JsonObject, JsonObject> {
    let amount = norn_ir::RuntimeValue::from(norn_ir::Decimal::parse("1.25").expect("decimal"));
    CellCommit {
        writes: (0..rows)
            .map(|i| FactWrite {
                key: FactKey::new("bench.Priced", format!("{prefix}{i:04}")),
                expected: FactVersion::ABSENT,
                mutation: FactMutation::Put {
                    values: JsonObject::from([("amount".to_owned(), amount.clone())]),
                },
            })
            .collect(),
        ..CellCommit::empty()
    }
}

#[test]
fn decimal_invariants_prepare_once_per_commit() {
    let mut by_rows = Vec::new();
    for rows in [10, 100] {
        let mut cell = decimal_cell();
        cell.commit(decimal_creates("warm", 1))
            .expect("warm-up commit");
        let _warm_up = counters::take();
        cell.commit(decimal_creates("row", rows))
            .expect("measured commit");
        let counts = counters::take();
        println!(
            "COMMIT-COUNTERS {}",
            json!({"axis": "decimal-rows", "rows": rows, "counts": json_counts(&counts)})
        );
        by_rows.push(counts.expression_compiles);
    }
    assert_eq!(
        by_rows[0], by_rows[1],
        "decimal compiles grow with rows: {by_rows:?}"
    );
}

fn decimal_put(key: &str, amount: &str) -> CellCommit<FactEvent, JsonObject, JsonObject> {
    let amount = norn_ir::RuntimeValue::from(norn_ir::Decimal::parse(amount).expect("decimal"));
    CellCommit {
        writes: vec![FactWrite {
            key: FactKey::new("bench.Priced", key),
            expected: FactVersion::ABSENT,
            mutation: FactMutation::Put {
                values: JsonObject::from([("amount".to_owned(), amount)]),
            },
        }],
        ..CellCommit::empty()
    }
}

/// The prepared exact path decides a decimal invariant as per-row evaluation
/// did: the boundary `0.00` holds, one cent below it is refused with the
/// invariant's code and message.
#[test]
fn a_prepared_decimal_invariant_holds_at_zero_and_refuses_one_cent_below() {
    let mut cell = decimal_cell();
    cell.commit(decimal_creates("warm", 1))
        .expect("warm-up commit");
    cell.commit(decimal_put("zero", "0.00"))
        .expect("0.00 meets `self.amount >= 0`");
    let refused = cell
        .commit(decimal_put("negative", "-0.01"))
        .expect_err("-0.01 breaks `self.amount >= 0`");
    assert_eq!(refused.code(), "commit.invariant_violation");
    assert!(
        refused.to_string().contains("amount is not negative"),
        "{refused}"
    );
}

/// One recorded commit sequence: every kernel result, then every stored
/// receipt and ledger entry, as one JSON document.
fn replay_transcript() -> Value {
    let mut cell = cell(INVARIANTS, 3);
    let mut steps = Vec::new();
    let mut record = |cell: &mut Cell, commit| {
        let result = match cell.commit(commit) {
            Ok(receipt) => json!({"ok": receipt}),
            Err(error) => json!({"refused": error.code(), "message": error.to_string()}),
        };
        steps.push(result);
    };
    record(&mut cell, creates_with_events("a", 10));
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
    let mut mixed = creates_with_events("b", 3);
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
    assert!(!ledger.is_empty(), "the sequence exercises ledger chaining");
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
