//! Planning cost: Norn's law and question shapes, and chains of views/CTEs,
//! through SQL text (SqlToRel) and as typed plans (LogicalPlanBuilder).

use std::path::PathBuf;
use std::time::Instant;

use datafusion::common::Column;
use datafusion::functions_aggregate::count::count_all;
use datafusion::functions_aggregate::expr_fn::sum;
use datafusion::logical_expr::{LogicalPlan, LogicalPlanBuilder};
use datafusion::prelude::*;
use serde_json::{Value as Json, json};

use crate::df::{builder_query, qcol, register_lake, session};
use crate::{Dialect, QUERIES, emit, q, query_sql, secs};

/// Chain shapes: each level reads the one below once (`linear`), or twice
/// (`union`: `UNION ALL` of it with itself; `join`: a self-join on case).
pub const CHAINS: [&str; 6] = [
    "cte_linear",
    "cte_union",
    "cte_join",
    "view_linear",
    "view_union",
    "view_join",
];

pub const DEPTHS: std::ops::RangeInclusive<usize> = 1..=24;

/// A sweep stops after the first depth whose planning passes this.
pub const STOP_MS: f64 = 8_000.0;

fn level(kind: &str, below: &str) -> String {
    let (ci, nw) = (q("case_id"), q("net_worth"));
    if kind.ends_with("linear") {
        format!("SELECT {ci}, {nw} + 1 AS {nw} FROM {below} WHERE {nw} > 0")
    } else if kind.ends_with("union") {
        format!("SELECT {ci}, {nw} FROM {below} UNION ALL SELECT {ci}, {nw} FROM {below}")
    } else {
        format!(
            "SELECT a.{ci} AS {ci}, a.{nw} + b.{nw} AS {nw} FROM {below} AS a JOIN {below} AS b ON a.{ci} = b.{ci}"
        )
    }
}

fn top(name: &str) -> String {
    format!("SELECT count(*) AS n, sum({}) AS s FROM {name}", q("net_worth"))
}

pub fn cte_sql(kind: &str, depth: usize) -> String {
    let mut ctes = vec![format!(
        "l0 AS (SELECT {}, {} FROM events)",
        q("case_id"),
        q("net_worth")
    )];
    for k in 1..=depth {
        ctes.push(format!("l{k} AS ({})", level(kind, &format!("l{}", k - 1))));
    }
    format!("WITH {} {}", ctes.join(", "), top(&format!("l{depth}")))
}

pub fn view_ddl(kind: &str, depth: usize, prefix: &str) -> Vec<String> {
    let mut ddl = vec![format!(
        "CREATE VIEW {prefix}_v0 AS SELECT {}, {} FROM events",
        q("case_id"),
        q("net_worth")
    )];
    for k in 1..=depth {
        ddl.push(format!(
            "CREATE VIEW {prefix}_v{k} AS {}",
            level(kind, &format!("{prefix}_v{}", k - 1))
        ));
    }
    ddl
}

pub fn view_query(prefix: &str, depth: usize) -> String {
    top(&format!("{prefix}_v{depth}"))
}

/// The chain as one typed plan: a level that reads the one below twice
/// shares it (`Arc`), so the plan is a DAG, not a tree.
async fn chain_plan(ctx: &SessionContext, kind: &str, depth: usize) -> LogicalPlan {
    let nw = || ident("net_worth");
    let mut plan = ctx
        .table("events")
        .await
        .expect("events")
        .select(vec![ident("case_id"), nw()])
        .expect("l0")
        .into_unoptimized_plan();
    for _ in 1..=depth {
        plan = if kind.ends_with("linear") {
            LogicalPlanBuilder::from(plan)
                .filter(nw().gt(lit(0)))
                .and_then(|b| b.project(vec![ident("case_id"), (nw() + lit(1)).alias("net_worth")]))
                .and_then(LogicalPlanBuilder::build)
                .expect("linear")
        } else if kind.ends_with("union") {
            LogicalPlanBuilder::from(plan.clone())
                .union(plan)
                .and_then(LogicalPlanBuilder::build)
                .expect("union")
        } else {
            let right = LogicalPlanBuilder::from(plan.clone())
                .alias("b")
                .and_then(LogicalPlanBuilder::build)
                .expect("b");
            LogicalPlanBuilder::from(plan)
                .alias("a")
                .and_then(|b| {
                    b.join(
                        right,
                        JoinType::Inner,
                        (vec![Column::new(Some("a"), "case_id")], vec![Column::new(Some("b"), "case_id")]),
                        None,
                    )
                })
                .and_then(|b| {
                    b.project(vec![
                        qcol("a", "case_id").alias("case_id"),
                        (qcol("a", "net_worth") + qcol("b", "net_worth")).alias("net_worth"),
                    ])
                })
                .and_then(LogicalPlanBuilder::build)
                .expect("join")
        };
    }
    LogicalPlanBuilder::from(plan)
        .aggregate(Vec::<Expr>::new(), vec![count_all().alias("n"), sum(nw()).alias("s")])
        .and_then(LogicalPlanBuilder::build)
        .expect("top")
}

/// (logical, optimize, physical) milliseconds for one plan.
async fn finish(ctx: &SessionContext, plan: &LogicalPlan) -> (f64, f64) {
    let state = ctx.state();
    let at = Instant::now();
    let optimized = state.optimize(plan).expect("optimize");
    let optimize = secs(at) * 1e3;
    let at = Instant::now();
    let physical = state
        .query_planner()
        .create_physical_plan(&optimized, &state)
        .await
        .expect("physical");
    std::hint::black_box(&physical);
    (optimize, secs(at) * 1e3)
}

async fn sql_point(ctx: &SessionContext, sql: &str) -> Json {
    let state = ctx.state();
    let at = Instant::now();
    let plan = state.create_logical_plan(sql).await.expect("sql");
    let sql_to_rel = secs(at) * 1e3;
    let (optimize, physical) = finish(ctx, &plan).await;
    json!({"sqlToRelMs": sql_to_rel, "optimizeMs": optimize, "physicalMs": physical, "totalMs": sql_to_rel + optimize + physical})
}

fn total(point: &Json) -> f64 {
    point["totalMs"].as_f64().unwrap_or(0.0) + point["ddlMs"].as_f64().unwrap_or(0.0)
}

async fn median_point<F, Fut>(reps: usize, mut f: F) -> Json
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Json>,
{
    let mut points = Vec::new();
    for _ in 0..reps {
        points.push(f().await);
    }
    points.sort_by(|a, b| total(a).total_cmp(&total(b)));
    points.swap_remove(points.len() / 2)
}

pub async fn df_plans(dir: PathBuf, threads: usize) {
    let ctx = session(threads, false);
    register_lake(&ctx, &dir).await;
    let vendor = std::fs::read_to_string(dir.join("vendor.txt")).expect("vendor");
    let mut shapes = serde_json::Map::new();
    for name in QUERIES.iter().chain(["law_check"].iter()) {
        let sql = query_sql(Dialect::Df, name, &vendor);
        let by_sql = median_point(5, || sql_point(&ctx, &sql)).await;
        let typed = median_point(5, || async {
            let at = Instant::now();
            let plan = builder_query(&ctx, name, &vendor).await.expect(name).into_unoptimized_plan();
            let build = secs(at) * 1e3;
            let (optimize, physical) = finish(&ctx, &plan).await;
            json!({"buildMs": build, "optimizeMs": optimize, "physicalMs": physical, "totalMs": build + optimize + physical})
        })
        .await;
        shapes.insert((*name).into(), json!({"sql": by_sql, "typed": typed}));
    }
    let mut chains = serde_json::Map::new();
    for kind in CHAINS {
        let mut sql_points = Vec::new();
        for depth in DEPTHS {
            let point = if kind.starts_with("view") {
                let prefix = format!("{kind}_{depth}");
                let at = Instant::now();
                for ddl in view_ddl(kind, depth, &prefix) {
                    ctx.sql(&ddl).await.expect("view ddl").collect().await.expect("view");
                }
                let ddl_ms = secs(at) * 1e3;
                let mut p = sql_point(&ctx, &view_query(&prefix, depth)).await;
                p["ddlMs"] = json!(ddl_ms);
                p
            } else {
                sql_point(&ctx, &cte_sql(kind, depth)).await
            };
            let stop = total(&point) > STOP_MS;
            let mut point = point;
            point["depth"] = json!(depth);
            sql_points.push(point);
            if stop {
                break;
            }
        }
        let mut typed_points = Vec::new();
        if kind.starts_with("cte") {
            for depth in DEPTHS {
                let at = Instant::now();
                let plan = chain_plan(&ctx, kind, depth).await;
                let build = secs(at) * 1e3;
                let (optimize, physical) = finish(&ctx, &plan).await;
                let all = build + optimize + physical;
                typed_points.push(json!({"depth": depth, "buildMs": build, "optimizeMs": optimize, "physicalMs": physical, "totalMs": all}));
                if all > STOP_MS {
                    break;
                }
            }
        }
        chains.insert(kind.into(), json!({"sql": sql_points, "typed": typed_points}));
    }
    emit(json!({"bench": "plans", "engine": "datafusion", "shapes": shapes, "chains": chains}));
}
