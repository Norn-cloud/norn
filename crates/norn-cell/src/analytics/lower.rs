//! QUERY-ENGINE-1: lower an admitted plan to the engine's typed question,
//! keeping the shared projection's equality, null and refusal semantics.

use norn_checker::entity_instance::{EffectiveKind, EffectiveProperty};
use norn_checker::metric::compute::contribution::instant_key;
use norn_checker::metric::compute::{QuestionPlan, UNSUPPORTED_ENGINE};
use norn_ir::metric::{FilterOperator, MetricFilter, TimeGrain};
use norn_ir::{PropertyType, RuntimeValue};

use super::{
    Bucket, Column, ColumnKind, Filter, Literal, Lowered, Operand, TableSchema, Test, Window,
};
use crate::CellError;

const fn refused() -> CellError {
    CellError::ComputeRefused(UNSUPPORTED_ENGINE)
}

/// The storage kind of a stored property; computed properties are not stored.
pub(crate) fn column_kind(property: &EffectiveProperty) -> Option<ColumnKind> {
    if property.computed {
        return None;
    }
    Some(match &property.kind {
        EffectiveKind::Base(PropertyType::Text { .. } | PropertyType::Enum { .. }) => {
            ColumnKind::Text
        }
        EffectiveKind::Base(PropertyType::Integer { .. }) => ColumnKind::Integer,
        EffectiveKind::Base(PropertyType::Decimal { scale, .. }) => match u32::try_from(scale.0) {
            Ok(scale) if scale <= 37 => ColumnKind::Decimal { scale },
            Ok(_) | Err(_) => ColumnKind::Opaque,
        },
        EffectiveKind::Base(PropertyType::Boolean {}) => ColumnKind::Boolean,
        EffectiveKind::Base(PropertyType::Timestamp { .. }) => ColumnKind::Timestamp,
        EffectiveKind::Base(PropertyType::Date {}) => ColumnKind::Date,
        EffectiveKind::Base(_) | EffectiveKind::Registry(_) | EffectiveKind::Unresolved => {
            ColumnKind::Opaque
        }
    })
}

/// The analytical table of `name` with its stored properties, in order, and
/// a pin over the columns, so a definitions change rebuilds the table.
///
/// # Errors
/// The columns cannot be encoded.
pub fn table_schema(name: &str, properties: &[EffectiveProperty]) -> Result<TableSchema, CellError> {
    let columns: Vec<Column> = properties
        .iter()
        .filter_map(|property| {
            column_kind(property).map(|kind| Column {
                id: property.id.as_str().to_owned(),
                kind,
                nullable: property.nullable,
            })
        })
        .collect();
    let pin = crate::codec::hash_json(&(name, &columns))?;
    Ok(TableSchema {
        name: name.to_owned(),
        columns,
        pin,
    })
}

fn property<'a>(plan: &'a QuestionPlan, id: &str) -> Result<&'a EffectiveProperty, CellError> {
    plan.properties()
        .iter()
        .find(|p| p.id.as_str() == id)
        .ok_or_else(refused)
}

fn kind(plan: &QuestionPlan, id: &str) -> Result<ColumnKind, CellError> {
    match column_kind(property(plan, id)?) {
        Some(ColumnKind::Opaque) | None => Err(refused()),
        Some(kind) => Ok(kind),
    }
}

/// Exact equality of a stored value with a filter literal, as the shared
/// projection decides it: timestamps by ordering key, everything else by
/// runtime-value equality.
fn filter(plan: &QuestionPlan, filter: &MetricFilter) -> Result<Filter, CellError> {
    if !filter.path.links.is_empty() {
        return Err(refused());
    }
    let negate = match filter.operator {
        FilterOperator::Eq => false,
        FilterOperator::Ne => true,
        FilterOperator::Lt | FilterOperator::Le | FilterOperator::Gt | FilterOperator::Ge => {
            return Err(refused());
        }
    };
    let column = filter.path.property.as_str();
    let test = match (kind(plan, column)?, &filter.value) {
        (_, RuntimeValue::Null) => Test::IsNull,
        (ColumnKind::Timestamp, RuntimeValue::String(text)) => match instant_key(text, false) {
            Some((secs, nanos)) => Test::Instant { secs, nanos },
            None => Test::RefusePresent,
        },
        (ColumnKind::Timestamp, _) => Test::RefusePresent,
        (ColumnKind::Text | ColumnKind::Date, RuntimeValue::String(text)) => {
            Test::Equals(Literal::Text(text.clone()))
        }
        (ColumnKind::Boolean, RuntimeValue::Bool(flag)) => Test::Equals(Literal::Bool(*flag)),
        (ColumnKind::Integer, RuntimeValue::Number(number)) if number.is_integer() => {
            Test::Equals(Literal::Number(number.canonical()))
        }
        (ColumnKind::Decimal { scale }, RuntimeValue::Number(number))
            if number.scale() <= scale =>
        {
            Test::Equals(Literal::Number(number.canonical()))
        }
        // A literal of another kind, or with more fractional digits than any
        // stored value has, equals no stored value.
        _ => Test::NeverPresent,
    };
    Ok(Filter {
        column: column.to_owned(),
        negate,
        test,
    })
}

fn instant_column(plan: &QuestionPlan, id: &str) -> Result<String, CellError> {
    match kind(plan, id)? {
        ColumnKind::Timestamp | ColumnKind::Date => Ok(id.to_owned()),
        ColumnKind::Text
        | ColumnKind::Integer
        | ColumnKind::Decimal { .. }
        | ColumnKind::Boolean
        | ColumnKind::Opaque => Err(refused()),
    }
}

/// QUERY-ENGINE-1: the plan as the engine's typed question. Anything outside
/// the lowered profile refuses with `core.compute.unsupported_engine`; it is
/// never approximated.
///
/// # Errors
/// Unsupported semantics, or a window that is not a pair of timestamps.
pub fn lower(plan: &QuestionPlan) -> Result<Lowered, CellError> {
    let question = plan.question();
    let first = plan.sums().first().ok_or_else(refused)?;
    let time = first.time_property.as_str();
    let window = Window {
        column: instant_column(plan, time)?,
        start: instant_key(question.time_window.start.as_str(), false).ok_or_else(refused)?,
        end: instant_key(question.time_window.end.as_str(), false).ok_or_else(refused)?,
    };
    let bucket = match question.dimensions.as_slice() {
        [] => Bucket::None,
        [dimension] if !dimension.path.links.is_empty() => return Err(refused()),
        [dimension] => match dimension.grain {
            Some(TimeGrain::Week) => Bucket::Week {
                column: instant_column(plan, dimension.path.property.as_str())?,
            },
            None => {
                let column = dimension.path.property.as_str();
                kind(plan, column)?;
                Bucket::Column {
                    column: column.to_owned(),
                }
            }
            Some(_) => return Err(refused()),
        },
        _ => return Err(refused()),
    };
    let operands = plan
        .sums()
        .iter()
        .map(|sum| {
            let column = sum.property.id.as_str();
            match kind(plan, column)? {
                ColumnKind::Integer | ColumnKind::Decimal { .. } => Ok(Operand {
                    column: column.to_owned(),
                    filters: sum
                        .filters
                        .iter()
                        .map(|f| filter(plan, f))
                        .collect::<Result<_, _>>()?,
                }),
                ColumnKind::Text
                | ColumnKind::Boolean
                | ColumnKind::Timestamp
                | ColumnKind::Date
                | ColumnKind::Opaque => Err(refused()),
            }
        })
        .collect::<Result<Vec<_>, CellError>>()?;
    if !(1..=2).contains(&operands.len()) {
        return Err(refused());
    }
    Ok(Lowered {
        filters: question
            .filters
            .iter()
            .map(|f| filter(plan, f))
            .collect::<Result<_, _>>()?,
        window,
        bucket,
        operands,
    })
}
