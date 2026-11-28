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
    // A snapshot sum reads each subject's versions, and a linked time its
    // linked object; neither projects one row in isolation.
    if plan.as_of() || plan.time_link().is_some() {
        return Err(UNSUPPORTED_ENGINE);
    }
    project_row(plan, row, None)
}

/// What a row whose time is its linked object's contributes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Linked {
    /// The row contributes in its bucket.
    Contribution(Contribution),
    /// The row does not count: a filter excludes it, or its time is outside
    /// the window.
    Excluded,
    /// The row would count but its optional link is absent, so it has no
    /// time: skipped and counted, never given an invented time.
    Unlinked,
}

/// METRIC-GRAIN-LINK-1: project one row whose time is a property of the
/// object its to-one time link reaches. `linked` holds that object's current
/// values, or `None` when the link is absent.
///
/// # Errors
/// Refuses a plan without a linked time, and what [`project`] refuses.
pub fn project_linked(
    plan: &QuestionPlan,
    row: &RuntimeObject,
    linked: Option<&RuntimeObject>,
) -> Result<Linked, &'static str> {
    let link = plan.time_link().ok_or(UNSUPPORTED_ENGINE)?;
    let Some(target) = linked else {
        let counts = matches_filters(plan, row, &plan.question().filters)?
            && plan
                .sums()
                .iter()
                .map(|sum| matches_filters(plan, row, &sum.filters))
                .collect::<Result<Vec<_>, _>>()?
                .contains(&true);
        return Ok(if counts {
            Linked::Unlinked
        } else {
            Linked::Excluded
        });
    };
    Ok(
        project_row(plan, row, Some((link, target)))?
            .map_or(Linked::Excluded, Linked::Contribution),
    )
}

fn project_row(
    plan: &QuestionPlan,
    row: &RuntimeObject,
    linked: Option<(&super::LinkedTime, &RuntimeObject)>,
) -> Result<Option<Contribution>, &'static str> {
    if !matches_filters(plan, row, &plan.question().filters)? {
        return Ok(None);
    }
    let first = plan.sums().first().ok_or(UNSUPPORTED_ENGINE)?;
    let linked_at = linked
        .map(|(link, target)| {
            let value = target
                .get(link.property.id.as_str())
                .ok_or(UNSUPPORTED_ENGINE)?;
            instant_of(&link.property, value)
        })
        .transpose()?;
    let at = match linked_at {
        Some(at) => at,
        None => instant(plan, row, first.time_property.as_str())?,
    };
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
        .map(|dimension| match (dimension.grain, linked_at) {
            (None, _) => Ok(value(plan, row, dimension.path.property.as_str())?.clone()),
            // Admission keeps a linked path for the linked time alone.
            (Some(grain), Some(at)) if !dimension.path.links.is_empty() => {
                Ok(RuntimeValue::from(period_start(at.0, grain)?))
            }
            (Some(grain), _) => {
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

/// Pure proleptic-Gregorian UTC civil date for whole Unix `seconds`,
/// restricted to four-digit years `0000..=9999` (Howard Hinnant's
/// civil-from-days algorithm, zero IANA tzdb dependency).
fn utc_civil_date(seconds: i64) -> Result<(u16, u8, u8), &'static str> {
    // 0000-01-01T00:00:00Z through 9999-12-31T23:59:59Z.
    const MIN_UTC_SECONDS: i64 = -62_167_219_200;
    const MAX_UTC_SECONDS: i64 = 253_402_300_799;
    if !(MIN_UTC_SECONDS..=MAX_UTC_SECONDS).contains(&seconds) {
        return Err(UNSUPPORTED_ENGINE);
    }
    let days = seconds.div_euclid(86_400);
    // Shift epoch from 1970-01-01 to 0000-03-01 (719,468 days earlier).
    let shifted = days.checked_add(719_468).ok_or(UNSUPPORTED_ENGINE)?;
    let era = shifted.div_euclid(146_097);
    let day_of_era = shifted.rem_euclid(146_097);
    let year_of_era = day_of_era
        .checked_sub(day_of_era.div_euclid(1_460))
        .and_then(|value| value.checked_add(day_of_era.div_euclid(36_524)))
        .and_then(|value| value.checked_sub(day_of_era.div_euclid(146_096)))
        .ok_or(UNSUPPORTED_ENGINE)?
        .div_euclid(365);
    let year_march = year_of_era
        .checked_add(era.checked_mul(400).ok_or(UNSUPPORTED_ENGINE)?)
        .ok_or(UNSUPPORTED_ENGINE)?;
    let day_of_year = day_of_era
        .checked_sub(
            year_of_era
                .checked_mul(365)
                .and_then(|value| value.checked_add(year_of_era.div_euclid(4)))
                .and_then(|value| value.checked_sub(year_of_era.div_euclid(100)))
                .ok_or(UNSUPPORTED_ENGINE)?,
        )
        .ok_or(UNSUPPORTED_ENGINE)?;
    let march_month = day_of_year
        .checked_mul(5)
        .and_then(|value| value.checked_add(2))
        .ok_or(UNSUPPORTED_ENGINE)?
        .div_euclid(153);
    let day = day_of_year
        .checked_sub(
            march_month
                .checked_mul(153)
                .and_then(|value| value.checked_add(2))
                .ok_or(UNSUPPORTED_ENGINE)?
                .div_euclid(5),
        )
        .and_then(|value| value.checked_add(1))
        .ok_or(UNSUPPORTED_ENGINE)?;
    let month = if march_month < 10 {
        march_month.checked_add(3)
    } else {
        march_month.checked_sub(9)
    }
    .ok_or(UNSUPPORTED_ENGINE)?;
    let year = year_march
        .checked_add(i64::from(month <= 2))
        .ok_or(UNSUPPORTED_ENGINE)?;
    if !(0..=9_999).contains(&year) {
        return Err(UNSUPPORTED_ENGINE);
    }
    Ok((
        u16::try_from(year).map_err(|_err| UNSUPPORTED_ENGINE)?,
        u8::try_from(month).map_err(|_err| UNSUPPORTED_ENGINE)?,
        u8::try_from(day).map_err(|_err| UNSUPPORTED_ENGINE)?,
    ))
}

/// The first day of the UTC civil `grain` period holding `seconds`, as
/// `YYYY-MM-DD`. Periods are half-open: a period's first instant is in it and
/// the next period's first instant is not. Weeks are ISO weeks, from Monday.
/// The same labels as `DuckDB`'s `date_trunc` in UTC, cast to a date.
///
/// # Errors
/// An instant outside the calendar's range.
pub fn period_start(seconds: i64, grain: TimeGrain) -> Result<String, &'static str> {
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
            utc_civil_date(monday)?
        }
        TimeGrain::Day => utc_civil_date(seconds)?,
        TimeGrain::Month => {
            let (year, month, _) = utc_civil_date(seconds)?;
            (year, month, 1)
        }
        TimeGrain::Quarter => {
            let (year, month, _) = utc_civil_date(seconds)?;
            let first = match month {
                1..=3 => 1,
                4..=6 => 4,
                7..=9 => 7,
                _ => 10,
            };
            (year, first, 1)
        }
        TimeGrain::Year => (utc_civil_date(seconds)?.0, 1, 1),
    };
    Ok(format!("{year:04}-{month:02}-{day:02}"))
}

/// Most output buckets one snapshot question may produce.
const MAX_AS_OF_BUCKETS: usize = 1024;

/// An exact UTC instant as `(seconds, nanoseconds)`.
pub type Instant = (i64, u32);

/// A snapshot bucket: its period label (none without a time grouping) and its
/// exclusive end.
pub type Period = (Option<String>, Instant);

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
/// Every engine reads a snapshot question's buckets from here.
///
/// # Errors
/// An instant outside the calendar's range, or more than the bucket bound.
pub fn periods(
    start: Instant,
    end: Instant,
    grain: TimeGrain,
) -> Result<Vec<Period>, &'static str> {
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
                return Err(UNSUPPORTED_ENGINE);
            }
        }
    }
    Err(UNSUPPORTED_ENGINE)
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
    if property.is_computed() || property.check_runtime_value(value).is_err() {
        return Err(UNSUPPORTED_ENGINE);
    }
    Ok(value)
}

fn instant(plan: &QuestionPlan, row: &RuntimeObject, id: &str) -> Result<(i64, u32), &'static str> {
    let property = plan
        .properties
        .iter()
        .find(|p| p.id.as_str() == id)
        .ok_or(UNSUPPORTED_ENGINE)?;
    instant_of(property, value(plan, row, id)?)
}

/// The exact UTC instant of a stored date or timestamp `value` of `property`.
fn instant_of(
    property: &crate::entity_instance::EffectiveProperty,
    value: &RuntimeValue,
) -> Result<(i64, u32), &'static str> {
    if property.is_computed() || property.check_runtime_value(value).is_err() {
        return Err(UNSUPPORTED_ENGINE);
    }
    let text = value.as_str().ok_or(UNSUPPORTED_ENGINE)?;
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

/// The exact UTC ordering key of a stored timestamp (or of a date's UTC
/// midnight), by the same rules [`project`] reads instants with.
#[must_use]
pub fn instant_key(text: &str, date: bool) -> Option<(i64, u32)> {
    norn_expr::temporal::timestamp_ordering_key(&if date {
        format!("{text}T00:00:00Z")
    } else {
        text.to_owned()
    })
    .ok()
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

    #[test]
    fn utc_period_start_matches_naming_series_date_at_across_bounds_and_leap_years() {
        let grains = [
            TimeGrain::Day,
            TimeGrain::Week,
            TimeGrain::Month,
            TimeGrain::Quarter,
            TimeGrain::Year,
        ];
        // Boundary instants, leap days, non-leap centuries, negative timestamps, and 2026c Edmonton anchor.
        let samples = [
            i64::MIN,
            -62_167_219_201, // -0001-12-31T23:59:59Z (out of range)
            -62_167_219_200, // 0000-01-01T00:00:00Z (Saturday: week falls in year -1 -> Err)
            -62_167_046_400, // 0000-01-03T00:00:00Z (Monday of first full week in year 0000)
            -62_162_121_600, // 0000-02-29T00:00:00Z (year 0000 leap day)
            -62_135_596_800, // 0001-01-01T00:00:00Z
            -2_208_988_800,  // 1900-01-01T00:00:00Z (non-leap century)
            -2_203_977_600,  // 1900-02-28T00:00:00Z
            -2_203_891_200,  // 1900-03-01T00:00:00Z
            -86_401,         // 1969-12-30T23:59:59Z
            -86_400,         // 1969-12-31T00:00:00Z
            -1,              // 1969-12-31T23:59:59Z
            0,               // 1970-01-01T00:00:00Z (Thursday -> week 1969-12-29)
            345_599,         // 1970-01-04T23:59:59Z (Sunday -> week 1969-12-29)
            345_600,         // 1970-01-05T00:00:00Z (Monday -> week 1970-01-05)
            951_782_400,     // 2000-02-29T00:00:00Z (quadricentennial leap day)
            1_798_785_000,   // 2027-01-01T06:30:00Z (UTC is tzdb-independent)
            4_107_542_400,   // 2100-03-01T00:00:00Z (non-leap century)
            253_402_300_799, // 9999-12-31T23:59:59Z (upper bound)
            253_402_300_800, // 10000-01-01T00:00:00Z (out of range)
            i64::MAX,
        ];
        for seconds in samples {
            for grain in grains {
                let reference = |secs: i64| -> Result<String, &'static str> {
                    let day_of = |s: i64| {
                        norn_expr::temporal::naming_series_date_at(
                            s.checked_mul(1_000).ok_or("oob")?,
                            "UTC",
                        )
                        .map_err(|_err| "oob")
                    };
                    let (y, m, d) = match grain {
                        TimeGrain::Week => {
                            let monday = secs
                                .checked_sub(345_600)
                                .ok_or("oob")?
                                .div_euclid(604_800)
                                .checked_mul(604_800)
                                .and_then(|s| s.checked_add(345_600))
                                .ok_or("oob")?;
                            let dt = day_of(monday)?;
                            (dt.year, dt.month, dt.day)
                        }
                        TimeGrain::Day => {
                            let dt = day_of(secs)?;
                            (dt.year, dt.month, dt.day)
                        }
                        TimeGrain::Month => {
                            let dt = day_of(secs)?;
                            (dt.year, dt.month, 1)
                        }
                        TimeGrain::Quarter => {
                            let dt = day_of(secs)?;
                            let first = match dt.month {
                                1..=3 => 1,
                                4..=6 => 4,
                                7..=9 => 7,
                                _ => 10,
                            };
                            (dt.year, first, 1)
                        }
                        TimeGrain::Year => (day_of(secs)?.year, 1, 1),
                    };
                    Ok(format!("{y:04}-{m:02}-{d:02}"))
                };
                assert_eq!(
                    period_start(seconds, grain).ok(),
                    reference(seconds).ok(),
                    "seconds={seconds} grain={grain:?}"
                );
            }
        }
    }
}
