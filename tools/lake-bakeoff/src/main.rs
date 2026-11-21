//! Lake engine bake-off (#1533): DuckDB against Apache DataFusion on Norn's
//! lake workloads. Each subcommand is one measurement in its own process, so
//! its peak RSS is that engine's alone, and prints one `RESULT {json}` line.
//!
//! The landing SQL mirrors `norn_cell::analytics::engine` (`check_file`,
//! `parse_sql`, `check_sql`, `check_rules`, `landing`) for the BPI 2019
//! schema of `crates/norn-cell/tests/bench_manifest.rs`, in each dialect.

use std::path::{Path, PathBuf};
use std::time::Instant;

use serde_json::{Value as Json, json};

mod df;
mod duck;
mod encrypt;
mod plans;
mod substrait;

/// Storage kinds of the BPI 2019 columns (bench_manifest.rs `fields()`).
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Text,
    Timestamp,
    Decimal2,
}

/// The flattened log's columns: name, kind, required.
pub const COLUMNS: [(&str, Kind, bool); 13] = [
    ("event_id", Kind::Text, true),
    ("case_id", Kind::Text, true),
    ("item_type", Kind::Text, false),
    ("document_type", Kind::Text, false),
    ("company", Kind::Text, false),
    ("vendor", Kind::Text, false),
    ("spend_area", Kind::Text, false),
    ("item_category", Kind::Text, false),
    ("activity", Kind::Text, true),
    ("occurred_at", Kind::Timestamp, true),
    ("resource", Kind::Text, false),
    ("user", Kind::Text, false),
    ("net_worth", Kind::Decimal2, false),
];

/// The three laws bench_manifest admits, as the engine lowers them: label,
/// whether a failing row is held back (error severity), the column it reads.
pub const LAWS: [(&str, bool); 3] = [
    ("bpi_net_worth_nonnegative", true),
    ("bpi_activity_named", true),
    ("bpi_case_named", false),
];

/// `2^96 - 1`, the per-row evaluator's exact decimal mantissa.
pub const EXACT_MANTISSA: i128 = 79_228_162_514_264_337_593_543_950_335;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Dialect {
    Duck,
    Df,
}

pub fn q(name: &str) -> String {
    format!("\"{}\"", name.replace('"', "\"\""))
}

/// `EXACT_MANTISSA / 10^scale` as SQL text.
fn bound(scale: u32) -> String {
    let digits = EXACT_MANTISSA.to_string();
    let split = digits.len() - scale as usize;
    if scale == 0 {
        digits
    } else {
        format!("{}.{}", &digits[..split], &digits[split..])
    }
}

/// `coalesce(TRY(abs(x) <= bound), FALSE)` (`engine::in_range`).
fn in_range(d: Dialect, x: &str, scale: u32) -> String {
    let inner = format!("abs({x}) <= CAST('{}' AS DECIMAL(38,{scale}))", bound(scale));
    format!("coalesce({}, FALSE)", try_(d, &inner))
}

/// DuckDB's `TRY(x)`; DataFusion has no TRY, and none of these expressions
/// can raise, so it is the expression itself.
fn try_(d: Dialect, x: &str) -> String {
    match d {
        Dialect::Duck => format!("TRY({x})"),
        Dialect::Df => format!("({x})"),
    }
}

fn nfc(x: &str) -> String {
    format!("(nfc_normalize({x}) = {x})")
}

/// One lowered law's (residue, fails) SQL over the dataset table
/// (`engine::check_sql`): residue when a read is absent, a guard fails or
/// the predicate cannot be computed.
pub fn law_sql(d: Dialect, i: usize) -> (String, String) {
    let empty = "CAST('' AS VARCHAR)";
    let (reads, pred, guards) = match i {
        0 => {
            let zero = "CAST('0' AS DECIMAL(38,0))";
            (
                q("net_worth"),
                format!("({} >= {zero})", q("net_worth")),
                vec![in_range(d, &q("net_worth"), 2), in_range(d, zero, 0)],
            )
        }
        1 => (
            q("activity"),
            format!("(NOT ({} = {empty}))", q("activity")),
            vec![nfc(&q("activity")), nfc(empty)],
        ),
        _ => (
            q("case_id"),
            format!("({} <> {empty})", q("case_id")),
            vec![nfc(&q("case_id")), nfc(empty)],
        ),
    };
    let mut residue = vec![format!("{reads} IS NULL")];
    residue.extend(guards.iter().map(|g| format!("NOT {g}")));
    residue.push(format!("{} IS NULL", try_(d, &pred)));
    (
        format!("({})", residue.join(" OR ")),
        format!("({} <> TRUE)", try_(d, &pred)),
    )
}

/// The law outcome query (`check_rules`): per row, per law, residue and fails.
pub fn outcome_sql(d: Dialect, table: &str) -> String {
    let mut select = vec![q(":row")];
    for i in 0..LAWS.len() {
        let (r, f) = law_sql(d, i);
        select.push(format!("{r} AS r{i}"));
        select.push(format!("{f} AS f{i}"));
    }
    format!("SELECT {} FROM {table}", select.join(", "))
}

/// The text file numbered in file order, one text column `c{i}` per field
/// (an empty or blank field is null), as `check_file` reads it.
pub fn numbered_sql(d: Dialect, file: &str) -> String {
    let (u64t, order) = match d {
        Dialect::Duck => ("UBIGINT", "ORDER BY rowid"),
        Dialect::Df => ("BIGINT UNSIGNED", ""),
    };
    let texts: Vec<String> = COLUMNS
        .iter()
        .enumerate()
        .map(|(i, (name, _, _))| {
            let text = format!("CAST({} AS VARCHAR)", q(name));
            format!("CASE WHEN trim({text}) = '' THEN NULL ELSE {text} END AS c{i}")
        })
        .collect();
    format!(
        "SELECT CAST(row_number() OVER ({order}) AS {u64t}) AS {}, {} FROM {file}",
        q(":row"),
        texts.join(", ")
    )
}

/// The typed projection of the text file (`check_file` + `parse_sql`).
pub fn checked_sql(d: Dialect, file: &str) -> String {
    typed_sql(d, &format!("({}) AS t", numbered_sql(d, file)))
}

/// Each text column parsed (`v{i}`, plus `t{i}`/`n{i}` for an instant) and
/// `":reason"`, the first refusal in column order, over numbered rows.
pub fn typed_sql(d: Dialect, from: &str) -> String {
    let strlen = match d {
        Dialect::Duck => "strlen",
        Dialect::Df => "octet_length",
    };
    let mut parsed = Vec::new();
    let mut reasons = Vec::new();
    for (i, (_, kind, required)) in COLUMNS.iter().enumerate() {
        let c = format!("c{i}");
        let v = format!("v{i}");
        let reason = match kind {
            Kind::Text => {
                parsed.push(format!("{c} AS {v}"));
                "CAST(NULL AS VARCHAR)".to_owned()
            }
            Kind::Decimal2 => {
                let scale = 2;
                parsed.push(format!(
                    "translate({c}, '0123456789', '') IN ('', '.', '-', '+', '-.', '+.') AND NOT ltrim({c}, '+-') LIKE '.%' AND NOT {c} LIKE '%.' AS s{i}"
                ));
                parsed.push(format!(
                    "strpos({c}, '.') = 0 OR {strlen}({c}) - strpos({c}, '.') <= {scale} AS f{i}"
                ));
                parsed.push(format!(
                    "CASE WHEN translate({c}, '0123456789', '') IN ('', '.', '-', '+', '-.', '+.') AND (strpos({c}, '.') = 0 OR {strlen}({c}) - strpos({c}, '.') <= {scale}) THEN TRY_CAST({c} AS DECIMAL(38,{scale})) END AS {v}"
                ));
                format!(
                    "CASE WHEN NOT s{i} THEN 'bulk.not_decimal' WHEN NOT f{i} THEN 'bulk.scale_exceeded' WHEN {v} IS NULL THEN 'bulk.not_decimal' END"
                )
            }
            Kind::Timestamp => {
                let zone = format!(
                    "CASE WHEN right({c}, 1) = 'Z' THEN 'Z' WHEN substr(right({c}, 6), 1, 1) IN ('+', '-') AND translate(right({c}, 6), '0123456789', '') IN ('+:', '-:') \
                     AND substr(right({c}, 6), 4, 1) = ':' AND substr(right({c}, 6), 2, 2) <= '23' AND substr(right({c}, 6), 5, 2) <= '59' \
                     AND right({c}, 6) <> '-00:00' THEN right({c}, 6) END"
                );
                let fraction = format!("substr({c}, 20, {strlen}({c}) - 19 - {strlen}({zone}))");
                let shape = format!(
                    "{strlen}({c}) BETWEEN 20 AND 35 AND translate(substr({c}, 1, 19), '0123456789', '') = '--T::' \
                     AND substr({c}, 5, 1) = '-' AND substr({c}, 8, 1) = '-' AND substr({c}, 11, 1) = 'T' AND substr({c}, 14, 1) = ':' \
                     AND substr({c}, 17, 1) = ':' AND substr({c}, 12, 2) <= '23' AND substr({c}, 15, 2) <= '59' AND substr({c}, 18, 2) <= '59' AND {zone} IS NOT NULL \
                     AND ({fraction} = '' OR (substr({fraction}, 1, 1) = '.' AND {strlen}({fraction}) BETWEEN 2 AND 10 AND translate(substr({fraction}, 2), '0123456789', '') = ''))"
                );
                let offset = format!(
                    "CASE WHEN {zone} = 'Z' THEN 0 ELSE (CASE WHEN substr({zone}, 1, 1) = '-' THEN -1 ELSE 1 END) * \
                     (CAST(substr({zone}, 2, 2) AS BIGINT) * 3600 + CAST(substr({zone}, 5, 2) AS BIGINT) * 60) END"
                );
                let seconds = match d {
                    Dialect::Duck => format!(
                        "(epoch_ms(TRY_CAST(substr({c}, 1, 19) AS TIMESTAMP)) // 1000)"
                    ),
                    Dialect::Df => {
                        format!("to_unixtime(TRY_CAST(substr({c}, 1, 19) AS TIMESTAMP))")
                    }
                };
                let utc = format!("({seconds} - {offset})");
                parsed.push(format!("{c} AS {v}"));
                parsed.push(format!(
                    "CASE WHEN {shape} THEN CASE WHEN {utc} BETWEEN -62167219200 AND 253402300799 THEN {utc} END END AS t{i}"
                ));
                parsed.push(format!(
                    "CASE WHEN {shape} THEN CAST(rpad(substr({fraction}, 2), 9, '0') AS INTEGER) END AS n{i}"
                ));
                format!("CASE WHEN t{i} IS NULL THEN 'bulk.not_timestamp' END")
            }
        };
        let missing = if *required {
            "'bulk.required'"
        } else {
            "CAST(NULL AS VARCHAR)"
        };
        reasons.push(format!(
            "CASE WHEN c{i} IS NULL THEN {missing} ELSE coalesce({reason}, CAST(NULL AS VARCHAR)) END"
        ));
    }
    format!(
        "SELECT *, coalesce({}) AS {} FROM (SELECT *, {} FROM {from}) AS p",
        reasons.join(", "),
        q(":reason"),
        parsed.join(", "),
    )
}

/// The dataset table's select over `checked` (`load_file`).
pub fn dataset_sql(d: Dialect) -> String {
    let u64t = match d {
        Dialect::Duck => "UBIGINT",
        Dialect::Df => "BIGINT UNSIGNED",
    };
    let mut cols = vec![
        format!("CAST({} AS VARCHAR) AS {}", q(":row"), q(":id")),
        format!("CAST(1 AS {u64t}) AS {}", q(":v")),
        q(":row"),
        format!("'bpi19' AS {}", q(":load")),
    ];
    for (i, (name, kind, _)) in COLUMNS.iter().enumerate() {
        cols.push(format!("v{i} AS {}", q(name)));
        if *kind == Kind::Timestamp {
            cols.push(format!("CAST(t{i} AS BIGINT) AS {}", q(&format!("{name}:t"))));
            cols.push(format!("CAST(n{i} AS INTEGER) AS {}", q(&format!("{name}:n"))));
        }
    }
    format!(
        "SELECT {} FROM checked WHERE {} IS NULL",
        cols.join(", "),
        q(":reason")
    )
}

pub fn quarantine_sql() -> String {
    let mut cols = vec![
        q(":row"),
        format!("'bpi19' AS {}", q(":load")),
        q(":reason"),
        format!("CAST(NULL AS VARCHAR) AS {}", q(":rule")),
        format!("TRUE AS {}", q(":holds")),
    ];
    for (i, (name, _, _)) in COLUMNS.iter().enumerate() {
        cols.push(format!("c{i} AS {}", q(name)));
    }
    format!(
        "SELECT {} FROM checked WHERE {} IS NOT NULL",
        cols.join(", "),
        q(":reason")
    )
}

pub fn law_insert_sql(i: usize) -> String {
    let (label, holds) = LAWS[i];
    format!(
        "INSERT INTO quarantine ({}, {}, {}, {}, {}) SELECT {}, 'bpi19', 'law:{label}', '{label}', {holds} FROM outcome WHERE NOT r{i} AND f{i}",
        q(":row"),
        q(":load"),
        q(":reason"),
        q(":rule"),
        q(":holds"),
        q(":row"),
    )
}

pub fn data_columns() -> Vec<String> {
    let mut cols = vec![q(":id"), q(":v"), q(":row")];
    for (name, kind, _) in COLUMNS {
        cols.push(q(name));
        if kind == Kind::Timestamp {
            cols.push(q(&format!("{name}:t")));
            cols.push(q(&format!("{name}:n")));
        }
    }
    cols
}

pub fn held_columns() -> Vec<String> {
    let mut cols = vec![q(":row"), q(":reason"), q(":rule"), q(":holds")];
    cols.extend(COLUMNS.iter().map(|(n, _, _)| q(n)));
    cols
}

pub fn trusted() -> String {
    format!(
        "{} NOT IN (SELECT {} FROM quarantine WHERE {} AND {} IS NOT NULL)",
        q(":row"),
        q(":row"),
        q(":holds"),
        q(":rule")
    )
}

pub fn data_select() -> String {
    format!(
        "SELECT {} FROM dataset WHERE {} ORDER BY {}",
        data_columns().join(", "),
        trusted(),
        q(":row")
    )
}

pub fn held_select() -> String {
    format!(
        "SELECT {} FROM quarantine ORDER BY {}, {} NULLS FIRST, {}",
        held_columns().join(", "),
        q(":row"),
        q(":rule"),
        q(":reason")
    )
}

/// The dashboard and lake queries, by name; `{events}` etc. are table names.
pub fn query_sql(d: Dialect, name: &str, vendor: &str) -> String {
    let month = match d {
        Dialect::Duck => format!("date_trunc('month', make_timestamp({} * 1000000))", q("occurred_at:t")),
        Dialect::Df => format!("date_trunc('month', to_timestamp_seconds({}))", q("occurred_at:t")),
    };
    match name {
        "q1_groupby" => format!(
            "SELECT {a}, count(*) AS n, sum({nw}) AS total, count(DISTINCT {c}) AS cases FROM events GROUP BY {a} ORDER BY n DESC, {a}",
            a = q("activity"),
            nw = q("net_worth"),
            c = q("case_id")
        ),
        "q2_topn" => format!(
            "SELECT {v}, sum({nw}) AS total FROM events GROUP BY {v} ORDER BY total DESC NULLS LAST, {v} LIMIT 10",
            v = q("vendor"),
            nw = q("net_worth")
        ),
        "q3_timebucket" => format!(
            "SELECT {month} AS month, count(*) AS n, sum({nw}) AS total FROM events GROUP BY 1 ORDER BY 1",
            nw = q("net_worth")
        ),
        "q4_join" => format!(
            "SELECT c.{sa}, count(*) AS n, sum(e.{nw}) AS total FROM events e JOIN cases c ON e.{ci} = c.{ci} GROUP BY c.{sa} ORDER BY total DESC NULLS LAST, c.{sa} LIMIT 20",
            sa = q("spend_area"),
            nw = q("net_worth"),
            ci = q("case_id")
        ),
        "mj1_marked_agg" => format!(
            "SELECT u.{a}, count(*) AS n, sum(m.{nw}) AS total, count(DISTINCT m.{us}) AS users FROM unmarked u JOIN marked m ON u.{r} = m.{r} GROUP BY u.{a} ORDER BY n DESC, u.{a}",
            a = q("activity"),
            nw = q("net_worth"),
            us = q("user"),
            r = q(":row")
        ),
        "mj2_marked_point" => format!(
            "SELECT u.*, m.{nw}, m.{us}, m.{rs} FROM unmarked u JOIN marked m ON u.{r} = m.{r} WHERE u.{v} = '{vendor}' ORDER BY u.{r}",
            nw = q("net_worth"),
            us = q("user"),
            rs = q("resource"),
            r = q(":row"),
            v = q("vendor"),
            vendor = vendor.replace('\'', "''")
        ),
        "law_check" => outcome_sql(d, "dataset"),
        other => panic!("unknown query {other}"),
    }
}

pub const QUERIES: [&str; 6] = [
    "q1_groupby",
    "q2_topn",
    "q3_timebucket",
    "q4_join",
    "mj1_marked_agg",
    "mj2_marked_point",
];

pub fn maxrss_mib() -> f64 {
    // SAFETY: getrusage writes into the zeroed struct it is given.
    unsafe {
        let mut usage: libc::rusage = std::mem::zeroed();
        libc::getrusage(libc::RUSAGE_SELF, &mut usage);
        usage.ru_maxrss as f64 / 1024.0
    }
}

pub fn emit(mut value: Json) {
    value["maxRssMiB"] = json!(maxrss_mib());
    println!("RESULT {value}");
}

pub fn sha256_file(path: &Path) -> (String, u64) {
    use sha2::Digest as _;
    use std::io::Read as _;
    let mut file = std::fs::File::open(path).expect("open");
    let mut hasher = sha2::Sha256::new();
    let mut buf = vec![0u8; 1 << 20];
    let mut bytes = 0u64;
    loop {
        let n = file.read(&mut buf).expect("read");
        if n == 0 {
            break;
        }
        bytes += n as u64;
        hasher.update(&buf[..n]);
    }
    let digest = hasher.finalize();
    (digest.iter().map(|b| format!("{b:02x}")).collect(), bytes)
}

pub fn median(values: &mut [f64]) -> f64 {
    values.sort_by(f64::total_cmp);
    values[values.len() / 2]
}

pub fn secs(at: Instant) -> f64 {
    at.elapsed().as_secs_f64()
}

/// The NFC check the per-row evaluator relies on, as a DataFusion UDF
/// (DuckDB has `nfc_normalize` built in).
pub fn nfc_text(text: &str) -> String {
    use unicode_normalization::UnicodeNormalization as _;
    if unicode_normalization::is_nfc(text) {
        text.to_owned()
    } else {
        text.nfc().collect()
    }
}

fn arg(args: &[String], i: usize) -> PathBuf {
    PathBuf::from(args.get(i).unwrap_or_else(|| panic!("argument {i}")))
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let cmd = args.get(1).map(String::as_str).unwrap_or("");
    let threads: usize = std::env::var("BAKEOFF_THREADS")
        .ok()
        .and_then(|t| t.parse().ok())
        .unwrap_or(4);
    match cmd {
        "land-duckdb" => duck::land(&arg(&args, 2), &arg(&args, 3), threads),
        "land-df" => df::run(threads, df::land(arg(&args, 2), arg(&args, 3), threads, false)),
        "land-df-builder" => df::run(threads, df::land(arg(&args, 2), arg(&args, 3), threads, true)),
        "prep" => duck::prep(&arg(&args, 2), threads),
        "queries-duckdb" => duck::queries(&arg(&args, 2), threads),
        "queries-df" => df::run(threads, df::queries(arg(&args, 2), threads)),
        "plan-duckdb" => duck::plans(&arg(&args, 2), threads),
        "plan-df" => df::run(threads, plans::df_plans(arg(&args, 2), threads)),
        "encrypt" => df::run(threads, encrypt::probe(arg(&args, 2))),
        "substrait" => df::run(threads, substrait::probe(arg(&args, 2), threads)),
        _ => {
            eprintln!(
                "usage: bakeoff land-duckdb|land-df|land-df-builder CSV OUT | prep DIR | queries-duckdb|queries-df DIR | plan-duckdb|plan-df DIR | encrypt DIR | substrait DIR"
            );
            std::process::exit(2);
        }
    }
}
