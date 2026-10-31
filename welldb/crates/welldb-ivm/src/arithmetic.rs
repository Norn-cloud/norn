//! Signed 256-bit exact arithmetic with checked, commutative accumulation.
//! Magnitudes use four little-endian limbs; zero has a unique positive sign.
#![allow(
    clippy::arithmetic_side_effects,
    clippy::indexing_slicing,
    reason = "fixed four-limb arithmetic uses bounded indices and u128 intermediates"
)]

use crate::{Decimal, MetricError};

/// Signed 256-bit integer (signed magnitude, restricted to |x| < 2^255).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(try_from = "SumWire", into = "SumWire"))]
pub struct ExactSum {
    negative: bool,
    limbs: [u64; 4],
}
/// A limb representation exceeds the `ExactSum` domain `|x| < 2^255`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct PartsError;
impl std::fmt::Display for PartsError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("coefficient magnitude must be below 2^255")
    }
}
impl std::error::Error for PartsError {}

impl ExactSum {
    /// Zero.
    pub const ZERO: Self = Self {
        negative: false,
        limbs: [0; 4],
    };
    /// Canonical sign and little-endian magnitude limbs; zero is never negative.
    #[must_use]
    pub const fn to_parts(&self) -> (bool, [u64; 4]) {
        (self.negative, self.limbs)
    }
    /// Build from a sign and little-endian magnitude. Normalize negative zero.
    /// The type domain is `|x| < 2^255`, wider than the admitted metric sums.
    /// # Errors
    /// `PartsError` if the top bit is set (magnitude at least `2^255`).
    pub fn from_parts(negative: bool, limbs: [u64; 4]) -> Result<Self, PartsError> {
        if limbs[3] >> 63 != 0 {
            return Err(PartsError);
        }
        Ok(Self {
            negative: negative && limbs != [0; 4],
            limbs,
        })
    }
    /// An exact signed coefficient.
    #[must_use]
    pub fn from_i128(value: i128) -> Self {
        let magnitude = value.unsigned_abs();
        let bytes = magnitude.to_le_bytes();
        let mut low = [0; 8];
        let mut high = [0; 8];
        low.copy_from_slice(&bytes[..8]);
        high.copy_from_slice(&bytes[8..]);
        Self {
            negative: value < 0,
            limbs: [u64::from_le_bytes(low), u64::from_le_bytes(high), 0, 0],
        }
    }
    /// Exact decimal rescaling. Refuses fractional loss rather than rounding.
    /// # Errors
    /// `InvalidField` for a value finer than the admitted fixed scale.
    pub fn scaled(value: Decimal, scale: u8) -> Result<Self, MetricError> {
        let places = scale
            .checked_sub(value.scale())
            .ok_or(MetricError::InvalidField)?;
        let mut sum = Self::from_i128(value.mantissa());
        for _ in 0..places {
            sum = sum.times_ten().ok_or(MetricError::AggregateOverflow)?;
        }
        Ok(sum)
    }
    fn times_ten(self) -> Option<Self> {
        let mut limbs = [0; 4];
        let mut carry = 0_u128;
        for (out, limb) in limbs.iter_mut().zip(self.limbs) {
            let wide = u128::from(limb) * 10 + carry;
            *out = u64::try_from(wide & u128::from(u64::MAX)).ok()?;
            carry = wide >> 64;
        }
        if carry != 0 || limbs[3] >> 63 != 0 {
            return None;
        }
        Some(Self {
            negative: self.negative,
            limbs,
        })
    }
    /// Exact checked addition. In the §24.3 admitted population domain this
    /// cannot overflow (|operand| < 2^190, population < 2^64).
    #[must_use]
    pub fn checked_add(self, other: Self) -> Option<Self> {
        if self.negative == other.negative {
            let mut limbs = [0; 4];
            let mut carry = 0_u128;
            for (i, out) in limbs.iter_mut().enumerate() {
                let wide = u128::from(self.limbs[i]) + u128::from(other.limbs[i]) + carry;
                *out = u64::try_from(wide & u128::from(u64::MAX)).ok()?;
                carry = wide >> 64;
            }
            if carry != 0 || limbs[3] >> 63 != 0 {
                return None;
            }
            Some(Self {
                negative: self.negative && limbs != [0; 4],
                limbs,
            })
        } else {
            let (large, small) = if self
                .limbs
                .iter()
                .rev()
                .cmp(other.limbs.iter().rev())
                .is_lt()
            {
                (other, self)
            } else {
                (self, other)
            };
            let mut limbs = [0; 4];
            let mut borrow = false;
            for (i, out) in limbs.iter_mut().enumerate() {
                let (a, b) = large.limbs[i].overflowing_sub(small.limbs[i]);
                let (a, c) = a.overflowing_sub(u64::from(borrow));
                *out = a;
                borrow = b || c;
            }
            Some(Self {
                negative: large.negative && limbs != [0; 4],
                limbs,
            })
        }
    }
    /// Additive inverse.
    #[must_use]
    pub fn negated(mut self) -> Self {
        if self != Self::ZERO {
            self.negative = !self.negative;
        }
        self
    }
    /// Whether the value is strictly negative.
    #[must_use]
    pub const fn is_negative(self) -> bool {
        self.negative
    }
    /// Whether the exact coefficient fits a native HUGEINT.
    #[must_use]
    pub fn fits_i128(self) -> bool {
        self.to_i128().is_some()
    }
    fn to_i128(self) -> Option<i128> {
        if self.limbs[2] != 0 || self.limbs[3] != 0 {
            return None;
        }
        let mag = u128::from(self.limbs[0]) | (u128::from(self.limbs[1]) << 64);
        if self.negative && mag == (1_u128 << 127) {
            return Some(i128::MIN);
        }
        let signed = i128::try_from(mag).ok()?;
        if self.negative {
            signed.checked_neg()
        } else {
            Some(signed)
        }
    }
    /// Fit the native DECIMAL(38,s) column's coefficient.
    #[must_use]
    pub fn fits_decimal38(self) -> bool {
        self.to_i128()
            .is_some_and(|v| v.unsigned_abs() < 10_u128.pow(38))
    }
    /// Normalize trailing fractional zeros before checking Norn representability.
    /// # Errors
    /// Returns `AggregateOverflow` if exact conversion is impossible.
    pub fn decimal(mut self, mut scale: u8) -> Result<Decimal, MetricError> {
        while scale > 0 {
            let (quotient, remainder) = self.div_ten();
            if remainder != 0 {
                break;
            }
            self = quotient;
            scale -= 1;
        }
        Decimal::new(self.to_i128().ok_or(MetricError::AggregateOverflow)?, scale)
    }
    fn div_ten(self) -> (Self, u8) {
        let mut limbs = [0; 4];
        let mut remainder = 0_u128;
        for i in (0..4).rev() {
            let wide = (remainder << 64) | u128::from(self.limbs[i]);
            // Quotient fits a limb because remainder < 10.
            limbs[i] = u64::try_from(wide / 10).unwrap_or_default();
            remainder = wide % 10;
        }
        (
            Self {
                negative: self.negative && limbs != [0; 4],
                limbs,
            },
            u8::try_from(remainder).unwrap_or_default(),
        )
    }
}

/// Commutative, associative bucket accumulator. No rounding or output policy.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct BucketAgg {
    /// Exact coefficient at the plan scale.
    pub sum: ExactSum,
    /// Contributing measure count. At `u64::MAX` finalization refuses.
    pub population: u64,
}
impl BucketAgg {
    /// Merge partial aggregates; populations at the limit remain representable
    /// internally so retractions can correct a refused result.
    /// # Errors
    /// Out-of-domain population or arithmetic overflow; no mutation on error.
    pub fn add(&mut self, other: &Self) -> Result<(), MetricError> {
        let population = self
            .population
            .checked_add(other.population)
            .ok_or(MetricError::PopulationOverflow)?;
        let sum = self
            .sum
            .checked_add(other.sum)
            .ok_or(MetricError::AggregateOverflow)?;
        *self = Self { sum, population };
        Ok(())
    }
    /// Retract a previously added aggregate.
    /// # Errors
    /// An unmatched retraction or out-of-domain arithmetic; no mutation on error.
    pub fn retract(&mut self, other: &Self) -> Result<(), MetricError> {
        let population = self
            .population
            .checked_sub(other.population)
            .ok_or(MetricError::PopulationOverflow)?;
        let sum = self
            .sum
            .checked_add(other.sum.negated())
            .ok_or(MetricError::AggregateOverflow)?;
        *self = Self { sum, population };
        Ok(())
    }
}

impl PartialOrd for ExactSum {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}
impl Ord for ExactSum {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        match (self.negative, other.negative) {
            (true, false) => std::cmp::Ordering::Less,
            (false, true) => std::cmp::Ordering::Greater,
            (false, false) => self.limbs.iter().rev().cmp(other.limbs.iter().rev()),
            (true, true) => other.limbs.iter().rev().cmp(self.limbs.iter().rev()),
        }
    }
}

#[cfg(feature = "serde")]
#[derive(serde::Serialize, serde::Deserialize)]
struct SumWire {
    negative: bool,
    limbs: [u64; 4],
}
#[cfg(feature = "serde")]
impl From<ExactSum> for SumWire {
    fn from(value: ExactSum) -> Self {
        Self {
            negative: value.negative,
            limbs: value.limbs,
        }
    }
}
#[cfg(feature = "serde")]
impl TryFrom<SumWire> for ExactSum {
    type Error = PartsError;
    fn try_from(value: SumWire) -> Result<Self, Self::Error> {
        Self::from_parts(value.negative, value.limbs)
    }
}
