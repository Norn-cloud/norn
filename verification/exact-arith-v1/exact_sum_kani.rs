//! A symbolic one-step check over both limbs; no loop unwinding bound.
use super::{ExactSum, U160};
use std::convert::TryFrom;

fn wide() -> U160 {
    U160 {
        hi: kani::any(),
        lo: kani::any(),
    }
}

#[kani::proof]
fn full_width_operations() {
    let s = ExactSum {
        pos: wide(),
        neg: wide(),
        terms: kani::any(),
    };
    let term: i128 = kani::any();
    // Arbitrary bit patterns must be panic-free, even outside reachable states.
    let next = s.add(term);
    let _ = (s.retract(term), s.total(), s.magnitude());
    assert_eq!(s.is_zero(), s.pos == s.neg);
    let (larger, smaller) = if s.pos >= s.neg {
        (s.pos, s.neg)
    } else {
        (s.neg, s.pos)
    };
    let (diff, borrow) = larger.lo.overflowing_sub(smaller.lo);
    let signed = (larger.hi - smaller.hi == u32::from(borrow))
        .then(|| i128::try_from(diff).ok())
        .flatten();
    assert_eq!(
        s.total(),
        signed.map(|n| if s.pos < s.neg { -n } else { n })
    );
    // On all reachable states each individual subtotal fits under this bound.
    kani::assume(s.pos.hi <= s.terms >> 1 && s.neg.hi <= s.terms >> 1);
    let (lo, carry) = s.pos.lo.overflowing_add(s.neg.lo);
    let magnitude = U160 {
        hi: s.pos.hi + s.neg.hi + u32::from(carry),
        lo,
    };
    assert_eq!(s.magnitude(), magnitude);
    kani::assume(
        magnitude
            <= U160 {
                hi: s.terms >> 1,
                lo: if s.terms & 1 == 0 { 0 } else { 1u128 << 127 },
            },
    );
    assert_eq!(next.is_none(), term == i128::MIN || s.terms == u32::MAX);
    if let Some(next) = next {
        let (old, changed) = if term < 0 {
            assert_eq!(s.pos, next.pos);
            (s.neg, next.neg)
        } else {
            assert_eq!(s.neg, next.neg);
            (s.pos, next.pos)
        };
        let (lo, carry) = old.lo.overflowing_add(term.unsigned_abs());
        assert_eq!(changed.lo, lo);
        assert_eq!(changed.hi, old.hi + u32::from(carry));
        assert_eq!(next.retract(term), Some(s));
    }
}
