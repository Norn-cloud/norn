//! Production-kernel checks: full-width sum safety and bounded ratio correspondence.
extern crate self as norn_ir;
#[path = "../../crates/norn-ir/src/exact_sum.rs"]
pub mod exact_sum;
pub use exact_sum::ExactSum;
#[path = "../../crates/norn-checker/src/metric/compute/arithmetic.rs"]
mod arithmetic;

#[kani::proof]
fn sum_never_wraps() {
    // Symbolic positive and negative subtotal, including cancellation and
    // both sides of the magnitude boundary. Each fits the signed input type.
    let pos: i128 = kani::any();
    let neg: i128 = kani::any();
    kani::assume(pos >= 0 && neg <= 0);
    let sum = ExactSum::ZERO.add(pos).unwrap().add(neg).unwrap();
    let magnitude = pos.unsigned_abs() + neg.unsigned_abs();
    let admitted = magnitude <= arithmetic::MAX_SUM.unsigned_abs();
    kani::cover!(admitted, "inside publication bound");
    kani::cover!(!admitted, "outside publication bound");
    assert_eq!(arithmetic::decide(sum).ok(), admitted.then_some(pos + neg));
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
    let next = arithmetic::add(before, value);
    assert_eq!(
        next.is_ok(),
        value.unsigned_abs() <= arithmetic::MAX_VALUE.unsigned_abs()
    );
    if let Ok(after) = next {
        assert_eq!(after.retract(i128::from(value)), Some(before));
    }
}
