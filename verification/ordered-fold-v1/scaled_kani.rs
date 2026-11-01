//! FOLD-SCALED / FOLD-ARITHMETIC: harnesses call production operations.
use super::{Rounding, ScaledArithmetic};
use rust_decimal::Decimal;

fn mode() -> Rounding {
    match kani::any::<u8>() % 4 {
        0 => Rounding::Refuse,
        1 => Rounding::TowardZero,
        2 => Rounding::HalfEven,
        _ => Rounding::HalfAwayFromZero,
    }
}

#[kani::proof]
fn rounding_obeys_nearest_and_tie_laws() {
    let n = i128::from(kani::any::<i8>());
    let d = i128::from(kani::any::<i8>());
    let rounding = mode();
    let result = ScaledArithmetic { scale: 0, rounding }.quotient(n, d);
    if d == 0 || (rounding == Rounding::Refuse && n % d != 0) {
        assert!(result.is_err());
        return;
    }
    let actual = result.unwrap();
    match rounding {
        Rounding::Refuse | Rounding::TowardZero => assert_eq!(actual, n / d),
        Rounding::HalfEven | Rounding::HalfAwayFromZero => {
            let error = (n - actual * d).abs();
            assert!(2 * error <= d.abs());
            if 2 * error == d.abs() {
                if rounding == Rounding::HalfEven {
                    assert_eq!(actual % 2, 0);
                } else {
                    assert_eq!(actual.abs(), (n / d).abs() + 1);
                }
            }
        }
    }
}

#[kani::proof]
fn full_width_quotient_boundaries_refuse_without_overflow() {
    let n: i128 = kani::any();
    let d = match kani::any::<u8>() % 3 {
        0 => 0,
        1 => 1,
        _ => -1,
    };
    let result = ScaledArithmetic {
        scale: 0,
        rounding: mode(),
    }
    .quotient(n, d);
    if d == 0 || (n == i128::MIN && d == -1) {
        assert!(result.is_err());
    } else {
        assert_eq!(result.unwrap(), if d == 1 { n } else { -n });
    }
}

#[kani::proof]
#[kani::unwind(40)]
fn scale_validation_and_coefficients_are_exact() {
    let n = i128::from(kani::any::<i8>());
    let scale: u32 = kani::any();
    let result = ScaledArithmetic {
        scale,
        rounding: mode(),
    }
    .coefficient(Decimal::from_i128_with_scale(n, 0));
    if scale > 18 {
        assert!(result.is_err());
    } else {
        assert_eq!(result.unwrap(), n * 10_i128.pow(scale));
    }
}

#[kani::proof]
#[kani::unwind(40)]
fn bounded_binary_operations_match_integer_reference() {
    let a = i128::from(kani::any::<i8>());
    let b = i128::from(kani::any::<i8>());
    let op = kani::any::<u8>() % 4;
    let name = match op {
        0 => "Add",
        1 => "Subtract",
        2 => "Multiply",
        _ => "Divide",
    };
    let profile = ScaledArithmetic {
        scale: 0,
        rounding: Rounding::Refuse,
    };
    let result = profile.binary(
        name,
        Decimal::from_i128_with_scale(a, 0),
        Decimal::from_i128_with_scale(b, 0),
    );
    if op == 3 && (b == 0 || a % b != 0) {
        assert!(result.is_err());
    } else {
        let expected = match op {
            0 => a + b,
            1 => a - b,
            2 => a * b,
            _ => a / b,
        };
        assert_eq!(profile.coefficient(result.unwrap()).unwrap(), expected);
    }
}

#[kani::proof]
#[kani::unwind(40)]
fn scaled_intermediate_and_decimal_range_overflow_refuse() {
    let huge = Decimal::from_i128_with_scale(100_000_000_000_000_000_000, 0);
    let (scale, op, a, b) = match kani::any::<u8>() % 4 {
        0 => (0, "Multiply", huge, huge),
        1 => (18, "Divide", huge, Decimal::ONE),
        2 => (0, "Add", Decimal::MAX, Decimal::ONE),
        _ => (0, "Subtract", Decimal::MIN, Decimal::ONE),
    };
    assert!(ScaledArithmetic {
        scale,
        rounding: mode()
    }
    .binary(op, a, b)
    .is_err());
}

/// FOLD-RESCALE: bounded coefficients/scales cover all four explicit modes.
#[kani::proof]
#[kani::unwind(40)]
fn explicit_rescale_obeys_exact_and_nearest_laws() {
    let n = i128::from(kani::any::<i8>());
    let source = u32::from(kani::any::<u8>() % 5);
    let target = u32::from(kani::any::<u8>() % 5);
    let rounding = mode();
    let profile = ScaledArithmetic { scale: target, rounding };
    let result = profile.rescale(Decimal::from_i128_with_scale(n, source));
    let numerator = n * 10_i128.pow(target);
    let denominator = 10_i128.pow(source);
    let q = numerator / denominator;
    let remainder = numerator % denominator;
    if rounding == Rounding::Refuse && remainder != 0 {
        assert!(result.is_err());
        return;
    }
    let actual = profile.coefficient(result.unwrap()).unwrap();
    match rounding {
        Rounding::Refuse | Rounding::TowardZero => assert_eq!(actual, q),
        Rounding::HalfEven | Rounding::HalfAwayFromZero => {
            let error = (numerator - actual * denominator).abs();
            assert!(2 * error <= denominator);
            if 2 * error == denominator {
                if rounding == Rounding::HalfEven { assert_eq!(actual % 2, 0); }
                else { assert_eq!(actual.abs(), q.abs() + 1); }
            }
        }
    }
}

#[kani::proof]
#[kani::unwind(40)]
fn rescale_invalid_scales_and_full_width_boundaries_refuse() {
    let target: u32 = kani::any();
    kani::assume(target > 18);
    assert!(ScaledArithmetic { scale: target, rounding: mode() }.rescale(Decimal::ZERO).is_err());
    let profile = ScaledArithmetic { scale: 18, rounding: mode() };
    assert!(profile.rescale(Decimal::MAX).is_err());
    assert!(profile.rescale(Decimal::MIN).is_err());
}
