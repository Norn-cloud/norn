//! Deterministic `SplitMix64` PRNG, ISO-week UTC calendar helpers, exact scaled
//! decimal utilities, and latency percentile aggregators for the xP&A DST and
//! benchmark harness.
//!
//! Pure Rust with zero external RNG or GPL/AGPL dependencies. Override the
//! default deterministic seed batch with `XPA_DST_SEED=<u64>` (decimal or
//! `0x`-prefixed hex) to replay any failing scenario deterministically.

use std::time::Duration;

use norn_checker::metric::compute::arithmetic::{FACTOR, ratio};
use norn_ir::Decimal;

/// Primary deterministic seed for single-seed runs.
pub const PRIMARY_SEED: u64 = 0x5850_415F_4453_5401;
/// Secondary deterministic seed for multi-cell isolation runs.
pub const SECONDARY_SEED: u64 = 0x5850_415F_4453_5402;
/// Default deterministic seed batch run in normal CI test executions.
pub const DEFAULT_SEEDS: [u64; 3] = [PRIMARY_SEED, SECONDARY_SEED, 0x5850_415F_4453_5403];

/// Canonical anchor Monday: `2026-07-06` (`W1` of the 16-week retail truth window).
pub const BASE_MONDAY_YEAR: i32 = 2026;
pub const BASE_MONDAY_MONTH: i8 = 7;
pub const BASE_MONDAY_DAY: i8 = 6;
/// Unix epoch milliseconds for `2026-07-06T00:00:00Z`.
pub const BASE_MONDAY_MS: u64 = 1_783_296_000_000;
/// Milliseconds in one UTC day (`86_400_000`).
pub const DAY_MS: u64 = 86_400_000;
/// Milliseconds in one UTC week (`7 * 86_400_000`).
pub const WEEK_MS: u64 = DAY_MS.saturating_mul(7);
/// Half of `FACTOR` (`500_000`), used for exact half-away-from-zero decimal rounding.
pub const HALF_FACTOR: i128 = 500_000;
/// One cent in 6-decimal scaled units (`10_000`).
pub const CENT_SCALE: i128 = 10_000;

/// Scale a whole integer quantity into 6-decimal `i128` units (`units * FACTOR`).
#[must_use]
pub const fn scaled(units: i128) -> i128 {
    units.saturating_mul(FACTOR)
}

/// Checked remainder helper (`a % b`, returning `0` when `b == 0`).
#[must_use]
pub const fn rem_usize(value: usize, divisor: usize) -> usize {
    match value.checked_rem(divisor) {
        Some(rem) => rem,
        None => 0,
    }
}

/// Checked integer division helper (`a / b`, returning `0` when `b == 0`).
#[must_use]
pub const fn div_usize(value: usize, divisor: usize) -> usize {
    match value.checked_div(divisor) {
        Some(q) => q,
        None => 0,
    }
}

/// Checked remainder helper for `u32` (`a % b`, returning `0` when `b == 0`).
#[must_use]
pub const fn rem_u32(value: u32, divisor: u32) -> u32 {
    match value.checked_rem(divisor) {
        Some(rem) => rem,
        None => 0,
    }
}

/// Checked integer division helper for `u64` (`a / b`, returning `0` when `b == 0`).
#[must_use]
pub const fn div_u64(value: u64, divisor: u64) -> u64 {
    match value.checked_div(divisor) {
        Some(q) => q,
        None => 0,
    }
}

/// Checked remainder helper for `u64` (`a % b`, returning `0` when `b == 0`).
#[must_use]
pub const fn rem_u64(value: u64, divisor: u64) -> u64 {
    match value.checked_rem(divisor) {
        Some(rem) => rem,
        None => 0,
    }
}

/// Checked integer division helper for `i128` (`a / b`, returning `0` when `b == 0`).
#[must_use]
pub const fn div_i128(value: i128, divisor: i128) -> i128 {
    match value.checked_div(divisor) {
        Some(q) => q,
        None => 0,
    }
}

/// Deterministic 64-bit `SplitMix64` PRNG (Steele, Lea, Flood 2014; public domain).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SplitMix64 {
    state: u64,
}

impl SplitMix64 {
    /// Create a generator from an explicit 64-bit seed.
    #[must_use]
    pub const fn new(seed: u64) -> Self {
        Self { state: seed }
    }

    /// Derive a domain-separated child generator from this seed and a tag.
    #[must_use]
    pub fn fork(&self, domain_tag: u64) -> Self {
        let mut child = Self::new(self.state ^ domain_tag.wrapping_mul(0x9E37_79B9_7F4A_7C15));
        let mixed = child.next_u64();
        Self::new(mixed)
    }

    /// Next deterministic 64-bit pseudorandom word.
    pub const fn next_u64(&mut self) -> u64 {
        self.state = self.state.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.state;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// Uniform integer in `[0, upper)` (returns `0` when `upper == 0`).
    pub fn next_below(&mut self, upper: u64) -> u64 {
        if upper == 0 {
            return 0;
        }
        let zone = u64::MAX.wrapping_sub(rem_u64(u64::MAX, upper));
        loop {
            let sample = self.next_u64();
            if sample < zone {
                return rem_u64(sample, upper);
            }
        }
    }

    /// Uniform integer in `[low, high]` inclusive.
    pub fn range_u64(&mut self, low: u64, high: u64) -> u64 {
        if high <= low {
            return low;
        }
        let span = high.saturating_sub(low).saturating_add(1);
        low.saturating_add(self.next_below(span))
    }

    /// Uniform `usize` in `[0, len)` (returns `0` when `len == 0`).
    pub fn index(&mut self, len: usize) -> usize {
        let Ok(upper) = u64::try_from(len) else {
            return 0;
        };
        usize::try_from(self.next_below(upper)).unwrap_or(0)
    }

    /// Bernoulli trial with probability `num / den`.
    pub fn chance(&mut self, num: u64, den: u64) -> bool {
        den > 0 && self.next_below(den) < num
    }

    /// Deterministic Fisher-Yates shuffle in place.
    pub fn shuffle<T>(&mut self, items: &mut [T]) {
        let mut idx = items.len();
        while idx > 1 {
            idx = idx.saturating_sub(1);
            let swap = self.index(idx.saturating_add(1));
            items.swap(idx, swap);
        }
    }

    /// Derive a deterministic 32-byte entropy seed for `norn_kernel::Entropy`.
    pub fn next_entropy_bytes(&mut self) -> [u8; 32] {
        let mut out = [0_u8; 32];
        for chunk in out.chunks_exact_mut(8) {
            chunk.copy_from_slice(&self.next_u64().to_le_bytes());
        }
        out
    }
}

/// Resolve the active DST seed batch: honors `XPA_DST_SEED` (decimal or `0x`
/// hex) for instant single-seed replay, otherwise returns [`DEFAULT_SEEDS`].
///
/// # Errors
/// Returns an error string if `XPA_DST_SEED` is set to an unparseable value.
pub fn active_seeds() -> Result<Vec<u64>, String> {
    match std::env::var("XPA_DST_SEED") {
        Ok(raw) => {
            let trimmed = raw.trim();
            let parsed = if let Some(hex) = trimmed
                .strip_prefix("0x")
                .or_else(|| trimmed.strip_prefix("0X"))
            {
                u64::from_str_radix(hex, 16)
            } else {
                trimmed.parse::<u64>()
            }
            .map_err(|e| format!("invalid XPA_DST_SEED `{trimmed}`: {e}"))?;
            Ok(vec![parsed])
        }
        Err(_) => Ok(DEFAULT_SEEDS.to_vec()),
    }
}

/// Return the canonical UTC ISO-8601 date (`YYYY-MM-DD`) for `week_index`
/// weeks after `2026-07-06` (Monday), plus `day_offset` days (`0..=6`).
///
/// # Errors
/// Returns an error if the date offset overflows `jiff::civil::Date`.
pub fn utc_date_iso(week_index: u32, day_offset: u32) -> Result<String, String> {
    let base = jiff::civil::date(BASE_MONDAY_YEAR, BASE_MONDAY_MONTH, BASE_MONDAY_DAY);
    let total_days = i64::from(week_index)
        .checked_mul(7)
        .and_then(|w| w.checked_add(i64::from(day_offset)))
        .ok_or_else(|| format!("date offset overflow: week={week_index}, day={day_offset}"))?;
    let date = base
        .checked_add(jiff::Span::new().days(total_days))
        .map_err(|e| format!("date add {total_days}d: {e}"))?;
    Ok(date.to_string())
}

/// Canonical ISO-week Monday (`YYYY-MM-DD`) for `week_index` (`0` = `2026-07-06`).
///
/// # Errors
/// Returns an error if `week_index` overflows date arithmetic.
pub fn week_monday_iso(week_index: u32) -> Result<String, String> {
    utc_date_iso(week_index, 0)
}

/// Canonical UTC timestamp (`YYYY-MM-DDTHH:00:00Z`) inside `week_index`.
///
/// # Errors
/// Returns an error if date arithmetic fails.
pub fn week_timestamp_utc(week_index: u32, day_offset: u32, hour: u8) -> Result<String, String> {
    let date = utc_date_iso(week_index, day_offset)?;
    let clamped_hour = hour.min(23);
    Ok(format!("{date}T{clamped_hour:02}:00:00Z"))
}

/// Milliseconds since Unix epoch for `(week_index, day_offset, hour)`.
#[must_use]
pub fn week_epoch_ms(week_index: u32, day_offset: u32, hour: u8) -> u64 {
    let week_off = u64::from(week_index).saturating_mul(WEEK_MS);
    let day_off = u64::from(day_offset).saturating_mul(DAY_MS);
    let hour_off = u64::from(hour.min(23)).saturating_mul(3_600_000);
    BASE_MONDAY_MS
        .saturating_add(week_off)
        .saturating_add(day_off)
        .saturating_add(hour_off)
}

/// Convert a [`Decimal`] (scale <= 6) into a checked 6-decimal scaled `i128`.
///
/// # Errors
/// Returns an error on scale > 6 or arithmetic overflow.
pub fn decimal_to_scaled(value: Decimal) -> Result<i128, String> {
    let text = value.canonical();
    let (negative, unsigned) = match text.strip_prefix('-') {
        Some(rest) => (true, rest),
        None => (false, text.as_str()),
    };
    let (whole_str, frac_str) = unsigned.split_once('.').unwrap_or((unsigned, ""));
    let whole: i128 = whole_str
        .parse()
        .map_err(|e| format!("parse whole `{text}`: {e}"))?;
    if frac_str.len() > 6 {
        return Err(format!("scale > 6 unsupported: `{text}`"));
    }
    let mut padded = frac_str.to_owned();
    while padded.len() < 6 {
        padded.push('0');
    }
    let frac: i128 = padded
        .parse()
        .map_err(|e| format!("parse frac `{text}`: {e}"))?;
    let magnitude = whole
        .checked_mul(FACTOR)
        .and_then(|w| w.checked_add(frac))
        .ok_or_else(|| format!("scaled overflow: `{text}`"))?;
    if negative {
        magnitude
            .checked_neg()
            .ok_or_else(|| format!("negation overflow: `{text}`"))
    } else {
        Ok(magnitude)
    }
}

/// Format a 6-decimal scaled `i128` as a canonical [`Decimal`].
///
/// # Errors
/// Returns an error if the formatted decimal fails canonical parsing.
pub fn decimal_from_scaled(scaled_val: i128) -> Result<Decimal, String> {
    let negative = scaled_val < 0;
    let abs = scaled_val.unsigned_abs();
    let factor = u128::try_from(FACTOR).map_err(|e| format!("factor: {e}"))?;
    let whole = abs.checked_div(factor).unwrap_or(0);
    let frac = abs.checked_rem(factor).unwrap_or(0);
    let sign = if negative && abs > 0 { "-" } else { "" };
    let text = if frac == 0 {
        format!("{sign}{whole}")
    } else {
        format!("{sign}{whole}.{frac:06}")
    };
    Decimal::parse(&text).map_err(|e| format!("parse `{text}`: {e}"))
}

/// Checked exact ratio-of-sums using Norn's own `metric::compute::arithmetic::ratio`.
///
/// Returns `Ok(None)` when the denominator is zero (withheld, never `NaN`/`Inf`).
///
/// # Errors
/// Returns an error if ratio arithmetic overflows.
pub fn exact_ratio_of_sums(
    numerator_scaled: i128,
    denominator_scaled: i128,
) -> Result<Option<Decimal>, String> {
    if denominator_scaled == 0 {
        return Ok(None);
    }
    ratio(numerator_scaled, denominator_scaled).map_err(|e| format!("exact ratio overflow: {e:?}"))
}

/// Latency distribution summary in microseconds (`p50`, `p95`, `p99`, `max`).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LatencySummary {
    pub samples: usize,
    pub p50_us: u64,
    pub p95_us: u64,
    pub p99_us: u64,
    pub max_us: u64,
}

impl LatencySummary {
    /// Compute deterministic nearest-rank latency percentiles from durations.
    #[must_use]
    pub fn from_durations(durations: &[Duration]) -> Self {
        if durations.is_empty() {
            return Self {
                samples: 0,
                p50_us: 0,
                p95_us: 0,
                p99_us: 0,
                max_us: 0,
            };
        }
        let mut micros: Vec<u64> = durations
            .iter()
            .map(|d| u64::try_from(d.as_micros()).unwrap_or(u64::MAX))
            .collect();
        micros.sort_unstable();
        let max_us = micros.last().copied().unwrap_or(0);
        Self {
            samples: micros.len(),
            p50_us: percentile(&micros, 50),
            p95_us: percentile(&micros, 95),
            p99_us: percentile(&micros, 99),
            max_us,
        }
    }
}

fn percentile(sorted: &[u64], pct: usize) -> u64 {
    if sorted.is_empty() {
        return 0;
    }
    let last = sorted.len().saturating_sub(1);
    let rank = div_usize(last.saturating_mul(pct), 100);
    sorted.get(rank).copied().unwrap_or(0)
}
