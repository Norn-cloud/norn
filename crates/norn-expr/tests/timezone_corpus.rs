#![allow(
    clippy::arithmetic_side_effects,
    clippy::as_conversions,
    clippy::expect_used,
    clippy::panic,
    clippy::pedantic,
    missing_docs,
    reason = "integration-test crate"
)]

//! Norn (#1436): `naming_series_date_at`, `canonical_timestamp` and
//! `timestamp_ordering_key` give the outputs recorded from the chrono /
//! chrono-tz 0.10.4 build they replaced (merge-base `3f787c06c4`, tzdb 2025b).
//!
//! The corpus is every pre-2100 offset transition of the zones below, each
//! probed at -1 s, -1 ms, 0, +1 ms and +1 s, and each spelled as an RFC 3339
//! timestamp in the offset on either side of it; plus a grid of pre- and
//! post-1970 fractional instants under assorted offsets. Inputs are listed
//! with `jiff` (transitions and RFC 3339 spelling only); outputs come only
//! from the `norn_expr::temporal` functions under test. On a mismatch the
//! test prints the actual table: a deliberate data move (#1440) re-records it
//! from that output after review.

use std::collections::BTreeMap;
use std::fmt::Write as _;

use jiff::Timestamp;
use jiff::tz::{Offset, TimeZone};
use norn_expr::temporal::{
    TzdbBundle, canonical_timestamp, naming_series_date_at, timestamp_ordering_key,
};

/// `(category, cases, FNV-1a)` recorded by running this test on the chrono
/// build (merge-base `3f787c06c4`, with `jiff`/`jiff-tzdb` added as
/// dev-dependencies only to list the inputs).
const RECORDED: &[(&str, u64, u64)] = &[
    ("canonical_timestamp", 38350, 0xce5bea33ec5ccb43),
    (
        "naming_series_date_at Africa/Addis_Ababa",
        30,
        0x6d2309ba9702221e,
    ),
    (
        "naming_series_date_at Africa/Casablanca",
        1110,
        0x96c6c35126e96017,
    ),
    (
        "naming_series_date_at America/Havana",
        1405,
        0x47f82875ec59bf50,
    ),
    (
        "naming_series_date_at America/Los_Angeles",
        1555,
        0x9ec26149f1c2df75,
    ),
    (
        "naming_series_date_at America/New_York",
        1805,
        0x1c7b918433909e6a,
    ),
    (
        "naming_series_date_at America/Santiago",
        1420,
        0x8c94eb4364463bc8,
    ),
    (
        "naming_series_date_at America/Sao_Paulo",
        460,
        0x838fcf5328a06eaf,
    ),
    (
        "naming_series_date_at America/St_Johns",
        1825,
        0x2918eec465b0fb04,
    ),
    (
        "naming_series_date_at Antarctica/Troll",
        960,
        0x1920032284541a8f,
    ),
    (
        "naming_series_date_at Asia/Kathmandu",
        15,
        0x67b44e8c0cb0b929,
    ),
    ("naming_series_date_at Asia/Kolkata", 40, 0x3ba865d12a0f0d4b),
    ("naming_series_date_at Asia/Tehran", 360, 0xec6248f1b5ecda7a),
    (
        "naming_series_date_at Australia/Lord_Howe",
        1200,
        0xec73cffe15486b62,
    ),
    (
        "naming_series_date_at Australia/Sydney",
        1335,
        0x7ae86dff907df3a6,
    ),
    (
        "naming_series_date_at Europe/Dublin",
        1770,
        0xf23fd0a87cd11502,
    ),
    (
        "naming_series_date_at Europe/London",
        1840,
        0x4eccd477c9f8c36b,
    ),
    (
        "naming_series_date_at Europe/Moscow",
        395,
        0x41f42698a251180f,
    ),
    (
        "naming_series_date_at Pacific/Apia",
        135,
        0x887ff813c525a2f0,
    ),
    (
        "naming_series_date_at Pacific/Chatham",
        1270,
        0xf5681e835c804047,
    ),
    (
        "naming_series_date_at Pacific/Kiritimati",
        20,
        0x8d0c5748e2b6cfde,
    ),
    ("naming_series_date_at UTC", 5, 0x2eeea40d56adcad7),
    ("timestamp_ordering_key", 38350, 0x6184fe935042978d),
];

/// Zones with DST at local midnight, half-hour and 45-minute offsets, negative
/// DST, date-line skips, pre-1970 LMT changes and frequent rule changes.
const ZONES: &[&str] = &[
    "Africa/Addis_Ababa",
    "Africa/Casablanca",
    "America/Havana",
    "America/Los_Angeles",
    "America/New_York",
    "America/Santiago",
    "America/Sao_Paulo",
    "America/St_Johns",
    "Antarctica/Troll",
    "Asia/Kathmandu",
    "Asia/Kolkata",
    "Asia/Tehran",
    "Australia/Lord_Howe",
    "Australia/Sydney",
    "Europe/Dublin",
    "Europe/London",
    "Europe/Moscow",
    "Pacific/Apia",
    "Pacific/Chatham",
    "Pacific/Kiritimati",
    "UTC",
];

/// 1800-01-01T00:00:00Z and 2100-01-01T00:00:00Z.
const FROM_SECOND: i64 = -5_364_662_400;
const UNTIL_SECOND: i64 = 4_102_444_800;

#[derive(Debug, PartialEq, Eq)]
struct Fingerprint {
    cases: u64,
    fnv1a: u64,
}

impl Default for Fingerprint {
    fn default() -> Self {
        Self {
            cases: 0,
            fnv1a: 0xcbf2_9ce4_8422_2325,
        }
    }
}

impl Fingerprint {
    fn add(&mut self, input: &str, output: &str) {
        self.cases += 1;
        for byte in input
            .bytes()
            .chain([0x1f])
            .chain(output.bytes())
            .chain([0x1e])
        {
            self.fnv1a ^= u64::from(byte);
            self.fnv1a = self.fnv1a.wrapping_mul(0x0100_0000_01b3);
        }
    }
}

type Transition = (Timestamp, Offset, Offset);

/// The pre-2100 transitions of `zone`, with the offsets before and after.
fn transitions(zone: &str) -> Result<Vec<Transition>, Box<dyn std::error::Error>> {
    let data = TzdbBundle::v2025b()
        .tzif(zone)
        .ok_or_else(|| format!("zone `{zone}` missing from tzdb 2025b"))?;
    let tz = TimeZone::tzif(zone, data)?;
    let from = Timestamp::from_second(FROM_SECOND)?;
    let until = Timestamp::from_second(UNTIL_SECOND)?;
    let mut before = tz.to_offset(from);
    let mut out = Vec::new();
    for transition in tz.following(from) {
        let at = transition.timestamp();
        if at >= until {
            break;
        }
        out.push((at, before, transition.offset()));
        before = transition.offset();
    }
    Ok(out)
}

fn date(epoch_millis: i64, zone: &str) -> String {
    match naming_series_date_at(epoch_millis, zone) {
        Ok(d) => format!("{:04}-{:02}-{:02}", d.year, d.month, d.day),
        Err(error) => format!("E:{error:?}"),
    }
}

fn canonical(value: &str) -> String {
    match canonical_timestamp(value) {
        Ok(s) => format!("S:{s}"),
        Err(error) => format!("E:{error:?}"),
    }
}

fn ordering_key(value: &str) -> String {
    match timestamp_ordering_key(value) {
        Ok((seconds, nanos)) => format!("K:{seconds},{nanos}"),
        Err(error) => format!("E:{error:?}"),
    }
}

/// RFC 3339 spellings: transition instants and a fractional-second grid.
fn timestamp_inputs() -> Result<Vec<String>, Box<dyn std::error::Error>> {
    let mut inputs = Vec::new();
    for zone in ZONES {
        for (at, before, after) in transitions(zone)? {
            for delta in [-1, 0, 1] {
                let instant = at.checked_add(jiff::SignedDuration::from_millis(delta))?;
                for offset in [before, after] {
                    inputs.push(instant.display_with_offset(offset).to_string());
                }
            }
        }
    }
    for year in [
        "0000", "0001", "0100", "1000", "1582", "1600", "1752", "1800", "1883", "1900", "1901",
        "1942", "1950", "1969", "1970", "1971", "2000", "2011", "2038", "2099", "2100", "9999",
    ] {
        for rest in [
            "01-01T00:00:00",
            "02-28T12:30:45",
            "02-29T00:00:00",
            "06-15T23:59:59",
            "12-31T23:59:59",
        ] {
            for fraction in [
                "",
                ".0",
                ".1",
                ".5",
                ".9",
                ".001",
                ".999",
                ".000000001",
                ".999999999",
                ".123456789",
                ".1234567890",
            ] {
                for offset in [
                    "Z", "+00:00", "-00:00", "+00:01", "-00:01", "+05:30", "-03:30", "+05:45",
                    "+14:00", "-12:00", "+23:59", "-23:59", "+24:00",
                ] {
                    inputs.push(format!("{year}-{rest}{fraction}{offset}"));
                }
            }
        }
    }
    Ok(inputs)
}

#[test]
fn timezone_functions_reproduce_the_recorded_chrono_outputs()
-> Result<(), Box<dyn std::error::Error>> {
    let mut actual: BTreeMap<String, Fingerprint> = BTreeMap::new();
    for zone in ZONES {
        let entry = actual
            .entry(format!("naming_series_date_at {zone}"))
            .or_default();
        let mut probes: Vec<i64> = transitions(zone)?
            .into_iter()
            .map(|(at, _, _)| at.as_millisecond())
            .flat_map(|ms| [ms - 1_000, ms - 1, ms, ms + 1, ms + 1_000])
            .collect();
        // Zones without transitions (UTC) still pin the epoch neighbourhood.
        probes.extend([-1_000, -1, 0, 1, 1_000]);
        for epoch_millis in probes {
            entry.add(&epoch_millis.to_string(), &date(epoch_millis, zone));
        }
    }
    let inputs = timestamp_inputs()?;
    let canonical_entry = actual.entry("canonical_timestamp".to_owned()).or_default();
    for input in &inputs {
        canonical_entry.add(input, &canonical(input));
    }
    let key_entry = actual
        .entry("timestamp_ordering_key".to_owned())
        .or_default();
    for input in &inputs {
        key_entry.add(input, &ordering_key(input));
    }

    let recorded: BTreeMap<String, Fingerprint> = RECORDED
        .iter()
        .map(|&(category, cases, fnv1a)| (category.to_owned(), Fingerprint { cases, fnv1a }))
        .collect();
    let mut table = String::new();
    for (category, print) in &actual {
        writeln!(
            table,
            "    ({category:?}, {}, {:#018x}),",
            print.cases, print.fnv1a
        )?;
    }
    assert!(actual == recorded, "actual:\n{table}");
    Ok(())
}

/// Sampled: over every admitted input of the recorded corpus,
/// `canonical_timestamp` is idempotent and `timestamp_ordering_key` orders
/// the canonical UTC timestamps exactly as their instants (#1713). The
/// instant is read independently: `jiff` parses the canonical spelling as a
/// UTC civil date-time, whose order is the order of UTC instants.
#[test]
fn canonical_timestamps_are_idempotent_and_keys_preserve_instant_order()
-> Result<(), Box<dyn std::error::Error>> {
    let mut admitted = Vec::new();
    let mut refused = 0_usize;
    for input in timestamp_inputs()? {
        let Ok(canonical) = canonical_timestamp(&input) else {
            assert!(timestamp_ordering_key(&input).is_err(), "{input}");
            refused += 1;
            continue;
        };
        assert_eq!(
            canonical_timestamp(&canonical).as_deref(),
            Ok(canonical.as_str()),
            "{input}"
        );
        let key = timestamp_ordering_key(&input)?;
        assert_eq!(timestamp_ordering_key(&canonical), Ok(key), "{input}");
        let utc = canonical
            .strip_suffix('Z')
            .ok_or_else(|| format!("canonical `{canonical}` is not UTC"))?;
        let instant: jiff::civil::DateTime = utc.parse()?;
        admitted.push((instant, key, canonical));
    }
    assert!(admitted.len() > 25_000, "admitted {}", admitted.len());
    assert!(refused > 1_000, "refused {refused}");

    // Sorted by instant, neighbours cover every pair by transitivity.
    admitted.sort();
    for pair in admitted.windows(2) {
        let [
            (instant, key, canonical),
            (next_instant, next_key, next_canonical),
        ] = pair
        else {
            unreachable!("windows(2) yields pairs");
        };
        assert_eq!(
            key.cmp(next_key),
            instant.cmp(next_instant),
            "{canonical} and {next_canonical}"
        );
        // One instant has one canonical spelling.
        assert_eq!(
            instant == next_instant,
            canonical == next_canonical,
            "{canonical} and {next_canonical}"
        );
    }

    // The canonical strings themselves do not sort as instants, which is why
    // ordering goes through the key: `.` sorts before `Z`.
    let (whole, half) = ("2026-01-01T00:00:00Z", "2026-01-01T00:00:00.500Z");
    assert_eq!(canonical_timestamp(half).as_deref(), Ok(half));
    assert!(half < whole);
    assert!(timestamp_ordering_key(whole)? < timestamp_ordering_key(half)?);
    Ok(())
}

/// Exhaustive-finite: every spelling `2026-03-09THH:MM:SS[.FFF]±HH:MM` on the
/// grid below (115,200 spellings), each checked against its UTC instant
/// computed by plain arithmetic on the same fields (#1713). The canonical
/// form is the UTC reading of that instant, canonicalizing it again changes
/// nothing, and the ordering key is that instant exactly, so keys order the
/// grid as instants do.
#[test]
fn canonical_timestamps_and_keys_are_the_exact_instant_on_a_day_grid()
-> Result<(), Box<dyn std::error::Error>> {
    /// Unix seconds of `2026-03-09T00:00:00Z`.
    const DAY_START: i64 = 1_773_014_400;
    let mut checked = 0_u32;
    let mut keys = Vec::new();
    for hour in 0..24_i64 {
        for minute in [0_i64, 1, 29, 30, 59] {
            for second in [0_i64, 59] {
                for millis in [0_u32, 500] {
                    let fraction = if millis == 0 {
                        String::new()
                    } else {
                        format!(".{millis:03}")
                    };
                    let local = hour * 3_600 + minute * 60 + second;
                    for sign in ['+', '-'] {
                        for offset_hours in 0..24_i64 {
                            for offset_minutes in [0_i64, 15, 30, 45, 59] {
                                let input = format!(
                                    "2026-03-09T{hour:02}:{minute:02}:{second:02}{fraction}\
                                     {sign}{offset_hours:02}:{offset_minutes:02}"
                                );
                                let offset = offset_hours * 3_600 + offset_minutes * 60;
                                // RFC 3339 `-00:00` is an unknown offset: refused.
                                if sign == '-' && offset == 0 {
                                    assert!(canonical_timestamp(&input).is_err(), "{input}");
                                    assert!(timestamp_ordering_key(&input).is_err(), "{input}");
                                    continue;
                                }
                                let utc_seconds =
                                    DAY_START + local - if sign == '-' { -offset } else { offset };
                                let utc = Timestamp::from_second(utc_seconds)?
                                    .to_zoned(TimeZone::UTC)
                                    .datetime();
                                let expected = format!(
                                    "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}{fraction}Z",
                                    utc.year(),
                                    utc.month(),
                                    utc.day(),
                                    utc.hour(),
                                    utc.minute(),
                                    utc.second()
                                );
                                let canonical = canonical_timestamp(&input)?;
                                assert_eq!(canonical, expected, "{input}");
                                assert_eq!(
                                    canonical_timestamp(&canonical).as_deref(),
                                    Ok(expected.as_str()),
                                    "{input}"
                                );
                                let key = (utc_seconds, millis * 1_000_000);
                                assert_eq!(timestamp_ordering_key(&input), Ok(key), "{input}");
                                assert_eq!(timestamp_ordering_key(&canonical), Ok(key), "{input}");
                                keys.push((key, canonical));
                                checked += 1;
                            }
                        }
                    }
                }
            }
        }
    }
    assert_eq!(checked, 114_720);

    // Equal keys are equal canonical spellings, and the reverse.
    keys.sort();
    keys.dedup();
    for pair in keys.windows(2) {
        let [(key, canonical), (next_key, next_canonical)] = pair else {
            unreachable!("windows(2) yields pairs");
        };
        assert!(
            key < next_key,
            "{canonical} and {next_canonical} share a key"
        );
    }
    Ok(())
}
