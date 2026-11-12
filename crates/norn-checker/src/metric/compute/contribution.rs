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
    /// Numerator then denominator; absence means the operand predicate excluded it.
    pub amounts: Vec<Option<i64>>,
}

/// COMPUTE-DETERMINISTIC-1: project one typed row without IO or arithmetic rounding.
///
/// # Errors
/// Refuses unsupported/missing properties and values outside the numeric profile.
pub fn project(
    plan: &QuestionPlan,
    row: &RuntimeObject,
) -> Result<Option<Contribution>, &'static str> {
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
        Some(dimension) => match dimension.grain {
            None => value(plan, row, dimension.path.property.as_str())?.clone(),
            Some(grain) => {
                let seconds = instant(plan, row, dimension.path.property.as_str())?.0;
                RuntimeValue::from(period_start(seconds, grain)?)
            }
        },
    };
    let amounts = plan
        .sums()
        .iter()
        .map(|sum| {
            if !matches_filters(plan, row, &sum.filters)? {
                return Ok(None);
            }
            let number = value(plan, row, sum.property.id.as_str())?
                .as_decimal()
                .ok_or(UNSUPPORTED_ENGINE)?;
            coefficient(number).map(Some).ok_or(UNSUPPORTED_ENGINE)
        })
        .collect::<Result<Vec<_>, _>>()?;
    if amounts.iter().all(Option::is_none) {
        return Ok(None);
    }
    Ok(Some(Contribution { bucket, amounts }))
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
        assert_eq!(period_start(q2, TimeGrain::Quarter).expect("q2"), "2024-04-01");
        assert_eq!(period_start(q2 - 1, TimeGrain::Quarter).expect("q1"), "2024-01-01");
        // Before the epoch a day still starts at its own midnight.
        assert_eq!(period_start(-1, TimeGrain::Day).expect("eve"), "1969-12-31");
        // 2024-12-30 is a Monday whose ISO week ends in 2025.
        assert_eq!(
            period_start(1_735_603_200, TimeGrain::Week).expect("week"),
            "2024-12-30"
        );
    }
}
