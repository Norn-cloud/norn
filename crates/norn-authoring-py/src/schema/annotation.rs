//! Closed static field-annotation AST resolver matching
//! `tools/norn-py/norn_py/schema.py` (`resolve_annotation` / `_eval_annotation_ast`).

use ruff_python_ast::{
    Expr, ExprAttribute, ExprCall, ExprSubscript, ExprUnaryOp, Number, Operator, Stmt, UnaryOp,
};
use ruff_python_parser::parse_module;

use crate::error::{CODE_FLOAT_LITERAL, CODE_UNSAFE_ANNOTATION, PyAuthoringError, SourceSpan};
use crate::sandbox::allowed_import_symbols;
use crate::schema::annotation_check::{ExtractedField, extract_property_and_meta};
use crate::schema::annotation_ctors::{ANNOTATION_CALLABLES, dispatch_annotation_ctor};
use crate::schema::env::{EnumMemberVal, EvalValue, ModuleEnv};
use crate::schema::types::{FieldMeta, LinkSpec, PropertyTypeSpec, UnitRef};

/// Maximum permitted nesting depth while resolving a field annotation AST.
pub const MAX_ANNOTATION_DEPTH: usize = 64;

const ANNOTATION_ALLOWLIST: &[&str] = &[
    "Annotated",
    "Date",
    "Decimal",
    "Duration",
    "Field",
    "FieldMeta",
    "Link",
    "Literal",
    "Marking",
    "Money",
    "None",
    "Optional",
    "Quantity",
    "Rate",
    "StrEnum",
    "Timestamp",
    "Union",
    "UnitRef",
    "bool",
    "boolean",
    "date_type",
    "decimal_type",
    "duration_type",
    "enum_type",
    "float",
    "int",
    "integer",
    "link",
    "rate_type",
    "ref_type",
    "str",
    "text",
    "timestamp_type",
];

#[derive(Clone, Debug)]
pub(crate) struct AnnotatedNode {
    pub(crate) base: AnnVal,
    pub(crate) metadata: Vec<AnnVal>,
}

#[derive(Clone, Debug)]
pub(crate) struct RateNode {
    pub(crate) currency: AnnVal,
    pub(crate) unit: AnnVal,
}

#[derive(Clone, Debug)]
pub(crate) enum AnnVal {
    None,
    Bool(bool),
    Int(String),
    Str(String),
    Decimal(String),
    Symbol(&'static str),
    ModuleNamespace(&'static str),
    EnumClass(String),
    EnumMember(Box<EnumMemberVal>),
    ConceptClass { py_name: String, norn_name: String },
    Nullable(Box<Self>),
    Union(Vec<Self>),
    Literal(Vec<Self>),
    Field(Box<Self>),
    Link(Box<Self>),
    Quantity(Box<Self>),
    Money(Box<Self>),
    Rate(Box<RateNode>),
    Annotated(Box<AnnotatedNode>),
    UnitRef(Box<UnitRef>),
    Marking(String),
    FieldMeta(Box<FieldMeta>),
    PropertyTypeSpec(Box<PropertyTypeSpec>),
    LinkSpec(Box<LinkSpec>),
}

/// Validates and resolves a standalone Python annotation string without ever invoking `eval()`.
///
/// # Errors
///
/// Returns [`PyAuthoringError`] with code [`CODE_UNSAFE_ANNOTATION`], [`CODE_FLOAT_LITERAL`],
/// [`crate::error::CODE_EXPRESSION`], or [`crate::error::CODE_SCHEMA`] if the annotation violates
/// the closed allowlist or exceeds [`MAX_ANNOTATION_DEPTH`].
pub fn validate_annotation_source(
    annotation: &str,
    span: Option<&SourceSpan>,
) -> Result<(), PyAuthoringError> {
    let parsed = parse_module(annotation).map_err(|err| {
        PyAuthoringError::with_optional_span(
            CODE_UNSAFE_ANNOTATION,
            format!("invalid annotation syntax '{annotation}': {}", err.error),
            span.cloned(),
        )
    })?;
    let body: &[Stmt] = parsed.suite();
    let [Stmt::Expr(expr_stmt)] = body else {
        return Err(PyAuthoringError::with_optional_span(
            CODE_UNSAFE_ANNOTATION,
            format!("invalid annotation syntax '{annotation}'"),
            span.cloned(),
        ));
    };
    let env = ModuleEnv::new();
    let _val = eval_annotation_ast(&expr_stmt.value, &env, span, 0)?;
    Ok(())
}

/// Resolves a field annotation AST node and validates `Annotated[base, meta...]` consistency.
pub(crate) fn extract_field_from_ast(
    field_name: &str,
    annotation: &Expr,
    env: &ModuleEnv<'_>,
    span: Option<&SourceSpan>,
) -> Result<ExtractedField, PyAuthoringError> {
    let resolved = eval_annotation_ast(annotation, env, span, 0)?;
    extract_property_and_meta(field_name, resolved, env, span)
}

fn eval_annotation_ast(
    expr: &Expr,
    env: &ModuleEnv<'_>,
    span: Option<&SourceSpan>,
    depth: usize,
) -> Result<AnnVal, PyAuthoringError> {
    if depth > MAX_ANNOTATION_DEPTH {
        return Err(PyAuthoringError::with_optional_span(
            CODE_UNSAFE_ANNOTATION,
            format!("field annotation exceeds maximum nesting depth ({MAX_ANNOTATION_DEPTH})"),
            span.cloned(),
        ));
    }
    let next_depth = depth.saturating_add(1);
    match expr {
        Expr::NoneLiteral(_) => Ok(AnnVal::None),
        Expr::BooleanLiteral(b) => Ok(AnnVal::Bool(b.value)),
        Expr::StringLiteral(s) => Ok(AnnVal::Str(s.value.to_str().to_owned())),
        Expr::NumberLiteral(num) => eval_annotation_number(&num.value, span),
        Expr::UnaryOp(unary) => eval_annotation_unary(unary, env, span, next_depth),
        Expr::Name(name) => eval_annotation_name(name.id.as_str(), env, span),
        Expr::Attribute(attr) => eval_annotation_attr(attr, env, span, next_depth),
        Expr::BinOp(bin_op) if matches!(bin_op.op, Operator::BitOr) => {
            let left = eval_annotation_ast(&bin_op.left, env, span, next_depth)?;
            let right = eval_annotation_ast(&bin_op.right, env, span, next_depth)?;
            if matches!(left, AnnVal::None) {
                Ok(AnnVal::Nullable(Box::new(right)))
            } else if matches!(right, AnnVal::None) {
                Ok(AnnVal::Nullable(Box::new(left)))
            } else {
                Err(PyAuthoringError::with_optional_span(
                    CODE_UNSAFE_ANNOTATION,
                    "union `|` in field annotations is only permitted for nullable types (`T | None`)",
                    span.cloned(),
                ))
            }
        }
        Expr::Subscript(sub) => eval_annotation_subscript(sub, env, span, next_depth),
        Expr::Call(call) => eval_annotation_call(call, env, span, next_depth),
        Expr::BinOp(_)
        | Expr::BoolOp(_)
        | Expr::Named(_)
        | Expr::Lambda(_)
        | Expr::If(_)
        | Expr::Dict(_)
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
        | Expr::Starred(_)
        | Expr::List(_)
        | Expr::Tuple(_)
        | Expr::Slice(_)
        | Expr::IpyEscapeCommand(_) => Err(PyAuthoringError::with_optional_span(
            CODE_UNSAFE_ANNOTATION,
            "unsupported syntax in field annotation",
            span.cloned(),
        )),
    }
}

fn eval_annotation_number(
    number: &Number,
    span: Option<&SourceSpan>,
) -> Result<AnnVal, PyAuthoringError> {
    match number {
        Number::Int(val) => Ok(AnnVal::Int(val.to_string())),
        Number::Float(val) => Err(PyAuthoringError::with_optional_span(
            CODE_FLOAT_LITERAL,
            format!(
                "Binary float {val} is refused in Norn field annotation (VAL-EXACT-1). Use exact Decimal('{val}') instead."
            ),
            span.cloned(),
        )),
        Number::Complex { .. } => Err(PyAuthoringError::with_optional_span(
            CODE_UNSAFE_ANNOTATION,
            "unsupported complex constant in field annotation",
            span.cloned(),
        )),
    }
}

fn eval_annotation_unary(
    unary: &ExprUnaryOp,
    env: &ModuleEnv<'_>,
    span: Option<&SourceSpan>,
    depth: usize,
) -> Result<AnnVal, PyAuthoringError> {
    let operand = eval_annotation_ast(&unary.operand, env, span, depth)?;
    if let AnnVal::Int(digits) = operand {
        match unary.op {
            UnaryOp::UAdd => return Ok(AnnVal::Int(digits)),
            UnaryOp::USub => {
                let negated = digits
                    .strip_prefix('-')
                    .map_or_else(|| format!("-{digits}"), str::to_owned);
                return Ok(AnnVal::Int(negated));
            }
            UnaryOp::Not | UnaryOp::Invert => {}
        }
    }
    Err(PyAuthoringError::with_optional_span(
        CODE_UNSAFE_ANNOTATION,
        "unary +/- in annotations is only permitted on integer literals",
        span.cloned(),
    ))
}

fn eval_annotation_attr(
    attr: &ExprAttribute,
    env: &ModuleEnv<'_>,
    span: Option<&SourceSpan>,
    depth: usize,
) -> Result<AnnVal, PyAuthoringError> {
    let attr_name = attr.attr.id.as_str();
    if attr_name.starts_with("__") {
        return Err(PyAuthoringError::with_optional_span(
            CODE_UNSAFE_ANNOTATION,
            format!("dunder attribute .{attr_name} is forbidden in field annotations"),
            span.cloned(),
        ));
    }
    let owner = eval_annotation_ast(&attr.value, env, span, depth)?;
    if let AnnVal::EnumClass(enum_name) = owner {
        if let Some(decl) = env.enums.get(&enum_name)
            && decl.is_str_enum
            && let Some(member) = decl.member(attr_name)
        {
            return Ok(AnnVal::EnumMember(Box::new(member.clone())));
        }
    } else if let AnnVal::ModuleNamespace(mod_name) = owner {
        if let Some(syms) = allowed_import_symbols(mod_name)
            && let Some(&sym) = syms.iter().find(|&&s| s == attr_name)
        {
            return eval_annotation_allowlist_symbol(sym, span);
        }
        return Err(PyAuthoringError::with_optional_span(
            CODE_UNSAFE_ANNOTATION,
            format!("unknown symbol '{mod_name}.{attr_name}' in field annotation"),
            span.cloned(),
        ));
    }
    Err(PyAuthoringError::with_optional_span(
        CODE_UNSAFE_ANNOTATION,
        format!(
            "attribute access .{attr_name} is only allowed on StrEnum members in field annotations"
        ),
        span.cloned(),
    ))
}

fn eval_annotation_allowlist_symbol(
    sym: &'static str,
    span: Option<&SourceSpan>,
) -> Result<AnnVal, PyAuthoringError> {
    if sym == "float" {
        return Err(PyAuthoringError::with_optional_span(
            CODE_FLOAT_LITERAL,
            "Binary float is refused in Norn field annotation (VAL-EXACT-1)",
            span.cloned(),
        ));
    }
    if sym == "None" {
        return Ok(AnnVal::None);
    }
    Ok(AnnVal::Symbol(sym))
}

fn eval_annotation_name(
    id: &str,
    env: &ModuleEnv<'_>,
    span: Option<&SourceSpan>,
) -> Result<AnnVal, PyAuthoringError> {
    if id.starts_with("__") {
        return Err(PyAuthoringError::with_optional_span(
            CODE_UNSAFE_ANNOTATION,
            format!("dunder name '{id}' is forbidden in field annotations"),
            span.cloned(),
        ));
    }
    if let Some(&sym) = ANNOTATION_ALLOWLIST.iter().find(|&&s| s == id) {
        return eval_annotation_allowlist_symbol(sym, span);
    }
    let Some(bound) = env.bindings.get(id) else {
        return Err(PyAuthoringError::with_optional_span(
            CODE_UNSAFE_ANNOTATION,
            format!("unknown symbol '{id}' in field annotation"),
            span.cloned(),
        ));
    };
    eval_value_to_ann_val(id, bound, env, span)
}

fn eval_value_to_ann_val(
    id: &str,
    val: &EvalValue<'_>,
    env: &ModuleEnv<'_>,
    span: Option<&SourceSpan>,
) -> Result<AnnVal, PyAuthoringError> {
    match val {
        EvalValue::None => Ok(AnnVal::None),
        EvalValue::Bool(b) => Ok(AnnVal::Bool(*b)),
        EvalValue::Int(digits) => Ok(AnnVal::Int(digits.clone())),
        EvalValue::Str(s) => Ok(AnnVal::Str(s.clone())),
        EvalValue::Decimal(d) => Ok(AnnVal::Decimal(d.clone())),
        EvalValue::ModuleNamespace(m) => Ok(AnnVal::ModuleNamespace(m)),
        EvalValue::BuiltinSymbol(sym) => {
            if let Some(&allowed) = ANNOTATION_ALLOWLIST.iter().find(|&&s| s == *sym) {
                eval_annotation_allowlist_symbol(allowed, span)
            } else {
                Err(PyAuthoringError::with_optional_span(
                    CODE_UNSAFE_ANNOTATION,
                    format!("symbol '{id}' is not an admitted annotation binding"),
                    span.cloned(),
                ))
            }
        }
        EvalValue::EnumClass(name) => Ok(AnnVal::EnumClass(name.clone())),
        EvalValue::EnumMember(m) => Ok(AnnVal::EnumMember(m.clone())),
        EvalValue::ConceptClass(py_name) => {
            let norn_name = env
                .concepts
                .get(py_name)
                .map_or_else(String::new, |c| c.norn_name.clone());
            Ok(AnnVal::ConceptClass {
                py_name: py_name.clone(),
                norn_name,
            })
        }
        EvalValue::UnitRef(u) => Ok(AnnVal::UnitRef(u.clone())),
        EvalValue::Marking(m) => Ok(AnnVal::Marking(m.clone())),
        EvalValue::FieldMeta(fm) => Ok(AnnVal::FieldMeta(fm.clone())),
        EvalValue::PropertyTypeSpec(spec) => Ok(AnnVal::PropertyTypeSpec(spec.clone())),
        EvalValue::LinkSpec(spec) => Ok(AnnVal::LinkSpec(spec.clone())),
        EvalValue::Tuple(_)
        | EvalValue::List(_)
        | EvalValue::Dict(_)
        | EvalValue::ProtocolClass
        | EvalValue::DependencyPin(_)
        | EvalValue::KeySpec(_)
        | EvalValue::FieldDefault(_)
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
        | EvalValue::OpaqueObject => Err(PyAuthoringError::with_optional_span(
            CODE_UNSAFE_ANNOTATION,
            format!("symbol '{id}' is not an admitted annotation binding"),
            span.cloned(),
        )),
    }
}

fn eval_annotation_subscript(
    sub: &ExprSubscript,
    env: &ModuleEnv<'_>,
    span: Option<&SourceSpan>,
    depth: usize,
) -> Result<AnnVal, PyAuthoringError> {
    let base = eval_annotation_ast(&sub.value, env, span, depth)?;
    let args: Vec<AnnVal> = match sub.slice.as_ref() {
        Expr::Tuple(tuple) => {
            let mut out = Vec::with_capacity(tuple.elts.len());
            for elt in &tuple.elts {
                out.push(eval_annotation_ast(elt, env, span, depth)?);
            }
            out
        }
        single => vec![eval_annotation_ast(single, env, span, depth)?],
    };

    let AnnVal::Symbol(sym) = base else {
        return Err(PyAuthoringError::with_optional_span(
            CODE_UNSAFE_ANNOTATION,
            "subscript target is not admitted in field annotations",
            span.cloned(),
        ));
    };

    match sym {
        "Annotated" => {
            let mut iter = args.into_iter();
            let Some(base_ty) = iter.next() else {
                return Err(PyAuthoringError::with_optional_span(
                    CODE_UNSAFE_ANNOTATION,
                    "Annotated requires a base type and metadata",
                    span.cloned(),
                ));
            };
            let metadata: Vec<AnnVal> = iter.collect();
            if metadata.is_empty() {
                return Err(PyAuthoringError::with_optional_span(
                    CODE_UNSAFE_ANNOTATION,
                    "Annotated requires a base type and metadata",
                    span.cloned(),
                ));
            }
            Ok(AnnVal::Annotated(Box::new(AnnotatedNode {
                base: base_ty,
                metadata,
            })))
        }
        "Optional" => Ok(AnnVal::Nullable(Box::new(take_one(
            args, "Optional", span,
        )?))),
        "Union" => Ok(AnnVal::Union(args)),
        "Literal" => Ok(AnnVal::Literal(args)),
        "Field" => Ok(AnnVal::Field(Box::new(take_one(args, "Field", span)?))),
        "Link" => Ok(AnnVal::Link(Box::new(take_one(args, "Link", span)?))),
        "Quantity" => Ok(AnnVal::Quantity(Box::new(take_one(
            args, "Quantity", span,
        )?))),
        "Money" => Ok(AnnVal::Money(Box::new(take_one(args, "Money", span)?))),
        "Rate" => {
            let [currency, unit] = take_two(args, "Rate", span)?;
            Ok(AnnVal::Rate(Box::new(RateNode { currency, unit })))
        }
        _ => Err(PyAuthoringError::with_optional_span(
            CODE_UNSAFE_ANNOTATION,
            format!("subscript on '{sym}' is not admitted in field annotations"),
            span.cloned(),
        )),
    }
}

fn take_one(
    args: Vec<AnnVal>,
    label: &str,
    span: Option<&SourceSpan>,
) -> Result<AnnVal, PyAuthoringError> {
    let [only]: [AnnVal; 1] = args.try_into().ok().ok_or_else(|| {
        PyAuthoringError::with_optional_span(
            CODE_UNSAFE_ANNOTATION,
            format!("{label} expects 1 type argument"),
            span.cloned(),
        )
    })?;
    Ok(only)
}

fn take_two(
    args: Vec<AnnVal>,
    label: &str,
    span: Option<&SourceSpan>,
) -> Result<[AnnVal; 2], PyAuthoringError> {
    args.try_into().ok().ok_or_else(|| {
        PyAuthoringError::with_optional_span(
            CODE_UNSAFE_ANNOTATION,
            format!("{label} expects 2 type arguments"),
            span.cloned(),
        )
    })
}

fn eval_annotation_call(
    call: &ExprCall,
    env: &ModuleEnv<'_>,
    span: Option<&SourceSpan>,
    depth: usize,
) -> Result<AnnVal, PyAuthoringError> {
    for arg in &call.arguments.args {
        if matches!(arg, Expr::Starred(_)) {
            return Err(PyAuthoringError::with_optional_span(
                CODE_UNSAFE_ANNOTATION,
                "starred args forbidden in annotations",
                span.cloned(),
            ));
        }
    }
    for kw in &call.arguments.keywords {
        if kw.arg.is_none() {
            return Err(PyAuthoringError::with_optional_span(
                CODE_UNSAFE_ANNOTATION,
                "kwargs unpacking forbidden in annotations",
                span.cloned(),
            ));
        }
    }

    let callee = eval_annotation_ast(&call.func, env, span, depth)?;
    let AnnVal::Symbol(fn_name) = callee else {
        return Err(PyAuthoringError::with_optional_span(
            CODE_UNSAFE_ANNOTATION,
            "call target is not an admitted Norn schema annotation constructor",
            span.cloned(),
        ));
    };
    if !ANNOTATION_CALLABLES.contains(&fn_name) {
        return Err(PyAuthoringError::with_optional_span(
            CODE_UNSAFE_ANNOTATION,
            format!("call to '{fn_name}' is not an admitted Norn schema annotation constructor"),
            span.cloned(),
        ));
    }

    let mut pos = Vec::with_capacity(call.arguments.args.len());
    for arg in &call.arguments.args {
        pos.push(eval_annotation_ast(arg, env, span, depth)?);
    }
    let mut kwargs: Vec<(String, AnnVal)> = Vec::with_capacity(call.arguments.keywords.len());
    for kw in &call.arguments.keywords {
        if let Some(arg_id) = &kw.arg {
            let val = eval_annotation_ast(&kw.value, env, span, depth)?;
            kwargs.push((arg_id.id.as_str().to_owned(), val));
        }
    }

    dispatch_annotation_ctor(fn_name, pos, kwargs, env, span)
}
