//! Class declaration collector (`StrEnum`, `Enum`, `Protocol`, `Concept`).

use ruff_python_ast::{Decorator, Expr, ExprCall, Stmt, StmtClassDef, StmtFunctionDef};
use ruff_text_size::Ranged;

use crate::error::{
    CODE_FLOW_LITERAL, CODE_FORBIDDEN_SYNTAX, CODE_RAW_EXPR, CODE_RAW_IR, CODE_SCHEMA,
    PyAuthoringError, SourceSpan,
};
use crate::line_table::LineTable;
use crate::schema::env::{
    ArtifactEntry, ConceptDecl, ConceptFieldAst, EnumDecl, EnumMemberVal, EvalValue, InvariantAst,
    ModuleEnv,
};
use crate::schema::eval::eval_expr;
use crate::schema::eval_ctors::{
    eval_value_to_field_default, expect_bool, expect_seq, expect_str, expect_str_seq,
};
use crate::schema::lower::push_unique_emission;
use crate::schema::types::{InvariantClassification, RoleKind, is_valid_object_type_name};

#[derive(Clone, Copy, PartialEq, Eq)]
enum ClassKind {
    Concept,
    StrEnum,
    PlainEnum,
    Protocol,
}

/// Evaluates and registers a top-level `class` statement (`Concept`, `StrEnum`, `Enum`, or `Protocol`).
pub(crate) fn collect_class_def<'a>(
    class_def: &'a StmtClassDef,
    env: &mut ModuleEnv<'a>,
    lines: &LineTable<'_>,
    span: &SourceSpan,
) -> Result<(), PyAuthoringError> {
    let py_name = class_def.name.id.as_str().to_owned();
    let (kind, base_concept_names) = classify_class_bases(class_def, &py_name, env, lines, span)?;

    match kind {
        ClassKind::StrEnum => collect_enum_class(class_def, &py_name, true, env, lines, span),
        ClassKind::PlainEnum => collect_enum_class(class_def, &py_name, false, env, lines, span),
        ClassKind::Protocol => {
            env.bindings.insert(py_name, EvalValue::ProtocolClass);
            Ok(())
        }
        ClassKind::Concept => {
            collect_concept_class(class_def, py_name, base_concept_names, env, lines, span)
        }
    }
}

fn classify_class_bases(
    class_def: &StmtClassDef,
    py_name: &str,
    env: &ModuleEnv<'_>,
    lines: &LineTable<'_>,
    span: &SourceSpan,
) -> Result<(ClassKind, Vec<String>), PyAuthoringError> {
    let bases: &[Expr] = class_def.arguments.as_deref().map_or(&[], |a| &a.args);
    if bases.is_empty() {
        return Err(PyAuthoringError::spanned(
            CODE_FORBIDDEN_SYNTAX,
            format!("authored class '{py_name}' must inherit from Concept, StrEnum, or Protocol"),
            span.clone(),
        ));
    }

    let mut kind: Option<ClassKind> = None;
    let mut base_concepts = Vec::new();
    for base_expr in bases {
        let unwrapped = match base_expr {
            Expr::Subscript(sub) => sub.value.as_ref(),
            other => other,
        };
        let resolved = eval_expr(unwrapped, env, lines)?;
        let base_kind = classify_single_base(resolved, py_name, &mut base_concepts, env, span)?;
        if kind.is_none() {
            kind = Some(base_kind);
        }
    }

    Ok((kind.unwrap_or(ClassKind::Protocol), base_concepts))
}

fn classify_single_base(
    resolved: EvalValue<'_>,
    py_name: &str,
    base_concepts: &mut Vec<String>,
    env: &ModuleEnv<'_>,
    span: &SourceSpan,
) -> Result<ClassKind, PyAuthoringError> {
    if matches!(resolved, EvalValue::BuiltinSymbol("Expr")) {
        return Err(PyAuthoringError::spanned(
            CODE_RAW_EXPR,
            "Expr cannot be subclassed",
            span.clone(),
        ));
    }
    if matches!(resolved, EvalValue::BuiltinSymbol("PropertyTypeSpec")) {
        return Err(PyAuthoringError::spanned(
            CODE_RAW_IR,
            "PropertyTypeSpec cannot be subclassed",
            span.clone(),
        ));
    }
    if matches!(resolved, EvalValue::BuiltinSymbol("MaskSpec")) {
        return Err(PyAuthoringError::spanned(
            CODE_RAW_IR,
            "MaskSpec cannot be subclassed",
            span.clone(),
        ));
    }
    if matches!(resolved, EvalValue::BuiltinSymbol("Concept")) {
        return Ok(ClassKind::Concept);
    }
    if let EvalValue::ConceptClass(parent_py) = resolved {
        base_concepts.push(parent_py);
        return Ok(ClassKind::Concept);
    }
    if matches!(resolved, EvalValue::BuiltinSymbol("StrEnum")) {
        return Ok(ClassKind::StrEnum);
    }
    if matches!(resolved, EvalValue::BuiltinSymbol("Enum")) {
        return Ok(ClassKind::PlainEnum);
    }
    if let EvalValue::EnumClass(parent_enum) = resolved {
        return Ok(
            if env.enums.get(&parent_enum).is_some_and(|e| e.is_str_enum) {
                ClassKind::StrEnum
            } else {
                ClassKind::PlainEnum
            },
        );
    }
    if matches!(
        resolved,
        EvalValue::BuiltinSymbol("Protocol") | EvalValue::ProtocolClass
    ) {
        return Ok(ClassKind::Protocol);
    }
    Err(PyAuthoringError::spanned(
        CODE_FORBIDDEN_SYNTAX,
        format!("authored class '{py_name}' may only inherit from Concept, StrEnum, or Protocol"),
        span.clone(),
    ))
}

fn collect_enum_class(
    class_def: &StmtClassDef,
    py_name: &str,
    is_str_enum: bool,
    env: &mut ModuleEnv<'_>,
    lines: &LineTable<'_>,
    span: &SourceSpan,
) -> Result<(), PyAuthoringError> {
    let mut members = Vec::new();
    let mut auto_counter: u64 = 1;

    for item in &class_def.body {
        let item_span = lines.span_at(item.range().start());
        match item {
            Stmt::Pass(_) => {}
            Stmt::Expr(expr_stmt) if matches!(expr_stmt.value.as_ref(), Expr::StringLiteral(_)) => {
            }
            Stmt::Assign(assign) => {
                collect_enum_assign(
                    assign,
                    py_name,
                    is_str_enum,
                    &mut auto_counter,
                    &mut members,
                    env,
                    lines,
                )?;
            }
            Stmt::FunctionDef(_) => {
                return Err(PyAuthoringError::spanned(
                    CODE_FORBIDDEN_SYNTAX,
                    format!("methods are forbidden on enum class '{py_name}'"),
                    item_span,
                ));
            }
            Stmt::Return(_)
            | Stmt::Delete(_)
            | Stmt::TypeAlias(_)
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
            | Stmt::ClassDef(_)
            | Stmt::Break(_)
            | Stmt::Continue(_)
            | Stmt::IpyEscapeCommand(_) => {
                return Err(PyAuthoringError::spanned(
                    CODE_SCHEMA,
                    format!("unsupported statement in enum class '{py_name}'"),
                    span.clone(),
                ));
            }
        }
    }

    env.enums.insert(
        py_name.to_owned(),
        EnumDecl {
            py_name: py_name.to_owned(),
            is_str_enum,
            members,
        },
    );
    env.bindings
        .insert(py_name.to_owned(), EvalValue::EnumClass(py_name.to_owned()));
    Ok(())
}

fn collect_enum_assign(
    assign: &ruff_python_ast::StmtAssign,
    py_name: &str,
    is_str_enum: bool,
    auto_counter: &mut u64,
    members: &mut Vec<(String, EnumMemberVal)>,
    env: &ModuleEnv<'_>,
    lines: &LineTable<'_>,
) -> Result<(), PyAuthoringError> {
    let item_span = lines.span_at(assign.range().start());
    let is_auto = is_auto_call(&assign.value, env, lines);
    for target in &assign.targets {
        let Expr::Name(name) = target else {
            return Err(PyAuthoringError::spanned(
                CODE_SCHEMA,
                format!("enum '{py_name}' member target must be an identifier"),
                item_span.clone(),
            ));
        };
        let member_name = name.id.as_str().to_owned();
        let val = if is_auto {
            if is_str_enum {
                EnumMemberVal::Str(member_name.to_ascii_lowercase())
            } else {
                let digits = auto_counter.to_string();
                *auto_counter = auto_counter.saturating_add(1);
                EnumMemberVal::Int(digits)
            }
        } else {
            let evaluated = eval_expr(&assign.value, env, lines)?;
            if let EvalValue::Str(s) = evaluated {
                EnumMemberVal::Str(s)
            } else if !is_str_enum && let EvalValue::Int(digits) = evaluated {
                EnumMemberVal::Int(digits)
            } else {
                return Err(PyAuthoringError::spanned(
                    CODE_SCHEMA,
                    format!("enum '{py_name}.{member_name}' must have a string literal value"),
                    item_span.clone(),
                ));
            }
        };
        members.push((member_name, val));
    }
    Ok(())
}

fn is_auto_call(expr: &Expr, env: &ModuleEnv<'_>, lines: &LineTable<'_>) -> bool {
    if let Expr::Call(call) = expr
        && call.arguments.args.is_empty()
        && call.arguments.keywords.is_empty()
        && let Ok(EvalValue::BuiltinSymbol("auto")) = eval_expr(&call.func, env, lines)
    {
        return true;
    }
    false
}

fn resolve_concept_ref(
    val: EvalValue<'_>,
    env: &ModuleEnv<'_>,
    label: &str,
    span: &SourceSpan,
) -> Result<String, PyAuthoringError> {
    if let EvalValue::Str(s) = val {
        return Ok(s);
    }
    if let EvalValue::ConceptClass(py_name) = val {
        let norn = env
            .concepts
            .get(&py_name)
            .map_or("", |c| c.norn_name.as_str());
        return Ok(norn.to_owned());
    }
    Err(PyAuthoringError::spanned(
        CODE_SCHEMA,
        format!("{label} must be a Concept subclass or string"),
        span.clone(),
    ))
}

fn collect_concept_class<'a>(
    class_def: &'a StmtClassDef,
    py_name: String,
    base_py_names: Vec<String>,
    env: &mut ModuleEnv<'a>,
    lines: &LineTable<'_>,
    span: &SourceSpan,
) -> Result<(), PyAuthoringError> {
    let mut decl =
        parse_concept_header(class_def, py_name.clone(), base_py_names, env, lines, span)?;
    if !decl.norn_name.is_empty() && !is_valid_object_type_name(&decl.norn_name) {
        return Err(PyAuthoringError::spanned(
            CODE_FLOW_LITERAL,
            format!(
                "type name '{}' must be a dotted lower-case namespace and a PascalCase name (e.g. hr.Employee)",
                decl.norn_name
            ),
            span.clone(),
        ));
    }

    collect_concept_body(class_def, &mut decl, env, lines)?;

    let emit = !decl.norn_name.is_empty();
    env.concepts.insert(py_name.clone(), decl);
    env.bindings
        .insert(py_name.clone(), EvalValue::ConceptClass(py_name.clone()));
    if emit {
        push_unique_emission(&mut env.emission_order, ArtifactEntry::Concept { py_name });
    }
    Ok(())
}

fn parse_concept_header<'a>(
    class_def: &'a StmtClassDef,
    py_name: String,
    base_py_names: Vec<String>,
    env: &ModuleEnv<'a>,
    lines: &LineTable<'_>,
    span: &SourceSpan,
) -> Result<ConceptDecl<'a>, PyAuthoringError> {
    let mut decl = ConceptDecl {
        py_name,
        norn_name: String::new(),
        title: String::new(),
        version: String::new(),
        is_interface: false,
        role: None,
        description: None,
        extends: None,
        implements: Vec::new(),
        interface_extends: Vec::new(),
        markings: Vec::new(),
        identity: Vec::new(),
        dependencies: Vec::new(),
        base_py_names,
        fields: Vec::new(),
        lifecycles: Vec::new(),
        invariants: Vec::new(),
        span: span.clone(),
    };

    for kw in class_def.keywords() {
        let Some(arg_id) = &kw.arg else {
            return Err(PyAuthoringError::spanned(
                CODE_SCHEMA,
                "`**kwargs` unpacking is forbidden in Concept class headers",
                span.clone(),
            ));
        };
        let val = eval_expr(&kw.value, env, lines)?;
        apply_concept_kwarg(arg_id.id.as_str(), val, &mut decl, env, span)?;
    }

    if decl.title.is_empty() && !decl.norn_name.is_empty() {
        decl.title = decl
            .norn_name
            .rsplit('.')
            .next()
            .unwrap_or(&decl.norn_name)
            .to_owned();
    }
    Ok(decl)
}

fn apply_concept_kwarg<'a>(
    key: &str,
    val: EvalValue<'a>,
    decl: &mut ConceptDecl<'a>,
    env: &ModuleEnv<'a>,
    span: &SourceSpan,
) -> Result<(), PyAuthoringError> {
    match key {
        "name" => decl.norn_name = expect_str(val, "Concept.name", span)?,
        "title" => decl.title = expect_str(val, "Concept.title", span)?,
        "version" => decl.version = expect_str(val, "Concept.version", span)?,
        "interface" => decl.is_interface = expect_bool(val, "Concept.interface", span)?,
        "role" => {
            decl.role = if matches!(val, EvalValue::None) {
                None
            } else {
                Some(RoleKind::parse(
                    &expect_str(val, "Concept.role", span)?,
                    Some(span),
                )?)
            };
        }
        "description" => {
            decl.description = if matches!(val, EvalValue::None) {
                None
            } else {
                Some(expect_str(val, "Concept.description", span)?)
            };
        }
        "extends" => {
            decl.extends = if matches!(val, EvalValue::None) {
                None
            } else {
                Some(resolve_concept_ref(val, env, "Concept.extends", span)?)
            };
        }
        "implements" => {
            for item in expect_seq(val, "Concept.implements", span)? {
                decl.implements
                    .push(resolve_concept_ref(item, env, "Concept.implements", span)?);
            }
        }
        "interface_extends" => {
            for item in expect_seq(val, "Concept.interface_extends", span)? {
                decl.interface_extends.push(resolve_concept_ref(
                    item,
                    env,
                    "Concept.interface_extends",
                    span,
                )?);
            }
        }
        "markings" => decl.markings = expect_str_seq(val, "Concept.markings", span)?,
        "identity" => {
            for item in expect_seq(val, "Concept.identity", span)? {
                if let EvalValue::KeySpec(k) = item {
                    decl.identity.push(*k);
                } else {
                    return Err(PyAuthoringError::spanned(
                        CODE_SCHEMA,
                        "Concept.identity entries must be KeySpec instances",
                        span.clone(),
                    ));
                }
            }
        }
        "dependencies" => {
            for item in expect_seq(val, "Concept.dependencies", span)? {
                if let EvalValue::DependencyPin(p) = item {
                    decl.dependencies.push(*p);
                } else {
                    return Err(PyAuthoringError::spanned(
                        CODE_SCHEMA,
                        "Concept.dependencies entries must be DependencyPin instances",
                        span.clone(),
                    ));
                }
            }
        }
        other => {
            return Err(PyAuthoringError::spanned(
                CODE_SCHEMA,
                format!(
                    "unexpected class keyword '{other}' on Concept '{}'",
                    decl.py_name
                ),
                span.clone(),
            ));
        }
    }
    Ok(())
}

fn collect_concept_body<'a>(
    class_def: &'a StmtClassDef,
    decl: &mut ConceptDecl<'a>,
    env: &ModuleEnv<'a>,
    lines: &LineTable<'_>,
) -> Result<(), PyAuthoringError> {
    for stmt in &class_def.body {
        let item_span = lines.span_at(stmt.range().start());
        match stmt {
            Stmt::Pass(_) => {}
            Stmt::Expr(expr_stmt) if matches!(expr_stmt.value.as_ref(), Expr::StringLiteral(_)) => {
            }
            Stmt::AnnAssign(ann) => {
                let Expr::Name(name) = ann.target.as_ref() else {
                    return Err(PyAuthoringError::spanned(
                        CODE_SCHEMA,
                        "Concept field target must be a plain identifier",
                        item_span,
                    ));
                };
                let field_name = name.id.as_str();
                if field_name.starts_with('_') {
                    continue;
                }
                let default = match ann.value.as_deref() {
                    None => None,
                    Some(val_expr) => {
                        let evaluated = eval_expr(val_expr, env, lines)?;
                        Some(eval_value_to_field_default(evaluated, &item_span)?)
                    }
                };
                decl.fields.push(ConceptFieldAst {
                    name: field_name.to_owned(),
                    annotation: &ann.annotation,
                    default,
                    span: item_span,
                });
            }
            Stmt::Assign(assign) => {
                let evaluated = eval_expr(&assign.value, env, lines)?;
                if let EvalValue::LifecycleSpec(lc) = evaluated {
                    decl.lifecycles.push(*lc);
                }
            }
            Stmt::FunctionDef(func_def) => {
                if let Some(inv) = parse_invariant_decorator(func_def, env, lines, &item_span)? {
                    decl.invariants.push(inv);
                }
            }
            Stmt::Return(_)
            | Stmt::Delete(_)
            | Stmt::TypeAlias(_)
            | Stmt::AugAssign(_)
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
            | Stmt::ClassDef(_)
            | Stmt::Break(_)
            | Stmt::Continue(_)
            | Stmt::IpyEscapeCommand(_) => {
                return Err(PyAuthoringError::spanned(
                    CODE_FORBIDDEN_SYNTAX,
                    format!("unsupported statement in Concept '{}'", decl.py_name),
                    item_span,
                ));
            }
        }
    }
    Ok(())
}

fn parse_invariant_decorator<'a>(
    func_def: &'a StmtFunctionDef,
    env: &ModuleEnv<'a>,
    lines: &LineTable<'_>,
    span: &SourceSpan,
) -> Result<Option<InvariantAst<'a>>, PyAuthoringError> {
    if func_def.decorator_list.is_empty() {
        return Ok(None);
    }
    let decs: &[Decorator] = &func_def.decorator_list;
    let [only_dec] = decs else {
        return Err(PyAuthoringError::spanned(
            CODE_SCHEMA,
            "Concept method supports at most one `@invariant(...)` decorator",
            span.clone(),
        ));
    };
    let Expr::Call(call) = &only_dec.expression else {
        return Err(PyAuthoringError::spanned(
            CODE_SCHEMA,
            "unsupported decorator on Concept method (expected `@invariant(...)`)",
            span.clone(),
        ));
    };
    let callee = eval_expr(&call.func, env, lines)?;
    if !matches!(callee, EvalValue::BuiltinSymbol("invariant")) {
        return Err(PyAuthoringError::spanned(
            CODE_SCHEMA,
            "unsupported decorator on Concept method (expected `@invariant(...)`)",
            span.clone(),
        ));
    }
    Ok(Some(extract_invariant_args(
        call, func_def, env, lines, span,
    )?))
}

fn extract_invariant_args<'a>(
    call: &'a ExprCall,
    func_def: &'a StmtFunctionDef,
    env: &ModuleEnv<'a>,
    lines: &LineTable<'_>,
    span: &SourceSpan,
) -> Result<InvariantAst<'a>, PyAuthoringError> {
    let mut id = None;
    let mut message = None;
    let mut description = None;
    let mut classification = InvariantClassification::Integrity;

    for (idx, arg) in call.arguments.args.iter().enumerate() {
        let val = eval_expr(arg, env, lines)?;
        if idx == 0 {
            id = Some(expect_str(val, "invariant.id", span)?);
        } else {
            return Err(PyAuthoringError::spanned(
                CODE_SCHEMA,
                "@invariant() takes at most 1 positional argument (id)",
                span.clone(),
            ));
        }
    }
    for kw in &call.arguments.keywords {
        let Some(arg_id) = &kw.arg else {
            return Err(PyAuthoringError::spanned(
                CODE_SCHEMA,
                "`**kwargs` unpacking is forbidden in @invariant(...)",
                span.clone(),
            ));
        };
        let val = eval_expr(&kw.value, env, lines)?;
        match arg_id.id.as_str() {
            "id" => id = Some(expect_str(val, "invariant.id", span)?),
            "message" => message = Some(expect_str(val, "invariant.message", span)?),
            "description" => {
                description = if matches!(val, EvalValue::None) {
                    None
                } else {
                    Some(expect_str(val, "invariant.description", span)?)
                };
            }
            "classification" => {
                classification = InvariantClassification::parse(
                    &expect_str(val, "invariant.classification", span)?,
                    Some(span),
                )?;
            }
            other => {
                return Err(PyAuthoringError::spanned(
                    CODE_SCHEMA,
                    format!("unexpected keyword '{other}' in @invariant(...)"),
                    span.clone(),
                ));
            }
        }
    }

    Ok(InvariantAst {
        id: id.ok_or_else(|| {
            PyAuthoringError::spanned(CODE_SCHEMA, "@invariant() missing 'id'", span.clone())
        })?,
        message: message.ok_or_else(|| {
            PyAuthoringError::spanned(CODE_SCHEMA, "@invariant() missing 'message'", span.clone())
        })?,
        func: func_def,
        description,
        classification,
    })
}
