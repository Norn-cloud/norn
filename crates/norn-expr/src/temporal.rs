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
use std::sync::OnceLock;

use jiff::civil::{Date, DateTime, Time};
use jiff::tz::{Offset, TimeZone, TimeZoneDatabase};
use jiff::{SignedDuration, Timestamp};

/// IANA timezone database version bundled by the exact-pinned `jiff-tzdb`
/// dependency (the only database `jiff` is built with here), for
/// naming-series allocation provenance.
pub const NAMING_SERIES_TZDB_VERSION: &str = "2026c";

/// A temporal value did not satisfy the supported wire representation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TemporalError {
    /// The value is not a real calendar date in `YYYY-MM-DD` form.
    InvalidDate,
    /// The value is not a supported RFC 3339 timestamp with a known offset.
    InvalidTimestamp,
    /// The supplied timezone is absent from the pinned IANA timezone data.
    InvalidTimezone,
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
            Self::InvalidTimezone => {
                f.write_str("expected a timezone in the pinned IANA timezone database")
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

/// The pinned database's zone with exactly this identifier. The database
/// lookup ignores ASCII case, so the canonical name must match byte for byte.
/// The bundled database is named explicitly: `jiff::tz::db()` would consult
/// the host's zoneinfo when another crate enables that feature.
fn pinned_timezone(name: &str) -> Result<TimeZone, TemporalError> {
    static PINNED: OnceLock<TimeZoneDatabase> = OnceLock::new();
    let timezone = PINNED
        .get_or_init(TimeZoneDatabase::bundled)
        .get(name)
        .map_err(|_lookup_error| TemporalError::InvalidTimezone)?;
    if timezone.iana_name() == Some(name) {
        Ok(timezone)
    } else {
        Err(TemporalError::InvalidTimezone)
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
    let local = match Timestamp::from_millisecond(epoch_millis) {
        Ok(instant) => timezone.to_offset(instant).to_datetime(instant),
        Err(_range_error) => local_after_last_instant(epoch_millis, &timezone)?,
    };
    let date = local.date();
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

/// `jiff::Timestamp` ends a day before the last civil date-time, so an instant
/// in that final stretch is converted on the civil scale with the zone's
/// offset at the last representable instant. Earlier instants are refused.
fn local_after_last_instant(
    epoch_millis: i64,
    timezone: &TimeZone,
) -> Result<DateTime, TemporalError> {
    let last_millis = Timestamp::MAX.as_millisecond();
    let excess = epoch_millis
        .checked_sub(last_millis)
        .filter(|excess| *excess > 0)
        .ok_or(TemporalError::InstantOutOfRange)?;
    let last = Timestamp::from_millisecond(last_millis)
        .map_err(|_error| TemporalError::InstantOutOfRange)?;
    let offset = timezone.to_offset(Timestamp::MAX);
    Offset::UTC
        .to_datetime(last)
        .checked_add(SignedDuration::from_millis(excess))
        .and_then(|utc| utc.checked_add(SignedDuration::from_secs(i64::from(offset.seconds()))))
        .map_err(|_error| TemporalError::InstantOutOfRange)
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
            .checked_pow(u32::try_from(9_usize.checked_sub(digits).ok_or(INVALID)?).map_err(|_error| INVALID)?)
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
    calendar_date(bytes)
        .ok_or(INVALID)?
        .to_datetime(time)
        .checked_sub(SignedDuration::from_secs(i64::from(offset_seconds)))
        .map_err(|_error| INVALID)
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
    fn the_named_tzdb_version_is_the_bundled_one() {
        assert_eq!(jiff_tzdb::VERSION, Some(NAMING_SERIES_TZDB_VERSION));
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
