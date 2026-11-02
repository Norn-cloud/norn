//! VAL-UNIT / VAL-RATE: bounded numeric dimensions, without implicit conversion.
use crate::{Type, ZenTypeEnv};
use norn_ir::{UnitRef, metric::DimensionPath};

mod algebra;
pub(super) use algebra::{Operation, admit, destination, join};
pub(super) type Operand = algebra::Operand<UnitRef, String>;

pub(super) fn declared(env: &ZenTypeEnv, owner: Option<&str>, unit: Option<&UnitRef>, source: Option<&DimensionPath>, per: Option<&UnitRef>) -> Operand {
    match (unit, source, per) {
        (Some(u), None, None) => Operand::Quantity(u.clone()),
        (None, None, None) => Operand::Dimensionless,
        (None, Some(path), per) => currency(env,owner,path).map_or(Operand::Unresolved, |c| per.map_or_else(|| Operand::Money(c.clone()), |u| Operand::Rate(c.clone(),u.clone()))),
        _ => Operand::Unresolved,
    }
}
fn currency(env: &ZenTypeEnv, owner: Option<&str>, path: &DimensionPath) -> Option<String> {
    fn record_currency(env: &ZenTypeEnv, mut record: &str, path: &DimensionPath) -> Option<String> {
        for link in &path.links {
            let Type::Record(next) = &env.record(record)?.fields.get(link.as_str())?.ty else { return None; };
            record = next;
        }
        let Type::Enum { values } = &env.record(record)?.fields.get(path.property.as_str())?.ty else { return None; };
        (values.len() == 1).then(|| values[0].clone())
    }
    if let Some(owner) = owner { return record_currency(env,owner,path); }
    let mut candidates = Vec::new();
    if path.links.is_empty() && let Some(Type::Enum { values }) = env.variables().get(path.property.as_str()) {
        if values.len() != 1 { return None; }
        candidates.push(values[0].clone());
    }
    for name in ["input", "self"] {
        if let Some(Type::Record(record)) = env.variables().get(name) && let Some(c) = record_currency(env,record,path) { candidates.push(c); }
    }
    let first = candidates.first()?;
    candidates.iter().all(|c| c == first).then(|| first.clone())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn grams() -> UnitRef { UnitRef { system:norn_ir::UnitSystem::Ucum, code:"g".into() } }
    #[test]
    fn val_rate_matches_currency_and_denominator_and_refuses_other_compounds() {
        let quantity = Operand::Quantity(grams());
        let usd = Operand::Money("USD".into());
        let rate = Operand::Rate("USD".into(),grams());
        assert_eq!(admit(Operation::Multiply,&rate,&quantity).unwrap().0,usd);
        assert_eq!(admit(Operation::Divide,&usd,&quantity).unwrap().0,rate);
        assert!(admit(Operation::Multiply,&rate,&rate).is_none());
        assert!(admit(Operation::Add,&rate,&Operand::Rate("EUR".into(),grams())).is_none());
        assert!(admit(Operation::Multiply,&rate,&Operand::Quantity(UnitRef { code:"mg".into(),..grams() })).is_none());
        assert!(admit(Operation::Add,&quantity,&Operand::Dimensionless).is_none());
        assert_eq!(admit(Operation::Compare,&quantity,&Operand::Literal).unwrap().1,Some(grams()));
    }
    proptest::proptest! {
        #[test]
        fn val_rate_cancellation_is_exact_identity_equality(a in proptest::prelude::any::<u8>(), b in proptest::prelude::any::<u8>(), c in proptest::prelude::any::<u8>(), d in proptest::prelude::any::<u8>()) {
            use algebra::Operand as O;
            let rate = O::Rate(a,b);
            let quantity = O::Quantity(d);
            proptest::prop_assert_eq!(admit(Operation::Multiply,&rate,&quantity).is_some(), b == d);
            proptest::prop_assert_eq!(admit(Operation::Compare,&rate,&O::Rate(c,d)).is_some(), a == c && b == d);
            proptest::prop_assert!(admit(Operation::Multiply,&rate,&O::Rate(c,d)).is_none());
        }
    }
}
