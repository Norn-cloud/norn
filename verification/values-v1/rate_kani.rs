//! Bounded evidence for the production cancellation table; not vocabulary or AST proof.
#[path = "../../crates/norn-expr/src/check/units/algebra.rs"]
mod algebra;
#[kani::proof]
fn val_rate_only_matching_identities_cancel() {
    use algebra::{Operand, Operation, admit};
    let currency: u8 = kani::any();
    let other_currency: u8 = kani::any();
    let unit: u8 = kani::any();
    let other_unit: u8 = kani::any();
    let rate = Operand::Rate(currency,unit);
    assert_eq!(admit(Operation::Multiply,&rate,&Operand::Quantity(other_unit)).is_some(),unit == other_unit);
    assert_eq!(admit(Operation::Add,&rate,&Operand::Rate(other_currency,other_unit)).is_some(),currency == other_currency && unit == other_unit);
    assert!(admit(Operation::Multiply,&rate,&rate).is_none());
    assert!(admit(Operation::Divide,&Operand::Dimensionless,&rate).is_none());
    let money = Operand::Money(currency);
    assert_eq!(admit(Operation::Multiply,&money,&Operand::Dimensionless),Some((money.clone(),None)));
    assert_eq!(admit(Operation::Multiply,&Operand::Dimensionless,&money),Some((money.clone(),None)));
    assert!(admit(Operation::Multiply,&money,&Operand::Money(other_currency)).is_none());
    assert!(admit(Operation::Multiply,&money,&Operand::Quantity(unit)).is_none());
    assert!(admit(Operation::Multiply,&money,&Operand::Legacy).is_none());
}
