//! VIEW-ENGINE-1 on `DataFusion`: an admitted plan lowered, before any read,
//! to one typed specification per node with its `DataFusion` expressions
//! built. The lowered subset is the `DuckDB` engine's exactly: expressions
//! through `norn_expr::RowExpr`, no computed or compared timestamp, no
//! normalized key join, no timestamp group without a grain, no decimal
//! scale above 18 and no non-scalar column; anything else refuses
//! `core.compute.unsupported_engine` here, so an unsupported plan reads
//! nothing.

use datafusion::common::{Column as DfColumn, ScalarValue};
use datafusion::logical_expr::expr::Sort;
use datafusion::logical_expr::{Expr, ExprFunctionExt as _, Operator, binary_expr, lit, when};
use datafusion::logical_expr::{WindowFrame, WindowFrameBound, WindowFrameUnits};
use norn_checker::transform::{
    AdmittedJoin, NodeSchema, Projection, RowArithmetic, RowComparison, RowExpr, Step,
    TransformPlan, Type, TypedExpression,
};
use norn_ir::metric::TimeGrain;
use norn_ir::{AggregateFn, DedupeKeep, JoinKind, SortDirection, WindowFn};

use super::kinds::{
    Kind, MAX_SCALE, PRECISION, ROW, carried, column, key_name, kind, kind_of, ordered, unsupported,
};
use crate::CellError;
use crate::compute::{UNSUPPORTED_ENGINE, refused};

/// The pointer columns a node's rows carry back to its inputs' rows.
pub(super) const LEFT: &str = "__ra";
pub(super) const RIGHT: &str = "__rb";
pub(super) const BRANCH: &str = "__u";
pub(super) const MEMBERS: &str = "__rs";
/// A link join's source column on its right side.
pub(super) const SOURCE: &str = "__src";

/// An unqualified column, never parsed for a qualifier.
pub(super) fn name(name: &str) -> Expr {
    Expr::Column(DfColumn::new_unqualified(name))
}

/// A column of the relation `relation`.
pub(super) fn qualified(relation: &str, name: &str) -> Expr {
    Expr::Column(DfColumn::new(Some(relation), name))
}

/// How a join pairs its rows.
pub(super) enum Pairing {
    /// Along a link: the edges of `(alias, link)`, the left source column
    /// and the right identity column.
    Link {
        alias: String,
        link: String,
        source: String,
        right_id: String,
    },
    /// Equal (left, right) columns.
    Keys(Vec<(String, String)>),
}

/// One node, lowered.
pub(super) enum Spec {
    Scan {
        alias: String,
    },
    /// A node of one input whose expressions keep its row pointer.
    Filter {
        input: usize,
        predicate: Expr,
    },
    Project {
        input: usize,
        items: Vec<Expr>,
    },
    Join {
        left: usize,
        right: usize,
        pairing: Pairing,
        outer: bool,
        left_columns: Vec<String>,
        right_columns: Vec<(String, String)>,
        right_carried: Vec<String>,
    },
    Aggregate {
        input: usize,
        /// Group columns bucketed by a grain: (source, bucket column, grain, type).
        buckets: Vec<(String, String, TimeGrain, Type)>,
        groups: Vec<Expr>,
        measures: Vec<Expr>,
    },
    Window {
        input: usize,
        items: Vec<Expr>,
    },
    Union {
        inputs: Vec<usize>,
        items: Vec<Vec<Expr>>,
    },
    Dedupe {
        input: usize,
        partition: Vec<Expr>,
        order: Vec<Sort>,
        columns: Vec<String>,
    },
}

/// The plan, lowered: a spec per node in plan order, and the index of each.
pub(super) struct Lowered {
    pub specs: Vec<Spec>,
}

fn index(plan: &TransformPlan, id: &str) -> Result<usize, CellError> {
    plan.order
        .iter()
        .position(|node| node == id)
        .ok_or_else(|| refused(UNSUPPORTED_ENGINE))
}

fn schema_of<'p>(plan: &'p TransformPlan, id: &str) -> Result<&'p NodeSchema, CellError> {
    plan.nodes
        .get(id)
        .map(|node| &node.schema)
        .ok_or_else(|| refused(UNSUPPORTED_ENGINE))
}

fn valid_identifier(text: &str) -> bool {
    !text.is_empty()
        && text
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '.')
}

/// Lower `plan`, refusing anything outside the subset before any read.
pub(super) fn lower(plan: &TransformPlan) -> Result<Lowered, CellError> {
    for (alias, link) in &plan.links {
        if !valid_identifier(alias) || !valid_identifier(link) {
            return unsupported();
        }
    }
    let mut specs = Vec::with_capacity(plan.order.len());
    for id in &plan.order {
        let node = plan
            .nodes
            .get(id)
            .ok_or_else(|| refused(UNSUPPORTED_ENGINE))?;
        // Every column of every node is a scalar of a lowered kind.
        for column in &node.schema.columns {
            kind(&column.ty)?;
        }
        specs.push(lower_node(plan, &node.schema, &node.step)?);
    }
    Ok(Lowered { specs })
}

fn expression(typed: &TypedExpression, schema: &NodeSchema) -> Result<Expr, CellError> {
    let tree = typed
        .row_tree()
        .ok_or_else(|| refused(UNSUPPORTED_ENGINE))?;
    tree_expr(&tree, schema)
}

fn number(text: &str) -> Result<Expr, CellError> {
    let scale = text
        .split_once('.')
        .map_or(0, |(_, fraction)| fraction.len());
    let scale = u32::try_from(scale).map_err(|_wide| refused(UNSUPPORTED_ENGINE))?;
    if scale > MAX_SCALE {
        return unsupported();
    }
    let value = super::kinds::scaled(text, scale).ok_or_else(|| refused(UNSUPPORTED_ENGINE))?;
    let scale = i8::try_from(scale).map_err(|_wide| refused(UNSUPPORTED_ENGINE))?;
    Ok(lit(ScalarValue::Decimal128(Some(value), PRECISION, scale)))
}

/// One row expression over `schema`'s columns.
fn tree_expr(tree: &RowExpr, schema: &NodeSchema) -> Result<Expr, CellError> {
    let both = |left: &RowExpr, right: &RowExpr| -> Result<(Expr, Expr), CellError> {
        Ok((tree_expr(left, schema)?, tree_expr(right, schema)?))
    };
    Ok(match tree {
        RowExpr::Null => lit(ScalarValue::Null),
        RowExpr::Bool(flag) => lit(*flag),
        RowExpr::Number(value) => number(value.as_str())?,
        RowExpr::Text(text) => lit(text.clone()),
        RowExpr::Column(column_name) => {
            // Temporal values compare as instants, not text.
            if kind(&column(schema, column_name)?.ty)? == Kind::Timestamp {
                return unsupported();
            }
            name(column_name)
        }
        RowExpr::Not(inner) => !tree_expr(inner, schema)?,
        RowExpr::Negate(inner) => Expr::Negative(Box::new(tree_expr(inner, schema)?)),
        RowExpr::Arithmetic(op, left, right) => {
            let (left, right) = both(left, right)?;
            let operator = match op {
                RowArithmetic::Add => Operator::Plus,
                RowArithmetic::Subtract => Operator::Minus,
                RowArithmetic::Multiply => Operator::Multiply,
            };
            binary_expr(left, operator, right)
        }
        RowExpr::Compare(op, left, right) => compare(*op, left, right, schema)?,
        RowExpr::And(left, right) => {
            let (left, right) = both(left, right)?;
            left.and(right)
        }
        RowExpr::Or(left, right) => {
            let (left, right) = both(left, right)?;
            left.or(right)
        }
        RowExpr::Coalesce(left, right) => {
            let (left, right) = both(left, right)?;
            when(left.clone().is_not_null(), left)
                .otherwise(right)
                .map_err(|error| CellError::Engine(error.to_string()))?
        }
        RowExpr::Conditional(condition, then, otherwise) => {
            let condition = tree_expr(condition, schema)?;
            let (then, otherwise) = both(then, otherwise)?;
            when(condition, then)
                .otherwise(otherwise)
                .map_err(|error| CellError::Engine(error.to_string()))?
        }
    })
}

/// A comparison; `= null` and `!= null` are null tests, as in the exact engine.
fn compare(
    op: RowComparison,
    left: &RowExpr,
    right: &RowExpr,
    schema: &NodeSchema,
) -> Result<Expr, CellError> {
    let null_test = matches!(right, RowExpr::Null) || matches!(left, RowExpr::Null);
    let tested = || {
        tree_expr(
            if matches!(right, RowExpr::Null) {
                left
            } else {
                right
            },
            schema,
        )
    };
    Ok(match (op, null_test) {
        (RowComparison::Equal, true) => tested()?.is_null(),
        (RowComparison::NotEqual, true) => tested()?.is_not_null(),
        (_, _) => {
            let (left, right) = (tree_expr(left, schema)?, tree_expr(right, schema)?);
            match op {
                RowComparison::Equal => left.eq(right),
                RowComparison::NotEqual => left.not_eq(right),
                RowComparison::Less => left.lt(right),
                RowComparison::LessOrEqual => left.lt_eq(right),
                RowComparison::Greater => left.gt(right),
                RowComparison::GreaterOrEqual => left.gt_eq(right),
            }
        }
    })
}

/// Order keys, nulls last in the declared order (so first in its reverse).
fn order(
    schema: &NodeSchema,
    order_by: &[(String, SortDirection, Type)],
    reverse: bool,
) -> Result<Vec<Sort>, CellError> {
    order_by
        .iter()
        .map(|(column_name, direction, _)| {
            let ascending = (*direction == SortDirection::Asc) != reverse;
            Ok(name(&ordered(schema, column_name)?).sort(ascending, reverse))
        })
        .collect()
}

fn carried_exprs(schema: &NodeSchema) -> Result<Vec<Expr>, CellError> {
    Ok(carried(schema)?.iter().map(|column| name(column)).collect())
}

fn lower_node(plan: &TransformPlan, schema: &NodeSchema, step: &Step) -> Result<Spec, CellError> {
    Ok(match step {
        Step::Scan { alias } => Spec::Scan {
            alias: alias.clone(),
        },
        Step::Filter { input, predicate } => Spec::Filter {
            input: index(plan, input)?,
            predicate: expression(predicate, schema_of(plan, input)?)?,
        },
        Step::Project { input, columns } => {
            let source = schema_of(plan, input)?;
            let mut items = Vec::new();
            for (output, projection) in columns {
                match projection {
                    Projection::Column(from) => {
                        items.push(name(from).alias(output));
                        if kind_of(source, from)? == Kind::Timestamp {
                            items.push(name(&key_name(from)).alias(key_name(output)));
                        }
                    }
                    Projection::Expression(typed) => {
                        if kind_of(schema, output)? == Kind::Timestamp {
                            return unsupported();
                        }
                        items.push(expression(typed, source)?.alias(output));
                    }
                }
            }
            items.push(name(ROW));
            Spec::Project {
                input: index(plan, input)?,
                items,
            }
        }
        Step::Join {
            left,
            right,
            on,
            kind: join_kind,
            renames,
        } => lower_join(plan, (left, right), on, *join_kind, renames)?,
        Step::Aggregate {
            input,
            group_by,
            measures,
        } => lower_aggregate(plan, input, group_by, measures)?,
        Step::Window {
            input,
            partition_by,
            order_by,
            columns,
        } => lower_window(plan, input, partition_by, order_by, columns)?,
        Step::Union { inputs, branch } => {
            let mut items = Vec::with_capacity(inputs.len());
            for (at, from) in inputs.iter().enumerate() {
                let mut branch_items = vec![lit(from.clone()).alias(branch)];
                for column in carried(schema)? {
                    if column != *branch {
                        branch_items.push(name(&column));
                    }
                }
                let at = u64::try_from(at).map_err(|_wide| refused(UNSUPPORTED_ENGINE))?;
                branch_items.push(lit(at).alias(BRANCH));
                branch_items.push(name(ROW));
                items.push(branch_items);
            }
            Spec::Union {
                inputs: inputs
                    .iter()
                    .map(|from| index(plan, from))
                    .collect::<Result<_, _>>()?,
                items,
            }
        }
        Step::Dedupe {
            input,
            key,
            order_by,
            keep,
        } => {
            let source = schema_of(plan, input)?;
            Spec::Dedupe {
                input: index(plan, input)?,
                partition: key
                    .iter()
                    .map(|column_name| ordered(source, column_name).map(|n| name(&n)))
                    .collect::<Result<_, _>>()?,
                order: order(source, order_by, *keep == DedupeKeep::Last)?,
                columns: carried(source)?,
            }
        }
    })
}

fn lower_join(
    plan: &TransformPlan,
    (left, right): (&str, &str),
    on: &AdmittedJoin,
    join_kind: JoinKind,
    renames: &[(String, String)],
) -> Result<Spec, CellError> {
    let (left_schema, right_schema) = (schema_of(plan, left)?, schema_of(plan, right)?);
    let mut right_columns = Vec::new();
    for (from, to) in renames {
        right_columns.push((from.clone(), to.clone()));
        if kind_of(right_schema, from)? == Kind::Timestamp {
            right_columns.push((key_name(from), key_name(to)));
        }
    }
    let pairing = match on {
        AdmittedJoin::Link {
            column: source,
            alias,
            link,
            right: right_id,
        } => Pairing::Link {
            alias: alias.clone(),
            link: link.clone(),
            source: source.clone(),
            right_id: right_id.clone(),
        },
        AdmittedJoin::Key { pairs } => {
            if pairs.iter().any(|(_, _, steps)| !steps.is_empty()) {
                return unsupported();
            }
            Pairing::Keys(
                pairs
                    .iter()
                    .map(|(l, r, _)| Ok((ordered(left_schema, l)?, ordered(right_schema, r)?)))
                    .collect::<Result<_, CellError>>()?,
            )
        }
    };
    Ok(Spec::Join {
        left: index(plan, left)?,
        right: index(plan, right)?,
        pairing,
        outer: join_kind == JoinKind::Left,
        left_columns: carried(left_schema)?,
        right_columns,
        right_carried: carried(right_schema)?,
    })
}

type Group = (String, Option<TimeGrain>, Type);
type Measure = (String, AggregateFn, Option<String>, Option<Type>);

fn lower_aggregate(
    plan: &TransformPlan,
    input: &str,
    group_by: &[Group],
    measures: &[Measure],
) -> Result<Spec, CellError> {
    use datafusion::functions_aggregate::expr_fn::{
        array_agg, count, count_distinct, first_value, max, min, sum,
    };
    let source = schema_of(plan, input)?;
    let mut buckets = Vec::new();
    let mut groups = Vec::new();
    for (at, (column_name, grain, ty)) in group_by.iter().enumerate() {
        if let Some(grain) = grain {
            if !matches!(kind_of(source, column_name)?, Kind::Date | Kind::Timestamp) {
                return unsupported();
            }
            let bucket = format!("__g_{at}");
            groups.push(name(&bucket).alias(column_name));
            buckets.push((column_name.clone(), bucket, *grain, ty.clone()));
        } else {
            if kind_of(source, column_name)? == Kind::Timestamp {
                return unsupported();
            }
            groups.push(name(column_name).alias(column_name));
        }
    }
    let mut items = Vec::new();
    for (output, function, read, _) in measures {
        let timestamp = read
            .as_deref()
            .map(|read| kind_of(source, read))
            .transpose()?
            == Some(Kind::Timestamp);
        let target = read.as_deref().map(name);
        let key = read.as_deref().map(|read| name(&key_name(read)));
        match (function, target, key) {
            (AggregateFn::Count, None, _) => items.push(count(lit(1_i64)).alias(output)),
            (AggregateFn::Count, Some(target), _) => items.push(count(target).alias(output)),
            (AggregateFn::CountDistinct, Some(_), Some(key)) if timestamp => {
                items.push(count_distinct(key).alias(output));
            }
            (AggregateFn::CountDistinct, Some(target), _) => {
                items.push(count_distinct(target).alias(output));
            }
            (AggregateFn::Sum, Some(target), _) => items.push(sum(target).alias(output)),
            (AggregateFn::Min | AggregateFn::Max, Some(target), Some(key)) if timestamp => {
                let least = *function == AggregateFn::Min;
                items.push(first_value(target, vec![key.clone().sort(least, false)]).alias(output));
                let extreme = if least { min(key) } else { max(key) };
                items.push(extreme.alias(key_name(output)));
            }
            (AggregateFn::Min, Some(target), _) => items.push(min(target).alias(output)),
            (AggregateFn::Max, Some(target), _) => items.push(max(target).alias(output)),
            (
                AggregateFn::CountDistinct | AggregateFn::Sum | AggregateFn::Min | AggregateFn::Max,
                None,
                _,
            ) => {
                return unsupported();
            }
        }
    }
    items.push(array_agg(name(ROW)).alias(MEMBERS));
    Ok(Spec::Aggregate {
        input: index(plan, input)?,
        buckets,
        groups,
        measures: items,
    })
}

type WindowColumn = (String, WindowFn, Option<String>, u32);

fn lower_window(
    plan: &TransformPlan,
    input: &str,
    partition_by: &[String],
    order_by: &[(String, SortDirection, Type)],
    columns: &[WindowColumn],
) -> Result<Spec, CellError> {
    use datafusion::functions_aggregate::sum::sum_udaf;
    use datafusion::functions_window::expr_fn::{dense_rank, lag, lead, rank, row_number};
    use datafusion::logical_expr::expr::WindowFunction;
    let source = schema_of(plan, input)?;
    let partition: Vec<Expr> = partition_by
        .iter()
        .map(|column_name| ordered(source, column_name).map(|n| name(&n)))
        .collect::<Result<_, _>>()?;
    let sorts = order(source, order_by, false)?;
    let windowed = |function: Expr| -> Result<Expr, CellError> {
        function
            .partition_by(partition.clone())
            .order_by(sorts.clone())
            .build()
            .map_err(|error| CellError::Engine(error.to_string()))
    };
    let mut items = carried_exprs(source)?;
    items.push(name(ROW));
    for (output, function, read, offset) in columns {
        let target = read.as_deref().map(name);
        let offset = i64::from(*offset);
        let item = match (function, target) {
            (WindowFn::RowNumber, _) => windowed(row_number())?,
            (WindowFn::Rank, _) => windowed(rank())?,
            (WindowFn::DenseRank, _) => windowed(dense_rank())?,
            (WindowFn::Lag | WindowFn::Lead, Some(target)) => {
                let shift = |value: Expr| {
                    if *function == WindowFn::Lag {
                        lag(value, Some(offset), None)
                    } else {
                        lead(value, Some(offset), None)
                    }
                };
                if read
                    .as_deref()
                    .map(|read| kind_of(source, read))
                    .transpose()?
                    == Some(Kind::Timestamp)
                {
                    let key = name(&key_name(read.as_deref().unwrap_or_default()));
                    items.push(windowed(shift(key))?.alias(key_name(output)));
                }
                windowed(shift(target))?
            }
            // The sum as a window function over its frame, not an aggregate.
            (WindowFn::RunningSum, Some(target)) => {
                Expr::WindowFunction(Box::new(WindowFunction::new(sum_udaf(), vec![target])))
                    .partition_by(partition.clone())
                    .order_by(sorts.clone())
                    .window_frame(WindowFrame::new_bounds(
                        WindowFrameUnits::Rows,
                        WindowFrameBound::Preceding(ScalarValue::UInt64(None)),
                        WindowFrameBound::CurrentRow,
                    ))
                    .build()
                    .map_err(|error| CellError::Engine(error.to_string()))?
            }
            (WindowFn::Lag | WindowFn::Lead | WindowFn::RunningSum, None) => return unsupported(),
        };
        items.push(item.alias(output));
    }
    Ok(Spec::Window {
        input: index(plan, input)?,
        items,
    })
}
