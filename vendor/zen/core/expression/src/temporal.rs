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
use jiff::civil::{Date, DateTime};
use jiff::{SignedDuration, Span};
use rust_decimal::prelude::ToPrimitive;
use rust_decimal::Decimal;
use serde_json::Value;
use std::any::Any;
use std::cmp::Ordering;
use std::fmt::{Display, Formatter};
use std::rc::Rc;
use thiserror::Error;

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
        let (hour, minute, second) = (
            digits(&b[11..13]),
            digits(&b[14..16]),
            digits(&b[17..19]),
        );
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
        let utc = local
            .checked_sub(SignedDuration::from_secs(i64::from(offset_seconds)))
            .map_err(|_| TemporalError::OutOfRange)?;
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

/// The value of a run of ASCII digits the caller has already checked (at most
/// nine, so the value fits).
fn digits(bytes: &[u8]) -> i32 {
    bytes
        .iter()
        .fold(0, |value, digit| value * 10 + i32::from(digit - b'0'))
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
                write!(f, "{:04}-{:02}-{:02}", date.year(), date.month(), date.day())
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
                    // Avoid i64 nanosecond overflow across the 9999-year range.
                    let ((a_seconds, a_nanos), (b_seconds, b_nanos)) =
                        (epoch_parts(a), epoch_parts(b));
                    let seconds = Decimal::from(b_seconds - a_seconds);
                    let fraction = Decimal::from(b_nanos) - Decimal::from(a_nanos);
                    Ok(Variable::Number(
                        seconds + fraction / Decimal::from(1_000_000_000u32),
                    ))
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
                    let result = dt
                        .checked_add(SignedDuration::from_secs(integer(&args, 1)?))
                        .map_err(|_| TemporalError::OutOfRange)?;
                    anyhow::ensure!(in_range(result.year()), TemporalError::OutOfRange);
                    Ok(Temporal(Kind::Timestamp(result)).into_variable())
                }
                _ => unreachable!("not a deterministic temporal builtin"),
            }
        }),
    })
}
