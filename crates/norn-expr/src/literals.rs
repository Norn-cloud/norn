//! Object literals in a ZEN expression, read from its parsed AST.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::rc::Rc;

use norn_ir::ResourceLimitProfile;
use zen_expression::intellisense::IntelliSense;
use zen_expression::parser::Node;

use crate::{ExprError, validate_evaluation_source};

/// The value of one object-literal field, as far as the expression fixes it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FieldValue {
    /// A string literal.
    String(String),
    /// Anything else: computed, a number, a nested value.
    Other,
}

/// One object literal: its literal-keyed fields, and whether any key is
/// computed (so a field may exist that is not listed).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ObjectLiteral {
    /// Fields whose key is a literal name.
    pub fields: BTreeMap<String, FieldValue>,
    /// Whether some key is not a literal name.
    pub computed_keys: bool,
}

/// Every object literal anywhere in `source`, including those nested in
/// conditionals, arrays, calls, closures and other objects, in AST order.
///
/// # Errors
/// [`ExprError::Compile`] when the source is malformed or exceeds the checked
/// expression source bounds.
pub fn object_literals(source: &str) -> Result<Vec<ObjectLiteral>, ExprError> {
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
            let found = Rc::new(RefCell::new(Vec::new()));
            let sink = Rc::clone(&found);
            ast.walk(move |node| {
                if let Node::Object(entries) = node {
                    sink.borrow_mut().push(literal(entries));
                }
            });
            found.take()
        })
        .ok_or_else(|| ExprError::Compile {
            message: "expression does not parse".to_owned(),
        })
}

/// The records a command-producing expression yields, when its shape fixes
/// them: an array literal of plain object literals with literal keys, or a
/// conditional (or parenthesized expression) whose every branch has that
/// shape. Anything that could construct a record any other way (a variable,
/// a call such as `merge`, a member read, a computed key) is `None`.
///
/// # Errors
/// [`ExprError::Compile`] when the source is malformed or over its bounds.
pub fn command_records(source: &str) -> Result<Option<Vec<ObjectLiteral>>, ExprError> {
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
            let mut records = Vec::new();
            records_of(ast, &mut records).then_some(records)
        })
        .ok_or_else(|| ExprError::Compile {
            message: "expression does not parse".to_owned(),
        })
}

/// Whether `node` has the fixed record shape, collecting its records.
fn records_of(node: &Node<'_>, records: &mut Vec<ObjectLiteral>) -> bool {
    match node {
        Node::Parenthesized(inner) => records_of(inner, records),
        Node::Conditional {
            on_true, on_false, ..
        } => records_of(on_true, records) && records_of(on_false, records),
        Node::Array(items) => items.iter().all(|item| match item {
            Node::Object(entries) => {
                let record = literal(entries);
                let fixed = !record.computed_keys
                    && matches!(record.fields.get("kind"), Some(FieldValue::String(_)));
                records.push(record);
                fixed
            }
            Node::Null
            | Node::Bool(_)
            | Node::Number(_)
            | Node::String(_)
            | Node::TemplateString(_)
            | Node::Pointer
            | Node::Array(_)
            | Node::Assignments { .. }
            | Node::Closure { .. }
            | Node::Root
            | Node::Identifier(_)
            | Node::Parenthesized(_)
            | Node::Member { .. }
            | Node::Slice { .. }
            | Node::Interval { .. }
            | Node::Conditional { .. }
            | Node::Unary { .. }
            | Node::Binary { .. }
            | Node::FunctionCall { .. }
            | Node::MethodCall { .. }
            | Node::Error { .. } => false,
        }),
        Node::Null
        | Node::Bool(_)
        | Node::Number(_)
        | Node::String(_)
        | Node::TemplateString(_)
        | Node::Pointer
        | Node::Object(_)
        | Node::Assignments { .. }
        | Node::Closure { .. }
        | Node::Root
        | Node::Identifier(_)
        | Node::Member { .. }
        | Node::Slice { .. }
        | Node::Interval { .. }
        | Node::Unary { .. }
        | Node::Binary { .. }
        | Node::FunctionCall { .. }
        | Node::MethodCall { .. }
        | Node::Error { .. } => false,
    }
}

fn literal(entries: &[(&Node<'_>, &Node<'_>)]) -> ObjectLiteral {
    let mut object = ObjectLiteral::default();
    for (key, value) in entries {
        let name = match key {
            Node::String(name) | Node::Identifier(name) => (*name).to_owned(),
            Node::Null
            | Node::Bool(_)
            | Node::Number(_)
            | Node::TemplateString(_)
            | Node::Pointer
            | Node::Array(_)
            | Node::Object(_)
            | Node::Assignments { .. }
            | Node::Closure { .. }
            | Node::Root
            | Node::Parenthesized(_)
            | Node::Member { .. }
            | Node::Slice { .. }
            | Node::Interval { .. }
            | Node::Conditional { .. }
            | Node::Unary { .. }
            | Node::Binary { .. }
            | Node::FunctionCall { .. }
            | Node::MethodCall { .. }
            | Node::Error { .. } => {
                object.computed_keys = true;
                continue;
            }
        };
        let value = match value {
            Node::String(text) => FieldValue::String((*text).to_owned()),
            Node::Identifier(_)
            | Node::Null
            | Node::Bool(_)
            | Node::Number(_)
            | Node::TemplateString(_)
            | Node::Pointer
            | Node::Array(_)
            | Node::Object(_)
            | Node::Assignments { .. }
            | Node::Closure { .. }
            | Node::Root
            | Node::Parenthesized(_)
            | Node::Member { .. }
            | Node::Slice { .. }
            | Node::Interval { .. }
            | Node::Conditional { .. }
            | Node::Unary { .. }
            | Node::Binary { .. }
            | Node::FunctionCall { .. }
            | Node::MethodCall { .. }
            | Node::Error { .. } => FieldValue::Other,
        };
        object.fields.insert(name, value);
    }
    object
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    reason = "fixed expressions"
)]
mod tests {
    use super::*;

    #[test]
    fn finds_nested_objects_and_tells_literals_from_computed() {
        let found = object_literals(
            "x > 1 ? [{kind: 'perform', function: 'a.b', functionVersion: '1'}] : [{kind: 'perform', function: input.name, functionVersion: '2'}]",
        )
        .unwrap();
        assert_eq!(found.len(), 2);
        assert_eq!(
            found[0].fields["function"],
            FieldValue::String("a.b".into())
        );
        assert_eq!(found[1].fields["function"], FieldValue::Other);
        assert_eq!(
            found[1].fields["functionVersion"],
            FieldValue::String("2".into())
        );
        assert!(object_literals("{").is_err());
    }

    #[test]
    fn only_fixed_shapes_yield_command_records() {
        let fixed = command_records(
            "x ? [{kind: 'perform', function: 'a', functionVersion: '1', input: {n: merge([{}])}}] : []",
        )
        .unwrap()
        .unwrap();
        assert_eq!(fixed.len(), 1);
        assert_eq!(command_records("[]").unwrap(), Some(vec![]));
        for constructed in [
            "[merge([{kind: 'timer', function: 'uninstalled', functionVersion: '1'}, {['ki' + 'nd']: 'perform'}])]",
            "merge([{kind: 'perform'}], [])",
            "[{['kind']: 'perform', function: 'a', functionVersion: '1'}]",
            "[event.command]",
            "event.commands",
            "[{kind: input.kind, function: 'a', functionVersion: '1'}]",
            "map([1], {kind: 'perform', function: 'a', functionVersion: '1'})",
            "x ? [{kind: 'perform', function: 'a', functionVersion: '1'}] : event.more",
        ] {
            // Refused either way: no fixed records (or the source does not parse).
            assert!(
                !matches!(command_records(constructed), Ok(Some(_))),
                "{constructed}"
            );
        }
    }
}
