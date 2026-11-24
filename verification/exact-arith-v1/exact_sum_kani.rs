//! A symbolic one-step check over both limbs; no loop unwinding bound.
use super::{ExactSum, U160};

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
    // On all reachable states each individual subtotal fits under this bound.
    kani::assume(s.pos.hi <= s.terms >> 1 && s.neg.hi <= s.terms >> 1);
    kani::assume(s.magnitude().hi <= s.terms >> 1);
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
