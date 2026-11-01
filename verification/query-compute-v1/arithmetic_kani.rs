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
    let d = if kani::any::<bool>() { 128 } else { -128 };
    let result = arithmetic::ratio(n, d);
    if d == 0 {
        assert!(result.is_none());
    } else if let Some(q) = result {
        let error = q * d - n * arithmetic::FACTOR;
        assert!(error.unsigned_abs() * 2 <= d.unsigned_abs());
        assert_eq!(arithmetic::ratio(-n, d), Some(-q));
        if error.unsigned_abs() * 2 == d.unsigned_abs() {
            assert!(q.unsigned_abs() * d.unsigned_abs() >= (n * arithmetic::FACTOR).unsigned_abs());
        }
    } else {
        assert!(false, "bounded operands must produce a ratio");
    }
}

#[kani::proof]
fn zero_divisor_always_refuses() {
    assert!(arithmetic::ratio(kani::any(), 0).is_none());
}

#[kani::proof]
fn ratio_never_panics_for_full_width_inputs() {
    let _result = arithmetic::ratio(kani::any(), kani::any());
}

#[kani::proof]
fn retracting_an_admitted_add_restores_the_original_total() {
    let before: i128 = kani::any();
    let value: i64 = kani::any();
    kani::assume(before >= -arithmetic::MAX_SUM && before <= arithmetic::MAX_SUM);
    if let Some(after) = arithmetic::add(before, value) {
        let inverse = value.checked_neg().expect("admitted coefficients exclude MIN");
        assert_eq!(arithmetic::add(after, inverse), Some(before));
    }
}
