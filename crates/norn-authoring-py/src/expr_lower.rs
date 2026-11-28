//! Direct Python callback AST-to-`ZenExpr` lowering with zero Python execution.

use std::collections::BTreeMap;

use ruff_python_ast::{
    CmpOp, Expr, ExprCall, ExprCompare, ExprLambda, Number, Operator, Stmt, StmtFunctionDef,
    UnaryOp,
};
use ruff_python_parser::{parse_expression, parse_module};
use ruff_text_size::Ranged;

use crate::decimal::format_exact_decimal;
use crate::error::{
    CODE_EXPRESSION, CODE_FLOAT_LITERAL, CODE_FORBIDDEN_CALL, CODE_FORBIDDEN_CONTROL_FLOW,
    CODE_SYNTAX, CODE_UNBOUNDED, CODE_UNKNOWN_FIELD, PyAuthoringError, SourceSpan,
};
use crate::expr::{
    BinaryOp, CompareOp, LogicalOp, LowerContext, ZenExpr, ZenLiteral, check_valid_ident,
    render_zen_string,
};
use crate::expr_validate::{
    extract_single_return_expr, validate_callback_expr, validate_callback_parameters,
};
use crate::line_table::LineTable;
use crate::sandbox::{MAX_SOURCE_BYTES, SandboxLimits, parse_and_validate_module};

/// Validates and lowers a standalone Python `def` or `lambda` callback directly
/// to [`ZenExpr`] with zero Python execution.
///
/// # Errors
///
/// Returns [`PyAuthoringError`] on syntax errors, sandbox/callback profile
/// violations, undeclared fields, or invalid ZEN expressions.
pub fn lower_callback_source(
    source: &str,
    filename: &str,
    ctx: &LowerContext,
) -> Result<ZenExpr, PyAuthoringError> {
    if source.len() > MAX_SOURCE_BYTES {
        return Err(PyAuthoringError::spanned(
            CODE_UNBOUNDED,
            format!("source exceeds {MAX_SOURCE_BYTES} byte limit"),
            SourceSpan::new(filename, 1, 0),
        ));
    }
    let lines = LineTable::new(filename, source);
    if let Ok(parsed_expr) = parse_expression(source)
        && let Expr::Lambda(lambda) = parsed_expr.expr()
    {
        return lower_lambda(lambda, &lines, ctx);
    }

    let parsed_mod = parse_module(source).map_err(|err| {
        PyAuthoringError::spanned(
            CODE_SYNTAX,
            err.error.to_string(),
            lines.span_at(err.location.start()),
        )
    })?;
    let body: &[Stmt] = parsed_mod.suite();
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
    lower_expr(return_expr, param_name, lines, ctx)
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
    lower_expr(&lambda.body, param_name, lines, ctx)
}

fn lower_expr(
    expr: &Expr,
    param_name: &str,
    lines: &LineTable<'_>,
    ctx: &LowerContext,
) -> Result<ZenExpr, PyAuthoringError> {
    let span = lines.span_at(expr.range().start());
    match expr {
        Expr::NoneLiteral(_) => Ok(ZenExpr::Literal(ZenLiteral::Null)),
        Expr::BooleanLiteral(b) => Ok(ZenExpr::Literal(ZenLiteral::Bool(b.value))),
        Expr::NumberLiteral(num) => match &num.value {
            Number::Int(val) => Ok(ZenExpr::Literal(ZenLiteral::Int(val.to_string()))),
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
            Ok(ZenExpr::Literal(ZenLiteral::Str(text.to_owned())))
        }
        Expr::Name(name) => {
            let id = name.id.as_str();
            if id == param_name {
                return Ok(ZenExpr::Ref {
                    root: ctx.root.clone(),
                    path: Vec::new(),
                });
            }
            if let Some(constant) = ctx.constants.get(id) {
                return Ok(ZenExpr::Literal(constant.clone()));
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
) -> Result<ZenExpr, PyAuthoringError> {
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
            return Ok(ZenExpr::Ref {
                root: ctx.root.clone(),
                path: vec![field.to_owned()],
            });
        }
        let qualified = format!("{base}.{field}");
        if let Some(constant) = ctx.constants.get(&qualified) {
            return Ok(ZenExpr::Literal(constant.clone()));
        }
    }

    let base_expr = lower_expr(&attr.value, param_name, lines, ctx)?;
    if let ZenExpr::Ref { root, mut path } = base_expr {
        let seg = attr.attr.id.as_str();
        check_valid_ident(seg, Some(span))?;
        path.push(seg.to_owned());
        return Ok(ZenExpr::Ref { root, path });
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
) -> Result<ZenExpr, PyAuthoringError> {
    let operand = lower_expr(&unary.operand, param_name, lines, ctx)?;
    match unary.op {
        UnaryOp::UAdd => Ok(operand),
        UnaryOp::USub => Ok(ZenExpr::UnaryNeg(Box::new(operand))),
        UnaryOp::Invert => Ok(ZenExpr::Not(Box::new(operand))),
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
) -> Result<ZenExpr, PyAuthoringError> {
    let left = lower_expr(&bin_op.left, param_name, lines, ctx)?;
    let right = lower_expr(&bin_op.right, param_name, lines, ctx)?;
    match bin_op.op {
        Operator::Add => Ok(ZenExpr::Binary {
            op: BinaryOp::Add,
            left: Box::new(left),
            right: Box::new(right),
        }),
        Operator::Sub => Ok(ZenExpr::Binary {
            op: BinaryOp::Sub,
            left: Box::new(left),
            right: Box::new(right),
        }),
        Operator::Mult => Ok(ZenExpr::Binary {
            op: BinaryOp::Mul,
            left: Box::new(left),
            right: Box::new(right),
        }),
        Operator::Div => Ok(ZenExpr::Binary {
            op: BinaryOp::Div,
            left: Box::new(left),
            right: Box::new(right),
        }),
        Operator::BitAnd => Ok(ZenExpr::Logical {
            op: LogicalOp::And,
            left: Box::new(left),
            right: Box::new(right),
        }),
        Operator::BitOr => Ok(ZenExpr::Logical {
            op: LogicalOp::Or,
            left: Box::new(left),
            right: Box::new(right),
        }),
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
) -> Result<ZenExpr, PyAuthoringError> {
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
    let left = lower_expr(left_ast, param_name, lines, ctx)?;
    let right = lower_expr(right_ast, param_name, lines, ctx)?;
    Ok(ZenExpr::Compare {
        op,
        left: Box::new(left),
        right: Box::new(right),
    })
}

fn lower_call(
    call: &ExprCall,
    param_name: &str,
    span: &SourceSpan,
    lines: &LineTable<'_>,
    ctx: &LowerContext,
) -> Result<ZenExpr, PyAuthoringError> {
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
) -> Result<ZenExpr, PyAuthoringError> {
    let method = attr.attr.id.as_str();
    if !call.arguments.keywords.is_empty() {
        return Err(PyAuthoringError::spanned(
            CODE_EXPRESSION,
            format!("method .{method}() does not accept keyword arguments"),
            span.clone(),
        ));
    }
    let receiver = lower_expr(&attr.value, param_name, lines, ctx)?;
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
            let rhs = lower_expr(rhs_ast, param_name, lines, ctx)?;
            let op = if method == "eq" {
                CompareOp::Eq
            } else {
                CompareOp::Ne
            };
            Ok(ZenExpr::Compare {
                op,
                left: Box::new(receiver),
                right: Box::new(rhs),
            })
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
            Ok(ZenExpr::Compare {
                op,
                left: Box::new(receiver),
                right: Box::new(ZenExpr::Literal(ZenLiteral::Null)),
            })
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
) -> Result<ZenExpr, PyAuthoringError> {
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

    match fn_name {
        "lit" => {
            let [arg] = take_fixed_args::<1>(args, fn_name, span)?;
            Ok(arg)
        }
        "all_of" => fold_logical(LogicalOp::And, fn_name, args, span),
        "any_of" => fold_logical(LogicalOp::Or, fn_name, args, span),
        "not_" => {
            let [arg] = take_fixed_args::<1>(args, fn_name, span)?;
            Ok(ZenExpr::Not(Box::new(arg)))
        }
        "when" | "if_else" => {
            let [cond, yes, no] = take_fixed_args::<3>(args, fn_name, span)?;
            Ok(ZenExpr::Conditional {
                condition: Box::new(cond),
                yes: Box::new(yes),
                no: Box::new(no),
            })
        }
        "norn_len" => {
            let [arg] = take_fixed_args::<1>(args, fn_name, span)?;
            Ok(ZenExpr::Call {
                fn_name: "len".to_owned(),
                args: vec![arg],
            })
        }
        "contains" => {
            let [haystack, needle] = take_fixed_args::<2>(args, fn_name, span)?;
            Ok(ZenExpr::Call {
                fn_name: "contains".to_owned(),
                args: vec![haystack, needle],
            })
        }
        _ => lower_temporal_func_call(fn_name, args, span),
    }
}

fn lower_temporal_func_call(
    fn_name: &str,
    args: Vec<ZenExpr>,
    span: &SourceSpan,
) -> Result<ZenExpr, PyAuthoringError> {
    match fn_name {
        "parse_date" => {
            let [arg] = take_fixed_args::<1>(args, fn_name, span)?;
            Ok(ZenExpr::Call {
                fn_name: "date".to_owned(),
                args: vec![arg],
            })
        }
        "parse_timestamp" => {
            let [arg] = take_fixed_args::<1>(args, fn_name, span)?;
            Ok(ZenExpr::Call {
                fn_name: "timestamp".to_owned(),
                args: vec![arg],
            })
        }
        "date_before" => {
            let [left, right] = take_fixed_args::<2>(args, fn_name, span)?;
            Ok(ZenExpr::Call {
                fn_name: "dateBefore".to_owned(),
                args: vec![left, right],
            })
        }
        "timestamp_before" => {
            let [left, right] = take_fixed_args::<2>(args, fn_name, span)?;
            Ok(ZenExpr::Call {
                fn_name: "timestampBefore".to_owned(),
                args: vec![left.as_timestamp(), right.as_timestamp()],
            })
        }
        "timestamp_at_or_before" => {
            let [left, right] = take_fixed_args::<2>(args, fn_name, span)?;
            Ok(ZenExpr::Call {
                fn_name: "timestampAtOrBefore".to_owned(),
                args: vec![left.as_timestamp(), right.as_timestamp()],
            })
        }
        "days_between" => {
            let [start, end] = take_fixed_args::<2>(args, fn_name, span)?;
            Ok(ZenExpr::Call {
                fn_name: "days_between".to_owned(),
                args: vec![start.as_date(), end.as_date()],
            })
        }
        "add_days" => {
            let [start, days] = take_fixed_args::<2>(args, fn_name, span)?;
            Ok(ZenExpr::Call {
                fn_name: "add_days".to_owned(),
                args: vec![start.as_date(), days],
            })
        }
        "seconds_between" => {
            let [start, end] = take_fixed_args::<2>(args, fn_name, span)?;
            Ok(ZenExpr::Call {
                fn_name: "seconds_between".to_owned(),
                args: vec![start.as_timestamp(), end.as_timestamp()],
            })
        }
        "add_seconds" => {
            let [start, seconds] = take_fixed_args::<2>(args, fn_name, span)?;
            Ok(ZenExpr::Call {
                fn_name: "add_seconds".to_owned(),
                args: vec![start.as_timestamp(), seconds],
            })
        }
        "intervals_overlap" | "interval_gap_seconds" | "interval_intersection_seconds" => {
            let [a_start, a_end, b_start, b_end] = take_fixed_args::<4>(args, fn_name, span)?;
            Ok(ZenExpr::Call {
                fn_name: fn_name.to_owned(),
                args: vec![
                    a_start.as_timestamp(),
                    a_end.as_timestamp(),
                    b_start.as_timestamp(),
                    b_end.as_timestamp(),
                ],
            })
        }
        _ => Err(PyAuthoringError::spanned(
            CODE_FORBIDDEN_CALL,
            format!("call to {fn_name}() is outside the closed Norn expression operator set"),
            span.clone(),
        )),
    }
}

fn lower_decimal_call(call: &ExprCall, span: &SourceSpan) -> Result<ZenExpr, PyAuthoringError> {
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
    Ok(ZenExpr::Literal(ZenLiteral::Decimal(formatted)))
}

fn lower_between_call(
    call: &ExprCall,
    param_name: &str,
    span: &SourceSpan,
    lines: &LineTable<'_>,
    ctx: &LowerContext,
) -> Result<ZenExpr, PyAuthoringError> {
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
    let [val, low, high] = take_fixed_args::<3>(args, "between", span)?;
    let (lo_op, hi_op) = if inclusive {
        (CompareOp::Ge, CompareOp::Le)
    } else {
        (CompareOp::Gt, CompareOp::Lt)
    };
    Ok(ZenExpr::Logical {
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
    })
}

fn take_fixed_args<const N: usize>(
    args: Vec<ZenExpr>,
    fn_name: &str,
    span: &SourceSpan,
) -> Result<[ZenExpr; N], PyAuthoringError> {
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
    args: Vec<ZenExpr>,
    span: &SourceSpan,
) -> Result<ZenExpr, PyAuthoringError> {
    let mut iter = args.into_iter();
    let Some(mut acc) = iter.next() else {
        return Err(PyAuthoringError::spanned(
            CODE_EXPRESSION,
            format!("{fn_name}() requires at least one condition"),
            span.clone(),
        ));
    };
    for next in iter {
        acc = ZenExpr::Logical {
            op,
            left: Box::new(acc),
            right: Box::new(next),
        };
    }
    Ok(acc)
}
