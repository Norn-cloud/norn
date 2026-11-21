//! Parquet modular encryption with a key per column: can arrow-rs (the
//! `parquet` crate DataFusion 55.3 resolves to, feature `encryption`) and
//! DataFusion (feature `parquet_encryption`) write and read it, and does
//! DuckDB read what they write?

use std::fs::File;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use datafusion::arrow::record_batch::RecordBatch;
use datafusion::config::{ConfigFileDecryptionProperties, ConfigFileEncryptionProperties, TableParquetOptions};
use datafusion::dataframe::DataFrameWriteOptions;
use datafusion::prelude::*;
use parquet::arrow::ArrowWriter;
use parquet::arrow::ProjectionMask;
use parquet::arrow::arrow_reader::{ArrowReaderOptions, ParquetRecordBatchReaderBuilder};
use parquet::encryption::decrypt::FileDecryptionProperties;
use parquet::encryption::encrypt::FileEncryptionProperties;
use parquet::file::properties::WriterProperties;
use serde_json::{Value as Json, json};

use crate::df::{exec, register_lake, session};
use crate::{emit, q, secs};

const FOOTER: &[u8; 16] = b"0123456789012345";
const NET_WORTH: &[u8; 16] = b"1234567890123450";
const USER: &[u8; 16] = b"2345678901234501";
const COLUMNS: [&str; 4] = [":row", "activity", "net_worth", "user"];

fn attempt(f: impl FnOnce() -> Result<Json, String>) -> Json {
    match f() {
        Ok(detail) => json!({"ok": true, "detail": detail}),
        Err(error) => json!({"ok": false, "error": error.chars().take(300).collect::<String>()}),
    }
}

fn per_column(plaintext_footer: bool) -> Arc<FileEncryptionProperties> {
    FileEncryptionProperties::builder(FOOTER.to_vec())
        .with_column_key("net_worth", NET_WORTH.to_vec())
        .with_column_key("user", USER.to_vec())
        .with_plaintext_footer(plaintext_footer)
        .build()
        .expect("encryption properties")
}

fn keys(footer: &[u8], columns: &[(&str, &[u8])]) -> Arc<FileDecryptionProperties> {
    let mut builder = FileDecryptionProperties::builder(footer.to_vec());
    for (name, key) in columns {
        builder = builder.with_column_key(name, key.to_vec());
    }
    builder.build().expect("decryption properties")
}

fn write(path: &Path, batches: &[RecordBatch], encryption: Option<Arc<FileEncryptionProperties>>) -> f64 {
    let at = Instant::now();
    let mut props = WriterProperties::builder()
        .set_compression(parquet::basic::Compression::ZSTD(Default::default()))
        .set_max_row_group_size(122_880);
    if let Some(e) = encryption {
        props = props.with_file_encryption_properties(e);
    }
    let mut writer = ArrowWriter::try_new(File::create(path).expect("create"), batches[0].schema(), Some(props.build()))
        .expect("writer");
    for batch in batches {
        writer.write(batch).expect("write");
    }
    writer.close().expect("close");
    secs(at)
}

/// Rows read of `columns`, with `keys` (or none).
fn read(path: &Path, keys: Option<Arc<FileDecryptionProperties>>, columns: &[&str]) -> Result<Json, String> {
    let at = Instant::now();
    let mut options = ArrowReaderOptions::new();
    if let Some(k) = keys {
        options = options.with_file_decryption_properties(k);
    }
    let builder = ParquetRecordBatchReaderBuilder::try_new_with_options(File::open(path).map_err(|e| e.to_string())?, options)
        .map_err(|e| e.to_string())?;
    let schema = builder.parquet_schema();
    let leaves: Vec<usize> = columns
        .iter()
        .map(|c| (0..schema.num_columns()).find(|i| schema.column(*i).name() == *c).expect("column"))
        .collect();
    let mask = ProjectionMask::leaves(schema, leaves);
    let reader = builder.with_projection(mask).build().map_err(|e| e.to_string())?;
    let mut rows = 0;
    for batch in reader {
        rows += batch.map_err(|e| e.to_string())?.num_rows();
    }
    Ok(json!({"rows": rows, "ms": secs(at) * 1e3}))
}

async fn df_read(ctx: &SessionContext, table: &str, path: &Path, keys: Option<Arc<FileDecryptionProperties>>, sql: &str) -> Result<Json, String> {
    let mut options = ParquetReadOptions::default();
    if let Some(k) = keys {
        options = options.file_decryption_properties(ConfigFileDecryptionProperties::try_from(&k).map_err(|e| e.to_string())?);
    }
    ctx.register_parquet(table, path.to_str().expect("path"), options)
        .await
        .map_err(|e| e.to_string())?;
    let frame = ctx.sql(sql).await.map_err(|e| e.to_string())?;
    let batches = frame.collect().await.map_err(|e| e.to_string())?;
    let text = datafusion::arrow::util::pretty::pretty_format_batches(&batches)
        .map_err(|e| e.to_string())?
        .to_string();
    Ok(json!(text))
}

fn duck(path: &Path, write_to: Option<&Path>) -> Result<Json, String> {
    let conn = duckdb::Connection::open_in_memory().map_err(|e| e.to_string())?;
    conn.execute_batch(&format!(
        "PRAGMA add_parquet_key('footer', '{}')",
        std::str::from_utf8(FOOTER).expect("ascii key")
    ))
    .map_err(|e| e.to_string())?;
    let p = path.to_string_lossy().replace('\'', "''");
    if let Some(out) = write_to {
        conn.execute_batch(&format!(
            "COPY (SELECT {} FROM read_parquet('{p}')) TO '{}' (FORMAT parquet, ENCRYPTION_CONFIG {{footer_key: 'footer'}})",
            COLUMNS.iter().map(|c| q(c)).collect::<Vec<_>>().join(", "),
            out.to_string_lossy().replace('\'', "''")
        ))
        .map_err(|e| e.to_string())?;
        return Ok(json!("written with the footer key (DuckDB: one key for every column)"));
    }
    let n: i64 = conn
        .query_row(
            &format!(
                "SELECT count({}) FROM read_parquet('{p}', encryption_config = {{footer_key: 'footer'}})",
                q("user")
            ),
            [],
            |row| row.get(0),
        )
        .map_err(|e| e.to_string())?;
    Ok(json!({"rows": n}))
}

pub async fn probe(dir: PathBuf) {
    let ctx = session(4, false);
    register_lake(&ctx, &dir).await;
    let select = COLUMNS.iter().map(|c| q(c)).collect::<Vec<_>>().join(", ");
    let batches = exec(&ctx, &format!("SELECT {select} FROM events")).await;
    let rows: usize = batches.iter().map(RecordBatch::num_rows).sum();
    let plain = dir.join("enc-plain.parquet");
    let keyed = dir.join("enc-per-column.parquet");
    let keyed_pf = dir.join("enc-per-column-plaintext-footer.parquet");
    let write_plain = write(&plain, &batches, None);
    let write_keyed = write(&keyed, &batches, Some(per_column(false)));
    let _ = write(&keyed_pf, &batches, Some(per_column(true)));
    let all = keys(FOOTER, &[("net_worth", NET_WORTH), ("user", USER)]);
    let only_net_worth = keys(FOOTER, &[("net_worth", NET_WORTH)]);
    let mut cases = serde_json::Map::new();
    let mut put = |name: &str, value: Json| {
        cases.insert(name.into(), value);
    };
    put("arrowrs_read_plaintext", attempt(|| read(&plain, None, &COLUMNS)));
    put("arrowrs_read_all_keys", attempt(|| read(&keyed, Some(all.clone()), &COLUMNS)));
    put(
        "arrowrs_read_net_worth_key_only_without_user",
        attempt(|| read(&keyed, Some(only_net_worth.clone()), &[":row", "activity", "net_worth"])),
    );
    put(
        "arrowrs_read_net_worth_key_only_user_column",
        attempt(|| read(&keyed, Some(only_net_worth.clone()), &["user"])),
    );
    put("arrowrs_read_no_keys", attempt(|| read(&keyed, None, &[":row", "activity"])));
    put(
        "arrowrs_read_wrong_net_worth_key",
        attempt(|| read(&keyed, Some(keys(FOOTER, &[("net_worth", USER), ("user", USER)])), &["net_worth"])),
    );
    put(
        "arrowrs_plaintext_footer_no_keys_plain_columns",
        attempt(|| read(&keyed_pf, None, &[":row", "activity"])),
    );
    put(
        "arrowrs_plaintext_footer_no_keys_net_worth",
        attempt(|| read(&keyed_pf, None, &["net_worth"])),
    );
    let check = format!(
        "SELECT count(*) AS n, sum({nw}) AS total, count(DISTINCT {u}) AS users FROM {{t}}",
        nw = q("net_worth"),
        u = q("user")
    );
    put(
        "datafusion_read_plaintext",
        df_read(&ctx, "p_plain", &plain, None, &check.replace("{t}", "p_plain")).await.map_or_else(|e| json!({"ok": false, "error": e}), |d| json!({"ok": true, "detail": d})),
    );
    put(
        "datafusion_read_all_keys",
        df_read(&ctx, "p_all", &keyed, Some(all.clone()), &check.replace("{t}", "p_all")).await.map_or_else(|e| json!({"ok": false, "error": e}), |d| json!({"ok": true, "detail": d})),
    );
    put(
        "datafusion_read_net_worth_key_only_without_user",
        df_read(
            &ctx,
            "p_nw",
            &keyed,
            Some(only_net_worth.clone()),
            &format!("SELECT count(*) AS n, sum({}) AS total FROM p_nw", q("net_worth")),
        )
        .await
        .map_or_else(|e| json!({"ok": false, "error": e}), |d| json!({"ok": true, "detail": d})),
    );
    put(
        "datafusion_read_net_worth_key_only_user_column",
        df_read(&ctx, "p_nw2", &keyed, Some(only_net_worth.clone()), &format!("SELECT count(DISTINCT {}) FROM p_nw2", q("user")))
            .await
            .map_or_else(|e| json!({"ok": false, "error": e}), |d| json!({"ok": true, "detail": d})),
    );
    // DataFusion writes per-column keys itself (COPY / write_parquet with
    // `crypto.file_encryption`), and arrow-rs reads them back.
    let df_written = dir.join("enc-df-written.parquet");
    let _ = std::fs::remove_file(&df_written);
    let write_df = async {
        let mut options = TableParquetOptions::default();
        options.crypto.file_encryption = Some(ConfigFileEncryptionProperties::from(&per_column(false)));
        ctx.sql(&format!("SELECT {select} FROM events"))
            .await
            .map_err(|e| e.to_string())?
            .write_parquet(
                df_written.to_str().expect("path"),
                DataFrameWriteOptions::new().with_single_file_output(true),
                Some(options),
            )
            .await
            .map_err(|e| e.to_string())?;
        Ok::<_, String>(())
    }
    .await;
    put(
        "datafusion_write_per_column_then_arrowrs_read_all_keys",
        match write_df {
            Ok(()) => attempt(|| read(&df_written, Some(all.clone()), &COLUMNS)),
            Err(e) => json!({"ok": false, "error": e}),
        },
    );
    put(
        "datafusion_write_per_column_then_arrowrs_read_footer_key_only_user",
        attempt(|| read(&df_written, Some(keys(FOOTER, &[])), &["user"])),
    );
    // DuckDB interop.
    put("duckdb_read_arrowrs_per_column_file", attempt(|| duck(&keyed, None)));
    let duck_written = dir.join("enc-duckdb-footer.parquet");
    let _ = std::fs::remove_file(&duck_written);
    put("duckdb_write_footer_key", attempt(|| duck(&plain, Some(&duck_written))));
    put(
        "arrowrs_read_duckdb_footer_key_file",
        attempt(|| read(&duck_written, Some(keys(FOOTER, &[])), &COLUMNS)),
    );
    put("duckdb_read_own_footer_key_file", attempt(|| duck(&duck_written, None)));
    let read_plain = read(&plain, None, &COLUMNS).ok();
    let read_keyed = read(&keyed, Some(all), &COLUMNS).ok();
    emit(json!({
        "bench": "encryption",
        "crates": {"datafusion": "55.3.0 (feature parquet_encryption)", "parquet": "59.x (feature encryption)", "duckdb": "1.20000.1 (bundled)"},
        "rows": rows,
        "overhead": {"writePlainMs": write_plain * 1e3, "writePerColumnMs": write_keyed * 1e3,
            "readPlain": read_plain, "readPerColumn": read_keyed,
            "bytesPlain": std::fs::metadata(&plain).map(|m| m.len()).ok(), "bytesPerColumn": std::fs::metadata(&keyed).map(|m| m.len()).ok()},
        "cases": cases,
    }));
}
