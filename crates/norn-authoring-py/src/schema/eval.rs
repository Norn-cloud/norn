//! Static evaluator for declarative Python module-level and class-body expressions
//! (zero `CPython` execution).

use std::collections::BTreeMap;

use ruff_python_ast::{Expr, ExprCall, Number, UnaryOp};
use ruff_text_size::Ranged;

use crate::decimal::format_exact_decimal;
use crate::error::{
    CODE_ELABORATION, CODE_FLOAT_LITERAL, CODE_FORBIDDEN_CALL, CODE_RAW_EXPR, CODE_RAW_IR,
    CODE_SCHEMA, CODE_UNBOUNDED, PyAuthoringError, SourceSpan,
};
use crate::expr::{ZenExpr, ZenLiteral, render_zen_string};
use crate::line_table::LineTable;
use crate::sandbox::allowed_import_symbols;
use crate::schema::env::{CallbackRef, EvalValue, MAX_EVAL_DEPTH, ModuleEnv};
use crate::schema::eval_ctors::{
    eval_dependency_pin, eval_field_ctor, eval_field_default_fn, eval_field_meta, eval_key_spec,
    eval_link, eval_marking, eval_property_type_ctor, eval_unit_ref,
};
use crate::schema::eval_markings::{
    eval_lifecycle, eval_marking_decl, eval_markings_catalog, eval_mask_ctor, eval_transition,
};
use crate::schema::eval_packs::{
    eval_compose_packs, eval_deployment_spec, eval_fn_ref, eval_pack_manifest, eval_secret_ref,
};

/// Evaluates a declarative Python AST expression into a closed [`EvalValue`],
/// enforcing [`MAX_EVAL_DEPTH`] and [`crate::schema::env::MAX_EVAL_VALUE_NODES`].
pub(crate) fn eval_expr<'a>(
    expr: &'a Expr,
    env: &mut ModuleEnv<'a>,
    lines: &LineTable<'_>,
) -> Result<EvalValue<'a>, PyAuthoringError> {
    eval_expr_depth(expr, env, lines, 0)
}

fn eval_expr_depth<'a>(
    expr: &'a Expr,
    env: &mut ModuleEnv<'a>,
    lines: &LineTable<'_>,
    depth: usize,
) -> Result<EvalValue<'a>, PyAuthoringError> {
    let span = lines.span_at(expr.range().start());
    if depth > MAX_EVAL_DEPTH {
        return Err(PyAuthoringError::spanned(
            CODE_UNBOUNDED,
            format!("static evaluation exceeded maximum nesting depth ({MAX_EVAL_DEPTH})"),
            span,
        ));
    }
    env.charge_eval_nodes(1, &span)?;
    let next_depth = depth.saturating_add(1);

    match expr {
        Expr::NoneLiteral(_) => Ok(EvalValue::None),
        Expr::BooleanLiteral(b) => Ok(EvalValue::Bool(b.value)),
        Expr::StringLiteral(s) => Ok(EvalValue::Str(s.value.to_str().to_owned())),
        Expr::NumberLiteral(num) => match &num.value {
            Number::Int(val) => Ok(EvalValue::Int(val.to_string())),
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
        Expr::UnaryOp(unary) => eval_unary(unary, env, lines, &span, next_depth),
        Expr::Tuple(tuple) => {
            let mut items = Vec::with_capacity(tuple.elts.len());
            for elt in &tuple.elts {
                items.push(eval_expr_depth(elt, env, lines, next_depth)?);
            }
            Ok(EvalValue::Tuple(items))
        }
        Expr::List(list) => {
            let mut items = Vec::with_capacity(list.elts.len());
            for elt in &list.elts {
                items.push(eval_expr_depth(elt, env, lines, next_depth)?);
            }
            Ok(EvalValue::List(items))
        }
        Expr::Dict(dict) => eval_dict(dict, env, lines, &span, next_depth),
        Expr::Name(name) => eval_name(name.id.as_str(), env, &span),
        Expr::Attribute(attr) => eval_attr(attr, env, lines, &span, next_depth),
        Expr::Lambda(lambda) => Ok(EvalValue::Callback(CallbackRef::Lambda(lambda))),
        Expr::Call(call) => eval_call(call, env, lines, &span, next_depth),
        Expr::BoolOp(_)
        | Expr::Named(_)
        | Expr::BinOp(_)
        | Expr::If(_)
        | Expr::Set(_)
        | Expr::ListComp(_)
        | Expr::SetComp(_)
        | Expr::DictComp(_)
        | Expr::Generator(_)
        | Expr::Await(_)
        | Expr::Yield(_)
        | Expr::YieldFrom(_)
        | Expr::Compare(_)
        | Expr::FString(_)
        | Expr::TString(_)
        | Expr::BytesLiteral(_)
        | Expr::EllipsisLiteral(_)
        | Expr::Subscript(_)
        | Expr::Starred(_)
        | Expr::Slice(_)
        | Expr::IpyEscapeCommand(_) => Err(PyAuthoringError::spanned(
            CODE_ELABORATION,
            "unsupported expression in declarative Norn authoring module",
            span,
        )),
    }
}

fn eval_name<'a>(
    id: &str,
    env: &mut ModuleEnv<'a>,
    span: &SourceSpan,
) -> Result<EvalValue<'a>, PyAuthoringError> {
    let remaining = env.remaining_eval_nodes();
    let Some(cost) = env
        .bindings
        .get(id)
        .map(|val| val.bounded_node_count(remaining))
    else {
        return Err(PyAuthoringError::spanned(
            CODE_ELABORATION,
            format!("NameError: name '{id}' is not defined"),
            span.clone(),
        ));
    };
    env.charge_eval_nodes(cost, span)?;
    env.bindings.get(id).cloned().ok_or_else(|| {
        PyAuthoringError::spanned(
            CODE_ELABORATION,
            format!("NameError: name '{id}' is not defined"),
            span.clone(),
        )
    })
}

fn eval_unary<'a>(
    unary: &'a ruff_python_ast::ExprUnaryOp,
    env: &mut ModuleEnv<'a>,
    lines: &LineTable<'_>,
    span: &SourceSpan,
    depth: usize,
) -> Result<EvalValue<'a>, PyAuthoringError> {
    let operand = eval_expr_depth(&unary.operand, env, lines, depth)?;
    if let EvalValue::Int(digits) = operand {
        match unary.op {
            UnaryOp::UAdd => return Ok(EvalValue::Int(digits)),
            UnaryOp::USub => {
                let negated = digits
                    .strip_prefix('-')
                    .map_or_else(|| format!("-{digits}"), str::to_owned);
                return Ok(EvalValue::Int(negated));
            }
            UnaryOp::Not | UnaryOp::Invert => {}
        }
    }
    Err(PyAuthoringError::spanned(
        CODE_ELABORATION,
        "unsupported unary operator in declarative Norn authoring module",
        span.clone(),
    ))
}

fn eval_dict<'a>(
    dict: &'a ruff_python_ast::ExprDict,
    env: &mut ModuleEnv<'a>,
    lines: &LineTable<'_>,
    span: &SourceSpan,
    depth: usize,
) -> Result<EvalValue<'a>, PyAuthoringError> {
    let mut map = BTreeMap::new();
    for item in &dict.items {
        let Some(key_expr) = &item.key else {
            return Err(PyAuthoringError::spanned(
                CODE_ELABORATION,
                "dictionary unpacking `**` is not admitted in declarative Norn authoring modules",
                span.clone(),
            ));
        };
        let key_val = eval_expr_depth(key_expr, env, lines, depth)?;
        let EvalValue::Str(key) = key_val else {
            return Err(PyAuthoringError::spanned(
                CODE_SCHEMA,
                "dictionary keys in Norn definitions must be strings",
                span.clone(),
            ));
        };
        let val = eval_expr_depth(&item.value, env, lines, depth)?;
        map.insert(key, val);
    }
    Ok(EvalValue::Dict(map))
}

fn eval_attr<'a>(
    attr: &'a ruff_python_ast::ExprAttribute,
    env: &mut ModuleEnv<'a>,
    lines: &LineTable<'_>,
    span: &SourceSpan,
    depth: usize,
) -> Result<EvalValue<'a>, PyAuthoringError> {
    let attr_name = attr.attr.id.as_str();
    let owner = eval_expr_depth(&attr.value, env, lines, depth)?;
    match owner {
        EvalValue::ModuleNamespace(mod_name) => {
            if let Some(allowed) = allowed_import_symbols(mod_name)
                && let Some(&sym) = allowed.iter().find(|&&s| s == attr_name)
            {
                return Ok(EvalValue::BuiltinSymbol(sym));
            }
            Err(PyAuthoringError::spanned(
                CODE_ELABORATION,
                format!("AttributeError: module '{mod_name}' has no attribute '{attr_name}'"),
                span.clone(),
            ))
        }
        EvalValue::EnumClass(enum_name) => {
            if let Some(decl) = env.enums.get(&enum_name)
                && let Some(member) = decl.member(attr_name)
            {
                return Ok(EvalValue::EnumMember(Box::new(member.clone())));
            }
            Err(PyAuthoringError::spanned(
                CODE_ELABORATION,
                format!("AttributeError: enum '{enum_name}' has no member '{attr_name}'"),
                span.clone(),
            ))
        }
        EvalValue::None
        | EvalValue::Bool(_)
        | EvalValue::Int(_)
        | EvalValue::Decimal(_)
        | EvalValue::Str(_)
        | EvalValue::Tuple(_)
        | EvalValue::List(_)
        | EvalValue::Dict(_)
        | EvalValue::BuiltinSymbol(_)
        | EvalValue::EnumMember(_)
        | EvalValue::ConceptClass(_)
        | EvalValue::ProtocolClass
        | EvalValue::UnitRef(_)
        | EvalValue::DependencyPin(_)
        | EvalValue::KeySpec(_)
        | EvalValue::Marking(_)
        | EvalValue::FieldMeta(_)
        | EvalValue::FieldDefault(_)
        | EvalValue::PropertyTypeSpec(_)
        | EvalValue::LinkSpec(_)
        | EvalValue::MaskSpec(_)
        | EvalValue::MarkingDecl(_)
        | EvalValue::MarkingsCatalog(_)
        | EvalValue::LawDecl(_)
        | EvalValue::FnRef(_)
        | EvalValue::SecretRef(_)
        | EvalValue::PackManifest(_)
        | EvalValue::DeploymentSpec
        | EvalValue::TransitionSpec(_)
        | EvalValue::LifecycleSpec(_)
        | EvalValue::ZenExpr(_)
        | EvalValue::Callback(_)
        | EvalValue::OpaqueObject => Err(PyAuthoringError::spanned(
            CODE_ELABORATION,
            format!("AttributeError: unsupported attribute access '.{attr_name}'"),
            span.clone(),
        )),
    }
}

fn eval_call<'a>(
    call: &'a ExprCall,
    env: &mut ModuleEnv<'a>,
    lines: &LineTable<'_>,
    span: &SourceSpan,
    depth: usize,
) -> Result<EvalValue<'a>, PyAuthoringError> {
    for arg in &call.arguments.args {
        if matches!(arg, Expr::Starred(_)) {
            return Err(PyAuthoringError::spanned(
                CODE_ELABORATION,
                "starred `*args` unpacking is forbidden in declarative Norn calls",
                span.clone(),
            ));
        }
    }
    for kw in &call.arguments.keywords {
        if kw.arg.is_none() {
            return Err(PyAuthoringError::spanned(
                CODE_ELABORATION,
                "`**kwargs` unpacking is forbidden in declarative Norn calls",
                span.clone(),
            ));
        }
    }

    let callee = eval_expr_depth(&call.func, env, lines, depth)?;
    let EvalValue::BuiltinSymbol(sym) = callee else {
        return Err(PyAuthoringError::spanned(
            CODE_FORBIDDEN_CALL,
            "call target is not an admitted Norn authoring constructor",
            span.clone(),
        ));
    };

    let mut pos = Vec::with_capacity(call.arguments.args.len());
    for arg in &call.arguments.args {
        pos.push(eval_expr_depth(arg, env, lines, depth)?);
    }
    let mut kwargs: Vec<(String, EvalValue<'a>)> =
        Vec::with_capacity(call.arguments.keywords.len());
    for kw in &call.arguments.keywords {
        if let Some(arg_id) = &kw.arg {
            kwargs.push((
                arg_id.id.as_str().to_owned(),
                eval_expr_depth(&kw.value, env, lines, depth)?,
            ));
        }
    }

    dispatch_ctor(sym, pos, kwargs, env, span)
}

fn dispatch_ctor<'a>(
    sym: &str,
    pos: Vec<EvalValue<'a>>,
    kwargs: Vec<(String, EvalValue<'a>)>,
    env: &ModuleEnv<'a>,
    span: &SourceSpan,
) -> Result<EvalValue<'a>, PyAuthoringError> {
    match sym {
        "Decimal" => eval_decimal_ctor(pos, &kwargs, span),
        "lit" => eval_lit_ctor(pos, &kwargs, span),
        "Expr" => Err(PyAuthoringError::spanned(
            CODE_RAW_EXPR,
            "Expr cannot be constructed directly from raw ZEN strings; use symbolic fields or lit(...)",
            span.clone(),
        )),
        "PropertyTypeSpec" => Err(PyAuthoringError::spanned(
            CODE_RAW_IR,
            "PropertyTypeSpec cannot be constructed directly from raw IR dicts; use text(), integer(), boolean(), decimal_type(), rate_type(), date_type(), timestamp_type(), duration_type(), enum_type(), or ref_type()",
            span.clone(),
        )),
        "MaskSpec" => Err(PyAuthoringError::spanned(
            CODE_RAW_IR,
            "MaskSpec cannot be constructed from raw dicts; use mask_redacted(), mask_null(), mask_last_n(), mask_refuse(), or mask_pseudonym()",
            span.clone(),
        )),
        "UnitRef" => eval_unit_ref(pos, kwargs, span),
        "DependencyPin" => eval_dependency_pin(pos, kwargs, span),
        "KeySpec" => eval_key_spec(pos, kwargs, span),
        "Marking" => eval_marking(pos, kwargs, span),
        "FieldMeta" => eval_field_meta(pos, kwargs, span),
        "Field" => eval_field_ctor(&pos, kwargs, span),
        "field_default" => eval_field_default_fn(pos, &kwargs, span),
        "text" | "integer" | "boolean" | "date_type" | "timestamp_type" | "duration_type"
        | "ref_type" | "decimal_type" | "rate_type" | "enum_type" => {
            eval_property_type_ctor(sym, pos, kwargs, env, span)
        }
        "link" => eval_link(pos, kwargs, env, span),
        "mask_redacted" | "mask_null" | "mask_refuse" | "mask_last_n" | "mask_pseudonym" => {
            eval_mask_ctor(sym, pos, kwargs, span)
        }
        "MarkingDecl" => eval_marking_decl(pos, kwargs, span),
        "MarkingsCatalog" => eval_markings_catalog(pos, kwargs, span),
        "transition" | "TransitionSpec" => eval_transition(pos, kwargs, span),
        "lifecycle" | "LifecycleSpec" => eval_lifecycle(pos, kwargs, env, span),
        "fn_ref" | "FnRef" => eval_fn_ref(pos, kwargs, span),
        "SecretRef" => eval_secret_ref(pos, kwargs, span),
        "PackManifest" => eval_pack_manifest(pos, kwargs, env, span),
        "compose_packs" => eval_compose_packs(pos, kwargs, span),
        "DeploymentSpec" => eval_deployment_spec(pos, kwargs, span),
        _ => Err(PyAuthoringError::spanned(
            CODE_FORBIDDEN_CALL,
            format!("call to '{sym}()' is not admitted at module/class scope"),
            span.clone(),
        )),
    }
}

fn eval_decimal_ctor<'a>(
    pos: Vec<EvalValue<'a>>,
    kwargs: &[(String, EvalValue<'a>)],
    span: &SourceSpan,
) -> Result<EvalValue<'a>, PyAuthoringError> {
    if !kwargs.is_empty() || pos.len() != 1 {
        return Err(PyAuthoringError::spanned(
            CODE_SCHEMA,
            "Decimal() requires exactly 1 positional argument",
            span.clone(),
        ));
    }
    let Some(EvalValue::Str(raw) | EvalValue::Int(raw)) = pos.into_iter().next() else {
        return Err(PyAuthoringError::spanned(
            CODE_SCHEMA,
            "Decimal() argument must be a string or integer",
            span.clone(),
        ));
    };
    let formatted = format_exact_decimal(&raw, Some(span.clone()))?;
    Ok(EvalValue::Decimal(formatted))
}

fn eval_lit_ctor<'a>(
    pos: Vec<EvalValue<'a>>,
    kwargs: &[(String, EvalValue<'a>)],
    span: &SourceSpan,
) -> Result<EvalValue<'a>, PyAuthoringError> {
    if !kwargs.is_empty() || pos.len() != 1 {
        return Err(PyAuthoringError::spanned(
            CODE_SCHEMA,
            "lit() requires exactly 1 positional argument",
            span.clone(),
        ));
    }
    let Some(arg) = pos.into_iter().next() else {
        return Err(PyAuthoringError::spanned(
            CODE_SCHEMA,
            "lit() requires a literal argument",
            span.clone(),
        ));
    };
    let lit = match arg {
        EvalValue::None => ZenLiteral::Null,
        EvalValue::Bool(b) => ZenLiteral::Bool(b),
        EvalValue::Int(n) => ZenLiteral::Int(n),
        EvalValue::Decimal(d) => ZenLiteral::Decimal(d),
        EvalValue::Str(s) => {
            let _validated = render_zen_string(&s, Some(span.clone()))?;
            ZenLiteral::Str(s)
        }
        EvalValue::EnumMember(m) => m.to_zen_literal(),
        EvalValue::Tuple(_)
        | EvalValue::List(_)
        | EvalValue::Dict(_)
        | EvalValue::ModuleNamespace(_)
        | EvalValue::BuiltinSymbol(_)
        | EvalValue::EnumClass(_)
        | EvalValue::ConceptClass(_)
        | EvalValue::ProtocolClass
        | EvalValue::UnitRef(_)
        | EvalValue::DependencyPin(_)
        | EvalValue::KeySpec(_)
        | EvalValue::Marking(_)
        | EvalValue::FieldMeta(_)
        | EvalValue::FieldDefault(_)
        | EvalValue::PropertyTypeSpec(_)
        | EvalValue::LinkSpec(_)
        | EvalValue::MaskSpec(_)
        | EvalValue::MarkingDecl(_)
        | EvalValue::MarkingsCatalog(_)
        | EvalValue::LawDecl(_)
        | EvalValue::FnRef(_)
        | EvalValue::SecretRef(_)
        | EvalValue::PackManifest(_)
        | EvalValue::DeploymentSpec
        | EvalValue::TransitionSpec(_)
        | EvalValue::LifecycleSpec(_)
        | EvalValue::ZenExpr(_)
        | EvalValue::Callback(_)
        | EvalValue::OpaqueObject => {
            return Err(PyAuthoringError::spanned(
                CODE_SCHEMA,
                "unsupported literal type in lit()",
                span.clone(),
            ));
        }
    };
    Ok(EvalValue::ZenExpr(Box::new(ZenExpr::Literal(lit))))
}
