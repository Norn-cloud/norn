//! Narrow structural equalities for consumers of admitted expressions.
use std::collections::BTreeSet;

use norn_ir::{PropertyId, ResourceLimitProfile};
use zen_expression::intellisense::IntelliSense;
use zen_expression::lexer::{ArithmeticOperator, ComparisonOperator, Operator};
use zen_expression::parser::Node;

use crate::{ExprError, reads::literal_path, validate_evaluation_source};

/// An equality between one direct `self` property and distinct summed properties.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StructuralSumEquality {
    /// The equality's left-hand property.
    pub total: PropertyId,
    /// Right-hand terms in AST order, without coefficients or repetition.
    pub terms: Vec<PropertyId>,
}

/// EXPR-STRUCTURAL-SUM-1: recognize only `self.a == self.b + self.c + ...`.
/// Parentheses and literal member syntax have their ordinary AST meaning.
/// Two to 64 distinct terms are required; the total cannot appear among them.
/// This extracts syntax only. Callers retain type, unit, scope and law admission.
///
/// # Errors
/// Returns a compile error for malformed or structurally over-budget source.
pub fn structural_sum_equality(source: &str) -> Result<Option<StructuralSumEquality>, ExprError> {
    validate_evaluation_source(source, &ResourceLimitProfile::norn_limits_v1()).map_err(
        |diagnostics| ExprError::Compile {
            message: diagnostics
                .into_iter()
                .map(|diagnostic| diagnostic.to_string())
                .collect::<Vec<_>>()
                .join("; "),
        },
    )?;
    IntelliSense::new()
        .with_strict(true)
        .with_ast(source, false, |ast, _metadata| {
            let Node::Binary {
                left,
                operator: Operator::Comparison(ComparisonOperator::Equal),
                right,
            } = unwrapped(ast)
            else {
                return None;
            };
            let total = property(unwrapped(left))?;
            let mut terms = Vec::new();
            collect(right, &mut terms)?;
            (terms.len() >= 2
                && !terms.contains(&total)
                && terms.iter().collect::<BTreeSet<_>>().len() == terms.len())
            .then_some(StructuralSumEquality { total, terms })
        })
        .ok_or_else(|| ExprError::Compile {
            message: "expression does not parse".to_owned(),
        })
}

fn unwrapped<'a>(mut node: &'a Node<'a>) -> &'a Node<'a> {
    while let Node::Parenthesized(inner) = node {
        node = inner;
    }
    node
}

fn property(node: &Node<'_>) -> Option<PropertyId> {
    let path = literal_path(node)?;
    let [root, field] = path.as_slice() else {
        return None;
    };
    (root == "self").then(|| PropertyId(field.clone()))
}

fn collect(node: &Node<'_>, terms: &mut Vec<PropertyId>) -> Option<()> {
    let node = unwrapped(node);
    if let Node::Binary {
        left,
        operator: Operator::Arithmetic(ArithmeticOperator::Add),
        right,
    } = node
    {
        collect(left, terms)?;
        collect(right, terms)
    } else if terms.len() < 64 {
        terms.push(property(node)?);
        Some(())
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn structural_sum_accepts_parentheses_and_literal_members() {
        let expected = Some(StructuralSumEquality {
            total: PropertyId("input".into()),
            terms: vec![PropertyId("output".into()), PropertyId("loss".into())],
        });
        for source in [
            "self.input == self.output + self.loss",
            "(self['input'] == ((self.output) + self['loss']))",
            // The total side unwraps its parentheses too.
            "((self.input)) == self.output + self.loss",
        ] {
            assert_eq!(structural_sum_equality(source), Ok(expected.clone()));
        }
    }

    #[test]
    fn structural_sum_refuses_other_operations_and_reads() {
        for source in [
            "self.a == self.b",
            "self.a == self.b + self.b",
            "self.a == self.a + self.b",
            "self.a == self.b * self.c",
            "self.a == 2 * self.b + self.c",
            "self.a == self.b - self.c",
            "self.a == self.b + self.c + 0",
            "self.a >= self.b + self.c",
            "self.a == self.b + self.c or true",
            "true ? self.a == self.b + self.c : false",
            "self.a == input.b + self.c",
            "self.a == self.link.b + self.c",
            "self.a == self[input.key] + self.c",
            "self.a == self.b + abs(self.c)",
        ] {
            assert_eq!(structural_sum_equality(source), Ok(None), "{source}");
        }
    }

    #[test]
    fn structural_sum_bounds_and_malformed_source_fail_closed() {
        for count in [2, 18, 64, 65] {
            let terms = (0..count)
                .map(|i| format!("self.output{i}"))
                .collect::<Vec<_>>()
                .join(" + ");
            let result = structural_sum_equality(&format!("self.input == {terms}"));
            assert_eq!(
                result.ok().flatten().map(|sum| sum.terms.len()),
                (count <= 64).then_some(count)
            );
        }
        assert!(structural_sum_equality("self.a == (").is_err());
        assert!(structural_sum_equality(&"(".repeat(100_000)).is_err());
    }

    #[test]
    fn structural_sum_keyword_properties_use_literal_members() {
        assert_eq!(
            structural_sum_equality("self['true'] == self['false'] + self['null']"),
            Ok(Some(StructuralSumEquality {
                total: PropertyId("true".into()),
                terms: vec![PropertyId("false".into()), PropertyId("null".into())],
            }))
        );
    }

    proptest::proptest! {
        #[test]
        fn structural_sum_preserves_generated_terms(a in "[a-z]{1,12}", b in "[a-z]{1,12}", c in "[a-z]{1,12}", wrappers in 0usize..8) {
            proptest::prop_assume!(a != b && a != c && b != c);
            let source = format!("{}self['{a}'] == (self['{b}'] + self['{c}']){}", "(".repeat(wrappers), ")".repeat(wrappers));
            proptest::prop_assert_eq!(structural_sum_equality(&source).ok().flatten(),
                Some(StructuralSumEquality { total: PropertyId(a), terms: vec![PropertyId(b), PropertyId(c)] }));
        }
    }
}
