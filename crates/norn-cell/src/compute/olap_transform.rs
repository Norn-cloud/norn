//! VIEW-ENGINE-1: an admitted transform lowered to embedded `DuckDB` over the
//! same authorized snapshot the exact interpreter reads.
//!
//! Each input alias becomes one typed table of exactly the rows the reader
//! may read; each link the plan joins along becomes one edge table. Every
//! node lowers to one CTE carrying its columns, a sortable key column per
//! timestamp and its row lineage (`__m`, the member list). Expressions lower
//! through `norn_expr::row_expression`; anything outside that subset, a
//! normalized key join, a computed timestamp, an unsupported type or a
//! decimal scale above 18 refuses `core.compute.unsupported_engine` rather
//! than run with other semantics. Values are bound or quoted, extensions and
//! external access are off, and execution is single-threaded.

use std::collections::{BTreeMap, BTreeSet};

use duckdb::{Connection, params_from_iter};
use norn_checker::transform::eval::{EvalError, MemberRef, TransformInput, TransformOutput};
use norn_checker::transform::{
    AdmittedJoin, Column, NodeSchema, Projection, RowArithmetic, RowComparison, RowExpr, Step,
    TransformPlan, Type, TypedExpression, eval,
};
use norn_ir::metric::TimeGrain;
use norn_ir::{AggregateFn, Decimal, DedupeKeep, JoinKind, RuntimeValue, SortDirection, WindowFn};

use super::{CellError, CellStore, Engine, SqlEngine, UNSUPPORTED_ENGINE, refused};
use crate::ObjectRecord;
use crate::compute::transform::{RowAccess, TransformResult, eval_refusal};

const MEMBER_FIELD: char = '\u{1f}';
const MEMBER_SEPARATOR: char = '\u{1e}';

fn db<T>(value: duckdb::Result<T>) -> Result<T, CellError> {
    value.map_err(|error| CellError::Engine(error.to_string()))
}

fn unsupported<T>() -> Result<T, CellError> {
    Err(refused(UNSUPPORTED_ENGINE))
}

impl<E: SqlEngine> CellStore<E> {
    /// VIEW-ENGINE-1: evaluate an admitted transform in embedded `DuckDB` over
    /// the authorized snapshot [`Self::evaluate_transform`] reads; the
    /// envelope differs only in its engine label.
    ///
    /// # Errors
    /// As [`Self::evaluate_transform`], and `core.compute.unsupported_engine`
    /// for a plan outside the lowered subset.
    pub fn evaluate_transform_duckdb(
        &self,
        plan: &TransformPlan,
        world: &str,
        scope: &str,
        access: impl FnMut(&ObjectRecord) -> Result<RowAccess, CellError>,
    ) -> Result<TransformResult, CellError> {
        // Lower first: an unsupported plan refuses before any read.
        let lowered = lower(plan)?;
        self.evaluate_transform_with(
            plan,
            (world, scope),
            access,
            Engine::Duckdb,
            |plan, input| execute(plan, input, &lowered),
        )
    }
}

/// How a column is held in `DuckDB`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Text,
    Bool,
    Integer,
    Decimal(u64),
    Number,
    Date,
    Timestamp,
}

fn strip(ty: &Type) -> &Type {
    if let Type::Optional(inner) = ty {
        strip(inner)
    } else {
        ty
    }
}

fn kind(ty: &Type) -> Result<Kind, CellError> {
    let ty = strip(ty);
    if matches!(ty, Type::Text { .. } | Type::Enum { .. }) {
        Ok(Kind::Text)
    } else if matches!(ty, Type::Bool) {
        Ok(Kind::Bool)
    } else if matches!(ty, Type::Integer) {
        Ok(Kind::Integer)
    } else if matches!(ty, Type::Number) {
        Ok(Kind::Number)
    } else if matches!(ty, Type::Date) {
        Ok(Kind::Date)
    } else if matches!(ty, Type::Timestamp { .. }) {
        Ok(Kind::Timestamp)
    } else if let Type::Decimal { scale, .. } = ty {
        if *scale <= 18 {
            Ok(Kind::Decimal(*scale))
        } else {
            unsupported()
        }
    } else {
        unsupported()
    }
}

fn quote(name: &str) -> String {
    format!("\"{}\"", name.replace('"', "\"\""))
}

fn literal(text: &str) -> String {
    format!("'{}'", text.replace('\'', "''"))
}

/// The sortable key column of a timestamp column.
fn key_column(name: &str) -> String {
    quote(&format!("__k_{name}"))
}

fn node(id: &str) -> String {
    quote(&format!("n_{id}"))
}

fn table(alias: &str) -> String {
    quote(&format!("t_{alias}"))
}

fn edges(alias: &str, link: &str) -> String {
    quote(&format!("l_{alias}_{link}"))
}

fn column<'s>(schema: &'s NodeSchema, name: &str) -> Result<&'s Column, CellError> {
    schema
        .column(name)
        .ok_or_else(|| refused(UNSUPPORTED_ENGINE))
}

/// A column reference usable for ordering, grouping and equality.
fn ordered(schema: &NodeSchema, name: &str) -> Result<String, CellError> {
    Ok(if kind(&column(schema, name)?.ty)? == Kind::Timestamp {
        key_column(name)
    } else {
        quote(name)
    })
}

/// Every column of `schema` as select items, plus timestamp keys.
fn carried(schema: &NodeSchema, from: &str) -> Result<Vec<String>, CellError> {
    let mut items = Vec::new();
    for column in &schema.columns {
        items.push(format!("{from}.{}", quote(&column.name)));
        if kind(&column.ty)? == Kind::Timestamp {
            items.push(format!("{from}.{}", key_column(&column.name)));
        }
    }
    Ok(items)
}

fn expression(typed: &TypedExpression, schema: &NodeSchema) -> Result<String, CellError> {
    let tree = typed
        .row_tree()
        .ok_or_else(|| refused(UNSUPPORTED_ENGINE))?;
    sql(&tree, schema)
}

fn sql(tree: &RowExpr, schema: &NodeSchema) -> Result<String, CellError> {
    let binary = |op: &str, left: &RowExpr, right: &RowExpr| -> Result<String, CellError> {
        Ok(format!(
            "({} {op} {})",
            sql(left, schema)?,
            sql(right, schema)?
        ))
    };
    Ok(match tree {
        RowExpr::Null => "NULL".to_owned(),
        RowExpr::Bool(flag) => if *flag { "TRUE" } else { "FALSE" }.to_owned(),
        RowExpr::Number(number) => {
            let text = number.as_str();
            let scale = text
                .split_once('.')
                .map_or(0, |(_, fraction)| fraction.len());
            if scale > 18 {
                return unsupported();
            }
            format!("CAST({} AS DECIMAL(38,{scale}))", literal(text))
        }
        RowExpr::Text(text) => literal(text),
        RowExpr::Column(name) => {
            let found = column(schema, name)?;
            // Temporal values compare as instants, not strings.
            if kind(&found.ty)? == Kind::Timestamp {
                return unsupported();
            }
            quote(name)
        }
        RowExpr::Not(inner) => format!("(NOT {})", sql(inner, schema)?),
        RowExpr::Negate(inner) => format!("(- {})", sql(inner, schema)?),
        RowExpr::Arithmetic(op, left, right) => binary(
            match op {
                RowArithmetic::Add => "+",
                RowArithmetic::Subtract => "-",
                RowArithmetic::Multiply => "*",
            },
            left,
            right,
        )?,
        RowExpr::Compare(op, left, right) => {
            let null_test = matches!(**right, RowExpr::Null) || matches!(**left, RowExpr::Null);
            match (op, null_test) {
                (RowComparison::Equal, true) => {
                    let value = if matches!(**right, RowExpr::Null) {
                        left
                    } else {
                        right
                    };
                    format!("({} IS NULL)", sql(value, schema)?)
                }
                (RowComparison::NotEqual, true) => {
                    let value = if matches!(**right, RowExpr::Null) {
                        left
                    } else {
                        right
                    };
                    format!("({} IS NOT NULL)", sql(value, schema)?)
                }
                (RowComparison::Equal, false) => binary("=", left, right)?,
                (RowComparison::NotEqual, false) => binary("<>", left, right)?,
                (RowComparison::Less, _) => binary("<", left, right)?,
                (RowComparison::LessOrEqual, _) => binary("<=", left, right)?,
                (RowComparison::Greater, _) => binary(">", left, right)?,
                (RowComparison::GreaterOrEqual, _) => binary(">=", left, right)?,
            }
        }
        RowExpr::And(left, right) => binary("AND", left, right)?,
        RowExpr::Or(left, right) => binary("OR", left, right)?,
        RowExpr::Coalesce(left, right) => {
            format!("COALESCE({}, {})", sql(left, schema)?, sql(right, schema)?)
        }
        RowExpr::Conditional(condition, then, otherwise) => format!(
            "(CASE WHEN {} THEN {} ELSE {} END)",
            sql(condition, schema)?,
            sql(then, schema)?,
            sql(otherwise, schema)?
        ),
    })
}

fn order_items(
    schema: &NodeSchema,
    order_by: &[(String, SortDirection, Type)],
    reverse: bool,
) -> Result<Vec<String>, CellError> {
    order_by
        .iter()
        .map(|(name, direction, _)| {
            let ascending = (*direction == SortDirection::Asc) != reverse;
            // Nulls sort last in the declared order, so first in its reverse.
            let nulls = if reverse { "FIRST" } else { "LAST" };
            Ok(format!(
                "{} {} NULLS {nulls}",
                ordered(schema, name)?,
                if ascending { "ASC" } else { "DESC" }
            ))
        })
        .collect()
}

fn schema_of<'p>(plan: &'p TransformPlan, id: &str) -> Result<&'p NodeSchema, CellError> {
    plan.nodes
        .get(id)
        .map(|node| &node.schema)
        .ok_or_else(|| refused(UNSUPPORTED_ENGINE))
}

fn grain_expression(
    name: &str,
    grain: TimeGrain,
    schema: &NodeSchema,
) -> Result<String, CellError> {
    let unit = match grain {
        TimeGrain::Day => "day",
        TimeGrain::Week => "week",
        TimeGrain::Month => "month",
        TimeGrain::Quarter => "quarter",
        TimeGrain::Year => "year",
    };
    let instant = match kind(&column(schema, name)?.ty)? {
        Kind::Date => format!("CAST({} AS DATE)", quote(name)),
        Kind::Timestamp => format!(
            "make_timestamp(CAST({} // 1000 AS BIGINT))",
            key_column(name)
        ),
        Kind::Text | Kind::Bool | Kind::Integer | Kind::Decimal(_) | Kind::Number => {
            return unsupported();
        }
    };
    Ok(format!(
        "strftime(date_trunc('{unit}', {instant}), '%Y-%m-%d')"
    ))
}

#[allow(
    clippy::too_many_lines,
    reason = "one lowering per closed operator, read top to bottom"
)]
fn lower_node(plan: &TransformPlan, id: &str, step: &Step) -> Result<String, CellError> {
    let schema = schema_of(plan, id)?;
    Ok(match step {
        Step::Scan { alias } => {
            let mut items = carried(schema, "s")?;
            items.push("[s.\"__m0\"] AS \"__m\"".to_owned());
            format!("SELECT {} FROM {} s", items.join(", "), table(alias))
        }
        Step::Filter { input, predicate } => format!(
            "SELECT * FROM {} WHERE {}",
            node(input),
            expression(predicate, schema_of(plan, input)?)?
        ),
        Step::Project { input, columns } => {
            let source = schema_of(plan, input)?;
            let mut items = Vec::new();
            for (name, projection) in columns {
                match projection {
                    Projection::Column(from) => {
                        items.push(format!("p.{} AS {}", quote(from), quote(name)));
                        if kind(&column(source, from)?.ty)? == Kind::Timestamp {
                            items.push(format!("p.{} AS {}", key_column(from), key_column(name)));
                        }
                    }
                    Projection::Expression(typed) => {
                        if kind(&column(schema, name)?.ty)? == Kind::Timestamp {
                            return unsupported();
                        }
                        items.push(format!("{} AS {}", expression(typed, source)?, quote(name)));
                    }
                }
            }
            items.push("p.\"__m\"".to_owned());
            format!("SELECT {} FROM {} p", items.join(", "), node(input))
        }
        Step::Join {
            left,
            right,
            on,
            kind: join_kind,
            renames,
        } => {
            let left_schema = schema_of(plan, left)?;
            let right_schema = schema_of(plan, right)?;
            let mut items = carried(left_schema, "a")?;
            for (from, to) in renames {
                items.push(format!("b.{} AS {}", quote(from), quote(to)));
                if kind(&column(right_schema, from)?.ty)? == Kind::Timestamp {
                    items.push(format!("b.{} AS {}", key_column(from), key_column(to)));
                }
            }
            items.push("list_concat(a.\"__m\", COALESCE(b.\"__m\", [])) AS \"__m\"".to_owned());
            let join = match join_kind {
                JoinKind::Inner => "INNER JOIN",
                JoinKind::Left => "LEFT JOIN",
            };
            let (right_source, condition) = match on {
                AdmittedJoin::Link {
                    column: source,
                    alias,
                    link,
                    right: right_id,
                } => (
                    format!(
                        "(SELECT e.\"src\" AS \"__src\", r.* FROM {} e INNER JOIN {} r ON r.{} = e.\"dst\")",
                        edges(alias, link),
                        node(right),
                        quote(right_id)
                    ),
                    format!("b.\"__src\" = a.{}", quote(source)),
                ),
                AdmittedJoin::Key { pairs } => {
                    if pairs.iter().any(|(_, _, steps)| !steps.is_empty()) {
                        return unsupported();
                    }
                    let mut conditions = Vec::new();
                    for (left_column, right_column, _) in pairs {
                        conditions.push(format!(
                            "a.{} = b.{}",
                            ordered(left_schema, left_column)?,
                            ordered(right_schema, right_column)?
                        ));
                    }
                    (node(right), conditions.join(" AND "))
                }
            };
            format!(
                "SELECT {} FROM {} a {join} {right_source} b ON {condition}",
                items.join(", "),
                node(left)
            )
        }
        Step::Aggregate {
            input,
            group_by,
            measures,
        } => {
            let source = schema_of(plan, input)?;
            let mut items = Vec::new();
            let mut groups = Vec::new();
            for (name, grain, _) in group_by {
                let expression = if let Some(grain) = grain {
                    grain_expression(name, *grain, source)?
                } else {
                    if kind(&column(source, name)?.ty)? == Kind::Timestamp {
                        return unsupported();
                    }
                    quote(name)
                };
                items.push(format!("{expression} AS {}", quote(name)));
                groups.push(expression);
            }
            for (name, function, read, _) in measures {
                let read_kind = read
                    .as_ref()
                    .map(|read| column(source, read).and_then(|c| kind(&c.ty)))
                    .transpose()?;
                let target = read.as_deref().map(quote).unwrap_or_default();
                let key = read.as_deref().map(key_column).unwrap_or_default();
                let timestamp = read_kind == Some(Kind::Timestamp);
                match (function, timestamp) {
                    (AggregateFn::Count, _) if read.is_none() => {
                        items.push(format!("count(*) AS {}", quote(name)));
                    }
                    (AggregateFn::Count, _) => {
                        items.push(format!("count({target}) AS {}", quote(name)));
                    }
                    (AggregateFn::CountDistinct, true) => {
                        items.push(format!("count(DISTINCT {key}) AS {}", quote(name)));
                    }
                    (AggregateFn::CountDistinct, false) => {
                        items.push(format!("count(DISTINCT {target}) AS {}", quote(name)));
                    }
                    (AggregateFn::Sum, _) => {
                        items.push(format!("sum({target}) AS {}", quote(name)));
                    }
                    (AggregateFn::Min | AggregateFn::Max, true) => {
                        let (pick, extreme) = if *function == AggregateFn::Min {
                            ("arg_min", "min")
                        } else {
                            ("arg_max", "max")
                        };
                        items.push(format!("{pick}({target}, {key}) AS {}", quote(name)));
                        items.push(format!("{extreme}({key}) AS {}", key_column(name)));
                    }
                    (AggregateFn::Min, false) => {
                        items.push(format!("min({target}) AS {}", quote(name)));
                    }
                    (AggregateFn::Max, false) => {
                        items.push(format!("max({target}) AS {}", quote(name)));
                    }
                }
            }
            items.push(
                "COALESCE(list_sort(list_distinct(flatten(list(g.\"__m\")))), []) AS \"__m\""
                    .to_owned(),
            );
            let grouped = if groups.is_empty() {
                String::new()
            } else {
                format!(" GROUP BY {}", groups.join(", "))
            };
            format!(
                "SELECT {} FROM {} g{grouped}",
                items.join(", "),
                node(input)
            )
        }
        Step::Window {
            input,
            partition_by,
            order_by,
            columns,
        } => {
            let source = schema_of(plan, input)?;
            let partition = partition_by
                .iter()
                .map(|name| ordered(source, name))
                .collect::<Result<Vec<_>, _>>()?;
            let order = order_items(source, order_by, false)?;
            let partition = if partition.is_empty() {
                String::new()
            } else {
                format!("PARTITION BY {}", partition.join(", "))
            };
            let order = if order.is_empty() {
                String::new()
            } else {
                format!(" ORDER BY {}", order.join(", "))
            };
            let window = format!("{partition}{order}");
            let mut items = vec!["w.*".to_owned()];
            for (name, function, read, offset) in columns {
                let target = read.as_deref().map(quote).unwrap_or_default();
                let read_kind = read
                    .as_ref()
                    .map(|read| column(source, read).and_then(|c| kind(&c.ty)))
                    .transpose()?;
                let item = match function {
                    WindowFn::RowNumber => format!("row_number() OVER ({window})"),
                    WindowFn::Rank => format!("rank() OVER ({window})"),
                    WindowFn::DenseRank => format!("dense_rank() OVER ({window})"),
                    WindowFn::Lag | WindowFn::Lead => {
                        let shift = if *function == WindowFn::Lag {
                            "lag"
                        } else {
                            "lead"
                        };
                        if read_kind == Some(Kind::Timestamp) {
                            let key = read.as_deref().map(key_column).unwrap_or_default();
                            items.push(format!(
                                "{shift}({key}, {offset}) OVER ({window}) AS {}",
                                key_column(name)
                            ));
                        }
                        format!("{shift}({target}, {offset}) OVER ({window})")
                    }
                    WindowFn::RunningSum => format!(
                        "sum({target}) OVER ({window} ROWS BETWEEN UNBOUNDED PRECEDING AND CURRENT ROW)"
                    ),
                };
                items.push(format!("{item} AS {}", quote(name)));
            }
            format!("SELECT {} FROM {} w", items.join(", "), node(input))
        }
        Step::Union { inputs, branch } => {
            let parts = inputs
                .iter()
                .map(|from| {
                    let mut items = vec![format!("{} AS {}", literal(from), quote(branch))];
                    items.extend(carried(schema_of(plan, from)?, "u")?);
                    items.push("u.\"__m\"".to_owned());
                    Ok(format!("SELECT {} FROM {} u", items.join(", "), node(from)))
                })
                .collect::<Result<Vec<_>, CellError>>()?;
            parts.join(" UNION ALL ")
        }
        Step::Dedupe {
            input,
            key,
            order_by,
            keep,
        } => {
            let source = schema_of(plan, input)?;
            let partition = key
                .iter()
                .map(|name| ordered(source, name))
                .collect::<Result<Vec<_>, _>>()?;
            let order = order_items(source, order_by, *keep == DedupeKeep::Last)?;
            let ordering = if order.is_empty() {
                String::new()
            } else {
                format!(" ORDER BY {}", order.join(", "))
            };
            format!(
                "SELECT * EXCLUDE (\"__rn\") FROM (SELECT d.*, row_number() OVER (PARTITION BY {}{ordering}) AS \"__rn\" FROM {} d) WHERE \"__rn\" = 1",
                partition.join(", "),
                node(input)
            )
        }
    })
}

/// The whole plan as one query over the loaded tables.
/// A lowered plan: the output query, and the rows each node produces (by
/// its index in the plan's order), over one WITH clause of node CTEs.
struct Lowered {
    output: String,
    counts: String,
}

fn lower(plan: &TransformPlan) -> Result<Lowered, CellError> {
    for (alias, link) in &plan.links {
        if !valid_identifier(alias) || !valid_identifier(link) {
            return unsupported();
        }
    }
    let mut ctes = Vec::new();
    for id in &plan.order {
        let step = &plan
            .nodes
            .get(id)
            .ok_or_else(|| refused(UNSUPPORTED_ENGINE))?
            .step;
        ctes.push(format!("{} AS ({})", node(id), lower_node(plan, id, step)?));
    }
    let schema = plan.output_schema();
    let mut items = Vec::new();
    for column in &schema.columns {
        kind(&column.ty)?;
        items.push(format!("CAST({} AS VARCHAR)", quote(&column.name)));
    }
    items.push(format!("array_to_string(\"__m\", '{MEMBER_SEPARATOR}')"));
    let with = ctes.join(", ");
    let counts = plan
        .order
        .iter()
        .enumerate()
        .map(|(index, id)| format!("SELECT {index}, COUNT(*) FROM {}", node(id)))
        .collect::<Vec<_>>()
        .join(" UNION ALL ");
    Ok(Lowered {
        output: format!(
            "WITH {with} SELECT {} FROM {}",
            items.join(", "),
            node(&plan.output)
        ),
        counts: format!("WITH {with} {counts}"),
    })
}

fn valid_identifier(name: &str) -> bool {
    !name.is_empty()
        && name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '.')
}

fn duck_type(kind: Kind) -> String {
    match kind {
        Kind::Text | Kind::Date | Kind::Timestamp => "VARCHAR".to_owned(),
        Kind::Bool => "BOOLEAN".to_owned(),
        Kind::Integer => "BIGINT".to_owned(),
        Kind::Decimal(scale) => format!("DECIMAL(38,{scale})"),
        Kind::Number => "DECIMAL(38,18)".to_owned(),
    }
}

/// One stored value as bound text, or `None` for null; refuses a non-scalar.
fn bound(value: Option<&RuntimeValue>) -> Result<Option<String>, CellError> {
    Ok(match value {
        None | Some(RuntimeValue::Null) => None,
        Some(RuntimeValue::Bool(flag)) => Some(flag.to_string()),
        Some(RuntimeValue::Number(number)) => Some(number.canonical()),
        Some(RuntimeValue::String(text)) => Some(text.clone()),
        Some(RuntimeValue::Array(_) | RuntimeValue::Object(_)) => return unsupported(),
    })
}

fn timestamp_key(text: Option<&str>) -> Result<Option<String>, CellError> {
    text.map(|text| {
        norn_checker::transform::instant(text)
            .ok_or_else(|| refused("core.compute.stale_input"))
            .and_then(|(seconds, nanos)| {
                i128::from(seconds)
                    .checked_mul(1_000_000_000)
                    .and_then(|whole| whole.checked_add(i128::from(nanos)))
                    .map(|key| key.to_string())
                    .ok_or_else(|| refused(UNSUPPORTED_ENGINE))
            })
    })
    .transpose()
}

fn member(alias: &str, id: &str, version: u64) -> Result<String, CellError> {
    if [alias, id]
        .iter()
        .any(|part| part.contains(MEMBER_FIELD) || part.contains(MEMBER_SEPARATOR))
    {
        return unsupported();
    }
    Ok(format!("{alias}{MEMBER_FIELD}{id}{MEMBER_FIELD}{version}"))
}

fn load(conn: &Connection, plan: &TransformPlan, input: &TransformInput) -> Result<(), CellError> {
    for scan in plan.inputs.values() {
        let Some(rows) = input.rows.get(&scan.alias) else {
            continue;
        };
        let schema = plan
            .nodes
            .values()
            .find(|node| matches!(&node.step, Step::Scan { alias } if alias == &scan.alias))
            .map(|node| &node.schema)
            .ok_or_else(|| refused(UNSUPPORTED_ENGINE))?;
        let mut columns = Vec::new();
        let mut placeholders = Vec::new();
        for column in &schema.columns {
            let kind = kind(&column.ty)?;
            columns.push(format!("{} {}", quote(&column.name), duck_type(kind)));
            placeholders.push(format!("CAST(? AS {})", duck_type(kind)));
            if kind == Kind::Timestamp {
                columns.push(format!("{} HUGEINT", key_column(&column.name)));
                placeholders.push("CAST(? AS HUGEINT)".to_owned());
            }
        }
        columns.push("\"__m0\" VARCHAR".to_owned());
        placeholders.push("?".to_owned());
        db(conn.execute_batch(&format!(
            "CREATE TABLE {} ({})",
            table(&scan.alias),
            columns.join(", ")
        )))?;
        let mut insert = db(conn.prepare(&format!(
            "INSERT INTO {} VALUES ({})",
            table(&scan.alias),
            placeholders.join(", ")
        )))?;
        for row in rows {
            let mut values: Vec<Option<String>> = Vec::new();
            for column in &schema.columns {
                let value = if column.name == "id" {
                    Some(row.id.clone())
                } else {
                    bound(row.values.get(&column.name))?
                };
                let timestamp = kind(&column.ty)? == Kind::Timestamp;
                let key = if timestamp {
                    Some(timestamp_key(value.as_deref())?)
                } else {
                    None
                };
                values.push(value);
                if let Some(key) = key {
                    values.push(key);
                }
            }
            values.push(Some(member(&scan.alias, &row.id, row.version)?));
            db(insert.execute(params_from_iter(values.iter())))?;
        }
    }
    for (alias, link) in &plan.links {
        db(conn.execute_batch(&format!(
            "CREATE TABLE {} (\"src\" VARCHAR, \"dst\" VARCHAR)",
            edges(alias, link)
        )))?;
        let mut insert =
            db(conn.prepare(&format!("INSERT INTO {} VALUES (?, ?)", edges(alias, link))))?;
        let pairs: BTreeSet<(&String, &String)> = input
            .links
            .get(&(alias.clone(), link.clone()))
            .into_iter()
            .flat_map(|by_source| {
                by_source.iter().flat_map(|(source, targets)| {
                    targets.iter().map(move |target| (source, target))
                })
            })
            .collect();
        for (source, target) in pairs {
            db(insert.execute(duckdb::params![source, target]))?;
        }
    }
    Ok(())
}

fn value(text: Option<String>, kind: Kind) -> Result<RuntimeValue, CellError> {
    let Some(text) = text else {
        return Ok(RuntimeValue::Null);
    };
    Ok(match kind {
        Kind::Text | Kind::Date | Kind::Timestamp => RuntimeValue::String(text),
        Kind::Bool => RuntimeValue::Bool(text == "true"),
        Kind::Integer | Kind::Decimal(_) | Kind::Number => {
            RuntimeValue::Number(Decimal::parse(&text).map_err(CellError::Value)?)
        }
    })
}

fn members(text: &str) -> Result<BTreeSet<MemberRef>, CellError> {
    text.split(MEMBER_SEPARATOR)
        .filter(|part| !part.is_empty())
        .map(|part| {
            let mut fields = part.split(MEMBER_FIELD);
            match (fields.next(), fields.next(), fields.next(), fields.next()) {
                (Some(alias), Some(id), Some(version), None) => Ok(MemberRef {
                    alias: alias.to_owned(),
                    id: id.to_owned(),
                    version: version
                        .parse()
                        .map_err(|_error| refused(UNSUPPORTED_ENGINE))?,
                }),
                _ => unsupported(),
            }
        })
        .collect()
}

fn execute(
    plan: &TransformPlan,
    input: &TransformInput,
    lowered: &Lowered,
) -> Result<TransformOutput, CellError> {
    let conn = db(Connection::open_in_memory())?;
    db(conn.execute_batch(
        "SET autoinstall_known_extensions=false; SET autoload_known_extensions=false; SET enable_external_access=false; SET threads=1;",
    ))?;
    load(&conn, plan, input)?;
    // Every node's volume first: a node past the bound refuses the whole
    // result, as the exact engine's does.
    let bound = u64::try_from(norn_checker::transform::MAX_ROWS).unwrap_or(u64::MAX);
    let mut node_rows = BTreeMap::new();
    let mut counts = db(conn.prepare(&lowered.counts))?;
    let found = db(counts.query_map([], |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?))))?;
    for item in found {
        let (index, count) = db(item)?;
        let id = usize::try_from(index)
            .ok()
            .and_then(|index| plan.order.get(index))
            .ok_or_else(|| refused(UNSUPPORTED_ENGINE))?;
        let count = u64::try_from(count).map_err(|_negative| refused(UNSUPPORTED_ENGINE))?;
        if count > bound {
            return Err(refused("core.compute.bound_exceeded"));
        }
        node_rows.insert(id.clone(), count);
    }
    let schema = plan.output_schema();
    let kinds = schema
        .columns
        .iter()
        .map(|column| kind(&column.ty))
        .collect::<Result<Vec<_>, _>>()?;
    let mut statement = db(conn.prepare(&lowered.output))?;
    let width = kinds.len();
    let rows = db(statement.query_map([], |row| {
        let mut texts = Vec::with_capacity(width);
        for index in 0..width {
            texts.push(row.get::<_, Option<String>>(index)?);
        }
        let lineage = row.get::<_, Option<String>>(width)?;
        Ok((texts, lineage))
    }))?;
    let mut out = Vec::new();
    for row in rows {
        let (texts, lineage) = db(row)?;
        if out.len() >= norn_checker::transform::MAX_ROWS {
            return Err(refused("core.compute.bound_exceeded"));
        }
        let mut values = BTreeMap::new();
        for ((column, kind), text) in schema.columns.iter().zip(&kinds).zip(texts) {
            values.insert(column.name.clone(), value(text, *kind)?);
        }
        out.push((values, members(lineage.as_deref().unwrap_or_default())?));
    }
    eval::order_output(plan, out, node_rows)
        .map_err(|error: EvalError| refused(eval_refusal(&error)))
}
