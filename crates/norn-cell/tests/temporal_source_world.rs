//! ASOF-KNOWLEDGE-1: synthetic knowledge is authenticated by committed receipts.
#![allow(
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    missing_docs,
    reason = "synthetic historical-world correspondence"
)]

use std::collections::BTreeMap;

use norn_cell::reactive::{DecodeBudget, ReactiveLimits};
use norn_cell::{CellStore, CommitBatch, FactWriteRow, Model};
use norn_ir::RuntimeValue as Value;
use norn_testkit::proptest::prelude::*;
use sha2::{Digest, Sha256};

const CSV: &str = include_str!("../../../fixtures/demo/scrap-yield-k1.csv");
const TABLE_SHA: &str = "22cee20298906d6f1ecb8bd3eabc3d405409d7c3091c275cbfe0a8932694c28d";
const BULK_AT: &str = "2026-10-26T00:00:00Z";
const SPLIT: &str = "2026-09-28T00:00:00Z";

fn millis(time: &str) -> u64 {
    u64::try_from(
        chrono::DateTime::parse_from_rfc3339(time)
            .expect("UTC instant")
            .timestamp_millis(),
    )
    .expect("post-epoch fixture")
}

fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn model() -> Model {
    let definition = serde_json::from_value(serde_json::json!({
        "name": "demo.Measurement", "properties": [
            {"id":"mass_mg", "type":{"kind":"integer"}},
            {"id":"effective_at", "type":{"kind":"text"}},
            {"id":"source_document", "type":{"kind":"text"}},
            {"id":"source_sha256", "type":{"kind":"text"}},
            {"id":"table_sha256", "type":{"kind":"text"}}
        ]
    }))
    .expect("synthetic source schema");
    Model::from_definitions(&[definition], &[]).expect("model")
}

fn world(bulk: bool) -> CellStore {
    assert_eq!(
        digest(CSV.as_bytes()),
        TABLE_SHA,
        "consume the integrator's exact table"
    );
    let mut events = Vec::new();
    for line in CSV.lines().skip(1) {
        let row: Vec<_> = line.split(',').collect();
        assert_eq!(row.len(), 10);
        for (role, mass, recorded) in [("input", 4, 7), ("output", 5, 9), ("loss", 6, 9)] {
            let effective = if role == "input" { row[7] } else { row[8] };
            let at = millis(if bulk { BULK_AT } else { row[recorded] });
            let key = format!("{}/{role}", row[2]);
            let values = BTreeMap::from([
                (
                    "mass_mg".into(),
                    Value::from(row[mass].parse::<i64>().expect("exact milligrams")),
                ),
                ("effective_at".into(), Value::from(effective)),
                ("source_document".into(), Value::from(row[2])),
                ("source_sha256".into(), Value::from(digest(line.as_bytes()))),
                ("table_sha256".into(), Value::from(TABLE_SHA)),
            ]);
            events.push((at, key, values));
        }
    }
    events.sort_by(|a, b| (&a.0, &a.1).cmp(&(&b.0, &b.1)));
    let mut store = CellStore::open_in_memory(model()).expect("synthetic world");
    for (logical_time, key, values) in events {
        store
            .commit(&CommitBatch {
                world: if bulk {
                    "synthetic-bulk"
                } else {
                    "synthetic-temporal"
                }
                .into(),
                scope: "org_demo/dev".into(),
                logical_time,
                writes: vec![FactWriteRow::Put {
                    object_type: "demo.Measurement".into(),
                    key,
                    values,
                }],
                ..CommitBatch::default()
            })
            .expect("advance synthetic clock and commit source incarnation");
    }
    store
}

#[test]
fn data_forecast_exports_actual_knowledge_receipts_for_the_pinned_population() {
    let limits = ReactiveLimits {
        max_candidate_bytes_per_commit: 2_000_000,
        ..ReactiveLimits::from_profile(&norn_ir::ResourceLimitProfile::norn_limits_v1())
    };
    for bulk in [false, true] {
        let store = world(bulk);
        let world_id = if bulk {
            "synthetic-bulk"
        } else {
            "synthetic-temporal"
        };
        let mut late_training_labels = 0;
        let mut late_features = 0;
        let mut train = 0;
        let mut holdout = 0;
        let mut budget = DecodeBudget::new(&limits);
        for line in CSV.lines().skip(1) {
            let row: Vec<_> = line.split(',').collect();
            for (role, mass, recorded) in [("input", 4, 7), ("output", 5, 9), ("loss", 6, 9)] {
                let evidence = store
                    .read()
                    .fact_version_evidence_with_budget(
                        world_id,
                        "demo.Measurement",
                        &format!("{}/{role}", row[2]),
                        1,
                        &mut budget,
                    )
                    .expect("authenticate source")
                    .expect("bounded evidence");
                let values = evidence
                    .snapshot()
                    .values
                    .as_ref()
                    .expect("live incarnation");
                assert_eq!(
                    values["mass_mg"],
                    Value::from(row[mass].parse::<i64>().expect("mass"))
                );
                assert_eq!(
                    values["source_sha256"],
                    Value::from(digest(line.as_bytes()))
                );
                assert_eq!(values["table_sha256"], Value::from(TABLE_SHA));
                let receipt = evidence.receipt();
                assert_eq!(receipt.body.world, world_id);
                assert_eq!(
                    receipt.body.logical_time,
                    millis(if bulk { BULK_AT } else { row[recorded] })
                );
                let hash = norn_ir::canonical::HashProfile::Norn1
                    .hash_json(serde_json::to_vec(evidence.snapshot()).expect("snapshot preimage"))
                    .expect("canonical digest");
                assert_eq!(hash, evidence.snapshot_sha256());
                let exported = serde_json::to_value(&evidence).expect("export with lineage");
                assert_eq!(exported["receipt"]["hash"], receipt.hash);
                assert_eq!(exported["snapshotSha256"], hash);
                assert_eq!(
                    exported["receipt"]["body"]["logicalTime"],
                    receipt.body.logical_time
                );
                if row[3] == "gold" {
                    if role == "input" && receipt.body.logical_time > millis(row[8]) {
                        late_features += 1;
                    }
                    if role == "output" {
                        if row[0].parse::<u32>().expect("week") < 12 {
                            train += 1;
                            if receipt.body.logical_time >= millis(SPLIT) {
                                late_training_labels += 1;
                            }
                        } else {
                            holdout += 1;
                        }
                    }
                }
            }
        }
        assert_eq!((train, holdout), (48, 16));
        assert_eq!(
            (late_features, late_training_labels),
            if bulk { (64, 48) } else { (0, 0) }
        );
        assert_eq!(
            store
                .export_commit_count()
                .expect("source receipts retained"),
            240
        );
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(32))]
    #[test]
    fn event_time_claim_cannot_replace_authenticated_recorded_time(
        recorded_at in 0_u64..1_000_000, claimed in any::<u64>(),
    ) {
        let mut store = CellStore::open_in_memory(model()).expect("world");
        store.commit(&CommitBatch {
            world: "synthetic-temporal".into(), scope: "org_demo/dev".into(), logical_time: recorded_at,
            writes: vec![FactWriteRow::Put {
                object_type: "demo.Measurement".into(), key: "input".into(),
                values: BTreeMap::from([
                    ("mass_mg".into(), Value::from(1_i64)),
                    ("effective_at".into(), Value::from(claimed.to_string())),
                    ("source_document".into(), Value::from("synthetic")),
                    ("source_sha256".into(), Value::from(TABLE_SHA)),
                    ("table_sha256".into(), Value::from(TABLE_SHA)),
                ]),
            }], ..CommitBatch::default()
        }).expect("commit");
        let limits = ReactiveLimits::from_profile(&norn_ir::ResourceLimitProfile::norn_limits_v1());
        let evidence = store.read().fact_version_evidence_with_budget(
            "synthetic-temporal", "demo.Measurement", "input", 1, &mut DecodeBudget::new(&limits),
        ).expect("authenticate").expect("budget");
        prop_assert_eq!(evidence.receipt().body.logical_time, recorded_at);
    }
}
