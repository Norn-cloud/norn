//! Production-kernel checks: full-width sum safety and bounded ratio correspondence.
#[path = "../../crates/norn-checker/src/metric/compute/arithmetic.rs"]
mod arithmetic;

#[kani::proof]
fn sum_never_wraps() {
    let total: i128 = kani::any();
    let value: i64 = kani::any();
    if let Some(sum) = arithmetic::add(total, value) {
        assert_eq!(sum.checked_sub(i128::from(value)), Some(total));
        assert!(sum >= -arithmetic::MAX_SUM && sum <= arithmetic::MAX_SUM);
    }
}

#[kani::proof]
fn ratio_is_nearest_and_sign_symmetric() {
    let n = i128::from(kani::any::<i16>());
    let d = i128::from(kani::any::<i16>());
    let result = arithmetic::ratio(n, d);
    if d == 0 {
        assert!(result.is_none());
    } else if let Some(q) = result {
        let error = q * d - n * arithmetic::FACTOR;
        assert!(error.unsigned_abs() * 2 <= d.unsigned_abs());
        assert_eq!(arithmetic::ratio(-n, d), Some(-q));
    } else {
        assert!(false, "bounded operands must produce a ratio");
    }
}
