//! Allocation-free decimal core shared by ledger lowering and direct proofs.

/// VAL-EXACT-1: remove only representational zeros; retain the exact value.
pub(super) fn canonical_parts(mut unscaled: i128, mut scale: u8) -> (i128, u8) {
    if unscaled == 0 {
        return (0, 0);
    }
    while scale > 0 && unscaled % 10 == 0 {
        unscaled /= 10;
        let Some(next_scale) = scale.checked_sub(1) else {
            break;
        };
        scale = next_scale;
    }
    (unscaled, scale)
}

/// VAL-EXACT-1: exact decimal alignment multiplier, or overflow refusal.
pub(super) fn checked_pow10(exponent: u8) -> Option<i128> {
    let mut value = 1_i128;
    for _ in 0..exponent {
        value = value.checked_mul(10)?;
    }
    Some(value)
}

#[cfg(test)]
mod tests {
    use super::{canonical_parts, checked_pow10};
    use proptest::prelude::*;

    #[test]
    fn val_exact_scale_boundaries() {
        assert_eq!(canonical_parts(0, u8::MAX), (0, 0));
        assert_eq!(canonical_parts(-100, 2), (-1, 0));
        assert_eq!(canonical_parts(i128::MIN, 28), (i128::MIN, 28));
        assert_eq!(canonical_parts(i128::MAX, 28), (i128::MAX, 28));
        assert_eq!(checked_pow10(0), Some(1));
        assert_eq!(
            checked_pow10(38),
            Some(100_000_000_000_000_000_000_000_000_000_000_000_000)
        );
        assert_eq!(checked_pow10(39), None);
        assert_eq!(checked_pow10(u8::MAX), None);
    }

    proptest! {
        #[test]
        fn canonical_decimal_is_exact_and_idempotent(unscaled in any::<i128>(), scale in any::<u8>()) {
            let (coefficient, normalized_scale) = canonical_parts(unscaled, scale);
            prop_assert!(normalized_scale <= scale);
            prop_assert_eq!(canonical_parts(coefficient, normalized_scale), (coefficient, normalized_scale));
            if unscaled != 0 {
                let gap = scale.checked_sub(normalized_scale).ok_or_else(|| TestCaseError::fail("normalization increased scale"))?;
                let factor = checked_pow10(gap);
                prop_assert_eq!(factor.and_then(|factor| coefficient.checked_mul(factor)), Some(unscaled));
                prop_assert!(normalized_scale == 0 || coefficient % 10 != 0);
            } else {
                prop_assert_eq!((coefficient, normalized_scale), (0, 0));
            }
        }
    }

    #[test]
    fn decimal_multiplier_refuses_exactly_on_overflow() {
        for exponent in 0..=u8::MAX {
            assert_eq!(
                checked_pow10(exponent),
                10_i128.checked_pow(u32::from(exponent)),
                "exponent {exponent}"
            );
            assert_eq!(
                checked_pow10(exponent).is_none(),
                exponent > 38,
                "exponent {exponent}"
            );
        }
    }
}
