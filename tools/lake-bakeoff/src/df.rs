//! The DataFusion side: SQL text, and typed plans built with the DataFrame
//! API (`LogicalPlanBuilder` underneath), which never pass through SqlToRel.

use std::future::Future;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use datafusion::arrow::array::{Array, AsArray, StringBuilder};
use datafusion::arrow::compute::cast;
use datafusion::arrow::datatypes::{DataType, Field, Schema};
use datafusion::arrow::record_batch::RecordBatch;
use datafusion::common::{Column, Result, ScalarValue};
use datafusion::execution::runtime_env::RuntimeEnvBuilder;
use datafusion::functions_aggregate::expr_fn::{count_all, count_distinct, sum};
use datafusion::logical_expr::{
    ColumnarValue, ScalarFunctionArgs, ScalarUDF, ScalarUDFImpl, Signature, Volatility,
};
use datafusion::prelude::*;
use serde_json::json;

use crate::{
    COLUMNS, Dialect, EXACT_MANTISSA, LAWS, QUERIES, data_select, dataset_sql, emit, held_select,
    law_insert_sql, median, numbered_sql, outcome_sql, q, quarantine_sql, query_sql, secs,
    sha256_file, trusted, typed_sql,
};

pub fn run<F: Future<Output = ()>>(threads: usize, future: F) {
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(threads)
        .enable_all()
        .build()
        .expect("runtime")
        .block_on(future);
}

/// `nfc_normalize` (DuckDB's built-in), for the laws' NFC guards.
#[derive(Debug, PartialEq, Eq, Hash)]
struct Nfc {
    signature: Signature,
}

impl ScalarUDFImpl for Nfc {
    fn name(&self) -> &str {
        "nfc_normalize"
    }
    fn signature(&self) -> &Signature {
        &self.signature
    }
    fn return_type(&self, _: &[DataType]) -> Result<DataType> {
        Ok(DataType::Utf8)
    }
    fn invoke_with_args(&self, args: ScalarFunctionArgs) -> Result<ColumnarValue> {
        let arrays = ColumnarValue::values_to_arrays(&args.args)?;
        let text = cast(&arrays[0], &DataType::Utf8)?;
        let text = text.as_string::<i32>();
        let mut out = StringBuilder::with_capacity(text.len(), text.value_data().len());
        for value in text.iter() {
            match value {
                Some(t) if unicode_normalization::is_nfc(t) => out.append_value(t),
                Some(t) => out.append_value(crate::nfc_text(t)),
                None => out.append_null(),
            }
        }
        Ok(ColumnarValue::Array(Arc::new(out.finish())))
    }
}

pub fn nfc_udf() -> ScalarUDF {
    ScalarUDF::new_from_impl(Nfc {
        signature: Signature::string(1, Volatility::Immutable),
    })
}

/// One session: `threads` partitions and a 5 GiB pool, as DuckDB's
/// `threads` and `memory_limit`. `ordered_scan` reads a file as one
/// partition, so its rows arrive in file order.
pub fn session(threads: usize, ordered_scan: bool) -> SessionContext {
    let mut config = SessionConfig::new().with_target_partitions(threads);
    if ordered_scan {
        config = config.set_bool("datafusion.optimizer.repartition_file_scans", false);
    }
    let runtime = RuntimeEnvBuilder::new()
        .with_memory_limit(5 << 30, 1.0)
        .build_arc()
        .expect("runtime env");
    let ctx = SessionContext::new_with_config_rt(config, runtime);
    ctx.register_udf(nfc_udf());
    ctx
}

pub async fn exec(ctx: &SessionContext, sql: &str) -> Vec<RecordBatch> {
    ctx.sql(sql)
        .await
        .unwrap_or_else(|e| panic!("{e}\n{sql}"))
        .collect()
        .await
        .unwrap_or_else(|e| panic!("{e}\n{sql}"))
}

async fn count(ctx: &SessionContext, sql: &str) -> u64 {
    let batches = exec(ctx, sql).await;
    let column = cast(batches[0].column(0), &DataType::UInt64).expect("count");
    column
        .as_primitive::<datafusion::arrow::datatypes::UInt64Type>()
        .value(0)
}

fn text_at(batch: &RecordBatch, column: usize, row: usize) -> Option<String> {
    let array = cast(batch.column(column), &DataType::Utf8).ok()?;
    let array = array.as_string::<i32>();
    (!array.is_null(row)).then(|| array.value(row).to_owned())
}

pub fn qcol(relation: &str, name: &str) -> Expr {
    Expr::Column(Column::new(Some(relation), name))
}

fn decimal(mantissa: i128, scale: i8) -> Expr {
    lit(ScalarValue::Decimal128(Some(mantissa), 38, scale))
}

/// The three lowered laws as typed expressions (`check_sql`'s residue and
/// fails per law), the same semantics as [`crate::law_sql`].
pub fn law_exprs() -> Vec<Expr> {
    let nfc = nfc_udf();
    let in_range = |x: Expr, s: i8| coalesce(vec![abs(x).lt_eq(decimal(EXACT_MANTISSA, s)), lit(false)]);
    let nfc_guard = |x: Expr| nfc.call(vec![x.clone()]).eq(x);
    let empty = || cast_expr(lit(""), DataType::Utf8);
    let zero = decimal(0, 0);
    let mut out = vec![ident(":row")];
    // (self.net_worth ?? 0) >= 0
    let nw = ident("net_worth");
    let p0 = nw.clone().gt_eq(zero.clone());
    let r0 = nw
        .clone()
        .is_null()
        .or(in_range(nw, 2).not())
        .or(in_range(zero, 0).not())
        .or(p0.clone().is_null());
    // not (self.activity == '')
    let activity = ident("activity");
    let p1 = activity.clone().eq(empty()).not();
    let r1 = activity
        .clone()
        .is_null()
        .or(nfc_guard(activity).not())
        .or(nfc_guard(empty()).not())
        .or(p1.clone().is_null());
    // self.case_id != ''
    let case_id = ident("case_id");
    let p2 = case_id.clone().not_eq(empty());
    let r2 = case_id
        .clone()
        .is_null()
        .or(nfc_guard(case_id).not())
        .or(nfc_guard(empty()).not())
        .or(p2.clone().is_null());
    for (i, (r, p)) in [(r0, p0), (r1, p1), (r2, p2)].into_iter().enumerate() {
        out.push(r.alias(format!("r{i}")));
        out.push(p.not_eq(lit(true)).alias(format!("f{i}")));
    }
    out
}

fn cast_expr(e: Expr, t: DataType) -> Expr {
    datafusion::prelude::cast(e, t)
}

/// The dashboard, lake and law queries as typed plans (DataFrame API).
pub async fn builder_query(ctx: &SessionContext, name: &str, vendor: &str) -> Result<DataFrame> {
    let nw = || ident("net_worth");
    Ok(match name {
        "q1_groupby" => ctx
            .table("events")
            .await?
            .aggregate(
                vec![ident("activity")],
                vec![
                    count_all().alias("n"),
                    sum(nw()).alias("total"),
                    count_distinct(ident("case_id")).alias("cases"),
                ],
            )?
            .sort(vec![ident("n").sort(false, true), ident("activity").sort(true, false)])?,
        "q2_topn" => ctx
            .table("events")
            .await?
            .aggregate(vec![ident("vendor")], vec![sum(nw()).alias("total")])?
            .sort(vec![ident("total").sort(false, false), ident("vendor").sort(true, false)])?
            .limit(0, Some(10))?,
        "q3_timebucket" => ctx
            .table("events")
            .await?
            .aggregate(
                vec![date_trunc(lit("month"), to_timestamp_seconds(vec![ident("occurred_at:t")])).alias("month")],
                vec![count_all().alias("n"), sum(nw()).alias("total")],
            )?
            .sort(vec![ident("month").sort(true, false)])?,
        "q4_join" => {
            let e = ctx.table("events").await?.select(vec![ident("case_id"), nw()])?.alias("e")?;
            let c = ctx.table("cases").await?.alias("c")?;
            e.join_on(c, JoinType::Inner, vec![qcol("e", "case_id").eq(qcol("c", "case_id"))])?
                .aggregate(
                    vec![qcol("c", "spend_area")],
                    vec![count_all().alias("n"), sum(qcol("e", "net_worth")).alias("total")],
                )?
                .sort(vec![ident("total").sort(false, false), qcol("c", "spend_area").sort(true, false)])?
                .limit(0, Some(20))?
        }
        "mj1_marked_agg" => {
            let u = ctx.table("unmarked").await?.alias("u")?;
            let m = ctx.table("marked").await?.alias("m")?;
            u.join_on(m, JoinType::Inner, vec![qcol("u", ":row").eq(qcol("m", ":row"))])?
                .aggregate(
                    vec![qcol("u", "activity")],
                    vec![
                        count_all().alias("n"),
                        sum(qcol("m", "net_worth")).alias("total"),
                        count_distinct(qcol("m", "user")).alias("users"),
                    ],
                )?
                .sort(vec![ident("n").sort(false, true), qcol("u", "activity").sort(true, false)])?
        }
        "mj2_marked_point" => {
            let u = ctx
                .table("unmarked")
                .await?
                .alias("u")?
                .filter(qcol("u", "vendor").eq(lit(vendor)))?;
            let m = ctx.table("marked").await?.alias("m")?;
            let mut select: Vec<Expr> = [
                ":row", "event_id", "case_id", "activity", "occurred_at:t", "vendor", "company", "item_type",
            ]
            .iter()
            .map(|c| qcol("u", c))
            .collect();
            select.extend(["net_worth", "user", "resource"].iter().map(|c| qcol("m", c)));
            u.join_on(m, JoinType::Inner, vec![qcol("u", ":row").eq(qcol("m", ":row"))])?
                .select(select)?
                .sort(vec![qcol("u", ":row").sort(true, false)])?
        }
        "law_check" => ctx.table("dataset").await?.select(law_exprs())?,
        other => panic!("unknown query {other}"),
    })
}

pub async fn register_lake(ctx: &SessionContext, dir: &std::path::Path) {
    for (table, file) in [
        ("events", "data.parquet"),
        ("dataset", "data.parquet"),
        ("cases", "cases.parquet"),
        ("unmarked", "unmarked.parquet"),
        ("marked", "marked.parquet"),
    ] {
        ctx.register_parquet(table, dir.join(file).to_str().expect("path"), ParquetReadOptions::default())
            .await
            .expect("register");
    }
}

/// MANIFEST-BENCH-1's landing in DataFusion. `typed`: the typing pass runs
/// on `threads` partitions (an explicit round-robin after row numbering)
/// and the laws are a typed plan; otherwise all of it is SQL text.
pub async fn land(csv: PathBuf, out: PathBuf, threads: usize, typed: bool) {
    std::fs::create_dir_all(&out).expect("out");
    let ctx = session(threads, true);
    let started = Instant::now();
    let (sha, source_bytes) = sha256_file(&csv);
    let hashed = secs(started);
    let schema = Schema::new(
        COLUMNS
            .iter()
            .map(|(n, _, _)| Field::new(*n, DataType::Utf8, true))
            .collect::<Vec<_>>(),
    );
    ctx.register_csv(
        "file_src",
        csv.to_str().expect("path"),
        CsvReadOptions::new().schema(&schema).has_header(true).quote(b'"'),
    )
    .await
    .expect("csv");
    let at = Instant::now();
    if typed {
        let numbered = ctx
            .sql(&numbered_sql(Dialect::Df, "file_src"))
            .await
            .expect("numbered")
            .repartition(Partitioning::RoundRobinBatch(threads))
            .expect("repartition");
        ctx.register_table("numbered", numbered.into_view()).expect("numbered");
        exec(&ctx, &format!("CREATE TABLE checked AS {}", typed_sql(Dialect::Df, "numbered"))).await;
    } else {
        exec(&ctx, &format!("CREATE TABLE checked AS {}", crate::checked_sql(Dialect::Df, "file_src"))).await;
    }
    exec(&ctx, &format!("CREATE TABLE dataset AS {}", dataset_sql(Dialect::Df))).await;
    exec(&ctx, &format!("CREATE TABLE quarantine AS {}", quarantine_sql())).await;
    exec(&ctx, "DROP TABLE checked").await;
    let typed_s = secs(at);
    let at = Instant::now();
    if typed {
        let outcome = ctx
            .table("dataset")
            .await
            .expect("dataset")
            .select(law_exprs())
            .expect("laws")
            .cache()
            .await
            .expect("outcome");
        ctx.register_table("outcome", outcome.into_view()).expect("outcome");
    } else {
        exec(&ctx, &format!("CREATE TABLE outcome AS {}", outcome_sql(Dialect::Df, "dataset"))).await;
    }
    for i in 0..LAWS.len() {
        exec(&ctx, &law_insert_sql(i)).await;
    }
    let residue = count(&ctx, "SELECT count(*) FROM outcome WHERE r0 OR r1 OR r2").await;
    ctx.deregister_table("outcome").expect("drop outcome");
    let laws = secs(at);
    let at = Instant::now();
    let data = out.join("data.parquet");
    let held = out.join("held.parquet");
    for (sql, path) in [(data_select(), &data), (held_select(), &held)] {
        let _ = std::fs::remove_file(path);
        exec(
            &ctx,
            &format!(
                "COPY ({sql}) TO '{}' STORED AS PARQUET OPTIONS ('format.compression' 'zstd(3)', 'format.max_row_group_size' '122880')",
                path.to_string_lossy()
            ),
        )
        .await;
    }
    let written = secs(at);
    let at = Instant::now();
    let admitted = count(&ctx, &format!("SELECT count(*) FROM dataset WHERE {}", trusted())).await;
    let held_rows = count(&ctx, "SELECT count(*) FROM quarantine").await;
    let mut reasons = serde_json::Map::new();
    for batch in exec(
        &ctx,
        &format!("SELECT {r}, count(*) FROM quarantine GROUP BY {r} ORDER BY {r}", r = q(":reason")),
    )
    .await
    {
        let n = cast(batch.column(1), &DataType::UInt64).expect("n");
        let n = n.as_primitive::<datafusion::arrow::datatypes::UInt64Type>();
        for row in 0..batch.num_rows() {
            reasons.insert(text_at(&batch, 0, row).unwrap_or_default(), json!(n.value(row)));
        }
    }
    let mut valid = Vec::new();
    for dir in ["ASC", "DESC"] {
        let batches = exec(
            &ctx,
            &format!(
                "SELECT {c} FROM dataset WHERE {c} IS NOT NULL AND {} ORDER BY {t} {dir}, {n} {dir} LIMIT 1",
                trusted(),
                c = q("occurred_at"),
                t = q("occurred_at:t"),
                n = q("occurred_at:n")
            ),
        )
        .await;
        valid.push(batches.first().and_then(|b| text_at(b, 0, 0)));
    }
    let counted = secs(at);
    let (data_sha, data_bytes) = sha256_file(&data);
    let (held_sha, held_bytes) = sha256_file(&held);
    let total = secs(started);
    let rows = admitted + held_rows;
    emit(json!({
        "bench": "land", "engine": if typed { "datafusion-typed" } else { "datafusion-sql" }, "threads": threads,
        "rows": rows, "admitted": admitted, "held": held_rows, "reasons": reasons, "residue": residue,
        "source": {"sha256": sha, "bytes": source_bytes},
        "data": {"sha256": data_sha, "bytes": data_bytes}, "heldFile": {"sha256": held_sha, "bytes": held_bytes},
        "validTime": valid,
        "seconds": {"hash": hashed, "readCsv": 0.0, "type": typed_s, "laws": laws, "write": written, "counts": counted, "total": total},
        "rowsPerSecond": rows as f64 / total,
    }));
}

fn shape(batches: &[RecordBatch]) -> (usize, String) {
    let rows = batches.iter().map(RecordBatch::num_rows).sum();
    let text = datafusion::arrow::util::pretty::pretty_format_batches(batches)
        .map(|t| t.to_string())
        .unwrap_or_default();
    (rows, text)
}

/// Dashboard and marked-join latency, through SQL text and typed plans.
pub async fn queries(dir: PathBuf, threads: usize) {
    let ctx = session(threads, false);
    register_lake(&ctx, &dir).await;
    let vendor = std::fs::read_to_string(dir.join("vendor.txt")).expect("vendor");
    let mut out = serde_json::Map::new();
    for name in QUERIES {
        let sql = query_sql(Dialect::Df, name, &vendor);
        let mut result = serde_json::Map::new();
        let mut texts = Vec::new();
        for path in ["sql", "typed"] {
            let mut times = Vec::new();
            let mut plans = Vec::new();
            let mut last = Vec::new();
            for _ in 0..6 {
                let at = Instant::now();
                let frame = if path == "sql" {
                    ctx.sql(&sql).await.unwrap_or_else(|e| panic!("{e}\n{sql}"))
                } else {
                    builder_query(&ctx, name, &vendor).await.expect(name)
                };
                plans.push(secs(at));
                last = frame.collect().await.unwrap_or_else(|e| panic!("{name}: {e}"));
                times.push(secs(at));
            }
            let cold = times.remove(0);
            plans.remove(0);
            let (rows, text) = shape(&last);
            texts.push(text);
            result.insert(
                path.into(),
                json!({"rows": rows, "coldMs": cold * 1e3, "warmMs": median(&mut times) * 1e3, "logicalPlanMs": median(&mut plans) * 1e3}),
            );
        }
        result.insert("sqlEqualsTyped".into(), json!(texts[0] == texts[1]));
        out.insert(name.into(), json!(result));
    }
    emit(json!({"bench": "queries", "engine": "datafusion", "threads": threads, "queries": out}));
}
