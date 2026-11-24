//! Exact aggregation of decimal coefficients, preserving all 28 fractional places.

use super::exact_sum::ExactSum;
use rust_decimal::Decimal;

/// Splitting at the decimal point avoids ever scaling a large integer by 10^28.
#[derive(Default)]
pub(crate) struct DecimalSum {
    whole: ExactSum,
    fractional: ExactSum,
}

impl DecimalSum {
    /// Include a decimal without rounding its coefficient.
    pub(crate) fn push(&mut self, value: Decimal) -> Option<()> {
        let divisor = 10_i128.checked_pow(value.scale())?;
        let whole = self.whole.add(value.mantissa().checked_div(divisor)?)?;
        let residual = value.mantissa().checked_rem(divisor)?;
        let factor = 10_i128.checked_pow(28_u32.checked_sub(value.scale())?)?;
        self.fractional = self.fractional.add(residual.checked_mul(factor)?)?;
        self.whole = whole;
        Some(())
    }

    /// Normalize after cancellation, then check the final 96-bit coefficient.
    #[must_use]
    pub(crate) fn total(&self) -> Option<Decimal> {
        let fraction = self.fractional.total()?;
        let unit = 10_i128.checked_pow(28)?;
        let whole = self
            .whole
            .total()?
            .checked_add(fraction.checked_div(unit)?)?;
        let mut residual = fraction.checked_rem(unit)?;
        let mut scale = 28_u32;
        while scale > 0 && residual.checked_rem(10)? == 0 {
            residual = residual.checked_div(10)?;
            scale = scale.checked_sub(1)?;
        }
        let coefficient = whole
            .checked_mul(10_i128.checked_pow(scale)?)?
            .checked_add(residual)?;
        Decimal::try_from_i128_with_scale(coefficient, scale).ok()
    }
}
