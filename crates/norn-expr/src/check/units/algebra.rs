//! VAL-UNIT / VAL-RATE: pure bounded cancellation table.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Operand<U, C> {
    Literal,
    LiteralZero,
    Dimensionless,
    Legacy,
    Quantity(U),
    Money(C),
    Rate(C, U),
    Bound(String),
    Unresolved,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Operation {
    Add,
    Subtract,
    Multiply,
    Divide,
    Compare,
}

/// VAL-RATE: only matching currency and denominator identities cancel.
pub(crate) fn admit<U: Clone + Eq, C: Clone + Eq>(
    op: Operation,
    left: &Operand<U, C>,
    right: &Operand<U, C>,
) -> Option<(Operand<U, C>, Option<U>)> {
    use self::Operand::{
        Bound, Dimensionless, Legacy, Literal, LiteralZero, Money, Quantity, Rate, Unresolved,
    };
    if matches!(left, Unresolved) || matches!(right, Unresolved) {
        return None;
    }
    let mut inferred = None;
    let result = match op {
        _ if matches!(
            (left, right),
            (Legacy, Legacy | Literal | LiteralZero | Dimensionless)
                | (Literal | LiteralZero | Dimensionless, Legacy)
        ) =>
        {
            Legacy
        }
        Operation::Add | Operation::Subtract | Operation::Compare => {
            let value = match (left, right) {
                (Quantity(unit), Literal | LiteralZero)
                | (Literal | LiteralZero, Quantity(unit)) => {
                    inferred = Some(unit.clone());
                    Quantity(unit.clone())
                }
                (Bound(_), LiteralZero) | (LiteralZero, Bound(_)) if op == Operation::Compare => {
                    Dimensionless
                }
                (Bound(_), _) | (_, Bound(_)) => return None,
                (Literal | LiteralZero | Dimensionless, Literal | LiteralZero | Dimensionless) => {
                    Dimensionless
                }
                _ if left == right => left.clone(),
                _ => return None,
            };
            if op == Operation::Compare {
                Dimensionless
            } else {
                value
            }
        }
        Operation::Multiply => match (left, right) {
            (Rate(currency, unit), Quantity(other_unit))
            | (Quantity(other_unit), Rate(currency, unit))
                if unit == other_unit =>
            {
                Money(currency.clone())
            }
            (Quantity(unit), Literal | LiteralZero | Dimensionless)
            | (Literal | LiteralZero | Dimensionless, Quantity(unit)) => Quantity(unit.clone()),
            (Literal | LiteralZero | Dimensionless, Literal | LiteralZero | Dimensionless) => {
                Dimensionless
            }
            _ => return None,
        },
        Operation::Divide => match (left, right) {
            (Money(currency), Quantity(unit)) => Rate(currency.clone(), unit.clone()),
            (Quantity(unit), Quantity(other_unit)) if unit == other_unit => Dimensionless,
            (Literal | LiteralZero | Dimensionless, Literal | LiteralZero | Dimensionless) => {
                Dimensionless
            }
            _ => return None,
        },
    };
    Some((result, inferred))
}
pub(crate) fn join<U: Clone + Eq, C: Clone + Eq>(
    left: &Operand<U, C>,
    right: &Operand<U, C>,
) -> Option<Operand<U, C>> {
    if matches!(left, Operand::Unresolved | Operand::Bound(_))
        || matches!(right, Operand::Unresolved | Operand::Bound(_))
    {
        return None;
    }
    if left == right {
        return Some(left.clone());
    }
    if matches!(
        (left, right),
        (Operand::Literal, Operand::LiteralZero) | (Operand::LiteralZero, Operand::Literal)
    ) {
        return Some(Operand::Literal);
    }
    if matches!(
        (left, right),
        (
            Operand::Legacy,
            Operand::Literal | Operand::LiteralZero | Operand::Dimensionless
        ) | (
            Operand::Literal | Operand::LiteralZero | Operand::Dimensionless,
            Operand::Legacy
        )
    ) {
        return Some(Operand::Legacy);
    }
    matches!(
        (left, right),
        (
            Operand::Literal | Operand::LiteralZero | Operand::Dimensionless,
            Operand::Literal | Operand::LiteralZero | Operand::Dimensionless
        )
    )
    .then_some(Operand::Dimensionless)
}
pub(crate) fn destination<U: Clone + Eq, C: Clone + Eq>(
    found: &Operand<U, C>,
    expected: &Operand<U, C>,
) -> bool {
    if matches!(expected, Operand::Unresolved | Operand::Bound(_))
        || matches!(found, Operand::Unresolved | Operand::Bound(_))
    {
        return false;
    }
    matches!(found, Operand::Literal | Operand::LiteralZero)
        || matches!((found, expected), (Operand::Dimensionless, Operand::Legacy))
        || join(found, expected).is_some()
}
