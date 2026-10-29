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
    let value = value.normalize();
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

impl Quantity for Decimal {
    fn zero() -> Self {
        Self::ZERO
    }

    fn add(&self, rhs: &Self) -> Result<Self, ArithmeticError> {
        let scale = self.normalize().scale().max(rhs.normalize().scale());
        let mantissa = aligned(*self, scale)?
            .checked_add(aligned(*rhs, scale)?)
            .ok_or(ArithmeticError::Overflow)?;
        decimal(mantissa, scale)
    }

    fn sub(&self, rhs: &Self) -> Result<Self, ArithmeticError> {
        let scale = self.normalize().scale().max(rhs.normalize().scale());
        let mantissa = aligned(*self, scale)?
            .checked_sub(aligned(*rhs, scale)?)
            .ok_or(ArithmeticError::Overflow)?;
        decimal(mantissa, scale)
    }

    fn mul(&self, rhs: &Self) -> Result<Self, ArithmeticError> {
        let left = self.normalize();
        let right = rhs.normalize();
        let mantissa = left
            .mantissa()
            .checked_mul(right.mantissa())
            .ok_or(ArithmeticError::Overflow)?;
        let scale = left
            .scale()
            .checked_add(right.scale())
            .ok_or(ArithmeticError::Overflow)?;
        decimal(mantissa, scale)
    }
}

#[cfg(test)]
mod tests {
    use super::{ArithmeticError, Quantity};
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
        assert_eq!(
            Quantity::mul(&Decimal::new(5, 28), &Decimal::new(2, 1)),
            Ok(Decimal::new(1, 28))
        );
        assert_eq!(
            Quantity::mul(&Decimal::new(1, 28), &Decimal::new(1, 1)),
            Err(ArithmeticError::Precision)
        );
    }
}
