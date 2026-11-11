//! Exact aggregate folds over the members of a predicate read (#1105).
//!
//! The kernel pins the matched set with a predicate read expectation; this
//! fold only computes over the values it was given, so it is pure and the
//! commit's read-set recheck keeps the result valid at commit.

use norn_ir::runtime::Decimal;
use norn_ir::{AggregateFunction, RuntimeObject, RuntimeValue};

/// Why an aggregate could not be computed exactly.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum AggregateError {
    /// A member has no value, or a non-numeric one, for the summed property.
    #[error("member {index} has no numeric {property:?}")]
    NotNumeric {
        /// The member's position in the read.
        index: usize,
        /// The summed property.
        property: String,
    },
    /// The sum cannot be held exactly.
    #[error("the sum of {property:?} cannot be held without rounding")]
    Overflow {
        /// The summed property.
        property: String,
    },
}

/// Fold `function` over `members`. A sum is exact and never rounded; an
/// empty sum is zero. A count is the number of members.
///
/// # Errors
///
/// [`AggregateError`] when a member's summed value is missing or not a
/// number, or the exact sum overflows.
pub fn fold_aggregate<'a, I>(
    function: &AggregateFunction,
    members: I,
) -> Result<RuntimeValue, AggregateError>
where
    I: IntoIterator<Item = &'a RuntimeObject>,
{
    match function {
        AggregateFunction::Count {} => {
            let count = u64::try_from(members.into_iter().count()).unwrap_or(u64::MAX);
            Ok(RuntimeValue::Number(Decimal::from(count)))
        }
        AggregateFunction::Sum { property } => {
            let property = property.as_str();
            let mut total = Decimal::ZERO;
            for (index, member) in members.into_iter().enumerate() {
                let Some(RuntimeValue::Number(value)) = member.get(property) else {
                    return Err(AggregateError::NotNumeric {
                        index,
                        property: property.to_owned(),
                    });
                };
                total = total
                    .checked_add(*value)
                    .ok_or_else(|| AggregateError::Overflow {
                        property: property.to_owned(),
                    })?;
            }
            Ok(RuntimeValue::Number(total))
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, reason = "fold assertions")]
    use super::*;

    fn objects(values: &[serde_json::Value]) -> Vec<RuntimeObject> {
        values
            .iter()
            .map(|value| serde_json::from_value(value.clone()).expect("object"))
            .collect()
    }

    fn sum(property: &str) -> AggregateFunction {
        serde_json::from_value(serde_json::json!({"kind": "sum", "property": property}))
            .expect("sum")
    }

    #[test]
    fn sums_are_exact_and_counts_count() {
        let members = objects(&[
            serde_json::json!({"amount": 0.1}),
            serde_json::json!({"amount": 0.2}),
            serde_json::json!({"amount": 10.25}),
        ]);
        let total = fold_aggregate(&sum("amount"), &members).expect("sum");
        assert_eq!(
            total,
            RuntimeValue::Number(Decimal::parse("10.55").expect("decimal"))
        );
        let count = fold_aggregate(&AggregateFunction::Count {}, &members).expect("count");
        assert_eq!(count, RuntimeValue::Number(Decimal::from(3_u64)));
        let empty = fold_aggregate(&sum("amount"), &Vec::<RuntimeObject>::new()).expect("empty");
        assert_eq!(empty, RuntimeValue::Number(Decimal::ZERO));
    }

    #[test]
    fn a_missing_or_non_numeric_value_refuses() {
        for member in [serde_json::json!({}), serde_json::json!({"amount": "1"})] {
            let members = objects(&[member]);
            assert!(matches!(
                fold_aggregate(&sum("amount"), &members),
                Err(AggregateError::NotNumeric { index: 0, .. })
            ));
        }
    }
}
