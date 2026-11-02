//! FOLD-METER-EXACT: two sequential charges through actual shared atomic storage.
//! Kani assumes sequential execution; this makes no concurrent-schedule claim.
use super::Meter;
use crate::scaled::{Rounding, ScaledArithmetic};

#[kani::proof]
fn shared_atomic_meter_matches_mathematical_budget_and_stays_refused() {
    let limit: u64 = kani::any();
    let first: u64 = kani::any();
    let second: u64 = kani::any();
    let meter = Meter::new(limit);
    let local = meter.with_arithmetic(ScaledArithmetic {
        scale: 18,
        rounding: Rounding::HalfEven,
    });
    let a = local.charge(first);
    assert_eq!(a.is_ok(), first <= limit);
    if let Err(error) = a {
        assert_eq!(meter.charge(second), Err(error));
        assert_eq!(local.charge(0), Err(error));
        assert_eq!(meter.used(), 0);
    } else {
        let total = u128::from(first) + u128::from(second);
        let b = meter.charge(second);
        assert_eq!(b.is_ok(), total <= u128::from(limit));
        if let Err(error) = b {
            assert_eq!(local.charge(0), Err(error));
            assert_eq!(meter.used(), first);
        } else {
            assert_eq!(u128::from(local.used()), total);
        }
    }
    assert!(meter.arithmetic().is_none());
    assert!(local.arithmetic().is_some());
}
