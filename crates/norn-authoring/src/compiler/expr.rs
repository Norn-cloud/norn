//! Typed expression AST, type inference, contextual assignability, and ZEN rendering.
//!
//! Faithful port of `tools/norn-ts/src/builder/expr.ts`, plus the
//! `parsedTemporal` and `narrowSubjectByCondition` helpers from
//! `tools/norn-ts/src/compiler/definitions.ts`.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::atomic::{AtomicU64, Ordering};

use serde_json::{Map, Value};

use super::value_schema::{ValueSchema, assignable};

pub(crate) use super::expr_render::{
    assert_expression_budget, narrow_subject_schema, parsed_temporal, render, zen_key, zen_string,
};
pub(crate) use super::expr_type::type_of;

static NEXT_BINDER: AtomicU64 = AtomicU64::new(1);

/// Opaque unique binder identifier for array closures (`some`, `all`, `filter`, `count`, `map`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) struct BinderId(u64);

impl BinderId {
    /// Allocates a fresh unique binder id.
    #[must_use]
    pub(crate) fn fresh() -> Self {
        Self(NEXT_BINDER.fetch_add(1, Ordering::Relaxed))
    }
}

/// Binary arithmetic or membership operator.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum BinaryOp {
    /// `+`
    Add,
    /// `-`
    Sub,
    /// `*`
    Mul,
    /// `/`
    Div,
    /// `%`
    Rem,
    /// `in`
    In,
}

impl BinaryOp {
    /// Returns the ZEN operator spelling.
    #[must_use]
    pub(crate) const fn wire_str(self) -> &'static str {
        match self {
            Self::Add => "+",
            Self::Sub => "-",
            Self::Mul => "*",
            Self::Div => "/",
            Self::Rem => "%",
            Self::In => "in",
        }
    }
}

/// Builtin call function admitted in typed expressions (`CallFn`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CallFn {
    /// `concat` (rendered as `flatten([a, b])`)
    Concat,
    /// `lower`
    Lower,
    /// `upper`
    Upper,
    /// `trim`
    Trim,
    /// `len`
    Len,
    /// `take`
    Take,
    /// `contains`
    Contains,
    /// `startsWith`
    StartsWith,
    /// `endsWith`
    EndsWith,
    /// `dateBefore` (rendered as `date(a) < date(b)`)
    DateBefore,
    /// `timestampBefore` (rendered as `a < b`)
    TimestampBefore,
    /// `timestampAtOrBefore` (rendered as `a <= b`)
    TimestampAtOrBefore,
    /// `date`
    Date,
    /// `timestamp`
    Timestamp,
    /// `sum`
    Sum,
    /// `floor`
    Floor,
    /// `days_between`
    DaysBetween,
    /// `add_days`
    AddDays,
    /// `seconds_between`
    SecondsBetween,
    /// `add_seconds`
    AddSeconds,
    /// `intervals_overlap`
    IntervalsOverlap,
    /// `interval_gap_seconds`
    IntervalGapSeconds,
    /// `interval_intersection_seconds`
    IntervalIntersectionSeconds,
    /// `resolve_local_timestamp`
    ResolveLocalTimestamp,
}

impl CallFn {
    /// Returns the ZEN function name.
    #[must_use]
    pub(crate) const fn wire_str(self) -> &'static str {
        match self {
            Self::Concat => "concat",
            Self::Lower => "lower",
            Self::Upper => "upper",
            Self::Trim => "trim",
            Self::Len => "len",
            Self::Take => "take",
            Self::Contains => "contains",
            Self::StartsWith => "startsWith",
            Self::EndsWith => "endsWith",
            Self::DateBefore => "dateBefore",
            Self::TimestampBefore => "timestampBefore",
            Self::TimestampAtOrBefore => "timestampAtOrBefore",
            Self::Date => "date",
            Self::Timestamp => "timestamp",
            Self::Sum => "sum",
            Self::Floor => "floor",
            Self::DaysBetween => "days_between",
            Self::AddDays => "add_days",
            Self::SecondsBetween => "seconds_between",
            Self::AddSeconds => "add_seconds",
            Self::IntervalsOverlap => "intervals_overlap",
            Self::IntervalGapSeconds => "interval_gap_seconds",
            Self::IntervalIntersectionSeconds => "interval_intersection_seconds",
            Self::ResolveLocalTimestamp => "resolve_local_timestamp",
        }
    }
}

/// Array closure function (`norn-zen/6`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ClosureFn {
    /// `some`
    Some,
    /// `all`
    All,
    /// `none`
    None,
    /// `one`
    One,
    /// `filter`
    Filter,
    /// `count`
    Count,
    /// `map`
    Map,
}

impl ClosureFn {
    /// Returns the ZEN closure function name.
    #[must_use]
    pub(crate) const fn wire_str(self) -> &'static str {
        match self {
            Self::Some => "some",
            Self::All => "all",
            Self::None => "none",
            Self::One => "one",
            Self::Filter => "filter",
            Self::Count => "count",
            Self::Map => "map",
        }
    }
}

/// Comparison operator.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CompareOp {
    /// `==`
    Eq,
    /// `!=`
    Ne,
    /// `<`
    Lt,
    /// `<=`
    Le,
    /// `>`
    Gt,
    /// `>=`
    Ge,
}

impl CompareOp {
    /// Returns the ZEN comparison spelling.
    #[must_use]
    pub(crate) const fn wire_str(self) -> &'static str {
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

/// Boolean logical operator.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum LogicalOp {
    /// `and`
    And,
    /// `or`
    Or,
}

impl LogicalOp {
    /// Returns the ZEN logical operator spelling.
    #[must_use]
    pub(crate) const fn wire_str(self) -> &'static str {
        match self {
            Self::And => "and",
            Self::Or => "or",
        }
    }
}

/// Parsed temporal value kind (`date` or `timestamp`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum TemporalKind {
    /// Parsed calendar date.
    Date,
    /// Parsed UTC timestamp.
    Timestamp,
}

impl TemporalKind {
    /// Returns the wire function name (`date` or `timestamp`).
    #[must_use]
    pub(crate) const fn wire_str(self) -> &'static str {
        match self {
            Self::Date => "date",
            Self::Timestamp => "timestamp",
        }
    }
}

/// Typed, first-order expression node compiled from TypeScript syntax.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Expr {
    /// Binary arithmetic or membership expression.
    Binary {
        /// Operator.
        op: BinaryOp,
        /// Left operand.
        left: Box<Self>,
        /// Right operand.
        right: Box<Self>,
    },
    /// Builtin function call.
    Call {
        /// Function identifier.
        func: CallFn,
        /// Evaluated arguments.
        args: Vec<Self>,
    },
    /// Array closure (`some`, `all`, `filter`, `count`, `map`, etc.).
    Closure {
        /// Closure operator.
        func: ClosureFn,
        /// Target list expression.
        list: Box<Self>,
        /// Unique element binder id.
        binder: BinderId,
        /// Body expression referencing `binder`.
        body: Box<Self>,
    },
    /// Ternary conditional (`cond ? yes : no`).
    Conditional {
        /// Boolean condition.
        condition: Box<Self>,
        /// True branch.
        yes: Box<Self>,
        /// False branch.
        no: Box<Self>,
    },
    /// Nullish coalescing (`value ?? fallback`).
    Coalesce {
        /// Primary expression.
        value: Box<Self>,
        /// Fallback expression.
        fallback: Box<Self>,
    },
    /// Reference to a closure element binder and property path.
    Bound {
        /// Binder identity.
        binder: BinderId,
        /// Property path segments.
        path: Vec<String>,
    },
    /// Plain JSON scalar literal.
    Literal(Value),
    /// Reference to a named scope binding and property path.
    Input {
        /// Binding name.
        name: String,
        /// Property path segments.
        path: Vec<String>,
    },
    /// Scalar or temporal comparison.
    Compare {
        /// Comparison operator.
        op: CompareOp,
        /// Left operand.
        left: Box<Self>,
        /// Right operand.
        right: Box<Self>,
    },
    /// Strict boolean `and` / `or`.
    Logical {
        /// Logical operator.
        op: LogicalOp,
        /// Left operand.
        left: Box<Self>,
        /// Right operand.
        right: Box<Self>,
    },
    /// Strict boolean negation.
    Not(Box<Self>),
    /// Bounded array literal.
    List(Vec<Self>),
    /// Closed record literal keeping written field order.
    Record(Vec<(String, Self)>),
}

impl Expr {
    /// Constructs a literal expression.
    #[must_use]
    pub(crate) const fn lit(value: Value) -> Self {
        Self::Literal(value)
    }

    /// Counts the exact node and leaf work required to deep-clone this expression tree.
    #[must_use]
    pub(crate) fn clone_work(&self) -> usize {
        let mut work = 0_usize;
        let mut stack = vec![self];
        while let Some(node) = stack.pop() {
            work = work.saturating_add(1);
            match node {
                Self::Binary { left, right, .. }
                | Self::Compare { left, right, .. }
                | Self::Logical { left, right, .. } => {
                    stack.push(left);
                    stack.push(right);
                }
                Self::Call { args, .. } | Self::List(args) => stack.extend(args),
                Self::Closure { list, body, .. } => {
                    stack.push(list);
                    stack.push(body);
                }
                Self::Conditional { condition, yes, no } => {
                    stack.push(condition);
                    stack.push(yes);
                    stack.push(no);
                }
                Self::Coalesce { value, fallback } => {
                    stack.push(value);
                    stack.push(fallback);
                }
                Self::Bound { path, .. } | Self::Input { path, .. } => {
                    work = work.saturating_add(path.len());
                }
                Self::Literal(val) => {
                    if let Value::String(s) = val {
                        work = work.saturating_add(s.len().div_ceil(64));
                    }
                }
                Self::Not(inner) => stack.push(inner),
                Self::Record(entries) => {
                    for (_, value) in entries {
                        stack.push(value);
                    }
                }
            }
        }
        work
    }

    /// Constructs a named binding reference with optional property path.
    #[must_use]
    pub(crate) fn input(name: impl Into<String>, path: Vec<String>) -> Self {
        Self::Input {
            name: name.into(),
            path,
        }
    }

    /// Appends a property segment to an `Input` or `Bound` expression.
    ///
    /// # Errors
    ///
    /// Returns [`ExprError`] (`NORN-FLOW-SLICE`) if `self` is not a path reference.
    pub(crate) fn field(self, key: String) -> Result<Self, ExprError> {
        match self {
            Self::Input { name, mut path } => {
                path.push(key);
                Ok(Self::Input { name, path })
            }
            Self::Bound { binder, mut path } => {
                path.push(key);
                Ok(Self::Bound { binder, path })
            }
            Self::Binary { .. }
            | Self::Call { .. }
            | Self::Closure { .. }
            | Self::Conditional { .. }
            | Self::Coalesce { .. }
            | Self::Literal(_)
            | Self::Compare { .. }
            | Self::Logical { .. }
            | Self::Not(_)
            | Self::List(_)
            | Self::Record(_) => Err(ExprError::new(
                "NORN-FLOW-SLICE",
                "expected a property path",
            )),
        }
    }

    /// Constructs a binary expression.
    #[must_use]
    pub(crate) fn binary(op: BinaryOp, left: Self, right: Self) -> Self {
        Self::Binary {
            op,
            left: Box::new(left),
            right: Box::new(right),
        }
    }

    /// Constructs a call expression.
    #[must_use]
    pub(crate) const fn call(func: CallFn, args: Vec<Self>) -> Self {
        Self::Call { func, args }
    }

    /// Constructs a comparison expression.
    #[must_use]
    pub(crate) fn compare(op: CompareOp, left: Self, right: Self) -> Self {
        Self::Compare {
            op,
            left: Box::new(left),
            right: Box::new(right),
        }
    }

    /// Constructs a logical `and` expression.
    #[must_use]
    pub(crate) fn and(left: Self, right: Self) -> Self {
        Self::Logical {
            op: LogicalOp::And,
            left: Box::new(left),
            right: Box::new(right),
        }
    }

    /// Constructs a logical `or` expression.
    #[must_use]
    pub(crate) fn or(left: Self, right: Self) -> Self {
        Self::Logical {
            op: LogicalOp::Or,
            left: Box::new(left),
            right: Box::new(right),
        }
    }

    /// Constructs a negation expression.
    #[must_use]
    pub(crate) fn not(operand: Self) -> Self {
        Self::Not(Box::new(operand))
    }

    /// Constructs a conditional expression.
    #[must_use]
    pub(crate) fn conditional(condition: Self, yes: Self, no: Self) -> Self {
        Self::Conditional {
            condition: Box::new(condition),
            yes: Box::new(yes),
            no: Box::new(no),
        }
    }

    /// Constructs a nullish coalescing expression.
    #[must_use]
    pub(crate) fn coalesce(value: Self, fallback: Self) -> Self {
        Self::Coalesce {
            value: Box::new(value),
            fallback: Box::new(fallback),
        }
    }

    /// Wraps a raw date expression in `date(...)` unless already parsed as a date.
    #[must_use]
    pub(crate) fn as_date(self) -> Self {
        if temporal_kind(&self) == Some(TemporalKind::Date) {
            self
        } else {
            Self::call(CallFn::Date, vec![self])
        }
    }

    /// Wraps a raw timestamp expression in `timestamp(...)` unless already parsed as a timestamp.
    #[must_use]
    pub(crate) fn as_timestamp(self) -> Self {
        if temporal_kind(&self) == Some(TemporalKind::Timestamp) {
            self
        } else {
            Self::call(CallFn::Timestamp, vec![self])
        }
    }

    /// Serializes the expression AST to plain JSON (for expansion budget checks).
    #[must_use]
    pub(crate) fn to_json(&self) -> Value {
        let mut map = Map::new();
        match self {
            Self::Binary { op, left, right } => {
                map.insert("_tag".to_owned(), Value::String("Binary".to_owned()));
                map.insert("op".to_owned(), Value::String(op.wire_str().to_owned()));
                map.insert("left".to_owned(), left.to_json());
                map.insert("right".to_owned(), right.to_json());
            }
            Self::Call { func, args } => {
                map.insert("_tag".to_owned(), Value::String("Call".to_owned()));
                map.insert("fn".to_owned(), Value::String(func.wire_str().to_owned()));
                map.insert(
                    "args".to_owned(),
                    Value::Array(args.iter().map(Self::to_json).collect()),
                );
            }
            Self::Closure {
                func,
                list,
                binder,
                body,
            } => {
                map.insert("_tag".to_owned(), Value::String("Closure".to_owned()));
                map.insert("fn".to_owned(), Value::String(func.wire_str().to_owned()));
                map.insert("list".to_owned(), list.to_json());
                map.insert("binder".to_owned(), Value::Number(binder.0.into()));
                map.insert("body".to_owned(), body.to_json());
            }
            Self::Conditional { condition, yes, no } => {
                map.insert("_tag".to_owned(), Value::String("Conditional".to_owned()));
                map.insert("condition".to_owned(), condition.to_json());
                map.insert("yes".to_owned(), yes.to_json());
                map.insert("no".to_owned(), no.to_json());
            }
            Self::Coalesce { value, fallback } => {
                map.insert("_tag".to_owned(), Value::String("Coalesce".to_owned()));
                map.insert("value".to_owned(), value.to_json());
                map.insert("fallback".to_owned(), fallback.to_json());
            }
            Self::Bound { binder, path } => {
                map.insert("_tag".to_owned(), Value::String("Bound".to_owned()));
                map.insert("binder".to_owned(), Value::Number(binder.0.into()));
                map.insert(
                    "path".to_owned(),
                    Value::Array(path.iter().cloned().map(Value::String).collect()),
                );
            }
            Self::Literal(val) => {
                map.insert("_tag".to_owned(), Value::String("Literal".to_owned()));
                map.insert("value".to_owned(), val.clone());
            }
            Self::Input { name, path } => {
                map.insert("_tag".to_owned(), Value::String("Input".to_owned()));
                map.insert("name".to_owned(), Value::String(name.clone()));
                map.insert(
                    "path".to_owned(),
                    Value::Array(path.iter().cloned().map(Value::String).collect()),
                );
            }
            Self::Compare { op, left, right } => {
                map.insert("_tag".to_owned(), Value::String("Compare".to_owned()));
                map.insert("op".to_owned(), Value::String(op.wire_str().to_owned()));
                map.insert("left".to_owned(), left.to_json());
                map.insert("right".to_owned(), right.to_json());
            }
            Self::Logical { op, left, right } => {
                map.insert("_tag".to_owned(), Value::String("Logical".to_owned()));
                map.insert("op".to_owned(), Value::String(op.wire_str().to_owned()));
                map.insert("left".to_owned(), left.to_json());
                map.insert("right".to_owned(), right.to_json());
            }
            Self::Not(op) => {
                map.insert("_tag".to_owned(), Value::String("Not".to_owned()));
                map.insert("operand".to_owned(), op.to_json());
            }
            Self::List(items) => {
                map.insert("_tag".to_owned(), Value::String("List".to_owned()));
                map.insert(
                    "items".to_owned(),
                    Value::Array(items.iter().map(Self::to_json).collect()),
                );
            }
            Self::Record(fields) => {
                map.insert("_tag".to_owned(), Value::String("Record".to_owned()));
                let arr = fields
                    .iter()
                    .map(|(k, v)| Value::Array(vec![Value::String(k.clone()), v.to_json()]))
                    .collect();
                map.insert("fields".to_owned(), Value::Array(arr));
            }
        }
        Value::Object(map)
    }
}

/// Returns the parsed temporal kind of `expr`, if it is a parsed date or timestamp.
#[must_use]
pub(crate) fn temporal_kind(expr: &Expr) -> Option<TemporalKind> {
    let Expr::Call { func, .. } = expr else {
        return None;
    };
    match func {
        CallFn::Date | CallFn::AddDays => Some(TemporalKind::Date),
        CallFn::Timestamp | CallFn::AddSeconds | CallFn::ResolveLocalTimestamp => {
            Some(TemporalKind::Timestamp)
        }
        CallFn::Concat
        | CallFn::Lower
        | CallFn::Upper
        | CallFn::Trim
        | CallFn::Len
        | CallFn::Take
        | CallFn::Contains
        | CallFn::StartsWith
        | CallFn::EndsWith
        | CallFn::DateBefore
        | CallFn::TimestampBefore
        | CallFn::TimestampAtOrBefore
        | CallFn::Sum
        | CallFn::Floor
        | CallFn::DaysBetween
        | CallFn::SecondsBetween
        | CallFn::IntervalsOverlap
        | CallFn::IntervalGapSeconds
        | CallFn::IntervalIntersectionSeconds => None,
    }
}

/// Scope binding entry for ZEN typing and rendering.
#[derive(Clone, Debug)]
pub(crate) enum Binding {
    /// Direct ZEN path root (for example `"self"`, `"input"`, `""`, `"event.results.load"`).
    Zen {
        /// ZEN root path.
        zen: String,
        /// Declared closed schema of this binding.
        schema: ValueSchema,
    },
    /// Inline expression bound earlier in the same decision row.
    Inline {
        /// Inline expression value.
        inline: Expr,
        /// Scope in which `inline` is rendered.
        scope: Scope,
        /// Declared closed schema of this binding.
        schema: ValueSchema,
    },
}

impl Binding {
    /// Returns the declared schema of this binding.
    #[must_use]
    pub(crate) const fn schema(&self) -> &ValueSchema {
        match self {
            Self::Zen { schema, .. } | Self::Inline { schema, .. } => schema,
        }
    }
}

/// Named bindings visible to an expression.
pub(crate) type Scope = BTreeMap<String, Binding>;

pub(crate) type Binders = BTreeMap<BinderId, (String, ValueSchema)>;

/// Expression typing or rendering diagnostic.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ExprError {
    /// Stable diagnostic code (`NORN-FLOW-EXPRESSION`, `NORN-FLOW-SLICE`, `NORN-FLOW-TYPE`).
    pub(crate) code: String,
    /// Human-readable error message.
    pub(crate) message: String,
}

impl ExprError {
    /// Creates a new expression error.
    #[must_use]
    pub(crate) fn new(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
        }
    }
}

/// Checks contextual assignability of `expr` against `expected` in `scope`.
///
/// Mirrors `assignmentError` in `tools/norn-ts/src/builder/expr.ts`.
///
/// # Errors
///
/// Returns [`ExprError`] if sub-expression typing fails.
pub(crate) fn assignment_error(
    expr: &Expr,
    expected: &ValueSchema,
    scope: &Scope,
    path: &str,
) -> Result<Option<String>, ExprError> {
    if let Expr::Conditional { condition, yes, no } = expr {
        if !matches!(type_of(condition, scope)?, ValueSchema::Boolean { .. }) {
            return Ok(Some(format!("{path} condition must be boolean")));
        }
        if let Some(problem) = assignment_error(yes, expected, scope, path)? {
            return Ok(Some(problem));
        }
        return assignment_error(no, expected, scope, path);
    }
    if let Expr::Coalesce { value, fallback } = expr {
        let source = type_of(value, scope)?;
        if !matches!(source, ValueSchema::Null { .. })
            && let Some(problem) = assignable(source.non_null(), expected, path)
        {
            return Ok(Some(problem));
        }
        return assignment_error(fallback, expected, scope, path);
    }
    let target = expected.non_null();
    if let (Expr::List(items), ValueSchema::Array { items: elem_ty, .. }) = (expr, target) {
        let item_path = format!("{path}[]");
        for item in items {
            if let Some(problem) = assignment_error(item, elem_ty, scope, &item_path)? {
                return Ok(Some(problem));
            }
        }
        return Ok(None);
    }
    if let (Expr::Record(fields), ValueSchema::Object(obj)) = (expr, target) {
        let mut keys = BTreeSet::new();
        for (key, value) in fields {
            let _ = zen_key(key)?;
            if !keys.insert(key.as_str()) {
                return Ok(Some(format!("{path}.{key} is repeated")));
            }
            let Some(field_schema) = obj.properties.get(key) else {
                return Ok(Some(format!("{path}.{key} is not declared")));
            };
            if let Some(problem) =
                assignment_error(value, field_schema, scope, &format!("{path}.{key}"))?
            {
                return Ok(Some(problem));
            }
        }
        for req in &obj.required {
            if !keys.contains(req.as_str()) {
                return Ok(Some(format!("{path}.{req} is required")));
            }
        }
        return Ok(None);
    }
    let source = if let Expr::Literal(Value::String(s)) = expr {
        ValueSchema::text_enum(vec![s.clone()])
    } else {
        type_of(expr, scope)?
    };
    Ok(assignable(&source, expected, path))
}
