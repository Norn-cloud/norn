//! Executable reference and Kani starter for INV-QL-CONSERVATION and
//! INV-QL-EXACT-ARITHMETIC.
//!
//! This file is not yet production correspondence. The fork must either use
//! these exact functions in its seal path or replace this file with harnesses
//! importing the production symbols named in README.md.

#![forbid(unsafe_code)]
#![allow(
    unexpected_cfgs,
    reason = "the standalone Kani invocation does not define rustc's test cfg"
)]

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Leg {
    pub unit: u8,
    pub layer: u8,
    pub amount: i128,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ArithmeticError {
    Overflow,
}

/// Returns true exactly when each (unit, layer) partition sums to zero using
/// checked i128 arithmetic. Repeated scans avoid giving the proof harness a
/// HashMap model; the production implementation may optimize after a
/// differential test binds it to this function.
pub fn is_balanced_by_key(legs: &[Leg]) -> Result<bool, ArithmeticError> {
    for key in legs {
        let mut total = 0_i128;
        for leg in legs {
            if leg.unit == key.unit && leg.layer == key.layer {
                total = total
                    .checked_add(leg.amount)
                    .ok_or(ArithmeticError::Overflow)?;
            }
        }
        if total != 0 {
            return Ok(false);
        }
    }
    Ok(true)
}

#[cfg(kani)]
mod proofs {
    use super::*;

    #[kani::proof]
    fn a_balanced_pair_is_accepted() {
        let amount: i64 = kani::any();
        let unit: u8 = kani::any();
        let layer: u8 = kani::any();
        let amount = i128::from(amount);
        let legs = [
            Leg { unit, layer, amount },
            Leg { unit, layer, amount: -amount },
        ];
        assert_eq!(is_balanced_by_key(&legs), Ok(true));
    }

    #[kani::proof]
    fn equal_values_in_different_units_are_not_balanced() {
        let amount: u16 = kani::any();
        kani::assume(amount > 0);
        let amount = i128::from(amount);
        let legs = [
            Leg { unit: 1, layer: 0, amount },
            Leg { unit: 2, layer: 0, amount: -amount },
        ];
        assert_eq!(is_balanced_by_key(&legs), Ok(false));
    }

    #[kani::proof]
    fn overflow_is_rejected_not_wrapped() {
        let legs = [
            Leg { unit: 1, layer: 0, amount: i128::MAX },
            Leg { unit: 1, layer: 0, amount: 1 },
            Leg { unit: 1, layer: 0, amount: -i128::MAX },
            Leg { unit: 1, layer: 0, amount: -1 },
        ];
        assert_eq!(is_balanced_by_key(&legs), Err(ArithmeticError::Overflow));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn partitions_by_unit_and_layer() {
        let legs = [
            Leg { unit: 1, layer: 0, amount: 10 },
            Leg { unit: 1, layer: 0, amount: -10 },
            Leg { unit: 2, layer: 1, amount: 7 },
            Leg { unit: 2, layer: 1, amount: -7 },
        ];
        assert_eq!(is_balanced_by_key(&legs), Ok(true));
    }

    #[test]
    fn cannot_balance_across_layers() {
        let legs = [
            Leg { unit: 1, layer: 0, amount: 10 },
            Leg { unit: 1, layer: 1, amount: -10 },
        ];
        assert_eq!(is_balanced_by_key(&legs), Ok(false));
    }

    #[test]
    fn rejects_intermediate_overflow() {
        let legs = [
            Leg { unit: 1, layer: 0, amount: i128::MAX },
            Leg { unit: 1, layer: 0, amount: 1 },
            Leg { unit: 1, layer: 0, amount: -i128::MAX },
            Leg { unit: 1, layer: 0, amount: -1 },
        ];
        assert_eq!(is_balanced_by_key(&legs), Err(ArithmeticError::Overflow));
    }
}
