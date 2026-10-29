//! Structural extraction of first-level dependencies from a ZEN expression.
//!
//! This is used by lineage compilers to conservatively propagate metadata.
//! It relies on the maintained ZEN AST and deliberately exposes only Norn
//! types; it does not evaluate the expression or infer its value.

use std::collections::{BTreeMap, BTreeSet};

use zen_expression::intellisense::IntelliSense;
use zen_expression::parser::Node;

use norn_ir::ResourceLimitProfile;

use crate::{ExprError, check::validate_evaluation_source};

/// Root fields an expression may read, and whether it uses the whole root.
///
/// A whole-root read (for example a record pass-through) means consumers must
/// union every available root field. `fields` contains only first-level
/// fields, even when an expression traverses deeper members or closure aliases.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ExpressionReads {
    /// First-level root fields read by the expression.
    pub fields: BTreeSet<String>,
    /// The expression reads or passes through the root as a whole.
    pub whole_root: bool,
}

impl ExpressionReads {
    fn extend(&mut self, other: &Self) {
        self.fields.extend(other.fields.iter().cloned());
        self.whole_root |= other.whole_root;
    }
}

/// Extracts first-level fields read from `root` using ZEN's parsed AST.
///
/// Static member reads such as `self[ 'email' ]` and nested alias reads such as
/// `map(self.orders as order, map(order.lines as line, line.sku))` are
/// attributed to their first root field. Dynamic root reads conservatively set
/// `whole_root`, so consumers can union all input-field metadata. Parsing and
/// the same source/AST bounds as expression checking are applied first.
///
/// # Errors
///
/// Returns [`ExprError::Compile`] when the source is malformed or exceeds the
/// checked expression source bounds.
pub fn expression_reads(source: &str, root: &str) -> Result<ExpressionReads, ExprError> {
    validate_evaluation_source(source, &ResourceLimitProfile::norn_limits_v1()).map_err(
        |diagnostics| ExprError::Compile {
            message: diagnostics
                .into_iter()
                .map(|diagnostic| diagnostic.to_string())
                .collect::<Vec<_>>()
                .join("; "),
        },
    )?;

    let mut intellisense = IntelliSense::new().with_strict(true);
    intellisense
        .with_ast(source, false, |ast, _metadata| {
            let mut reads = ExpressionReads::default();
            collect(ast, root, &BTreeMap::new(), &[], &mut reads);
            reads
        })
        .ok_or_else(|| ExprError::Compile {
            message: "expression does not parse".to_owned(),
        })
}

/// Extract complete literal member paths rooted at `root` from the ZEN AST.
/// An empty path represents an unknown or whole-root dependency. Dynamic
/// selectors, aliases, closures and environment pointers fail closed; callers
/// must retain whole-object lineage for paths they cannot resolve exactly.
///
/// # Errors
/// Returns an expression error for malformed or over-budget source.
pub fn expression_read_paths(source: &str, root: &str) -> Result<BTreeSet<Vec<String>>, ExprError> {
    // Apply exactly the source/AST bounds of the ordinary dependency walker.
    expression_reads(source, root)?;
    IntelliSense::new()
        .with_strict(true)
        .with_ast(source, false, |ast, _metadata| {
            let mut paths = BTreeSet::new();
            collect_paths(ast, root, &mut paths);
            paths
        })
        .ok_or_else(|| ExprError::Compile {
            message: "expression does not parse".to_owned(),
        })
}

fn literal_path(node: &Node<'_>) -> Option<Vec<String>> {
    match node {
        Node::Identifier(name) => Some(vec![(*name).to_owned()]),
        Node::Parenthesized(inner) => literal_path(inner),
        Node::Member { node, property } => {
            let Node::String(property) = property else {
                return None;
            };
            let mut path = literal_path(node)?;
            path.push((*property).to_owned());
            Some(path)
        }
        Node::Null
        | Node::Bool(_)
        | Node::Number(_)
        | Node::String(_)
        | Node::TemplateString(_)
        | Node::Pointer
        | Node::Array(_)
        | Node::Object(_)
        | Node::Assignments { .. }
        | Node::Closure { .. }
        | Node::Root
        | Node::Slice { .. }
        | Node::Interval { .. }
        | Node::Conditional { .. }
        | Node::Unary { .. }
        | Node::Binary { .. }
        | Node::FunctionCall { .. }
        | Node::MethodCall { .. }
        | Node::Error { .. } => None,
    }
}

fn collect_paths(node: &Node<'_>, root: &str, output: &mut BTreeSet<Vec<String>>) {
    if let Some(mut path) = literal_path(node) {
        if path.first().is_some_and(|name| name == root) {
            path.remove(0);
            output.insert(path);
        }
        return;
    }
    match node {
        Node::Null | Node::Bool(_) | Node::Number(_) | Node::String(_) | Node::Identifier(_) => {}
        Node::Root
        | Node::Pointer
        | Node::Closure { .. }
        | Node::Assignments { .. }
        | Node::Error { .. } => {
            output.insert(Vec::new());
        }
        Node::Parenthesized(inner) | Node::Unary { node: inner, .. } => {
            collect_paths(inner, root, output);
        }
        Node::Member { node, property } => {
            collect_paths(node, root, output);
            collect_paths(property, root, output);
        }
        Node::FunctionCall { arguments, .. } => {
            for argument in *arguments {
                collect_paths(argument, root, output);
            }
        }
        Node::TemplateString(parts) | Node::Array(parts) => {
            for part in *parts {
                collect_paths(part, root, output);
            }
        }
        Node::Object(entries) => {
            for (key, value) in *entries {
                collect_paths(key, root, output);
                collect_paths(value, root, output);
            }
        }
        Node::Slice { node, from, to } => {
            collect_paths(node, root, output);
            for bound in [from, to].into_iter().flatten() {
                collect_paths(bound, root, output);
            }
        }
        Node::Interval { left, right, .. } | Node::Binary { left, right, .. } => {
            collect_paths(left, root, output);
            collect_paths(right, root, output);
        }
        Node::Conditional {
            condition,
            on_true,
            on_false,
        } => {
            for branch in [condition, on_true, on_false] {
                collect_paths(branch, root, output);
            }
        }
        Node::MethodCall {
            this, arguments, ..
        } => {
            collect_paths(this, root, output);
            for argument in *arguments {
                collect_paths(argument, root, output);
            }
        }
    }
}

#[allow(
    clippy::too_many_lines,
    reason = "the exhaustive ZEN AST walk keeps each node's conservative dependency rule together"
)]
fn collect(
    node: &Node<'_>,
    root: &str,
    aliases: &BTreeMap<String, ExpressionReads>,
    pointers: &[ExpressionReads],
    output: &mut ExpressionReads,
) {
    match node {
        Node::Null
        | Node::Bool(_)
        | Node::Number(_)
        | Node::String(_)
        | Node::Error { node: None, .. } => {}
        // `$root` denotes the whole ZEN environment, not a field whose value
        // can be resolved to the requested input object. Treat it as a
        // pass-through so metadata consumers take the conservative union.
        Node::Root => output.whole_root = true,
        Node::Identifier(name) if *name == root => output.whole_root = true,
        Node::Identifier(name) => {
            if let Some(reads) = aliases.get(*name) {
                output.extend(reads);
            }
        }
        Node::Pointer => {
            if let Some(reads) = pointers.last() {
                output.extend(reads);
            }
        }
        Node::Parenthesized(inner)
        | Node::Unary { node: inner, .. }
        | Node::Error {
            node: Some(inner), ..
        } => collect(inner, root, aliases, pointers, output),
        Node::Member {
            node: target,
            property,
        } => {
            if let Some((reads, dynamic)) = access(target, property, root, aliases, pointers) {
                output.extend(&reads);
                if dynamic {
                    collect(property, root, aliases, pointers, output);
                }
            } else {
                collect(target, root, aliases, pointers, output);
                collect(property, root, aliases, pointers, output);
            }
        }
        Node::FunctionCall { arguments, .. } => {
            collect_call(arguments, root, aliases, pointers, output);
        }
        Node::Closure { body, alias } => {
            let mut nested_aliases = aliases.clone();
            if let Some(alias) = alias {
                nested_aliases.insert((*alias).to_owned(), ExpressionReads::default());
            }
            collect(body, root, &nested_aliases, pointers, output);
        }
        Node::TemplateString(parts) | Node::Array(parts) => {
            for part in *parts {
                collect(part, root, aliases, pointers, output);
            }
        }
        Node::Object(entries) => {
            for (key, value) in *entries {
                collect(key, root, aliases, pointers, output);
                collect(value, root, aliases, pointers, output);
            }
        }
        Node::Assignments {
            list,
            output: result,
        } => {
            for (key, value) in *list {
                collect(key, root, aliases, pointers, output);
                collect(value, root, aliases, pointers, output);
            }
            if let Some(result) = result {
                collect(result, root, aliases, pointers, output);
            }
        }
        Node::Slice { node, from, to } => {
            collect(node, root, aliases, pointers, output);
            if let Some(from) = from {
                collect(from, root, aliases, pointers, output);
            }
            if let Some(to) = to {
                collect(to, root, aliases, pointers, output);
            }
        }
        Node::Interval { left, right, .. } | Node::Binary { left, right, .. } => {
            collect(left, root, aliases, pointers, output);
            collect(right, root, aliases, pointers, output);
        }
        Node::Conditional {
            condition,
            on_true,
            on_false,
        } => {
            collect(condition, root, aliases, pointers, output);
            collect(on_true, root, aliases, pointers, output);
            collect(on_false, root, aliases, pointers, output);
        }
        Node::MethodCall {
            this, arguments, ..
        } => {
            collect(this, root, aliases, pointers, output);
            for argument in *arguments {
                collect(argument, root, aliases, pointers, output);
            }
        }
    }
}

fn collect_call(
    arguments: &[&Node<'_>],
    root: &str,
    aliases: &BTreeMap<String, ExpressionReads>,
    pointers: &[ExpressionReads],
    output: &mut ExpressionReads,
) {
    let Some((list, closures)) = arguments.split_first() else {
        return;
    };
    collect(list, root, aliases, pointers, output);
    let mut list_reads = ExpressionReads::default();
    collect(list, root, aliases, pointers, &mut list_reads);
    for argument in closures {
        if let Node::Closure { body, alias } = argument {
            let mut nested_aliases = aliases.clone();
            if let Some(alias) = alias {
                nested_aliases.insert((*alias).to_owned(), list_reads.clone());
            }
            let mut nested_pointers = pointers.to_vec();
            nested_pointers.push(list_reads.clone());
            collect(body, root, &nested_aliases, &nested_pointers, output);
        } else {
            collect(argument, root, aliases, pointers, output);
        }
    }
}

/// Resolves a member chain to the root-level dependency it represents.
/// `dynamic` marks a nonliteral selector, which is conservatively a whole-root
/// read when no first-level field can be determined.
fn access(
    target: &Node<'_>,
    property: &Node<'_>,
    root: &str,
    aliases: &BTreeMap<String, ExpressionReads>,
    pointers: &[ExpressionReads],
) -> Option<(ExpressionReads, bool)> {
    let mut reads = root_access(target, root, aliases, pointers)?;
    let dynamic = !matches!(property, Node::String(_));
    if reads.fields.is_empty() && !reads.whole_root {
        if let Node::String(field) = property {
            reads.fields.insert((*field).to_owned());
        } else {
            reads.whole_root = true;
        }
    }
    Some((reads, dynamic))
}

fn root_access(
    node: &Node<'_>,
    root: &str,
    aliases: &BTreeMap<String, ExpressionReads>,
    pointers: &[ExpressionReads],
) -> Option<ExpressionReads> {
    match node {
        Node::Root => Some(ExpressionReads {
            fields: BTreeSet::new(),
            whole_root: true,
        }),
        Node::Identifier(name) if *name == root => Some(ExpressionReads {
            fields: BTreeSet::new(),
            whole_root: false,
        }),
        Node::Identifier(name) => aliases.get(*name).cloned(),
        Node::Pointer => pointers.last().cloned(),
        Node::Parenthesized(inner) => root_access(inner, root, aliases, pointers),
        Node::Member {
            node: target,
            property,
        } => {
            let mut reads = root_access(target, root, aliases, pointers)?;
            if reads.fields.is_empty() && !reads.whole_root {
                match property {
                    Node::String(field) => {
                        reads.fields.insert((*field).to_owned());
                    }
                    Node::Null
                    | Node::Bool(_)
                    | Node::Number(_)
                    | Node::TemplateString(_)
                    | Node::Pointer
                    | Node::Array(_)
                    | Node::Object(_)
                    | Node::Assignments { .. }
                    | Node::Identifier(_)
                    | Node::Closure { .. }
                    | Node::Parenthesized(_)
                    | Node::Root
                    | Node::Member { .. }
                    | Node::Slice { .. }
                    | Node::Interval { .. }
                    | Node::Conditional { .. }
                    | Node::Unary { .. }
                    | Node::Binary { .. }
                    | Node::FunctionCall { .. }
                    | Node::MethodCall { .. }
                    | Node::Error { .. } => reads.whole_root = true,
                }
            }
            Some(reads)
        }
        Node::Null
        | Node::Bool(_)
        | Node::Number(_)
        | Node::String(_)
        | Node::TemplateString(_)
        | Node::Array(_)
        | Node::Object(_)
        | Node::Assignments { .. }
        | Node::Closure { .. }
        | Node::Slice { .. }
        | Node::Interval { .. }
        | Node::Conditional { .. }
        | Node::Unary { .. }
        | Node::Binary { .. }
        | Node::FunctionCall { .. }
        | Node::MethodCall { .. }
        | Node::Error { .. } => None,
    }
}

#[cfg(test)]
mod tests {
    use super::{ExpressionReads, expression_read_paths, expression_reads};
    use std::collections::BTreeSet;

    fn fields(values: &[&str]) -> ExpressionReads {
        ExpressionReads {
            fields: values
                .iter()
                .map(|value| (*value).to_owned())
                .collect::<BTreeSet<_>>(),
            whole_root: false,
        }
    }

    #[test]
    fn extracts_spaced_single_and_double_quoted_bracket_reads() {
        let reads = expression_reads("self[ 'email' ] + self[\"phone\"]", "self")
            .expect("literal member reads should parse");
        assert_eq!(reads, fields(&["email", "phone"]));
    }

    #[test]
    fn tracks_nested_closure_aliases_to_their_root_field() {
        let reads = expression_reads(
            "map(self.orders as order, map(order.lines as line, line.sku))",
            "self",
        )
        .expect("nested closure aliases should parse");
        assert_eq!(reads, fields(&["orders"]));
    }

    #[test]
    fn marks_a_whole_root_pass_through() {
        let reads = expression_reads("self", "self").expect("root pass-through should parse");
        assert!(reads.whole_root);
        assert!(reads.fields.is_empty());
    }

    #[test]
    fn marks_the_zen_environment_root_as_a_whole_root_read() {
        let reads = expression_reads("$root", "input").expect("environment root should parse");
        assert!(reads.whole_root);
        let reads = expression_reads("$root.customer", "input")
            .expect("environment-root member should parse");
        assert!(reads.whole_root);
    }

    #[test]
    fn dynamic_selector_fails_closed_to_whole_root() {
        let reads = expression_reads("self[key]", "self").expect("selector should parse");
        assert!(reads.whole_root);
    }

    #[test]
    fn precise_paths_preserve_literal_operands_and_flag_unknown_access() {
        let paths = expression_read_paths(
            "slice.invoice.status == 'Overdue' and slice['invoice'][ 'amount' ] > input.minimum",
            "slice",
        )
        .expect("static operands");
        assert_eq!(
            paths,
            BTreeSet::from([
                vec!["invoice".to_owned(), "amount".to_owned()],
                vec!["invoice".to_owned(), "status".to_owned()],
            ])
        );
        for source in [
            "slice",
            "$root",
            "slice[input.alias].status",
            "map([slice.invoice] as row, row.status)",
        ] {
            assert!(
                expression_read_paths(source, "slice")
                    .expect("bounded expression")
                    .contains(&Vec::new()),
                "{source}"
            );
        }
        assert_eq!(
            expression_read_paths("slice.invoice[input.property]", "slice")
                .expect("dynamic property"),
            BTreeSet::from([vec!["invoice".to_owned()]])
        );
        assert!(
            expression_read_paths("input.status == 'Paid'", "slice")
                .expect("input only")
                .is_empty()
        );
    }
}
