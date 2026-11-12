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
    // No group-by buckets everything together; one buckets by its value; more
    // than one by the tuple of their values, in the question's order. A null
    // member is its own group, never dropped and never equal to "".
    let members = plan
        .question()
        .dimensions
        .iter()
        .map(|dimension| match dimension.grain {
            None => Ok(value(plan, row, dimension.path.property.as_str())?.clone()),
            Some(grain) => {
                let seconds = instant(plan, row, dimension.path.property.as_str())?.0;
                Ok(RuntimeValue::from(period_start(seconds, grain)?))
            }
        })
        .collect::<Result<Vec<_>, &'static str>>()?;
    let bucket = match <[RuntimeValue; 1]>::try_from(members) {
        Ok([one]) => one,
        Err(members) if members.is_empty() => RuntimeValue::Null,
        Err(members) => RuntimeValue::Array(members),
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

/// The first day of the UTC civil `grain` period holding `seconds`, as
/// `YYYY-MM-DD`. Periods are half-open: a period's first instant is in it and
/// the next period's first instant is not. Weeks are ISO weeks, from Monday.
/// The same labels as `DuckDB`'s `date_trunc` in UTC, cast to a date.
///
/// # Errors
/// An instant outside the calendar's range.
pub fn period_start(seconds: i64, grain: TimeGrain) -> Result<String, &'static str> {
    let day_of = |seconds: i64| {
        norn_expr::temporal::naming_series_date_at(
            seconds.checked_mul(1000).ok_or(UNSUPPORTED_ENGINE)?,
            "UTC",
        )
        .map_err(|_date| UNSUPPORTED_ENGINE)
    };
    let (year, month, day) = match grain {
        TimeGrain::Week => {
            // 1970-01-05 (345,600 s after the epoch) is a Monday.
            let monday = seconds
                .checked_sub(345_600)
                .ok_or(UNSUPPORTED_ENGINE)?
                .div_euclid(604_800)
                .checked_mul(604_800)
                .and_then(|s| s.checked_add(345_600))
                .ok_or(UNSUPPORTED_ENGINE)?;
            let date = day_of(monday)?;
            (date.year, date.month, date.day)
        }
        TimeGrain::Day => {
            let date = day_of(seconds)?;
            (date.year, date.month, date.day)
        }
        TimeGrain::Month => {
            let date = day_of(seconds)?;
            (date.year, date.month, 1)
        }
        TimeGrain::Quarter => {
            let date = day_of(seconds)?;
            let first = match date.month {
                1..=3 => 1,
                4..=6 => 4,
                7..=9 => 7,
                _ => 10,
            };
            (date.year, first, 1)
        }
        TimeGrain::Year => (day_of(seconds)?.year, 1, 1),
    };
    Ok(format!("{year:04}-{month:02}-{day:02}"))
}

/// Most output buckets one snapshot question may produce.
const MAX_AS_OF_BUCKETS: usize = 1024;

/// An exact UTC instant as `(seconds, nanoseconds)`.
type Instant = (i64, u32);

/// A snapshot bucket: its period label (none without a time grouping) and its
/// exclusive end.
type Period = (Option<String>, Instant);

/// METRIC-SNAPSHOT-ASOF-1: one subject's contributions, given its stored
/// versions as `(version, values)` in any order. A time grouping (on the
/// measure's own time property) cuts the window into its civil periods,
/// clipped to the window; without one, a single bucket ends at the window's
/// end. In each bucket the subject takes its last version whose time is
/// before the bucket's end, ties to the later version, carried forward; a
/// subject with no such version is absent. Filters and every other grouping
/// read that version, so the bucket key is the period label and those values,
/// in the question's order, as [`project`] keys them. A null measured value
/// is skipped and counted. Each contribution carries its version.
///
/// # Errors
/// Refuses a plan that is not one snapshot sum, values outside the profile,
/// or more than 1024 periods.
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
    let dimensions = &plan.question().dimensions;
    let timed: Vec<_> = dimensions.iter().filter(|d| d.grain.is_some()).collect();
    let buckets = match timed.as_slice() {
        [] => vec![(None, end)],
        [dimension] if dimension.path.property == sum.time_property => {
            periods(start, end, dimension.grain.ok_or(UNSUPPORTED_ENGINE)?)?
        }
        _ => return Err(UNSUPPORTED_ENGINE),
    };
    let mut contributions = Vec::new();
    for (label, bucket_end) in buckets {
        let Some((_, version, row)) = series.iter().rev().find(|(at, ..)| *at < bucket_end) else {
            continue;
        };
        if !matches_filters(plan, row, &plan.question().filters)?
            || !matches_filters(plan, row, &sum.filters)?
        {
            continue;
        }
        let members = dimensions
            .iter()
            .map(|dimension| match (&label, dimension.grain) {
                (Some(label), Some(_)) => Ok(RuntimeValue::from(label.clone())),
                _ => Ok(value(plan, row, dimension.path.property.as_str())?.clone()),
            })
            .collect::<Result<Vec<_>, &'static str>>()?;
        let bucket = match <[RuntimeValue; 1]>::try_from(members) {
            Ok([one]) => one,
            Err(members) if members.is_empty() => RuntimeValue::Null,
            Err(members) => RuntimeValue::Array(members),
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

/// The civil `grain` periods meeting `[start, end)`: each period's label (as
/// [`period_start`] gives it) and its exclusive end, clipped to `end`.
fn periods(start: Instant, end: Instant, grain: TimeGrain) -> Result<Vec<Period>, &'static str> {
    // Every period boundary is a UTC midnight: walk midnights until the label changes.
    let mut day = start
        .0
        .div_euclid(86_400)
        .checked_mul(86_400)
        .ok_or(UNSUPPORTED_ENGINE)?;
    let mut label = period_start(start.0, grain)?;
    let mut periods = Vec::new();
    for _ in 0..400_000 {
        day = day.checked_add(86_400).ok_or(UNSUPPORTED_ENGINE)?;
        if (day, 0) >= end {
            periods.push((Some(label), end));
            return Ok(periods);
        }
        let next = period_start(day, grain)?;
        if next != label {
            periods.push((Some(std::mem::replace(&mut label, next)), (day, 0)));
            if periods.len() >= MAX_AS_OF_BUCKETS {
                return Err("core.compute.bound_exceeded");
            }
        }
    }
    Err("core.compute.bound_exceeded")
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

#[cfg(test)]
#[allow(
    clippy::expect_used,
    clippy::arithmetic_side_effects,
    reason = "calendar label fixtures"
)]
mod tests {
    use super::period_start;
    use norn_ir::metric::TimeGrain;

    /// 2024-02-29T23:59:59Z, a leap-day Thursday in Q1.
    const LEAP_EVE: i64 = 1_709_251_199;

    #[test]
    fn each_grain_labels_its_periods_first_utc_day() {
        for (grain, label) in [
            (TimeGrain::Day, "2024-02-29"),
            (TimeGrain::Week, "2024-02-26"),
            (TimeGrain::Month, "2024-02-01"),
            (TimeGrain::Quarter, "2024-01-01"),
            (TimeGrain::Year, "2024-01-01"),
        ] {
            assert_eq!(period_start(LEAP_EVE, grain).expect("label"), label);
        }
    }

    #[test]
    fn periods_are_half_open() {
        // 2024-04-01T00:00:00Z opens Q2; the second before it is still Q1.
        let q2 = 1_711_929_600;
        assert_eq!(
            period_start(q2, TimeGrain::Quarter).expect("q2"),
            "2024-04-01"
        );
        assert_eq!(
            period_start(q2 - 1, TimeGrain::Quarter).expect("q1"),
            "2024-01-01"
        );
        // Before the epoch a day still starts at its own midnight.
        assert_eq!(period_start(-1, TimeGrain::Day).expect("eve"), "1969-12-31");
        // 2024-12-30 is a Monday whose ISO week ends in 2025.
        assert_eq!(
            period_start(1_735_603_200, TimeGrain::Week).expect("week"),
            "2024-12-30"
        );
    }
}
