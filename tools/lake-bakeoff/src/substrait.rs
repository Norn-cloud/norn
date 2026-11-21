//! Substrait round trips: a law plan and a metric plan produced by
//! datafusion-substrait from typed plans, consumed back and executed, and
//! offered to DuckDB's `substrait` extension (and DuckDB's plan back).

use std::path::PathBuf;
use std::time::Instant;

use datafusion::arrow::record_batch::RecordBatch;
use datafusion::functions_aggregate::expr_fn::sum;
use datafusion::prelude::*;
use datafusion_substrait::logical_plan::consumer::from_substrait_plan;
use datafusion_substrait::logical_plan::producer::to_substrait_plan;
use datafusion_substrait::substrait::proto::Plan;
use prost::Message as _;
use serde_json::{Value as Json, json};

use crate::df::{builder_query, register_lake, session};
use crate::{emit, q, secs};

fn pretty(batches: &[RecordBatch]) -> String {
    datafusion::arrow::util::pretty::pretty_format_batches(batches)
        .map(|t| t.to_string())
        .unwrap_or_default()
}

/// The law plan: per law, how many rows are residue and how many fail.
async fn law_plan(ctx: &SessionContext) -> datafusion::common::Result<DataFrame> {
    let outcome = builder_query(ctx, "law_check", "").await?;
    let mut aggregates = Vec::new();
    for i in 0..3 {
        for prefix in ["r", "f"] {
            let name = format!("{prefix}{i}");
            aggregates.push(sum(cast(ident(&name), datafusion::arrow::datatypes::DataType::Int64)).alias(name));
        }
    }
    outcome.aggregate(Vec::<Expr>::new(), aggregates)
}

async fn round_trip(ctx: &SessionContext, frame: DataFrame) -> (Json, Option<Vec<u8>>) {
    let state = ctx.state();
    let plan = frame.logical_plan().clone();
    let direct = match frame.collect().await {
        Ok(b) => pretty(&b),
        Err(e) => return (json!({"ok": false, "stage": "direct", "error": e.to_string()}), None),
    };
    let at = Instant::now();
    let produced = match to_substrait_plan(&plan, &state) {
        Ok(p) => p,
        Err(e) => return (json!({"ok": false, "stage": "produce", "error": e.to_string()}), None),
    };
    let bytes = produced.encode_to_vec();
    let produce_ms = secs(at) * 1e3;
    let at = Instant::now();
    let decoded = Plan::decode(bytes.as_slice()).expect("decode");
    let consumed = match from_substrait_plan(&state, &decoded).await {
        Ok(p) => p,
        Err(e) => return (json!({"ok": false, "stage": "consume", "error": e.to_string(), "bytes": bytes.len()}), Some(bytes)),
    };
    let consume_ms = secs(at) * 1e3;
    let back = match ctx.execute_logical_plan(consumed).await {
        Ok(f) => match f.collect().await {
            Ok(b) => pretty(&b),
            Err(e) => return (json!({"ok": false, "stage": "execute", "error": e.to_string()}), Some(bytes)),
        },
        Err(e) => return (json!({"ok": false, "stage": "plan", "error": e.to_string()}), Some(bytes)),
    };
    (
        json!({"ok": back == direct, "bytes": bytes.len(), "produceMs": produce_ms, "consumeMs": consume_ms, "result": direct, "roundTripResult": back}),
        Some(bytes),
    )
}

fn duck_conn(dir: &std::path::Path) -> Result<duckdb::Connection, String> {
    let conn = duckdb::Connection::open_in_memory().map_err(|e| e.to_string())?;
    for (view, file) in [("events", "data.parquet"), ("dataset", "data.parquet")] {
        conn.execute_batch(&format!(
            "CREATE VIEW {view} AS SELECT * FROM read_parquet('{}')",
            dir.join(file).to_string_lossy()
        ))
        .map_err(|e| e.to_string())?;
    }
    conn.execute_batch("INSTALL substrait FROM community; LOAD substrait;")
        .map_err(|e| format!("loading the substrait extension: {e}"))?;
    Ok(conn)
}

fn duck_from_substrait(dir: &std::path::Path, bytes: &[u8]) -> Json {
    let run = || -> Result<Json, String> {
        let conn = duck_conn(dir)?;
        let mut stmt = conn.prepare("CALL from_substrait(?)").map_err(|e| e.to_string())?;
        let rows: usize = stmt
            .query_arrow([bytes])
            .map_err(|e| e.to_string())?
            .map(|b| b.num_rows())
            .sum();
        Ok(json!({"rows": rows}))
    };
    run().map_or_else(|e| json!({"ok": false, "error": e}), |d| json!({"ok": true, "detail": d}))
}

/// DuckDB's own Substrait for the metric query, consumed by DataFusion.
async fn duck_to_datafusion(ctx: &SessionContext, dir: &std::path::Path, sql: &str) -> Json {
    let produced = (|| -> Result<Vec<u8>, String> {
        let conn = duck_conn(dir)?;
        conn.query_row("CALL get_substrait(?)", [sql], |row| row.get::<_, Vec<u8>>(0))
            .map_err(|e| e.to_string())
    })();
    match produced {
        Ok(bytes) => consume(ctx, &bytes).await,
        Err(e) => json!({"ok": false, "stage": "duckdb get_substrait", "error": e}),
    }
}

/// A Substrait plan another engine produced, consumed and run by DataFusion.
async fn consume(ctx: &SessionContext, bytes: &[u8]) -> Json {
    let plan = match Plan::decode(bytes) {
        Ok(p) => p,
        Err(e) => return json!({"ok": false, "stage": "decode", "error": e.to_string()}),
    };
    match from_substrait_plan(&ctx.state(), &plan).await {
        Ok(lp) => match ctx.execute_logical_plan(lp).await {
            Ok(f) => match f.collect().await {
                Ok(b) => json!({"ok": true, "bytes": bytes.len(), "result": pretty(&b)}),
                Err(e) => json!({"ok": false, "stage": "execute", "error": e.to_string()}),
            },
            Err(e) => json!({"ok": false, "stage": "plan", "error": e.to_string()}),
        },
        Err(e) => json!({"ok": false, "stage": "consume", "error": e.to_string()}),
    }
}

pub async fn probe(dir: PathBuf, threads: usize) {
    let ctx = session(threads, false);
    register_lake(&ctx, &dir).await;
    let (law, law_bytes) = round_trip(&ctx, law_plan(&ctx).await.expect("law plan")).await;
    let metric_frame = builder_query(&ctx, "q2_topn", "").await.expect("metric plan");
    let (metric, metric_bytes) = round_trip(&ctx, metric_frame).await;
    // Kept for engines outside this binary (another DuckDB build).
    for (name, bytes) in [("law", &law_bytes), ("metric", &metric_bytes)] {
        if let Some(b) = bytes {
            std::fs::write(dir.join(format!("{name}.substrait")), b).expect("plan file");
        }
    }
    let duck_law = law_bytes.as_deref().map(|b| duck_from_substrait(&dir, b));
    let duck_metric = metric_bytes.as_deref().map(|b| duck_from_substrait(&dir, b));
    // DuckDB's planner names its own functions; a plain metric shape.
    let duck_sql = format!(
        "SELECT {v}, sum({nw}) AS total FROM events GROUP BY {v} ORDER BY total DESC LIMIT 10",
        v = q("vendor"),
        nw = q("net_worth")
    );
    let back = duck_to_datafusion(&ctx, &dir, &duck_sql).await;
    // The same shape from DuckDB 1.2.2's `get_substrait` (the last release
    // with the extension), saved by an outside run.
    let from_file = match std::fs::read(dir.join("duckdb-metric.substrait")) {
        Ok(bytes) => consume(&ctx, &bytes).await,
        Err(e) => json!({"ok": false, "stage": "read", "error": e.to_string()}),
    };
    emit(json!({
        "bench": "substrait",
        "datafusionRoundTrip": {"law": law, "metric": metric},
        "duckdbFromDatafusionPlan": {"law": duck_law, "metric": duck_metric},
        "duckdbPlanIntoDatafusion": back,
        "duckdb122PlanIntoDatafusion": from_file,
    }));
}
