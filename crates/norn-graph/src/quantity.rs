use rust_decimal::Decimal;

/// Failure to represent an arithmetic result exactly.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ArithmeticError {
    /// The checked integer intermediate or decimal mantissa overflowed.
    Overflow,
    /// The exact result requires more than 28 fractional decimal digits.
    Precision,
}

/// Exact, ordered quantities used for durations, requirements and roll-ups.
///
/// Implementations must have a common additive zero and return errors rather
/// than round, saturate, wrap, panic or perform IO. Methods must take bounded
/// work independent of graph size. Units and currency are the caller's contract.
pub trait Quantity: Clone + Ord {
    /// Accumulator for unordered sibling sums.
    type Sum: Default;
    /// Include one term in an unordered sum.
    ///
    /// # Errors
    /// Returns an error when the accumulator capacity is exhausted.
    fn accumulate(sum: &mut Self::Sum, term: &Self) -> Result<(), ArithmeticError>;
    /// Convert a completed sum to its output representation.
    ///
    /// # Errors
    /// Returns an error when the result cannot be represented exactly.
    fn finish(sum: Self::Sum) -> Result<Self, ArithmeticError>;
    /// Additive identity.
    fn zero() -> Self;
    /// Exact checked addition.
    ///
    /// # Errors
    /// Returns an error when the exact result is not representable.
    fn add(&self, rhs: &Self) -> Result<Self, ArithmeticError>;
    /// Exact checked subtraction.
    ///
    /// # Errors
    /// Returns an error when the exact result is not representable.
    fn sub(&self, rhs: &Self) -> Result<Self, ArithmeticError>;
    /// Exact checked multiplication (used for BOM line quantities).
    ///
    /// # Errors
    /// Returns an error when the exact result is not representable.
    fn mul(&self, rhs: &Self) -> Result<Self, ArithmeticError>;
}

fn decimal(mut mantissa: i128, mut scale: u32) -> Result<Decimal, ArithmeticError> {
    while scale > 0 && mantissa.checked_rem(10) == Some(0) {
        mantissa = mantissa.checked_div(10).ok_or(ArithmeticError::Overflow)?;
        scale = scale.checked_sub(1).ok_or(ArithmeticError::Overflow)?;
    }
    if scale > 28 {
        return Err(ArithmeticError::Precision);
    }
    Decimal::try_from_i128_with_scale(mantissa, scale)
        .map_err(|_unrepresentable| ArithmeticError::Overflow)
}

fn aligned(value: Decimal, scale: u32) -> Result<i128, ArithmeticError> {
    let shift = scale
        .checked_sub(value.scale())
        .ok_or(ArithmeticError::Overflow)?;
    let factor = 10_i128
        .checked_pow(shift)
        .ok_or(ArithmeticError::Overflow)?;
    value
        .mantissa()
        .checked_mul(factor)
        .ok_or(ArithmeticError::Overflow)
}

fn combine(
    lhs: Decimal,
    rhs: Decimal,
    op: impl FnOnce(i128, i128) -> Option<i128>,
) -> Result<Decimal, ArithmeticError> {
    let left = lhs.normalize();
    let right = rhs.normalize();
    let scale = left.scale().max(right.scale());
    let mantissa =
        op(aligned(left, scale)?, aligned(right, scale)?).ok_or(ArithmeticError::Overflow)?;
    decimal(mantissa, scale)
}

fn cancel_ten_pairs(a: &mut i128, b: &mut i128, scale: &mut u32) -> Result<(), ArithmeticError> {
    while *scale > 0 && a.checked_rem(10) == Some(0) {
        *a = a.checked_div(10).ok_or(ArithmeticError::Overflow)?;
        *scale = scale.checked_sub(1).ok_or(ArithmeticError::Overflow)?;
    }
    while *scale > 0 && b.checked_rem(10) == Some(0) {
        *b = b.checked_div(10).ok_or(ArithmeticError::Overflow)?;
        *scale = scale.checked_sub(1).ok_or(ArithmeticError::Overflow)?;
    }
    while *scale > 0 && a.checked_rem(2) == Some(0) && b.checked_rem(5) == Some(0) {
        *a = a.checked_div(2).ok_or(ArithmeticError::Overflow)?;
        *b = b.checked_div(5).ok_or(ArithmeticError::Overflow)?;
        *scale = scale.checked_sub(1).ok_or(ArithmeticError::Overflow)?;
    }
    while *scale > 0 && a.checked_rem(5) == Some(0) && b.checked_rem(2) == Some(0) {
        *a = a.checked_div(5).ok_or(ArithmeticError::Overflow)?;
        *b = b.checked_div(2).ok_or(ArithmeticError::Overflow)?;
        *scale = scale.checked_sub(1).ok_or(ArithmeticError::Overflow)?;
    }
    Ok(())
}

/// Wide accumulator for decimal-valued subtree rollups.
#[derive(Default)]
pub struct DecimalAccumulator(crate::decimal_sum::DecimalSum);

impl Quantity for Decimal {
    type Sum = DecimalAccumulator;

    fn accumulate(sum: &mut Self::Sum, term: &Self) -> Result<(), ArithmeticError> {
        sum.0.push(*term).ok_or(ArithmeticError::Overflow)
    }

    fn finish(sum: Self::Sum) -> Result<Self, ArithmeticError> {
        sum.0.total().ok_or(ArithmeticError::Overflow)
    }

    fn zero() -> Self {
        Self::ZERO
    }

    fn add(&self, rhs: &Self) -> Result<Self, ArithmeticError> {
        combine(*self, *rhs, i128::checked_add)
    }

    fn sub(&self, rhs: &Self) -> Result<Self, ArithmeticError> {
        combine(*self, *rhs, i128::checked_sub)
    }

    fn mul(&self, rhs: &Self) -> Result<Self, ArithmeticError> {
        let left = self.normalize();
        let right = rhs.normalize();
        let mut lm = left.mantissa();
        let mut rm = right.mantissa();
        if lm == 0 || rm == 0 {
            return Ok(Self::ZERO);
        }
        let mut scale = left
            .scale()
            .checked_add(right.scale())
            .ok_or(ArithmeticError::Overflow)?;
        cancel_ten_pairs(&mut lm, &mut rm, &mut scale)?;
        let mantissa = lm.checked_mul(rm).ok_or(ArithmeticError::Overflow)?;
        decimal(mantissa, scale)
    }
}

#[cfg(test)]
mod tests {
    use super::Quantity;
    use rust_decimal::Decimal;

    #[test]
    fn exact_decimal_arithmetic_rejects_rounding_and_preserves_cancellation() {
        assert_eq!(
            Quantity::add(&Decimal::new(125, 2), &Decimal::new(-25, 2)),
            Ok(Decimal::ONE)
        );
        assert_eq!(
            Quantity::sub(&Decimal::ONE, &Decimal::new(125, 2)),
            Ok(Decimal::new(-25, 2))
        );
    }
}
