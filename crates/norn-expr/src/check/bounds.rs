//! Structural bounds of `norn-expr/4` (T1-EXPR-4, #2522).
//!
//! Checked before any recursive walk: [`lexical`] on the source text before
//! it is parsed, and [`structure`] on the AST with an explicit stack. Once
//! both pass, the AST is at most [`StructuralLimits::max_depth`] deep, so the
//! recursive type checker, zen's strict analysis, and the cost estimate each
//! recurse a bounded number of frames (#2852 risk: a deeply nested
//! expression must not overflow the checker's stack).

use zen_expression::functions::{FunctionKind, InternalFunction};
use zen_expression::parser::Node;

use super::{CHECKER_PROFILE, Diagnostic, codes};

/// The structural bounds of the `norn-expr/4` checker profile.
///
/// These are properties of the expression language profile, not of a cell's
/// resource budget, so they are pinned by the profile id rather than read from
/// the `norn.limits` profile. The closure nesting bound is not here: it is the
/// limit profile's slice relation depth (see [`check_expression_with`]).
///
/// [`check_expression_with`]: super::check_expression_with
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub struct StructuralLimits {
    /// Maximum source length in bytes, checked before parsing.
    pub max_source_bytes: u32,
    /// Maximum number of AST nodes.
    pub max_nodes: u32,
    /// Maximum AST nesting depth (and bracket nesting depth of the source).
    pub max_depth: u32,
    /// Maximum number of recursion-opening tokens in the source, counted
    /// before parsing: an upper bound on zen's parser and compiler recursion.
    pub max_parse_nesting: u32,
    /// Maximum length of one string literal, in bytes.
    pub max_string_literal_bytes: u32,
    /// Maximum number of elements of one list literal or entries of one
    /// record literal.
    pub max_literal_items: u32,
}

impl StructuralLimits {
    /// The bounds of `norn-expr/4`.
    pub const NORN_EXPR_4: Self = Self {
        max_source_bytes: 16_384,
        max_nodes: 4096,
        max_depth: 128,
        max_parse_nesting: 512,
        max_string_literal_bytes: 4096,
        max_literal_items: 1024,
    };
}

fn bound(code: &'static str, span: (u32, u32), message: &str) -> Diagnostic {
    Diagnostic {
        code,
        message: format!("{message} ({CHECKER_PROFILE})"),
        span,
    }
}

fn as_usize(n: u32) -> usize {
    usize::try_from(n).unwrap_or(usize::MAX)
}

fn span_of(source: &str) -> (u32, u32) {
    (0, u32::try_from(source.len()).unwrap_or(u32::MAX))
}

fn bounded_integer_literal(node: Option<&&Node<'_>>, max: u16) -> bool {
    matches!(
        node,
        Some(Node::Number(value)) if value.to_string().parse::<u16>().is_ok_and(|value| value <= max)
    )
}

fn bounded_string_call(
    node: &Node<'_>,
    span: impl Fn(&Node<'_>) -> (u32, u32),
) -> Option<Diagnostic> {
    let Node::FunctionCall {
        kind: FunctionKind::Internal(function),
        arguments,
    } = node
    else {
        return None;
    };
    let (name, valid, detail) = match function {
        InternalFunction::Tokens => (
            "tokens",
            arguments.len() == 4
                && matches!(arguments.get(1), Some(Node::String("alnum")))
                && bounded_integer_literal(arguments.get(2), 256)
                && bounded_integer_literal(arguments.get(3), 256),
            "requires (text, 'alnum', integer maxCount 0..=256, integer maxLength 0..=256) literals",
        ),
        InternalFunction::Take => (
            "take",
            arguments.len() == 2 && bounded_integer_literal(arguments.get(1), 16_384),
            "requires a literal integer maxScalars from 0 to 16384",
        ),
        InternalFunction::Join => (
            "join",
            arguments.len() == 2
                && matches!(arguments.get(1), Some(Node::String(separator)) if separator.len() <= 256),
            "requires a literal separator no longer than 256 bytes",
        ),
        InternalFunction::Len
        | InternalFunction::Contains
        | InternalFunction::Flatten
        | InternalFunction::Merge
        | InternalFunction::MergeDeep
        | InternalFunction::Upper
        | InternalFunction::Lower
        | InternalFunction::Trim
        | InternalFunction::StartsWith
        | InternalFunction::EndsWith
        | InternalFunction::Matches
        | InternalFunction::Extract
        | InternalFunction::FuzzyMatch
        | InternalFunction::Split
        | InternalFunction::Abs
        | InternalFunction::Sum
        | InternalFunction::Avg
        | InternalFunction::Min
        | InternalFunction::Max
        | InternalFunction::Rand
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
        | InternalFunction::Date
        | InternalFunction::CalendarDate
        | InternalFunction::Timestamp
        | InternalFunction::DaysBetween
        | InternalFunction::AddDays
        | InternalFunction::SecondsBetween
        | InternalFunction::AddSeconds => return None,
    };
    (!valid).then(|| Diagnostic {
        code: codes::UNSUPPORTED_CONSTRUCT,
        message: format!("`{name}` {detail} ({CHECKER_PROFILE})"),
        span: span(node),
    })
}

/// The source-level bounds, checked before zen parses the source (its parser
/// and compiler recurse once per nesting level, before any AST bound can run):
///
/// - its length;
/// - its bracket nesting;
/// - the number of characters and keywords that can open a recursion level:
///   every bracket, parenthesis, and brace (closing ones too: zen's interval
///   parser probes any bracket), member dot, operator character
///   (`+ - * / % ^ ! < > = & | ? :`), and `and`/`or`/`not`/`in` (and, in a
///   unary test, every comma: zen joins the top-level parts left-deep). Every
///   AST level is opened by one of these, so their total is an upper bound on
///   zen's recursion depth, whatever the construct (`a[0][0]...`, `a.a...`,
///   `- - x`, `f(f(...))`). A decimal point between digits is not counted.
///
/// The count is **stateless**: it scans the raw source with no string,
/// template, or escape tracking, so it cannot disagree with zen's lexer about
/// what is code. Characters inside literals are over-counted, which is
/// conservative. The proper fix, a depth guard inside the zen fork with
/// bounded interval backtracking, is #2875.
pub(crate) fn lexical(source: &str, limits: &StructuralLimits, unary: bool) -> Option<Diagnostic> {
    if source.len() > as_usize(limits.max_source_bytes) {
        return Some(bound(
            codes::BOUND_SOURCE,
            span_of(source),
            &format!(
                "the source is {} bytes, more than {}",
                source.len(),
                limits.max_source_bytes
            ),
        ));
    }
    let too_deep = |index: usize, what: &str, max: u32| {
        let at = u32::try_from(index).unwrap_or(u32::MAX);
        Some(bound(
            codes::BOUND_DEPTH,
            (at, at.saturating_add(1)),
            &format!("{what} more than {max}"),
        ))
    };
    let bytes = source.as_bytes();
    let digit_at = |index: Option<usize>| {
        index
            .and_then(|i| bytes.get(i))
            .is_some_and(u8::is_ascii_digit)
    };
    let mut brackets: u32 = 0;
    let mut levels: u32 = 0;
    let mut word = String::new();
    for (index, byte) in source.bytes().chain(std::iter::once(b' ')).enumerate() {
        if byte.is_ascii_alphanumeric() || byte == b'_' {
            word.push(char::from(byte));
            continue;
        }
        let keyword = matches!(word.as_str(), "and" | "or" | "not" | "in");
        word.clear();
        let opens = match byte {
            b'(' | b'[' | b'{' => {
                brackets = brackets.saturating_add(1);
                if brackets > limits.max_depth {
                    return too_deep(index, "brackets nest", limits.max_depth);
                }
                true
            }
            b')' | b']' | b'}' => {
                brackets = brackets.saturating_sub(1);
                true
            }
            b'.' => !(digit_at(index.checked_sub(1)) && digit_at(index.checked_add(1))),
            b'+' | b'-' | b'*' | b'/' | b'%' | b'^' | b'!' | b'<' | b'>' | b'=' | b'&' | b'|'
            | b'?' | b':' => true,
            b',' => unary,
            _ => false,
        };
        levels = levels
            .saturating_add(u32::from(opens))
            .saturating_add(u32::from(keyword));
        if levels > limits.max_parse_nesting {
            return too_deep(
                index,
                "the source has recursion-opening characters",
                limits.max_parse_nesting,
            );
        }
    }
    None
}

/// The direct children of `node`.
fn children<'n>(node: &'n Node<'n>, out: &mut Vec<&'n Node<'n>>) {
    match node {
        Node::Null
        | Node::Bool(_)
        | Node::Number(_)
        | Node::String(_)
        | Node::Pointer
        | Node::Identifier(_)
        | Node::Root => {}
        Node::Error { node, .. } => out.extend(node.iter().copied()),
        Node::TemplateString(parts) | Node::Array(parts) => out.extend(parts.iter().copied()),
        Node::Object(entries) => {
            for (key, value) in *entries {
                out.push(key);
                out.push(value);
            }
        }
        Node::Assignments { list, output } => {
            for (key, value) in *list {
                out.push(key);
                out.push(value);
            }
            out.extend(output.iter().copied());
        }
        Node::Closure { body: inner, .. } | Node::Parenthesized(inner) => out.push(inner),
        Node::Member { node, property } => {
            out.push(node);
            out.push(property);
        }
        Node::Slice { node, from, to } => {
            out.push(node);
            out.extend(from.iter().copied());
            out.extend(to.iter().copied());
        }
        Node::Interval { left, right, .. } | Node::Binary { left, right, .. } => {
            out.push(left);
            out.push(right);
        }
        Node::Conditional {
            condition,
            on_true,
            on_false,
        } => {
            out.push(condition);
            out.push(on_true);
            out.push(on_false);
        }
        Node::Unary { node, .. } => out.push(node),
        Node::FunctionCall { arguments, .. } => out.extend(arguments.iter().copied()),
        Node::MethodCall {
            this, arguments, ..
        } => {
            out.push(this);
            out.extend(arguments.iter().copied());
        }
    }
}

/// The AST-level bounds, checked with an explicit stack (no recursion): node
/// count, nesting depth, literal sizes, exact numeric source literals, and
/// closure nesting depth. Returns at most one finding per code.
pub(super) fn structure<'n>(
    root: &'n Node<'n>,
    source: &str,
    limits: &StructuralLimits,
    max_closure_depth: u32,
    span: impl Fn(&Node<'_>) -> (u32, u32),
) -> Vec<Diagnostic> {
    let mut found: Vec<Diagnostic> = Vec::new();
    let mut report = |diagnostic: Diagnostic| {
        if found.iter().all(|d| d.code != diagnostic.code) {
            found.push(diagnostic);
        }
    };
    let mut nodes: u32 = 0;
    let mut stack: Vec<(&'n Node<'n>, u32, u32)> = vec![(root, 1, 0)];
    let mut next: Vec<&'n Node<'n>> = Vec::new();
    while let Some((node, depth, closures)) = stack.pop() {
        nodes = nodes.saturating_add(1);
        if nodes > limits.max_nodes {
            report(bound(
                codes::BOUND_NODES,
                span(root),
                &format!("the expression has more than {} nodes", limits.max_nodes),
            ));
            break; // No more findings matter once the node bound is exceeded.
        }
        if depth > limits.max_depth {
            report(bound(
                codes::BOUND_DEPTH,
                span(node),
                &format!("the expression nests more than {} deep", limits.max_depth),
            ));
            // Do not descend further below an over-deep node.
            continue;
        }
        if let Some(diagnostic) = literal_bound(node, source, limits, &span) {
            report(diagnostic);
        }
        if let Some(diagnostic) = bounded_string_call(node, &span) {
            report(diagnostic);
        }
        let closures = if matches!(node, Node::Closure { .. }) {
            let closures = closures.saturating_add(1);
            if closures > max_closure_depth {
                report(bound(
                    codes::BOUND_CLOSURE_DEPTH,
                    span(node),
                    &format!("closures nest more than {max_closure_depth} deep"),
                ));
            }
            closures
        } else {
            closures
        };
        next.clear();
        children(node, &mut next);
        for child in next.iter().rev() {
            stack.push((child, depth.saturating_add(1), closures));
        }
    }
    found
}

fn literal_bound(
    node: &Node<'_>,
    source: &str,
    limits: &StructuralLimits,
    span: &impl Fn(&Node<'_>) -> (u32, u32),
) -> Option<Diagnostic> {
    match node {
        Node::Number(_) => {
            // Reuse the engine lexer's original token span, not the
            // parser's normalized Decimal: scientific mantissas may
            // already have rounded. Norn Number admits exact plain
            // decimal text only, so scientific notation is refused.
            let (start, end) = span(node);
            let exact = source
                .get(as_usize(start)..as_usize(end))
                .is_some_and(|literal| literal.parse::<crate::Number>().is_ok());
            if exact {
                None
            } else {
                Some(bound(
                    codes::UNSUPPORTED_CONSTRUCT,
                    span(node),
                    "a number literal must be exact plain decimal text, without an exponent",
                ))
            }
        }
        Node::String(text) if text.len() > as_usize(limits.max_string_literal_bytes) => {
            Some(bound(
                codes::BOUND_LITERAL,
                span(node),
                &format!(
                    "a string literal of {} bytes is longer than {}",
                    text.len(),
                    limits.max_string_literal_bytes
                ),
            ))
        }
        Node::Array(items) if items.len() > as_usize(limits.max_literal_items) => Some(bound(
            codes::BOUND_LITERAL,
            span(node),
            &format!(
                "a list literal of {} elements is longer than {}",
                items.len(),
                limits.max_literal_items
            ),
        )),
        Node::Object(entries) if entries.len() > as_usize(limits.max_literal_items) => Some(bound(
            codes::BOUND_LITERAL,
            span(node),
            &format!(
                "a record literal of {} entries is larger than {}",
                entries.len(),
                limits.max_literal_items
            ),
        )),
        Node::Null
        | Node::Bool(_)
        | Node::String(_)
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
        | Node::Error { .. } => None,
    }
}
