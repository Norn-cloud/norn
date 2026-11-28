//! Two-phase static Python-to-`EntityIR`/`LawIR` module elaborator
//! (zero `CPython` execution).

use std::collections::BTreeMap;

use ruff_python_ast::{Decorator, Expr, ExprCall, ModModule, Stmt, StmtFunctionDef};
use ruff_text_size::Ranged;

use crate::canonical::{
    DependencyPin, EnvelopeHeader, EnvelopeOptions, JsonValue, build_envelope, canonical_json,
    canonical_norn1_bytes, norn1_sha256_bytes,
};
use crate::error::{
    CODE_ELABORATION, CODE_FORBIDDEN_SYNTAX, CODE_SCHEMA, PyAuthoringError, SourceSpan,
};
use crate::expr::LowerContext;
use crate::expr_lower::lower_function_def;
use crate::line_table::LineTable;
use crate::sandbox::{
    ALLOWED_IMPORT_ROOTS, SandboxLimits, allowed_import_symbols, parse_and_validate_module,
};
use crate::schema::ElaborationResult;
use crate::schema::env::{ArtifactEntry, CallbackRef, EvalValue, LawDeclAst, ModuleEnv};
use crate::schema::eval::eval_expr;
use crate::schema::eval_ctors::{expect_seq, expect_str};
use crate::schema::lower_class::collect_class_def;
use crate::schema::lower_concept::lower_concept;
use crate::schema::types::{LawModality, LawSeverity, ScopeRootKind};

/// Runs Layer-1 sandbox preflight and two-phase static elaboration on Python authoring source.
pub(crate) fn elaborate_module_source(
    source: &str,
    options: &EnvelopeOptions,
    limits: SandboxLimits,
) -> Result<ElaborationResult, PyAuthoringError> {
    let lines = LineTable::new(&options.source_path, source);
    let (module, report) = parse_and_validate_module(source, &lines, limits)?;

    let env = collect_module_env(&module, &lines)?;
    let artifacts = emit_artifacts(&env, &lines, options)?;

    let artifacts_value = JsonValue::Array(artifacts.clone());
    let canonical_text = canonical_json(&artifacts_value)?;
    let norn1_bytes = canonical_norn1_bytes(&artifacts_value)?;
    let norn1_sha256 = norn1_sha256_bytes(&norn1_bytes);

    Ok(ElaborationResult {
        artifacts,
        canonical_text,
        norn1_bytes,
        norn1_sha256,
        source_map: report.spans,
    })
}

fn collect_module_env<'a>(
    module: &'a ModModule,
    lines: &LineTable<'_>,
) -> Result<ModuleEnv<'a>, PyAuthoringError> {
    let mut env = ModuleEnv::new();
    for stmt in &module.body {
        let span = lines.span_at(stmt.range().start());
        match stmt {
            Stmt::Import(import_stmt) => collect_import(import_stmt, &mut env),
            Stmt::ImportFrom(import_from) => collect_import_from(import_from, &mut env),
            Stmt::ClassDef(class_def) => collect_class_def(class_def, &mut env, lines, &span)?,
            Stmt::FunctionDef(func_def) => collect_function_def(func_def, &mut env, lines, &span)?,
            Stmt::Assign(assign) => collect_assign(assign, &mut env, lines, &span)?,
            Stmt::AnnAssign(ann_assign) => collect_ann_assign(ann_assign, &mut env, lines, &span)?,
            Stmt::Expr(expr_stmt) => {
                if !matches!(expr_stmt.value.as_ref(), Expr::StringLiteral(_)) {
                    let _val = eval_expr(&expr_stmt.value, &env, lines)?;
                }
            }
            Stmt::Pass(_) => {}
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
            | Stmt::Global(_)
            | Stmt::Nonlocal(_)
            | Stmt::Break(_)
            | Stmt::Continue(_)
            | Stmt::IpyEscapeCommand(_) => {
                return Err(PyAuthoringError::spanned(
                    CODE_FORBIDDEN_SYNTAX,
                    "unsupported top-level statement in declarative Norn authoring module",
                    span,
                ));
            }
        }
    }
    Ok(env)
}

fn collect_import(import_stmt: &ruff_python_ast::StmtImport, env: &mut ModuleEnv<'_>) {
    for alias in &import_stmt.names {
        let raw_mod = alias.name.id.as_str();
        if let Some(&static_mod) = ALLOWED_IMPORT_ROOTS.iter().find(|&&m| m == raw_mod) {
            let bind_name = alias
                .asname
                .as_ref()
                .map_or(raw_mod, |as_id| as_id.id.as_str());
            env.bindings
                .insert(bind_name.to_owned(), EvalValue::ModuleNamespace(static_mod));
        }
    }
}

fn collect_import_from(import_from: &ruff_python_ast::StmtImportFrom, env: &mut ModuleEnv<'_>) {
    let Some(module) = &import_from.module else {
        return;
    };
    let Some(allowed) = allowed_import_symbols(module.id.as_str()) else {
        return;
    };
    for alias in &import_from.names {
        let sym_name = alias.name.id.as_str();
        if let Some(&static_sym) = allowed.iter().find(|&&s| s == sym_name) {
            let bind_name = alias
                .asname
                .as_ref()
                .map_or(sym_name, |as_id| as_id.id.as_str());
            env.bindings
                .insert(bind_name.to_owned(), EvalValue::BuiltinSymbol(static_sym));
        }
    }
}

fn collect_assign<'a>(
    assign: &'a ruff_python_ast::StmtAssign,
    env: &mut ModuleEnv<'a>,
    lines: &LineTable<'_>,
    span: &SourceSpan,
) -> Result<(), PyAuthoringError> {
    let value = eval_expr(&assign.value, env, lines)?;
    let is_catalog = matches!(value, EvalValue::MarkingsCatalog(_));
    for target in &assign.targets {
        let Expr::Name(name) = target else {
            return Err(PyAuthoringError::spanned(
                CODE_ELABORATION,
                "only plain identifier assignment targets are supported at module scope",
                span.clone(),
            ));
        };
        let key = name.id.as_str().to_owned();
        if is_catalog {
            push_unique_emission(
                &mut env.emission_order,
                ArtifactEntry::MarkingsCatalog {
                    binding_name: key.clone(),
                },
            );
        }
        env.bindings.insert(key, value.clone());
    }
    Ok(())
}

fn collect_ann_assign<'a>(
    ann: &'a ruff_python_ast::StmtAnnAssign,
    env: &mut ModuleEnv<'a>,
    lines: &LineTable<'_>,
    span: &SourceSpan,
) -> Result<(), PyAuthoringError> {
    let Expr::Name(name) = ann.target.as_ref() else {
        return Err(PyAuthoringError::spanned(
            CODE_ELABORATION,
            "only plain identifier targets are supported in top-level annotated assignments",
            span.clone(),
        ));
    };
    if let Some(val_expr) = ann.value.as_deref() {
        let value = eval_expr(val_expr, env, lines)?;
        let key = name.id.as_str().to_owned();
        if matches!(value, EvalValue::MarkingsCatalog(_)) {
            push_unique_emission(
                &mut env.emission_order,
                ArtifactEntry::MarkingsCatalog {
                    binding_name: key.clone(),
                },
            );
        }
        env.bindings.insert(key, value);
    }
    Ok(())
}

fn collect_function_def<'a>(
    func_def: &'a StmtFunctionDef,
    env: &mut ModuleEnv<'a>,
    lines: &LineTable<'_>,
    span: &SourceSpan,
) -> Result<(), PyAuthoringError> {
    let fn_name = func_def.name.id.as_str().to_owned();
    if func_def.decorator_list.is_empty() {
        env.bindings
            .insert(fn_name, EvalValue::Callback(CallbackRef::Func(func_def)));
        return Ok(());
    }

    let decs: &[Decorator] = &func_def.decorator_list;
    let [only_dec] = decs else {
        return Err(PyAuthoringError::spanned(
            CODE_SCHEMA,
            format!("function '{fn_name}' supports at most one `@law(...)` decorator"),
            span.clone(),
        ));
    };
    let Expr::Call(call) = &only_dec.expression else {
        return Err(PyAuthoringError::spanned(
            CODE_SCHEMA,
            format!(
                "unsupported decorator on top-level function '{fn_name}' (expected `@law(...)`)"
            ),
            span.clone(),
        ));
    };
    let callee = eval_expr(&call.func, env, lines)?;
    if !matches!(callee, EvalValue::BuiltinSymbol("law")) {
        return Err(PyAuthoringError::spanned(
            CODE_SCHEMA,
            format!(
                "unsupported decorator on top-level function '{fn_name}' (expected `@law(...)`)"
            ),
            span.clone(),
        ));
    }

    let law_decl = parse_law_decorator(call, func_def, env, lines, span)?;
    let law_id = law_decl.id.clone();
    env.laws.insert(fn_name.clone(), law_decl);
    env.bindings
        .insert(fn_name.clone(), EvalValue::LawDecl(law_id));
    push_unique_emission(
        &mut env.emission_order,
        ArtifactEntry::Law {
            binding_name: fn_name,
        },
    );
    Ok(())
}

#[derive(Default)]
struct LawArgs {
    id: Option<String>,
    title: Option<String>,
    version: Option<String>,
    owner: Option<String>,
    subject_py_name: Option<String>,
    rationale: Option<String>,
    severity: LawSeverity,
    modality: LawModality,
    scope_root_kind: ScopeRootKind,
    scope_root_id: Option<String>,
    dependencies: Vec<DependencyPin>,
}

fn parse_law_decorator<'a>(
    call: &'a ExprCall,
    func_def: &'a StmtFunctionDef,
    env: &ModuleEnv<'a>,
    lines: &LineTable<'_>,
    span: &SourceSpan,
) -> Result<LawDeclAst<'a>, PyAuthoringError> {
    if !call.arguments.args.is_empty() {
        return Err(PyAuthoringError::spanned(
            CODE_SCHEMA,
            "@law(...) accepts keyword arguments only",
            span.clone(),
        ));
    }
    let mut args = LawArgs::default();
    for kw in &call.arguments.keywords {
        let Some(arg_id) = &kw.arg else {
            return Err(PyAuthoringError::spanned(
                CODE_SCHEMA,
                "`**kwargs` unpacking is forbidden in @law(...)",
                span.clone(),
            ));
        };
        let val = eval_expr(&kw.value, env, lines)?;
        apply_law_kwarg(arg_id.id.as_str(), val, &mut args, span)?;
    }
    Ok(LawDeclAst {
        id: require_law_field(args.id, "id", span)?,
        title: require_law_field(args.title, "title", span)?,
        version: require_law_field(args.version, "version", span)?,
        owner: require_law_field(args.owner, "owner", span)?,
        subject_py_name: require_law_field(args.subject_py_name, "subject", span)?,
        rationale: require_law_field(args.rationale, "rationale", span)?,
        predicate: func_def,
        severity: args.severity,
        modality: args.modality,
        scope_root_kind: args.scope_root_kind,
        scope_root_id: args.scope_root_id,
        dependencies: args.dependencies,
        span: span.clone(),
    })
}

fn require_law_field(
    val: Option<String>,
    field: &str,
    span: &SourceSpan,
) -> Result<String, PyAuthoringError> {
    val.ok_or_else(|| {
        PyAuthoringError::spanned(
            CODE_SCHEMA,
            format!("@law() missing '{field}'"),
            span.clone(),
        )
    })
}

fn apply_law_kwarg(
    key: &str,
    val: EvalValue<'_>,
    args: &mut LawArgs,
    span: &SourceSpan,
) -> Result<(), PyAuthoringError> {
    match key {
        "id" => args.id = Some(expect_str(val, "law.id", span)?),
        "title" => args.title = Some(expect_str(val, "law.title", span)?),
        "version" => args.version = Some(expect_str(val, "law.version", span)?),
        "owner" => args.owner = Some(expect_str(val, "law.owner", span)?),
        "subject" => {
            if let EvalValue::ConceptClass(py_name) = val {
                args.subject_py_name = Some(py_name);
            } else {
                return Err(PyAuthoringError::spanned(
                    CODE_SCHEMA,
                    "law.subject must be a declared Concept subclass",
                    span.clone(),
                ));
            }
        }
        "rationale" => args.rationale = Some(expect_str(val, "law.rationale", span)?),
        "severity" => {
            args.severity =
                LawSeverity::parse(&expect_str(val, "law.severity", span)?, Some(span))?;
        }
        "modality" => {
            args.modality =
                LawModality::parse(&expect_str(val, "law.modality", span)?, Some(span))?;
        }
        "scope_root_kind" => {
            args.scope_root_kind =
                ScopeRootKind::parse(&expect_str(val, "law.scope_root_kind", span)?, Some(span))?;
        }
        "scope_root_id" => {
            args.scope_root_id = if matches!(val, EvalValue::None) {
                None
            } else {
                Some(expect_str(val, "law.scope_root_id", span)?)
            };
        }
        "dependencies" => {
            for item in expect_seq(val, "law.dependencies", span)? {
                if let EvalValue::DependencyPin(pin) = item {
                    args.dependencies.push(*pin);
                } else {
                    return Err(PyAuthoringError::spanned(
                        CODE_SCHEMA,
                        "law.dependencies entries must be DependencyPin instances",
                        span.clone(),
                    ));
                }
            }
        }
        other => {
            return Err(PyAuthoringError::spanned(
                CODE_SCHEMA,
                format!("unexpected keyword '{other}' in @law(...)"),
                span.clone(),
            ));
        }
    }
    Ok(())
}

pub(crate) fn push_unique_emission(order: &mut Vec<ArtifactEntry>, entry: ArtifactEntry) {
    let exists = order.iter().any(|existing| match (existing, &entry) {
        (
            ArtifactEntry::MarkingsCatalog { binding_name: a },
            ArtifactEntry::MarkingsCatalog { binding_name: b },
        )
        | (ArtifactEntry::Law { binding_name: a }, ArtifactEntry::Law { binding_name: b }) => {
            a == b
        }
        (ArtifactEntry::Concept { py_name: a }, ArtifactEntry::Concept { py_name: b }) => a == b,
        (
            ArtifactEntry::MarkingsCatalog { .. }
            | ArtifactEntry::Concept { .. }
            | ArtifactEntry::Law { .. },
            ArtifactEntry::MarkingsCatalog { .. }
            | ArtifactEntry::Concept { .. }
            | ArtifactEntry::Law { .. },
        ) => false,
    });
    if !exists {
        order.push(entry);
    }
}

fn emit_artifacts(
    env: &ModuleEnv<'_>,
    lines: &LineTable<'_>,
    options: &EnvelopeOptions,
) -> Result<Vec<JsonValue>, PyAuthoringError> {
    let constants = env.callback_constants();
    let mut artifacts = Vec::with_capacity(env.emission_order.len());

    for entry in &env.emission_order {
        match entry {
            ArtifactEntry::MarkingsCatalog { binding_name } => {
                if let Some(EvalValue::MarkingsCatalog(catalog)) = env.bindings.get(binding_name) {
                    artifacts.push(catalog.lower(options)?);
                }
            }
            ArtifactEntry::Concept { py_name } => {
                if let Some(concept) = env.concepts.get(py_name)
                    && !concept.norn_name.is_empty()
                {
                    artifacts.push(lower_concept(concept, env, lines, &constants, options)?);
                }
            }
            ArtifactEntry::Law { binding_name } => {
                if let Some(law) = env.laws.get(binding_name) {
                    artifacts.push(lower_law(law, env, lines, &constants, options)?);
                }
            }
        }
    }

    Ok(artifacts)
}

fn lower_law(
    law: &LawDeclAst<'_>,
    env: &ModuleEnv<'_>,
    lines: &LineTable<'_>,
    constants: &BTreeMap<String, crate::expr::ZenLiteral>,
    options: &EnvelopeOptions,
) -> Result<JsonValue, PyAuthoringError> {
    let subject = env.concepts.get(&law.subject_py_name).ok_or_else(|| {
        PyAuthoringError::spanned(
            CODE_SCHEMA,
            format!(
                "law '{}' references unknown subject '{}'",
                law.id, law.subject_py_name
            ),
            law.span.clone(),
        )
    })?;
    if subject.norn_name.is_empty() {
        return Err(PyAuthoringError::spanned(
            CODE_SCHEMA,
            format!(
                "law '{}' subject '{}' has no declared Norn name",
                law.id, law.subject_py_name
            ),
            law.span.clone(),
        ));
    }

    let mut ctx = LowerContext::with_root("self")
        .with_allowed_fields(env.declared_field_names(&law.subject_py_name));
    ctx.constants = constants.clone();

    let expr = lower_function_def(law.predicate, lines, &ctx)?;
    let expr_zen = expr.to_zen()?;

    let deps = if law.dependencies.is_empty() {
        vec![DependencyPin::new(
            format!("{}.entity", subject.norn_name),
            subject.version.clone(),
        )]
    } else {
        law.dependencies.clone()
    };

    let payload = build_law_payload(law, &subject.norn_name, expr_zen, &options.source_path);
    let artifact_id = format!("{}.law", law.id);
    let namespace = law.id.split('.').next().unwrap_or("");
    let header = EnvelopeHeader {
        kind: "law",
        artifact_id: &artifact_id,
        namespace,
        name: &law.title,
        version: &law.version,
    };
    build_envelope(&header, payload, options, &deps)
}

fn build_law_payload(
    law: &LawDeclAst<'_>,
    subj_name: &str,
    expr_zen: String,
    source_path: &str,
) -> BTreeMap<String, JsonValue> {
    let mut modality_map = BTreeMap::new();
    modality_map.insert(
        "kind".to_owned(),
        JsonValue::String(law.modality.as_str().to_owned()),
    );

    let mut prov_map = BTreeMap::new();
    prov_map.insert(
        "source".to_owned(),
        JsonValue::String("authoring_sdk".to_owned()),
    );
    prov_map.insert(
        "sourcePath".to_owned(),
        JsonValue::String(source_path.to_owned()),
    );

    let mut response_map = BTreeMap::new();
    response_map.insert("expression".to_owned(), JsonValue::String(expr_zen));
    response_map.insert("kind".to_owned(), JsonValue::String("predicate".to_owned()));

    let mut root_map = BTreeMap::new();
    root_map.insert(
        "id".to_owned(),
        JsonValue::String(
            law.scope_root_id
                .clone()
                .unwrap_or_else(|| subj_name.to_owned()),
        ),
    );
    root_map.insert(
        "kind".to_owned(),
        JsonValue::String(law.scope_root_kind.as_str().to_owned()),
    );

    let mut scope_map = BTreeMap::new();
    scope_map.insert(
        "objectTypes".to_owned(),
        JsonValue::Array(vec![JsonValue::String(subj_name.to_owned())]),
    );
    scope_map.insert("root".to_owned(), JsonValue::Object(root_map));

    let mut payload = BTreeMap::new();
    payload.insert("id".to_owned(), JsonValue::String(law.id.clone()));
    payload.insert("lawVersion".to_owned(), JsonValue::String("0.1".to_owned()));
    payload.insert("modality".to_owned(), JsonValue::Object(modality_map));
    payload.insert("owner".to_owned(), JsonValue::String(law.owner.clone()));
    payload.insert("provenance".to_owned(), JsonValue::Object(prov_map));
    payload.insert(
        "rationale".to_owned(),
        JsonValue::String(law.rationale.clone()),
    );
    payload.insert("response".to_owned(), JsonValue::Object(response_map));
    payload.insert("scope".to_owned(), JsonValue::Object(scope_map));
    payload.insert(
        "severity".to_owned(),
        JsonValue::String(law.severity.as_str().to_owned()),
    );
    payload.insert(
        "subject".to_owned(),
        JsonValue::String(subj_name.to_owned()),
    );
    payload
}
