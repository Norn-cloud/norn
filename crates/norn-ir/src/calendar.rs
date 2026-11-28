//! Proleptic Gregorian civil calendar conversions (Howard Hinnant's algorithm).
//!
//! Uses `div_euclid`/`rem_euclid` and checked arithmetic exclusively (no `/`
//! or `%` operators, no silent overflow).

/// Convert a proleptic Gregorian `(year, month, day)` date to signed days
/// since `1970-01-01` using Howard Hinnant's algorithm with checked arithmetic.
///
/// Returns `None` if any intermediate computation overflows `i64`.
#[must_use]
pub fn days_from_civil(year: i64, month: u32, day: u32) -> Option<i64> {
    let y = if month <= 2 {
        year.checked_sub(1)?
    } else {
        year
    };
    let era = y.div_euclid(400);
    let yoe = y.rem_euclid(400);
    let m = i64::from(month);
    let m_shifted = if month > 2 {
        m.checked_sub(3)?
    } else {
        m.checked_add(9)?
    };
    let doy = m_shifted
        .checked_mul(153)?
        .checked_add(2)?
        .div_euclid(5)
        .checked_add(i64::from(day))?
        .checked_sub(1)?;
    let doe = yoe
        .checked_mul(365)?
        .checked_add(yoe.div_euclid(4))?
        .checked_sub(yoe.div_euclid(100))?
        .checked_add(doy)?;
    era.checked_mul(146_097)?
        .checked_add(doe)?
        .checked_sub(719_468)
}

/// Convert signed days since `1970-01-01` to a proleptic Gregorian
/// `(year, month, day)` tuple using Howard Hinnant's algorithm with checked
/// arithmetic.
///
/// Returns `None` if any intermediate computation overflows `i64`.
#[must_use]
pub fn civil_from_days(days: i64) -> Option<(i64, u32, u32)> {
    let z = days.checked_add(719_468)?;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = doe
        .checked_sub(doe.div_euclid(1_460))?
        .checked_add(doe.div_euclid(36_524))?
        .checked_sub(doe.div_euclid(146_096))?
        .div_euclid(365);
    let doy = doe.checked_sub(
        yoe.checked_mul(365)?
            .checked_add(yoe.div_euclid(4))?
            .checked_sub(yoe.div_euclid(100))?,
    )?;
    let mp = doy.checked_mul(5)?.checked_add(2)?.div_euclid(153);
    let day = doy
        .checked_sub(mp.checked_mul(153)?.checked_add(2)?.div_euclid(5))?
        .checked_add(1)?;
    let month = if mp < 10 {
        mp.checked_add(3)?
    } else {
        mp.checked_sub(9)?
    };
    let year = yoe
        .checked_add(era.checked_mul(400)?)?
        .checked_add(i64::from(month <= 2))?;
    Some((year, u32::try_from(month).ok()?, u32::try_from(day).ok()?))
}

#[cfg(test)]
mod tests {
    use super::{civil_from_days, days_from_civil};

    #[test]
    fn civil_calendar_roundtrip_and_boundaries() {
        let cases = [
            (1970_i64, 1_u32, 1_u32, 0_i64),
            (1969, 12, 31, -1),
            (1970, 1, 2, 1),
            (2024, 2, 29, 19_782),
            (2000, 2, 29, 11_016),
            (0, 1, 1, -719_528),
            (9999, 12, 31, 2_932_896),
        ];
        for (year, month, day, expected_days) in cases {
            assert_eq!(days_from_civil(year, month, day), Some(expected_days));
            assert_eq!(civil_from_days(expected_days), Some((year, month, day)));
        }
        assert_eq!(days_from_civil(i64::MIN, 1, 1), None);
        assert_eq!(days_from_civil(i64::MAX, 12, 31), None);
        assert_eq!(civil_from_days(i64::MAX), None);
    }
}
