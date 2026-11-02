//! VAL-UNIT checked implementation core over all u8 code identities.
//! Strings, vocabulary admission and expression lowering are outside this proof.
#[path = "../../crates/norn-expr/src/units.rs"]
mod units;
use units::{Operand, Operation};

#[kani::proof]
fn val_unit_mixing_compounds_and_inverses_refuse() {
    let (a, b): (u8, u8) = kani::any();
    let first = Operand::Quantity(a);
    let second = Operand::Quantity(b);
    for op in [Operation::Add, Operation::Subtract, Operation::Compare, Operation::Divide] {
        assert_eq!(units::admit(op, &first, &second).is_ok(), a == b);
    }
    assert!(units::admit(Operation::Multiply, &first, &second).is_err());
    assert!(units::admit(Operation::Divide, &Operand::Dimensionless, &first).is_err());
    for op in [Operation::Add, Operation::Subtract, Operation::Compare] {
        let inferred = units::admit(op, &first, &Operand::Literal).unwrap();
        assert_eq!(inferred.inferred_literal_unit, Some(a));
        assert!(units::admit(op, &first, &Operand::Dimensionless).is_err());
    }
    assert_eq!(units::admit(Operation::Multiply, &first, &Operand::Dimensionless).unwrap().result, first);
}
