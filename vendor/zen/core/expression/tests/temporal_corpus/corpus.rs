//! Norn (#1436): the deterministic temporal corpus. Every expression is
//! `(category, source)`; `tests/temporal_corpus.rs` checks the engine's output
//! for each category against digests recorded from the chrono implementation
//! this crate used before it moved to jiff (the differential run is recorded
//! on #1436).

/// SplitMix64: a fixed, dependency-free stream, so the corpus never drifts.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }

    fn range(&mut self, lo: i64, hi: i64) -> i64 {
        lo + (self.next() % ((hi - lo) as u64 + 1)) as i64
    }

    fn chance(&mut self, percent: u64) -> bool {
        self.below(100) < percent
    }

    fn pick<'a, T>(&mut self, items: &'a [T]) -> &'a T {
        &items[self.below(items.len() as u64) as usize]
    }
}

const YEARS: &[i64] = &[
    0, 1, 2, 3, 4, 99, 100, 101, 399, 400, 401, 1582, 1600, 1700, 1752, 1800, 1899, 1900, 1901,
    1969, 1970, 1971, 1999, 2000, 2001, 2004, 2024, 2025, 2026, 2037, 2038, 2099, 2100, 2101, 2400,
    9998, 9999,
];

fn date_text(r: &mut Rng) -> String {
    match r.below(10) {
        0..=5 => {
            let y = if r.chance(40) {
                *r.pick(YEARS)
            } else {
                r.range(0, 9999)
            };
            let m = if r.chance(90) {
                r.range(1, 12)
            } else {
                r.range(0, 19)
            };
            let d = if r.chance(85) {
                r.range(1, 31)
            } else {
                r.range(0, 39)
            };
            format!("{y:04}-{m:02}-{d:02}")
        }
        6..=7 => {
            let y = r.range(0, 9999);
            let m = r.range(1, 12);
            let d = r.range(27, 32);
            format!("{y:04}-{m:02}-{d:02}")
        }
        _ => {
            const ALPHABET: &[u8] = b"0123456789-0123456789-T:Z+. /x";
            let len = r.range(8, 12);
            (0..len).map(|_| *r.pick(ALPHABET) as char).collect()
        }
    }
}

fn two(r: &mut Rng, common_max: i64, wide_percent: u64) -> String {
    let v = if r.chance(wide_percent) {
        r.range(0, 99)
    } else {
        r.range(0, common_max)
    };
    format!("{v:02}")
}

fn timestamp_text(r: &mut Rng) -> String {
    let mut s = date_text(r);
    if r.chance(30) {
        let y = r.range(0, 9999);
        let m = r.range(1, 12);
        let d = r.range(1, 28);
        s = format!("{y:04}-{m:02}-{d:02}");
    }
    s.push(match r.below(20) {
        0 => 't',
        1 => ' ',
        _ => 'T',
    });
    s += &two(r, 24, 5);
    s.push(':');
    s += &two(r, 60, 5);
    s.push(':');
    s += &two(r, 61, 5);
    match r.below(10) {
        0..=4 => {}
        5 => s.push('.'),
        _ => {
            s.push('.');
            for _ in 0..r.range(1, 11) {
                s.push((b'0' + r.below(10) as u8) as char);
            }
        }
    }
    match r.below(12) {
        0..=3 => s.push('Z'),
        4 => s.push('z'),
        5 => s += "-00:00",
        6 => {}
        7 => s += &format!("+{}{}", two(r, 23, 10), two(r, 59, 10)),
        _ => {
            s.push(if r.chance(50) { '+' } else { '-' });
            s += &two(r, 24, 10);
            s.push(':');
            s += &two(r, 60, 10);
        }
    }
    if r.chance(5) {
        const ALPHABET: &[u8] = b"0123456789-T:Z+.z ";
        let mut bytes = s.into_bytes();
        let i = r.below(bytes.len() as u64) as usize;
        bytes[i] = *r.pick(ALPHABET);
        s = String::from_utf8(bytes).expect("ASCII");
    }
    s
}

fn dates() -> Vec<String> {
    let mut years: Vec<i64> = YEARS.to_vec();
    years.extend(1995..=2030);
    years.sort_unstable();
    years.dedup();
    let mut out = Vec::new();
    for y in years {
        for m in 0..=13 {
            for d in 0..=32 {
                out.push(format!("{y:04}-{m:02}-{d:02}"));
            }
        }
    }
    out
}

fn edge_timestamps() -> Vec<String> {
    let mut out = Vec::new();
    for date in [
        "0000-01-01",
        "0000-12-31",
        "0001-01-01",
        "0001-01-02",
        "1969-12-31",
        "1970-01-01",
        "2000-02-29",
        "9999-12-30",
        "9999-12-31",
        "2024-02-29",
        "2023-02-29",
    ] {
        for time in [
            "00:00:00", "00:00:59", "23:59:59", "12:00:00", "23:59:60", "24:00:00",
        ] {
            for fraction in [
                "",
                ".0",
                ".000",
                ".5",
                ".999999999",
                ".000000001",
                ".123400000",
                ".1234567890",
            ] {
                for offset in [
                    "Z", "+00:00", "-00:00", "+00:01", "-00:01", "+23:59", "-23:59", "+24:00",
                    "-24:00", "+14:00", "-12:00", "+05:30", "+00:60", "-01:60",
                ] {
                    out.push(format!("{date}T{time}{fraction}{offset}"));
                }
            }
        }
    }
    out
}

/// The literal is safe to quote: no quote or backslash.
fn quotable(text: &str) -> bool {
    !text.contains('\'') && !text.contains('\\')
}

/// Whether the old (and new) engine admits the literal, decided by the
/// literal's shape alone so the corpus does not depend on the engine.
fn plausible_date(text: &str) -> bool {
    let b = text.as_bytes();
    b.len() == 10
        && b[4] == b'-'
        && b[7] == b'-'
        && b.iter()
            .enumerate()
            .all(|(i, c)| i == 4 || i == 7 || c.is_ascii_digit())
        && (1..=12).contains(&text[5..7].parse::<u32>().unwrap_or(0))
        && (1..=28).contains(&text[8..10].parse::<u32>().unwrap_or(0))
        && text[..4] != *"0000"
}

/// The whole corpus, in a fixed order.
pub fn cases() -> Vec<(&'static str, String)> {
    let mut r = Rng(0x1436_2026_1118);
    let mut date_texts = dates();
    for _ in 0..20_000 {
        date_texts.push(date_text(&mut r));
    }
    let mut stamp_texts = edge_timestamps();
    for _ in 0..60_000 {
        stamp_texts.push(timestamp_text(&mut r));
    }
    let mut out = Vec::new();
    for d in date_texts.iter().filter(|d| quotable(d)) {
        out.push(("date", format!("string(date('{d}'))")));
    }
    for t in stamp_texts.iter().filter(|t| quotable(t)) {
        out.push(("timestamp", format!("string(timestamp('{t}'))")));
    }

    let valid_dates: Vec<&String> = date_texts.iter().filter(|d| plausible_date(d)).collect();
    let valid_stamps: Vec<String> = (0..4_000)
        .map(|_| {
            let date = r.pick(&valid_dates).to_string();
            let fraction = match r.below(4) {
                0 => String::new(),
                1 => format!(".{:03}", r.below(1_000)),
                2 => format!(".{:06}", r.below(1_000_000)),
                _ => format!(".{:09}", r.below(1_000_000_000)),
            };
            let offset = if r.chance(40) {
                "Z".to_owned()
            } else {
                let sign = if r.chance(50) { '+' } else { '-' };
                format!("{sign}{:02}:{:02}", r.range(0, 23), r.range(0, 59))
            };
            format!(
                "{date}T{:02}:{:02}:{:02}{fraction}{offset}",
                r.range(0, 23),
                r.range(0, 59),
                r.range(0, 59)
            )
        })
        .collect();
    const DAYS: &[&str] = &[
        "0",
        "1",
        "-1",
        "365",
        "-366",
        "3652058",
        "-3652058",
        "3652059",
        "-3652059",
        "7304484",
        "-7304484",
        "7304485",
        "9223372036854775807",
        "-9223372036854775808",
        "1.5",
        "-0.5",
        "1e3",
        "99999999999999999999999",
    ];
    const SECONDS: &[&str] = &[
        "0",
        "1",
        "-1",
        "86400",
        "-86400",
        "315537897599",
        "-315537897599",
        "315537897600",
        "9223372036854775",
        "9223372036854776",
        "-9223372036854776",
        "9223372036854775807",
        "0.5",
        "-1.25",
    ];
    for _ in 0..3_000 {
        let (a, b) = (
            r.pick(&valid_dates).to_string(),
            r.pick(&valid_dates).to_string(),
        );
        out.push((
            "days_between",
            format!("days_between(date('{a}'), date('{b}'))"),
        ));
        let n = if r.chance(50) {
            r.pick(DAYS).to_string()
        } else {
            r.range(-4_000_000, 4_000_000).to_string()
        };
        out.push(("add_days", format!("string(add_days(date('{a}'), {n}))")));
        let (x, y) = (r.pick(&valid_stamps).clone(), r.pick(&valid_stamps).clone());
        out.push((
            "seconds_between",
            format!("seconds_between(timestamp('{x}'), timestamp('{y}'))"),
        ));
        let n = if r.chance(50) {
            r.pick(SECONDS).to_string()
        } else {
            r.range(-400_000_000_000, 400_000_000_000).to_string()
        };
        out.push((
            "add_seconds",
            format!("string(add_seconds(timestamp('{x}'), {n}))"),
        ));
        for op in ["<", "<=", "==", "!=", ">", ">="] {
            out.push(("compare", format!("date('{a}') {op} date('{b}')")));
            out.push(("compare", format!("timestamp('{x}') {op} timestamp('{y}')")));
        }
        out.push(("compare", format!("date('{a}') < timestamp('{x}')")));
        out.push((
            "seconds_between",
            format!("seconds_between(timestamp('{x}'), timestamp('{x}'))"),
        ));
    }

    // Both ends of the 0001..9999 range.
    const NEAR: &[&str] = &[
        "0001-01-01T00:00:00Z",
        "0001-01-01T00:00:00.5Z",
        "0001-01-02T03:04:05+14:00",
        "9999-12-31T23:59:59.999999999Z",
        "9999-12-31T00:00:00Z",
        "9999-12-30T22:00:00Z",
        "9999-12-30T21:59:59Z",
        "9999-12-31T12:00:00-11:00",
        "9999-12-31T23:59:59+23:59",
        "9999-12-30T23:59:59-23:59",
    ];
    for x in NEAR {
        for n in -200_000_i64..=200_000 {
            if n % 9_973 == 0 || n.abs() < 50 || (n.abs() - 86_400).abs() < 3 {
                out.push((
                    "add_seconds",
                    format!("string(add_seconds(timestamp('{x}'), {n}))"),
                ));
            }
        }
        for y in NEAR {
            out.push((
                "seconds_between",
                format!("seconds_between(timestamp('{x}'), timestamp('{y}'))"),
            ));
        }
    }
    for d in [
        "0001-01-01",
        "0001-01-02",
        "9999-12-31",
        "9999-12-30",
        "5000-02-29",
    ] {
        for n in -300_i64..=300 {
            out.push(("add_days", format!("string(add_days(date('{d}'), {n}))")));
        }
        out.push((
            "days_between",
            format!("days_between(date('{d}'), date('0001-01-01'))"),
        ));
        out.push((
            "days_between",
            format!("days_between(date('{d}'), date('9999-12-31'))"),
        ));
    }

    // The legacy API (ambient clock, chrono-tz, humantime durations) is not
    // registered: every spelling stays refused.
    const HUMANTIME: &[&str] = &[
        "1ns",
        "1nsec",
        "1nanos",
        "1us",
        "1usec",
        "1micros",
        "1ms",
        "1msec",
        "1millis",
        "1s",
        "1sec",
        "1secs",
        "1second",
        "1seconds",
        "1m",
        "1min",
        "1mins",
        "1minute",
        "1minutes",
        "1h",
        "1hr",
        "1hrs",
        "1hour",
        "1hours",
        "1d",
        "1day",
        "1days",
        "1w",
        "1week",
        "1weeks",
        "1M",
        "1month",
        "1months",
        "1y",
        "1year",
        "1years",
        "1h 30m",
        "1h30m",
        "2 hours",
        "1year 2months 3days",
        "",
        "x",
        "-1h",
        "1.5h",
        "18446744073709551616s",
    ];
    for h in HUMANTIME {
        out.push(("legacy", format!("duration('{h}')")));
        out.push(("legacy", format!("d('2023-01-01').add('{h}')")));
        out.push(("legacy", format!("d('2023-01-01').sub('{h}')")));
    }
    for source in [
        "d()",
        "d('2023-01-01')",
        "d('2023-01-01 10:30', 'Europe/Berlin')",
        "date('2023-09-18T12:00:00Z') > 0",
        "time('12:00:00')",
        "now()",
        "year(1695038400)",
        "dayOfWeek(1695038400)",
        "dayOfMonth(1695038400)",
        "dayOfYear(1695038400)",
        "weekOfYear(1695038400)",
        "monthOfYear(1695038400)",
        "monthString(1695038400)",
        "dateString(1695038400)",
        "weekdayString(1695038400)",
        "startOf('2023-01-01 15:45:01', 'day')",
        "endOf('2023-01-01 15:45:01', 'd')",
        "d('2024-01-01').format('%Y-%m')",
        "d('2024-01-01').tz('Europe/Berlin')",
        "d('2024-01-01').isSame(d('2024-01-02'), 'day')",
        "[d('2024-01-01')..d('2024-12-31')]",
        "d('2024-01-01').isLeapYear()",
        "d('2024-01-01').offsetName()",
        "d('2024-01-01').diff(d('2023-01-01'), 'month')",
    ] {
        out.push(("legacy", source.to_owned()));
    }
    out
}

/// FNV-1a over every `(source, output)` of a category, in corpus order: a
/// dependency-free fingerprint of the recorded outputs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Fingerprint {
    pub cases: u64,
    pub fnv1a: u64,
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
    pub fn add(&mut self, source: &str, output: &str) {
        self.cases += 1;
        for byte in source
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
