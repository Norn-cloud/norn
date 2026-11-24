//! Norn S5: deterministic temporal values, enabled by `deterministic-temporal`.
//!
//! JSON strings are never implicitly converted. Use `date(text)` or
//! `timestamp(text)` in expressions, or these checked constructors at admission.
//! Constructors outside the VM are unmetered and inspect at most 35 bytes.
//! Dates and UTC instants range from year 0001 through 9999 inclusive.

use crate::functions::arguments::Arguments;
use crate::functions::defs::{FunctionDefinition, FunctionSignature, StaticFunction};
use crate::functions::InternalFunction;
use crate::variable::{DynamicVariable, VariableType};
use crate::Variable;
use jiff::civil::{Date, DateTime, Time};
use jiff::tz::{AmbiguousOffset, TimeZone};
use jiff::Span;
use rust_decimal::prelude::ToPrimitive;
use rust_decimal::Decimal;
use serde_json::Value;
use std::any::Any;
use std::cmp::Ordering;
use std::fmt::{Display, Formatter};
use std::rc::Rc;
use thiserror::Error;

/// The one zone name upstream tzdb carries that Norn never admits.
/// Refusing it keeps the accepted IANA identifiers identical to Norn's pinned 597-zone set.
const NOT_A_PINNED_IANA_ZONE: &str = "Factory";

/// Explicit, immutable lookup interface for a versioned offline IANA timezone
/// database bundle injected into a [`crate::vm::VM`] or [`crate::Isolate`] (#1440).
///
/// Lookup must be exact and case-sensitive; no host zoneinfo or global mutable
/// state may be consulted.
pub trait TzdbLookup: Send + Sync + std::fmt::Debug {
    /// Returns raw TZif bytes for exact case-sensitive IANA zone `name`, or
    /// `None` if absent.
    fn tzif(&self, name: &str) -> Option<&[u8]>;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum TemporalError {
    #[error("invalid calendar date; expected YYYY-MM-DD in years 0001..9999")]
    InvalidDate,
    #[error(
        "invalid timestamp; expected RFC3339 with explicit known offset and at most 9 fractional digits"
    )]
    InvalidTimestamp,
    #[error("temporal operands must have the same kind")]
    MixedKinds,
    #[error("temporal arithmetic exceeds years 0001..9999")]
    OutOfRange,
    #[error("invalid temporal interval; expected start < end")]
    InvalidInterval,
    #[error(
        "invalid local timestamp; expected YYYY-MM-DDTHH:MM:SS with optional up to 9 fractional digits and no offset"
    )]
    InvalidLocalTimestamp,
    #[error("unknown IANA timezone; expected a canonical zone in the pinned database")]
    InvalidTimezone,
    #[error("no IANA timezone database configured for expression evaluation")]
    TzdbNotConfigured,
    #[error("invalid DST policy; expected 'reject', 'earlier', or 'later'")]
    InvalidDstPolicy,
    #[error("local wall time does not exist in this timezone (DST spring-forward gap)")]
    NonexistentLocalTime,
    #[error("local wall time is ambiguous in this timezone (DST fall-back fold)")]
    AmbiguousLocalTime,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Kind {
    Date(Date),
    /// The UTC wall-clock reading of the instant. A civil date-time (rather than
    /// `jiff::Timestamp`) spans the whole 0001..9999 UTC range.
    Timestamp(DateTime),
}

/// A checked temporal value. Private storage prevents invalid or mixed-kind states.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Temporal(Kind);

impl Temporal {
    pub fn date(text: &str) -> Result<Self, TemporalError> {
        let b = text.as_bytes();
        if b.len() != 10
            || b[4] != b'-'
            || b[7] != b'-'
            || !b
                .iter()
                .enumerate()
                .all(|(i, c)| i == 4 || i == 7 || c.is_ascii_digit())
        {
            return Err(TemporalError::InvalidDate);
        }
        let date = Date::new(
            digits(&b[0..4]) as i16,
            digits(&b[5..7]) as i8,
            digits(&b[8..10]) as i8,
        )
        .map_err(|_| TemporalError::InvalidDate)?;
        if !in_range(date.year()) {
            return Err(TemporalError::InvalidDate);
        }
        Ok(Self(Kind::Date(date)))
    }

    pub fn timestamp(text: &str) -> Result<Self, TemporalError> {
        let b = text.as_bytes();
        // Bound lexical checking and calendar work before examining the input.
        if !(20..=35).contains(&b.len()) || b[10] != b'T' || b[13] != b':' || b[16] != b':' {
            return Err(TemporalError::InvalidTimestamp);
        }
        let Ok(Self(Kind::Date(date))) = Self::date(&text[..10]) else {
            return Err(TemporalError::InvalidTimestamp);
        };
        if ![11, 12, 14, 15, 17, 18]
            .iter()
            .all(|i| b[*i].is_ascii_digit())
        {
            return Err(TemporalError::InvalidTimestamp);
        }
        let mut zone = 19;
        if b[zone] == b'.' {
            zone += 1;
            let start = zone;
            while zone < b.len() && b[zone].is_ascii_digit() {
                zone += 1;
            }
            if !(1..=9).contains(&(zone - start)) {
                return Err(TemporalError::InvalidTimestamp);
            }
        }
        let offset = &b[zone..];
        let valid_offset = offset == b"Z"
            || (offset.len() == 6
                && matches!(offset[0], b'+' | b'-')
                && offset[3] == b':'
                && [1, 2, 4, 5].iter().all(|i| offset[*i].is_ascii_digit())
                && offset != b"-00:00");
        if !valid_offset {
            return Err(TemporalError::InvalidTimestamp);
        }
        // RFC 3339 field ranges: hour 00-23, minute 00-59 and second 00-59 (second
        // 60, a leap second, is refused), offset hour 00-23 and minute 00-59.
        let (hour, minute, second) = (digits(&b[11..13]), digits(&b[14..16]), digits(&b[17..19]));
        let nanos = if zone > 20 {
            let fraction = &b[20..zone];
            digits(fraction) * 10_i32.pow(9 - fraction.len() as u32)
        } else {
            0
        };
        let offset_seconds = if offset == b"Z" {
            0
        } else {
            let (hours, minutes) = (digits(&offset[1..3]), digits(&offset[4..6]));
            if hours > 23 || minutes > 59 {
                return Err(TemporalError::InvalidTimestamp);
            }
            let seconds = hours * 3600 + minutes * 60;
            if offset[0] == b'-' {
                -seconds
            } else {
                seconds
            }
        };
        if hour > 23 || minute > 59 || second > 59 {
            return Err(TemporalError::InvalidTimestamp);
        }
        let local = date.at(hour as i8, minute as i8, second as i8, nanos);
        let utc = shift(local, -i64::from(offset_seconds)).ok_or(TemporalError::OutOfRange)?;
        if !in_range(utc.year()) {
            return Err(TemporalError::OutOfRange);
        }
        Ok(Self(Kind::Timestamp(utc)))
    }

    pub fn parse_local_datetime(text: &str) -> Result<DateTime, TemporalError> {
        let b = text.as_bytes();
        // YYYY-MM-DDTHH:MM:SS (19 bytes) plus optional .f{1..9} (up to 29 bytes).
        if !(19..=29).contains(&b.len()) || b[10] != b'T' || b[13] != b':' || b[16] != b':' {
            return Err(TemporalError::InvalidLocalTimestamp);
        }
        let Ok(Self(Kind::Date(date))) = Self::date(&text[..10]) else {
            return Err(TemporalError::InvalidLocalTimestamp);
        };
        if ![11, 12, 14, 15, 17, 18]
            .iter()
            .all(|i| b[*i].is_ascii_digit())
        {
            return Err(TemporalError::InvalidLocalTimestamp);
        }
        let nanos = if b.len() > 19 {
            if b[19] != b'.' {
                return Err(TemporalError::InvalidLocalTimestamp);
            }
            let fraction = &b[20..];
            if !(1..=9).contains(&fraction.len()) || !fraction.iter().all(u8::is_ascii_digit) {
                return Err(TemporalError::InvalidLocalTimestamp);
            }
            digits(fraction) * 10_i32.pow(9 - fraction.len() as u32)
        } else {
            0
        };
        let (hour, minute, second) = (digits(&b[11..13]), digits(&b[14..16]), digits(&b[17..19]));
        if hour > 23 || minute > 59 || second > 59 {
            return Err(TemporalError::InvalidLocalTimestamp);
        }
        Ok(date.at(hour as i8, minute as i8, second as i8, nanos))
    }

    pub fn resolve_local_timestamp(
        local_wall_time: &str,
        iana_tz: &str,
        dst_policy: &str,
        tzdb: &dyn TzdbLookup,
    ) -> Result<Self, TemporalError> {
        let local = Self::parse_local_datetime(local_wall_time)?;
        let tz = pinned_timezone(iana_tz, tzdb)?;
        if !matches!(dst_policy, "reject" | "earlier" | "later") {
            return Err(TemporalError::InvalidDstPolicy);
        }
        let offset = match tz.to_ambiguous_timestamp(local).offset() {
            AmbiguousOffset::Unambiguous { offset } => offset,
            AmbiguousOffset::Gap { .. } => return Err(TemporalError::NonexistentLocalTime),
            AmbiguousOffset::Fold { before, after } => match dst_policy {
                "reject" => return Err(TemporalError::AmbiguousLocalTime),
                "earlier" => before,
                "later" => after,
                _ => return Err(TemporalError::InvalidDstPolicy),
            },
        };
        let utc = shift(local, -i64::from(offset.seconds())).ok_or(TemporalError::OutOfRange)?;
        if !in_range(utc.year()) {
            return Err(TemporalError::OutOfRange);
        }
        Ok(Self(Kind::Timestamp(utc)))
    }

    pub fn into_variable(self) -> Variable {
        Variable::Dynamic(Rc::new(self))
    }

    pub(crate) fn compare(&self, other: &Self) -> Result<Ordering, TemporalError> {
        match (&self.0, &other.0) {
            (Kind::Date(a), Kind::Date(b)) => Ok(a.cmp(b)),
            (Kind::Timestamp(a), Kind::Timestamp(b)) => Ok(a.cmp(b)),
            _ => Err(TemporalError::MixedKinds),
        }
    }
}

fn in_range(year: i16) -> bool {
    (1..=9999).contains(&year)
}

fn pinned_timezone(name: &str, tzdb: &dyn TzdbLookup) -> Result<TimeZone, TemporalError> {
    if name == NOT_A_PINNED_IANA_ZONE {
        return Err(TemporalError::InvalidTimezone);
    }
    match tzdb.tzif(name) {
        Some(data) => TimeZone::tzif(name, data).map_err(|_| TemporalError::InvalidTimezone),
        None => Err(TemporalError::InvalidTimezone),
    }
}

/// Evaluates `resolve_local_timestamp` using the VM's explicit [`TzdbLookup`].
pub(crate) fn call_resolve_local_timestamp(
    args: Arguments<'_>,
    tzdb: Option<&dyn TzdbLookup>,
) -> anyhow::Result<Variable> {
    anyhow::ensure!(args.len() == 3, "expected 3 temporal arguments");
    let tzdb = tzdb.ok_or(TemporalError::TzdbNotConfigured)?;
    Ok(
        Temporal::resolve_local_timestamp(args.str(0)?, args.str(1)?, args.str(2)?, tzdb)?
            .into_variable(),
    )
}

fn diff_seconds(a: &DateTime, b: &DateTime) -> Decimal {
    // Avoid i64 nanosecond overflow across the 9999-year range.
    let ((a_seconds, a_nanos), (b_seconds, b_nanos)) = (epoch_parts(a), epoch_parts(b));
    let seconds = Decimal::from(b_seconds - a_seconds);
    let fraction = Decimal::from(b_nanos) - Decimal::from(a_nanos);
    seconds + fraction / Decimal::from(1_000_000_000u32)
}

fn proper_interval<'a>(
    args: &'a Arguments<'a>,
    start_pos: usize,
    end_pos: usize,
) -> anyhow::Result<(&'a DateTime, &'a DateTime)> {
    let (Kind::Timestamp(start), Kind::Timestamp(end)) = (
        &args.dynamic::<Temporal>(start_pos)?.0,
        &args.dynamic::<Temporal>(end_pos)?.0,
    ) else {
        return Err(TemporalError::MixedKinds.into());
    };
    if start >= end {
        return Err(TemporalError::InvalidInterval.into());
    }
    Ok((start, end))
}

/// The value of a run of ASCII digits the caller has already checked (at most
/// nine, so the value fits).
fn digits(bytes: &[u8]) -> i32 {
    bytes
        .iter()
        .fold(0, |value, digit| value * 10 + i32::from(digit - b'0'))
}

/// `reading` moved by `seconds`, using integer seconds-of-day and whole-day
/// date arithmetic only: exact over the whole civil range, where jiff's civil
/// time arithmetic refuses results near its upper end and long durations.
fn shift(reading: DateTime, seconds: i64) -> Option<DateTime> {
    let of_day = i64::from(reading.hour()) * 3_600
        + i64::from(reading.minute()) * 60
        + i64::from(reading.second());
    let of_day = of_day.checked_add(seconds)?;
    let days = Span::new().try_days(of_day.div_euclid(86_400)).ok()?;
    let of_day = of_day.rem_euclid(86_400);
    let time = Time::new(
        (of_day / 3_600) as i8,
        (of_day % 3_600 / 60) as i8,
        (of_day % 60) as i8,
        reading.subsec_nanosecond(),
    )
    .ok()?;
    Some(reading.date().checked_add(days).ok()?.to_datetime(time))
}

/// Whole seconds since the Unix epoch (floored) and the nanosecond of the
/// second, of a UTC wall-clock reading.
fn epoch_parts(utc: &DateTime) -> (i64, i32) {
    let since = utc.duration_since(DateTime::constant(1970, 1, 1, 0, 0, 0, 0));
    let floor = since.as_secs() - i64::from(since.subsec_nanos() < 0);
    (floor, utc.subsec_nanosecond())
}

/// RFC 3339 with `Z` and the shortest of 0, 3, 6 or 9 fractional digits that is
/// exact.
fn write_utc(f: &mut Formatter<'_>, utc: &DateTime) -> std::fmt::Result {
    write!(
        f,
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}",
        utc.year(),
        utc.month(),
        utc.day(),
        utc.hour(),
        utc.minute(),
        utc.second()
    )?;
    let nanos = utc.subsec_nanosecond();
    if nanos == 0 {
    } else if nanos % 1_000_000 == 0 {
        write!(f, ".{:03}", nanos / 1_000_000)?;
    } else if nanos % 1_000 == 0 {
        write!(f, ".{:06}", nanos / 1_000)?;
    } else {
        write!(f, ".{nanos:09}")?;
    }
    f.write_str("Z")
}

impl Display for Temporal {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match &self.0 {
            Kind::Date(date) => {
                write!(
                    f,
                    "{:04}-{:02}-{:02}",
                    date.year(),
                    date.month(),
                    date.day()
                )
            }
            Kind::Timestamp(utc) => write_utc(f, utc),
        }
    }
}

impl DynamicVariable for Temporal {
    fn type_name(&self) -> &'static str {
        match self.0 {
            Kind::Date(_) => "date",
            Kind::Timestamp(_) => "timestamp",
        }
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
    fn to_value(&self) -> Value {
        Value::String(self.to_string())
    }
}

fn integer(args: &Arguments, pos: usize) -> anyhow::Result<i64> {
    let n = args.number(pos)?;
    anyhow::ensure!(n.fract().is_zero(), "temporal amount must be an integer");
    n.to_i64().ok_or_else(|| TemporalError::OutOfRange.into())
}

pub(crate) fn definition(kind: InternalFunction) -> Rc<dyn FunctionDefinition> {
    use InternalFunction as F;
    use VariableType as T;
    let (parameters, return_type) = match kind {
        F::CalendarDate => (vec![T::String], T::Date),
        F::Timestamp => (vec![T::String], T::Timestamp),
        F::DaysBetween => (vec![T::Date, T::Date], T::Number),
        F::AddDays => (vec![T::Date, T::Number], T::Date),
        F::SecondsBetween => (vec![T::Timestamp, T::Timestamp], T::Number),
        F::AddSeconds => (vec![T::Timestamp, T::Number], T::Timestamp),
        F::IntervalsOverlap => (
            vec![T::Timestamp, T::Timestamp, T::Timestamp, T::Timestamp],
            T::Bool,
        ),
        F::IntervalGapSeconds | F::IntervalIntersectionSeconds => (
            vec![T::Timestamp, T::Timestamp, T::Timestamp, T::Timestamp],
            T::Number,
        ),
        F::ResolveLocalTimestamp => (vec![T::String, T::String, T::String], T::Timestamp),
        _ => unreachable!("not a deterministic temporal builtin"),
    };
    let arity = parameters.len();
    Rc::new(StaticFunction {
        signature: FunctionSignature {
            parameters,
            return_type,
        },
        implementation: Rc::new(move |args| {
            anyhow::ensure!(args.len() == arity, "expected {arity} temporal arguments");
            match kind {
                F::CalendarDate => Ok(Temporal::date(args.str(0)?)?.into_variable()),
                F::Timestamp => Ok(Temporal::timestamp(args.str(0)?)?.into_variable()),
                F::DaysBetween => {
                    let (Kind::Date(a), Kind::Date(b)) = (
                        &args.dynamic::<Temporal>(0)?.0,
                        &args.dynamic::<Temporal>(1)?.0,
                    ) else {
                        return Err(TemporalError::MixedKinds.into());
                    };
                    Ok(Variable::Number(
                        (b.duration_since(*a).as_secs() / 86_400).into(),
                    ))
                }
                F::SecondsBetween => {
                    let (Kind::Timestamp(a), Kind::Timestamp(b)) = (
                        &args.dynamic::<Temporal>(0)?.0,
                        &args.dynamic::<Temporal>(1)?.0,
                    ) else {
                        return Err(TemporalError::MixedKinds.into());
                    };
                    Ok(Variable::Number(diff_seconds(a, b)))
                }
                F::AddDays => {
                    let Kind::Date(date) = &args.dynamic::<Temporal>(0)?.0 else {
                        return Err(TemporalError::MixedKinds.into());
                    };
                    let amount = integer(&args, 1)?;
                    let result = Span::new()
                        .try_days(amount)
                        .and_then(|days| date.checked_add(days))
                        .map_err(|_| TemporalError::OutOfRange)?;
                    anyhow::ensure!(in_range(result.year()), TemporalError::OutOfRange);
                    Ok(Temporal(Kind::Date(result)).into_variable())
                }
                F::AddSeconds => {
                    let Kind::Timestamp(dt) = &args.dynamic::<Temporal>(0)?.0 else {
                        return Err(TemporalError::MixedKinds.into());
                    };
                    let result = shift(*dt, integer(&args, 1)?).ok_or(TemporalError::OutOfRange)?;
                    anyhow::ensure!(in_range(result.year()), TemporalError::OutOfRange);
                    Ok(Temporal(Kind::Timestamp(result)).into_variable())
                }
                F::IntervalsOverlap => {
                    let (a_start, a_end) = proper_interval(&args, 0, 1)?;
                    let (b_start, b_end) = proper_interval(&args, 2, 3)?;
                    Ok(Variable::Bool(a_start < b_end && b_start < a_end))
                }
                F::IntervalGapSeconds => {
                    let (a_start, a_end) = proper_interval(&args, 0, 1)?;
                    let (b_start, b_end) = proper_interval(&args, 2, 3)?;
                    let gap = if a_end < b_start {
                        diff_seconds(a_end, b_start)
                    } else if b_end < a_start {
                        diff_seconds(b_end, a_start)
                    } else {
                        Decimal::ZERO
                    };
                    Ok(Variable::Number(gap))
                }
                F::IntervalIntersectionSeconds => {
                    let (a_start, a_end) = proper_interval(&args, 0, 1)?;
                    let (b_start, b_end) = proper_interval(&args, 2, 3)?;
                    let start = a_start.max(b_start);
                    let end = a_end.min(b_end);
                    let overlap = if start < end {
                        diff_seconds(start, end)
                    } else {
                        Decimal::ZERO
                    };
                    Ok(Variable::Number(overlap))
                }
                F::ResolveLocalTimestamp => {
                    // VM dispatches ResolveLocalTimestamp directly with its explicit TzdbLookup.
                    // Any direct call outside the VM fails closed rather than reading ambient state.
                    Err(TemporalError::TzdbNotConfigured.into())
                }
                _ => unreachable!("not a deterministic temporal builtin"),
            }
        }),
    })
}
