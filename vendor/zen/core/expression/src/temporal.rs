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
use chrono::{DateTime, Datelike, Days, NaiveDate, SecondsFormat, TimeDelta, Timelike, Utc};
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
    Date(NaiveDate),
    Timestamp(DateTime<Utc>),
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
        let date =
            NaiveDate::parse_from_str(text, "%Y-%m-%d").map_err(|_| TemporalError::InvalidDate)?;
        if !in_range(date.year()) {
            return Err(TemporalError::InvalidDate);
        }
        Ok(Self(Kind::Date(date)))
    }

    pub fn timestamp(text: &str) -> Result<Self, TemporalError> {
        let b = text.as_bytes();
        // Bound lexical checking and chrono work before examining the input.
        if !(20..=35).contains(&b.len()) || b[10] != b'T' || b[13] != b':' || b[16] != b':' {
            return Err(TemporalError::InvalidTimestamp);
        }
        Self::date(&text[..10]).map_err(|_| TemporalError::InvalidTimestamp)?;
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
        let dt = DateTime::parse_from_rfc3339(text).map_err(|_| TemporalError::InvalidTimestamp)?;
        // Chrono represents leap seconds with nanos >= 1e9; this profile refuses them.
        if dt.nanosecond() >= 1_000_000_000 {
            return Err(TemporalError::InvalidTimestamp);
        }
        let utc = dt.with_timezone(&Utc);
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

fn in_range(year: i32) -> bool {
    (1..=9999).contains(&year)
}

impl Display for Temporal {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match &self.0 {
            Kind::Date(date) => write!(f, "{date}"),
            Kind::Timestamp(dt) => f.write_str(&dt.to_rfc3339_opts(SecondsFormat::AutoSi, true)),
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
                        b.signed_duration_since(*a).num_days().into(),
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
                    let seconds = Decimal::from(b.timestamp() - a.timestamp());
                    let fraction = Decimal::from(b.timestamp_subsec_nanos())
                        - Decimal::from(a.timestamp_subsec_nanos());
                    Ok(Variable::Number(
                        seconds + fraction / Decimal::from(1_000_000_000u32),
                    ))
                }
                F::AddDays => {
                    let Kind::Date(date) = &args.dynamic::<Temporal>(0)?.0 else {
                        return Err(TemporalError::MixedKinds.into());
                    };
                    let amount = integer(&args, 1)?;
                    let days = Days::new(amount.unsigned_abs());
                    let result = if amount < 0 {
                        date.checked_sub_days(days)
                    } else {
                        date.checked_add_days(days)
                    }
                    .ok_or(TemporalError::OutOfRange)?;
                    anyhow::ensure!(in_range(result.year()), TemporalError::OutOfRange);
                    Ok(Temporal(Kind::Date(result)).into_variable())
                }
                F::AddSeconds => {
                    let Kind::Timestamp(dt) = &args.dynamic::<Temporal>(0)?.0 else {
                        return Err(TemporalError::MixedKinds.into());
                    };
                    let delta = TimeDelta::try_seconds(integer(&args, 1)?)
                        .ok_or(TemporalError::OutOfRange)?;
                    let result = dt
                        .checked_add_signed(delta)
                        .ok_or(TemporalError::OutOfRange)?;
                    anyhow::ensure!(in_range(result.year()), TemporalError::OutOfRange);
                    Ok(Temporal(Kind::Timestamp(result)).into_variable())
                }
                _ => unreachable!("not a deterministic temporal builtin"),
            }
        }),
    })
}
