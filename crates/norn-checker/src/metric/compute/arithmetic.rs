//! COMPUTE-DETERMINISTIC-1: exact coefficients, with rounding only at ratio output.

/// Supported decimal places for query inputs, sums and dimensionless ratios.
pub const SCALE: u32 = 6;
/// One unit in the coefficient representation.
pub const FACTOR: i128 = 1_000_000;
/// Per-object coefficient bound; the evaluator refuses larger values.
pub const MAX_VALUE: i64 = 9_000_000_000_000_000_000;
/// At most 1,000 objects, each within `MAX_VALUE`; every sum fits this range.
pub const MAX_SUM: i128 = 9_000_000_000_000_000_000_000;

/// Add one signed contribution without rounding or a partial overflow result.
#[must_use]
pub fn add(total: i128, value: i64) -> Option<i128> {
    if !(-MAX_VALUE..=MAX_VALUE).contains(&value) {
        return None;
    }
    total
        .checked_add(i128::from(value))
        .filter(|v| (-MAX_SUM..=MAX_SUM).contains(v))
}

/// Divide same-unit sums, rounding once to scale six, midpoint away from zero.
/// Zero divisors and coefficients outside the bounded query profile refuse.
#[must_use]
pub fn ratio(numerator: i128, denominator: i128) -> Option<i128> {
    if !(-MAX_SUM..=MAX_SUM).contains(&numerator) || !(-MAX_SUM..=MAX_SUM).contains(&denominator) {
        return None;
    }
    let scaled = numerator.checked_mul(FACTOR)?;
    let quotient = scaled.checked_div(denominator)?;
    let remainder = scaled.checked_rem(denominator)?.unsigned_abs();
    if remainder >= denominator.unsigned_abs().checked_sub(remainder)? {
        let direction = if (numerator < 0) == (denominator < 0) {
            1
        } else {
            -1
        };
        quotient.checked_add(direction)
    } else {
        Some(quotient)
    }
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::expect_used,
        missing_docs,
        reason = "arithmetic conformance assertions"
    )]
    use super::*;
    use proptest::prelude::*;

    #[test]
    fn query_ratio_rounding_and_bound_refusals() {
        assert_eq!(
            norn_ir::ResourceLimitProfile::norn_limits_v1()
                .slice
                .max_records,
            1000
        );
        assert_eq!(ratio(294_000, 300_000), Some(980_000));
        assert_eq!(ratio(292_500, 300_000), Some(975_000));
        for (n, d, expected) in [
            (1, 128, 7813),
            (-1, 128, -7813),
            (1, -128, -7813),
            (-1, -128, 7813),
        ] {
            assert_eq!(ratio(n, d), Some(expected));
        }
        assert_eq!(ratio(0, 0), None);
        assert_eq!(ratio(i128::MAX, 1), None);
        assert_eq!(add(MAX_SUM, 1), None);
        assert_eq!(add(0, i64::MIN), None);
    }

    proptest! {
        #[test]
        fn exact_sum_is_commutative(a in -MAX_VALUE..=MAX_VALUE, b in -MAX_VALUE..=MAX_VALUE) {
            prop_assert_eq!(add(i128::from(a), b), add(i128::from(b), a));
        }
        #[test]
        fn ratio_is_within_half_a_quantum(n in any::<i64>(), d in any::<i64>()) {
            let actual = ratio(i128::from(n), i128::from(d));
            if d == 0 { prop_assert!(actual.is_none()); }
            else {
                let q = actual.expect("i64 operands fit the wider ratio intermediates");
                let error = q.checked_mul(i128::from(d)).expect("product")
                    .checked_sub(i128::from(n).checked_mul(FACTOR).expect("scaled numerator")).expect("difference");
                prop_assert!(error.unsigned_abs().checked_mul(2).expect("double error") <= i128::from(d).unsigned_abs());
                prop_assert_eq!(ratio(-i128::from(n), i128::from(d)), Some(-q));
            }
        }
    }
}
