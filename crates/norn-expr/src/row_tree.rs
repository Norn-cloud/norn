//! VIEW-ENGINE-1: the bounded row-expression subset another engine can lower.
//!
//! A transform's admitted ZEN expressions run in the exact interpreter as
//! written. An engine that lowers a plan (DuckDB) needs their structure, but
//! no `zen-*` type may cross this crate's boundary, so this module projects
//! the parsed AST onto a small Norn-owned tree: literals, `root.<column>`
//! reads, `+ - *`, comparisons, `and`/`or`/`not`, `??` and `?:`. Anything
//! else (division, functions, closures, intervals, templates, nested
//! members) projects to `None`, and the engine refuses rather than guess.
//! The projection is syntax only: admission still owns types and bounds.

use norn_ir::ResourceLimitProfile;
use zen_expression::intellisense::IntelliSense;
use zen_expression::lexer::{ArithmeticOperator, ComparisonOperator, LogicalOperator, Operator};
use zen_expression::parser::Node;

use crate::{ExprError, Number, validate_evaluation_source};

/// An arithmetic operator another engine computes exactly.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RowArithmetic {
    /// `+`.
    Add,
    /// `-`.
    Subtract,
    /// `*`.
    Multiply,
}

/// A comparison operator.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RowComparison {
    /// `==`.
    Equal,
    /// `!=`.
    NotEqual,
    /// `<`.
    Less,
    /// `<=`.
    LessOrEqual,
    /// `>`.
    Greater,
    /// `>=`.
    GreaterOrEqual,
}

/// One node of the lowerable row-expression subset.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RowExpr {
    /// `null`.
    Null,
    /// A Boolean literal.
    Bool(bool),
    /// An exact number literal.
    Number(Number),
    /// A string literal.
    Text(String),
    /// `root.<column>`.
    Column(String),
    /// `not x`.
    Not(Box<RowExpr>),
    /// `-x`.
    Negate(Box<RowExpr>),
    /// `x op y`.
    Arithmetic(RowArithmetic, Box<RowExpr>, Box<RowExpr>),
    /// `x op y`.
    Compare(RowComparison, Box<RowExpr>, Box<RowExpr>),
    /// `x and y`.
    And(Box<RowExpr>, Box<RowExpr>),
    /// `x or y`.
    Or(Box<RowExpr>, Box<RowExpr>),
    /// `x ?? y`.
    Coalesce(Box<RowExpr>, Box<RowExpr>),
    /// `c ? x : y`.
    Conditional(Box<RowExpr>, Box<RowExpr>, Box<RowExpr>),
}

/// Project `source` onto [`RowExpr`] with row reads rooted at `root`;
/// `Ok(None)` when it uses anything outside the subset.
///
/// # Errors
/// A compile error for malformed or structurally over-budget source.
pub fn row_expression(source: &str, root: &str) -> Result<Option<RowExpr>, ExprError> {
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
        .with_ast(source, false, |ast, _metadata| Some(project(ast, root)))
        .ok_or_else(|| ExprError::Compile {
            message: "expression does not parse".to_owned(),
        })
}

fn boxed(node: &Node<'_>, root: &str) -> Option<Box<RowExpr>> {
    project(node, root).map(Box::new)
}

fn project(node: &Node<'_>, root: &str) -> Option<RowExpr> {
    Some(match node {
        Node::Null => RowExpr::Null,
        Node::Bool(flag) => RowExpr::Bool(*flag),
        Node::Number(number) => RowExpr::Number(number.normalize().to_string().parse().ok()?),
        Node::String(text) => RowExpr::Text((*text).to_owned()),
        Node::Parenthesized(inner) => project(inner, root)?,
        Node::Member { .. } => {
            let path = crate::reads::literal_path(node)?;
            let [first, column] = path.as_slice() else {
                return None;
            };
            if first != root {
                return None;
            }
            RowExpr::Column(column.clone())
        }
        Node::Unary { node, operator } => match operator {
            Operator::Logical(LogicalOperator::Not) => RowExpr::Not(boxed(node, root)?),
            Operator::Arithmetic(ArithmeticOperator::Subtract) => {
                RowExpr::Negate(boxed(node, root)?)
            }
            Operator::Arithmetic(_)
            | Operator::Logical(_)
            | Operator::Comparison(_)
            | Operator::Range
            | Operator::Comma
            | Operator::Slice
            | Operator::Dot
            | Operator::QuestionMark
            | Operator::Assign
            | Operator::Semi => return None,
        },
        Node::Binary {
            left,
            operator,
            right,
        } => binary(left, *operator, right, root)?,
        Node::Conditional {
            condition,
            on_true,
            on_false,
        } => RowExpr::Conditional(
            boxed(condition, root)?,
            boxed(on_true, root)?,
            boxed(on_false, root)?,
        ),
        Node::TemplateString(_)
        | Node::Pointer
        | Node::Array(_)
        | Node::Object(_)
        | Node::Assignments { .. }
        | Node::Identifier(_)
        | Node::Closure { .. }
        | Node::Root
        | Node::Slice { .. }
        | Node::Interval { .. }
        | Node::FunctionCall { .. }
        | Node::MethodCall { .. }
        | Node::Error { .. } => return None,
    })
}

fn binary(left: &Node<'_>, operator: Operator, right: &Node<'_>, root: &str) -> Option<RowExpr> {
    let (left, right) = (boxed(left, root)?, boxed(right, root)?);
    Some(match operator {
        Operator::Arithmetic(ArithmeticOperator::Add) => {
            RowExpr::Arithmetic(RowArithmetic::Add, left, right)
        }
        Operator::Arithmetic(ArithmeticOperator::Subtract) => {
            RowExpr::Arithmetic(RowArithmetic::Subtract, left, right)
        }
        Operator::Arithmetic(ArithmeticOperator::Multiply) => {
            RowExpr::Arithmetic(RowArithmetic::Multiply, left, right)
        }
        Operator::Comparison(comparison) => {
            let comparison = match comparison {
                ComparisonOperator::Equal => RowComparison::Equal,
                ComparisonOperator::NotEqual => RowComparison::NotEqual,
                ComparisonOperator::LessThan => RowComparison::Less,
                ComparisonOperator::LessThanOrEqual => RowComparison::LessOrEqual,
                ComparisonOperator::GreaterThan => RowComparison::Greater,
                ComparisonOperator::GreaterThanOrEqual => RowComparison::GreaterOrEqual,
                ComparisonOperator::In | ComparisonOperator::NotIn => return None,
            };
            RowExpr::Compare(comparison, left, right)
        }
        Operator::Logical(LogicalOperator::And) => RowExpr::And(left, right),
        Operator::Logical(LogicalOperator::Or) => RowExpr::Or(left, right),
        Operator::Logical(LogicalOperator::NullishCoalescing) => RowExpr::Coalesce(left, right),
        Operator::Arithmetic(
            ArithmeticOperator::Divide | ArithmeticOperator::Modulus | ArithmeticOperator::Power,
        )
        | Operator::Logical(LogicalOperator::Not)
        | Operator::Range
        | Operator::Comma
        | Operator::Slice
        | Operator::Dot
        | Operator::QuestionMark
        | Operator::Assign
        | Operator::Semi => return None,
    })
}
