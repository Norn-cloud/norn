//! MANIFEST-BENCH-1 (#1381): BPI Challenge 2019 admitted through one dataset
//! manifest, measured phase by phase, beside the per-row path on a slice.
//!
//! Opt-in: the log is fetched at test time by `scripts/bench_bpi.py`, with
//! its pinned SHA-256 and licence check, and flattened by its `csv` command
//! outside the checkout. Nothing of it is committed.
#![cfg(all(feature = "duckdb", feature = "native"))]
#![allow(
    clippy::arithmetic_side_effects,
    clippy::as_conversions,
    clippy::cast_precision_loss,
    clippy::expect_used,
    clippy::print_stdout,
    clippy::too_many_lines,
    missing_docs,
    reason = "benchmark driver"
)]

#[path = "support/dataset_cell.rs"]
mod support;

use std::collections::BTreeMap;
use std::io::BufRead as _;
use std::time::Instant;

use norn_cell::analytics::engine::{Engine, Settings};
use norn_cell::analytics::{
    FileFormat, FileSource, LandRequest, LoadRequest, SettleRequest, TenantId,
};
use norn_cell::dataset_manifest::{
    DatasetManifest, MANIFEST_VERSION, ManifestCounts, ManifestFile, ManifestSource,
};
use norn_ir::RuntimeValue as Value;
use norn_kernel::{LogicalTime, ObservationId, Observed, Provenance};
use serde_json::json;
use support::{Cell, Field, cell};

const TYPE: &str = "bench.Bpi19Event";

/// The flattened log's columns (`bench_bpi.py csv`) and their property types.
fn fields() -> Vec<Field> {
    let text = |id: &'static str, required| Field {
        id,
        kind: json!({"kind":"text"}),
        required,
    };
    vec![
        text("event_id", true),
        text("case_id", true),
        text("item_type", false),
        text("document_type", false),
        text("company", false),
        text("vendor", false),
        text("spend_area", false),
        text("item_category", false),
        text("activity", true),
        Field {
            id: "occurred_at",
            kind: json!({"kind":"timestamp","timezone":"utc"}),
            required: true,
        },
        text("resource", false),
        text("user", false),
        Field {
            id: "net_worth",
            kind: json!({"kind":"decimal","scale":2,"unit":{"system":"ucum","code":"1"},"additivity":{"kind":"non_additive"}}),
            required: false,
        },
    ]
}

/// The laws the benchmark admits: each reads present values, and one reads
/// a value that is sometimes absent.
const LAWS: [(&str, &str, &str); 3] = [
    (
        "bpi_net_worth_nonnegative",
        "error",
        "(self.net_worth ?? 0) >= 0",
    ),
    ("bpi_activity_named", "error", "not (self.activity == '')"),
    ("bpi_case_named", "warning", "self.case_id != ''"),
];

fn env_u64(name: &str, default: u64) -> u64 {
    std::env::var(name)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

#[test]
#[ignore = "requires the BPI 2019 log flattened outside the checkout"]
fn bpi_2019_through_one_manifest() {
    let path = std::env::var("NORN_BENCH_BPI_CSV").expect("flattened log path");
    let threads = u32::try_from(env_u64("NORN_BENCH_THREADS", 4)).expect("threads");
    let memory = std::env::var("NORN_BENCH_MEMORY").unwrap_or_else(|_| "4GB".into());
    let per_row = env_u64("NORN_BENCH_PER_ROW", 2_000);
    let fields = fields();
    let (_dir, mut cell) = cell(TYPE, &fields, &LAWS);
    let started = Instant::now();
    let bytes = std::fs::read(&path).expect("log");
    let sha256 = norn_cell::analytics::hex_digest(&bytes);
    let source_bytes = bytes.len() as u64;
    drop(bytes);
    let hashed = started.elapsed();
    let engine_dir = tempfile::tempdir().expect("engine dir");
    let engine = Engine::new(Settings {
        root: engine_dir.path().to_path_buf(),
        memory_limit: memory.clone(),
        threads,
    })
    .expect("engine");
    let copy = engine.scratch().join("bpi19.csv");
    std::fs::copy(&path, &copy).expect("scratch copy");
    let store = cell.store();
    let table = store.analytics_table(TYPE).expect("table");
    let checks = store.dataset_checks(TYPE, &table).expect("checks");
    let lowered = checks.iter().filter(|c| c.predicate.is_some()).count();
    let request = LandRequest {
        load: LoadRequest {
            tenant: TenantId::of_scope("bench"),
            table: table.clone(),
            load: "bpi19".into(),
            file: FileSource {
                url: String::new(),
                uri: "s3://bench/uploads/bpi19.csv".into(),
                sha256: sha256.clone(),
                format: FileFormat::Csv,
            },
            headers: BTreeMap::new(),
        },
        data_url: String::new(),
        held_url: String::new(),
        checks,
        residue_page: 2_000,
    };
    let at = Instant::now();
    let mut landed = engine.land(&request, &copy).expect("landed");
    let land = at.elapsed();
    let (mut residue_rows, mut residue_eval, mut settle_time) = (0u64, 0f64, 0f64);
    while landed.response.residue_total > 0 {
        let page = landed.response.residue.clone();
        residue_rows += page.len() as u64;
        let at = Instant::now();
        let findings = store
            .residue_findings(TYPE, &table, &page)
            .expect("per-row residue");
        residue_eval += at.elapsed().as_secs_f64();
        let at = Instant::now();
        landed = engine
            .settle(&SettleRequest {
                land: request.clone(),
                through: page.last().expect("page").row,
                findings,
            })
            .expect("settled");
        settle_time += at.elapsed().as_secs_f64();
    }
    let response = landed.response;
    let (data, held) = (
        response.data.clone().expect("data file"),
        response.held.clone().expect("held file"),
    );
    let binding = BTreeMap::new();
    let manifest = DatasetManifest {
        version: MANIFEST_VERSION.into(),
        object_type: TYPE.into(),
        load: "bpi19".into(),
        source: ManifestSource {
            uri: "s3://bench/uploads/bpi19.csv".into(),
            sha256,
            format: FileFormat::Csv,
            bytes: source_bytes,
        },
        binding: binding.clone(),
        table,
        pins: store.dataset_pins(TYPE, &binding).expect("pins"),
        data: ManifestFile {
            uri: "s3://bench/datasets/data.parquet".into(),
            sha256: data.sha256.clone(),
            rows: data.rows,
            bytes: data.bytes,
        },
        held: ManifestFile {
            uri: "s3://bench/datasets/held.parquet".into(),
            sha256: held.sha256.clone(),
            rows: held.rows,
            bytes: held.bytes,
        },
        counts: ManifestCounts {
            source: response.load.rows + response.load.rejected,
            admitted: response.load.rows,
            held: response.load.reasons.clone(),
            findings: response.findings.clone(),
            held_by_rules: response.held_by_rules,
        },
        valid_time: response.valid_time.clone(),
    };
    let at = Instant::now();
    let (hash, receipt) = cell
        .commit_dataset_manifest(&manifest)
        .expect("manifest committed");
    let commit = at.elapsed();
    let total = started.elapsed();
    let rows = response.load.rows + response.load.rejected;

    // The per-row path on the first rows of the same log, for the same laws.
    let baseline = (per_row > 0).then(|| per_row_rate(&mut cell, &path, per_row));
    println!(
        "BENCH-MANIFEST {}",
        json!({
            "rows": rows, "admitted": response.load.rows, "refused": response.load.rejected,
            "refusedReasons": response.load.reasons, "heldByRules": response.held_by_rules,
            "findings": response.findings, "rulesLowered": lowered, "rules": LAWS.len(),
            "residueRows": residue_rows, "sourceBytes": source_bytes,
            "dataFile": {"rows": data.rows, "bytes": data.bytes}, "heldFile": {"rows": held.rows, "bytes": held.bytes},
            "validTime": response.valid_time, "manifest": hash, "commitSeq": receipt.body.commit_seq,
            "engine": {"threads": threads, "memory": memory},
            "seconds": {"hashSource": hashed.as_secs_f64(), "landAndCheck": land.as_secs_f64(),
                "residuePerRow": residue_eval, "residueSettle": settle_time,
                "commit": commit.as_secs_f64(), "total": total.as_secs_f64()},
            "rowsPerSecond": rows as f64 / total.as_secs_f64(),
            "perRowBaseline": baseline,
        })
    );
}

/// Rows per second of the per-row governed path (map, check, commit, receipt
/// per row) over the first `limit` rows of the log.
fn per_row_rate(cell: &mut Cell, path: &str, limit: u64) -> serde_json::Value {
    let input = norn_cell::mapping_bundle::MappingInput {
        mapping: "bench.dataset.mapping".into(),
        dataset: "Rows".into(),
        connector_instance: "public".into(),
        partition: "bpi19".into(),
        recorded_at: norn_ir::provenance::UtcTimestamp("2026-11-04T00:00:00Z".into()),
    };
    let reader = std::io::BufReader::new(std::fs::File::open(path).expect("log"));
    let mut lines = reader.lines();
    let header: Vec<String> = lines
        .next()
        .expect("header")
        .expect("header")
        .split(',')
        .map(str::to_owned)
        .collect();
    let mut outcomes = BTreeMap::<String, u64>::new();
    let started = Instant::now();
    let mut rows = 0u64;
    for line in lines.take(usize::try_from(limit).expect("limit")) {
        let line = line.expect("line");
        let mut record = csv_fields(&line).into_iter();
        let mut payload = serde_json::Map::new();
        for name in &header {
            let value = record.next().unwrap_or_default();
            if value.is_empty() {
                continue;
            }
            // A decimal field is a JSON number in the source, as an extract carries it.
            let value = if name == "net_worth" {
                serde_json::from_str(&value).unwrap_or_else(|_| json!(value))
            } else {
                json!(value)
            };
            payload.insert(name.clone(), value);
        }
        rows += 1;
        let id = payload
            .get("event_id")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default()
            .to_owned();
        let text = serde_json::Value::Object(payload).to_string();
        let observed = Observed {
            id: ObservationId(format!("row-{rows}")),
            provenance: Provenance {
                source: "Bench".into(),
                source_id: id.clone(),
                mapping_version: "pipeline/1".into(),
                observed_at: LogicalTime::from_millis(1_793_750_400_000),
            },
            input: norn_ir::runtime_object! {"contract"=>"norn.raw_observation/v1", "source"=>"Bench",
            "source_id"=>id, "event_id"=>format!("row-{rows}"), "source_version"=>"1",
            "pipeline_version"=>"pipeline/1", "kind"=>"upsert", "extracted_at"=>1_793_750_400_000u64,
            "payload"=>Value::from_json_text(&text).expect("payload"),
            "payload_digest"=>norn_ir::canonical::HashProfile::Norn1.hash_json(&text).expect("digest")},
        };
        let outcome = cell
            .inject_mapping_bundle(&observed, &input)
            .and_then(|(bundle, _)| cell.process_mapping_bundle(&bundle));
        let key = match outcome {
            Ok(true) => "processed".to_owned(),
            Ok(false) => "not pending".to_owned(),
            Err(error) => error.to_string().chars().take(80).collect(),
        };
        *outcomes.entry(key).or_default() += 1;
    }
    let seconds = started.elapsed().as_secs_f64();
    json!({"rows": rows, "seconds": seconds, "rowsPerSecond": rows as f64 / seconds, "outcomes": outcomes})
}

/// One RFC 4180 line's fields (the flattened log quotes fields with commas).
fn csv_fields(line: &str) -> Vec<String> {
    let mut fields = Vec::new();
    let mut field = String::new();
    let mut quoted = false;
    let mut chars = line.chars().peekable();
    while let Some(c) = chars.next() {
        match (c, quoted) {
            ('"', true) if chars.peek() == Some(&'"') => {
                field.push('"');
                chars.next();
            }
            ('"', _) => quoted = !quoted,
            (',', false) => fields.push(std::mem::take(&mut field)),
            (c, _) => field.push(c),
        }
    }
    fields.push(field);
    fields
}
