//! OVERLAY-LINEAR-1: bounded typed ZEN projection for the Core 06 SMT fragment.
use crate::{ExprError, Expression, Type, ZenTypeEnv, check_expression};
use std::{collections::BTreeMap, fmt::Write};
use zen_expression::{intellisense::IntelliSense, parser::Node};

/// A typed, source-derived SMT predicate. This is a goal, never solver evidence.
#[derive(Debug, Clone)]
pub struct LinearGuard {
    predicate: String,
    variables: BTreeMap<Vec<String>, Type>,
}
impl LinearGuard {
    /// SMT-LIB Boolean term, with explicit arithmetic definedness conditions.
    #[must_use]
    pub fn predicate(&self) -> &str {
        &self.predicate
    }
    /// Exact source paths and their existing Norn types.
    #[must_use]
    pub fn variables(&self) -> &BTreeMap<Vec<String>, Type> {
        &self.variables
    }
}

/// Injective SMT symbol for a literal path; no source bytes become SMT syntax.
#[must_use]
pub fn linear_symbol(path: &[String]) -> String {
    let mut out = String::from("v");
    for member in path {
        let _infallible = write!(out, "_{}_", member.len());
        for byte in member.bytes() {
            let _infallible = write!(out, "{byte:02x}");
        }
    }
    out
}

fn unsupported() -> ExprError {
    ExprError::Compile {
        message: "unsupported_predicate: outside Core 06 linear/equality/boolean syntax".into(),
    }
}

/// Compile a guard using the ordinary parser, bounds and type environment.
/// # Errors
/// Refuses untyped predicates, calls, dynamic paths, nullable/opaque operands,
/// quantifiers, strings outside SMT Unicode planes 0–2, and nonlinear terms.
pub fn linear_guard(source: &str, env: &ZenTypeEnv) -> Result<LinearGuard, ExprError> {
    let expression = Expression::compile(source)?;
    check_expression(&expression, env, &Type::Bool).map_err(|_error| unsupported())?;
    IntelliSense::new()
        .with_strict(true)
        .with_ast(source, false, |ast, _| {
            let mut compiler = Compiler {
                env,
                variables: BTreeMap::new(),
            };
            let term = compiler.emit(ast)?;
            Ok(LinearGuard {
                predicate: format!("(and {} {})", term.defined, term.smt),
                variables: compiler.variables,
            })
        })
        .ok_or_else(unsupported)?
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Number,
    Bool,
    Text,
}
struct Term {
    smt: String,
    constant: bool,
    kind: Kind,
    defined: String,
}
struct Compiler<'a> {
    env: &'a ZenTypeEnv,
    variables: BTreeMap<Vec<String>, Type>,
}
impl Compiler<'_> {
    fn variable(&mut self, node: &Node<'_>) -> Result<Term, ExprError> {
        let path = crate::reads::literal_path(node).ok_or_else(unsupported)?;
        let mut parts = path.iter();
        let mut ty = self
            .env
            .variables()
            .get(parts.next().ok_or_else(unsupported)?)
            .ok_or_else(unsupported)?;
        for part in parts {
            let Type::Record(record) = ty else {
                return Err(unsupported());
            };
            ty = &self
                .env
                .record(record)
                .and_then(|r| r.fields.get(part))
                .ok_or_else(unsupported)?
                .ty;
        }
        let kind = match ty {
            Type::Integer | Type::Decimal { .. } | Type::Number => Kind::Number,
            Type::Bool => Kind::Bool,
            Type::Text { .. } | Type::Enum { .. } => Kind::Text,
            Type::DecimalBinding { .. }
            | Type::Timestamp { .. }
            | Type::Date
            | Type::Duration
            | Type::Record(_)
            | Type::List(_)
            | Type::Optional(_)
            | Type::Json => return Err(unsupported()),
        };
        self.variables.insert(path.clone(), ty.clone());
        Ok(Term {
            smt: linear_symbol(&path),
            constant: false,
            kind,
            defined: "true".into(),
        })
    }
    fn emit(&mut self, node: &Node<'_>) -> Result<Term, ExprError> {
        let (smt, constant, kind) = match node {
            Node::Identifier(_) | Node::Member { .. } => return self.variable(node),
            Node::Parenthesized(inner) => return self.emit(inner),
            Node::Bool(value) => (value.to_string(), true, Kind::Bool),
            Node::String(value) => {
                let mut text = String::from("\"");
                for ch in value.chars() {
                    if u32::from(ch) > 0x2_FFFF {
                        return Err(unsupported());
                    }
                    let _infallible = write!(text, "\\u{{{:x}}}", u32::from(ch));
                }
                text.push('"');
                (text, true, Kind::Text)
            }
            Node::Number(value) => {
                let value = value.normalize();
                let magnitude = value.abs().mantissa();
                let numerator = if value.is_sign_negative() {
                    format!("(- {magnitude})")
                } else {
                    magnitude.to_string()
                };
                (
                    format!("(/ {numerator} {})", 10_u128.pow(value.scale())),
                    true,
                    Kind::Number,
                )
            }
            Node::Unary { node, operator } => {
                let term = self.emit(node)?;
                let op = operator.to_string();
                let operator = match (op.as_str(), term.kind) {
                    ("-", Kind::Number) => "-",
                    ("not" | "!", Kind::Bool) => "not",
                    _ => return Err(unsupported()),
                };
                return Ok(Term {
                    smt: format!("({operator} {})", term.smt),
                    constant: term.constant,
                    kind: term.kind,
                    defined: term.defined,
                });
            }
            Node::Binary {
                left,
                operator,
                right,
            } => return self.binary(left, &operator.to_string(), right),
            Node::Null
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
            | Node::FunctionCall { .. }
            | Node::MethodCall { .. }
            | Node::Error { .. } => return Err(unsupported()),
        };
        Ok(Term {
            smt,
            constant,
            kind,
            defined: "true".into(),
        })
    }
    fn binary(&mut self, left: &Node<'_>, op: &str, right: &Node<'_>) -> Result<Term, ExprError> {
        let left = self.emit(left)?;
        let right = self.emit(right)?;
        let numeric = left.kind == Kind::Number && right.kind == Kind::Number;
        let (operator, kind) = match op {
            "+" | "-" if numeric => (op, Kind::Number),
            "*" if numeric && (left.constant || right.constant) => ("*", Kind::Number),
            "/" if numeric && right.constant => ("/", Kind::Number),
            "and" | "or" if left.kind == Kind::Bool && right.kind == Kind::Bool => (op, Kind::Bool),
            "==" if left.kind == right.kind => ("=", Kind::Bool),
            "!=" if left.kind == right.kind => ("distinct", Kind::Bool),
            "<" | ">" | "<=" | ">=" if numeric => (op, Kind::Bool),
            _ => return Err(unsupported()),
        };
        let defined = match op {
            "and" => format!(
                "(and {} (or (not {}) {}))",
                left.defined, left.smt, right.defined
            ),
            "or" => format!("(and {} (or {} {}))", left.defined, left.smt, right.defined),
            "/" => format!(
                "(and {} {} (not (= {} 0)))",
                left.defined, right.defined, right.smt
            ),
            _ => format!("(and {} {})", left.defined, right.defined),
        };
        Ok(Term {
            smt: format!("({operator} {} {})", left.smt, right.smt),
            constant: left.constant && right.constant,
            kind,
            defined,
        })
    }
}
