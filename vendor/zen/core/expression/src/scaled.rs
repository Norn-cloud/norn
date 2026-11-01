//! Checked scaled-integer arithmetic for a declaration's numeric profile.
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

#[cfg(test)]
mod tests {
    use super::*;
    fn calculate(op: &str, a: &str, b: &str, rounding: Rounding) -> Result<String, &'static str> {
        ScaledArithmetic { scale: 2, rounding }
            .binary(op, a.parse().unwrap(), b.parse().unwrap())
            .map(|n| n.normalize().to_string())
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
}
