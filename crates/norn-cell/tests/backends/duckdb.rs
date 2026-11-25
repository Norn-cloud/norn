//! D136 parity suites on `DuckDB`: the engine under test and the hooks a
//! suite uses to inject a fault into, or read back, the engine's own files.

use std::path::Path;

use duckdb::OptionalExt as _;
use norn_cell::analytics::TenantId;
#[allow(unused_imports, reason = "each suite uses some of these")]
pub(crate) use norn_cell::analytics::engine::{
    ATTEMPT_BUSY, ATTEMPT_UNKNOWN, DATASET_COLD, ENGINE_ID, Engine, Settings,
};

#[allow(clippy::expect_used, reason = "test support")]
fn open(root: &Path, tenant: &TenantId) -> duckdb::Connection {
    duckdb::Connection::open(root.join(format!("{}.duckdb", tenant.as_str())))
        .expect("the tenant's database")
}

/// Rows of the mirror table `table` a reader could see.
#[allow(dead_code, clippy::expect_used, reason = "a hook only some suites use")]
pub(crate) fn mirror_rows(root: &Path, tenant: &TenantId, table: &str) -> u64 {
    open(root, tenant)
        .query_row(&format!("SELECT count(*) FROM \"m:{table}\""), [], |r| {
            r.get(0)
        })
        .expect("mirror rows")
}

/// Make the tenant's metadata refuse to record `key`.
#[allow(dead_code, clippy::expect_used, reason = "a hook only some suites use")]
pub(crate) fn refuse_meta_key(root: &Path, tenant: &TenantId, key: &str) {
    open(root, tenant)
        .execute_batch(&format!(
            "CREATE TABLE \":kept\" AS SELECT * FROM \":meta\"; DROP TABLE \":meta\"; \
             CREATE TABLE \":meta\" (key VARCHAR PRIMARY KEY, value VARCHAR NOT NULL, \
             CHECK (key <> '{key}')); \
             INSERT INTO \":meta\" SELECT * FROM \":kept\"; DROP TABLE \":kept\";"
        ))
        .expect("refusing metadata table");
}

/// One metadata record.
#[allow(dead_code, clippy::expect_used, reason = "a hook only some suites use")]
pub(crate) fn meta(root: &Path, tenant: &TenantId, key: &str) -> Option<String> {
    open(root, tenant)
        .query_row("SELECT value FROM \":meta\" WHERE key = ?", [key], |row| {
            row.get(0)
        })
        .optional()
        .expect("meta")
}

/// Rows of the current dataset `table`.
#[allow(dead_code, clippy::expect_used, reason = "a hook only some suites use")]
pub(crate) fn dataset_rows(root: &Path, tenant: &TenantId, table: &str) -> u64 {
    open(root, tenant)
        .query_row(&format!("SELECT count(*) FROM \"d:{table}\""), [], |r| {
            r.get(0)
        })
        .expect("dataset rows")
}

/// Every `lineage:` record key, in order.
#[allow(dead_code, clippy::expect_used, reason = "a hook only some suites use")]
pub(crate) fn lineage_keys(root: &Path, tenant: &TenantId) -> Vec<String> {
    let conn = open(root, tenant);
    let mut statement = conn
        .prepare("SELECT key FROM \":meta\" WHERE key LIKE 'lineage:%' ORDER BY key")
        .expect("lineage");
    statement
        .query_map([], |row| row.get::<_, String>(0))
        .expect("keys")
        .map(|key| key.expect("key"))
        .collect()
}
