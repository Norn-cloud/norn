//! VIEW-ENGINE-1: the exact reference interpreter of an admitted [`TransformPlan`].
//!
//! Pure and deterministic: the host supplies one complete, authorized
//! snapshot (VIEW-ROWS-1: the reader's readable rows of every input, plus the
//! link edges the plan reads) and receives every output row with its row
//! lineage (VIEW-LINEAGE-1). Nulls sort last in both directions; a sum skips
//! nulls and is null when nothing was summed; an aggregate without group keys
//! returns exactly one row. Every bound, overflow or expression error refuses
//! the whole result: nothing partial is returned.

use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet};

use norn_expr::{EvaluationBudget, Type, Value};
use norn_ir::{
    AggregateFn, Decimal, DedupeKeep, JoinKind, RuntimeValue, SortDirection, TimeGrain, WindowFn,
};

use super::{AdmittedJoin, MAX_ROWS, Projection, ROW_ROOT, Step, TransformPlan, TypedExpression};

/// One readable input row, as the cell stores it.
#[derive(Debug, Clone, PartialEq)]
pub struct InputRow {
    /// The object id.
    pub id: String,
    /// Its current version.
    pub version: u64,
    /// Its stored property values.
    pub values: BTreeMap<String, RuntimeValue>,
}

/// One complete authorized snapshot of a plan's inputs.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct TransformInput {
    /// Readable rows by input alias; every scanned alias must be present.
    pub rows: BTreeMap<String, Vec<InputRow>>,
    /// Link edges by `(alias, link)`: source id → target ids.
    pub links: BTreeMap<(String, String), BTreeMap<String, Vec<String>>>,
}

/// One input row an output row derives from (VIEW-LINEAGE-1).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct MemberRef {
    /// The input alias.
    pub alias: String,
    /// The object id.
    pub id: String,
    /// The object version read.
    pub version: u64,
}

/// One output row and its lineage.
#[derive(Debug, Clone, PartialEq)]
pub struct OutputRow {
    /// The grain values, in grain order.
    pub key: Vec<RuntimeValue>,
    /// Every column value.
    pub values: BTreeMap<String, RuntimeValue>,
    /// The input rows it derives from.
    pub members: BTreeSet<MemberRef>,
}

/// The rows of a plan's output node, ordered by grain (nulls last).
#[derive(Debug, Clone, PartialEq)]
pub struct TransformOutput {
    /// Output column names, in schema order.
    pub columns: Vec<String>,
    /// The grain columns.
    pub grain: Vec<String>,
    /// The rows.
    pub rows: Vec<OutputRow>,
}

/// Why an evaluation refused. Nothing partial is ever returned.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EvalError {
    /// A node would hold more than [`MAX_ROWS`] rows, or a sum overflowed.
    BoundExceeded,
    /// The snapshot does not match the plan (missing alias, duplicate id,
    /// a value outside its column type).
    Input(String),
    /// An admitted expression failed at runtime.
    Expression(String),
}

#[derive(Debug, Clone)]
struct Row {
    values: BTreeMap<String, Value>,
    members: BTreeSet<MemberRef>,
}

/// A total, typed order over one column's values; `Null` sorts last.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
enum Key {
    Bool(bool),
    Number(Decimal),
    Instant(i64, u32),
    Text(String),
    Other(String),
    Null,
}

fn key(value: &Value, ty: &Type) -> Result<Key, EvalError> {
    Ok(match value {
        Value::Null => Key::Null,
        Value::Bool(flag) => Key::Bool(*flag),
        Value::Number(number) => Key::Number(
            Decimal::parse(number.as_str()).map_err(|error| EvalError::Input(error.to_string()))?,
        ),
        Value::String(text) => match strip(ty) {
            Type::Timestamp { .. } => {
                let (seconds, nanos) = norn_expr::temporal::timestamp_ordering_key(text)
                    .map_err(|error| EvalError::Input(error.to_string()))?;
                Key::Instant(seconds, nanos)
            }
            _ => Key::Text(text.clone()),
        },
        Value::Array(_) | Value::Object(_) => Key::Other(
            norn_canonical::canonical_json_of(&to_runtime(value)?)
                .map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
                .map_err(|error| EvalError::Input(error.to_string()))?,
        ),
    })
}

fn strip(ty: &Type) -> &Type {
    match ty {
        Type::Optional(inner) => strip(inner),
        other => other,
    }
}

fn to_runtime(value: &Value) -> Result<RuntimeValue, EvalError> {
    Ok(match value {
        Value::Null => RuntimeValue::Null,
        Value::Bool(flag) => RuntimeValue::Bool(*flag),
        Value::Number(number) => RuntimeValue::Number(
            Decimal::parse(number.as_str()).map_err(|error| EvalError::Input(error.to_string()))?,
        ),
        Value::String(text) => RuntimeValue::String(text.clone()),
        Value::Array(items) => {
            RuntimeValue::Array(items.iter().map(to_runtime).collect::<Result<_, _>>()?)
        }
        Value::Object(entries) => RuntimeValue::Object(
            entries
                .iter()
                .map(|(name, item)| Ok((name.clone(), to_runtime(item)?)))
                .collect::<Result<_, EvalError>>()?,
        ),
    })
}

fn number(value: Decimal) -> Result<Value, EvalError> {
    value
        .canonical()
        .parse()
        .map(Value::Number)
        .map_err(|_error| EvalError::BoundExceeded)
}

fn decimal(value: &Value) -> Result<Option<Decimal>, EvalError> {
    match value {
        Value::Null => Ok(None),
        Value::Number(number) => Decimal::parse(number.as_str())
            .map(Some)
            .map_err(|error| EvalError::Input(error.to_string())),
        other => Err(EvalError::Input(format!("{other:?} is not a number"))),
    }
}

fn bounded(rows: Vec<Row>) -> Result<Vec<Row>, EvalError> {
    if rows.len() > MAX_ROWS {
        return Err(EvalError::BoundExceeded);
    }
    Ok(rows)
}

fn count(value: usize) -> Value {
    i64::try_from(value).map_or(Value::Null, Value::from)
}

/// Evaluate `plan` over one complete authorized snapshot.
///
/// # Errors
/// [`EvalError`] for any bound, overflow, snapshot mismatch or runtime
/// expression failure; nothing partial is returned.
pub fn evaluate(
    plan: &TransformPlan,
    input: &TransformInput,
) -> Result<TransformOutput, EvalError> {
    let mut results: BTreeMap<&str, Vec<Row>> = BTreeMap::new();
    for id in &plan.order {
        let node = &plan.nodes[id];
        let rows = match &node.step {
            Step::Scan { alias } => scan(plan, input, alias)?,
            Step::Filter {
                input: from,
                predicate,
            } => filter(&results[from.as_str()], predicate, plan.fuel_units)?,
            Step::Project {
                input: from,
                columns,
            } => project(&results[from.as_str()], columns, plan.fuel_units)?,
            Step::Join {
                left,
                right,
                on,
                kind,
                renames,
            } => join(
                &results[left.as_str()],
                &results[right.as_str()],
                on,
                *kind,
                renames,
                input,
            )?,
            Step::Aggregate {
                input: from,
                group_by,
                measures,
            } => aggregate(&results[from.as_str()], group_by, measures)?,
            Step::Window {
                input: from,
                partition_by,
                order_by,
                columns,
            } => window(
                &results[from.as_str()],
                plan,
                from,
                partition_by,
                order_by,
                columns,
            )?,
            Step::Union { inputs, branch } => {
                let mut rows = Vec::new();
                for from in inputs {
                    for row in &results[from.as_str()] {
                        let mut row = row.clone();
                        row.values
                            .insert(branch.clone(), Value::String(from.clone()));
                        rows.push(row);
                    }
                }
                bounded(rows)?
            }
            Step::Dedupe {
                input: from,
                key: columns,
                order_by,
                keep,
            } => dedupe(
                &results[from.as_str()],
                plan,
                from,
                columns,
                order_by,
                *keep,
            )?,
        };
        results.insert(id.as_str(), rows);
    }
    let schema = plan.output_schema();
    let rows = results.remove(plan.output.as_str()).unwrap_or_default();
    let types: Vec<&Type> = schema
        .grain
        .iter()
        .map(|name| schema.column(name).map(|column| &column.ty))
        .collect::<Option<_>>()
        .ok_or_else(|| EvalError::Input("grain column missing".into()))?;
    let mut keyed = Vec::with_capacity(rows.len());
    for row in rows {
        let sort = schema
            .grain
            .iter()
            .zip(&types)
            .map(|(name, ty)| key(row.values.get(name).unwrap_or(&Value::Null), ty))
            .collect::<Result<Vec<_>, _>>()?;
        keyed.push((sort, row));
    }
    keyed.sort_by(|a, b| a.0.cmp(&b.0));
    if keyed.windows(2).any(|pair| pair[0].0 == pair[1].0) {
        return Err(EvalError::Input(
            "two output rows share one identity".into(),
        ));
    }
    let columns: Vec<String> = schema.columns.iter().map(|c| c.name.clone()).collect();
    let rows = keyed
        .into_iter()
        .map(|(_, row)| {
            let values = columns
                .iter()
                .map(|name| {
                    Ok((
                        name.clone(),
                        to_runtime(row.values.get(name).unwrap_or(&Value::Null))?,
                    ))
                })
                .collect::<Result<BTreeMap<_, _>, EvalError>>()?;
            let key = schema
                .grain
                .iter()
                .map(|name| values.get(name).cloned().unwrap_or(RuntimeValue::Null))
                .collect();
            Ok(OutputRow {
                key,
                values,
                members: row.members,
            })
        })
        .collect::<Result<_, EvalError>>()?;
    Ok(TransformOutput {
        columns,
        grain: schema.grain.clone(),
        rows,
    })
}

fn scan(plan: &TransformPlan, input: &TransformInput, alias: &str) -> Result<Vec<Row>, EvalError> {
    let scanned = &plan.inputs[alias];
    let source = input
        .rows
        .get(alias)
        .ok_or_else(|| EvalError::Input(format!("input {alias:?} is missing")))?;
    if source.len() > MAX_ROWS {
        return Err(EvalError::BoundExceeded);
    }
    let mut seen = BTreeSet::new();
    let mut rows = Vec::with_capacity(source.len());
    for record in source {
        if !seen.insert(record.id.as_str()) {
            return Err(EvalError::Input(format!(
                "input {alias:?} repeats id {:?}",
                record.id
            )));
        }
        let mut values = BTreeMap::from([("id".to_owned(), Value::String(record.id.clone()))]);
        for property in &scanned.properties {
            let value = match record.values.get(property) {
                Some(value) => {
                    Value::try_from(value).map_err(|error| EvalError::Input(error.to_string()))?
                }
                None => Value::Null,
            };
            values.insert(property.clone(), value);
        }
        rows.push(Row {
            values,
            members: BTreeSet::from([MemberRef {
                alias: alias.to_owned(),
                id: record.id.clone(),
                version: record.version,
            }]),
        });
    }
    Ok(rows)
}

fn evaluate_expression(typed: &TypedExpression, row: &Row, fuel: u64) -> Result<Value, EvalError> {
    let context = Value::object([(ROW_ROOT, Value::Object(row.values.clone()))]);
    let attempt = typed.expression.evaluate_admitted_metered(
        &context,
        EvaluationBudget::new(fuel),
        &typed.env,
        &typed.expected,
    );
    attempt
        .result
        .map(norn_expr::Evaluation::into_value)
        .map_err(|error| EvalError::Expression(error.to_string()))
}

fn filter(rows: &[Row], predicate: &TypedExpression, fuel: u64) -> Result<Vec<Row>, EvalError> {
    let mut kept = Vec::new();
    for row in rows {
        match evaluate_expression(predicate, row, fuel)? {
            Value::Bool(true) => kept.push(row.clone()),
            Value::Bool(false) => {}
            other => {
                return Err(EvalError::Expression(format!(
                    "a predicate returned {other:?}"
                )));
            }
        }
    }
    Ok(kept)
}

fn project(
    rows: &[Row],
    columns: &[(String, Projection)],
    fuel: u64,
) -> Result<Vec<Row>, EvalError> {
    rows.iter()
        .map(|row| {
            let mut values = BTreeMap::new();
            for (name, projection) in columns {
                let value = match projection {
                    Projection::Column(source) => {
                        row.values.get(source).cloned().unwrap_or(Value::Null)
                    }
                    Projection::Expression(typed) => evaluate_expression(typed, row, fuel)?,
                };
                values.insert(name.clone(), value);
            }
            Ok(Row {
                values,
                members: row.members.clone(),
            })
        })
        .collect()
}

fn joined(left: &Row, right: Option<&Row>, renames: &[(String, String)]) -> Row {
    let mut values = left.values.clone();
    let mut members = left.members.clone();
    for (source, name) in renames {
        let value = right
            .and_then(|row| row.values.get(source).cloned())
            .unwrap_or(Value::Null);
        values.insert(name.clone(), value);
    }
    if let Some(right) = right {
        members.extend(right.members.iter().cloned());
    }
    Row { values, members }
}

fn text(value: Option<&Value>) -> Option<&str> {
    match value {
        Some(Value::String(text)) => Some(text),
        _ => None,
    }
}

fn normalized(value: &Value, steps: &[norn_ir::Normalization]) -> Result<Key, EvalError> {
    match value {
        Value::String(text) => Ok(Key::Text(crate::entity_instance::normalize_text(
            text, steps,
        ))),
        other => key(other, &Type::Json),
    }
}

fn join(
    left: &[Row],
    right: &[Row],
    on: &AdmittedJoin,
    kind: JoinKind,
    renames: &[(String, String)],
    input: &TransformInput,
) -> Result<Vec<Row>, EvalError> {
    let mut out = Vec::new();
    let mut push = |row: Row| -> Result<(), EvalError> {
        if out.len() >= MAX_ROWS {
            return Err(EvalError::BoundExceeded);
        }
        out.push(row);
        Ok(())
    };
    match on {
        AdmittedJoin::Link {
            column,
            alias,
            link,
            right: right_id,
        } => {
            let by_id: BTreeMap<&str, &Row> = right
                .iter()
                .filter_map(|row| text(row.values.get(right_id)).map(|id| (id, row)))
                .collect();
            let edges = input.links.get(&(alias.clone(), link.clone()));
            for row in left {
                let targets: Vec<&Row> = text(row.values.get(column))
                    .and_then(|source| edges.and_then(|edges| edges.get(source)))
                    .map(|targets| {
                        let mut ids: Vec<&String> = targets.iter().collect();
                        ids.sort();
                        ids.dedup();
                        ids.into_iter()
                            .filter_map(|id| by_id.get(id.as_str()).copied())
                            .collect()
                    })
                    .unwrap_or_default();
                if targets.is_empty() {
                    if kind == JoinKind::Left {
                        push(joined(row, None, renames))?;
                    }
                    continue;
                }
                for target in targets {
                    push(joined(row, Some(target), renames))?;
                }
            }
        }
        AdmittedJoin::Key { pairs } => {
            let mut index: BTreeMap<Vec<Key>, &Row> = BTreeMap::new();
            for row in right {
                let mut tuple = Vec::with_capacity(pairs.len());
                let mut null = false;
                for (_, column, steps) in pairs {
                    let value = row.values.get(column).unwrap_or(&Value::Null);
                    null |= matches!(value, Value::Null);
                    tuple.push(normalized(value, steps)?);
                }
                if !null && index.insert(tuple, row).is_some() {
                    return Err(EvalError::Input(
                        "a declared key repeats on the right".into(),
                    ));
                }
            }
            for row in left {
                let mut tuple = Vec::with_capacity(pairs.len());
                let mut null = false;
                for (column, _, steps) in pairs {
                    let value = row.values.get(column).unwrap_or(&Value::Null);
                    null |= matches!(value, Value::Null);
                    tuple.push(normalized(value, steps)?);
                }
                match (null, index.get(&tuple)) {
                    (false, Some(target)) => push(joined(row, Some(target), renames))?,
                    _ if kind == JoinKind::Left => push(joined(row, None, renames))?,
                    _ => {}
                }
            }
        }
    }
    Ok(out)
}

/// Days since 1970-01-01 of a proleptic Gregorian date.
fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = year.div_euclid(400);
    let yoe = year - era * 400;
    let mp = (month + 9) % 12;
    let doy = (153 * mp + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// The proleptic Gregorian date of a day count since 1970-01-01.
fn civil_from_days(days: i64) -> (i64, i64, i64) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    (year, month, day)
}

/// ISO/UTC calendar bucket start of a date or timestamp, as a date
/// (`YYYY-MM-DD`; ISO weeks start on Monday).
///
/// # Errors
/// A value that is not a valid date or timestamp, or a year outside 0–9999.
pub fn bucket(value: &Value, ty: &Type, grain: TimeGrain) -> Result<Value, EvalError> {
    let Value::String(text) = value else {
        return match value {
            Value::Null => Ok(Value::Null),
            other => Err(EvalError::Input(format!("{other:?} is not temporal"))),
        };
    };
    let invalid = || EvalError::Input(format!("{text:?} is not a valid date or timestamp"));
    let days = match strip(ty) {
        Type::Timestamp { .. } => {
            let (seconds, _) =
                norn_expr::temporal::timestamp_ordering_key(text).map_err(|_error| invalid())?;
            seconds.div_euclid(86_400)
        }
        _ => {
            norn_expr::temporal::validate_date(text).map_err(|_error| invalid())?;
            let mut parts = text.splitn(3, '-').map(str::parse::<i64>);
            let (Some(Ok(year)), Some(Ok(month)), Some(Ok(day))) =
                (parts.next(), parts.next(), parts.next())
            else {
                return Err(invalid());
            };
            days_from_civil(year, month, day)
        }
    };
    let (year, month, day) = civil_from_days(days);
    let (year, month, day) = match grain {
        TimeGrain::Day => (year, month, day),
        TimeGrain::Week => civil_from_days(days - (days + 3).rem_euclid(7)),
        TimeGrain::Month => (year, month, 1),
        TimeGrain::Quarter => (year, (month - 1) / 3 * 3 + 1, 1),
        TimeGrain::Year => (year, 1, 1),
    };
    if !(0..=9999).contains(&year) {
        return Err(invalid());
    }
    Ok(Value::String(format!("{year:04}-{month:02}-{day:02}")))
}

type GroupSpec = (String, Option<TimeGrain>, Type);
type MeasureSpec = (String, AggregateFn, Option<String>, Option<Type>);

fn aggregate(
    rows: &[Row],
    group_by: &[GroupSpec],
    measures: &[MeasureSpec],
) -> Result<Vec<Row>, EvalError> {
    let mut groups: BTreeMap<Vec<Key>, (Vec<Value>, Vec<&Row>)> = BTreeMap::new();
    for row in rows {
        let mut values = Vec::with_capacity(group_by.len());
        let mut keys = Vec::with_capacity(group_by.len());
        for (column, grain, ty) in group_by {
            let value = row.values.get(column).unwrap_or(&Value::Null);
            let value = match grain {
                Some(grain) => bucket(value, ty, *grain)?,
                None => value.clone(),
            };
            let bucket_type = if grain.is_some() { &Type::Date } else { ty };
            keys.push(key(&value, bucket_type)?);
            values.push(value);
        }
        groups
            .entry(keys)
            .or_insert_with(|| (values, Vec::new()))
            .1
            .push(row);
    }
    if group_by.is_empty() && groups.is_empty() {
        groups.insert(Vec::new(), (Vec::new(), Vec::new()));
    }
    let mut out = Vec::with_capacity(groups.len());
    for (_, (keys, members)) in groups {
        let mut values: BTreeMap<String, Value> = group_by
            .iter()
            .map(|(column, _, _)| column.clone())
            .zip(keys)
            .collect();
        for (name, function, column, ty) in measures {
            let read: Vec<&Value> = column.as_ref().map_or_else(Vec::new, |column| {
                members
                    .iter()
                    .map(|row| row.values.get(column).unwrap_or(&Value::Null))
                    .filter(|value| !matches!(value, Value::Null))
                    .collect()
            });
            let value = match (function, column) {
                (AggregateFn::Count, None) => count(members.len()),
                (AggregateFn::Count, Some(_)) => count(read.len()),
                (AggregateFn::CountDistinct, _) => {
                    let ty = ty.as_ref().unwrap_or(&Type::Json);
                    let distinct = read
                        .iter()
                        .map(|value| key(value, ty))
                        .collect::<Result<BTreeSet<_>, _>>()?;
                    count(distinct.len())
                }
                (AggregateFn::Sum, _) => sum(read.iter().copied())?,
                (AggregateFn::Min | AggregateFn::Max, _) => {
                    let ty = ty.as_ref().unwrap_or(&Type::Json);
                    let mut best: Option<(Key, &Value)> = None;
                    for value in read {
                        let candidate = key(value, ty)?;
                        let better = best.as_ref().is_none_or(|(current, _)| {
                            if *function == AggregateFn::Min {
                                candidate < *current
                            } else {
                                candidate > *current
                            }
                        });
                        if better {
                            best = Some((candidate, value));
                        }
                    }
                    best.map_or(Value::Null, |(_, value)| value.clone())
                }
            };
            values.insert(name.clone(), value);
        }
        out.push(Row {
            values,
            members: members
                .iter()
                .flat_map(|row| row.members.iter().cloned())
                .collect(),
        });
    }
    bounded(out)
}

fn sum<'v>(values: impl Iterator<Item = &'v Value>) -> Result<Value, EvalError> {
    let mut total: Option<Decimal> = None;
    for value in values {
        if let Some(addend) = decimal(value)? {
            total = Some(match total {
                None => addend,
                Some(sum) => sum.checked_add(addend).ok_or(EvalError::BoundExceeded)?,
            });
        }
    }
    total.map_or(Ok(Value::Null), number)
}

fn order_keys(
    row: &Row,
    order_by: &[(String, SortDirection, Type)],
) -> Result<Vec<(Key, SortDirection)>, EvalError> {
    order_by
        .iter()
        .map(|(column, direction, ty)| {
            Ok((
                key(row.values.get(column).unwrap_or(&Value::Null), ty)?,
                *direction,
            ))
        })
        .collect()
}

/// Nulls last in both directions.
fn compare(a: &[(Key, SortDirection)], b: &[(Key, SortDirection)]) -> Ordering {
    for ((left, direction), (right, _)) in a.iter().zip(b) {
        let ordering = match (left, right) {
            (Key::Null, Key::Null) => Ordering::Equal,
            (Key::Null, _) => Ordering::Greater,
            (_, Key::Null) => Ordering::Less,
            _ if *direction == SortDirection::Desc => right.cmp(left),
            _ => left.cmp(right),
        };
        if ordering != Ordering::Equal {
            return ordering;
        }
    }
    Ordering::Equal
}

fn column_types<'p>(plan: &'p TransformPlan, node: &str, columns: &[String]) -> Vec<&'p Type> {
    let schema = &plan.nodes[node].schema;
    columns
        .iter()
        .map(|name| schema.column(name).map_or(&Type::Json, |column| &column.ty))
        .collect()
}

fn partitions<'r>(
    rows: &'r [Row],
    columns: &[String],
    types: &[&Type],
) -> Result<BTreeMap<Vec<Key>, Vec<&'r Row>>, EvalError> {
    let mut out: BTreeMap<Vec<Key>, Vec<&Row>> = BTreeMap::new();
    for row in rows {
        let keys = columns
            .iter()
            .zip(types)
            .map(|(column, ty)| key(row.values.get(column).unwrap_or(&Value::Null), ty))
            .collect::<Result<Vec<_>, _>>()?;
        out.entry(keys).or_default().push(row);
    }
    Ok(out)
}

fn sorted<'r>(
    rows: Vec<&'r Row>,
    order_by: &[(String, SortDirection, Type)],
) -> Result<Vec<(Vec<(Key, SortDirection)>, &'r Row)>, EvalError> {
    let mut keyed = rows
        .into_iter()
        .map(|row| Ok((order_keys(row, order_by)?, row)))
        .collect::<Result<Vec<_>, EvalError>>()?;
    keyed.sort_by(|a, b| compare(&a.0, &b.0));
    Ok(keyed)
}

fn window(
    rows: &[Row],
    plan: &TransformPlan,
    node: &str,
    partition_by: &[String],
    order_by: &[(String, SortDirection, Type)],
    columns: &[(String, WindowFn, Option<String>, u32)],
) -> Result<Vec<Row>, EvalError> {
    let types = column_types(plan, node, partition_by);
    let mut out = Vec::with_capacity(rows.len());
    for (_, members) in partitions(rows, partition_by, &types)? {
        let ordered = sorted(members, order_by)?;
        let mut computed: Vec<Row> = ordered.iter().map(|(_, row)| (*row).clone()).collect();
        for (name, function, column, offset) in columns {
            let offset = usize::try_from(*offset).map_err(|_error| EvalError::BoundExceeded)?;
            let mut rank = 0usize;
            let mut dense = 0usize;
            let mut total: Option<Decimal> = None;
            for (index, (keys, row)) in ordered.iter().enumerate() {
                let tied = index > 0 && compare(&ordered[index - 1].0, keys) == Ordering::Equal;
                if !tied {
                    rank = index + 1;
                    dense += 1;
                }
                let read = |at: Option<usize>| {
                    at.and_then(|at| ordered.get(at))
                        .and_then(|(_, other)| column.as_ref().and_then(|c| other.values.get(c)))
                        .cloned()
                        .unwrap_or(Value::Null)
                };
                let value = match function {
                    WindowFn::RowNumber => count(index + 1),
                    WindowFn::Rank => count(rank),
                    WindowFn::DenseRank => count(dense),
                    WindowFn::Lag => read(index.checked_sub(offset)),
                    WindowFn::Lead => read(index.checked_add(offset)),
                    WindowFn::RunningSum => {
                        let current = column
                            .as_ref()
                            .and_then(|c| row.values.get(c))
                            .unwrap_or(&Value::Null);
                        if let Some(addend) = decimal(current)? {
                            total = Some(match total {
                                None => addend,
                                Some(sum) => {
                                    sum.checked_add(addend).ok_or(EvalError::BoundExceeded)?
                                }
                            });
                        }
                        match total {
                            Some(sum) => number(sum)?,
                            None => Value::Null,
                        }
                    }
                };
                computed[index].values.insert(name.clone(), value);
            }
        }
        out.extend(computed);
    }
    bounded(out)
}

fn dedupe(
    rows: &[Row],
    plan: &TransformPlan,
    node: &str,
    columns: &[String],
    order_by: &[(String, SortDirection, Type)],
    keep: DedupeKeep,
) -> Result<Vec<Row>, EvalError> {
    let types = column_types(plan, node, columns);
    let mut out = Vec::new();
    for (_, members) in partitions(rows, columns, &types)? {
        let ordered = sorted(members, order_by)?;
        let chosen = match keep {
            DedupeKeep::First => ordered.first(),
            DedupeKeep::Last => ordered.last(),
        };
        if let Some((_, row)) = chosen {
            out.push((*row).clone());
        }
    }
    Ok(out)
}
