//! Production-kernel checks: full-width sum safety and bounded ratio correspondence.
extern crate self as norn_ir;
#[path = "../../crates/norn-ir/src/exact_sum.rs"]
pub mod exact_sum;
pub use exact_sum::ExactSum;
#[path = "../../crates/norn-checker/src/metric/compute/arithmetic.rs"]
mod arithmetic;

#[kani::proof]
fn sum_never_wraps() {
    let values: [i64; 4] = kani::any();
    let mut sum = ExactSum::ZERO;
    let mut reference = 0i128;
    let mut magnitude = 0u128;
    for value in values {
        if let Ok(next) = arithmetic::add(sum, value) {
            sum = next;
            reference += i128::from(value);
            magnitude += u128::from(value.unsigned_abs());
        } else {
            assert!(value.unsigned_abs() > arithmetic::MAX_VALUE.unsigned_abs());
            return;
        }
    }
    assert_eq!(
        arithmetic::decide(sum).ok(),
        (magnitude <= arithmetic::MAX_SUM.unsigned_abs()).then_some(reference)
    );
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
    let before = ExactSum::ZERO.add(i128::from(kani::any::<i64>())).unwrap();
    let value: i64 = kani::any();
    if let Ok(after) = arithmetic::add(before, value) {
        assert_eq!(after.retract(i128::from(value)), Some(before));
    }
}
