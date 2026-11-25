//! VIEW-ENGINE-1 (#1765): an admitted transform lowered to `DataFusion` over
//! the same authorized snapshot the exact interpreter reads, as the `DuckDB`
//! engine lowers it (`compute::olap_transform`), with the same subset and
//! the same refusals.
//!
//! The plan is lowered first ([`lower`]): every node becomes a typed
//! specification with its `DataFusion` expressions built from the admitted
//! plan alone, so a plan outside the subset refuses
//! `core.compute.unsupported_engine` before anything is read. Each input
//! alias becomes one typed table of exactly the rows the reader may read,
//! each link the plan joins along one edge table, and each node, in plan
//! order, one bounded table materialized from its inputs ([`table`]): at
//! most one row past the bound is ever computed, and a node past it refuses
//! the whole result as the exact engine does. Every row carries a pointer
//! back to the rows it derives from, from which its lineage is resolved
//! exactly; calendar grains are bucketed by the exact interpreter's own
//! function. No caller text is ever a statement: columns are plan names and
//! values are typed literals. Execution is single-threaded on a session of
//! its own with no files, no spill and a bounded memory pool.

mod kinds;
mod lower;
mod table;

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use datafusion::arrow::array::StringArray;
use datafusion::arrow::datatypes::{DataType, Field, Schema};
use datafusion::arrow::record_batch::RecordBatch;
use datafusion::common::JoinType;
use datafusion::datasource::MemTable;
use datafusion::execution::disk_manager::{DiskManagerBuilder, DiskManagerMode};
use datafusion::execution::memory_pool::GreedyMemoryPool;
use datafusion::execution::runtime_env::RuntimeEnvBuilder;
use datafusion::logical_expr::{Expr, ExprFunctionExt as _, lit};
use datafusion::prelude::{DataFrame, SessionConfig, SessionContext};
use norn_checker::transform::eval::{self, EvalError, MemberRef, TransformInput, TransformOutput};
use norn_checker::transform::{MAX_ROWS, TransformPlan, Type};
use norn_ir::RuntimeValue;
use norn_ir::metric::TimeGrain;

use super::transform::{RowAccess, TransformResult, eval_refusal};
use super::{CellError, CellStore, Engine, SqlEngine, refused};
use crate::ObjectRecord;
use kinds::{Kind, ROW, array, bound, decimal, field, key_name, keys, kind, texts, value};
use lower::{Lowered, Pairing, SOURCE, Spec, name, qualified};
use table::{Back, Table, engine, materialize};

/// The memory one evaluation's session may hold.
const MEMORY: usize = 256 << 20;
/// The dedupe rank column.
const RANK: &str = "__rn";

impl<E: SqlEngine> CellStore<E> {
    /// VIEW-ENGINE-1: evaluate an admitted transform on `DataFusion` over the
    /// authorized snapshot [`Self::evaluate_transform`] reads; the envelope
    /// differs only in its engine label.
    ///
    /// # Errors
    /// As [`Self::evaluate_transform`], and `core.compute.unsupported_engine`
    /// for a plan outside the lowered subset (before anything is read).
    pub fn evaluate_transform_datafusion(
        &self,
        plan: &TransformPlan,
        world: &str,
        scope: &str,
        access: impl FnMut(&ObjectRecord) -> Result<RowAccess, CellError>,
    ) -> Result<TransformResult, CellError> {
        // Lower first: an unsupported plan refuses before any read.
        let lowered = lower::lower(plan)?;
        self.evaluate_transform_with(
            plan,
            (world, scope),
            access,
            Engine::DataFusion,
            |plan, input| block_on(execute(plan, input, &lowered))?,
        )
    }
}

/// Drive `future` on a current-thread runtime on a thread of its own, so a
/// caller already inside a runtime never nests one.
fn block_on<F>(future: F) -> Result<F::Output, CellError>
where
    F: std::future::Future + Send,
    F::Output: Send,
{
    std::thread::scope(|scope| {
        let handle = std::thread::Builder::new()
            .name("norn-transform".to_owned())
            .spawn_scoped(scope, || {
                tokio::runtime::Builder::new_current_thread()
                    .build()
                    .map(|runtime| runtime.block_on(future))
                    .map_err(engine)
            })
            .map_err(engine)?;
        handle
            .join()
            .map_err(|_panicked| CellError::Engine("a transform failed".to_owned()))?
    })
}

/// A single-threaded session with no files, no spill and a bounded pool.
fn context() -> Result<SessionContext, CellError> {
    let runtime = RuntimeEnvBuilder::new()
        .with_memory_pool(Arc::new(GreedyMemoryPool::new(MEMORY)))
        .with_disk_manager_builder(
            DiskManagerBuilder::default().with_mode(DiskManagerMode::Disabled),
        )
        .build_arc()
        .map_err(engine)?;
    let config = SessionConfig::new()
        .with_target_partitions(1)
        .with_information_schema(false);
    Ok(SessionContext::new_with_config_rt(config, runtime))
}

fn bounded() -> CellError {
    refused("core.compute.bound_exceeded")
}

async fn execute(
    plan: &TransformPlan,
    input: &TransformInput,
    lowered: &Lowered,
) -> Result<TransformOutput, CellError> {
    let ctx = context()?;
    let mut tables: Vec<Table> = Vec::with_capacity(lowered.specs.len());
    let mut members: Vec<MemberRef> = Vec::new();
    let mut node_rows = BTreeMap::new();
    for (id, spec) in plan.order.iter().zip(&lowered.specs) {
        let table = match spec {
            Spec::Scan { alias } => scan(plan, input, id, alias, &mut members)?,
            derived @ (Spec::Filter { .. }
            | Spec::Project { .. }
            | Spec::Join { .. }
            | Spec::Aggregate { .. }
            | Spec::Window { .. }
            | Spec::Union { .. }
            | Spec::Dedupe { .. }) => node(&ctx, input, derived, &tables).await?,
        };
        // A node past the bound refuses the whole result, as the exact
        // engine's does; no node computes more than one row past it.
        if table.rows() > MAX_ROWS {
            return Err(bounded());
        }
        node_rows.insert(id.clone(), u64::try_from(table.rows()).unwrap_or(u64::MAX));
        tables.push(table);
    }
    let output = plan
        .order
        .iter()
        .position(|id| *id == plan.output)
        .and_then(|at| tables.get(at))
        .ok_or_else(|| CellError::Engine("the output node did not run".to_owned()))?;
    let rows = answer(plan, output, &members)?;
    eval::order_output(plan, rows, node_rows)
        .map_err(|error: EvalError| refused(eval_refusal(&error)))
}

/// One answer row: its values and the input rows it derives from.
type AnswerRow = (BTreeMap<String, RuntimeValue>, BTreeSet<MemberRef>);

/// The output node's rows as values of the output schema, with lineage.
fn answer(
    plan: &TransformPlan,
    output: &Table,
    members: &[MemberRef],
) -> Result<Vec<AnswerRow>, CellError> {
    let schema = plan.output_schema();
    let kinds = schema
        .columns
        .iter()
        .map(|column| kind(&column.ty))
        .collect::<Result<Vec<_>, _>>()?;
    let mut rows = Vec::with_capacity(output.rows());
    let mut lineage = output.lineage.iter();
    for batch in &output.batches {
        let columns = schema
            .columns
            .iter()
            .map(|column| {
                batch
                    .column_by_name(&column.name)
                    .ok_or_else(|| CellError::Engine(format!("no output column {}", column.name)))
                    .and_then(|array| texts(array.as_ref()))
            })
            .collect::<Result<Vec<_>, _>>()?;
        for row in 0..batch.num_rows() {
            let mut values = BTreeMap::new();
            for ((column, kind), texts) in schema.columns.iter().zip(&kinds).zip(&columns) {
                let text = texts.get(row).and_then(Option::as_deref);
                values.insert(column.name.clone(), value(text, *kind)?);
            }
            let derived = lineage
                .next()
                .ok_or_else(|| CellError::Engine("a row without lineage".to_owned()))?;
            let refs = derived
                .iter()
                .map(|member| {
                    usize::try_from(*member)
                        .ok()
                        .and_then(|member| members.get(member))
                        .cloned()
                        .ok_or_else(|| CellError::Engine("a lineage past the inputs".to_owned()))
                })
                .collect::<Result<BTreeSet<_>, _>>()?;
            rows.push((values, refs));
        }
    }
    Ok(rows)
}

/// The scan of `alias`: the reader's readable rows, typed.
fn scan(
    plan: &TransformPlan,
    input: &TransformInput,
    id: &str,
    alias: &str,
    members: &mut Vec<MemberRef>,
) -> Result<Table, CellError> {
    let rows = input
        .rows
        .get(alias)
        .ok_or_else(|| CellError::Engine(format!("the input {alias} was not read")))?;
    if rows.len() > MAX_ROWS {
        return Err(bounded());
    }
    let schema = &plan
        .nodes
        .get(id)
        .ok_or_else(|| CellError::Engine(format!("no node {id}")))?
        .schema;
    let mut fields = Vec::new();
    let mut columns = Vec::new();
    for column in &schema.columns {
        let kind = kind(&column.ty)?;
        let column_texts = rows
            .iter()
            .map(|row| {
                if column.name == "id" {
                    Ok(Some(row.id.clone()))
                } else {
                    bound(row.values.get(&column.name))
                }
            })
            .collect::<Result<Vec<_>, _>>()?;
        fields.push(field(&column.name, kind));
        columns.push(array(kind, &column_texts)?);
        if kind == Kind::Timestamp {
            fields.push(Field::new(key_name(&column.name), decimal(0), true));
            columns.push(keys(&column_texts)?);
        }
    }
    let mut lineage = Vec::with_capacity(rows.len());
    for row in rows {
        let member = u32::try_from(members.len()).map_err(|_wide| bounded())?;
        members.push(MemberRef {
            alias: alias.to_owned(),
            id: row.id.clone(),
            version: row.version,
        });
        lineage.push(Arc::from(vec![member]));
    }
    Table::of(fields, columns, lineage)
}

/// Collect `frame` (at most one row past the bound) as a table pointing into `back`.
async fn collect(frame: Result<DataFrame, CellError>, back: Back<'_>) -> Result<Table, CellError> {
    let bound = MAX_ROWS.checked_add(1).ok_or_else(bounded)?;
    let frame = frame?.limit(0, Some(bound)).map_err(engine)?;
    let schema = frame.schema().as_arrow().clone();
    let batches = frame.collect().await.map_err(engine)?;
    materialize(&schema, &batches, &back)
}

fn input_of(tables: &[Table], at: usize) -> Result<&Table, CellError> {
    tables
        .get(at)
        .ok_or_else(|| CellError::Engine("a node ran before its input".to_owned()))
}

async fn node(
    ctx: &SessionContext,
    input: &TransformInput,
    spec: &Spec,
    tables: &[Table],
) -> Result<Table, CellError> {
    match spec {
        Spec::Scan { .. } => Err(CellError::Engine("a scan is not a derived node".to_owned())),
        Spec::Filter {
            input: at,
            predicate,
        } => {
            let from = input_of(tables, *at)?;
            let frame = from.frame(ctx)?.filter(predicate.clone()).map_err(engine);
            collect(frame, Back::One(from)).await
        }
        Spec::Project { input: at, items } | Spec::Window { input: at, items } => {
            let from = input_of(tables, *at)?;
            let frame = from.frame(ctx)?.select(items.clone()).map_err(engine);
            collect(frame, Back::One(from)).await
        }
        Spec::Join {
            left,
            right,
            pairing,
            outer,
            left_columns,
            right_columns,
            right_carried,
        } => {
            let (left, right) = (input_of(tables, *left)?, input_of(tables, *right)?);
            let frame = join(
                ctx,
                input,
                (left, right),
                pairing,
                *outer,
                (left_columns, right_columns, right_carried),
            );
            collect(frame, Back::Two(left, right)).await
        }
        Spec::Aggregate {
            input: at,
            buckets,
            groups,
            measures,
        } => {
            let from = bucketed(input_of(tables, *at)?, buckets)?;
            let frame = from
                .frame(ctx)?
                .aggregate(groups.clone(), measures.clone())
                .map_err(engine);
            collect(frame, Back::Many(&from)).await
        }
        Spec::Union { inputs, items } => {
            let mut frame: Option<DataFrame> = None;
            let mut branches = Vec::with_capacity(inputs.len());
            for (at, branch_items) in inputs.iter().zip(items) {
                let from = input_of(tables, *at)?;
                branches.push(from);
                let branch = from
                    .frame(ctx)?
                    .select(branch_items.clone())
                    .map_err(engine)?;
                frame = Some(match frame {
                    None => branch,
                    Some(before) => before.union(branch).map_err(engine)?,
                });
            }
            let frame = frame.ok_or_else(|| CellError::Engine("a union of nothing".to_owned()));
            collect(frame, Back::Branch(branches)).await
        }
        Spec::Dedupe {
            input: at,
            partition,
            order,
            columns,
        } => {
            use datafusion::functions_window::expr_fn::row_number;
            let from = input_of(tables, *at)?;
            let mut kept: Vec<Expr> = columns.iter().map(|column| name(column)).collect();
            kept.push(name(ROW));
            let ranked = row_number()
                .partition_by(partition.clone())
                .order_by(order.clone())
                .build()
                .map_err(engine)?
                .alias(RANK);
            let mut items = kept.clone();
            items.push(ranked);
            let frame = from
                .frame(ctx)?
                .select(items)
                .and_then(|frame| frame.filter(name(RANK).eq(lit(1_u64))))
                .and_then(|frame| frame.select(kept))
                .map_err(engine);
            collect(frame, Back::One(from)).await
        }
    }
}

/// The columns a join keeps: the left's, the right's renamed, and the right
/// node's own (for a link join's edge side).
type JoinColumns<'s> = (&'s [String], &'s [(String, String)], &'s [String]);

fn join(
    ctx: &SessionContext,
    input: &TransformInput,
    (left, right): (&Table, &Table),
    pairing: &Pairing,
    outer: bool,
    (left_columns, right_columns, right_carried): JoinColumns<'_>,
) -> Result<DataFrame, CellError> {
    let a = left.frame(ctx)?.alias("a").map_err(engine)?;
    let (b, on) = match pairing {
        Pairing::Link {
            alias,
            link,
            source,
            right_id,
        } => {
            let e = ctx
                .read_table(edges(input, alias, link)?)
                .and_then(|frame| frame.alias("e"))
                .map_err(engine)?;
            let r = right.frame(ctx)?.alias("r").map_err(engine)?;
            let mut items = vec![qualified("e", "src").alias(SOURCE)];
            items.extend(
                right_carried
                    .iter()
                    .map(|column| qualified("r", column).alias(column)),
            );
            items.push(qualified("r", ROW).alias(ROW));
            let b = e
                .join_on(
                    r,
                    JoinType::Inner,
                    vec![qualified("r", right_id).eq(qualified("e", "dst"))],
                )
                .and_then(|frame| frame.select(items))
                .and_then(|frame| frame.alias("b"))
                .map_err(engine)?;
            (b, vec![qualified("b", SOURCE).eq(qualified("a", source))])
        }
        Pairing::Keys(pairs) => (
            right.frame(ctx)?.alias("b").map_err(engine)?,
            pairs
                .iter()
                .map(|(l, r)| qualified("a", l).eq(qualified("b", r)))
                .collect(),
        ),
    };
    let kind = if outer {
        JoinType::Left
    } else {
        JoinType::Inner
    };
    let mut items: Vec<Expr> = left_columns
        .iter()
        .map(|column| qualified("a", column).alias(column))
        .collect();
    items.extend(
        right_columns
            .iter()
            .map(|(from, to)| qualified("b", from).alias(to)),
    );
    items.push(qualified("a", ROW).alias(lower::LEFT));
    items.push(qualified("b", ROW).alias(lower::RIGHT));
    a.join_on(b, kind, on)
        .and_then(|frame| frame.select(items))
        .map_err(engine)
}

/// The edge table of `(alias, link)`: each (source, target) pair once.
fn edges(input: &TransformInput, alias: &str, link: &str) -> Result<Arc<MemTable>, CellError> {
    let pairs: BTreeSet<(&String, &String)> = input
        .links
        .get(&(alias.to_owned(), link.to_owned()))
        .into_iter()
        .flat_map(|by_source| {
            by_source
                .iter()
                .flat_map(|(source, targets)| targets.iter().map(move |target| (source, target)))
        })
        .collect();
    let schema = Arc::new(Schema::new(vec![
        Field::new("src", DataType::Utf8, false),
        Field::new("dst", DataType::Utf8, false),
    ]));
    let batch = RecordBatch::try_new(
        Arc::clone(&schema),
        vec![
            Arc::new(StringArray::from_iter_values(
                pairs.iter().map(|(s, _)| s.as_str()),
            )),
            Arc::new(StringArray::from_iter_values(
                pairs.iter().map(|(_, t)| t.as_str()),
            )),
        ],
    )
    .map_err(engine)?;
    MemTable::try_new(schema, vec![vec![batch]])
        .map(Arc::new)
        .map_err(engine)
}

/// `from` with each grained group's bucket column added, bucketed by the
/// exact interpreter's own calendar function.
fn bucketed(
    from: &Table,
    buckets: &[(String, String, TimeGrain, Type)],
) -> Result<Table, CellError> {
    if buckets.is_empty() {
        return Ok(Table {
            schema: Arc::clone(&from.schema),
            batches: from.batches.clone(),
            lineage: from.lineage.clone(),
        });
    }
    let mut fields: Vec<Field> = from
        .schema
        .fields()
        .iter()
        .map(|f| f.as_ref().clone())
        .collect();
    fields.extend(
        buckets
            .iter()
            .map(|(_, bucket, _, _)| Field::new(bucket, DataType::Utf8, true)),
    );
    let schema = Arc::new(Schema::new(fields));
    let mut batches = Vec::with_capacity(from.batches.len());
    for batch in &from.batches {
        let mut columns = batch.columns().to_vec();
        for (source, _, grain, ty) in buckets {
            let column = batch
                .column_by_name(source)
                .ok_or_else(|| CellError::Engine(format!("no column {source}")))?;
            let days = texts(column.as_ref())?
                .into_iter()
                .map(|text| {
                    text.map(|text| {
                        eval::bucket_text(&text, ty, *grain)
                            .map_err(|error| refused(eval_refusal(&error)))
                    })
                    .transpose()
                })
                .collect::<Result<Vec<_>, _>>()?;
            columns.push(Arc::new(StringArray::from(days)));
        }
        batches.push(RecordBatch::try_new(Arc::clone(&schema), columns).map_err(engine)?);
    }
    Ok(Table {
        schema,
        batches,
        lineage: from.lineage.clone(),
    })
}
