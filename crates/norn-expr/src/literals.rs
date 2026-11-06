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

fn literal(entries: &[(&Node<'_>, &Node<'_>)]) -> ObjectLiteral {
    let mut object = ObjectLiteral::default();
    for (key, value) in entries {
        let name = match key {
            Node::String(name) | Node::Identifier(name) => (*name).to_owned(),
            _ => {
                object.computed_keys = true;
                continue;
            }
        };
        let value = match value {
            Node::String(text) => FieldValue::String((*text).to_owned()),
            _ => FieldValue::Other,
        };
        object.fields.insert(name, value);
    }
    object
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing, reason = "fixed expressions")]
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
}
