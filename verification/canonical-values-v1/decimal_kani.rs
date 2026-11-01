//! VAL-EXACT-1: direct production-core checks without checker/serde lowering.
#[path = "../../crates/norn-checker/src/quantity_ledger/decimal.rs"]
mod decimal;

#[kani::proof]
#[kani::unwind(4)]
fn canonical_decimal_preserves_bounded_values() {
    let coefficient: i8 = kani::any();
    let scale: u8 = kani::any();
    kani::assume(scale <= 2);
    let original = i128::from(coefficient);
    let (normalized, normalized_scale) = decimal::canonical_parts(original, scale);
    assert!(normalized_scale <= scale);
    assert_eq!(
        decimal::canonical_parts(normalized, normalized_scale),
        (normalized, normalized_scale)
    );
    let factor = match scale.checked_sub(normalized_scale) {
        Some(0) => 1,
        Some(1) => 10,
        Some(2) => 100,
        _ => unreachable!(),
    };
    assert_eq!(normalized.checked_mul(factor), Some(original));
}

#[kani::proof]
#[kani::unwind(8)]
fn decimal_scale_multiplier_is_exact_in_bounded_profile() {
    let exponent: u8 = kani::any();
    kani::assume(exponent <= 6);
    let expected = match exponent {
        0 => 1,
        1 => 10,
        2 => 100,
        3 => 1_000,
        4 => 10_000,
        5 => 100_000,
        _ => 1_000_000,
    };
    assert_eq!(decimal::checked_pow10(exponent), Some(expected));
}

#[kani::proof]
#[kani::unwind(41)]
fn decimal_scale_overflow_refuses() {
    assert_eq!(decimal::checked_pow10(39), None);
}
