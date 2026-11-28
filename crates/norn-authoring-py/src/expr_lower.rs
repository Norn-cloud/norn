//! Direct Python callback AST-to-`ZenExpr` lowering with zero Python execution.

use std::collections::BTreeMap;

use ruff_python_ast::{
    CmpOp, Expr, ExprCall, ExprCompare, ExprLambda, Number, Operator, Stmt, StmtFunctionDef,
    UnaryOp,
};
use ruff_python_parser::{parse_expression, parse_module};
use ruff_text_size::Ranged;

use crate::budget::{check_body_budget, check_expr_budget};
use crate::decimal::format_exact_decimal;
use crate::error::{
    CODE_EXPRESSION, CODE_FLOAT_LITERAL, CODE_FORBIDDEN_CALL, CODE_FORBIDDEN_CONTROL_FLOW,
    CODE_RAW_EXPR, CODE_SYNTAX, CODE_UNBOUNDED, CODE_UNKNOWN_FIELD, PyAuthoringError, SourceSpan,
};
use crate::expr::{
    BinaryOp, CompareOp, LogicalOp, LowerContext, ZenExpr, ZenLiteral, check_valid_ident,
    render_zen_string,
};
use crate::expr_validate::{
    extract_single_return_expr, validate_callback_expr, validate_callback_parameters,
};
use crate::line_table::LineTable;
use crate::sandbox::{SandboxLimits, parse_and_validate_module};

/// Distinguishes closed symbolic `Expr` nodes from raw Python scalar literals,
/// matching `tools/norn-py/norn_py/expr.py` (`_require_expr` vs `_coerce_operand`).
enum LoweredValue {
    /// Closed symbolic expression (`s.field`, `lit(...)`, combinator/operator result).
    Expr(ZenExpr),
    /// Raw Python scalar literal (`None`, `bool`, `int`, `str`, `Decimal(...)`, enum constant).
    Raw(ZenExpr),
}

impl LoweredValue {
    fn into_coerced(self) -> ZenExpr {
        match self {
            Self::Expr(expr) | Self::Raw(expr) => expr,
        }
    }

    fn require_expr(self, context: &str, span: &SourceSpan) -> Result<ZenExpr, PyAuthoringError> {
        match self {
            Self::Expr(expr) => Ok(expr),
            Self::Raw(_) => Err(PyAuthoringError::spanned(
                CODE_RAW_EXPR,
                format!(
                    "{context} must be a closed norn_py Expr node, not a raw Python literal (wrap with `lit(...)` if a constant expression is intended)"
                ),
                span.clone(),
            )),
        }
    }
}

/// Validates and lowers a standalone Python `def` or `lambda` callback directly
/// to [`ZenExpr`] with zero Python execution under default [`SandboxLimits`].
///
/// # Errors
///
/// Returns [`PyAuthoringError`] on budget exhaustion, syntax errors, sandbox/callback
/// profile violations, raw-literal callback returns, undeclared fields, or invalid ZEN expressions.
pub fn lower_callback_source(
    source: &str,
    filename: &str,
    ctx: &LowerContext,
) -> Result<ZenExpr, PyAuthoringError> {
    lower_callback_source_with_limits(source, filename, ctx, SandboxLimits::default())
}

/// Validates and lowers a standalone Python `def` or `lambda` callback directly
/// to [`ZenExpr`] with explicit [`SandboxLimits`] enforced before recursion.
///
/// # Errors
///
/// Returns [`PyAuthoringError`] on budget exhaustion, syntax errors, sandbox/callback
/// profile violations, raw-literal callback returns, undeclared fields, or invalid ZEN expressions.
pub fn lower_callback_source_with_limits(
    source: &str,
    filename: &str,
    ctx: &LowerContext,
    limits: SandboxLimits,
) -> Result<ZenExpr, PyAuthoringError> {
    if source.len() > limits.max_source_bytes {
        return Err(PyAuthoringError::spanned(
            CODE_UNBOUNDED,
            format!("source exceeds {} byte limit", limits.max_source_bytes),
            SourceSpan::new(filename, 1, 0),
        ));
    }
    let lines = LineTable::new(filename, source);
    if let Ok(parsed_expr) = parse_expression(source)
        && parsed_expr.unsupported_syntax_errors().is_empty()
        && let Expr::Lambda(lambda) = parsed_expr.expr()
    {
        let _stats = check_expr_budget(parsed_expr.expr(), &lines, limits)?;
        return lower_lambda(lambda, &lines, ctx);
    }

    let parsed_mod = parse_module(source).map_err(|err| {
        PyAuthoringError::spanned(
            CODE_SYNTAX,
            err.error.to_string(),
            lines.span_at(err.location.start()),
        )
    })?;
    if let Some(unsupported) = parsed_mod.unsupported_syntax_errors().first() {
        return Err(PyAuthoringError::spanned(
            CODE_SYNTAX,
            unsupported.to_string(),
            lines.span_at(unsupported.range().start()),
        ));
    }
    let body: &[Stmt] = parsed_mod.suite();
    let _stats = check_body_budget(body, &lines, limits)?;
    match body {
        [Stmt::FunctionDef(func_def)] => lower_function_def(func_def, &lines, ctx),
        [Stmt::Expr(expr_stmt)] => {
            if let Expr::Lambda(lambda) = expr_stmt.value.as_ref() {
                lower_lambda(lambda, &lines, ctx)
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

/// Runs full Layer-1 preflight on a Python authoring module and lowers every
/// top-level function and class method callback directly to [`ZenExpr`].
///
/// Top-level functions are keyed by `fn_name`; class methods (such as
/// `@invariant` methods on a `Concept` class) are keyed by `ClassName.method_name`.
///
/// # Errors
///
/// Returns [`PyAuthoringError`] if preflight fails or any callback fails lowering.
pub fn lower_module_callbacks(
    source: &str,
    filename: &str,
    ctx: &LowerContext,
) -> Result<BTreeMap<String, ZenExpr>, PyAuthoringError> {
    let lines = LineTable::new(filename, source);
    let (module, _report) = parse_and_validate_module(source, &lines, SandboxLimits::default())?;

    let mut lowered = BTreeMap::new();
    for stmt in &module.body {
        match stmt {
            Stmt::FunctionDef(func_def) => {
                let expr = lower_function_def(func_def, &lines, ctx)?;
                lowered.insert(func_def.name.id.as_str().to_owned(), expr);
            }
            Stmt::ClassDef(class_def) => {
                let class_name = class_def.name.id.as_str();
                for item in &class_def.body {
                    if let Stmt::FunctionDef(method) = item {
                        let expr = lower_function_def(method, &lines, ctx)?;
                        lowered.insert(format!("{class_name}.{}", method.name.id.as_str()), expr);
                    }
                }
            }
            Stmt::Return(_)
            | Stmt::Delete(_)
            | Stmt::TypeAlias(_)
            | Stmt::Assign(_)
            | Stmt::AugAssign(_)
            | Stmt::AnnAssign(_)
            | Stmt::For(_)
            | Stmt::While(_)
            | Stmt::If(_)
            | Stmt::With(_)
            | Stmt::Match(_)
            | Stmt::Raise(_)
            | Stmt::Try(_)
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
    Ok(lowered)
}

fn lower_function_def(
    func_def: &StmtFunctionDef,
    lines: &LineTable<'_>,
    ctx: &LowerContext,
) -> Result<ZenExpr, PyAuthoringError> {
    let span = lines.span_at(func_def.range().start());
    let param_name = validate_callback_parameters(&func_def.parameters, &span)?;
    let return_expr = extract_single_return_expr(&func_def.body, &span, lines)?;
    validate_callback_expr(return_expr, lines)?;
    check_valid_ident(&ctx.root, Some(&span))?;
    let lowered = lower_expr(return_expr, param_name, lines, ctx)?;
    let ret_span = lines.span_at(return_expr.range().start());
    lowered.require_expr("callback return value", &ret_span)
}

fn lower_lambda(
    lambda: &ExprLambda,
    lines: &LineTable<'_>,
    ctx: &LowerContext,
) -> Result<ZenExpr, PyAuthoringError> {
    let span = lines.span_at(lambda.range().start());
    let Some(params) = lambda.parameters.as_deref() else {
        return Err(PyAuthoringError::spanned(
            CODE_FORBIDDEN_CONTROL_FLOW,
            "authoring callbacks must take exactly 1 parameter, got 0",
            span,
        ));
    };
    let param_name = validate_callback_parameters(params, &span)?;
    validate_callback_expr(&lambda.body, lines)?;
    check_valid_ident(&ctx.root, Some(&span))?;
    let lowered = lower_expr(&lambda.body, param_name, lines, ctx)?;
    let body_span = lines.span_at(lambda.body.range().start());
    lowered.require_expr("callback return value", &body_span)
}

fn lower_expr(
    expr: &Expr,
    param_name: &str,
    lines: &LineTable<'_>,
    ctx: &LowerContext,
) -> Result<LoweredValue, PyAuthoringError> {
    let span = lines.span_at(expr.range().start());
    match expr {
        Expr::NoneLiteral(_) => Ok(LoweredValue::Raw(ZenExpr::Literal(ZenLiteral::Null))),
        Expr::BooleanLiteral(b) => Ok(LoweredValue::Raw(ZenExpr::Literal(ZenLiteral::Bool(
            b.value,
        )))),
        Expr::NumberLiteral(num) => match &num.value {
            Number::Int(val) => Ok(LoweredValue::Raw(ZenExpr::Literal(ZenLiteral::Int(
                val.to_string(),
            )))),
            Number::Float(val) => Err(PyAuthoringError::spanned(
                CODE_FLOAT_LITERAL,
                format!("float literal {val} is refused (VAL-EXACT-1); use Decimal('{val}')"),
                span,
            )),
            Number::Complex { .. } => Err(PyAuthoringError::spanned(
                CODE_FLOAT_LITERAL,
                "complex literal is refused (VAL-EXACT-1)",
                span,
            )),
        },
        Expr::StringLiteral(s) => {
            let text = s.value.to_str();
            let _validated = render_zen_string(text, Some(span))?;
            Ok(LoweredValue::Raw(ZenExpr::Literal(ZenLiteral::Str(
                text.to_owned(),
            ))))
        }
        Expr::Name(name) => {
            let id = name.id.as_str();
            if id == param_name {
                return Ok(LoweredValue::Expr(ZenExpr::Ref {
                    root: ctx.root.clone(),
                    path: Vec::new(),
                }));
            }
            if let Some(constant) = ctx.constants.get(id) {
                return Ok(LoweredValue::Raw(ZenExpr::Literal(constant.clone())));
            }
            Err(PyAuthoringError::spanned(
                CODE_EXPRESSION,
                format!(
                    "unbound identifier '{id}' in callback (expected parameter '{param_name}' or a declared constant)"
                ),
                span,
            ))
        }
        Expr::Attribute(attr) => lower_attribute(attr, param_name, &span, lines, ctx),
        Expr::UnaryOp(unary) => lower_unary(unary, param_name, &span, lines, ctx),
        Expr::BinOp(bin_op) => lower_bin_op(bin_op, param_name, &span, lines, ctx),
        Expr::Compare(compare) => lower_compare(compare, param_name, &span, lines, ctx),
        Expr::Call(call) => lower_call(call, param_name, &span, lines, ctx),
        Expr::If(_)
        | Expr::BoolOp(_)
        | Expr::Lambda(_)
        | Expr::ListComp(_)
        | Expr::SetComp(_)
        | Expr::DictComp(_)
        | Expr::Generator(_)
        | Expr::Named(_)
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
            "unsupported expression in Norn callback lowering",
            span,
        )),
    }
}

fn lower_attribute(
    attr: &ruff_python_ast::ExprAttribute,
    param_name: &str,
    span: &SourceSpan,
    lines: &LineTable<'_>,
    ctx: &LowerContext,
) -> Result<LoweredValue, PyAuthoringError> {
    if let Expr::Name(base_name) = attr.value.as_ref() {
        let base = base_name.id.as_str();
        let field = attr.attr.id.as_str();
        if base == param_name {
            check_valid_ident(field, Some(span))?;
            if let Some(allowed) = &ctx.allowed_fields
                && !allowed.contains(field)
            {
                return Err(PyAuthoringError::spanned(
                    CODE_UNKNOWN_FIELD,
                    format!("undeclared field '{field}' on symbolic parameter '{param_name}'"),
                    span.clone(),
                ));
            }
            return Ok(LoweredValue::Expr(ZenExpr::Ref {
                root: ctx.root.clone(),
                path: vec![field.to_owned()],
            }));
        }
        let qualified = format!("{base}.{field}");
        if let Some(constant) = ctx.constants.get(&qualified) {
            return Ok(LoweredValue::Raw(ZenExpr::Literal(constant.clone())));
        }
    }

    let base_expr =
        lower_expr(&attr.value, param_name, lines, ctx)?.require_expr("attribute target", span)?;
    if let ZenExpr::Ref { root, mut path } = base_expr {
        let seg = attr.attr.id.as_str();
        check_valid_ident(seg, Some(span))?;
        path.push(seg.to_owned());
        return Ok(LoweredValue::Expr(ZenExpr::Ref { root, path }));
    }

    Err(PyAuthoringError::spanned(
        CODE_EXPRESSION,
        format!(
            "attribute access '.{}' is only supported on symbolic field paths or registered enum constants",
            attr.attr.id.as_str()
        ),
        span.clone(),
    ))
}

fn lower_unary(
    unary: &ruff_python_ast::ExprUnaryOp,
    param_name: &str,
    span: &SourceSpan,
    lines: &LineTable<'_>,
    ctx: &LowerContext,
) -> Result<LoweredValue, PyAuthoringError> {
    let operand = lower_expr(&unary.operand, param_name, lines, ctx)?;
    match unary.op {
        UnaryOp::UAdd => Ok(operand),
        UnaryOp::USub => match operand {
            LoweredValue::Expr(expr) => Ok(LoweredValue::Expr(ZenExpr::UnaryNeg(Box::new(expr)))),
            LoweredValue::Raw(expr) => Ok(LoweredValue::Raw(ZenExpr::UnaryNeg(Box::new(expr)))),
        },
        UnaryOp::Invert => {
            let expr = operand.require_expr("unary `~` operand", span)?;
            Ok(LoweredValue::Expr(ZenExpr::Not(Box::new(expr))))
        }
        UnaryOp::Not => Err(PyAuthoringError::spanned(
            CODE_FORBIDDEN_CONTROL_FLOW,
            "Python `not` is forbidden in Norn callbacks; use `not_(...)` instead",
            span.clone(),
        )),
    }
}

fn lower_bin_op(
    bin_op: &ruff_python_ast::ExprBinOp,
    param_name: &str,
    span: &SourceSpan,
    lines: &LineTable<'_>,
    ctx: &LowerContext,
) -> Result<LoweredValue, PyAuthoringError> {
    let left_val = lower_expr(&bin_op.left, param_name, lines, ctx)?;
    let right_val = lower_expr(&bin_op.right, param_name, lines, ctx)?;
    match bin_op.op {
        Operator::BitAnd | Operator::BitOr => {
            let left = left_val.require_expr("logical operator left operand", span)?;
            let right = right_val.require_expr("logical operator right operand", span)?;
            let op = if matches!(bin_op.op, Operator::BitAnd) {
                LogicalOp::And
            } else {
                LogicalOp::Or
            };
            Ok(LoweredValue::Expr(ZenExpr::Logical {
                op,
                left: Box::new(left),
                right: Box::new(right),
            }))
        }
        Operator::Add | Operator::Sub | Operator::Mult | Operator::Div => {
            let (left, right) = match (left_val, right_val) {
                (LoweredValue::Expr(l), r) => (l, r.into_coerced()),
                (l, LoweredValue::Expr(r)) => (l.into_coerced(), r),
                (LoweredValue::Raw(_), LoweredValue::Raw(_)) => {
                    return Err(PyAuthoringError::spanned(
                        CODE_RAW_EXPR,
                        "arithmetic expression requires at least one closed norn_py Expr operand",
                        span.clone(),
                    ));
                }
            };
            let op = match bin_op.op {
                Operator::Add => BinaryOp::Add,
                Operator::Sub => BinaryOp::Sub,
                Operator::Mult => BinaryOp::Mul,
                Operator::Div => BinaryOp::Div,
                Operator::MatMult
                | Operator::Mod
                | Operator::Pow
                | Operator::LShift
                | Operator::RShift
                | Operator::BitOr
                | Operator::BitXor
                | Operator::BitAnd
                | Operator::FloorDiv => {
                    return Err(PyAuthoringError::spanned(
                        CODE_FORBIDDEN_CONTROL_FLOW,
                        format!(
                            "binary operator {:?} is not admitted in Norn callbacks",
                            bin_op.op
                        ),
                        span.clone(),
                    ));
                }
            };
            Ok(LoweredValue::Expr(ZenExpr::Binary {
                op,
                left: Box::new(left),
                right: Box::new(right),
            }))
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

fn lower_compare(
    compare: &ExprCompare,
    param_name: &str,
    span: &SourceSpan,
    lines: &LineTable<'_>,
    ctx: &LowerContext,
) -> Result<LoweredValue, PyAuthoringError> {
    let Some((left_ast, op_ast, right_ast)) = compare.as_single() else {
        return Err(PyAuthoringError::spanned(
            CODE_FORBIDDEN_CONTROL_FLOW,
            "chained comparisons are forbidden in Norn callbacks",
            span.clone(),
        ));
    };
    let op = match op_ast {
        CmpOp::Eq => CompareOp::Eq,
        CmpOp::NotEq => CompareOp::Ne,
        CmpOp::Lt => CompareOp::Lt,
        CmpOp::LtE => CompareOp::Le,
        CmpOp::Gt => CompareOp::Gt,
        CmpOp::GtE => CompareOp::Ge,
        CmpOp::Is | CmpOp::IsNot | CmpOp::In | CmpOp::NotIn => {
            return Err(PyAuthoringError::spanned(
                CODE_FORBIDDEN_CONTROL_FLOW,
                "unsupported comparison operator in Norn callback",
                span.clone(),
            ));
        }
    };
    let left_val = lower_expr(left_ast, param_name, lines, ctx)?;
    let right_val = lower_expr(right_ast, param_name, lines, ctx)?;
    let (left, right) = match (left_val, right_val) {
        (LoweredValue::Expr(l), r) => (l, r.into_coerced()),
        (l, LoweredValue::Expr(r)) => (l.into_coerced(), r),
        (LoweredValue::Raw(_), LoweredValue::Raw(_)) => {
            return Err(PyAuthoringError::spanned(
                CODE_RAW_EXPR,
                "comparison requires at least one closed norn_py Expr operand",
                span.clone(),
            ));
        }
    };
    Ok(LoweredValue::Expr(ZenExpr::Compare {
        op,
        left: Box::new(left),
        right: Box::new(right),
    }))
}

fn lower_call(
    call: &ExprCall,
    param_name: &str,
    span: &SourceSpan,
    lines: &LineTable<'_>,
    ctx: &LowerContext,
) -> Result<LoweredValue, PyAuthoringError> {
    if let Expr::Attribute(attr) = call.func.as_ref() {
        lower_method_call(attr, call, param_name, span, lines, ctx)
    } else if let Expr::Name(name) = call.func.as_ref() {
        lower_func_call(name.id.as_str(), call, param_name, span, lines, ctx)
    } else {
        Err(PyAuthoringError::spanned(
            CODE_FORBIDDEN_CALL,
            "dynamic call targets are forbidden in Norn callbacks",
            span.clone(),
        ))
    }
}

fn lower_method_call(
    attr: &ruff_python_ast::ExprAttribute,
    call: &ExprCall,
    param_name: &str,
    span: &SourceSpan,
    lines: &LineTable<'_>,
    ctx: &LowerContext,
) -> Result<LoweredValue, PyAuthoringError> {
    let method = attr.attr.id.as_str();
    if !call.arguments.keywords.is_empty() {
        return Err(PyAuthoringError::spanned(
            CODE_EXPRESSION,
            format!("method .{method}() does not accept keyword arguments"),
            span.clone(),
        ));
    }
    let receiver = lower_expr(&attr.value, param_name, lines, ctx)?
        .require_expr(&format!("method .{method}() receiver"), span)?;
    match method {
        "eq" | "ne" => {
            let call_args: &[Expr] = &call.arguments.args;
            let [rhs_ast] = call_args else {
                return Err(PyAuthoringError::spanned(
                    CODE_EXPRESSION,
                    format!("method .{method}() requires exactly 1 argument"),
                    span.clone(),
                ));
            };
            let rhs = lower_expr(rhs_ast, param_name, lines, ctx)?.into_coerced();
            let op = if method == "eq" {
                CompareOp::Eq
            } else {
                CompareOp::Ne
            };
            Ok(LoweredValue::Expr(ZenExpr::Compare {
                op,
                left: Box::new(receiver),
                right: Box::new(rhs),
            }))
        }
        "is_null" | "is_not_null" => {
            if !call.arguments.args.is_empty() {
                return Err(PyAuthoringError::spanned(
                    CODE_EXPRESSION,
                    format!("method .{method}() takes no arguments"),
                    span.clone(),
                ));
            }
            let op = if method == "is_null" {
                CompareOp::Eq
            } else {
                CompareOp::Ne
            };
            Ok(LoweredValue::Expr(ZenExpr::Compare {
                op,
                left: Box::new(receiver),
                right: Box::new(ZenExpr::Literal(ZenLiteral::Null)),
            }))
        }
        _ => Err(PyAuthoringError::spanned(
            CODE_FORBIDDEN_CALL,
            format!("method .{method}() is outside the closed Norn expression method set"),
            span.clone(),
        )),
    }
}

fn lower_func_call(
    fn_name: &str,
    call: &ExprCall,
    param_name: &str,
    span: &SourceSpan,
    lines: &LineTable<'_>,
    ctx: &LowerContext,
) -> Result<LoweredValue, PyAuthoringError> {
    if fn_name == "Decimal" {
        return lower_decimal_call(call, span);
    }
    if fn_name == "between" {
        return lower_between_call(call, param_name, span, lines, ctx);
    }
    if !call.arguments.keywords.is_empty() {
        return Err(PyAuthoringError::spanned(
            CODE_EXPRESSION,
            format!("{fn_name}() does not accept keyword arguments"),
            span.clone(),
        ));
    }

    let mut args = Vec::with_capacity(call.arguments.args.len());
    for arg in &call.arguments.args {
        args.push(lower_expr(arg, param_name, lines, ctx)?);
    }

    let expr = match fn_name {
        "lit" => {
            let [arg] = take_fixed_args::<1>(args, fn_name, span)?;
            arg.into_coerced()
        }
        "all_of" => fold_logical(LogicalOp::And, fn_name, args, span)?,
        "any_of" => fold_logical(LogicalOp::Or, fn_name, args, span)?,
        "not_" => {
            let [arg] = take_fixed_args::<1>(args, fn_name, span)?;
            ZenExpr::Not(Box::new(arg.require_expr("not_() argument", span)?))
        }
        "when" | "if_else" => {
            let [cond, yes, no] = take_fixed_args::<3>(args, fn_name, span)?;
            ZenExpr::Conditional {
                condition: Box::new(cond.require_expr(&format!("{fn_name}() condition"), span)?),
                yes: Box::new(yes.into_coerced()),
                no: Box::new(no.into_coerced()),
            }
        }
        "norn_len" => {
            let [arg] = take_fixed_args::<1>(args, fn_name, span)?;
            ZenExpr::Call {
                fn_name: "len".to_owned(),
                args: vec![arg.require_expr("norn_len() argument", span)?],
            }
        }
        "contains" => {
            let [haystack, needle] = take_fixed_args::<2>(args, fn_name, span)?;
            ZenExpr::Call {
                fn_name: "contains".to_owned(),
                args: vec![
                    haystack.require_expr("contains() haystack", span)?,
                    needle.into_coerced(),
                ],
            }
        }
        _ => lower_temporal_func_call(fn_name, args, span)?,
    };
    Ok(LoweredValue::Expr(expr))
}

fn lower_temporal_func_call(
    fn_name: &str,
    args: Vec<LoweredValue>,
    span: &SourceSpan,
) -> Result<ZenExpr, PyAuthoringError> {
    match fn_name {
        "parse_date" | "parse_timestamp" => {
            let [arg] = take_fixed_args::<1>(args, fn_name, span)?;
            let zen_fn = if fn_name == "parse_date" {
                "date"
            } else {
                "timestamp"
            };
            Ok(ZenExpr::Call {
                fn_name: zen_fn.to_owned(),
                args: vec![arg.into_coerced()],
            })
        }
        "date_before" => {
            let [left, right] = take_fixed_args::<2>(args, fn_name, span)?;
            Ok(ZenExpr::Call {
                fn_name: "dateBefore".to_owned(),
                args: vec![
                    left.require_expr("date_before() left", span)?,
                    right.require_expr("date_before() right", span)?,
                ],
            })
        }
        "timestamp_before" | "timestamp_at_or_before" | "seconds_between" => {
            let [left, right] = take_fixed_args::<2>(args, fn_name, span)?;
            let zen_fn = match fn_name {
                "timestamp_before" => "timestampBefore",
                "timestamp_at_or_before" => "timestampAtOrBefore",
                _ => "seconds_between",
            };
            let ctx_l = format!("{fn_name}() first argument");
            let ctx_r = format!("{fn_name}() second argument");
            Ok(ZenExpr::Call {
                fn_name: zen_fn.to_owned(),
                args: vec![
                    left.require_expr(&ctx_l, span)?.as_timestamp(),
                    right.require_expr(&ctx_r, span)?.as_timestamp(),
                ],
            })
        }
        "days_between" => {
            let [start, end] = take_fixed_args::<2>(args, fn_name, span)?;
            Ok(ZenExpr::Call {
                fn_name: "days_between".to_owned(),
                args: vec![
                    start.require_expr("days_between() start", span)?.as_date(),
                    end.require_expr("days_between() end", span)?.as_date(),
                ],
            })
        }
        "add_days" | "add_seconds" => lower_temporal_add(fn_name, args, span),
        "intervals_overlap" | "interval_gap_seconds" | "interval_intersection_seconds" => {
            lower_interval_call(fn_name, args, span)
        }
        _ => Err(PyAuthoringError::spanned(
            CODE_FORBIDDEN_CALL,
            format!("call to {fn_name}() is outside the closed Norn expression operator set"),
            span.clone(),
        )),
    }
}

fn lower_temporal_add(
    fn_name: &str,
    args: Vec<LoweredValue>,
    span: &SourceSpan,
) -> Result<ZenExpr, PyAuthoringError> {
    let [start, delta] = take_fixed_args::<2>(args, fn_name, span)?;
    let start_expr = start.require_expr(&format!("{fn_name}() start"), span)?;
    let wrapped = if fn_name == "add_days" {
        start_expr.as_date()
    } else {
        start_expr.as_timestamp()
    };
    Ok(ZenExpr::Call {
        fn_name: fn_name.to_owned(),
        args: vec![wrapped, delta.into_coerced()],
    })
}

fn lower_interval_call(
    fn_name: &str,
    args: Vec<LoweredValue>,
    span: &SourceSpan,
) -> Result<ZenExpr, PyAuthoringError> {
    let [a_start, a_end, b_start, b_end] = take_fixed_args::<4>(args, fn_name, span)?;
    let ctx_label = format!("{fn_name}() arg");
    Ok(ZenExpr::Call {
        fn_name: fn_name.to_owned(),
        args: vec![
            a_start.require_expr(&ctx_label, span)?.as_timestamp(),
            a_end.require_expr(&ctx_label, span)?.as_timestamp(),
            b_start.require_expr(&ctx_label, span)?.as_timestamp(),
            b_end.require_expr(&ctx_label, span)?.as_timestamp(),
        ],
    })
}

fn lower_decimal_call(
    call: &ExprCall,
    span: &SourceSpan,
) -> Result<LoweredValue, PyAuthoringError> {
    if !call.arguments.keywords.is_empty() {
        return Err(PyAuthoringError::spanned(
            CODE_EXPRESSION,
            "Decimal() does not accept keyword arguments",
            span.clone(),
        ));
    }
    let call_args: &[Expr] = &call.arguments.args;
    let [arg] = call_args else {
        return Err(PyAuthoringError::spanned(
            CODE_EXPRESSION,
            "Decimal() requires exactly 1 literal argument",
            span.clone(),
        ));
    };
    let raw = if let Expr::StringLiteral(s) = arg {
        s.value.to_str().to_owned()
    } else if let Expr::NumberLiteral(num) = arg {
        match &num.value {
            Number::Int(val) => val.to_string(),
            Number::Float(val) => {
                return Err(PyAuthoringError::spanned(
                    CODE_FLOAT_LITERAL,
                    format!(
                        "float literal {val} is refused (VAL-EXACT-1); pass an exact string to Decimal()"
                    ),
                    span.clone(),
                ));
            }
            Number::Complex { .. } => {
                return Err(PyAuthoringError::spanned(
                    CODE_FLOAT_LITERAL,
                    "complex literal is refused (VAL-EXACT-1)",
                    span.clone(),
                ));
            }
        }
    } else if let Expr::UnaryOp(unary) = arg {
        let Expr::NumberLiteral(num) = unary.operand.as_ref() else {
            return Err(PyAuthoringError::spanned(
                CODE_EXPRESSION,
                "Decimal() argument must be a string or integer literal",
                span.clone(),
            ));
        };
        let inner = match &num.value {
            Number::Int(val) => val.to_string(),
            Number::Float(_) | Number::Complex { .. } => {
                return Err(PyAuthoringError::spanned(
                    CODE_FLOAT_LITERAL,
                    "float/complex literal is refused in Decimal()",
                    span.clone(),
                ));
            }
        };
        match unary.op {
            UnaryOp::USub => format!("-{inner}"),
            UnaryOp::UAdd => inner,
            UnaryOp::Not | UnaryOp::Invert => {
                return Err(PyAuthoringError::spanned(
                    CODE_EXPRESSION,
                    "Decimal() argument must be a string or integer literal",
                    span.clone(),
                ));
            }
        }
    } else {
        return Err(PyAuthoringError::spanned(
            CODE_EXPRESSION,
            "Decimal() argument must be a string or integer literal",
            span.clone(),
        ));
    };
    let formatted = format_exact_decimal(&raw, Some(span.clone()))?;
    Ok(LoweredValue::Raw(ZenExpr::Literal(ZenLiteral::Decimal(
        formatted,
    ))))
}

fn lower_between_call(
    call: &ExprCall,
    param_name: &str,
    span: &SourceSpan,
    lines: &LineTable<'_>,
    ctx: &LowerContext,
) -> Result<LoweredValue, PyAuthoringError> {
    let mut inclusive = true;
    for kw in &call.arguments.keywords {
        let Some(arg_name) = kw.arg.as_ref() else {
            return Err(PyAuthoringError::spanned(
                CODE_FORBIDDEN_CONTROL_FLOW,
                "`**kwargs` unpacking is forbidden in Norn callbacks",
                span.clone(),
            ));
        };
        if arg_name.id.as_str() != "inclusive" {
            return Err(PyAuthoringError::spanned(
                CODE_EXPRESSION,
                format!(
                    "unexpected keyword argument '{}' in between()",
                    arg_name.id.as_str()
                ),
                span.clone(),
            ));
        }
        if let Expr::BooleanLiteral(b) = &kw.value {
            inclusive = b.value;
        } else {
            return Err(PyAuthoringError::spanned(
                CODE_EXPRESSION,
                "between(..., inclusive=...) requires a boolean literal",
                span.clone(),
            ));
        }
    }

    let mut args = Vec::with_capacity(call.arguments.args.len());
    for arg in &call.arguments.args {
        args.push(lower_expr(arg, param_name, lines, ctx)?);
    }
    let [val_raw, low_raw, high_raw] = take_fixed_args::<3>(args, "between", span)?;
    let val = val_raw.require_expr("between() value", span)?;
    let low = low_raw.into_coerced();
    let high = high_raw.into_coerced();
    let (lo_op, hi_op) = if inclusive {
        (CompareOp::Ge, CompareOp::Le)
    } else {
        (CompareOp::Gt, CompareOp::Lt)
    };
    Ok(LoweredValue::Expr(ZenExpr::Logical {
        op: LogicalOp::And,
        left: Box::new(ZenExpr::Compare {
            op: lo_op,
            left: Box::new(val.clone()),
            right: Box::new(low),
        }),
        right: Box::new(ZenExpr::Compare {
            op: hi_op,
            left: Box::new(val),
            right: Box::new(high),
        }),
    }))
}

fn take_fixed_args<const N: usize>(
    args: Vec<LoweredValue>,
    fn_name: &str,
    span: &SourceSpan,
) -> Result<[LoweredValue; N], PyAuthoringError> {
    let actual = args.len();
    args.try_into().ok().ok_or_else(|| {
        PyAuthoringError::spanned(
            CODE_EXPRESSION,
            format!("{fn_name}() expects {N} argument(s), got {actual}"),
            span.clone(),
        )
    })
}

fn fold_logical(
    op: LogicalOp,
    fn_name: &str,
    args: Vec<LoweredValue>,
    span: &SourceSpan,
) -> Result<ZenExpr, PyAuthoringError> {
    let ctx_label = format!("{fn_name}() argument");
    let mut iter = args.into_iter();
    let Some(first) = iter.next() else {
        return Err(PyAuthoringError::spanned(
            CODE_EXPRESSION,
            format!("{fn_name}() requires at least one condition"),
            span.clone(),
        ));
    };
    let mut acc = first.require_expr(&ctx_label, span)?;
    for next in iter {
        let rhs = next.require_expr(&ctx_label, span)?;
        acc = ZenExpr::Logical {
            op,
            left: Box::new(acc),
            right: Box::new(rhs),
        };
    }
    Ok(acc)
}
