//! VAL-UNIT / VAL-RATE: pure bounded cancellation table.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Operand<U, C> {
    Literal,
    Dimensionless,
    Legacy,
    Quantity(U),
    Money(C),
    Rate(C, U),
    Unresolved,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Operation { Add, Subtract, Multiply, Divide, Compare }

/// VAL-RATE: only matching currency and denominator identities cancel.
pub(crate) fn admit<U: Clone + Eq, C: Clone + Eq>(op: Operation, a: &Operand<U, C>, b: &Operand<U, C>) -> Option<(Operand<U, C>, Option<U>)> {
    use Operand::{Dimensionless, Literal, Money, Quantity, Rate, Unresolved, Legacy};
    if matches!(a, Unresolved) || matches!(b, Unresolved) { return None; }
    let mut inferred = None;
    let result = match op {
        _ if matches!((a,b),(Legacy,Legacy | Literal | Dimensionless) | (Literal | Dimensionless,Legacy)) => Legacy,
        Operation::Add | Operation::Subtract | Operation::Compare => {
            let value = match (a,b) {
                (Quantity(u), Literal) | (Literal, Quantity(u)) => {
                    inferred = Some(u.clone()); Quantity(u.clone())
                }
                (Literal | Dimensionless, Literal | Dimensionless) => Dimensionless,
                _ if a == b => a.clone(),
                _ => return None,
            };
            if op == Operation::Compare { Dimensionless } else { value }
        }
        Operation::Multiply => match (a,b) {
            (Rate(c,u), Quantity(v)) | (Quantity(v), Rate(c,u)) if u == v => Money(c.clone()),
            (Quantity(u), Literal | Dimensionless) | (Literal | Dimensionless, Quantity(u)) => Quantity(u.clone()),
            (Literal | Dimensionless, Literal | Dimensionless) => Dimensionless,
            _ => return None,
        },
        Operation::Divide => match (a,b) {
            (Money(c), Quantity(u)) => Rate(c.clone(),u.clone()),
            (Quantity(u), Quantity(v)) if u == v => Dimensionless,
            (Literal | Dimensionless, Literal | Dimensionless) => Dimensionless,
            _ => return None,
        },
    };
    Some((result,inferred))
}
pub(crate) fn join<U: Clone + Eq, C: Clone + Eq>(a: &Operand<U, C>, b: &Operand<U, C>) -> Option<Operand<U, C>> {
    if matches!(a, Operand::Unresolved) || matches!(b, Operand::Unresolved) { return None; }
    if a == b { return Some(a.clone()); }
    if matches!((a,b),(Operand::Legacy,Operand::Literal | Operand::Dimensionless) | (Operand::Literal | Operand::Dimensionless,Operand::Legacy)) {return Some(Operand::Legacy); }
    matches!((a,b), (Operand::Literal | Operand::Dimensionless, Operand::Literal | Operand::Dimensionless)).then_some(Operand::Dimensionless)
}
pub(crate) fn destination<U: Clone + Eq, C: Clone + Eq>(found: &Operand<U, C>, expected: &Operand<U, C>) -> bool {
    !matches!(expected, Operand::Unresolved) && (matches!(found, Operand::Literal) || matches!((found,expected),(Operand::Dimensionless,Operand::Legacy)) || join(found,expected).is_some())
}
