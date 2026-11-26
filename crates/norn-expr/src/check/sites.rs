//! VAL-ROUND: lower declared result sites into the one shared rescale opcode
//! and enforce exact-profile function/operator admission.
use std::fmt::Write as _;

use zen_expression::functions::{FunctionKind, InternalFunction};
use zen_expression::lexer::{ArithmeticOperator, ComparisonOperator, Operator};

use super::{AstMetadata, Diagnostic, Node, Type, ZenTypeEnv, address, bounds};

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

/// Builtins whose semantics are exact and non-rounding (no implicit numeric
/// precision loss), so they may appear alongside `Decimal` operands in exact
/// expressions without requiring an enclosing `round` site.
const fn is_exact_supported_builtin(function: InternalFunction) -> bool {
    match function {
        InternalFunction::Len
        | InternalFunction::CalendarDate
        | InternalFunction::Timestamp
        | InternalFunction::DaysBetween
        | InternalFunction::AddDays
        | InternalFunction::SecondsBetween
        | InternalFunction::AddSeconds
        | InternalFunction::IntervalsOverlap
        | InternalFunction::IntervalGapSeconds
        | InternalFunction::IntervalIntersectionSeconds
        | InternalFunction::ResolveLocalTimestamp
        | InternalFunction::Lower
        | InternalFunction::Upper
        | InternalFunction::Trim
        | InternalFunction::Contains
        | InternalFunction::StartsWith
        | InternalFunction::EndsWith
        | InternalFunction::Split
        | InternalFunction::Tokens
        | InternalFunction::Take
        | InternalFunction::Join => true,
        InternalFunction::Flatten
        | InternalFunction::Merge
        | InternalFunction::MergeDeep
        | InternalFunction::Matches
        | InternalFunction::Extract
        | InternalFunction::FuzzyMatch
        | InternalFunction::Abs
        | InternalFunction::Sum
        | InternalFunction::Avg
        | InternalFunction::Min
        | InternalFunction::Max
        | InternalFunction::Median
        | InternalFunction::Mode
        | InternalFunction::Floor
        | InternalFunction::Ceil
        | InternalFunction::Round
        | InternalFunction::Trunc
        | InternalFunction::IsNumeric
        | InternalFunction::String
        | InternalFunction::Number
        | InternalFunction::Bool
        | InternalFunction::Type
        | InternalFunction::Keys
        | InternalFunction::Values
        | InternalFunction::Date => false,
    }
}

/// VAL-ROUND: no information-losing operation enters the exact runtime implicitly.
pub(super) fn exact_admission<'n>(
    root: &'n Node<'n>,
    metadata: &AstMetadata,
    destination: bool,
) -> Vec<Diagnostic> {
    let mut pending = vec![(root, destination)];
    let mut children = Vec::new();
    let mut out = Vec::new();
    while let Some((node, destination)) = pending.pop() {
        let round = matches!(
            node,
            Node::FunctionCall {
                kind: FunctionKind::Internal(InternalFunction::Round),
                arguments
            } if arguments.len() >= 2
        );
        let site = destination
            || matches!(
                node,
                Node::FunctionCall {
                    kind: FunctionKind::Internal(InternalFunction::Round),
                    arguments
                } if arguments.len() == 3
            );
        let supported_builtin = matches!(
            node,
            Node::FunctionCall {
                kind: FunctionKind::Internal(function),
                ..
            } if is_exact_supported_builtin(*function)
        );
        let reason = if matches!(
            node,
            Node::Binary {
                operator: Operator::Arithmetic(ArithmeticOperator::Divide),
                ..
            }
        ) && !destination
        {
            Some("division requires a declared Decimal destination or explicit round site")
        } else if matches!(
            node,
            Node::Binary {
                operator: Operator::Comparison(ComparisonOperator::In | ComparisonOperator::NotIn),
                ..
            }
        ) {
            Some("membership is outside the exact arithmetic profile")
        } else if matches!(node, Node::FunctionCall { .. })
            && !(supported_builtin || (round && site))
        {
            Some("function requires an explicit supported exact site")
        } else if matches!(
            node,
            Node::FunctionCall {
                kind: FunctionKind::Internal(InternalFunction::Round),
                arguments
            } if arguments.get(1).is_some_and(|value| matches!(
                value,
                Node::Number(scale) if scale.to_string().parse::<u32>().is_ok_and(|scale| scale > 18)
            ))
        ) {
            Some("round exceeds the exact arithmetic scale bound")
        } else if !destination && matches!(node, Node::Number(value) if value.scale() > 18) {
            Some("numeric literal exceeds the exact arithmetic scale bound")
        } else {
            None
        };
        if let Some(reason) = reason {
            out.push(Diagnostic {
                code: "core.values.inexact",
                message: reason.to_owned(),
                span: metadata.get(&address(node)).map_or((0, 0), |m| m.span),
            });
        }
        // Only an explicit `round` call (or transparent expression wrapper) propagates
        // a rounding destination to its value operands; a conditional's condition and
        // arguments to non-rounding builtins must supply their own exact site for
        // division or over-scale literals.
        if let Node::Conditional {
            condition,
            on_true,
            on_false,
        } = node
        {
            pending.push((condition, false));
            pending.push((on_true, site));
            pending.push((on_false, site));
        } else {
            let child_site = if matches!(node, Node::FunctionCall { .. }) && !round {
                false
            } else {
                site
            };
            children.clear();
            bounds::children(node, &mut children);
            pending.extend(children.drain(..).map(|child| (child, child_site)));
        }
    }
    out
}
