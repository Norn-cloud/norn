//! D137: the adapter's stored values. A number is exact in the storage domain
//! `DECIMAL(38,s)` (`s <= 37`); an instant keeps its original text beside its
//! signed UTC seconds and nanoseconds (an `i64` of nanoseconds cannot span
//! the years 0000–9999). Every conversion here is exact or refused.

use std::cmp::Ordering;

use super::super::{ColumnKind, Value};

/// The storage domain's digit bound: `|m| < 10^38`.
pub(super) const STORAGE_DIGITS: u32 = 38;
/// The largest storage scale.
pub(super) const MAX_STORAGE_SCALE: u32 = 37;
/// The bulk domain's largest mantissa (`2^96 - 1`): the per-row evaluator's.
pub(super) const EXACT_MANTISSA: i128 = 79_228_162_514_264_337_593_543_950_335;
/// The question domain's largest coefficient, in millionths.
pub(super) const MAX_COEFFICIENT: i128 = 9_000_000_000_000_000_000;
/// 0000-01-01T00:00:00Z and 9999-12-31T23:59:59Z, in UTC seconds.
pub(super) const FIRST_SECOND: i64 = -62_167_219_200;
pub(super) const LAST_SECOND: i64 = 253_402_300_799;
const DAY: i64 = 86_400;

/// `10^n`, when it fits.
pub(super) fn pow10(n: u32) -> Option<i128> {
    10_i128.checked_pow(n)
}

/// An exact decimal `m * 10^-s` in the storage domain.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Dec {
    /// The mantissa, `|m| < 10^38`.
    pub m: i128,
    /// The scale.
    pub s: u32,
}

/// Why a text is not a decimal of a column's scale.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum DecError {
    /// Not a plain signed decimal.
    Shape,
    /// More fractional digits than the scale.
    Scale,
    /// More than the storage domain's digits.
    Range,
}

impl Dec {
    /// `text` exactly at `scale`: an optional sign, digits, and an optional
    /// fraction of at most `scale` digits, with digits on both sides of a point.
    ///
    /// # Errors
    /// The [`DecError`] that refuses it.
    pub(super) fn parse_at(text: &str, scale: u32) -> Result<Self, DecError> {
        let (negative, body) = match text.as_bytes().first() {
            Some(b'-') => (true, text.get(1..).unwrap_or_default()),
            Some(b'+') => (false, text.get(1..).unwrap_or_default()),
            _ => (false, text),
        };
        let (whole, fraction) = body.split_once('.').unwrap_or((body, ""));
        let digits = |part: &str| part.bytes().all(|b| b.is_ascii_digit());
        if whole.is_empty()
            || !digits(whole)
            || !digits(fraction)
            || (body.contains('.') && fraction.is_empty())
        {
            return Err(DecError::Shape);
        }
        if u32::try_from(fraction.len()).map_or(true, |len| len > scale) {
            return Err(DecError::Scale);
        }
        let mut m: i128 = 0;
        let padded = fraction.len()..usize::try_from(scale).map_err(|_e| DecError::Range)?;
        for b in whole
            .bytes()
            .chain(fraction.bytes())
            .chain(padded.map(|_| b'0'))
        {
            m = m
                .checked_mul(10)
                .and_then(|m| m.checked_add(i128::from(b.wrapping_sub(b'0'))))
                .ok_or(DecError::Range)?;
        }
        let bound = pow10(STORAGE_DIGITS).ok_or(DecError::Range)?;
        if m >= bound {
            return Err(DecError::Range);
        }
        Ok(Self {
            m: if negative { m.saturating_neg() } else { m },
            s: scale,
        })
    }

    /// `CAST(text AS DECIMAL(38,scale))`: a canonical number rounded half
    /// away from zero to the scale, as the storage cast rounds.
    pub(super) fn cast(text: &str, scale: u32) -> Option<Self> {
        let decimal = norn_ir::Decimal::parse(text).ok()?;
        let canonical = decimal.canonical();
        let (negative, body) = canonical
            .strip_prefix('-')
            .map_or((false, canonical.as_str()), |rest| (true, rest));
        let (whole, fraction) = body.split_once('.').unwrap_or((body, ""));
        let keep = usize::try_from(scale).ok()?;
        let (kept, dropped) = fraction.split_at(fraction.len().min(keep));
        let up = dropped.as_bytes().first().is_some_and(|d| *d >= b'5');
        let mut digits = format!("{whole}.{kept}");
        if kept.is_empty() {
            digits.pop();
        }
        let mut value = Self::parse_at(&digits, scale).ok()?;
        if up {
            value.m = value.m.checked_add(1)?;
        }
        if negative {
            value.m = value.m.saturating_neg();
        }
        (value.m.unsigned_abs() < pow10(STORAGE_DIGITS)?.unsigned_abs()).then_some(value)
    }

    /// The text `CAST(x AS VARCHAR)` gives: every digit of the scale.
    pub(super) fn text(self) -> String {
        let digits = self.m.unsigned_abs().to_string();
        let sign = if self.m < 0 { "-" } else { "" };
        let scale = usize::try_from(self.s).unwrap_or(0);
        if scale == 0 {
            return format!("{sign}{digits}");
        }
        let padded = format!("{digits:0>width$}", width = scale.saturating_add(1));
        let (whole, fraction) = padded
            .split_at_checked(padded.len().saturating_sub(scale))
            .unwrap_or((&padded, ""));
        format!("{sign}{whole}.{fraction}")
    }

    /// The canonical decimal: no trailing fractional zeros, no `-0`.
    pub(super) fn canonical(self) -> String {
        let text = self.text();
        let trimmed = if text.contains('.') {
            text.trim_end_matches('0').trim_end_matches('.').to_owned()
        } else {
            text
        };
        if trimmed == "-0" {
            "0".to_owned()
        } else {
            trimmed
        }
    }

    /// The same value at `scale`, when exact and in the domain.
    pub(super) fn rescale(self, scale: u32) -> Option<Self> {
        match scale.cmp(&self.s) {
            Ordering::Equal => Some(self),
            Ordering::Greater => Some(Self {
                m: self.m.checked_mul(pow10(scale.checked_sub(self.s)?)?)?,
                s: scale,
            }),
            Ordering::Less => {
                let factor = pow10(self.s.checked_sub(scale)?)?;
                (self.m.checked_rem(factor)? == 0).then(|| Self {
                    m: self.m.checked_div(factor).unwrap_or(0),
                    s: scale,
                })
            }
        }
    }

    /// Exact order across scales.
    pub(super) fn compare(self, other: Self) -> Ordering {
        let split = |d: Self| {
            let factor = pow10(d.s).unwrap_or(1);
            (d.m.div_euclid(factor), d.m.rem_euclid(factor), d.s)
        };
        let ((a_whole, a_frac, a_s), (b_whole, b_frac, b_s)) = (split(self), split(other));
        a_whole.cmp(&b_whole).then_with(|| {
            let scale = a_s.max(b_s);
            let lift = |frac: i128, s: u32| {
                frac.checked_mul(pow10(scale.saturating_sub(s)).unwrap_or(1))
                    .unwrap_or(i128::MAX)
            };
            lift(a_frac, a_s).cmp(&lift(b_frac, b_s))
        })
    }

    /// `|self| <= 2^96 - 1` at its own scale: the bulk domain's bound.
    pub(super) const fn in_bulk_range(self) -> bool {
        self.m.unsigned_abs() <= EXACT_MANTISSA.unsigned_abs()
    }

    /// Exact `self + other`, at the larger scale.
    pub(super) fn checked_add(self, other: Self) -> Option<Self> {
        let scale = self.s.max(other.s);
        let (a, b) = (self.rescale(scale)?, other.rescale(scale)?);
        Some(Self {
            m: a.m.checked_add(b.m)?,
            s: scale,
        })
        .filter(Self::in_storage)
    }

    /// Exact `self * other`, at the summed scale.
    pub(super) fn checked_mul(self, other: Self) -> Option<Self> {
        Some(Self {
            m: self.m.checked_mul(other.m)?,
            s: self.s.checked_add(other.s)?,
        })
        .filter(|d| d.s <= MAX_STORAGE_SCALE && d.in_storage())
    }

    pub(super) fn in_storage(&self) -> bool {
        pow10(STORAGE_DIGITS).is_some_and(|bound| self.m.unsigned_abs() < bound.unsigned_abs())
    }

    /// The question domain's coefficient: exact millionths, `|c| <= 9e18`.
    pub(super) fn millionths(self) -> Option<i64> {
        let six = self.rescale(6)?;
        (six.m.unsigned_abs() <= MAX_COEFFICIENT.unsigned_abs())
            .then(|| i64::try_from(six.m).ok())
            .flatten()
    }
}

/// The bulk domain (S4): a canonical decimal with at most 28 fractional
/// digits and a mantissa of at most `2^96 - 1`.
pub(super) fn in_bulk_domain(value: Dec) -> bool {
    let mut mag = value.m.unsigned_abs();
    if mag == 0 {
        return true;
    }
    let mut scale = value.s;
    while scale > 0 && mag.rem_euclid(10) == 0 {
        mag = mag.div_euclid(10);
        scale = scale.saturating_sub(1);
    }
    scale <= 28 && mag <= EXACT_MANTISSA.unsigned_abs()
}

/// One stored value of a column.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Cell {
    /// Absent.
    Null,
    /// Text, an enum value, or canonical JSON.
    Text(String),
    /// A boolean.
    Bool(bool),
    /// A number in the storage domain.
    Dec(Dec),
    /// A timestamp or date: its text and its UTC ordering key.
    Instant {
        /// The original text.
        text: String,
        /// UTC seconds.
        t: i64,
        /// Nanoseconds within the second.
        n: i32,
    },
}

impl Cell {
    /// Whether the value is present.
    pub(super) const fn is_present(&self) -> bool {
        !matches!(self, Self::Null)
    }

    /// The cell a mirrored value is stored as in a column of `kind`.
    ///
    /// # Errors
    /// A number column's value that is not a decimal of the storage domain:
    /// the storage cast fails, so the sync refuses (B5), as `DuckDB`'s does.
    pub(super) fn of_value(kind: ColumnKind, value: &Value) -> Result<Self, String> {
        let text = match value {
            Value::Null => return Ok(Self::Null),
            Value::Bool(flag) => flag.to_string(),
            Value::Number(text)
            | Value::Text(text)
            | Value::Json(text)
            | Value::Instant { text, .. } => text.clone(),
        };
        let number = |scale: u32| {
            Dec::cast(&text, scale)
                .map(Self::Dec)
                .ok_or_else(|| format!("{text:?} is not a decimal of scale {scale}"))
        };
        Ok(match (kind, value) {
            (ColumnKind::Integer, _) => number(0)?,
            (ColumnKind::Decimal { scale }, _) => number(scale)?,
            (ColumnKind::Boolean, Value::Bool(flag)) => Self::Bool(*flag),
            (ColumnKind::Boolean, _) => Self::Bool(text.to_lowercase() == "true"),
            (_, Value::Instant { secs, nanos, .. }) => Self::Instant {
                text,
                t: *secs,
                n: i32::try_from(*nanos).unwrap_or(i32::MAX),
            },
            (
                ColumnKind::Text | ColumnKind::Timestamp | ColumnKind::Date | ColumnKind::Opaque,
                Value::Null | Value::Bool(_) | Value::Number(_) | Value::Text(_) | Value::Json(_),
            ) => Self::Text(text),
        })
    }

    /// `CAST(x AS VARCHAR)`.
    pub(super) fn text(&self) -> Option<String> {
        match self {
            Self::Null => None,
            Self::Text(text) | Self::Instant { text, .. } => Some(text.clone()),
            Self::Bool(flag) => Some(flag.to_string()),
            Self::Dec(value) => Some(value.text()),
        }
    }

    /// The text, by reference, of a text or instant cell.
    pub(super) fn as_str(&self) -> Option<&str> {
        match self {
            Self::Text(text) | Self::Instant { text, .. } => Some(text),
            Self::Null | Self::Bool(_) | Self::Dec(_) => None,
        }
    }

    /// The ordering key of an instant.
    pub(super) const fn instant(&self) -> Option<(i64, i32)> {
        match self {
            Self::Instant { t, n, .. } => Some((*t, *n)),
            Self::Null | Self::Text(_) | Self::Bool(_) | Self::Dec(_) => None,
        }
    }

    /// The value a reader is answered, as a column of `kind` presents it.
    pub(super) fn present(&self, kind: ColumnKind) -> Option<Value> {
        let text = self.text()?;
        Some(match kind {
            ColumnKind::Integer | ColumnKind::Decimal { .. } => {
                Value::Number(norn_ir::Decimal::parse(&text).map_or(text, |d| d.canonical()))
            }
            ColumnKind::Timestamp | ColumnKind::Date => {
                let (secs, nanos) = self.instant()?;
                Value::Instant {
                    text,
                    secs,
                    nanos: u32::try_from(nanos).ok()?,
                }
            }
            ColumnKind::Boolean => Value::Bool(text == "true"),
            ColumnKind::Text => Value::Text(text),
            ColumnKind::Opaque => Value::Json(text),
        })
    }

    /// The order of two present cells of one column: text by bytes,
    /// numbers exactly, Booleans false first, instants by their key.
    pub(super) fn order(&self, other: &Self) -> Ordering {
        match (self, other) {
            (Self::Dec(a), Self::Dec(b)) => a.compare(*b),
            (Self::Bool(a), Self::Bool(b)) => a.cmp(b),
            (Self::Instant { t, n, .. }, Self::Instant { t: u, n: m, .. }) => (t, n).cmp(&(u, m)),
            (Self::Text(a), Self::Text(b)) => a.as_bytes().cmp(b.as_bytes()),
            (Self::Null, Self::Null) => Ordering::Equal,
            (Self::Null, _) => Ordering::Less,
            (_, Self::Null) => Ordering::Greater,
            (a, b) => a.text().cmp(&b.text()),
        }
    }
}

/// Days since 1970-01-01 of a proleptic Gregorian date (Hinnant's
/// algorithm, in saturating arithmetic: every year here is 0000–9999).
pub(super) fn days_from_civil(year: i64, month: u32, day: u32) -> i64 {
    let year = if month <= 2 {
        year.saturating_sub(1)
    } else {
        year
    };
    let era = year.div_euclid(400);
    let yoe = year.rem_euclid(400);
    let month = i64::from(month);
    let shifted = if month > 2 {
        month.saturating_sub(3)
    } else {
        month.saturating_add(9)
    };
    let doy = shifted
        .saturating_mul(153)
        .saturating_add(2)
        .div_euclid(5)
        .saturating_add(i64::from(day))
        .saturating_sub(1);
    let doe = yoe
        .saturating_mul(365)
        .saturating_add(yoe.div_euclid(4))
        .saturating_sub(yoe.div_euclid(100))
        .saturating_add(doy);
    era.saturating_mul(146_097)
        .saturating_add(doe)
        .saturating_sub(719_468)
}

/// The proleptic Gregorian date of a day since 1970-01-01.
pub(super) fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days.saturating_add(719_468);
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = doe
        .saturating_sub(doe.div_euclid(1460))
        .saturating_add(doe.div_euclid(36_524))
        .saturating_sub(doe.div_euclid(146_096))
        .div_euclid(365);
    let doy = doe.saturating_sub(
        yoe.saturating_mul(365)
            .saturating_add(yoe.div_euclid(4))
            .saturating_sub(yoe.div_euclid(100)),
    );
    let mp = doy.saturating_mul(5).saturating_add(2).div_euclid(153);
    let day = doy
        .saturating_sub(mp.saturating_mul(153).saturating_add(2).div_euclid(5))
        .saturating_add(1);
    let month = if mp < 10 {
        mp.saturating_add(3)
    } else {
        mp.saturating_sub(9)
    };
    let year = yoe
        .saturating_add(era.saturating_mul(400))
        .saturating_add(i64::from(month <= 2));
    (
        year,
        u32::try_from(month).unwrap_or(1),
        u32::try_from(day).unwrap_or(1),
    )
}

/// Whether `year-month-day` is a real date.
fn valid_date(year: i64, month: u32, day: u32) -> bool {
    (1..=12).contains(&month)
        && day >= 1
        && civil_from_days(days_from_civil(year, month, day)) == (year, month, day)
}

/// The first day of the UTC period of `grain` holding the instant second `t`.
pub(super) fn period_start(t: i64, grain: super::super::Grain) -> i64 {
    use super::super::Grain;
    let day = t.div_euclid(DAY);
    let (year, month, _) = civil_from_days(day);
    match grain {
        Grain::Day => day,
        // ISO weeks start on Monday; 1970-01-01 was a Thursday.
        Grain::Week => day.saturating_sub(day.saturating_add(3).rem_euclid(7)),
        Grain::Month => days_from_civil(year, month, 1),
        Grain::Quarter => days_from_civil(
            year,
            month
                .saturating_sub(1)
                .div_euclid(3)
                .saturating_mul(3)
                .saturating_add(1),
            1,
        ),
        Grain::Year => days_from_civil(year, 1, 1),
    }
}

fn two(text: &str, at: usize) -> Option<u32> {
    let part = text.get(at..at.checked_add(2)?)?;
    part.bytes()
        .all(|b| b.is_ascii_digit())
        .then(|| part.parse().ok())?
}

/// A `YYYY-MM-DD` date's UTC midnight, as a load types it.
pub(super) fn parse_date(text: &str) -> Option<i64> {
    let bytes = text.as_bytes();
    if bytes.len() != 10
        || bytes.get(4) != Some(&b'-')
        || bytes.get(7) != Some(&b'-')
        || !bytes
            .iter()
            .enumerate()
            .all(|(i, b)| i == 4 || i == 7 || b.is_ascii_digit())
    {
        return None;
    }
    let year: i64 = text.get(..4)?.parse().ok()?;
    let (month, day) = (two(text, 5)?, two(text, 8)?);
    valid_date(year, month, day).then(|| days_from_civil(year, month, day).saturating_mul(DAY))
}

/// An RFC 3339 timestamp as the cell reads it: `YYYY-MM-DDTHH:MM:SS`, an
/// optional `.` and 1–9 digits, then `Z` or `±HH:MM` other than `-00:00`,
/// whose UTC second lies in the years 0000–9999.
pub(super) fn parse_timestamp(text: &str) -> Option<(i64, i32)> {
    let length = text.len();
    if !(20..=35).contains(&length) || !text.is_ascii() {
        return None;
    }
    let zone = if text.ends_with('Z') {
        "Z"
    } else {
        let zone = text.get(length.checked_sub(6)?..)?;
        let z = zone.as_bytes();
        let shaped = matches!(z.first(), Some(b'+' | b'-'))
            && z.get(3) == Some(&b':')
            && two(zone, 1).is_some_and(|h| h <= 23)
            && two(zone, 4).is_some_and(|m| m <= 59)
            && zone != "-00:00";
        if !shaped {
            return None;
        }
        zone
    };
    let head = text.get(..19)?;
    let h = head.as_bytes();
    let shaped = h.iter().enumerate().all(|(i, b)| match i {
        4 | 7 => *b == b'-',
        10 => *b == b'T',
        13 | 16 => *b == b':',
        _ => b.is_ascii_digit(),
    });
    let (hour, minute, second) = (two(head, 11)?, two(head, 14)?, two(head, 17)?);
    if !shaped || hour > 23 || minute > 59 || second > 59 {
        return None;
    }
    let fraction = text.get(19..length.checked_sub(zone.len())?)?;
    let nanos = if fraction.is_empty() {
        0
    } else {
        let digits = fraction.strip_prefix('.')?;
        if digits.is_empty() || digits.len() > 9 || !digits.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        format!("{digits:0<9}").parse::<i32>().ok()?
    };
    let year: i64 = head.get(..4)?.parse().ok()?;
    let (month, day) = (two(head, 5)?, two(head, 8)?);
    if !valid_date(year, month, day) {
        return None;
    }
    let offset = if zone == "Z" {
        0
    } else {
        let span = i64::from(two(zone, 1)?)
            .saturating_mul(3600)
            .saturating_add(i64::from(two(zone, 4)?).saturating_mul(60));
        if zone.starts_with('-') {
            span.saturating_neg()
        } else {
            span
        }
    };
    let local = days_from_civil(year, month, day)
        .saturating_mul(DAY)
        .saturating_add(i64::from(hour).saturating_mul(3600))
        .saturating_add(i64::from(minute).saturating_mul(60))
        .saturating_add(i64::from(second));
    let utc = local.saturating_sub(offset);
    (FIRST_SECOND..=LAST_SECOND)
        .contains(&utc)
        .then_some((utc, nanos))
}

/// `YYYY-MM-DDTHH:MM:SS` of a UTC second.
pub(super) fn format_second(secs: i64) -> String {
    let (year, month, day) = civil_from_days(secs.div_euclid(DAY));
    let within = secs.rem_euclid(DAY);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}",
        within.div_euclid(3600),
        within.rem_euclid(3600).div_euclid(60),
        within.rem_euclid(60)
    )
}

/// `YYYY-MM-DD` of a day since the epoch.
pub(super) fn format_day(days: i64) -> String {
    let (year, month, day) = civil_from_days(days);
    format!("{year:04}-{month:02}-{day:02}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decimals_parse_and_print_in_the_storage_domain() {
        assert_eq!(
            Dec::parse_at("1.5", 2).map(Dec::text),
            Ok("1.50".to_owned())
        );
        assert_eq!(
            Dec::parse_at("-0.5", 1).map(Dec::text),
            Ok("-0.5".to_owned())
        );
        assert_eq!(Dec::parse_at("1.555", 2), Err(DecError::Scale));
        assert_eq!(Dec::parse_at("1.", 2), Err(DecError::Shape));
        assert_eq!(Dec::parse_at(&"9".repeat(39), 0), Err(DecError::Range));
        assert_eq!(
            Dec::cast("1.235", 2).map(Dec::text),
            Some("1.24".to_owned())
        );
        assert_eq!(
            Dec::cast("-1.235", 2).map(Dec::text),
            Some("-1.24".to_owned())
        );
        assert_eq!(
            Dec::parse_at("1.50", 2).map(Dec::canonical),
            Ok("1.5".to_owned())
        );
    }

    #[test]
    fn instants_keep_the_cells_grammar_and_calendar() {
        assert_eq!(parse_timestamp("1970-01-01T00:00:00Z"), Some((0, 0)));
        assert_eq!(
            parse_timestamp("1969-12-31T23:59:59.5Z"),
            Some((-1, 500_000_000))
        );
        assert_eq!(
            parse_timestamp("2024-01-01T02:00:00+02:00"),
            parse_timestamp("2024-01-01T00:00:00Z")
        );
        assert_eq!(parse_timestamp("2024-01-01T00:00:00-00:00"), None);
        assert_eq!(parse_timestamp("2023-02-29T00:00:00Z"), None);
        assert_eq!(
            parse_timestamp("0000-01-01T00:00:00Z"),
            Some((FIRST_SECOND, 0))
        );
        assert_eq!(
            parse_date("2024-02-29"),
            Some(days_from_civil(2024, 2, 29) * DAY)
        );
        assert_eq!(period_start(0, super::super::super::Grain::Week), -3);
        assert_eq!(format_second(-1), "1969-12-31T23:59:59");
    }
}
