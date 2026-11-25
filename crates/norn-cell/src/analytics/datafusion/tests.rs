//! B1 bounds on the adapter: a load past a cell or footer bound refuses and
//! leaves nothing behind; an engine holds a bounded number of landings and
//! evicts idle ones past their time to live, their directories with them.

use std::collections::BTreeMap;
use std::io::Write as _;

use super::super::{FileFormat, FileSource, LandRequest, LoadRequest, TableSchema, TenantId};
use super::{EXHAUSTED, Engine, Settings};

fn engine(memory: &str) -> (tempfile::TempDir, Engine) {
    let dir = tempfile::tempdir().expect("engine dir");
    let engine = Engine::new(Settings {
        root: dir.path().to_path_buf(),
        memory_limit: memory.to_owned(),
        threads: 1,
    })
    .expect("engine");
    (dir, engine)
}

fn tenant() -> TenantId {
    TenantId::of_scope("bounds")
}

fn table() -> TableSchema {
    serde_json::from_value(serde_json::json!({
        "name": "t.Wide",
        "columns": [{"id": "text", "kind": "text", "nullable": true}],
        "pin": "wide"
    }))
    .expect("table")
}

fn source(engine: &Engine, bytes: &[u8], format: FileFormat) -> (std::path::PathBuf, LoadRequest) {
    let path = engine
        .scratch(&tenant())
        .expect("scratch")
        .join(format!("source-{}", bytes.len()));
    std::fs::File::create(&path)
        .and_then(|mut file| file.write_all(bytes))
        .expect("source");
    let request = LoadRequest {
        tenant: tenant(),
        table: table(),
        load: "load-1".to_owned(),
        file: FileSource {
            url: String::new(),
            uri: "s3://synthetic/source".to_owned(),
            sha256: super::super::hex_digest(bytes),
            format,
        },
        headers: BTreeMap::new(),
    };
    (path, request)
}

fn staging_is_empty(dir: &std::path::Path) -> bool {
    let staging = dir.join("t").join(tenant().as_str()).join("staging");
    std::fs::read_dir(staging).map_or(true, |mut entries| entries.next().is_none())
}

#[test]
fn a_cell_past_its_bound_refuses_and_leaves_nothing_staged() {
    let (dir, engine) = engine("8MB");
    let mut csv = b"text\n".to_vec();
    csv.extend(std::iter::repeat_n(b'w', 3 << 20));
    csv.push(b'\n');
    let (path, request) = source(&engine, &csv, FileFormat::Csv);
    let refused = engine
        .load(&request, &path)
        .expect_err("a 3 MiB cell under 8MB");
    assert_eq!(refused.code, EXHAUSTED);
    assert!(staging_is_empty(dir.path()));
}

#[test]
fn a_footer_past_its_bound_refuses_before_it_is_parsed() {
    let (_dir, engine) = engine("8MB");
    let mut file = b"PAR1".to_vec();
    file.extend([0_u8; 64]);
    file.extend((64_u32 << 20).to_le_bytes());
    file.extend(b"PAR1");
    let (path, request) = source(&engine, &file, FileFormat::Parquet);
    let refused = engine.load(&request, &path).expect_err("a 64 MiB footer");
    assert_eq!(refused.code, EXHAUSTED);
}

/// A landing whose one row is left as residue, so the engine holds it.
fn held_landing(engine: &Engine, landing: usize) -> super::Outcome<super::super::LandResponse> {
    let csv = b"text\nann\n".to_vec();
    let (_path, load) = source(engine, &csv, FileFormat::Csv);
    let request: LandRequest = serde_json::from_value(serde_json::json!({
        "load": load,
        "pins": {"definitionsVersion": 1, "modelHash": "m", "binding": "b", "mapping": "p", "laws": "l"},
        "landing": format!("landing-{landing}"),
        "attempt": "",
        "checks": [{"rule": "r", "label": "l", "holds": true, "expect": true, "fuel": 1}],
        "residuePage": 10
    }))
    .map_err(|error| super::engine_error(error.to_string()))?;
    engine.land(&request, |mut file| {
        file.write_all(&csv)
            .map_err(|error| super::engine_error(error.to_string()))
    })
}

#[test]
fn landings_are_bounded_and_idle_ones_expire_with_their_files() {
    let (dir, mut engine) = engine("64MB");
    for landing in 0..16 {
        let held = held_landing(&engine, landing).expect("a landing left with residue");
        assert_eq!(held.residue_total, 1);
    }
    let refused = held_landing(&engine, 16).expect_err("a tenant's seventeenth landing");
    assert_eq!(refused.code, EXHAUSTED);
    let scratch = dir.path().join("t").join(tenant().as_str()).join("scratch");
    let count = || std::fs::read_dir(&scratch).map_or(0, Iterator::count);
    assert_eq!(count(), 16);
    engine.attempt_ttl = std::time::Duration::ZERO;
    held_landing(&engine, 17).expect("expired landings make room");
    assert_eq!(count(), 1, "the expired landings' directories are removed");
}

/// N6.5: an instant filter over a value without its ordering key is unknown,
/// so it holds neither negated nor not, as SQL's `NULL` does.
#[test]
fn a_keyless_instant_satisfies_no_filter_negated_or_not() {
    use super::query::{Typed, TypedFilter, filters_hold};
    use super::values::Cell;
    let keyless = [Cell::Text("2024-01-01".to_owned())];
    for negate in [false, true] {
        let filter = TypedFilter {
            index: 0,
            negate,
            test: Typed::Instant(0, 0),
        };
        assert_eq!(filters_hold(&[filter], &keyless), (false, false));
    }
}

/// Blocker R2: a Parquet source decodes only its declared columns, and a
/// dictionary-encoded string column is read as views of its dictionary: an
/// undeclared 32 MiB column and a 32 KiB value repeated 1,024 times load
/// under a 16 MiB chunk bound.
#[test]
fn a_parquet_source_decodes_only_declared_columns_as_dictionary_views() {
    use std::sync::Arc;

    use datafusion::arrow::array::{ArrayRef, StringArray};
    use datafusion::arrow::datatypes::{DataType, Field, Schema};
    use datafusion::arrow::record_batch::RecordBatch;

    let (_dir, engine) = engine("64MB");
    let wide = "w".repeat(32 << 10);
    let distinct: Vec<String> = (0..1024_u32)
        .map(|i| format!("{i:08}{}", "u".repeat(32 << 10)))
        .collect();
    let schema = Arc::new(Schema::new(vec![
        Field::new("undeclared", DataType::Utf8, true),
        Field::new("text", DataType::Utf8, true),
    ]));
    let columns: Vec<ArrayRef> = vec![
        Arc::new(StringArray::from_iter_values(distinct.iter())),
        Arc::new(StringArray::from_iter_values(std::iter::repeat_n(
            wide.as_str(),
            1024,
        ))),
    ];
    let batch = RecordBatch::try_new(Arc::clone(&schema), columns).expect("batch");
    let mut bytes = Vec::new();
    let mut writer =
        parquet::arrow::ArrowWriter::try_new(&mut bytes, schema, None).expect("writer");
    writer.write(&batch).expect("write");
    writer.close().expect("close");
    let (path, request) = source(&engine, &bytes, FileFormat::Parquet);
    let loaded = engine.load(&request, &path).expect("a bounded load");
    assert_eq!((loaded.rows, loaded.rejected), (1024, 0));
}

/// R3 (B2, B3): wide plain-encoded Parquet rows (200 KiB each, 60 MiB in all)
/// load under a 16 MiB chunk bound: each row group is decoded, staged and
/// merged in batches its own rows' size allows.
#[test]
fn wide_parquet_rows_load_in_batches_their_size_allows() {
    use std::sync::Arc;

    use datafusion::arrow::array::{ArrayRef, StringArray};
    use datafusion::arrow::datatypes::{DataType, Field, Schema};
    use datafusion::arrow::record_batch::RecordBatch;
    use parquet::file::properties::WriterProperties;

    let (_dir, engine) = engine("64MB");
    let rows: Vec<String> = (0..300_u32)
        .map(|i| format!("{i:08}{}", "p".repeat(200 << 10)))
        .collect();
    let schema = Arc::new(Schema::new(vec![Field::new("text", DataType::Utf8, true)]));
    let column: ArrayRef = Arc::new(StringArray::from_iter_values(rows.iter()));
    let batch = RecordBatch::try_new(Arc::clone(&schema), vec![column]).expect("batch");
    let properties = WriterProperties::builder()
        .set_dictionary_enabled(false)
        .build();
    let mut bytes = Vec::new();
    let mut writer =
        parquet::arrow::ArrowWriter::try_new(&mut bytes, schema, Some(properties)).expect("writer");
    writer.write(&batch).expect("write");
    writer.close().expect("close");
    let (path, request) = source(&engine, &bytes, FileFormat::Parquet);
    let loaded = engine
        .load(&request, &path)
        .expect("a wide load in small batches");
    assert_eq!((loaded.rows, loaded.rejected), (300, 0));
}

/// R3 (N5): three held parts, each in key order and with tied keys across
/// parts, merge into one file in key order with every row kept.
#[test]
fn held_parts_merge_in_key_order_with_ties() {
    use super::columnar::Sink;
    use super::files::{Caps, Meter};
    use super::laws::HeldRow;
    use super::staging::{held_schema, write_held};

    let dir = tempfile::tempdir().expect("dir");
    let table = table();
    let held = |row: u64, rule: Option<&str>, reason: &str| HeldRow {
        row,
        reason: reason.to_owned(),
        rule: rule.map(str::to_owned),
        holds: true,
        values: vec![Some(format!("{row}-{reason}"))],
    };
    let parts = [
        vec![held(1, None, "bulk.required"), held(3, Some("r1"), "l")],
        vec![held(1, Some("r1"), "l"), held(2, None, "bulk.not_date")],
        vec![held(1, Some("r1"), "l"), held(3, Some("r0"), "a")],
    ];
    let mut paths = Vec::new();
    for (at, rows) in parts.iter().enumerate() {
        let path = dir.path().join(format!("part-{at}.parquet"));
        let mut sink = Sink::create(&path, &held_schema(&table)).expect("part");
        write_held(&mut sink, &table, rows).expect("rows");
        sink.finish().expect("finish");
        paths.push(path);
    }
    let out = dir.path().join("held.parquet");
    let mut sink = Sink::create(&out, &held_schema(&table)).expect("out");
    let meter = Meter::new(Caps::of(1 << 20));
    super::merge::merge_held(&table, &paths, &meter, &mut sink).expect("merge");
    assert_eq!(sink.finish().expect("finish"), 6);
    let file = std::fs::File::open(&out).expect("open");
    let reader = parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder::try_new(file)
        .expect("reader")
        .build()
        .expect("build");
    let mut keys = Vec::new();
    for batch in reader {
        let batch = batch.expect("batch");
        let rows = super::columnar::system(&batch, ":row", super::columnar::u64_at).expect("rows");
        let rules =
            super::columnar::system(&batch, ":rule", super::columnar::text_at).expect("rules");
        let reasons =
            super::columnar::system(&batch, ":reason", super::columnar::text_at).expect("reasons");
        for ((row, rule), reason) in rows.into_iter().zip(rules).zip(reasons) {
            keys.push((row, rule, reason));
        }
    }
    let mut sorted = keys.clone();
    sorted.sort();
    assert_eq!(keys, sorted);
    assert_eq!(keys.len(), 6);
}
