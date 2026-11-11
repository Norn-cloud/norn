//! VAL-ROUND: lower declared result sites into the one shared rescale opcode.
use super::{AstMetadata, Node, Type, ZenTypeEnv, address};
use std::fmt::Write as _;

pub(super) fn lower(
    root: &Node<'_>,
    expected: &Type,
    env: &ZenTypeEnv,
    metadata: &AstMetadata,
    source: &str,
) -> Result<String, String> {
    let mut edits = Vec::new();
    visit(root, expected, env, metadata, &mut edits)?;
    edits.sort_by_key(|(start, end, _, _, _)| (*start, *end));
    let mut output = String::new();
    let mut cursor = 0;
    for (start, end, scale, mode, nullable) in edits {
        let start = usize::try_from(start)
            .map_err(|error| format!("exact site offset overflow: {error}"))?;
        let end =
            usize::try_from(end).map_err(|error| format!("exact site offset overflow: {error}"))?;
        if start < cursor || end > source.len() || start >= end {
            return Err("exact site has no faithful source span".into());
        }
        output.push_str(source.get(cursor..start).ok_or("invalid site span")?);
        if nullable {
            output.push_str("((");
            output.push_str(source.get(start..end).ok_or("invalid site span")?);
            output.push_str(") == null ? null : ");
        }
        output.push_str("round((");
        output.push_str(source.get(start..end).ok_or("invalid site span")?);
        write!(output, "), {scale}, '{mode}')").map_err(|error| error.to_string())?;
        if nullable {
            output.push(')');
        }
        cursor = end;
    }
    output.push_str(source.get(cursor..).ok_or("invalid site span")?);
    Ok(output)
}
fn visit(
    node: &Node<'_>,
    expected: &Type,
    env: &ZenTypeEnv,
    metadata: &AstMetadata,
    edits: &mut Vec<(u32, u32, u64, &'static str, bool)>,
) -> Result<(), String> {
    if let Type::Optional(inner) = expected {
        match node {
            Node::Null => return Ok(()),
            Node::Parenthesized(node) => return visit(node, expected, env, metadata, edits),
            Node::Conditional {
                on_true, on_false, ..
            } => {
                visit(on_true, expected, env, metadata, edits)?;
                return visit(on_false, expected, env, metadata, edits);
            }
            Node::Bool(_)
            | Node::Number(_)
            | Node::String(_)
            | Node::TemplateString(_)
            | Node::Pointer
            | Node::Array(_)
            | Node::Object(_)
            | Node::Assignments { .. }
            | Node::Identifier(_)
            | Node::Closure { .. }
            | Node::Root
            | Node::Member { .. }
            | Node::Slice { .. }
            | Node::Interval { .. }
            | Node::Unary { .. }
            | Node::Binary { .. }
            | Node::FunctionCall { .. }
            | Node::MethodCall { .. }
            | Node::Error { .. } => {}
        }
        if matches!(inner.as_ref(), Type::Decimal { .. }) {
            return add_site(node, inner, metadata, edits, true);
        }
        return visit(node, inner, env, metadata, edits);
    }
    if matches!(node, Node::Null) {
        return Ok(());
    }
    // A conditional's parser span starts at `?`, so each branch is its own site.
    if matches!(expected, Type::Decimal { .. }) && !matches!(node, Node::Conditional { .. }) {
        return add_site(node, expected, metadata, edits, false);
    }
    match node {
        Node::Parenthesized(inner) => visit(inner, expected, env, metadata, edits),
        Node::Conditional {
            on_true, on_false, ..
        } => {
            visit(on_true, expected, env, metadata, edits)?;
            visit(on_false, expected, env, metadata, edits)
        }
        Node::Object(fields) => {
            let Type::Record(name) = expected else {
                return Ok(());
            };
            let record = env.record(name).ok_or("unknown destination record")?;
            for (key, value) in *fields {
                let Node::String(key) = key else {
                    return Err("exact destination needs literal fields".into());
                };
                if let Some(field) = record.fields.get(*key) {
                    visit(value, &field.ty, env, metadata, edits)?;
                }
            }
            Ok(())
        }
        Node::Array(items) => {
            if let Type::List(item) = expected {
                for value in *items {
                    visit(value, item, env, metadata, edits)?;
                }
            }
            Ok(())
        }
        Node::Null
        | Node::Bool(_)
        | Node::Number(_)
        | Node::String(_)
        | Node::TemplateString(_)
        | Node::Pointer
        | Node::Assignments { .. }
        | Node::Identifier(_)
        | Node::Closure { .. }
        | Node::Root
        | Node::Member { .. }
        | Node::Slice { .. }
        | Node::Interval { .. }
        | Node::Unary { .. }
        | Node::Binary { .. }
        | Node::FunctionCall { .. }
        | Node::MethodCall { .. }
        | Node::Error { .. } => Ok(()),
    }
}

fn add_site(
    node: &Node<'_>,
    expected: &Type,
    metadata: &AstMetadata,
    edits: &mut Vec<(u32, u32, u64, &'static str, bool)>,
    nullable: bool,
) -> Result<(), String> {
    let Type::Decimal {
        scale, rounding, ..
    } = expected
    else {
        return Err("rounding site requires Decimal".into());
    };
    if *scale > 18 {
        return Err("exact destination scale exceeds 18".into());
    }
    let (start, end) = metadata
        .get(&address(node))
        .ok_or("missing exact site span")?
        .span;
    edits.push((
        start,
        end,
        *scale,
        rounding.canonical_field().unwrap_or("refuse"),
        nullable,
    ));
    Ok(())
}
