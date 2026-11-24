//! Exact sums of at most `u32::MAX` full-width signed coefficients. Only the final
//! difference needs to fit in an i128. Neither subtotal depends on input order.

#![deny(clippy::default_numeric_fallback)]

#[cfg(verus_keep_ghost)]
use vstd::prelude::*;

/// An unsigned 160-bit integer, ordered most significant limb first.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
#[cfg_attr(verus_keep_ghost, verus_verify)]
pub struct U160 {
    hi: u32,
    lo: u128,
}

impl U160 {
    /// Embed a single unsigned limb.
    #[must_use]
    pub const fn from_u128(lo: u128) -> Self {
        Self { hi: 0, lo }
    }

    #[allow(
        clippy::as_conversions,
        clippy::cast_sign_loss,
        clippy::arithmetic_side_effects,
        reason = "MIN has a dedicated unsigned branch; all other absolute values fit in i128"
    )]
    #[cfg_attr(verus_keep_ghost, verus_spec(r => ensures
        r.value() == positive(term as int) + positive(-(term as int))
    ))]
    const fn absolute(term: i128) -> Self {
        let lo: u128 = if term == i128::MIN {
            1_u128 << 127_u32
        } else if term < 0 {
            (-term) as u128
        } else {
            term as u128
        };
        Self { hi: 0, lo }
    }

    #[cfg_attr(verus_keep_ghost, verus_spec(r =>
        requires self.value() + rhs.value() < radix() * 0x1_0000_0000,
        ensures r.value() == self.value() + rhs.value()
    ))]
    const fn plus(self, rhs: Self) -> Self {
        let lo: u128 = self.lo.wrapping_add(rhs.lo);
        let carry: bool = lo < self.lo;
        let hi: u32 = self
            .hi
            .wrapping_add(rhs.hi)
            .wrapping_add(if carry { 1 } else { 0 });
        Self { hi, lo }
    }

    #[cfg_attr(verus_keep_ghost, verus_spec(r =>
        ensures r.is_some() == (self.value() >= rhs.value()),
            r.is_some() ==> r.unwrap().value() == self.value() - rhs.value()
    ))]
    const fn minus(self, rhs: Self) -> Option<Self> {
        let lo: u128 = self.lo.wrapping_sub(rhs.lo);
        let borrow: bool = self.lo < rhs.lo;
        if self.hi < rhs.hi || (self.hi == rhs.hi && borrow) {
            return None;
        }
        let hi: u32 = self
            .hi
            .wrapping_sub(rhs.hi)
            .wrapping_sub(if borrow { 1 } else { 0 });
        Some(Self { hi, lo })
    }
}

/// An amount whose additive inverse fits in the same signed representation.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Amount(i128);

impl Amount {
    /// Check reversibility.
    #[must_use]
    pub const fn new(term: i128) -> Option<Self> {
        if term == i128::MIN {
            None
        } else {
            Some(Self(term))
        }
    }

    /// The signed coefficient.
    #[must_use]
    pub const fn get(self) -> i128 {
        self.0
    }
}

/// The positive and negative subtotals of at most `u32::MAX` terms.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[cfg_attr(verus_keep_ghost, verus_verify)]
pub struct ExactSum {
    pos: U160,
    neg: U160,
    terms: u32,
}

impl ExactSum {
    /// The empty sum.
    pub const ZERO: Self = Self::EMPTY;

    #[cfg_attr(verus_keep_ghost, verus_verify)]
    const EMPTY: Self = Self {
        pos: U160 { hi: 0, lo: 0 },
        neg: U160 { hi: 0, lo: 0 },
        terms: 0,
    };

    /// Append any signed coefficient. Refuses only when the sum already holds `u32::MAX` terms.
    #[must_use]
    #[allow(
        clippy::should_implement_trait,
        reason = "addition explicitly reports capacity"
    )]
    #[cfg_attr(verus_keep_ghost, verus_spec(r =>
        requires self.bounded(),
        ensures r.is_none() == (self.count() == u32::MAX),
            r.is_some() ==> r.unwrap().count() == self.count() + 1,
            r.is_some() ==> r.unwrap().positives() == self.positives() + positive(term as int),
            r.is_some() ==> r.unwrap().negatives() == self.negatives() + positive(-(term as int)),
            r.is_some() ==> r.unwrap().bounded(),
            forall|xs: Seq<int>| r.is_some() && #[trigger] self.represents(xs)
                ==> r.unwrap().represents(xs.push(term as int))
    ))]
    pub const fn add(mut self, term: i128) -> Option<Self> {
        if self.terms == u32::MAX {
            return None;
        }
        let limb = U160::absolute(term);
        #[cfg(verus_keep_ghost)]
        proof! {
            assert(self.bounded());
            assert(self.pos.value() + self.neg.value() <= self.terms as int * (radix()/2));
            assert(radix()/2 == 170141183460469231731687303715884105728int) by (compute);
            assert(self.pos.value() >= 0 && self.neg.value() >= 0);
            assert(self.pos.value() + self.neg.value() <= (u32::MAX as int) * (radix() / 2));
        }
        #[cfg(verus_keep_ghost)]
        proof! {
            assert forall|xs: Seq<int>| #[trigger] self.represents(xs) implies
                subtotal(xs.push(term as int), true) == self.positives() + positive(term as int) &&
                subtotal(xs.push(term as int), false) == self.negatives() + positive(-(term as int)) by {
                append(xs, term as int, true);
                append(xs, term as int, false);
            }
        }
        if term < 0 {
            self.neg = self.neg.plus(limb);
        } else {
            self.pos = self.pos.plus(limb);
        }
        self.terms = self.terms.wrapping_add(1);
        Some(self)
    }

    /// Remove a previously added coefficient. The caller owns membership.
    /// Reject underflow and retractions that exceed the remaining capacity.
    #[must_use]
    #[cfg_attr(verus_keep_ghost, verus_spec(r =>
        requires self.bounded(),
        ensures r.is_some() ==> r.unwrap().count() == self.count() - 1,
            r.is_some() ==> r.unwrap().positives() == self.positives() - positive(term as int),
            r.is_some() ==> r.unwrap().negatives() == self.negatives() - positive(-(term as int)),
            r.is_some() ==> r.unwrap().bounded()
    ))]
    pub const fn retract(mut self, term: i128) -> Option<Self> {
        if self.terms == 0 {
            return None;
        }
        let limb = U160::absolute(term);
        if term < 0 {
            match self.neg.minus(limb) {
                Some(rest) => self.neg = rest,
                None => return None,
            }
        } else {
            match self.pos.minus(limb) {
                Some(rest) => self.pos = rest,
                None => return None,
            }
        }
        self.terms = self.terms.wrapping_sub(1);
        let magnitude = self.magnitude();
        let hi: u32 = self.terms >> 1_u32;
        let lo: u128 = if self.terms & 1 == 0 {
            0
        } else {
            1_u128 << 127_u32
        };
        if magnitude.hi > hi || (magnitude.hi == hi && magnitude.lo > lo) {
            return None;
        }
        Some(self)
    }

    /// The exact signed result, when representable.
    #[must_use]
    #[allow(
        clippy::as_conversions,
        clippy::cast_possible_wrap,
        clippy::arithmetic_side_effects,
        clippy::cast_sign_loss,
        reason = "range checked below"
    )]
    #[cfg_attr(verus_keep_ghost, verus_spec(r =>
        ensures r.is_some() == ((i128::MIN as int) <= self.net() <= i128::MAX),
            r.is_some() ==> r.unwrap() as int == self.net(),
            forall|xs: Seq<int>| #[trigger] self.represents(xs) ==> self.net() == sum(xs)
    ))]
    pub const fn total(self) -> Option<i128> {
        let (difference, negative) = match self.pos.minus(self.neg) {
            Some(d) => (d, false),
            None => match self.neg.minus(self.pos) {
                Some(d) => (d, true),
                None => return None,
            },
        };
        if negative && difference.hi == 0 && difference.lo == 1_u128 << 127_u32 {
            return Some(i128::MIN);
        }
        if difference.hi != 0 || difference.lo > i128::MAX as u128 {
            return None;
        }
        let value: i128 = difference.lo as i128;
        Some(if negative { -value } else { value })
    }

    /// Sum of absolute values, including contributions that cancel.
    #[must_use]
    #[cfg_attr(verus_keep_ghost, verus_spec(r =>
        requires self.positives() + self.negatives() < radix() * 0x1_0000_0000,
        ensures r.value() == self.positives() + self.negatives(),
            forall|xs: Seq<int>| #[trigger] self.represents(xs) ==> r.value() == mag(xs)
    ))]
    pub const fn magnitude(self) -> U160 {
        self.pos.plus(self.neg)
    }

    /// Whether both subtotals are equal, regardless of their width.
    #[must_use]
    #[cfg_attr(verus_keep_ghost, verus_spec(r => ensures r == (self.net() == 0),
        forall|xs: Seq<int>| #[trigger] self.represents(xs) ==> r == (sum(xs) == 0)))]
    pub const fn is_zero(self) -> bool {
        #[cfg(verus_keep_ghost)]
        proof! {
            if self.pos.hi < self.neg.hi { assert(self.pos.value() < self.neg.value()); }
            if self.pos.hi > self.neg.hi { assert(self.pos.value() > self.neg.value()); }
        }
        self.pos.hi == self.neg.hi && self.pos.lo == self.neg.lo
    }
}

#[cfg(verus_keep_ghost)]
verus! {
    pub open spec fn radix() -> int { 340282366920938463463374607431768211456int }
    pub open spec fn positive(x: int) -> int { if x > 0 { x } else { 0 } }
    impl U160 {
        pub closed spec fn value(self) -> int { self.hi as int * radix() + self.lo as int }
    }
    impl ExactSum {
        pub closed spec fn bounded(self) -> bool {
            self.pos.value() + self.neg.value() <= self.terms as int * (radix() / 2)
        }
        pub closed spec fn net(self) -> int { self.pos.value() - self.neg.value() }
        pub closed spec fn count(self) -> int { self.terms as int }
        pub closed spec fn positives(self) -> int { self.pos.value() }
        pub closed spec fn negatives(self) -> int { self.neg.value() }
        pub closed spec fn represents(self, xs: Seq<int>) -> bool {
            self.terms == xs.len() && self.pos.value() == subtotal(xs, true)
                && self.neg.value() == subtotal(xs, false)
        }
    }
    spec fn subtotal(xs: Seq<int>, pos: bool) -> int {
        xs.fold_left(0int, |s: int, x: int| s + positive(if pos { x } else { -x }))
    }
    pub closed spec fn sum(xs: Seq<int>) -> int { subtotal(xs, true) - subtotal(xs, false) }
    pub closed spec fn mag(xs: Seq<int>) -> int { subtotal(xs, true) + subtotal(xs, false) }

    proof fn empty_represents_empty() ensures ExactSum::EMPTY.represents(Seq::empty()), ExactSum::EMPTY.bounded() {}

    proof fn append(xs: Seq<int>, t: int, pos: bool)
        ensures subtotal(xs.push(t), pos) == subtotal(xs, pos) + positive(if pos { t } else { -t }),
    {
        assert(xs.push(t).drop_last() =~= xs);
    }

    proof fn perm_subtotal(a: Seq<int>, b: Seq<int>, pos: bool)
        requires a.to_multiset() == b.to_multiset(),
        ensures subtotal(a, pos) == subtotal(b, pos),
    {
        vstd::seq_lib::lemma_fold_left_permutation(a, b, |s: int, x: int| s + positive(if pos { x } else { -x }), 0int);
    }

    proof fn permutation(a: Seq<int>, b: Seq<int>)
        requires a.to_multiset() == b.to_multiset(),
        ensures sum(a) == sum(b), mag(a) == mag(b),
    {
        perm_subtotal(a, b, true);
        perm_subtotal(a, b, false);
    }

    proof fn no_internal_overflow(xs: Seq<int>)
        requires xs.len() <= u32::MAX,
            forall|i: int| 0 <= i < xs.len() ==> -(radix()/2) <= #[trigger] xs[i] < radix()/2,
        ensures 0 <= subtotal(xs, true) < radix() * 0x1_0000_0000,
            0 <= subtotal(xs, false) < radix() * 0x1_0000_0000,
            mag(xs) <= xs.len() * (radix()/2),
        decreases xs.len(),
    {
        if xs.len() > 0 {
            let pre = xs.drop_last();
            assert forall|i: int| 0 <= i < pre.len() implies
                -(radix()/2) <= #[trigger] pre[i] < radix()/2 by { assert(pre[i] == xs[i]); }
            no_internal_overflow(pre);
            assert(xs.last() == xs[xs.len() - 1]);
            assert(pre.push(xs.last()) =~= xs);
            append(pre, xs.last(), true);
            append(pre, xs.last(), false);
        }
    }
}

#[cfg(kani)]
#[path = "../../../verification/exact-arith-v1/exact_sum_kani.rs"]
mod kani_proofs;

#[cfg(test)]
mod tests {
    use super::{Amount, ExactSum};

    #[test]
    fn full_width_cancellation_and_minimum_result() {
        for (a, b, expected) in [(i128::MIN, i128::MAX, -1), (-i128::MAX, -1, i128::MIN)] {
            for [first, second] in [[a, b], [b, a]] {
                let prefix = ExactSum::ZERO.add(first);
                let sum = prefix.and_then(|s| s.add(second));
                assert_eq!(sum.and_then(ExactSum::total), Some(expected));
                assert_eq!(sum.and_then(|s| s.retract(second)), prefix);
            }
        }
        assert!(Amount::new(i128::MIN).is_none());
    }
}
