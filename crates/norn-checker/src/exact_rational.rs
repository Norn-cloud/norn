//! EXACT-RATIONAL-1: normalized, overflow-checked rational arithmetic.
/// A normalized exact rational with bounded representation; never rounded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rational {
    pub(crate) numerator: i64,
    pub(crate) denominator: i64,
}

impl Rational {
    /// Construct an exact rational. Zero denominators and representation overflow
    /// return None, including results too large after cancellation.
    #[must_use]
    pub fn new(numerator: i64, denominator: i64) -> Option<Self> {
        Self::normalize(i128::from(numerator), i128::from(denominator))
    }

    /// Normalize an i128 fraction into the bounded representation.
    /// Returns None for zero denominators or intermediate/representation overflow.
    #[must_use]
    pub fn normalize(mut numerator: i128, mut denominator: i128) -> Option<Self> {
        if denominator == 0 {
            return None;
        }
        if denominator < 0 {
            numerator = numerator.checked_neg()?;
            denominator = denominator.checked_neg()?;
        }
        let mut a = numerator.checked_abs()?;
        let mut b = denominator;
        while b != 0 {
            let remainder = a.checked_rem(b)?;
            a = b;
            b = remainder;
        }
        Some(Self {
            numerator: i64::try_from(numerator.checked_div(a)?).ok()?,
            denominator: i64::try_from(denominator.checked_div(a)?).ok()?,
        })
    }

    pub(crate) fn add(self, rhs: Self) -> Option<Self> {
        let n = i128::from(self.numerator)
            .checked_mul(i128::from(rhs.denominator))?
            .checked_add(i128::from(rhs.numerator).checked_mul(i128::from(self.denominator))?)?;
        Self::normalize(
            n,
            i128::from(self.denominator).checked_mul(i128::from(rhs.denominator))?,
        )
    }

    pub(crate) fn mul(self, rhs: Self) -> Option<Self> {
        Self::normalize(
            i128::from(self.numerator).checked_mul(i128::from(rhs.numerator))?,
            i128::from(self.denominator).checked_mul(i128::from(rhs.denominator))?,
        )
    }
    /// Exact addition, or None on representation overflow.
    #[must_use]
    pub fn checked_add(self, rhs: Self) -> Option<Self> {
        self.add(rhs)
    }

    /// Exact subtraction, or None on representation overflow.
    #[must_use]
    pub fn checked_sub(self, rhs: Self) -> Option<Self> {
        Self::normalize(
            i128::from(self.numerator)
                .checked_mul(i128::from(rhs.denominator))?
                .checked_sub(
                    i128::from(rhs.numerator).checked_mul(i128::from(self.denominator))?,
                )?,
            i128::from(self.denominator).checked_mul(i128::from(rhs.denominator))?,
        )
    }

    /// Exact multiplication, or None on representation overflow.
    #[must_use]
    pub fn checked_mul(self, rhs: Self) -> Option<Self> {
        self.mul(rhs)
    }

    /// Exact division; zero divisors and representation overflow return None.
    #[must_use]
    pub fn checked_div(self, rhs: Self) -> Option<Self> {
        Self::normalize(
            i128::from(self.numerator).checked_mul(i128::from(rhs.denominator))?,
            i128::from(self.denominator).checked_mul(i128::from(rhs.numerator))?,
        )
    }

    /// Exact negation, or None when the positive numerator cannot be represented.
    #[must_use]
    pub fn checked_neg(self) -> Option<Self> {
        Self::normalize(
            i128::from(self.numerator).checked_neg()?,
            i128::from(self.denominator),
        )
    }

    /// Exact ordering by checked cross-products; never compares rounded values.
    #[must_use]
    #[expect(
        clippy::should_implement_trait,
        reason = "the checked comparison API returns Option, unlike Ord::cmp"
    )]
    pub fn cmp(self, rhs: Self) -> Option<std::cmp::Ordering> {
        Some(
            i128::from(self.numerator)
                .checked_mul(i128::from(rhs.denominator))?
                .cmp(&i128::from(rhs.numerator).checked_mul(i128::from(self.denominator))?),
        )
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "exact rational test fixtures")]
mod tests {
    use super::Rational;
    use proptest::prelude::*;
    use std::cmp::Ordering;

    fn q(n: i64, d: i64) -> Rational {
        Rational::new(n, d).unwrap()
    }

    #[test]
    fn exact_rational_fraction_operations() {
        let (a, b) = (q(1, 2), q(1, 3));
        assert_eq!(a.checked_add(b), Some(q(5, 6)));
        assert_eq!(a.checked_sub(b), Some(q(1, 6)));
        assert_eq!(a.checked_mul(b), Some(q(1, 6)));
        assert_eq!(a.checked_div(b), Some(q(3, 2)));
        assert_eq!(a.checked_neg(), Some(q(-1, 2)));
        assert_eq!(a.cmp(b), Some(Ordering::Greater));
        assert_eq!(q(-1, 2).cmp(q(-2, 3)), Some(Ordering::Greater));
        assert_eq!(Rational::normalize(6, -12), Some(q(-1, 2)));
        assert_eq!(q(0, -17), q(0, 1));
    }

    #[test]
    fn exact_rational_limits_and_cancellation() {
        let (min, max, zero, one) = (q(i64::MIN, 1), q(i64::MAX, 1), q(0, 1), q(1, 1));
        assert_eq!(min.checked_neg(), None);
        assert_eq!(min.checked_sub(one), None);
        assert_eq!(max.checked_add(one), None);
        assert_eq!(max.checked_mul(max), None);
        assert_eq!(one.checked_div(zero), None);
        assert_eq!(min.checked_div(q(-1, 1)), None);
        assert_eq!(min.checked_sub(min), Some(zero));
        assert_eq!(min.checked_div(min), Some(one));
        assert_eq!(q(1, i64::MAX).checked_div(q(1, i64::MAX)), Some(one));
        assert_eq!(max.cmp(min), Some(Ordering::Greater));
        assert_eq!(Rational::normalize(1, 0), None);
        assert_eq!(Rational::normalize(i128::MIN, 1), None);
        assert_eq!(Rational::normalize(1, i128::MIN), None);
    }

    proptest! {
        #[test]
        fn exact_rational_inverse_and_order(n in -10000_i64..10000, d in 1_i64..10000,
                                           m in -10000_i64..10000, e in 1_i64..10000) {
            let (a, b) = (q(n, d), q(m, e));
            prop_assert_eq!(a.checked_add(b).unwrap().checked_sub(b), Some(a));
            prop_assert_eq!(a.checked_neg().unwrap().checked_neg(), Some(a));
            prop_assert_eq!(a.cmp(b), Some(n.checked_mul(e).unwrap().cmp(&m.checked_mul(d).unwrap())));
            if m != 0 { prop_assert_eq!(a.checked_div(b).unwrap().checked_mul(b), Some(a)); }
        }
    }
}
