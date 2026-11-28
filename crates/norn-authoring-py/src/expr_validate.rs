//! Static AST validation for Python authoring callbacks (`def` and `lambda`).

use ruff_python_ast::{
    BoolOp, CmpOp, Expr, ExprCall, ExprCompare, ExprLambda, Number, Operator,
    ParameterWithDefault, Parameters, Stmt, StmtFunctionDef, UnaryOp,
};
use ruff_python_parser::parse_module;
use ruff_text_size::Ranged;

use crate::error::{
    CODE_FLOAT_LITERAL, CODE_FORBIDDEN_ATTR, CODE_FORBIDDEN_CALL, CODE_FORBIDDEN_CONTROL_FLOW,
    CODE_FORBIDDEN_NAME, CODE_SYNTAX, CODE_UNBOUNDED, PyAuthoringError, SourceSpan,
};
use crate::line_table::LineTable;
use crate::sandbox::{FORBIDDEN_INTERNAL_ATTRS, MAX_SOURCE_BYTES, is_forbidden_name};

/// Closed allowlist of standalone functions callable inside Norn expression callbacks.
pub const ALLOWED_CALLBACK_FUNCS: &[&str] = &[
    "Decimal",
    "add_days",
    "add_seconds",
    "all_of",
    "any_of",
    "between",
    "contains",
    "date_before",
    "days_between",
    "if_else",
    "interval_gap_seconds",
    "interval_intersection_seconds",
    "intervals_overlap",
    "lit",
    "norn_len",
    "not_",
    "parse_date",
    "parse_timestamp",
    "seconds_between",
    "timestamp_at_or_before",
    "timestamp_before",
    "when",
];

/// Closed allowlist of methods callable on symbolic expressions inside callbacks.
pub const ALLOWED_CALLBACK_METHODS: &[&str] = &["eq", "is_not_null", "is_null", "ne"];

/// Validates a standalone Python callback (`def f(s): return ...` or `lambda s: ...`)
/// against the closed callback AST profile without executing Python.
///
/// # Errors
///
/// Returns [`PyAuthoringError`] if parsing fails or the callback violates the
/// closed expression profile.
pub fn validate_callback_source(source: &str, filename: &str) -> Result<(), PyAuthoringError> {
    if source.len() > MAX_SOURCE_BYTES {
        return Err(PyAuthoringError::spanned(
            CODE_UNBOUNDED,
            format!("source exceeds {MAX_SOURCE_BYTES} byte limit"),
            SourceSpan::new(filename, 1, 0),
        ));
    }
    let lines = LineTable::new(filename, source);
    let parsed_mod = parse_module(source).map_err(|err| {
        PyAuthoringError::spanned(
            CODE_SYNTAX,
            err.error.to_string(),
            lines.span_at(err.location.start()),
        )
    })?;
    let body: &[Stmt] = parsed_mod.suite();
    match body {
        [Stmt::FunctionDef(func_def)] => validate_function_callback(func_def, &lines),
        [Stmt::Expr(expr_stmt)] => {
            if let Expr::Lambda(lambda) = expr_stmt.value.as_ref() {
                validate_lambda_callback(lambda, &lines)
            } else {
                Err(PyAuthoringError::spanned(
                    CODE_FORBIDDEN_CONTROL_FLOW,
                    "expected a `def` or `lambda` authoring callback",
                    lines.span_at(expr_stmt.value.range().start()),
                ))
            }
        }
        _ => Err(PyAuthoringError::spanned(
            CODE_FORBIDDEN_CONTROL_FLOW,
            "callback source must contain a single `def` or `lambda` definition",
            lines.file_start(),
        )),
    }
}

pub(crate) fn validate_function_callback(
    func_def: &StmtFunctionDef,
    lines: &LineTable<'_>,
) -> Result<(), PyAuthoringError> {
    let span = lines.span_at(func_def.range().start());
    let _param = validate_callback_parameters(&func_def.parameters, &span)?;
    let return_expr = extract_single_return_expr(&func_def.body, &span, lines)?;
    validate_callback_expr(return_expr, lines)
}

pub(crate) fn validate_lambda_callback(
    lambda: &ExprLambda,
    lines: &LineTable<'_>,
) -> Result<(), PyAuthoringError> {
    let span = lines.span_at(lambda.range().start());
    let Some(params) = lambda.parameters.as_deref() else {
        return Err(PyAuthoringError::spanned(
            CODE_FORBIDDEN_CONTROL_FLOW,
            "authoring callbacks must take exactly 1 parameter, got 0",
            span,
        ));
    };
    let _param = validate_callback_parameters(params, &span)?;
    validate_callback_expr(&lambda.body, lines)
}

pub(crate) fn validate_callback_parameters<'a>(
    params: &'a Parameters,
    span: &SourceSpan,
) -> Result<&'a str, PyAuthoringError> {
    let has_disallowed_shape = params.vararg.is_some()
        || params.kwarg.is_some()
        || !params.kwonlyargs.is_empty()
        || !params.posonlyargs.is_empty()
        || params.args.iter().any(|arg| arg.default.is_some());
    if has_disallowed_shape {
        return Err(PyAuthoringError::spanned(
            CODE_FORBIDDEN_CONTROL_FLOW,
            "authoring callbacks must take a single plain positional parameter (e.g. `s`)",
            span.clone(),
        ));
    }
    let positional_args: &[ParameterWithDefault] = &params.args;
    let [only_arg] = positional_args else {
        return Err(PyAuthoringError::spanned(
            CODE_FORBIDDEN_CONTROL_FLOW,
            format!(
                "authoring callbacks must take exactly 1 parameter, got {}",
                params.args.len()
            ),
            span.clone(),
        ));
    };
    let param_name = only_arg.parameter.name.id.as_str();
    if param_name.starts_with("__") || is_forbidden_name(param_name) {
        return Err(PyAuthoringError::spanned(
            CODE_FORBIDDEN_NAME,
            format!("parameter name '{param_name}' is forbidden in Norn callbacks"),
            span.clone(),
        ));
    }
    Ok(param_name)
}

pub(crate) fn extract_single_return_expr<'a>(
    body: &'a [Stmt],
    fn_span: &SourceSpan,
    lines: &LineTable<'_>,
) -> Result<&'a Expr, PyAuthoringError> {
    let effective = if let Some(Stmt::Expr(expr_stmt)) = body.first()
        && matches!(expr_stmt.value.as_ref(), Expr::StringLiteral(_))
    {
        body.get(1..).unwrap_or(&[])
    } else {
        body
    };

    for stmt in effective {
        let stmt_span = lines.span_at(stmt.range().start());
        match stmt {
            Stmt::If(_) => {
                return Err(PyAuthoringError::spanned(
                    CODE_FORBIDDEN_CONTROL_FLOW,
                    "Python `if`/`else` statements are forbidden in Norn callbacks; use `when(cond, yes, no)` / `if_else(cond, yes, no)` instead",
                    stmt_span,
                ));
            }
            Stmt::For(_) | Stmt::While(_) => {
                return Err(PyAuthoringError::spanned(
                    CODE_FORBIDDEN_CONTROL_FLOW,
                    "loops (`for`/`while`) are forbidden in Norn callbacks",
                    stmt_span,
                ));
            }
            Stmt::Try(_) | Stmt::With(_) => {
                return Err(PyAuthoringError::spanned(
                    CODE_FORBIDDEN_CONTROL_FLOW,
                    "`try`/`with` blocks are forbidden in Norn callbacks",
                    stmt_span,
                ));
            }
            Stmt::FunctionDef(_) | Stmt::ClassDef(_) => {
                return Err(PyAuthoringError::spanned(
                    CODE_FORBIDDEN_CONTROL_FLOW,
                    "nested function/class definitions are forbidden in Norn callbacks",
                    stmt_span,
                ));
            }
            Stmt::Return(_)
            | Stmt::Delete(_)
            | Stmt::TypeAlias(_)
            | Stmt::Assign(_)
            | Stmt::AugAssign(_)
            | Stmt::AnnAssign(_)
            | Stmt::Match(_)
            | Stmt::Raise(_)
            | Stmt::Assert(_)
            | Stmt::Import(_)
            | Stmt::ImportFrom(_)
            | Stmt::Global(_)
            | Stmt::Nonlocal(_)
            | Stmt::Expr(_)
            | Stmt::Pass(_)
            | Stmt::Break(_)
            | Stmt::Continue(_)
            | Stmt::IpyEscapeCommand(_) => {}
        }
    }

    if let [Stmt::Return(ret)] = effective
        && let Some(value) = ret.value.as_deref()
    {
        return Ok(value);
    }

    Err(PyAuthoringError::spanned(
        CODE_FORBIDDEN_CONTROL_FLOW,
        "authoring callback body must be a single `return <expr>` statement",
        fn_span.clone(),
    ))
}

pub(crate) fn validate_callback_expr(
    expr: &Expr,
    lines: &LineTable<'_>,
) -> Result<(), PyAuthoringError> {
    let span = lines.span_at(expr.range().start());
    match expr {
        Expr::If(_) => Err(PyAuthoringError::spanned(
            CODE_FORBIDDEN_CONTROL_FLOW,
            "Python conditional expression (`a if cond else b`) is forbidden in Norn callbacks; use `when(cond, yes, no)` / `if_else(cond, yes, no)` instead",
            span,
        )),
        Expr::BoolOp(bool_op) => validate_bool_op(bool_op.op, span),
        Expr::UnaryOp(unary) => validate_unary_op(unary, &span, lines),
        Expr::Compare(compare) => validate_compare(compare, &span, lines),
        Expr::BinOp(bin_op) => validate_bin_op(bin_op, &span, lines),
        Expr::Call(call) => validate_call(call, &span, lines),
        Expr::Attribute(attr) => validate_attribute(attr, &span, lines),
        Expr::Name(name) => validate_name(name.id.as_str(), span),
        Expr::NumberLiteral(num) => validate_number(&num.value, span),
        Expr::StringLiteral(_) | Expr::BooleanLiteral(_) | Expr::NoneLiteral(_) => Ok(()),
        Expr::Lambda(_) => Err(PyAuthoringError::spanned(
            CODE_FORBIDDEN_CONTROL_FLOW,
            "nested `lambda` expressions are forbidden in Norn callbacks",
            span,
        )),
        Expr::ListComp(_) | Expr::SetComp(_) | Expr::DictComp(_) | Expr::Generator(_) => {
            Err(PyAuthoringError::spanned(
                CODE_FORBIDDEN_CONTROL_FLOW,
                "comprehensions and generator expressions are forbidden in Norn callbacks",
                span,
            ))
        }
        Expr::Named(_)
        | Expr::Dict(_)
        | Expr::Set(_)
        | Expr::Await(_)
        | Expr::Yield(_)
        | Expr::YieldFrom(_)
        | Expr::FString(_)
        | Expr::TString(_)
        | Expr::BytesLiteral(_)
        | Expr::EllipsisLiteral(_)
        | Expr::Subscript(_)
        | Expr::Starred(_)
        | Expr::List(_)
        | Expr::Tuple(_)
        | Expr::Slice(_)
        | Expr::IpyEscapeCommand(_) => Err(PyAuthoringError::spanned(
            CODE_FORBIDDEN_CONTROL_FLOW,
            "unsupported syntax in Norn callback",
            span,
        )),
    }
}

fn validate_bool_op(op: BoolOp, span: SourceSpan) -> Result<(), PyAuthoringError> {
    let (op_name, replacement) = match op {
        BoolOp::And => ("and", "all_of(...)"),
        BoolOp::Or => ("or", "any_of(...)"),
    };
    Err(PyAuthoringError::spanned(
        CODE_FORBIDDEN_CONTROL_FLOW,
        format!(
            "Python `{op_name}` short-circuits and is forbidden in Norn callbacks; use `{replacement}` instead"
        ),
        span,
    ))
}

fn validate_unary_op(
    unary: &ruff_python_ast::ExprUnaryOp,
    span: &SourceSpan,
    lines: &LineTable<'_>,
) -> Result<(), PyAuthoringError> {
    match unary.op {
        UnaryOp::Not => Err(PyAuthoringError::spanned(
            CODE_FORBIDDEN_CONTROL_FLOW,
            "Python `not` coerces to bool and is forbidden in Norn callbacks; use `not_(...)` instead",
            span.clone(),
        )),
        UnaryOp::Invert | UnaryOp::USub | UnaryOp::UAdd => {
            validate_callback_expr(&unary.operand, lines)
        }
    }
}

fn validate_bin_op(
    bin_op: &ruff_python_ast::ExprBinOp,
    span: &SourceSpan,
    lines: &LineTable<'_>,
) -> Result<(), PyAuthoringError> {
    match bin_op.op {
        Operator::Add
        | Operator::Sub
        | Operator::Mult
        | Operator::Div
        | Operator::BitAnd
        | Operator::BitOr => {
            validate_callback_expr(&bin_op.left, lines)?;
            validate_callback_expr(&bin_op.right, lines)
        }
        Operator::MatMult
        | Operator::Mod
        | Operator::Pow
        | Operator::LShift
        | Operator::RShift
        | Operator::BitXor
        | Operator::FloorDiv => Err(PyAuthoringError::spanned(
            CODE_FORBIDDEN_CONTROL_FLOW,
            format!(
                "binary operator {:?} is not admitted in Norn callbacks",
                bin_op.op
            ),
            span.clone(),
        )),
    }
}

fn validate_attribute(
    attr: &ruff_python_ast::ExprAttribute,
    span: &SourceSpan,
    lines: &LineTable<'_>,
) -> Result<(), PyAuthoringError> {
    let name = attr.attr.id.as_str();
    if name.starts_with("__") || FORBIDDEN_INTERNAL_ATTRS.contains(&name) {
        return Err(PyAuthoringError::spanned(
            CODE_FORBIDDEN_ATTR,
            format!("attribute access .{name} is forbidden in Norn callbacks"),
            span.clone(),
        ));
    }
    validate_callback_expr(&attr.value, lines)
}

fn validate_name(id: &str, span: SourceSpan) -> Result<(), PyAuthoringError> {
    if is_forbidden_name(id) {
        return Err(PyAuthoringError::spanned(
            CODE_FORBIDDEN_CALL,
            format!("reference to '{id}' is forbidden in Norn callbacks"),
            span,
        ));
    }
    if id.starts_with("__") {
        return Err(PyAuthoringError::spanned(
            CODE_FORBIDDEN_NAME,
            format!("dunder name '{id}' is forbidden in Norn callbacks"),
            span,
        ));
    }
    Ok(())
}

fn validate_number(number: &Number, span: SourceSpan) -> Result<(), PyAuthoringError> {
    match number {
        Number::Int(_) => Ok(()),
        Number::Float(val) => Err(PyAuthoringError::spanned(
            CODE_FLOAT_LITERAL,
            format!("float literal {val} is refused (VAL-EXACT-1); use Decimal('{val}')"),
            span,
        )),
        Number::Complex { .. } => Err(PyAuthoringError::spanned(
            CODE_FLOAT_LITERAL,
            "complex literal is refused (VAL-EXACT-1); use exact Decimal instead",
            span,
        )),
    }
}

fn validate_compare(
    compare: &ExprCompare,
    span: &SourceSpan,
    lines: &LineTable<'_>,
) -> Result<(), PyAuthoringError> {
    let Some((left, op, right)) = compare.as_single() else {
        return Err(PyAuthoringError::spanned(
            CODE_FORBIDDEN_CONTROL_FLOW,
            "chained comparisons are forbidden in Norn callbacks; use `between(...)` or `all_of(...)` instead",
            span.clone(),
        ));
    };
    match op {
        CmpOp::Is | CmpOp::IsNot => Err(PyAuthoringError::spanned(
            CODE_FORBIDDEN_CONTROL_FLOW,
            "Python `is` / `is not` is forbidden in Norn callbacks; use `.is_null()` / `.is_not_null()` instead",
            span.clone(),
        )),
        CmpOp::In | CmpOp::NotIn => Err(PyAuthoringError::spanned(
            CODE_FORBIDDEN_CONTROL_FLOW,
            "Python `in` / `not in` is forbidden in Norn callbacks; use `contains(...)` instead",
            span.clone(),
        )),
        CmpOp::Eq | CmpOp::NotEq | CmpOp::Lt | CmpOp::LtE | CmpOp::Gt | CmpOp::GtE => {
            validate_callback_expr(left, lines)?;
            validate_callback_expr(right, lines)
        }
    }
}

fn validate_call(
    call: &ExprCall,
    span: &SourceSpan,
    lines: &LineTable<'_>,
) -> Result<(), PyAuthoringError> {
    for arg in &call.arguments.args {
        if matches!(arg, Expr::Starred(_)) {
            return Err(PyAuthoringError::spanned(
                CODE_FORBIDDEN_CONTROL_FLOW,
                "starred `*args` unpacking is forbidden in Norn callbacks",
                span.clone(),
            ));
        }
    }
    for kw in &call.arguments.keywords {
        if kw.arg.is_none() {
            return Err(PyAuthoringError::spanned(
                CODE_FORBIDDEN_CONTROL_FLOW,
                "`**kwargs` unpacking is forbidden in Norn callbacks",
                span.clone(),
            ));
        }
    }

    if let Expr::Name(name) = call.func.as_ref() {
        let fn_name = name.id.as_str();
        if !ALLOWED_CALLBACK_FUNCS.contains(&fn_name) {
            return Err(PyAuthoringError::spanned(
                CODE_FORBIDDEN_CALL,
                format!(
                    "call to {fn_name}() is outside the closed Norn expression operator set (allowed: {ALLOWED_CALLBACK_FUNCS:?})"
                ),
                span.clone(),
            ));
        }
    } else if let Expr::Attribute(attr) = call.func.as_ref() {
        let method = attr.attr.id.as_str();
        if method.starts_with("__") || FORBIDDEN_INTERNAL_ATTRS.contains(&method) {
            return Err(PyAuthoringError::spanned(
                CODE_FORBIDDEN_ATTR,
                format!("attribute access .{method} is forbidden in Norn callbacks"),
                span.clone(),
            ));
        }
        if !ALLOWED_CALLBACK_METHODS.contains(&method) {
            return Err(PyAuthoringError::spanned(
                CODE_FORBIDDEN_CALL,
                format!(
                    "method .{method}() is outside the closed Norn expression method set (allowed: {ALLOWED_CALLBACK_METHODS:?})"
                ),
                span.clone(),
            ));
        }
        validate_callback_expr(&attr.value, lines)?;
    } else {
        return Err(PyAuthoringError::spanned(
            CODE_FORBIDDEN_CALL,
            "dynamic call targets are forbidden in Norn callbacks",
            span.clone(),
        ));
    }

    for arg in &call.arguments.args {
        validate_callback_expr(arg, lines)?;
    }
    for kw in &call.arguments.keywords {
        validate_callback_expr(&kw.value, lines)?;
    }
    Ok(())
}
