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

/// The pre-2100 transitions of `zone`, with the offsets before and after.
fn transitions(zone: &str) -> Vec<(Timestamp, Offset, Offset)> {
    let data = TzdbBundle::v2025b().tzif(zone).expect("zone in tzdb 2025b");
    let tz = TimeZone::tzif(zone, data).expect("TZif parses");
    let from = Timestamp::from_second(FROM_SECOND).expect("1800");
    let until = Timestamp::from_second(UNTIL_SECOND).expect("2100");
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
    out
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
fn timestamp_inputs() -> Vec<String> {
    let mut inputs = Vec::new();
    for zone in ZONES {
        for (at, before, after) in transitions(zone) {
            for delta in [-1, 0, 1] {
                let instant = at
                    .checked_add(jiff::SignedDuration::from_millis(delta))
                    .expect("in range");
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
    inputs
}

#[test]
fn timezone_functions_reproduce_the_recorded_chrono_outputs() {
    let mut actual: BTreeMap<String, Fingerprint> = BTreeMap::new();
    for zone in ZONES {
        let entry = actual
            .entry(format!("naming_series_date_at {zone}"))
            .or_default();
        let mut probes: Vec<i64> = transitions(zone)
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
    let inputs = timestamp_inputs();
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
        )
        .expect("write to a String");
    }
    assert!(actual == recorded, "actual:\n{table}");
}
