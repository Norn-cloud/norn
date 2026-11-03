//! VAL-UNIT / VAL-RATE: bounded numeric dimensions, without implicit conversion.
use crate::{Type, ZenTypeEnv};
use norn_ir::{UnitRef, metric::DimensionPath};

mod algebra;
pub(super) use algebra::{Operation, admit, destination, join};
pub(super) type Operand = algebra::Operand<UnitRef, String>;

pub(super) fn declared(
    env: &ZenTypeEnv,
    owner: Option<&str>,
    unit: Option<&UnitRef>,
    source: Option<&DimensionPath>,
    per: Option<&UnitRef>,
) -> Operand {
    match (unit, source, per) {
        (Some(u), None, None) => Operand::Quantity(u.clone()),
        (None, None, None) => Operand::Dimensionless,
        (None, Some(path), per) => currency(env, owner, path).map_or(Operand::Unresolved, |c| {
            per.map_or_else(
                || Operand::Money(c.clone()),
                |u| Operand::Rate(c.clone(), u.clone()),
            )
        }),
        _ => Operand::Unresolved,
    }
}
fn currency(env: &ZenTypeEnv, owner: Option<&str>, path: &DimensionPath) -> Option<String> {
    fn record_currency<'a>(
        env: &'a ZenTypeEnv,
        mut record: &'a str,
        path: &DimensionPath,
    ) -> Option<String> {
        for link in &path.links {
            let Type::Record(next) = &env.record(record)?.fields.get(link.as_str())?.ty else {
                return None;
            };
            record = next;
        }
        let Type::Enum { values } = &env.record(record)?.fields.get(path.property.as_str())?.ty
        else {
            return None;
        };
        let [code] = values.as_slice() else {
            return None;
        };
        (code.len() == 3 && code.bytes().all(|c| c.is_ascii_uppercase())).then(|| code.clone())
    }
    if let Some(owner) = owner {
        return record_currency(env, owner, path);
    }
    let mut candidates = Vec::new();
    if path.links.is_empty()
        && let Some(Type::Enum { values }) = env.variables().get(path.property.as_str())
    {
        let [code] = values.as_slice() else {
            return None;
        };
        candidates.push(code.clone());
    }
    for name in ["input", "self"] {
        if let Some(Type::Record(record)) = env.variables().get(name)
            && let Some(c) = record_currency(env, record, path)
        {
            candidates.push(c);
        }
    }
    let first = candidates.first()?;
    candidates.iter().all(|c| c == first).then(|| first.clone())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn grams() -> UnitRef {
        UnitRef {
            system: norn_ir::UnitSystem::Ucum,
            code: "g".into(),
        }
    }
    #[test]
    fn val_rate_matches_currency_and_denominator_and_refuses_other_compounds() {
        let quantity = Operand::Quantity(grams());
        let usd = Operand::Money("USD".into());
        let rate = Operand::Rate("USD".into(), grams());
        assert_eq!(admit(Operation::Multiply, &rate, &quantity).unwrap().0, usd);
        assert_eq!(admit(Operation::Divide, &usd, &quantity).unwrap().0, rate);
        assert!(admit(Operation::Multiply, &rate, &rate).is_none());
        assert!(admit(Operation::Add, &rate, &Operand::Rate("EUR".into(), grams())).is_none());
        assert!(
            admit(
                Operation::Multiply,
                &rate,
                &Operand::Quantity(UnitRef {
                    code: "mg".into(),
                    ..grams()
                })
            )
            .is_none()
        );
        assert!(admit(Operation::Add, &quantity, &Operand::Dimensionless).is_none());
        assert_eq!(
            admit(Operation::Compare, &quantity, &Operand::Literal)
                .unwrap()
                .1,
            Some(grams())
        );
    }

    proptest::proptest! {
        #[test]
        fn val_money_ratio_preserves_currency_and_refuses_other_dimensions(currency in proptest::prelude::any::<u8>(), unit in proptest::prelude::any::<u8>()) {
            let money = algebra::Operand::<u8, u8>::Money(currency);
            for scalar in [algebra::Operand::Dimensionless, algebra::Operand::Literal] {
                proptest::prop_assert_eq!(admit(Operation::Multiply, &money, &scalar), Some((money.clone(), None)));
                proptest::prop_assert_eq!(admit(Operation::Multiply, &scalar, &money), Some((money.clone(), None)));
                proptest::prop_assert!(admit(Operation::Add, &money, &scalar).is_none());
            }
            let foreign = algebra::Operand::Money(currency.wrapping_add(1));
            proptest::prop_assert!(admit(Operation::Add, &money, &foreign).is_none());
            proptest::prop_assert!(admit(Operation::Multiply, &money, &foreign).is_none());
            proptest::prop_assert!(admit(Operation::Multiply, &money, &algebra::Operand::Quantity(unit)).is_none());
            proptest::prop_assert!(admit(Operation::Multiply, &money, &algebra::Operand::Legacy).is_none());
        }
    }
}
