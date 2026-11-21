//! The DuckDB side: the engine olapd runs, at norn-cell's pin.

use std::path::Path;
use std::time::Instant;

use duckdb::Connection;
use serde_json::json;

use crate::{
    Dialect, LAWS, QUERIES, checked_sql, data_select, dataset_sql, emit, held_select, law_insert_sql,
    median, outcome_sql, q, quarantine_sql, query_sql, secs, sha256_file, trusted,
};

fn open(path: Option<&Path>, threads: usize) -> Connection {
    let conn = match path {
        Some(p) => Connection::open(p).expect("open"),
        None => Connection::open_in_memory().expect("open"),
    };
    conn.execute_batch(&format!("SET threads={threads}; SET memory_limit='5GB';"))
        .expect("settings");
    conn
}

fn count(conn: &Connection, sql: &str) -> u64 {
    conn.query_row(sql, [], |row| row.get::<_, u64>(0)).expect(sql)
}

fn lit(path: &Path) -> String {
    format!("'{}'", path.to_string_lossy().replace('\'', "''"))
}

/// MANIFEST-BENCH-1's landing, in DuckDB: read, type, check 3 laws, write.
pub fn land(csv: &Path, out: &Path, threads: usize) {
    std::fs::create_dir_all(out).expect("out");
    let db = out.join("duck-land.db");
    let _ = std::fs::remove_file(&db);
    let conn = open(Some(&db), threads);
    let started = Instant::now();
    let (sha, source_bytes) = sha256_file(csv);
    let hashed = secs(started);
    let at = Instant::now();
    conn.execute_batch("BEGIN TRANSACTION").expect("begin");
    conn.execute_batch(&format!(
        "CREATE TEMP TABLE \":file\" AS SELECT * FROM read_csv({}, header = true, all_varchar = true, delim = ',', quote = '\"', escape = '\"', strict_mode = true, null_padding = false)",
        lit(csv)
    ))
    .expect("read csv");
    let read = secs(at);
    let at = Instant::now();
    conn.execute_batch(&format!(
        "CREATE TEMP TABLE checked AS {}",
        checked_sql(Dialect::Duck, "\":file\"")
    ))
    .expect("checked");
    conn.execute_batch(&format!("CREATE TABLE dataset AS {}", dataset_sql(Dialect::Duck)))
        .expect("dataset");
    conn.execute_batch(&format!("CREATE TABLE quarantine AS {}", quarantine_sql()))
        .expect("quarantine");
    conn.execute_batch("DROP TABLE checked; DROP TABLE \":file\"; COMMIT")
        .expect("typed");
    let typed = secs(at);
    let at = Instant::now();
    conn.execute_batch("BEGIN TRANSACTION").expect("begin");
    conn.execute_batch(&format!(
        "CREATE TEMP TABLE outcome AS {}",
        outcome_sql(Dialect::Duck, "dataset")
    ))
    .expect("outcome");
    for i in 0..LAWS.len() {
        conn.execute_batch(&law_insert_sql(i)).expect("law insert");
    }
    let residue = count(&conn, "SELECT count(*) FROM outcome WHERE r0 OR r1 OR r2");
    conn.execute_batch("DROP TABLE outcome; COMMIT").expect("laws");
    let laws = secs(at);
    let at = Instant::now();
    let data = out.join("data.parquet");
    let held = out.join("held.parquet");
    for (sql, path) in [(data_select(), &data), (held_select(), &held)] {
        let _ = std::fs::remove_file(path);
        conn.execute_batch(&format!(
            "COPY ({sql}) TO {} (FORMAT parquet, COMPRESSION zstd, ROW_GROUP_SIZE 122880)",
            lit(path)
        ))
        .expect("copy");
    }
    let written = secs(at);
    let at = Instant::now();
    let admitted = count(
        &conn,
        &format!("SELECT count(*) FROM dataset WHERE {}", trusted()),
    );
    let held_rows = count(&conn, "SELECT count(*) FROM quarantine");
    let mut reasons = serde_json::Map::new();
    let mut stmt = conn
        .prepare(&format!(
            "SELECT {r}, count(*) FROM quarantine GROUP BY {r} ORDER BY {r}",
            r = q(":reason")
        ))
        .expect("reasons");
    for row in stmt
        .query_map([], |row| Ok((row.get::<_, String>(0)?, row.get::<_, u64>(1)?)))
        .expect("reasons")
    {
        let (reason, n) = row.expect("reason");
        reasons.insert(reason, json!(n));
    }
    let range = |dir: &str| -> Option<String> {
        conn.query_row(
            &format!(
                "SELECT {c} FROM dataset WHERE {c} IS NOT NULL AND {} ORDER BY {t} {dir}, {n} {dir} LIMIT 1",
                trusted(),
                c = q("occurred_at"),
                t = q("occurred_at:t"),
                n = q("occurred_at:n")
            ),
            [],
            |row| row.get(0),
        )
        .ok()
    };
    let valid = (range("ASC"), range("DESC"));
    let counted = secs(at);
    let (data_sha, data_bytes) = sha256_file(&data);
    let (held_sha, held_bytes) = sha256_file(&held);
    let total = secs(started);
    let rows = admitted + held_rows;
    emit(json!({
        "bench": "land", "engine": "duckdb", "threads": threads,
        "rows": rows, "admitted": admitted, "held": held_rows, "reasons": reasons, "residue": residue,
        "source": {"sha256": sha, "bytes": source_bytes},
        "data": {"sha256": data_sha, "bytes": data_bytes}, "heldFile": {"sha256": held_sha, "bytes": held_bytes},
        "validTime": [valid.0, valid.1],
        "seconds": {"hash": hashed, "readCsv": read, "type": typed, "laws": laws, "write": written, "counts": counted, "total": total},
        "rowsPerSecond": rows as f64 / total,
    }));
    drop(conn);
    let _ = std::fs::remove_file(&db);
}

/// The derived lake files every later bench reads: a case dimension and
/// the unmarked / marked column split of the data file.
pub fn prep(dir: &Path, threads: usize) {
    let conn = open(None, threads);
    let data = lit(&dir.join("data.parquet"));
    conn.execute_batch(&format!("CREATE VIEW events AS SELECT * FROM read_parquet({data})"))
        .expect("events");
    let copy = |sql: String, name: &str| {
        conn.execute_batch(&format!(
            "COPY ({sql}) TO {} (FORMAT parquet, COMPRESSION zstd, ROW_GROUP_SIZE 122880)",
            lit(&dir.join(name))
        ))
        .expect(name);
    };
    let cols = |names: &[&str]| names.iter().map(|n| q(n)).collect::<Vec<_>>().join(", ");
    copy(
        format!(
            "SELECT {ci}, any_value({sa}) AS {sa}, any_value({ic}) AS {ic}, any_value({co}) AS {co}, any_value({dt}) AS {dt} FROM events GROUP BY {ci} ORDER BY {ci}",
            ci = q("case_id"),
            sa = q("spend_area"),
            ic = q("item_category"),
            co = q("company"),
            dt = q("document_type")
        ),
        "cases.parquet",
    );
    copy(
        format!(
            "SELECT {} FROM events ORDER BY {}",
            cols(&[":row", "event_id", "case_id", "activity", "occurred_at:t", "vendor", "company", "item_type"]),
            q(":row")
        ),
        "unmarked.parquet",
    );
    copy(
        format!(
            "SELECT {} FROM events ORDER BY {}",
            cols(&[":row", "net_worth", "user", "resource"]),
            q(":row")
        ),
        "marked.parquet",
    );
    let vendor: String = conn
        .query_row(
            &format!(
                "SELECT {v} FROM events WHERE {v} IS NOT NULL GROUP BY {v} ORDER BY count(*) DESC, {v} LIMIT 1 OFFSET 20",
                v = q("vendor")
            ),
            [],
            |row| row.get(0),
        )
        .expect("vendor");
    std::fs::write(dir.join("vendor.txt"), &vendor).expect("vendor.txt");
    let n = |name: &str| count(&conn, &format!("SELECT count(*) FROM read_parquet({})", lit(&dir.join(name))));
    emit(json!({"bench": "prep", "cases": n("cases.parquet"), "unmarked": n("unmarked.parquet"), "marked": n("marked.parquet"), "vendor": vendor}));
}

fn views(conn: &Connection, dir: &Path) {
    for (view, file) in [
        ("events", "data.parquet"),
        ("dataset", "data.parquet"),
        ("cases", "cases.parquet"),
        ("unmarked", "unmarked.parquet"),
        ("marked", "marked.parquet"),
    ] {
        conn.execute_batch(&format!(
            "CREATE VIEW {view} AS SELECT * FROM read_parquet({})",
            lit(&dir.join(file))
        ))
        .expect("view");
    }
}

fn run_query(conn: &Connection, sql: &str) -> (f64, f64, usize) {
    let at = Instant::now();
    let mut stmt = conn.prepare(sql).expect(sql);
    let planned = secs(at);
    let rows: usize = stmt.query_arrow([]).expect(sql).map(|b| b.num_rows()).sum();
    (planned, secs(at), rows)
}

/// Dashboard and marked-join latency: a cold run, then the median of 5.
pub fn queries(dir: &Path, threads: usize) {
    let conn = open(None, threads);
    views(&conn, dir);
    let vendor = std::fs::read_to_string(dir.join("vendor.txt")).expect("vendor");
    let mut out = serde_json::Map::new();
    for name in QUERIES {
        let sql = query_sql(Dialect::Duck, name, &vendor);
        let (_, cold, rows) = run_query(&conn, &sql);
        let mut warm = Vec::new();
        let mut plan = Vec::new();
        for _ in 0..5 {
            let (p, t, _) = run_query(&conn, &sql);
            plan.push(p);
            warm.push(t);
        }
        out.insert(
            name.into(),
            json!({"rows": rows, "coldMs": cold * 1e3, "warmMs": median(&mut warm) * 1e3, "prepareMs": median(&mut plan) * 1e3}),
        );
    }
    emit(json!({"bench": "queries", "engine": "duckdb", "threads": threads, "queries": out}));
}

fn prepare_ms(conn: &Connection, sql: &str, reps: usize) -> f64 {
    let mut times = Vec::new();
    for _ in 0..reps {
        let at = Instant::now();
        let stmt = conn.prepare(sql).expect(sql);
        times.push(secs(at));
        drop(stmt);
    }
    median(&mut times) * 1e3
}

/// Planning cost (parse + bind + optimize + physical plan = `prepare`).
pub fn plans(dir: &Path, threads: usize) {
    let conn = open(None, threads);
    views(&conn, dir);
    let vendor = std::fs::read_to_string(dir.join("vendor.txt")).expect("vendor");
    let mut shapes = serde_json::Map::new();
    for name in QUERIES.iter().chain(["law_check"].iter()) {
        shapes.insert(
            (*name).into(),
            json!(prepare_ms(&conn, &query_sql(Dialect::Duck, name, &vendor), 5)),
        );
    }
    let mut chains = serde_json::Map::new();
    for shape in crate::plans::CHAINS {
        let mut points = Vec::new();
        for depth in crate::plans::DEPTHS {
            let ms = if shape.starts_with("view") {
                let prefix = format!("{shape}_{depth}");
                let at = Instant::now();
                for ddl in crate::plans::view_ddl(shape, depth, &prefix) {
                    conn.execute_batch(&ddl).expect("view ddl");
                }
                let ddl_ms = secs(at) * 1e3;
                let p = prepare_ms(&conn, &crate::plans::view_query(&prefix, depth), 1);
                json!({"depth": depth, "ddlMs": ddl_ms, "planMs": p})
            } else {
                let p = prepare_ms(&conn, &crate::plans::cte_sql(shape, depth), 1);
                json!({"depth": depth, "planMs": p})
            };
            let last = ms["planMs"].as_f64().unwrap_or(0.0) + ms["ddlMs"].as_f64().unwrap_or(0.0);
            points.push(ms);
            if last > crate::plans::STOP_MS {
                break;
            }
        }
        chains.insert(shape.into(), json!(points));
    }
    emit(json!({"bench": "plans", "engine": "duckdb", "shapes": shapes, "chains": chains}));
}
