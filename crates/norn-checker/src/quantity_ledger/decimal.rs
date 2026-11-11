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
    let mut remaining = exponent;
    while let Some(next_remaining) = remaining.checked_sub(1) {
        value = value.checked_mul(10)?;
        remaining = next_remaining;
    }
    Some(value)
}

/// An exact decimal `unscaled × 10^(-scale)`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct ScaledInteger {
    pub(super) unscaled: i128,
    pub(super) scale: u8,
}

/// Why exact scaled-integer arithmetic refused. Ledger lowering attaches the
/// error code, the authoring path and the message.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ScaledIntegerRefusal {
    /// Aligning to the target scale would discard digits.
    ScaleLoss,
    /// The alignment multiplier `10^gap` leaves `i128`.
    AlignmentMultiplierOverflow,
    /// The aligned coefficient leaves `i128`.
    AlignmentOverflow,
    AdditionOverflow,
    SubtractionOverflow,
    /// The product scale leaves `u8`.
    MultiplicationScaleOverflow,
    /// The product scale exceeds the caller's maximum.
    MultiplicationScaleAboveMaximum(u8),
    MultiplicationOverflow,
}

/// VAL-EXACT-1 canonical form of a scaled integer.
pub(super) fn canonical(value: ScaledInteger) -> ScaledInteger {
    let (unscaled, scale) = canonical_parts(value.unscaled, value.scale);
    ScaledInteger { unscaled, scale }
}

/// The coefficient of `value` at `target_scale`, refusing instead of rounding
/// or overflowing.
pub(super) fn rescale(
    value: ScaledInteger,
    target_scale: u8,
) -> Result<i128, ScaledIntegerRefusal> {
    let gap = match target_scale.checked_sub(value.scale) {
        Some(gap) => gap,
        None => return Err(ScaledIntegerRefusal::ScaleLoss),
    };
    let multiplier = match checked_pow10(gap) {
        Some(multiplier) => multiplier,
        None => return Err(ScaledIntegerRefusal::AlignmentMultiplierOverflow),
    };
    match value.unscaled.checked_mul(multiplier) {
        Some(aligned) => Ok(aligned),
        None => Err(ScaledIntegerRefusal::AlignmentOverflow),
    }
}

/// The two coefficients aligned to the larger scale, and that scale.
fn align(lhs: ScaledInteger, rhs: ScaledInteger) -> Result<(i128, i128, u8), ScaledIntegerRefusal> {
    let target_scale = if lhs.scale >= rhs.scale {
        lhs.scale
    } else {
        rhs.scale
    };
    let lhs = rescale(lhs, target_scale)?;
    let rhs = rescale(rhs, target_scale)?;
    Ok((lhs, rhs, target_scale))
}

/// Exact canonical sum, or a refusal.
pub(super) fn checked_add(
    lhs: ScaledInteger,
    rhs: ScaledInteger,
) -> Result<ScaledInteger, ScaledIntegerRefusal> {
    let (lhs, rhs, scale) = align(lhs, rhs)?;
    match lhs.checked_add(rhs) {
        Some(unscaled) => Ok(canonical(ScaledInteger { unscaled, scale })),
        None => Err(ScaledIntegerRefusal::AdditionOverflow),
    }
}

/// Exact canonical difference, or a refusal.
pub(super) fn checked_sub(
    lhs: ScaledInteger,
    rhs: ScaledInteger,
) -> Result<ScaledInteger, ScaledIntegerRefusal> {
    let (lhs, rhs, scale) = align(lhs, rhs)?;
    match lhs.checked_sub(rhs) {
        Some(unscaled) => Ok(canonical(ScaledInteger { unscaled, scale })),
        None => Err(ScaledIntegerRefusal::SubtractionOverflow),
    }
}

/// Exact canonical product with a scale of at most `max_scale`, or a refusal.
pub(super) fn checked_mul(
    lhs: ScaledInteger,
    rhs: ScaledInteger,
    max_scale: u8,
) -> Result<ScaledInteger, ScaledIntegerRefusal> {
    let scale = match lhs.scale.checked_add(rhs.scale) {
        Some(scale) => scale,
        None => return Err(ScaledIntegerRefusal::MultiplicationScaleOverflow),
    };
    if scale > max_scale {
        return Err(ScaledIntegerRefusal::MultiplicationScaleAboveMaximum(scale));
    }
    match lhs.unscaled.checked_mul(rhs.unscaled) {
        Some(unscaled) => Ok(canonical(ScaledInteger { unscaled, scale })),
        None => Err(ScaledIntegerRefusal::MultiplicationOverflow),
    }
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

        #[test]
        fn decimal_multiplier_refuses_exactly_on_overflow(exponent in any::<u8>()) {
            prop_assert_eq!(checked_pow10(exponent), 10_i128.checked_pow(u32::from(exponent)));
            prop_assert_eq!(checked_pow10(exponent).is_none(), exponent > 38);
        }
    }
}
