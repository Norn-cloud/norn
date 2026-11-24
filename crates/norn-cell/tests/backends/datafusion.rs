//! D137 parity suites on `DataFusion`: the engine under test and the hooks a
//! suite uses to inject a fault into, or read back, the engine's catalog and
//! objects. Every answer a suite checks must name `datafusion`, so `DuckDB`
//! can never satisfy these suites.
#![allow(
    clippy::expect_used,
    unreachable_pub,
    dead_code,
    unused_imports,
    reason = "test support shared by suites that use different hooks"
)]

use std::path::Path;

use norn_cell::analytics::TenantId;
pub use norn_cell::analytics::datafusion::{
    ATTEMPT_BUSY, ATTEMPT_UNKNOWN, DATASET_COLD, ENGINE_ID, Engine, Settings,
};
use rusqlite::OptionalExtension as _;

fn open(root: &Path, tenant: &TenantId) -> rusqlite::Connection {
    rusqlite::Connection::open(root.join("t").join(tenant.as_str()).join("catalog.sqlite3"))
        .expect("the tenant's catalog")
}

fn rows_of(path: &str) -> u64 {
    let file = std::fs::File::open(path).expect("an object");
    let reader = parquet::file::reader::SerializedFileReader::new(file).expect("parquet");
    let rows = parquet::file::reader::FileReader::metadata(&reader)
        .file_metadata()
        .num_rows();
    u64::try_from(rows).expect("rows")
}

/// Rows of the mirror table `table` a reader could see: every version in
/// the segments the catalog publishes.
pub fn mirror_rows(root: &Path, tenant: &TenantId, table: &str) -> u64 {
    let conn = open(root, tenant);
    let mut statement = conn
        .prepare("SELECT path FROM segment WHERE tbl = ?1")
        .expect("segments");
    statement
        .query_map([table], |row| row.get::<_, String>(0))
        .expect("segments")
        .map(|path| rows_of(&path.expect("path")))
        .sum()
}

/// Make the tenant's catalog refuse to record `key`.
pub fn refuse_meta_key(root: &Path, tenant: &TenantId, key: &str) {
    open(root, tenant)
        .execute_batch(&format!(
            "CREATE TRIGGER refuse_key BEFORE INSERT ON meta WHEN NEW.key = '{key}' \
             BEGIN SELECT RAISE(ABORT, 'refused'); END;"
        ))
        .expect("refusing catalog");
}

/// One catalog record.
pub fn meta(root: &Path, tenant: &TenantId, key: &str) -> Option<String> {
    open(root, tenant)
        .query_row("SELECT value FROM meta WHERE key = ?1", [key], |row| {
            row.get(0)
        })
        .optional()
        .expect("meta")
}

/// Rows of the current dataset `table`: its published data file's rows.
pub fn dataset_rows(root: &Path, tenant: &TenantId, table: &str) -> u64 {
    let files: serde_json::Value = serde_json::from_str(
        &meta(root, tenant, &format!("files:{table}")).expect("published files"),
    )
    .expect("files");
    rows_of(
        files
            .get("data")
            .and_then(serde_json::Value::as_str)
            .expect("data path"),
    )
}

/// Every `lineage:` record key, in order.
pub fn lineage_keys(root: &Path, tenant: &TenantId) -> Vec<String> {
    if !root
        .join("t")
        .join(tenant.as_str())
        .join("catalog.sqlite3")
        .exists()
    {
        return Vec::new();
    }
    let conn = open(root, tenant);
    let mut statement = conn
        .prepare("SELECT key FROM meta WHERE key LIKE 'lineage:%' ORDER BY key")
        .expect("lineage");
    statement
        .query_map([], |row| row.get::<_, String>(0))
        .expect("keys")
        .map(|key| key.expect("key"))
        .collect()
}
