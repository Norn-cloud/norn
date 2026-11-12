//! Pure row projection shared by reference and incremental query execution.
use norn_ir::metric::{FilterOperator, MetricFilter, TimeGrain};
use norn_ir::{Decimal, PropertyType, RuntimeObject, RuntimeValue};

use super::{QuestionPlan, UNSUPPORTED_ENGINE, arithmetic};
use crate::entity_instance::EffectiveKind;

/// One root identity's contributions at the declared output grain.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Contribution {
    /// Scalar group value, or null for an ungrouped question.
    pub bucket: RuntimeValue,
    /// Numerator then denominator; absence means the operand predicate excluded it,
    /// or the operand is null (then `nulls` says so).
    pub amounts: Vec<Option<i64>>,
    /// Per operand: the row passed its predicate but the measured property is
    /// null. A null is skipped by the sum, never read as 0, and counted.
    pub nulls: Vec<bool>,
}

/// COMPUTE-DETERMINISTIC-1: project one typed row without IO or arithmetic rounding.
///
/// # Errors
/// Refuses unsupported/missing properties and values outside the numeric profile.
pub fn project(
    plan: &QuestionPlan,
    row: &RuntimeObject,
) -> Result<Option<Contribution>, &'static str> {
    // A snapshot sum reads each subject's versions, never one row in isolation.
    if plan.as_of() {
        return Err(UNSUPPORTED_ENGINE);
    }
    if !matches_filters(plan, row, &plan.question().filters)? {
        return Ok(None);
    }
    let first = plan.sums().first().ok_or(UNSUPPORTED_ENGINE)?;
    let at = instant(plan, row, first.time_property.as_str())?;
    let parse = norn_expr::temporal::timestamp_ordering_key;
    let start =
        parse(plan.question().time_window.start.as_str()).map_err(|_time| UNSUPPORTED_ENGINE)?;
    let end =
        parse(plan.question().time_window.end.as_str()).map_err(|_time| UNSUPPORTED_ENGINE)?;
    if at < start || at >= end {
        return Ok(None);
    }
    let bucket = match plan.question().dimensions.first() {
        None => RuntimeValue::Null,
        Some(dimension) if dimension.grain == Some(TimeGrain::Week) => {
            let seconds = instant(plan, row, dimension.path.property.as_str())?.0;
            week_key(week_monday(seconds).ok_or(UNSUPPORTED_ENGINE)?)?
        }
        Some(dimension) => value(plan, row, dimension.path.property.as_str())?.clone(),
    };
    let (amounts, nulls): (Vec<_>, Vec<_>) = plan
        .sums()
        .iter()
        .map(|sum| {
            if !matches_filters(plan, row, &sum.filters)? {
                return Ok((None, false));
            }
            // A null on a nullable property (admission refuses null elsewhere) is absent, not 0.
            let measured = value(plan, row, sum.property.id.as_str())?;
            if measured.is_null() {
                return Ok((None, true));
            }
            let number = measured.as_decimal().ok_or(UNSUPPORTED_ENGINE)?;
            coefficient(number)
                .map(|n| (Some(n), false))
                .ok_or(UNSUPPORTED_ENGINE)
        })
        .collect::<Result<Vec<_>, &'static str>>()?
        .into_iter()
        .unzip();
    if amounts.iter().all(Option::is_none) && !nulls.contains(&true) {
        return Ok(None);
    }
    Ok(Some(Contribution {
        bucket,
        amounts,
        nulls,
    }))
}

/// The UTC Monday 00:00 (epoch seconds) of the ISO week holding `seconds`.
fn week_monday(seconds: i64) -> Option<i64> {
    seconds
        .checked_sub(345_600)?
        .div_euclid(604_800)
        .checked_mul(604_800)?
        .checked_add(345_600)
}

/// The bucket key of the ISO week starting at `monday`: its `YYYY-MM-DD`.
fn week_key(monday: i64) -> Result<RuntimeValue, &'static str> {
    let date = norn_expr::temporal::naming_series_date_at(
        monday.checked_mul(1000).ok_or(UNSUPPORTED_ENGINE)?,
        "UTC",
    )
    .map_err(|_date| UNSUPPORTED_ENGINE)?;
    Ok(RuntimeValue::from(format!(
        "{:04}-{:02}-{:02}",
        date.year, date.month, date.day
    )))
}

/// Most output buckets one snapshot question may produce.
const MAX_AS_OF_BUCKETS: usize = 1024;

/// METRIC-SNAPSHOT-ASOF-1: one subject's contributions, given its stored
/// versions as `(version, values)` in any order. Each bucket (an ISO week of
/// the window clipped to it, or one bucket ending at the window's end) takes
/// the subject's last version whose time is before the bucket's end, ties to
/// the later version, carried forward; a subject with no such version is
/// absent. Filters and a non-time grouping read that version; a null measured
/// value is skipped and counted. Each contribution carries its version.
///
/// # Errors
/// Refuses a plan that is not one snapshot sum, values outside the profile,
/// or more than [`MAX_AS_OF_BUCKETS`] buckets.
pub fn as_of(
    plan: &QuestionPlan,
    versions: &[(u64, RuntimeObject)],
) -> Result<Vec<(u64, Contribution)>, &'static str> {
    let [sum] = plan.sums() else {
        return Err(UNSUPPORTED_ENGINE);
    };
    if !sum.as_of {
        return Err(UNSUPPORTED_ENGINE);
    }
    let parse = norn_expr::temporal::timestamp_ordering_key;
    let window = &plan.question().time_window;
    let start = parse(window.start.as_str()).map_err(|_time| UNSUPPORTED_ENGINE)?;
    let end = parse(window.end.as_str()).map_err(|_time| UNSUPPORTED_ENGINE)?;
    let mut series = versions
        .iter()
        .map(|(version, row)| {
            Ok((
                instant(plan, row, sum.time_property.as_str())?,
                *version,
                row,
            ))
        })
        .collect::<Result<Vec<_>, &'static str>>()?;
    series.sort_by_key(|(at, version, _)| (*at, *version));
    let grouping = plan.question().dimensions.first();
    let mut buckets = Vec::new();
    if grouping.is_some_and(|dimension| dimension.grain == Some(TimeGrain::Week)) {
        let mut monday = week_monday(start.0).ok_or(UNSUPPORTED_ENGINE)?;
        while (monday, 0) < end {
            let next = monday.checked_add(604_800).ok_or(UNSUPPORTED_ENGINE)?;
            buckets.push((Some(week_key(monday)?), std::cmp::min((next, 0), end)));
            if buckets.len() > MAX_AS_OF_BUCKETS {
                return Err("core.compute.bound_exceeded");
            }
            monday = next;
        }
    } else {
        buckets.push((None, end));
    }
    let mut contributions = Vec::new();
    for (key, bucket_end) in buckets {
        let Some((_, version, row)) = series.iter().rev().find(|(at, ..)| *at < bucket_end) else {
            continue;
        };
        if !matches_filters(plan, row, &plan.question().filters)?
            || !matches_filters(plan, row, &sum.filters)?
        {
            continue;
        }
        let bucket = match (key, grouping) {
            (Some(key), _) => key,
            (None, None) => RuntimeValue::Null,
            (None, Some(dimension)) => value(plan, row, dimension.path.property.as_str())?.clone(),
        };
        let measured = value(plan, row, sum.property.id.as_str())?;
        let (amount, null) = if measured.is_null() {
            (None, true)
        } else {
            let number = measured.as_decimal().ok_or(UNSUPPORTED_ENGINE)?;
            (Some(coefficient(number).ok_or(UNSUPPORTED_ENGINE)?), false)
        };
        contributions.push((
            *version,
            Contribution {
                bucket,
                amounts: vec![amount],
                nulls: vec![null],
            },
        ));
    }
    Ok(contributions)
}

fn value<'a>(
    plan: &QuestionPlan,
    row: &'a RuntimeObject,
    id: &str,
) -> Result<&'a RuntimeValue, &'static str> {
    let property = plan
        .properties
        .iter()
        .find(|p| p.id.as_str() == id)
        .ok_or(UNSUPPORTED_ENGINE)?;
    let value = row.get(id).ok_or(UNSUPPORTED_ENGINE)?;
    if property.computed || property.check_runtime_value(value).is_err() {
        return Err(UNSUPPORTED_ENGINE);
    }
    Ok(value)
}

fn instant(plan: &QuestionPlan, row: &RuntimeObject, id: &str) -> Result<(i64, u32), &'static str> {
    let text = value(plan, row, id)?.as_str().ok_or(UNSUPPORTED_ENGINE)?;
    let property = plan
        .properties
        .iter()
        .find(|p| p.id.as_str() == id)
        .ok_or(UNSUPPORTED_ENGINE)?;
    let date = matches!(property.kind, EffectiveKind::Base(PropertyType::Date {}));
    norn_expr::temporal::timestamp_ordering_key(&if date {
        format!("{text}T00:00:00Z")
    } else {
        text.to_owned()
    })
    .map_err(|_time| UNSUPPORTED_ENGINE)
}

fn matches_filters(
    plan: &QuestionPlan,
    row: &RuntimeObject,
    filters: &[MetricFilter],
) -> Result<bool, &'static str> {
    for filter in filters {
        let actual = value(plan, row, filter.path.property.as_str())?;
        let property = plan
            .properties
            .iter()
            .find(|p| p.id == filter.path.property)
            .ok_or(UNSUPPORTED_ENGINE)?;
        let equal = if matches!(
            property.kind,
            EffectiveKind::Base(PropertyType::Timestamp { .. })
        ) && !actual.is_null()
            && !filter.value.is_null()
        {
            let parse = norn_expr::temporal::timestamp_ordering_key;
            parse(actual.as_str().ok_or(UNSUPPORTED_ENGINE)?).map_err(|_a| UNSUPPORTED_ENGINE)?
                == parse(filter.value.as_str().ok_or(UNSUPPORTED_ENGINE)?)
                    .map_err(|_b| UNSUPPORTED_ENGINE)?
        } else {
            actual == &filter.value
        };
        let matches = match filter.operator {
            FilterOperator::Eq => equal,
            FilterOperator::Ne => !equal,
            FilterOperator::Lt | FilterOperator::Le | FilterOperator::Gt | FilterOperator::Ge => {
                return Err(UNSUPPORTED_ENGINE);
            }
        };
        if !matches {
            return Ok(false);
        }
    }
    Ok(true)
}

/// Convert a canonical exact decimal to this profile's coefficient; never round.
#[must_use]
pub fn coefficient(value: &Decimal) -> Option<i64> {
    if value.scale() > arithmetic::SCALE {
        return None;
    }
    let text = value.canonical();
    let (whole, fraction) = text.split_once('.').unwrap_or((&text, ""));
    let coefficient: i64 = format!("{whole}{fraction:0<6}").parse().ok()?;
    (-arithmetic::MAX_VALUE..=arithmetic::MAX_VALUE)
        .contains(&coefficient)
        .then_some(coefficient)
}
