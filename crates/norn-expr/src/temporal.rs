//! Pure validation and canonicalization for semantic temporal values.
//!
//! Calendar dates use the `EntityIR` `YYYY-MM-DD` spelling and the proleptic
//! Gregorian calendar. Timestamps use the admitted RFC 3339 subset: uppercase
//! `T`/`Z`, explicit known offset, seconds from 00 through 59, and zero to
//! nine fractional-second digits. Timestamp instants are formatted in UTC;
//! subsecond precision is preserved to nanoseconds. Naming-series calendar
//! rendering uses pinned IANA timezone data and an explicitly supplied commit
//! instant; no clock or locale is consulted.

use std::fmt;
use std::ops::Range;

use jiff::civil::{Date, DateTime, Time};
use jiff::tz::{AmbiguousOffset, TimeZone};
use jiff::{Span, Timestamp};

/// IANA timezone database release of the exact-pinned `jiff-tzdb` data (the
/// only timezone data used here), for naming-series allocation and local
/// timestamp resolution provenance.
/// It is the release `chrono-tz` 0.10.4 had; moving it is #1440.
pub const NAMING_SERIES_TZDB_VERSION: &str = "2025b";

/// The one zone name `jiff-tzdb` 0.1.4 carries that `chrono-tz` 0.10.4 did
/// not (tzdb's `factory` file). Refusing it keeps the accepted names exactly
/// `chrono-tz` 0.10.4's 597.
const NOT_A_CHRONO_TZ_ZONE: &str = "Factory";

/// Explicit disambiguation policy for local wall-clock times during a DST
/// fall-back fold (`TIME-DST`). Nonexistent spring-forward gaps are always
/// refused regardless of policy.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum DstPolicy {
    /// Refuse ambiguous fall-back wall times.
    Reject,
    /// Select the earlier UTC instant (pre-transition offset).
    Earlier,
    /// Select the later UTC instant (post-transition offset).
    Later,
}

impl DstPolicy {
    /// Parses a wire DST policy identifier (`"reject"`, `"earlier"`, or `"later"`).
    ///
    /// # Errors
    ///
    /// Returns [`TemporalError::InvalidDstPolicy`] for any other spelling.
    pub fn parse(value: &str) -> Result<Self, TemporalError> {
        match value {
            "reject" => Ok(Self::Reject),
            "earlier" => Ok(Self::Earlier),
            "later" => Ok(Self::Later),
            _ => Err(TemporalError::InvalidDstPolicy),
        }
    }

    /// Returns the canonical wire spelling of this policy.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Reject => "reject",
            Self::Earlier => "earlier",
            Self::Later => "later",
        }
    }
}

/// A temporal value did not satisfy the supported wire representation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TemporalError {
    /// The value is not a real calendar date in `YYYY-MM-DD` form.
    InvalidDate,
    /// The value is not a supported RFC 3339 timestamp with a known offset.
    InvalidTimestamp,
    /// The value is not an offset-free local wall-clock timestamp in
    /// `YYYY-MM-DDTHH:MM:SS[.f{1..9}]` form in years `0001..9999`.
    InvalidLocalTimestamp,
    /// The supplied timezone is absent from the pinned IANA timezone data.
    InvalidTimezone,
    /// The supplied DST policy is not `"reject"`, `"earlier"`, or `"later"`.
    InvalidDstPolicy,
    /// The local wall time falls in a DST spring-forward gap and does not exist.
    NonexistentLocalTime,
    /// The local wall time is ambiguous in a DST fall-back fold under `"reject"`.
    AmbiguousLocalTime,
    /// The instant does not resolve to a date with a four-digit year.
    InstantOutOfRange,
}

impl fmt::Display for TemporalError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidDate => f.write_str("expected a real calendar date in YYYY-MM-DD form"),
            Self::InvalidTimestamp => f.write_str(
                "expected an RFC 3339 timestamp with a known explicit offset and at most nanosecond precision",
            ),
            Self::InvalidLocalTimestamp => f.write_str(
                "expected an offset-free local wall timestamp YYYY-MM-DDTHH:MM:SS[.f{1..9}] in years 0001..9999",
            ),
            Self::InvalidTimezone => {
                f.write_str("expected a timezone in the pinned IANA timezone database")
            }
            Self::InvalidDstPolicy => {
                f.write_str("expected DST policy 'reject', 'earlier', or 'later'")
            }
            Self::NonexistentLocalTime => {
                f.write_str("local wall time does not exist in this timezone (DST spring-forward gap)")
            }
            Self::AmbiguousLocalTime => {
                f.write_str("local wall time is ambiguous in this timezone (DST fall-back fold)")
            }
            Self::InstantOutOfRange => {
                f.write_str("instant must resolve to a local date with a four-digit year")
            }
        }
    }
}

impl std::error::Error for TemporalError {}

/// A local calendar date used to render naming-series date placeholders.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct NamingSeriesDate {
    /// Four-digit proleptic-Gregorian year.
    pub year: u16,
    /// Month number, from 1 through 12.
    pub month: u8,
    /// Day number in the local month.
    pub day: u8,
}

/// Validates a declared timezone against the pinned IANA database.
///
/// # Errors
///
/// Returns [`TemporalError::InvalidTimezone`] when the timezone is not
/// present in that database.
pub fn validate_timezone(timezone: &str) -> Result<(), TemporalError> {
    pinned_timezone(timezone).map(|_| ())
}

/// Validates an offset-free local wall-clock timestamp in years `0001..9999`.
///
/// # Errors
///
/// Returns [`TemporalError::InvalidLocalTimestamp`] when the spelling, date,
/// clock field, or fractional precision is invalid, or when an offset or `Z`
/// suffix is present.
pub fn validate_local_timestamp(value: &str) -> Result<(), TemporalError> {
    local_wall_datetime(value).map(|_| ())
}

/// Resolves an offset-free local wall-clock timestamp in a pinned IANA
/// timezone under an explicit DST policy, returning its canonical UTC RFC 3339
/// timestamp (`TIME-PIN-1`, `TIME-DST`).
///
/// Nonexistent spring-forward wall times are always refused with
/// [`TemporalError::NonexistentLocalTime`]. Ambiguous fall-back wall times are
/// refused under [`DstPolicy::Reject`] and disambiguated deterministically
/// under [`DstPolicy::Earlier`] or [`DstPolicy::Later`].
///
/// # Errors
///
/// Returns [`TemporalError::InvalidLocalTimestamp`],
/// [`TemporalError::InvalidTimezone`],
/// [`TemporalError::NonexistentLocalTime`],
/// [`TemporalError::AmbiguousLocalTime`], or
/// [`TemporalError::InstantOutOfRange`].
pub fn resolve_local_timestamp(
    local_wall_time: &str,
    iana_timezone: &str,
    policy: DstPolicy,
) -> Result<String, TemporalError> {
    let local = local_wall_datetime(local_wall_time)?;
    let timezone = pinned_timezone(iana_timezone)?;
    let offset = match timezone.to_ambiguous_timestamp(local).offset() {
        AmbiguousOffset::Unambiguous { offset } => offset,
        AmbiguousOffset::Gap { .. } => return Err(TemporalError::NonexistentLocalTime),
        AmbiguousOffset::Fold { before, after } => match policy {
            DstPolicy::Reject => return Err(TemporalError::AmbiguousLocalTime),
            DstPolicy::Earlier => before,
            DstPolicy::Later => after,
        },
    };
    let offset_seconds = i64::from(offset.seconds())
        .checked_neg()
        .ok_or(TemporalError::InstantOutOfRange)?;
    let utc = shift(local, offset_seconds).ok_or(TemporalError::InstantOutOfRange)?;
    if !(1..=9_999).contains(&utc.year()) {
        return Err(TemporalError::InstantOutOfRange);
    }
    rfc3339_utc(&utc).map_err(|_error| TemporalError::InstantOutOfRange)
}

fn local_wall_datetime(value: &str) -> Result<DateTime, TemporalError> {
    const INVALID: TemporalError = TemporalError::InvalidLocalTimestamp;
    let bytes = value.as_bytes();
    if !(19..=29).contains(&bytes.len())
        || !value.is_ascii()
        || bytes.get(4) != Some(&b'-')
        || bytes.get(7) != Some(&b'-')
        || bytes.get(10) != Some(&b'T')
        || bytes.get(13) != Some(&b':')
        || bytes.get(16) != Some(&b':')
    {
        return Err(INVALID);
    }
    let date = calendar_date(bytes).ok_or(INVALID)?;
    if !(1..=9_999).contains(&date.year()) {
        return Err(INVALID);
    }
    let field = |range: Range<usize>| number(bytes, range).ok_or(INVALID);
    let (hour, minute, second) = (field(11..13)?, field(14..16)?, field(17..19)?);
    let nanos = if bytes.len() > 19 {
        if bytes.get(19) != Some(&b'.') {
            return Err(INVALID);
        }
        let frac_len = bytes.len().checked_sub(20).ok_or(INVALID)?;
        if !(1..=9).contains(&frac_len) {
            return Err(INVALID);
        }
        let scale = 10_i32
            .checked_pow(
                u32::try_from(9_usize.checked_sub(frac_len).ok_or(INVALID)?)
                    .map_err(|_error| INVALID)?,
            )
            .ok_or(INVALID)?;
        field(20..bytes.len())?.checked_mul(scale).ok_or(INVALID)?
    } else {
        0
    };
    let time = Time::new(
        i8::try_from(hour).map_err(|_error| INVALID)?,
        i8::try_from(minute).map_err(|_error| INVALID)?,
        i8::try_from(second).map_err(|_error| INVALID)?,
        nanos,
    )
    .map_err(|_error| INVALID)?;
    Ok(date.to_datetime(time))
}

/// The pinned database's zone with exactly this identifier. `jiff_tzdb::get`
/// ignores ASCII case, so the canonical name must match byte for byte, as
/// `chrono-tz`'s `FromStr` required. The data is read from `jiff-tzdb`
/// directly: `jiff::tz::db()` would consult the host's zoneinfo when another
/// crate enables that feature.
fn pinned_timezone(name: &str) -> Result<TimeZone, TemporalError> {
    match jiff_tzdb::get(name) {
        Some((canonical, data)) if canonical == name && name != NOT_A_CHRONO_TZ_ZONE => {
            TimeZone::tzif(name, data).map_err(|_parse_error| TemporalError::InvalidTimezone)
        }
        Some(_) | None => Err(TemporalError::InvalidTimezone),
    }
}

/// Converts a supplied Unix epoch-millisecond commit instant to a local date.
///
/// This function reads no clock. The caller supplies the time that belongs to
/// the commit so replay uses the same instant and pinned timezone data.
///
/// # Errors
///
/// Returns [`TemporalError::InvalidTimezone`] for an unknown IANA timezone,
/// or [`TemporalError::InstantOutOfRange`] if the instant cannot be represented
/// or converts to a local year outside `0000` through `9999`.
pub fn naming_series_date_at(
    epoch_millis: i64,
    timezone: &str,
) -> Result<NamingSeriesDate, TemporalError> {
    let timezone = pinned_timezone(timezone)?;
    let date = local_date(epoch_millis, &timezone)?;
    let year = date.year();
    if !(0..=9_999).contains(&year) {
        return Err(TemporalError::InstantOutOfRange);
    }
    Ok(NamingSeriesDate {
        year: u16::try_from(year).map_err(|_error| TemporalError::InstantOutOfRange)?,
        month: u8::try_from(date.month()).map_err(|_error| TemporalError::InstantOutOfRange)?,
        day: u8::try_from(date.day()).map_err(|_error| TemporalError::InstantOutOfRange)?,
    })
}

/// The local calendar date of an epoch-millisecond instant. The zone's offset
/// is the one at the whole second containing the instant (rounded toward
/// negative infinity, as chrono-tz did; jiff's own lookup truncates a
/// pre-1970 fractional second toward zero, one second late), or at the
/// nearest representable instant beyond either end of `jiff::Timestamp`
/// (which stops a day short of the civil range). Only whole days are added to
/// a date, which is exact over the whole civil range.
fn local_date(epoch_millis: i64, timezone: &TimeZone) -> Result<Date, TemporalError> {
    const OUT: TemporalError = TemporalError::InstantOutOfRange;
    let instant =
        Timestamp::from_second(epoch_millis.div_euclid(1_000)).unwrap_or(if epoch_millis < 0 {
            Timestamp::MIN
        } else {
            Timestamp::MAX
        });
    let offset_millis = i64::from(timezone.to_offset(instant).seconds())
        .checked_mul(1_000)
        .ok_or(OUT)?;
    let local_millis = epoch_millis.checked_add(offset_millis).ok_or(OUT)?;
    let days = Span::new()
        .try_days(local_millis.div_euclid(86_400_000))
        .map_err(|_error| OUT)?;
    Date::constant(1970, 1, 1)
        .checked_add(days)
        .map_err(|_error| OUT)
}

/// Validates the exact `EntityIR` calendar-date representation.
///
/// The four-digit year includes `0000`, matching the existing `EntityIR`
/// property-date validator's proleptic-Gregorian calendar semantics.
///
/// # Errors
///
/// Returns [`TemporalError::InvalidDate`] when the spelling or calendar day
/// is invalid.
pub fn validate_date(value: &str) -> Result<(), TemporalError> {
    let bytes = value.as_bytes();
    if bytes.len() != 10
        || !value.is_ascii()
        || !digits(bytes.get(..4).ok_or(TemporalError::InvalidDate)?)
        || bytes.get(4) != Some(&b'-')
        || !digits(bytes.get(5..7).ok_or(TemporalError::InvalidDate)?)
        || bytes.get(7) != Some(&b'-')
        || !digits(bytes.get(8..10).ok_or(TemporalError::InvalidDate)?)
    {
        return Err(TemporalError::InvalidDate);
    }

    calendar_date(value.as_bytes())
        .map(|_| ())
        .ok_or(TemporalError::InvalidDate)
}

/// Parses an RFC 3339 timestamp and returns its canonical UTC instant.
///
/// Both timestamp policies use this same stored form. The RFC 3339 `-00:00`
/// spelling is rejected because it means the local offset is unknown. Inputs
/// with leap seconds or more than nine fractional digits are also refused so
/// parsing and UTC normalization never discard temporal precision. The UTC
/// result must itself fit the four-digit-year RFC 3339 representation.
///
/// # Errors
///
/// Returns [`TemporalError::InvalidTimestamp`] when the spelling, calendar
/// value, offset, precision, or normalized UTC year is unsupported.
pub fn canonical_timestamp(value: &str) -> Result<String, TemporalError> {
    timestamp_shape(value)?;
    let canonical = rfc3339_utc(&utc_reading(value)?)?;

    // UTC conversion near the supported year boundaries can produce a signed
    // or five-digit year. Such an instant cannot be stored in our admitted
    // four-digit RFC 3339 subset.
    timestamp_shape(&canonical)?;
    Ok(canonical)
}

/// The exact UTC ordering key of an admitted RFC 3339 timestamp.
/// The tuple compares whole seconds before nanoseconds, including timestamps
/// before the Unix epoch and spellings with different explicit offsets.
///
/// # Errors
/// Returns [`TemporalError::InvalidTimestamp`] for unsupported timestamps.
pub fn timestamp_ordering_key(value: &str) -> Result<(i64, u32), TemporalError> {
    let canonical = canonical_timestamp(value)?;
    let utc = utc_reading(&canonical)?;
    let since_epoch = utc.duration_since(DateTime::constant(1970, 1, 1, 0, 0, 0, 0));
    let seconds = since_epoch
        .as_secs()
        .checked_sub(i64::from(since_epoch.subsec_nanos() < 0))
        .ok_or(TemporalError::InvalidTimestamp)?;
    let nanos =
        u32::try_from(utc.subsec_nanosecond()).map_err(|_error| TemporalError::InvalidTimestamp)?;
    Ok((seconds, nanos))
}

/// The value of an all-ASCII-digit field, or `None` when absent or not digits.
fn number(bytes: &[u8], range: Range<usize>) -> Option<i32> {
    let field = bytes.get(range)?;
    if !digits(field) {
        return None;
    }
    field.iter().try_fold(0_i32, |value, digit| {
        value
            .checked_mul(10)?
            .checked_add(i32::from(digit.checked_sub(b'0')?))
    })
}

/// The proleptic-Gregorian day a `YYYY-MM-DD` prefix names, if it is real.
fn calendar_date(bytes: &[u8]) -> Option<Date> {
    let year = i16::try_from(number(bytes, 0..4)?).ok()?;
    let month = i8::try_from(number(bytes, 5..7)?).ok()?;
    let day = i8::try_from(number(bytes, 8..10)?).ok()?;
    Date::new(year, month, day).ok()
}

/// The UTC wall-clock reading of a value that passed [`timestamp_shape`].
/// RFC 3339 ranges: hour 00-23, minute and second 00-59, offset hour 00-23
/// and offset minute 00-59.
fn utc_reading(value: &str) -> Result<DateTime, TemporalError> {
    const INVALID: TemporalError = TemporalError::InvalidTimestamp;
    let bytes = value.as_bytes();
    let field = |range: Range<usize>| number(bytes, range).ok_or(INVALID);
    let (hour, minute, second) = (field(11..13)?, field(14..16)?, field(17..19)?);
    let zone_start = if value.ends_with('Z') {
        bytes.len().checked_sub(1)
    } else {
        bytes.len().checked_sub(6)
    }
    .ok_or(INVALID)?;
    let nanos = if bytes.get(19) == Some(&b'.') {
        let digits = zone_start.checked_sub(20).ok_or(INVALID)?;
        let scale = 10_i32
            .checked_pow(
                u32::try_from(9_usize.checked_sub(digits).ok_or(INVALID)?)
                    .map_err(|_error| INVALID)?,
            )
            .ok_or(INVALID)?;
        field(20..zone_start)?.checked_mul(scale).ok_or(INVALID)?
    } else {
        0
    };
    let offset_seconds = if value.ends_with('Z') {
        0
    } else {
        let start = |at: usize| zone_start.checked_add(at).ok_or(INVALID);
        let hours = field(start(1)?..start(3)?)?;
        let minutes = field(start(4)?..start(6)?)?;
        if hours > 23 || minutes > 59 {
            return Err(INVALID);
        }
        let magnitude = hours
            .checked_mul(3600)
            .and_then(|seconds| seconds.checked_add(minutes.checked_mul(60)?))
            .ok_or(INVALID)?;
        if bytes.get(zone_start) == Some(&b'-') {
            magnitude.checked_neg().ok_or(INVALID)?
        } else {
            magnitude
        }
    };
    let time = Time::new(
        i8::try_from(hour).map_err(|_error| INVALID)?,
        i8::try_from(minute).map_err(|_error| INVALID)?,
        i8::try_from(second).map_err(|_error| INVALID)?,
        nanos,
    )
    .map_err(|_error| INVALID)?;
    let date = calendar_date(bytes).ok_or(INVALID)?;
    shift(
        date.to_datetime(time),
        offset_seconds.checked_neg().ok_or(INVALID)?.into(),
    )
    .ok_or(INVALID)
}

/// `reading` moved by `seconds`, using integer seconds-of-day and whole-day
/// date arithmetic only (exact over the whole civil range, unlike jiff's
/// civil time arithmetic near its upper end).
fn shift(reading: DateTime, seconds: i64) -> Option<DateTime> {
    let of_day = i64::from(reading.hour())
        .checked_mul(3_600)?
        .checked_add(i64::from(reading.minute()).checked_mul(60)?)?
        .checked_add(i64::from(reading.second()))?
        .checked_add(seconds)?;
    let days = Span::new().try_days(of_day.div_euclid(86_400)).ok()?;
    let of_day = of_day.rem_euclid(86_400);
    let time = Time::new(
        i8::try_from(of_day / 3_600).ok()?,
        i8::try_from(of_day % 3_600 / 60).ok()?,
        i8::try_from(of_day % 60).ok()?,
        reading.subsec_nanosecond(),
    )
    .ok()?;
    Some(reading.date().checked_add(days).ok()?.to_datetime(time))
}

/// RFC 3339 with `Z` and the shortest exact fraction of 0, 3, 6 or 9 digits.
fn rfc3339_utc(utc: &DateTime) -> Result<String, TemporalError> {
    if !(0..=9_999).contains(&utc.year()) {
        return Err(TemporalError::InvalidTimestamp);
    }
    let nanos = utc.subsec_nanosecond();
    let fraction = if nanos == 0 {
        String::new()
    } else if nanos % 1_000_000 == 0 {
        format!(".{:03}", nanos / 1_000_000)
    } else if nanos % 1_000 == 0 {
        format!(".{:06}", nanos / 1_000)
    } else {
        format!(".{nanos:09}")
    };
    Ok(format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}{fraction}Z",
        utc.year(),
        utc.month(),
        utc.day(),
        utc.hour(),
        utc.minute(),
        utc.second()
    ))
}

fn digits(bytes: &[u8]) -> bool {
    !bytes.is_empty() && bytes.iter().all(u8::is_ascii_digit)
}

/// Checks the RFC 3339 spelling constraints; [`utc_reading`] then checks
/// calendar, time, and offset validity.
fn timestamp_shape(value: &str) -> Result<(), TemporalError> {
    let bytes = value.as_bytes();
    if !(20..=35).contains(&bytes.len())
        || !value.is_ascii()
        || !digits(bytes.get(..4).ok_or(TemporalError::InvalidTimestamp)?)
        || bytes.get(4) != Some(&b'-')
        || !digits(bytes.get(5..7).ok_or(TemporalError::InvalidTimestamp)?)
        || bytes.get(7) != Some(&b'-')
        || !digits(bytes.get(8..10).ok_or(TemporalError::InvalidTimestamp)?)
        || bytes.get(10) != Some(&b'T')
        || !digits(bytes.get(11..13).ok_or(TemporalError::InvalidTimestamp)?)
        || bytes.get(13) != Some(&b':')
        || !digits(bytes.get(14..16).ok_or(TemporalError::InvalidTimestamp)?)
        || bytes.get(16) != Some(&b':')
        || !digits(bytes.get(17..19).ok_or(TemporalError::InvalidTimestamp)?)
        || bytes
            .get(17..19)
            .is_some_and(|seconds| seconds == &b"60"[..])
    {
        return Err(TemporalError::InvalidTimestamp);
    }

    let zone_start = if value.ends_with('Z') {
        bytes
            .len()
            .checked_sub(1)
            .ok_or(TemporalError::InvalidTimestamp)?
    } else {
        let offset_start = bytes
            .len()
            .checked_sub(6)
            .ok_or(TemporalError::InvalidTimestamp)?;
        let offset = bytes
            .get(offset_start..)
            .ok_or(TemporalError::InvalidTimestamp)?;
        if !offset
            .first()
            .is_some_and(|sign| matches!(*sign, b'+' | b'-'))
            || !digits(offset.get(1..3).ok_or(TemporalError::InvalidTimestamp)?)
            || offset.get(3) != Some(&b':')
            || !digits(offset.get(4..6).ok_or(TemporalError::InvalidTimestamp)?)
        {
            return Err(TemporalError::InvalidTimestamp);
        }
        if offset == &b"-00:00"[..] {
            return Err(TemporalError::InvalidTimestamp);
        }
        offset_start
    };

    let time = bytes
        .get(11..zone_start)
        .ok_or(TemporalError::InvalidTimestamp)?;
    let (whole, fraction) = match time.iter().position(|byte| *byte == b'.') {
        Some(separator) => (
            time.get(..separator)
                .ok_or(TemporalError::InvalidTimestamp)?,
            Some(
                time.get(
                    separator
                        .checked_add(1)
                        .ok_or(TemporalError::InvalidTimestamp)?..,
                )
                .ok_or(TemporalError::InvalidTimestamp)?,
            ),
        ),
        None => (time, None),
    };
    if whole.len() != 8
        || whole.get(2) != Some(&b':')
        || whole.get(5) != Some(&b':')
        || fraction.is_some_and(|fraction| !(1..=9).contains(&fraction.len()) || !digits(fraction))
    {
        return Err(TemporalError::InvalidTimestamp);
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{
        NAMING_SERIES_TZDB_VERSION, NamingSeriesDate, TemporalError, canonical_timestamp,
        naming_series_date_at, timestamp_ordering_key, validate_date, validate_timezone,
    };

    #[test]
    fn timestamp_ordering_preserves_offsets_and_fractional_seconds() {
        let zero = timestamp_ordering_key("2026-01-01T00:00:00Z");
        assert_eq!(zero, timestamp_ordering_key("2026-01-01T02:00:00+02:00"));
        assert_eq!(zero, timestamp_ordering_key("2025-12-31T19:00:00-05:00"));
        for (earlier, later) in [
            ("2026-01-01T00:00:00Z", "2026-01-01T00:00:00.001Z"),
            ("2026-01-01T00:00:00.001Z", "2026-01-01T00:00:00.001000001Z"),
            ("1969-12-31T23:59:59.9Z", "1970-01-01T00:00:00Z"),
        ] {
            let earlier = timestamp_ordering_key(earlier).expect("valid timestamp");
            let later = timestamp_ordering_key(later).expect("valid timestamp");
            assert!(earlier < later);
        }
    }

    #[test]
    fn a_pre_1970_fractional_ordering_key_floors_the_second() {
        assert_eq!(
            timestamp_ordering_key("1969-12-31T23:59:59.9Z"),
            Ok((-1, 900_000_000))
        );
        assert_eq!(
            timestamp_ordering_key("1969-12-31T23:59:59.000000001Z"),
            Ok((-1, 1))
        );
        assert_eq!(
            timestamp_ordering_key("1970-01-01T05:29:59.5+05:30"),
            Ok((-1, 500_000_000))
        );
    }

    fn ymd(year: u16, month: u8, day: u8) -> NamingSeriesDate {
        NamingSeriesDate { year, month, day }
    }

    #[test]
    fn spring_forward_and_fall_back_move_the_local_date() {
        let ny = "America/New_York";
        // 2024-03-10 spring forward: 04:30Z is 23:30 EST the day before, and
        // a day later 04:30Z is 00:30 EDT.
        assert_eq!(
            naming_series_date_at(1_710_045_000_000, ny),
            Ok(ymd(2024, 3, 9))
        );
        assert_eq!(
            naming_series_date_at(1_710_131_400_000, ny),
            Ok(ymd(2024, 3, 11))
        );
        // 2024-11-03 fall back: 04:30Z is 00:30 EDT, a day later 23:30 EST.
        assert_eq!(
            naming_series_date_at(1_730_608_200_000, ny),
            Ok(ymd(2024, 11, 3))
        );
        assert_eq!(
            naming_series_date_at(1_730_694_600_000, ny),
            Ok(ymd(2024, 11, 3))
        );

        // America/Sao_Paulo moved its clocks at local midnight. Spring forward
        // 2018-11-04T03:00Z: 23:59:59.999 on 3 November, then 01:00 on the 4th.
        let sp = "America/Sao_Paulo";
        assert_eq!(
            naming_series_date_at(1_541_300_399_999, sp),
            Ok(ymd(2018, 11, 3))
        );
        assert_eq!(
            naming_series_date_at(1_541_300_400_000, sp),
            Ok(ymd(2018, 11, 4))
        );
        // Fall back 2019-02-17T02:00Z: midnight of the 17th becomes 23:00 on
        // the 16th again, so the 16th lasts until 03:00Z.
        assert_eq!(
            naming_series_date_at(1_550_368_799_999, sp),
            Ok(ymd(2019, 2, 16))
        );
        assert_eq!(
            naming_series_date_at(1_550_368_800_000, sp),
            Ok(ymd(2019, 2, 16))
        );
        assert_eq!(
            naming_series_date_at(1_550_372_399_999, sp),
            Ok(ymd(2019, 2, 16))
        );
        assert_eq!(
            naming_series_date_at(1_550_372_400_000, sp),
            Ok(ymd(2019, 2, 17))
        );
    }

    #[test]
    fn dates_require_exact_shape_and_a_real_proleptic_gregorian_day() {
        for date in [
            "0000-02-29",
            "0001-01-01",
            "2000-02-29",
            "2024-02-29",
            "2026-12-31",
        ] {
            assert_eq!(validate_date(date), Ok(()), "{date}");
        }
        for date in [
            "2026-02-29",
            "1900-02-29",
            "2026-13-01",
            "2026-00-10",
            "2026-04-31",
            "2026-1-01",
            "26-01-01",
            "２０２６-01-01",
        ] {
            assert_eq!(
                validate_date(date),
                Err(TemporalError::InvalidDate),
                "{date}"
            );
        }
    }

    #[test]
    fn timestamps_normalize_offsets_and_fractional_precision() {
        assert_eq!(
            canonical_timestamp("2026-01-01T02:00:00+02:00").as_deref(),
            Ok("2026-01-01T00:00:00Z")
        );
        assert_eq!(
            canonical_timestamp("2025-12-31T19:00:00-05:00").as_deref(),
            Ok("2026-01-01T00:00:00Z")
        );
        assert_eq!(
            canonical_timestamp("2026-01-01T00:00:00.123400000Z").as_deref(),
            Ok("2026-01-01T00:00:00.123400Z")
        );
        assert_eq!(
            canonical_timestamp("2026-01-01T00:00:00.000Z").as_deref(),
            Ok("2026-01-01T00:00:00Z")
        );
    }

    #[test]
    fn timestamps_reject_unknown_offsets_invalid_calendar_and_lossy_precision() {
        for timestamp in [
            "2026-01-01T02:00:00",
            "2026-01-01T02:00:00-00:00",
            "2026-01-01 02:00:00Z",
            "2026-01-01t02:00:00z",
            "2026-02-30T00:00:00Z",
            "2026-01-01T24:00:00Z",
            "2026-01-01T00:00:60Z",
            "2026-01-01T00:00:00.1234567890Z",
            "2026-01-01T00:00:00+24:00",
        ] {
            assert_eq!(
                canonical_timestamp(timestamp),
                Err(TemporalError::InvalidTimestamp),
                "{timestamp}"
            );
        }
    }

    #[test]
    fn utc_conversion_must_remain_in_the_supported_four_digit_year_range() {
        assert_eq!(
            canonical_timestamp("0000-01-01T00:00:00+00:01"),
            Err(TemporalError::InvalidTimestamp)
        );
        assert_eq!(
            canonical_timestamp("9999-12-31T23:59:59-00:01"),
            Err(TemporalError::InvalidTimestamp)
        );
    }

    #[test]
    fn naming_series_date_refuses_extreme_instants_without_panicking() {
        for (epoch_millis, timezone) in [
            (
                // chrono 0.4's `DateTime::<Utc>::MAX_UTC`, the old upper bound.
                8_210_266_876_799_999,
                "Etc/GMT-14",
            ),
            (
                // chrono 0.4's `DateTime::<Utc>::MIN_UTC`, the old lower bound.
                -8_334_601_315_200_000,
                "Etc/GMT+12",
            ),
            (i64::MAX, "UTC"),
            (i64::MIN, "UTC"),
        ] {
            assert_eq!(
                naming_series_date_at(epoch_millis, timezone),
                Err(TemporalError::InstantOutOfRange),
                "{epoch_millis} in {timezone}"
            );
        }
    }

    #[test]
    fn a_pre_1970_instant_just_before_a_transition_keeps_the_earlier_offset() {
        // Africa/Addis_Ababa moved from UTC+2:45 to UTC+3 at local midnight
        // 1942-08-01, i.e. 1942-07-31T21:15:00Z. One millisecond earlier the
        // old offset still applies: it is 23:59:59.999 on 31 July locally.
        assert_eq!(
            naming_series_date_at(-865_305_900_001, "Africa/Addis_Ababa"),
            naming_series_date_at(-865_305_901_000, "Africa/Addis_Ababa"),
        );
        assert_eq!(
            naming_series_date_at(-865_305_900_001, "Africa/Addis_Ababa"),
            Ok(NamingSeriesDate {
                year: 1942,
                month: 7,
                day: 31,
            })
        );
    }

    #[test]
    fn the_named_tzdb_version_is_the_pinned_data() {
        assert_eq!(jiff_tzdb::VERSION, Some(NAMING_SERIES_TZDB_VERSION));
        // tzdb 2026c moved America/Edmonton to year-round UTC-6 from
        // 2026-12-01; in 2025b it is back on MST (UTC-7) that winter, so
        // 2027-01-01T06:30Z is still 2026-12-31 there. A data bump fails here.
        assert_eq!(
            naming_series_date_at(1_798_785_000_000, "America/Edmonton"),
            Ok(NamingSeriesDate {
                year: 2026,
                month: 12,
                day: 31,
            })
        );
    }

    #[test]
    fn the_zone_names_are_exactly_chrono_tz_0_10_4s() {
        let names: Vec<&str> = jiff_tzdb::available()
            .filter(|name| *name != "Factory")
            .collect();
        assert_eq!(names.len(), 597);
        for name in &names {
            assert_eq!(validate_timezone(name), Ok(()), "{name}");
        }
        // Links from `backward` and `etcetera` are names in their own right.
        for name in ["US/Eastern", "Etc/GMT+5", "UTC", "Zulu", "Asia/Calcutta"] {
            assert!(names.contains(&name), "{name}");
        }
        // `Factory` (tzdb's `factory` file) was never part of chrono-tz, the
        // lookup is case-sensitive as chrono-tz's `FromStr` was, and only
        // whole names match.
        for name in [
            "Factory",
            "factory",
            "america/new_york",
            "AMERICA/NEW_YORK",
            "America/New_York ",
            "America",
            "",
            "localtime",
            "posixrules",
            "EST5EDT,M3.2.0,M11.1.0",
        ] {
            assert_eq!(
                validate_timezone(name),
                Err(TemporalError::InvalidTimezone),
                "{name:?}"
            );
        }
    }

    #[test]
    fn naming_series_date_uses_the_explicit_instant_and_iana_timezone() {
        assert_eq!(NAMING_SERIES_TZDB_VERSION, "2025b");
        assert_eq!(validate_timezone("America/New_York"), Ok(()));
        assert_eq!(
            validate_timezone("Not/A_Timezone"),
            Err(TemporalError::InvalidTimezone)
        );
        assert_eq!(
            naming_series_date_at(1_767_241_800_000, "America/New_York"),
            Ok(NamingSeriesDate {
                year: 2025,
                month: 12,
                day: 31,
            })
        );
        assert_eq!(
            naming_series_date_at(1_767_245_400_000, "America/New_York"),
            Ok(NamingSeriesDate {
                year: 2026,
                month: 1,
                day: 1,
            })
        );
    }
}
