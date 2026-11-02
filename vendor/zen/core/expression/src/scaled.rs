//! FOLD-SCALED: checked scaled-integer arithmetic for the declared numeric profile.
use rust_decimal::Decimal;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Rounding {
    Refuse,
    TowardZero,
    HalfEven,
    HalfAwayFromZero,
}

#[derive(Debug, Clone, Copy)]
pub struct ScaledArithmetic {
    pub scale: u32,
    pub rounding: Rounding,
}

impl ScaledArithmetic {
    /// VAL-ROUND: explicit modes override inherited sites; absent modes never round.
    pub fn rounding_site(
        scale: u32,
        mode: Option<Rounding>,
        inherited: Option<Self>,
        exact: bool,
    ) -> Result<Option<Self>, &'static str> {
        if mode.is_none() && inherited.is_none() && !exact {
            return Ok(None);
        }
        let rounding = mode
            .or_else(|| inherited.map(|site| site.rounding))
            .ok_or("rounding must be declared")?;
        if scale > 18 {
            return Err("invalid exact site scale");
        }
        Ok(Some(Self { scale, rounding }))
    }

    /// VAL-EXACT-1: exact operations derive their scale without a rounding site.
    pub fn exact_result_scale(op: &str, a: u32, b: u32) -> Result<u32, &'static str> {
        let scale = match op {
            "Add" | "Subtract" => a.max(b),
            "Multiply" => a.checked_add(b).ok_or("exact result scale overflow")?,
            _ => return Err("operation requires a declared result scale and rounding"),
        };
        (scale <= 18)
            .then_some(scale)
            .ok_or("exact result scale exceeds 18")
    }

    /// VAL-EXACT-1: checked addition, subtraction and multiplication never round.
    pub fn exact_binary(op: &str, a: Decimal, b: Decimal) -> Result<Decimal, &'static str> {
        let scale = Self::exact_result_scale(op, a.scale(), b.scale())?;
        let coefficient = match op {
            "Multiply" => a.mantissa().checked_mul(b.mantissa()),
            "Add" | "Subtract" => {
                let profile = Self {
                    scale,
                    rounding: Rounding::Refuse,
                };
                let (a, b) = (profile.coefficient(a)?, profile.coefficient(b)?);
                if op == "Add" {
                    a.checked_add(b)
                } else {
                    a.checked_sub(b)
                }
            }
            _ => return Err("unsupported exact operation"),
        }
        .ok_or("exact coefficient overflow")?;
        Decimal::try_from_i128_with_scale(coefficient, scale)
            .map_err(|_| "number exceeds the exact decimal range")
    }

    /// VAL-EXACT-1: compare after checked exact scale alignment.
    pub fn exact_compare(a: Decimal, b: Decimal) -> Result<std::cmp::Ordering, &'static str> {
        let scale = Self::exact_result_scale("Add", a.scale(), b.scale())?;
        let profile = Self {
            scale,
            rounding: Rounding::Refuse,
        };
        Ok(profile.coefficient(a)?.cmp(&profile.coefficient(b)?))
    }

    pub fn coefficient(self, value: Decimal) -> Result<i128, &'static str> {
        let value = value.normalize();
        let places = self
            .scale
            .checked_sub(value.scale())
            .filter(|_| self.scale <= 18)
            .ok_or("number is outside the declared exact scale")?;
        value
            .mantissa()
            .checked_mul(10_i128.pow(places))
            .ok_or("scaled coefficient overflow")
    }

    /// FOLD-RESCALE / VAL-ROUND: quantize at an explicit result-scale site.
    /// Higher source precision is accepted here; inexact results obey the mode.
    /// Invalid target scales and intermediate or exact-decimal overflow refuse.
    pub fn rescale(self, value: Decimal) -> Result<Decimal, &'static str> {
        if self.scale > 18 {
            return Err("number is outside the declared exact scale");
        }
        let value = value.normalize();
        let coefficient = if self.scale >= value.scale() {
            value
                .mantissa()
                .checked_mul(10_i128.pow(self.scale - value.scale()))
                .ok_or("scaled coefficient overflow")?
        } else {
            self.quotient(value.mantissa(), 10_i128.pow(value.scale() - self.scale))?
        };
        Decimal::try_from_i128_with_scale(coefficient, self.scale)
            .map_err(|_| "number exceeds the exact decimal range")
    }

    /// VAL-ROUND: divide exact operands directly into a declared rounding site.
    pub fn divide(self, a: Decimal, b: Decimal) -> Result<Decimal, &'static str> {
        Self::exact_result_scale("Add", a.scale(), b.scale())?;
        if self.scale > 18 {
            return Err("number is outside the declared exact scale");
        }
        let exponent = i64::from(self.scale) + i64::from(b.scale()) - i64::from(a.scale());
        let places = u32::try_from(exponent.unsigned_abs())
            .map_err(|_overflow| "division scale overflow")?;
        let factor = 10_i128.pow(places);
        let (numerator, denominator) = if exponent >= 0 {
            (
                a.mantissa()
                    .checked_mul(factor)
                    .ok_or("dividend overflow")?,
                b.mantissa(),
            )
        } else {
            (
                a.mantissa(),
                b.mantissa().checked_mul(factor).ok_or("divisor overflow")?,
            )
        };
        Decimal::try_from_i128_with_scale(self.quotient(numerator, denominator)?, self.scale)
            .map_err(|_| "number exceeds the exact decimal range")
    }

    pub fn binary(self, op: &str, a: Decimal, b: Decimal) -> Result<Decimal, &'static str> {
        let (a, b) = (self.coefficient(a)?, self.coefficient(b)?);
        let factor = 10_i128.pow(self.scale);
        let checked = match op {
            "Add" => a.checked_add(b),
            "Subtract" => a.checked_sub(b),
            "Multiply" => Some(self.quotient(a.checked_mul(b).ok_or("product overflow")?, factor)?),
            "Divide" => Some(self.quotient(a.checked_mul(factor).ok_or("dividend overflow")?, b)?),
            _ => return Err("unsupported scaled operation"),
        }
        .ok_or("scaled coefficient overflow")?;
        Decimal::try_from_i128_with_scale(checked, self.scale)
            .map_err(|_| "number exceeds the exact decimal range")
    }

    /// FOLD-ARITHMETIC: exact signed remainders select the declared rounding rule.
    fn quotient(self, numerator: i128, denominator: i128) -> Result<i128, &'static str> {
        let quotient = numerator
            .checked_div(denominator)
            .ok_or("zero divisor or quotient overflow")?;
        let remainder = numerator
            .checked_rem(denominator)
            .ok_or("remainder overflow")?;
        if remainder == 0 {
            return Ok(quotient);
        }
        let round = match self.rounding {
            Rounding::Refuse => return Err("inexact result refused"),
            Rounding::TowardZero => false,
            Rounding::HalfEven | Rounding::HalfAwayFromZero => {
                let magnitude = remainder.unsigned_abs();
                let complement = denominator.unsigned_abs() - magnitude;
                magnitude > complement
                    || (magnitude == complement
                        && (self.rounding == Rounding::HalfAwayFromZero || quotient % 2 != 0))
            }
        };
        if round {
            let direction = if (numerator < 0) == (denominator < 0) {
                1
            } else {
                -1
            };
            quotient.checked_add(direction).ok_or("rounding overflow")
        } else {
            Ok(quotient)
        }
    }
}

#[cfg(kani)]
#[path = "../../../../../verification/ordered-fold-v1/scaled_kani.rs"]
mod proof;

#[cfg(test)]
mod tests {
    use super::*;
    fn calculate(op: &str, a: &str, b: &str, rounding: Rounding) -> Result<String, &'static str> {
        ScaledArithmetic { scale: 2, rounding }
            .binary(op, a.parse().unwrap(), b.parse().unwrap())
            .map(|n| n.normalize().to_string())
    }
    #[test]
    fn explicit_rescale_covers_signed_midpoints_and_refusal_boundaries() {
        for (mode, expected) in [
            (Rounding::TowardZero, "1.00"),
            (Rounding::HalfEven, "1.00"),
            (Rounding::HalfAwayFromZero, "1.01"),
        ] {
            let p = ScaledArithmetic {
                scale: 2,
                rounding: mode,
            };
            for sign in ["", "-"] {
                let n = p.rescale(format!("{sign}1.005").parse().unwrap()).unwrap();
                assert_eq!(n.to_string(), format!("{sign}{expected}"));
            }
        }
        let p = ScaledArithmetic {
            scale: 2,
            rounding: Rounding::Refuse,
        };
        assert_eq!(
            p.rescale("1.0100".parse().unwrap()).unwrap().to_string(),
            "1.01"
        );
        assert!(p.rescale("1.005".parse().unwrap()).is_err());
        assert!(p.rescale(Decimal::MAX).is_err());
        assert!(ScaledArithmetic { scale: 18, ..p }
            .rescale(Decimal::MAX)
            .is_err());
        assert!(ScaledArithmetic { scale: 19, ..p }
            .rescale(Decimal::ZERO)
            .is_err());
        assert_eq!(
            ScaledArithmetic { scale: 18, ..p }
                .rescale(Decimal::ZERO)
                .unwrap()
                .scale(),
            18
        );
        assert_eq!(
            p.rescale("0.0000000000000000000000000001".parse().unwrap()),
            Err("inexact result refused")
        );
    }

    #[test]
    fn store_rounding_does_not_quantize_exact_intermediate_products() {
        let p = ScaledArithmetic {
            scale: 3,
            rounding: Rounding::Refuse,
        };
        let intermediate = p
            .binary("Multiply", "1.25".parse().unwrap(), "0.1".parse().unwrap())
            .unwrap();
        let exact = p
            .binary("Multiply", intermediate, Decimal::from(100))
            .unwrap();
        let site = ScaledArithmetic {
            scale: 2,
            rounding: Rounding::HalfEven,
        };
        assert_eq!(site.rescale(exact).unwrap().to_string(), "12.50");
        assert!(ScaledArithmetic {
            rounding: Rounding::Refuse,
            ..site
        }
        .binary("Multiply", "1.25".parse().unwrap(), "0.1".parse().unwrap())
        .is_err());
    }

    #[test]
    fn declared_rounding_is_symmetric_and_uses_exact_remainders() {
        for (rounding, eighth, three_eighths) in [
            (Rounding::TowardZero, "0.12", "0.37"),
            (Rounding::HalfEven, "0.12", "0.38"),
            (Rounding::HalfAwayFromZero, "0.13", "0.38"),
        ] {
            for (a, result) in [("1", eighth), ("3", three_eighths)] {
                assert_eq!(calculate("Divide", a, "8", rounding), Ok(result.into()));
                assert_eq!(
                    calculate("Divide", &format!("-{a}"), "8", rounding),
                    Ok(format!("-{result}"))
                );
                assert_eq!(
                    calculate("Divide", a, "-8", rounding),
                    Ok(format!("-{result}"))
                );
            }
        }
        assert_eq!(
            calculate("Divide", "1", "4", Rounding::Refuse),
            Ok("0.25".into())
        );
        assert!(calculate("Divide", "1", "8", Rounding::Refuse).is_err());
    }
    #[test]
    fn vm_policy_shares_fuel_and_refuses_implicit_numeric_fallbacks() {
        use crate::{compile_expression, meter::Meter, vm::VM, Variable};
        let meter = Meter::new(1000);
        let mut vm = VM::new();
        vm.set_meter(Some(meter.with_arithmetic(ScaledArithmetic {
            scale: 2,
            rounding: Rounding::HalfAwayFromZero,
        })));
        let value = compile_expression("1 / 8")
            .unwrap()
            .evaluate_with(Variable::Null, &mut vm)
            .unwrap();
        assert_eq!(value, Variable::Number("0.13".parse().unwrap()));
        assert!(meter.used() > 0);
        for source in ["1 / 0", "sum([1, 2])", "round(1.25)", "2 ^ 3", "0.001 + 1"] {
            assert!(
                compile_expression(source)
                    .unwrap()
                    .evaluate_with(Variable::Null, &mut vm)
                    .is_err(),
                "{source}"
            );
        }
    }

    #[test]
    fn profiled_identity_negation_and_nested_outputs_require_exact_inputs() {
        use crate::{compile_expression, meter::Meter, vm::VM, Variable};
        let mut vm = VM::new();
        vm.set_meter(Some(Meter::new(1000).with_arithmetic(ScaledArithmetic {
            scale: 2,
            rounding: Rounding::Refuse,
        })));
        for source in ["x", "-x", "{nested:[x]}"] {
            let context = |n: &str| {
                Variable::from_object(
                    [(Variable::key("x"), Variable::Number(n.parse().unwrap()))]
                        .into_iter()
                        .collect(),
                )
            };
            let expression = compile_expression(source).unwrap();
            assert!(expression.evaluate_with(context("0.001"), &mut vm).is_err());
            assert!(expression.evaluate_with(context("0.01"), &mut vm).is_ok());
        }
    }

    #[test]
    fn exact_operations_and_refusal_boundaries() {
        for (op, a, b, result) in [
            ("Add", "0.1", "0.2", "0.3"),
            ("Subtract", "0.1", "0.2", "-0.1"),
            ("Multiply", "1.25", "2", "2.5"),
            ("Multiply", "0.5", "0.25", "0.12"),
        ] {
            assert_eq!(calculate(op, a, b, Rounding::HalfEven), Ok(result.into()));
        }
        for (op, a, b) in [
            ("Divide", "0", "0"),
            ("Add", "0.001", "1"),
            ("Multiply", "79228162514264337593543950335", "100"),
            ("Add", "79228162514264337593543950335", "1"),
        ] {
            assert!(calculate(op, a, b, Rounding::Refuse).is_err());
        }
        let profile = ScaledArithmetic {
            scale: 18,
            rounding: Rounding::Refuse,
        };
        assert!(profile
            .binary(
                "Add",
                Decimal::ZERO,
                "0.000000000000000001".parse().unwrap()
            )
            .is_ok());
        assert!(ScaledArithmetic {
            scale: 19,
            ..profile
        }
        .coefficient(Decimal::ZERO)
        .is_err());
    }
    #[test]
    fn derived_exact_operations_preserve_intermediates_and_refuse_overflow() {
        let product = ScaledArithmetic::exact_binary(
            "Multiply",
            "1.25".parse().unwrap(),
            "0.1".parse().unwrap(),
        )
        .unwrap();
        assert_eq!(product.to_string(), "0.125");
        let result =
            ScaledArithmetic::exact_binary("Multiply", product, Decimal::from(100)).unwrap();
        assert_eq!(
            ScaledArithmetic {
                scale: 2,
                rounding: Rounding::HalfEven
            }
            .rescale(result)
            .unwrap()
            .to_string(),
            "12.50"
        );
        for (op, a, b) in [
            ("Add", Decimal::MAX, Decimal::ONE),
            ("Subtract", Decimal::MIN, Decimal::ONE),
            ("Multiply", Decimal::MAX, Decimal::MAX),
        ] {
            assert!(ScaledArithmetic::exact_binary(op, a, b).is_err());
        }
        assert!(ScaledArithmetic::exact_binary("Divide", Decimal::ONE, Decimal::ONE).is_err());
    }
}
