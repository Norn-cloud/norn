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
    let profile = ScaledArithmetic {
        scale: target,
        rounding,
    };
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
                if rounding == Rounding::HalfEven {
                    assert_eq!(actual % 2, 0);
                } else {
                    assert_eq!(actual.abs(), q.abs() + 1);
                }
            }
        }
    }
}

#[kani::proof]
#[kani::unwind(40)]
fn rescale_invalid_scales_and_full_width_boundaries_refuse() {
    let target: u32 = kani::any();
    kani::assume(target > 18);
    assert!(ScaledArithmetic {
        scale: target,
        rounding: mode()
    }
    .rescale(Decimal::ZERO)
    .is_err());
    let profile = ScaledArithmetic {
        scale: 18,
        rounding: mode(),
    };
    assert!(profile.rescale(Decimal::MAX).is_err());
    assert!(profile.rescale(Decimal::MIN).is_err());
}

/// VAL-EXACT-1: all u32 scale derivations and fixed full-width overflow witnesses.
#[kani::proof]
#[kani::unwind(40)]
fn derived_exact_scales_and_overflow_refuse() {
    let (a, b): (u32, u32) = kani::any();
    let multiply: bool = kani::any();
    let op = if multiply { "Multiply" } else { "Add" };
    let expected = if multiply {
        u64::from(a) + u64::from(b)
    } else {
        u64::from(a.max(b))
    };
    assert_eq!(
        ScaledArithmetic::exact_result_scale(op, a, b)
            .ok()
            .map(u64::from),
        (expected <= 18).then_some(expected)
    );
    assert_eq!(
        ScaledArithmetic::exact_result_scale("Add", a, b),
        ScaledArithmetic::exact_result_scale("Subtract", a, b)
    );
    assert!(ScaledArithmetic::exact_binary("Add", Decimal::MAX, Decimal::ONE).is_err());
    assert!(ScaledArithmetic::exact_binary("Subtract", Decimal::MIN, Decimal::ONE).is_err());
    assert!(ScaledArithmetic::exact_binary("Multiply", Decimal::MAX, Decimal::MAX).is_err());
}

/// VAL-ROUND: checked division uses the declared site, including signed truncation.
#[kani::proof]
#[kani::unwind(40)]
fn exact_division_signed_halves_and_refusals() {
    let a = i16::from(kani::any::<i8>());
    let site = ScaledArithmetic {
        scale: 0,
        rounding: Rounding::TowardZero,
    };
    let result = site.divide(Decimal::from(a), Decimal::from(2)).unwrap();
    assert_eq!(result.mantissa(), i128::from(a / 2));
    assert_eq!(result.scale(), 0);
    assert!(site.divide(Decimal::ONE, Decimal::ZERO).is_err());
    assert!(ScaledArithmetic { scale: 19, ..site }
        .divide(Decimal::ZERO, Decimal::ONE)
        .is_err());
}

/// VAL-ROUND: all u32 scales, finite mode choices and inherited/explicit precedence.
#[kani::proof]
fn explicit_round_site_requires_a_mode_and_bounds_scale() {
    fn mode(choice: u8) -> Option<super::Rounding> {
        match choice % 5 {
            0 => None,
            1 => Some(super::Rounding::Refuse),
            2 => Some(super::Rounding::TowardZero),
            3 => Some(super::Rounding::HalfEven),
            _ => Some(super::Rounding::HalfAwayFromZero),
        }
    }
    let scale: u32 = kani::any();
    let declared = mode(kani::any());
    let inherited =
        mode(kani::any()).map(|rounding| super::ScaledArithmetic { scale: 2, rounding });
    let exact: bool = kani::any();
    let result = super::ScaledArithmetic::rounding_site(scale, declared, inherited, exact);
    if let Some(site) = result.as_ref().ok().and_then(|site| *site) {
        assert!(scale <= 18);
        assert_eq!(site.scale, scale);
        assert_eq!(
            Some(site.rounding),
            declared.or_else(|| inherited.map(|site| site.rounding))
        );
    }
    assert_eq!(
        matches!(result, Ok(None)),
        declared.is_none() && inherited.is_none() && !exact
    );
    assert_eq!(
        result.is_err(),
        (declared.is_none() && inherited.is_none() && exact)
            || (scale > 18 && (declared.is_some() || inherited.is_some() || exact))
    );
}
