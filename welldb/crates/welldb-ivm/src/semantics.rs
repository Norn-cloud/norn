//! Exact Gregorian time parsing for the admitted metric window profile.
/// Parse admitted UTC ISO tokens exactly, without the native i64 nanosecond limit.
#[must_use]
#[allow(
    clippy::arithmetic_side_effects,
    reason = "validated 4-digit Gregorian components are bounded far inside i128"
)]
pub fn parse_time(text: &str) -> Option<i128> {
    let num = |a, b| {
        let token = text.get(a..b)?;
        if !token.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        token.parse::<i128>().ok()
    };
    if text.get(4..5)? != "-" || text.get(7..8)? != "-" {
        return None;
    }
    let (year, month, day) = (num(0, 4)?, num(5, 7)?, num(8, 10)?);
    if !(0..=9999).contains(&year) || !(1..=12).contains(&month) {
        return None;
    }
    let leap = year % 4 == 0 && (year % 100 != 0 || year % 400 == 0);
    let days = match month {
        2 => {
            if leap {
                29
            } else {
                28
            }
        }
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    };
    if day < 1 || day > days {
        return None;
    }
    let y = year - i128::from(month <= 2);
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = month + if month > 2 { -3 } else { 9 };
    let doy = (153 * mp + 2) / 5 + day - 1;
    let epoch_days = era * 146_097 + yoe * 365 + yoe / 4 - yoe / 100 + doy - 719_468;
    let mut nanos = epoch_days * 86400 * 1_000_000_000;
    if text.len() == 10 {
        return Some(nanos);
    }
    if text.get(10..11)? != "T" || text.get(13..14)? != ":" || text.get(16..17)? != ":" {
        return None;
    }
    let (hour, minute, second) = (num(11, 13)?, num(14, 16)?, num(17, 19)?);
    if hour > 23 || minute > 59 || second > 59 {
        return None;
    }
    nanos += (hour * 3600 + minute * 60 + second) * 1_000_000_000;
    let (body, offset) = if let Some(body) = text.strip_suffix('Z') {
        (body, 0)
    } else {
        let offset_start = text.len().checked_sub(6)?;
        let zone = text.get(offset_start..)?;
        if zone == "-00:00" || zone.get(3..4)? != ":" {
            return None;
        }
        let sign = match zone.get(..1)? {
            "+" => 1,
            "-" => -1,
            _ => return None,
        };
        let offset_hour = num(offset_start + 1, offset_start + 3)?;
        let offset_minute = num(offset_start + 4, offset_start + 6)?;
        if offset_hour > 23 || offset_minute > 59 {
            return None;
        }
        (
            text.get(..offset_start)?,
            sign * (offset_hour * 3600 + offset_minute * 60) * 1_000_000_000,
        )
    };
    let suffix = body.get(19..)?;
    if !suffix.is_empty() {
        let fraction = suffix.strip_prefix('.')?;
        if fraction.is_empty()
            || fraction.len() > 9
            || !fraction.bytes().all(|b| b.is_ascii_digit())
        {
            return None;
        }
        let places = u32::try_from(9_usize.checked_sub(fraction.len())?).ok()?;
        nanos += fraction.parse::<i128>().ok()? * 10_i128.pow(places);
    }
    nanos -= offset;
    // UTC normalization must retain a four-digit year, as Norn requires.
    if !(-62_167_219_200_000_000_000..253_402_300_800_000_000_000).contains(&nanos) {
        return None;
    }
    Some(nanos)
}
