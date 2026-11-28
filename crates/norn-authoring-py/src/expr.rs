//! Closed `norn-zen/7` expression IR and canonical ZEN renderer.
//!
//! Faithfully mirrors `tools/norn-py/norn_py/expr.py` without executing any
//! Python code at runtime: every authoring `def` or `lambda` callback is
//! statically validated against the closed expression subset and lowered
//! directly from `ruff_python_ast` to canonical ZEN text.

use std::collections::{BTreeMap, BTreeSet};

use crate::error::{CODE_EXPRESSION, PyAuthoringError, SourceSpan};

pub use crate::expr_lower::{lower_callback_source, lower_module_callbacks};
pub use crate::expr_validate::{
    ALLOWED_CALLBACK_FUNCS, ALLOWED_CALLBACK_METHODS, validate_callback_source,
};

/// Comparison operators admitted in `norn-expr` / ZEN.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum CompareOp {
    /// Equality (`==`).
    Eq,
    /// Inequality (`!=`).
    Ne,
    /// Less-than (`<`).
    Lt,
    /// Less-than-or-equal (`<=`).
    Le,
    /// Greater-than (`>`).
    Gt,
    /// Greater-than-or-equal (`>=`).
    Ge,
}

impl CompareOp {
    const fn as_zen(self) -> &'static str {
        match self {
            Self::Eq => "==",
            Self::Ne => "!=",
            Self::Lt => "<",
            Self::Le => "<=",
            Self::Gt => ">",
            Self::Ge => ">=",
        }
    }
}

/// Arithmetic operators admitted in `norn-expr` / ZEN.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum BinaryOp {
    /// Addition (`+`).
    Add,
    /// Subtraction (`-`).
    Sub,
    /// Multiplication (`*`).
    Mul,
    /// Division (`/`).
    Div,
}

impl BinaryOp {
    const fn as_zen(self) -> &'static str {
        match self {
            Self::Add => "+",
            Self::Sub => "-",
            Self::Mul => "*",
            Self::Div => "/",
        }
    }
}

/// Logical connectives admitted in `norn-expr` / ZEN.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum LogicalOp {
    /// Conjunction (`and`).
    And,
    /// Disjunction (`or`).
    Or,
}

impl LogicalOp {
    const fn as_zen(self) -> &'static str {
        match self {
            Self::And => "and",
            Self::Or => "or",
        }
    }
}

/// Exact scalar literal admitted in [`ZenExpr`].
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum ZenLiteral {
    /// `null`.
    Null,
    /// `true` or `false`.
    Bool(bool),
    /// Exact integer digits (base 10, optionally signed).
    Int(String),
    /// Exact fixed-point decimal text validated by [`crate::decimal::format_exact_decimal`].
    Decimal(String),
    /// Exact ZEN string value (unquoted).
    Str(String),
}

/// Closed immutable expression AST lowering to `norn-zen/7` text.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum ZenExpr {
    /// Scalar literal.
    Literal(ZenLiteral),
    /// Dotted field reference (e.g., `self.hours`).
    Ref {
        /// Root identifier (typically `"self"`).
        root: String,
        /// Attribute path segments.
        path: Vec<String>,
    },
    /// Binary comparison.
    Compare {
        /// Comparison operator.
        op: CompareOp,
        /// Left operand.
        left: Box<Self>,
        /// Right operand.
        right: Box<Self>,
    },
    /// Binary arithmetic operation.
    Binary {
        /// Arithmetic operator.
        op: BinaryOp,
        /// Left operand.
        left: Box<Self>,
        /// Right operand.
        right: Box<Self>,
    },
    /// Binary logical operation.
    Logical {
        /// Logical operator.
        op: LogicalOp,
        /// Left operand.
        left: Box<Self>,
        /// Right operand.
        right: Box<Self>,
    },
    /// Logical negation (`not ...`).
    Not(Box<Self>),
    /// Arithmetic negation (`-...`).
    UnaryNeg(Box<Self>),
    /// Ternary conditional (`cond ? yes : no`).
    Conditional {
        /// Guard condition.
        condition: Box<Self>,
        /// True branch.
        yes: Box<Self>,
        /// False branch.
        no: Box<Self>,
    },
    /// Built-in ZEN function or temporal combinator call.
    Call {
        /// Canonical ZEN function name.
        fn_name: String,
        /// Positional arguments.
        args: Vec<Self>,
    },
}

impl ZenExpr {
    /// Renders this expression to canonical `norn-zen/7` text matching
    /// `tools/norn-py/norn_py/expr.py` and `tools/norn-ts/src/builder/expr.ts`.
    ///
    /// # Errors
    ///
    /// Returns [`PyAuthoringError`] with code [`CODE_EXPRESSION`] if any
    /// string literal or identifier violates `norn-expr` v1 syntax rules.
    pub fn to_zen(&self) -> Result<String, PyAuthoringError> {
        render_node(self, None)
    }

    fn is_atomic(&self) -> bool {
        match self {
            Self::Literal(_) | Self::Ref { .. } => true,
            Self::Call { fn_name, .. } => !matches!(
                fn_name.as_str(),
                "dateBefore" | "timestampBefore" | "timestampAtOrBefore"
            ),
            Self::Compare { .. }
            | Self::Binary { .. }
            | Self::Logical { .. }
            | Self::Not(_)
            | Self::UnaryNeg(_)
            | Self::Conditional { .. } => false,
        }
    }

    fn temporal_kind(&self) -> Option<TemporalKind> {
        if let Self::Call { fn_name, .. } = self {
            return match fn_name.as_str() {
                "date" | "add_days" => Some(TemporalKind::Date),
                "timestamp" | "add_seconds" | "resolve_local_timestamp" => {
                    Some(TemporalKind::Timestamp)
                }
                _ => None,
            };
        }
        None
    }

    pub(crate) fn as_date(self) -> Self {
        if self.temporal_kind() == Some(TemporalKind::Date) {
            self
        } else {
            Self::Call {
                fn_name: "date".to_owned(),
                args: vec![self],
            }
        }
    }

    pub(crate) fn as_timestamp(self) -> Self {
        if self.temporal_kind() == Some(TemporalKind::Timestamp) {
            self
        } else {
            Self::Call {
                fn_name: "timestamp".to_owned(),
                args: vec![self],
            }
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum TemporalKind {
    Date,
    Timestamp,
}

/// Static lowering context for compiling Python callbacks directly to [`ZenExpr`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LowerContext {
    /// ZEN root identifier bound to the callback parameter (default `"self"`).
    pub root: String,
    /// Optional closed set of declared fields on the subject; when `Some`,
    /// undeclared field accesses fail closed with [`crate::error::CODE_UNKNOWN_FIELD`].
    pub allowed_fields: Option<BTreeSet<String>>,
    /// Optional compile-time constant table mapping bare names or `Enum.MEMBER`
    /// dotted names to exact [`ZenLiteral`] values.
    pub constants: BTreeMap<String, ZenLiteral>,
}

impl Default for LowerContext {
    fn default() -> Self {
        Self {
            root: "self".to_owned(),
            allowed_fields: None,
            constants: BTreeMap::new(),
        }
    }
}

impl LowerContext {
    /// Creates a new [`LowerContext`] rooted at `root`.
    #[must_use]
    pub fn with_root(root: impl Into<String>) -> Self {
        Self {
            root: root.into(),
            ..Self::default()
        }
    }

    /// Restricts symbolic parameter attribute accesses to `fields`.
    #[must_use]
    pub fn with_allowed_fields<I, S>(mut self, fields: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.allowed_fields = Some(fields.into_iter().map(Into::into).collect());
        self
    }

    /// Registers a compile-time constant or enum member (e.g. `"Status.DRAFT"`).
    #[must_use]
    pub fn with_constant(mut self, name: impl Into<String>, value: ZenLiteral) -> Self {
        self.constants.insert(name.into(), value);
        self
    }
}

/// Renders a ZEN string literal matching `render_zen_string` in `norn_py/expr.py`.
///
/// # Errors
///
/// Returns [`PyAuthoringError`] with code [`CODE_EXPRESSION`] if `value`
/// contains backslashes, newlines, ASCII control characters (including DEL
/// `0x7F`), or both single and double quotes.
pub fn render_zen_string(
    value: &str,
    span: Option<SourceSpan>,
) -> Result<String, PyAuthoringError> {
    let has_forbidden_char = value
        .chars()
        .any(|ch| ch == '\\' || ch == '\n' || ch == '\r' || ch.is_ascii_control());
    let has_single = value.contains('\'');
    let has_double = value.contains('"');
    if has_forbidden_char || (has_single && has_double) {
        return Err(PyAuthoringError::with_optional_span(
            CODE_EXPRESSION,
            format!("string literal '{value}' contains characters not admitted in norn-expr v1"),
            span,
        ));
    }
    if has_single {
        Ok(format!("\"{value}\""))
    } else {
        Ok(format!("'{value}'"))
    }
}

pub(crate) fn check_valid_ident(
    name: &str,
    span: Option<&SourceSpan>,
) -> Result<(), PyAuthoringError> {
    let mut chars = name.chars();
    let valid_head = chars
        .next()
        .is_some_and(|ch| ch.is_ascii_alphabetic() || ch == '_');
    let valid_tail = chars.all(|ch| ch.is_ascii_alphanumeric() || ch == '_');
    if valid_head && valid_tail {
        Ok(())
    } else {
        Err(PyAuthoringError::with_optional_span(
            CODE_EXPRESSION,
            format!("invalid identifier '{name}'"),
            span.cloned(),
        ))
    }
}

fn render_operand(node: &ZenExpr, span: Option<&SourceSpan>) -> Result<String, PyAuthoringError> {
    let text = render_node(node, span)?;
    if node.is_atomic() {
        Ok(text)
    } else {
        Ok(format!("({text})"))
    }
}

fn render_node(node: &ZenExpr, span: Option<&SourceSpan>) -> Result<String, PyAuthoringError> {
    match node {
        ZenExpr::Literal(lit) => match lit {
            ZenLiteral::Null => Ok("null".to_owned()),
            ZenLiteral::Bool(true) => Ok("true".to_owned()),
            ZenLiteral::Bool(false) => Ok("false".to_owned()),
            ZenLiteral::Int(digits) | ZenLiteral::Decimal(digits) => Ok(digits.clone()),
            ZenLiteral::Str(text) => render_zen_string(text, span.cloned()),
        },
        ZenExpr::Ref { root, path } => {
            check_valid_ident(root, span)?;
            let mut out = root.clone();
            for seg in path {
                check_valid_ident(seg, span)?;
                out.push('.');
                out.push_str(seg);
            }
            Ok(out)
        }
        ZenExpr::Compare { op, left, right } => Ok(format!(
            "{} {} {}",
            render_operand(left, span)?,
            op.as_zen(),
            render_operand(right, span)?
        )),
        ZenExpr::Binary { op, left, right } => Ok(format!(
            "{} {} {}",
            render_operand(left, span)?,
            op.as_zen(),
            render_operand(right, span)?
        )),
        ZenExpr::Logical { op, left, right } => Ok(format!(
            "{} {} {}",
            render_operand(left, span)?,
            op.as_zen(),
            render_operand(right, span)?
        )),
        ZenExpr::Not(operand) => Ok(format!("not {}", render_operand(operand, span)?)),
        ZenExpr::UnaryNeg(operand) => Ok(format!("-{}", render_operand(operand, span)?)),
        ZenExpr::Conditional { condition, yes, no } => Ok(format!(
            "{} ? {} : {}",
            render_operand(condition, span)?,
            render_operand(yes, span)?,
            render_operand(no, span)?
        )),
        ZenExpr::Call { fn_name, args } => render_call(fn_name, args, span),
    }
}

fn render_call(
    fn_name: &str,
    args: &[ZenExpr],
    span: Option<&SourceSpan>,
) -> Result<String, PyAuthoringError> {
    if fn_name == "dateBefore" {
        let [left, right] = args else {
            return Err(PyAuthoringError::with_optional_span(
                CODE_EXPRESSION,
                "dateBefore requires 2 arguments",
                span.cloned(),
            ));
        };
        return Ok(format!(
            "date({}) < date({})",
            render_node(left, span)?,
            render_node(right, span)?
        ));
    }
    if fn_name == "timestampBefore" || fn_name == "timestampAtOrBefore" {
        let [left, right] = args else {
            return Err(PyAuthoringError::with_optional_span(
                CODE_EXPRESSION,
                format!("{fn_name} requires 2 arguments"),
                span.cloned(),
            ));
        };
        let op = if fn_name == "timestampBefore" {
            "<"
        } else {
            "<="
        };
        return Ok(format!(
            "{} {op} {}",
            render_operand(left, span)?,
            render_operand(right, span)?
        ));
    }

    let mut out = String::from(fn_name);
    out.push('(');
    for (idx, arg) in args.iter().enumerate() {
        if idx > 0 {
            out.push_str(", ");
        }
        out.push_str(&render_node(arg, span)?);
    }
    out.push(')');
    Ok(out)
}
