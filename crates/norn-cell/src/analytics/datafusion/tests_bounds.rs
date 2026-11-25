//! #1765 NB1–NB6: the pre-default bounds of the adapter's loads. A Parquet
//! batch is bounded by the file's real pages, not its footer's averages; a
//! declared field of an unreadable type refuses before anything decodes; a
//! file's field count is bounded alike for CSV and Parquet; staged parts
//! are written without dictionaries; and no chunk or held-set batch can
//! overflow a `Utf8` array's offsets.

use std::collections::BTreeMap;
use std::io::Write as _;
use std::sync::Arc;

use datafusion::arrow::array::{ArrayRef, BinaryArray, StringArray, UInt64Array};
use datafusion::arrow::datatypes::{DataType, Field, Schema};
use datafusion::arrow::record_batch::RecordBatch;
use parquet::file::properties::WriterProperties;

use super::super::{FileFormat, FileSource, LoadRequest, TableSchema, TenantId};
use super::columnar::{MAX_BATCH_BYTES, Sink};
use super::files::{Caps, MAX_FIELDS, Meter, read_source};
use super::laws::HeldRow;
use super::staging::held_slices;
use super::{EXHAUSTED, Engine, Settings};

const ENCODING: &str = "bulk.unsupported_encoding";

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

fn table() -> TableSchema {
    serde_json::from_value(serde_json::json!({
        "name": "t.Bound",
        "columns": [{"id": "text", "kind": "text", "nullable": true}],
        "pin": "bound"
    }))
    .expect("table")
}

fn source(engine: &Engine, bytes: &[u8], format: FileFormat) -> (std::path::PathBuf, LoadRequest) {
    let tenant = TenantId::of_scope("bounds");
    let path = engine
        .scratch(&tenant)
        .expect("scratch")
        .join(format!("source-{}", super::super::hex_digest(bytes)));
    std::fs::File::create(&path)
        .and_then(|mut file| file.write_all(bytes))
        .expect("source");
    let request = LoadRequest {
        tenant,
        table: table(),
        load: format!(
            "load-{}",
            super::super::hex_digest(bytes)
                .get(..16)
                .unwrap_or_default()
        ),
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

fn array_ref(array: impl datafusion::arrow::array::Array + 'static) -> ArrayRef {
    Arc::new(array)
}

fn binary_column() -> ArrayRef {
    array_ref(BinaryArray::from_iter_values([b"ab".as_slice()]))
}

fn parquet(batch: &RecordBatch, properties: WriterProperties) -> Vec<u8> {
    let mut bytes = Vec::new();
    let mut writer =
        parquet::arrow::ArrowWriter::try_new(&mut bytes, batch.schema(), Some(properties))
            .expect("writer");
    writer.write(batch).expect("write");
    writer.close().expect("close");
    bytes
}

fn text_batch(values: &[String]) -> RecordBatch {
    let schema = Arc::new(Schema::new(vec![Field::new("text", DataType::Utf8, true)]));
    let column: ArrayRef = Arc::new(StringArray::from_iter_values(values.iter()));
    RecordBatch::try_new(schema, vec![column]).expect("batch")
}

/// NB1: four adjacent 3 MiB rows among 4,000 tiny ones, in one row group.
/// The footer's average row (about 3 KiB) would size batches of 1,024 rows
/// holding all four (12 MiB, past the 8 MiB chunk); the real pages size
/// them so every batch stays within half a chunk, and the file loads.
#[test]
fn a_skewed_row_group_loads_in_batches_its_pages_allow() {
    let (_dir, engine) = engine("32MB");
    let mut values: Vec<String> = (0..4_000_u32).map(|i| format!("t{i:08}")).collect();
    for at in 0..4_usize {
        values.insert(
            2_000_usize.saturating_add(at),
            format!("{at}{}", "g".repeat(3 << 20)),
        );
    }
    let properties = WriterProperties::builder()
        .set_dictionary_enabled(false)
        .set_write_batch_size(1)
        .set_data_page_row_count_limit(1)
        .build();
    let bytes = parquet(&text_batch(&values), properties);
    let (path, request) = source(&engine, &bytes, FileFormat::Parquet);
    let loaded = engine.load(&request, &path).expect("a skewed load");
    assert_eq!((loaded.rows, loaded.rejected), (4_004, 0));
}

/// NB1: a dictionary page larger than half a chunk refuses before the
/// reader would allocate it, whatever the footer says of the column.
#[test]
fn a_huge_dictionary_page_refuses_before_it_decodes() {
    let (_dir, engine) = engine("32MB");
    let values: Vec<String> = (0..4_u8)
        .flat_map(|at| std::iter::repeat_n(format!("{at}{}", "d".repeat(2 << 20)), 8))
        .collect();
    let properties = WriterProperties::builder()
        .set_dictionary_enabled(true)
        .set_dictionary_page_size_limit(64 << 20)
        .build();
    let bytes = parquet(&text_batch(&values), properties);
    let (path, request) = source(&engine, &bytes, FileFormat::Parquet);
    let refused = engine
        .load(&request, &path)
        .expect_err("an 8 MiB dictionary");
    assert_eq!(refused.code, EXHAUSTED);
    assert!(refused.detail.contains("page"), "{}", refused.detail);
}

/// NB4: a declared field of a type no text cast reads (binary, nested)
/// refuses from the file's schema, before the first batch is decoded.
#[test]
fn an_unreadable_field_type_refuses_before_any_batch_decodes() {
    let (_dir, engine) = engine("32MB");
    let binary = RecordBatch::try_new(
        Arc::new(Schema::new(vec![Field::new(
            "text",
            DataType::Binary,
            true,
        )])),
        vec![binary_column()],
    )
    .expect("binary");
    let properties = || {
        WriterProperties::builder()
            .set_column_encoding(
                "text".into(),
                parquet::basic::Encoding::DELTA_LENGTH_BYTE_ARRAY,
            )
            .set_dictionary_enabled(false)
            .build()
    };
    let list = {
        use datafusion::arrow::array::{Array as _, ListArray};
        use datafusion::arrow::datatypes::Int32Type;
        let values = ListArray::from_iter_primitive::<Int32Type, _, _>([Some(vec![Some(1)])]);
        let field = Field::new("text", values.data_type().clone(), true);
        RecordBatch::try_new(Arc::new(Schema::new(vec![field])), vec![array_ref(values)])
            .expect("list")
    };
    for batch in [binary, list] {
        let bytes = parquet(&batch, properties());
        let (path, request) = source(&engine, &bytes, FileFormat::Parquet);
        let refused = engine
            .load(&request, &path)
            .expect_err("an unreadable type");
        assert_eq!(
            (refused.code.as_str(), refused.detail.as_str()),
            (ENCODING, "text is of a type this load does not read")
        );
    }
}

/// NB5: a file of exactly `MAX_FIELDS` fields loads, CSV or Parquet; one
/// more refuses alike, as `bulk.unsupported_encoding`.
#[test]
fn the_field_bound_is_the_same_for_csv_and_parquet() {
    let (_dir, engine) = engine("64MB");
    for fields in [MAX_FIELDS, MAX_FIELDS.saturating_add(1)] {
        let names: Vec<String> = std::iter::once("text".to_owned())
            .chain((1..fields).map(|at| format!("f{at}")))
            .collect();
        let mut csv = names.join(",").into_bytes();
        csv.push(b'\n');
        csv.extend(vec!["v"; fields].join(",").into_bytes());
        csv.push(b'\n');
        let columns: Vec<ArrayRef> = (0..fields)
            .map(|_| array_ref(StringArray::from_iter_values(["v"])))
            .collect();
        let schema = Schema::new(
            names
                .iter()
                .map(|name| Field::new(name, DataType::Utf8, true))
                .collect::<Vec<_>>(),
        );
        let batch = RecordBatch::try_new(Arc::new(schema), columns).expect("wide");
        let bytes = parquet(&batch, WriterProperties::builder().build());
        for (file, format) in [(csv, FileFormat::Csv), (bytes, FileFormat::Parquet)] {
            let (path, request) = source(&engine, &file, format);
            let answer = engine.load(&request, &path);
            if fields == MAX_FIELDS {
                assert_eq!(answer.expect("at the bound").rows, 1, "{format:?}");
            } else {
                let refused = answer.expect_err("past the bound");
                assert_eq!(
                    (refused.code.as_str(), refused.detail.as_str()),
                    (ENCODING, "the file declares too much structure"),
                    "{format:?}"
                );
            }
        }
    }
}

/// NB6: a typed part whose row has no `:row` number refuses the trusted
/// pass instead of writing a row the held set cannot name.
#[test]
fn a_typed_row_without_its_number_refuses_the_trusted_pass() {
    let dir = tempfile::tempdir().expect("dir");
    let schema = Arc::new(Schema::new(vec![Field::new(
        ":row",
        DataType::UInt64,
        true,
    )]));
    let rows: ArrayRef = Arc::new(UInt64Array::from(vec![Some(1), None]));
    let batch = RecordBatch::try_new(Arc::clone(&schema), vec![rows]).expect("batch");
    let typed = dir.path().join("typed.parquet");
    let mut part = Sink::staged(&typed, &schema).expect("part");
    part.write(&batch).expect("write");
    part.finish().expect("finish");
    let mut sink = Sink::create(&dir.path().join("data.parquet"), &schema).expect("sink");
    let meter = Meter::new(Caps::of(1 << 20));
    let refused = super::merge::trusted(&typed, &[], &meter, &mut sink, &mut |_| Ok(()))
        .expect_err("a row without its number");
    assert_eq!(refused.detail, "a typed row without its number");
}

/// NB2: a staged part is written without dictionary encoding, so its
/// row groups' average row size is the size its rows decode to; a final
/// file keeps its dictionaries.
#[test]
fn staged_parts_are_written_without_dictionaries() {
    let dir = tempfile::tempdir().expect("dir");
    let batch = text_batch(&vec!["r".repeat(4096); 256]);
    let schema = batch.schema();
    let mut uses_dictionary = Vec::new();
    for (name, staged) in [("staged.parquet", true), ("final.parquet", false)] {
        let path = dir.path().join(name);
        let mut sink = if staged {
            Sink::staged(&path, &schema)
        } else {
            Sink::create(&path, &schema)
        }
        .expect("sink");
        sink.write(&batch).expect("write");
        sink.finish().expect("finish");
        let file = std::fs::File::open(&path).expect("open");
        let reader =
            parquet::file::serialized_reader::SerializedFileReader::new(file).expect("reader");
        let metadata = parquet::file::reader::FileReader::metadata(&reader);
        let group = metadata.row_group(0);
        uses_dictionary.push(group.column(0).dictionary_page_offset().is_some());
        if staged {
            // The average a staged reader sizes batches by is the real row.
            let average = group
                .total_byte_size()
                .checked_div(group.num_rows())
                .unwrap_or(0);
            assert!(average >= 4096, "{average}");
        }
    }
    assert_eq!(uses_dictionary, vec![false, true]);
}

fn held(row: u64, bytes: usize) -> HeldRow {
    HeldRow {
        row,
        reason: String::new(),
        rule: Some("r".to_owned()),
        holds: true,
        values: vec![Some("v".repeat(bytes))],
    }
}

/// NB3: held rows are written in slices of at most the batch bound, a row
/// alone past it refuses, and the chunk bound never passes it however large
/// the memory bound is.
#[test]
fn held_rows_and_chunks_stay_within_the_batch_bound() {
    let rows = [held(1, 40), held(2, 40), held(3, 40), held(4, 100)];
    let slices = held_slices(&rows, 100).expect("slices");
    assert_eq!(slices, vec![0..2, 2..3, 3..4]);
    assert_eq!(held_slices(&[], 100).expect("none").len(), 0);
    let refused = held_slices(&[held(1, 101)], 100).expect_err("one row past the bound");
    assert_eq!(refused.code, EXHAUSTED);
    assert_eq!(Caps::of(64 << 30).flush, MAX_BATCH_BYTES);
    assert_eq!(Caps::of(1 << 20).flush, 128 << 10);
}

/// NB3: rows are handed on before a chunk would pass its flush bound, so
/// every chunk's text stays within it.
#[test]
fn a_chunk_is_handed_on_before_it_passes_its_flush_bound() {
    let (_dir, engine) = engine("32MB");
    let mut csv = b"text\n".to_vec();
    for at in 0..64_u32 {
        csv.extend(format!("{at:02}{}\n", "c".repeat(98)).into_bytes());
    }
    let (path, request) = source(&engine, &csv, FileFormat::Csv);
    let caps = Caps {
        flush: 250,
        ..Caps::of(1 << 20)
    };
    let mut chunks = Vec::new();
    read_source(&request, &path, caps, &mut |chunk| {
        let bytes = chunk
            .iter()
            .flat_map(|row| row.texts.iter().flatten())
            .fold(0_usize, |total, text| total.saturating_add(text.len()));
        chunks.push((chunk.len(), bytes));
        Ok(())
    })
    .expect("read");
    assert!(chunks.iter().all(|(_, bytes)| *bytes <= 250), "{chunks:?}");
    assert_eq!(
        chunks.iter().map(|(rows, _)| rows).sum::<usize>(),
        64,
        "every row is handed on"
    );
}
