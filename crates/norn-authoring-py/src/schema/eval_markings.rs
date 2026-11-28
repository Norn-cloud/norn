//! Mask, markings catalog, lifecycle, and transition constructor evaluators
//! used by [`crate::schema::eval`].

use crate::error::{CODE_RAW_IR, CODE_SCHEMA, PyAuthoringError, SourceSpan};
use crate::schema::env::{CallbackRef, EvalValue, LifecycleAst, ModuleEnv, TransitionAst};
use crate::schema::eval_ctors::{expect_non_neg_u64, expect_seq, expect_str};
use crate::schema::types::{MarkingDecl, MarkingSeverity, MarkingsCatalog, MaskSpec};

pub(crate) fn eval_mask_ctor<'a>(
    sym: &str,
    pos: Vec<EvalValue<'a>>,
    kwargs: Vec<(String, EvalValue<'a>)>,
    span: &SourceSpan,
) -> Result<EvalValue<'a>, PyAuthoringError> {
    let spec = match sym {
        "mask_redacted" => MaskSpec::Redacted,
        "mask_null" => MaskSpec::Null,
        "mask_refuse" => MaskSpec::Refuse,
        "mask_last_n" => {
            let mut val = pos.into_iter().next();
            for (k, v) in kwargs {
                if k == "count" {
                    val = Some(v);
                } else {
                    return Err(PyAuthoringError::spanned(
                        CODE_SCHEMA,
                        format!("unexpected keyword '{k}' in mask_last_n()"),
                        span.clone(),
                    ));
                }
            }
            let count = expect_non_neg_u64(
                val.ok_or_else(|| {
                    PyAuthoringError::spanned(
                        CODE_SCHEMA,
                        "mask_last_n() requires count",
                        span.clone(),
                    )
                })?,
                "mask_last_n count",
                span,
            )?;
            MaskSpec::LastN(count)
        }
        "mask_pseudonym" => {
            let mut val = pos.into_iter().next();
            for (k, v) in kwargs {
                if k == "subject" {
                    val = Some(v);
                } else {
                    return Err(PyAuthoringError::spanned(
                        CODE_SCHEMA,
                        format!("unexpected keyword '{k}' in mask_pseudonym()"),
                        span.clone(),
                    ));
                }
            }
            let subject = expect_str(
                val.ok_or_else(|| {
                    PyAuthoringError::spanned(
                        CODE_SCHEMA,
                        "mask_pseudonym() requires subject",
                        span.clone(),
                    )
                })?,
                "mask_pseudonym subject",
                span,
            )?;
            MaskSpec::pseudonym(subject, Some(span))?
        }
        _ => {
            return Err(PyAuthoringError::spanned(
                CODE_SCHEMA,
                format!("unsupported mask constructor '{sym}'"),
                span.clone(),
            ));
        }
    };
    Ok(EvalValue::MaskSpec(spec))
}

#[derive(Default)]
struct MarkingDeclArgs<'a> {
    id: Option<EvalValue<'a>>,
    label: Option<EvalValue<'a>>,
    severity: Option<EvalValue<'a>>,
    category: Option<EvalValue<'a>>,
    mask: Option<EvalValue<'a>>,
}

pub(crate) fn eval_marking_decl<'a>(
    pos: Vec<EvalValue<'a>>,
    kwargs: Vec<(String, EvalValue<'a>)>,
    span: &SourceSpan,
) -> Result<EvalValue<'a>, PyAuthoringError> {
    let mut args = MarkingDeclArgs::default();
    for (idx, v) in pos.into_iter().enumerate() {
        match idx {
            0 => args.id = Some(v),
            1 => args.label = Some(v),
            2 => args.severity = Some(v),
            3 => args.category = Some(v),
            4 => args.mask = Some(v),
            _ => {
                return Err(PyAuthoringError::spanned(
                    CODE_SCHEMA,
                    "MarkingDecl() takes at most 5 positional arguments",
                    span.clone(),
                ));
            }
        }
    }
    for (k, v) in kwargs {
        match k.as_str() {
            "id" => args.id = Some(v),
            "label" => args.label = Some(v),
            "severity" => args.severity = Some(v),
            "category" => args.category = Some(v),
            "mask" => args.mask = Some(v),
            _ => {
                return Err(PyAuthoringError::spanned(
                    CODE_SCHEMA,
                    format!("unexpected keyword '{k}' in MarkingDecl()"),
                    span.clone(),
                ));
            }
        }
    }
    finish_marking_decl(args, span)
}

fn finish_marking_decl<'a>(
    args: MarkingDeclArgs<'a>,
    span: &SourceSpan,
) -> Result<EvalValue<'a>, PyAuthoringError> {
    let Some(EvalValue::MaskSpec(mask)) = args.mask else {
        return Err(PyAuthoringError::spanned(
            CODE_RAW_IR,
            "MarkingDecl.mask must be a closed MaskSpec (from mask_redacted/mask_null/mask_last_n/mask_refuse/mask_pseudonym)",
            span.clone(),
        ));
    };
    let id = expect_required_str(args.id, "MarkingDecl", "id", span)?;
    let label = expect_required_str(args.label, "MarkingDecl", "label", span)?;
    let sev_raw = expect_required_str(args.severity, "MarkingDecl", "severity", span)?;
    let severity = MarkingSeverity::parse(&sev_raw, Some(span))?;
    let category = expect_required_str(args.category, "MarkingDecl", "category", span)?;
    Ok(EvalValue::MarkingDecl(Box::new(MarkingDecl {
        id,
        label,
        severity,
        category,
        mask,
    })))
}

pub(crate) fn eval_markings_catalog<'a>(
    pos: Vec<EvalValue<'a>>,
    kwargs: Vec<(String, EvalValue<'a>)>,
    span: &SourceSpan,
) -> Result<EvalValue<'a>, PyAuthoringError> {
    let mut id = None;
    let mut title = None;
    let mut version = None;
    let mut markings_val = None;
    for (idx, v) in pos.into_iter().enumerate() {
        match idx {
            0 => id = Some(v),
            1 => title = Some(v),
            2 => version = Some(v),
            3 => markings_val = Some(v),
            _ => {
                return Err(PyAuthoringError::spanned(
                    CODE_SCHEMA,
                    "MarkingsCatalog() takes at most 4 positional arguments",
                    span.clone(),
                ));
            }
        }
    }
    for (k, v) in kwargs {
        match k.as_str() {
            "id" => id = Some(v),
            "title" => title = Some(v),
            "version" => version = Some(v),
            "markings" => markings_val = Some(v),
            _ => {
                return Err(PyAuthoringError::spanned(
                    CODE_SCHEMA,
                    format!("unexpected keyword '{k}' in MarkingsCatalog()"),
                    span.clone(),
                ));
            }
        }
    }
    let markings = extract_marking_decls(markings_val, span)?;
    Ok(EvalValue::MarkingsCatalog(Box::new(MarkingsCatalog {
        id: expect_required_str(id, "MarkingsCatalog", "id", span)?,
        title: expect_required_str(title, "MarkingsCatalog", "title", span)?,
        version: expect_required_str(version, "MarkingsCatalog", "version", span)?,
        markings,
    })))
}

fn extract_marking_decls(
    markings_val: Option<EvalValue<'_>>,
    span: &SourceSpan,
) -> Result<Vec<MarkingDecl>, PyAuthoringError> {
    let items = expect_seq(
        markings_val.ok_or_else(|| {
            PyAuthoringError::spanned(
                CODE_SCHEMA,
                "MarkingsCatalog() missing 'markings'",
                span.clone(),
            )
        })?,
        "MarkingsCatalog.markings",
        span,
    )?;
    let mut markings = Vec::with_capacity(items.len());
    for item in items {
        if let EvalValue::MarkingDecl(decl) = item {
            markings.push(*decl);
        } else {
            return Err(PyAuthoringError::spanned(
                CODE_SCHEMA,
                "MarkingsCatalog.markings entries must be MarkingDecl instances",
                span.clone(),
            ));
        }
    }
    Ok(markings)
}

fn expect_required_str(
    val: Option<EvalValue<'_>>,
    ctor: &str,
    field: &str,
    span: &SourceSpan,
) -> Result<String, PyAuthoringError> {
    let raw = val.ok_or_else(|| {
        PyAuthoringError::spanned(
            CODE_SCHEMA,
            format!("{ctor}() missing '{field}'"),
            span.clone(),
        )
    })?;
    expect_str(raw, &format!("{ctor}.{field}"), span)
}

fn enum_state_str(
    val: EvalValue<'_>,
    label: &str,
    span: &SourceSpan,
) -> Result<String, PyAuthoringError> {
    if let EvalValue::EnumMember(m) = val
        && let Some(s) = m.as_str_value()
    {
        return Ok(s.to_owned());
    }
    Err(PyAuthoringError::spanned(
        CODE_SCHEMA,
        format!("{label} must be a StrEnum member"),
        span.clone(),
    ))
}

fn enum_state_seq(
    val: EvalValue<'_>,
    label: &str,
    span: &SourceSpan,
) -> Result<Vec<String>, PyAuthoringError> {
    let items = expect_seq(val, label, span)?;
    let mut out = Vec::with_capacity(items.len());
    for item in items {
        out.push(enum_state_str(item, label, span)?);
    }
    Ok(out)
}

pub(crate) fn eval_transition<'a>(
    pos: Vec<EvalValue<'a>>,
    kwargs: Vec<(String, EvalValue<'a>)>,
    span: &SourceSpan,
) -> Result<EvalValue<'a>, PyAuthoringError> {
    let mut id = pos.into_iter().next();
    let mut from_val = None;
    let mut to_val = None;
    let mut guards_val = None;
    let mut description = None;
    let mut guard_refusal_code = None;
    for (k, v) in kwargs {
        match k.as_str() {
            "id" => id = Some(v),
            "from_" => from_val = Some(v),
            "to" => to_val = Some(v),
            "guards" => guards_val = Some(v),
            "description" => {
                description = if matches!(v, EvalValue::None) {
                    None
                } else {
                    Some(expect_str(v, "transition.description", span)?)
                };
            }
            "guard_refusal_code" => {
                guard_refusal_code = if matches!(v, EvalValue::None) {
                    None
                } else {
                    Some(expect_str(v, "transition.guard_refusal_code", span)?)
                };
            }
            _ => {
                return Err(PyAuthoringError::spanned(
                    CODE_SCHEMA,
                    format!("unexpected keyword '{k}' in transition()"),
                    span.clone(),
                ));
            }
        }
    }
    let guards = extract_guard_callbacks(guards_val, span)?;
    Ok(EvalValue::TransitionSpec(Box::new(TransitionAst {
        id: expect_required_str(id, "transition", "id", span)?,
        from_states: enum_state_seq(
            from_val.ok_or_else(|| {
                PyAuthoringError::spanned(CODE_SCHEMA, "transition() missing 'from_'", span.clone())
            })?,
            "transition.from_",
            span,
        )?,
        to_state: enum_state_str(
            to_val.ok_or_else(|| {
                PyAuthoringError::spanned(CODE_SCHEMA, "transition() missing 'to'", span.clone())
            })?,
            "transition.to",
            span,
        )?,
        guards,
        description,
        guard_refusal_code,
    })))
}

fn extract_guard_callbacks<'a>(
    guards_val: Option<EvalValue<'a>>,
    span: &SourceSpan,
) -> Result<Vec<CallbackRef<'a>>, PyAuthoringError> {
    let Some(gv) = guards_val else {
        return Ok(Vec::new());
    };
    let mut guards = Vec::new();
    for item in expect_seq(gv, "transition.guards", span)? {
        if let EvalValue::Callback(cb) = item {
            guards.push(cb);
        } else {
            return Err(PyAuthoringError::spanned(
                CODE_SCHEMA,
                "transition guards must be callback functions or lambdas",
                span.clone(),
            ));
        }
    }
    Ok(guards)
}

#[derive(Default)]
struct LifecycleArgs<'a> {
    property: Option<EvalValue<'a>>,
    enum_cls: Option<EvalValue<'a>>,
    initial: Option<EvalValue<'a>>,
    transitions: Option<EvalValue<'a>>,
    terminal: Option<EvalValue<'a>>,
    immutable: Option<EvalValue<'a>>,
    description: Option<String>,
}

pub(crate) fn eval_lifecycle<'a>(
    pos: Vec<EvalValue<'a>>,
    kwargs: Vec<(String, EvalValue<'a>)>,
    env: &ModuleEnv<'a>,
    span: &SourceSpan,
) -> Result<EvalValue<'a>, PyAuthoringError> {
    let mut args = LifecycleArgs::default();
    for (idx, v) in pos.into_iter().enumerate() {
        match idx {
            0 => args.property = Some(v),
            1 => args.enum_cls = Some(v),
            _ => {
                return Err(PyAuthoringError::spanned(
                    CODE_SCHEMA,
                    "lifecycle() takes at most 2 positional arguments",
                    span.clone(),
                ));
            }
        }
    }
    for (k, v) in kwargs {
        match k.as_str() {
            "property" => args.property = Some(v),
            "enum_cls" => args.enum_cls = Some(v),
            "initial" => args.initial = Some(v),
            "transitions" => args.transitions = Some(v),
            "terminal" => args.terminal = Some(v),
            "immutable" => args.immutable = Some(v),
            "description" => {
                args.description = if matches!(v, EvalValue::None) {
                    None
                } else {
                    Some(expect_str(v, "lifecycle.description", span)?)
                };
            }
            _ => {
                return Err(PyAuthoringError::spanned(
                    CODE_SCHEMA,
                    format!("unexpected keyword '{k}' in lifecycle()"),
                    span.clone(),
                ));
            }
        }
    }
    finish_lifecycle(args, env, span)
}

fn finish_lifecycle<'a>(
    args: LifecycleArgs<'a>,
    env: &ModuleEnv<'a>,
    span: &SourceSpan,
) -> Result<EvalValue<'a>, PyAuthoringError> {
    if let Some(EvalValue::EnumClass(cls_name)) = args.enum_cls {
        if !env.enums.get(&cls_name).is_some_and(|e| e.is_str_enum) {
            return Err(PyAuthoringError::spanned(
                CODE_SCHEMA,
                "lifecycle enum_cls must be a StrEnum subclass",
                span.clone(),
            ));
        }
    } else {
        return Err(PyAuthoringError::spanned(
            CODE_SCHEMA,
            "lifecycle() requires a StrEnum enum_cls",
            span.clone(),
        ));
    }

    let mut transitions = Vec::new();
    for item in expect_seq(
        args.transitions.ok_or_else(|| {
            PyAuthoringError::spanned(
                CODE_SCHEMA,
                "lifecycle() missing 'transitions'",
                span.clone(),
            )
        })?,
        "lifecycle.transitions",
        span,
    )? {
        if let EvalValue::TransitionSpec(tr) = item {
            transitions.push(*tr);
        } else {
            return Err(PyAuthoringError::spanned(
                CODE_SCHEMA,
                "lifecycle.transitions entries must be transition(...) specs",
                span.clone(),
            ));
        }
    }

    let terminal = match args.terminal {
        None => Vec::new(),
        Some(v) => enum_state_seq(v, "lifecycle.terminal", span)?,
    };
    let immutable = match args.immutable {
        None => Vec::new(),
        Some(v) => enum_state_seq(v, "lifecycle.immutable", span)?,
    };

    Ok(EvalValue::LifecycleSpec(Box::new(LifecycleAst {
        property: expect_required_str(args.property, "lifecycle", "property", span)?,
        initial: enum_state_seq(
            args.initial.ok_or_else(|| {
                PyAuthoringError::spanned(
                    CODE_SCHEMA,
                    "lifecycle() missing 'initial'",
                    span.clone(),
                )
            })?,
            "lifecycle.initial",
            span,
        )?,
        transitions,
        terminal,
        immutable,
        description: args.description,
    })))
}
