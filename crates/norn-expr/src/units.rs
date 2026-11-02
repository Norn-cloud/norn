//! VAL-UNIT: bounded unit algebra; admission supplies pinned vocabulary members.

/// Numeric operand provenance. A literal may inherit a quantity unit; a
/// declared dimensionless property cannot. The unit is the caller's existing
/// unit reference, not a second vocabulary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Operand<U> {
    /// A numeric literal, including its unary sign.
    Literal,
    /// A declared dimensionless value or an arithmetic result without units.
    Dimensionless,
    /// A value with a pinned unit reference.
    Quantity(U),
}

/// The arithmetic and ordering operations supported before compound units.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Operation {
    /// Same-code addition.
    Add,
    /// Same-code subtraction.
    Subtract,
    /// Scalar multiplication.
    Multiply,
    /// Same-code quantity division to a scalar.
    Divide,
    /// Equality or ordering.
    Compare,
}

/// The admitted result and the unit assigned to a literal, if any.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Admission<U> {
    /// Result qualifier; comparisons produce a dimensionless Boolean.
    pub result: Operand<U>,
    /// Explanation evidence for additive or comparison literal inference.
    pub inferred_literal_unit: Option<U>,
}

/// VAL-UNIT refuses mixed, compound, inverse or dimensionless/quantity pairs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UnitMismatch;

/// Apply VAL-UNIT without conversions or numeric evaluation.
///
/// # Errors
/// Returns [`UnitMismatch`] when the declared operation requires a conversion
/// or unit algebra outside this bounded profile.
pub fn admit<U: Clone + Eq>(
    operation: Operation,
    left: &Operand<U>,
    right: &Operand<U>,
) -> Result<Admission<U>, UnitMismatch> {
    use Operand::{Dimensionless, Literal, Quantity};
    let mut inferred_literal_unit = None;
    let result = match operation {
        Operation::Add | Operation::Subtract | Operation::Compare => {
            let result = match (left, right) {
                (Quantity(a), Quantity(b)) if a == b => Quantity(a.clone()),
                (Quantity(unit), Literal) | (Literal, Quantity(unit)) => {
                    inferred_literal_unit = Some(unit.clone());
                    Quantity(unit.clone())
                }
                (Dimensionless | Literal, Dimensionless | Literal) => Dimensionless,
                _ => return Err(UnitMismatch),
            };
            if operation == Operation::Compare { Dimensionless } else { result }
        }
        Operation::Multiply => match (left, right) {
            (Quantity(unit), Dimensionless | Literal) | (Dimensionless | Literal, Quantity(unit)) => Quantity(unit.clone()),
            (Quantity(_), Quantity(_)) => return Err(UnitMismatch),
            (Dimensionless | Literal, Dimensionless | Literal) => Dimensionless,
        },
        Operation::Divide => match (left, right) {
            (Quantity(a), Quantity(b)) if a == b => Dimensionless,
            (Quantity(_), _) | (_, Quantity(_)) => return Err(UnitMismatch),
            (Dimensionless | Literal, Dimensionless | Literal) => Dimensionless,
        },
    };
    Ok(Admission { result, inferred_literal_unit })
}
