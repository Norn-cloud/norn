//! Admitted annotation-scope constructor evaluators (`Decimal`, `UnitRef`, `Marking`,
//! `FieldMeta`, `text`, `decimal_type`, `rate_type`, `link`, etc.).

use crate::decimal::format_exact_decimal;
use crate::error::{CODE_SCHEMA, CODE_UNSAFE_ANNOTATION, PyAuthoringError, SourceSpan};
use crate::schema::annotation::AnnVal;
use crate::schema::env::ModuleEnv;
use crate::schema::types::{
    FieldDefault, FieldMeta, LinkSpec, Multiplicity, PropertyTypeSpec, Rounding, TextFormat,
    TimezonePolicy, UnitRef, UnitSystem,
};

pub(crate) const ANNOTATION_CALLABLES: &[&str] = &[
    "Decimal",
    "FieldMeta",
    "Marking",
    "UnitRef",
    "boolean",
    "date_type",
    "decimal_type",
    "duration_type",
    "enum_type",
    "integer",
    "link",
    "rate_type",
    "ref_type",
    "text",
    "timestamp_type",
];

pub(crate) fn dispatch_annotation_ctor(
    fn_name: &str,
    pos: Vec<AnnVal>,
    kwargs: Vec<(String, AnnVal)>,
    env: &ModuleEnv<'_>,
    span: Option<&SourceSpan>,
) -> Result<AnnVal, PyAuthoringError> {
    match fn_name {
        "Decimal" => build_ann_decimal(pos, &kwargs, span),
        "UnitRef" => build_ann_unit_ref(pos, kwargs, span),
        "Marking" => build_ann_marking(pos, kwargs, span),
        "FieldMeta" => build_ann_field_meta(pos, kwargs, span),
        "text" => build_ann_text(pos, kwargs, span),
        "integer" => {
            expect_no_args("integer", &pos, &kwargs, span)?;
            Ok(AnnVal::PropertyTypeSpec(Box::new(
                PropertyTypeSpec::Integer,
            )))
        }
        "boolean" => {
            expect_no_args("boolean", &pos, &kwargs, span)?;
            Ok(AnnVal::PropertyTypeSpec(Box::new(
                PropertyTypeSpec::Boolean,
            )))
        }
        "date_type" => {
            expect_no_args("date_type", &pos, &kwargs, span)?;
            Ok(AnnVal::PropertyTypeSpec(Box::new(PropertyTypeSpec::Date)))
        }
        "duration_type" => {
            expect_no_args("duration_type", &pos, &kwargs, span)?;
            Ok(AnnVal::PropertyTypeSpec(Box::new(
                PropertyTypeSpec::Duration,
            )))
        }
        "timestamp_type" => build_ann_timestamp(pos, kwargs, span),
        "ref_type" => build_ann_ref_type(pos, kwargs, span),
        "decimal_type" => build_ann_decimal_type(pos, kwargs, span),
        "rate_type" => build_ann_rate_type(pos, kwargs, span),
        "enum_type" => build_ann_enum_type(pos, kwargs, env, span),
        "link" => build_ann_link(pos, kwargs, span),
        _ => Err(PyAuthoringError::with_optional_span(
            CODE_UNSAFE_ANNOTATION,
            format!("unsupported annotation constructor '{fn_name}'"),
            span.cloned(),
        )),
    }
}

fn build_ann_decimal(
    pos: Vec<AnnVal>,
    kwargs: &[(String, AnnVal)],
    span: Option<&SourceSpan>,
) -> Result<AnnVal, PyAuthoringError> {
    if !kwargs.is_empty() || pos.len() != 1 {
        return Err(PyAuthoringError::with_optional_span(
            CODE_UNSAFE_ANNOTATION,
            "Decimal() in annotation requires exactly 1 positional argument",
            span.cloned(),
        ));
    }
    let Some(AnnVal::Str(raw) | AnnVal::Int(raw)) = pos.into_iter().next() else {
        return Err(PyAuthoringError::with_optional_span(
            CODE_UNSAFE_ANNOTATION,
            "Decimal() argument in annotation must be a string or integer",
            span.cloned(),
        ));
    };
    let formatted = format_exact_decimal(&raw, span.cloned()).map_err(|err| {
        let msg = err.message();
        if msg.contains("exceeds admitted") || msg.contains("non-finite") {
            err
        } else {
            PyAuthoringError::with_optional_span(
                CODE_UNSAFE_ANNOTATION,
                format!("invalid annotation constructor call Decimal: {msg}"),
                span.cloned(),
            )
        }
    })?;
    Ok(AnnVal::Decimal(formatted))
}

fn expect_no_args(
    name: &str,
    pos: &[AnnVal],
    kwargs: &[(String, AnnVal)],
    span: Option<&SourceSpan>,
) -> Result<(), PyAuthoringError> {
    if pos.is_empty() && kwargs.is_empty() {
        Ok(())
    } else {
        Err(PyAuthoringError::with_optional_span(
            CODE_UNSAFE_ANNOTATION,
            format!("{name}() takes no arguments"),
            span.cloned(),
        ))
    }
}

fn ann_str(
    val: AnnVal,
    label: &str,
    span: Option<&SourceSpan>,
) -> Result<String, PyAuthoringError> {
    if let AnnVal::Str(s) = val {
        Ok(s)
    } else {
        Err(PyAuthoringError::with_optional_span(
            CODE_SCHEMA,
            format!("{label} must be a string"),
            span.cloned(),
        ))
    }
}

fn ann_non_neg_u64(
    val: AnnVal,
    label: &str,
    span: Option<&SourceSpan>,
) -> Result<u64, PyAuthoringError> {
    if let AnnVal::Int(digits) = val
        && let Ok(parsed) = digits.parse::<u64>()
    {
        return Ok(parsed);
    }
    Err(PyAuthoringError::with_optional_span(
        CODE_SCHEMA,
        format!("{label} must be a non-negative int"),
        span.cloned(),
    ))
}

fn build_ann_unit_ref(
    pos: Vec<AnnVal>,
    kwargs: Vec<(String, AnnVal)>,
    span: Option<&SourceSpan>,
) -> Result<AnnVal, PyAuthoringError> {
    let mut system_val: Option<AnnVal> = None;
    let mut code_val: Option<AnnVal> = None;
    for (idx, v) in pos.into_iter().enumerate() {
        match idx {
            0 => system_val = Some(v),
            1 => code_val = Some(v),
            _ => {
                return Err(PyAuthoringError::with_optional_span(
                    CODE_UNSAFE_ANNOTATION,
                    "UnitRef() takes at most 2 positional arguments",
                    span.cloned(),
                ));
            }
        }
    }
    for (k, v) in kwargs {
        match k.as_str() {
            "system" => system_val = Some(v),
            "code" => code_val = Some(v),
            _ => {
                return Err(PyAuthoringError::with_optional_span(
                    CODE_UNSAFE_ANNOTATION,
                    format!("unexpected keyword '{k}' in UnitRef()"),
                    span.cloned(),
                ));
            }
        }
    }
    let sys_raw = ann_str(
        system_val.ok_or_else(|| {
            PyAuthoringError::with_optional_span(
                CODE_UNSAFE_ANNOTATION,
                "UnitRef() missing 'system'",
                span.cloned(),
            )
        })?,
        "UnitRef.system",
        span,
    )?;
    let code = ann_str(
        code_val.ok_or_else(|| {
            PyAuthoringError::with_optional_span(
                CODE_UNSAFE_ANNOTATION,
                "UnitRef() missing 'code'",
                span.cloned(),
            )
        })?,
        "UnitRef.code",
        span,
    )?;
    let system = UnitSystem::parse(&sys_raw, span)?;
    Ok(AnnVal::UnitRef(Box::new(UnitRef::new(system, code))))
}

fn build_ann_marking(
    pos: Vec<AnnVal>,
    kwargs: Vec<(String, AnnVal)>,
    span: Option<&SourceSpan>,
) -> Result<AnnVal, PyAuthoringError> {
    let mut id_val = pos.into_iter().next();
    for (k, v) in kwargs {
        if k == "id" {
            id_val = Some(v);
        } else {
            return Err(PyAuthoringError::with_optional_span(
                CODE_UNSAFE_ANNOTATION,
                format!("unexpected keyword '{k}' in Marking()"),
                span.cloned(),
            ));
        }
    }
    let id = ann_str(
        id_val.ok_or_else(|| {
            PyAuthoringError::with_optional_span(
                CODE_UNSAFE_ANNOTATION,
                "Marking() missing 'id'",
                span.cloned(),
            )
        })?,
        "Marking.id",
        span,
    )?;
    Ok(AnnVal::Marking(id))
}

fn build_ann_field_meta(
    pos: Vec<AnnVal>,
    kwargs: Vec<(String, AnnVal)>,
    span: Option<&SourceSpan>,
) -> Result<AnnVal, PyAuthoringError> {
    let mut desc_val: Option<AnnVal> = None;
    let mut default_val: Option<AnnVal> = None;
    for (idx, v) in pos.into_iter().enumerate() {
        match idx {
            0 => desc_val = Some(v),
            1 => default_val = Some(v),
            _ => {
                return Err(PyAuthoringError::with_optional_span(
                    CODE_UNSAFE_ANNOTATION,
                    "FieldMeta() takes at most 2 positional arguments",
                    span.cloned(),
                ));
            }
        }
    }
    for (k, v) in kwargs {
        match k.as_str() {
            "description" => desc_val = Some(v),
            "default" => default_val = Some(v),
            _ => {
                return Err(PyAuthoringError::with_optional_span(
                    CODE_UNSAFE_ANNOTATION,
                    format!("unexpected keyword '{k}' in FieldMeta()"),
                    span.cloned(),
                ));
            }
        }
    }
    let description = match desc_val {
        None | Some(AnnVal::None) => None,
        Some(other) => Some(ann_str(other, "FieldMeta.description", span)?),
    };
    let default = match default_val {
        None => None,
        Some(v) => Some(ann_val_to_default(v, span)?),
    };
    Ok(AnnVal::FieldMeta(Box::new(FieldMeta {
        description,
        default,
    })))
}

fn ann_val_to_default(
    val: AnnVal,
    span: Option<&SourceSpan>,
) -> Result<FieldDefault, PyAuthoringError> {
    match val {
        AnnVal::None => Ok(FieldDefault::Null),
        AnnVal::Str(s) => Ok(FieldDefault::Str(s)),
        AnnVal::Bool(b) => Ok(FieldDefault::Bool(b)),
        AnnVal::Int(digits) => Ok(FieldDefault::Int(digits)),
        AnnVal::Decimal(dec) => Ok(FieldDefault::Decimal(dec)),
        AnnVal::EnumMember(m) => m.as_str_value().map_or_else(
            || {
                Err(PyAuthoringError::with_optional_span(
                    CODE_SCHEMA,
                    "FieldMeta.default only accepts StrEnum members",
                    span.cloned(),
                ))
            },
            |s| Ok(FieldDefault::Str(s.to_owned())),
        ),
        AnnVal::Symbol(_)
        | AnnVal::ModuleNamespace(_)
        | AnnVal::EnumClass(_)
        | AnnVal::ConceptClass { .. }
        | AnnVal::Nullable(_)
        | AnnVal::Union(_)
        | AnnVal::Literal(_)
        | AnnVal::Field(_)
        | AnnVal::Link(_)
        | AnnVal::Quantity(_)
        | AnnVal::Money(_)
        | AnnVal::Rate(_)
        | AnnVal::Annotated(_)
        | AnnVal::UnitRef(_)
        | AnnVal::Marking(_)
        | AnnVal::FieldMeta(_)
        | AnnVal::PropertyTypeSpec(_)
        | AnnVal::LinkSpec(_) => Err(PyAuthoringError::with_optional_span(
            CODE_SCHEMA,
            "unsupported default value in FieldMeta()",
            span.cloned(),
        )),
    }
}

fn build_ann_text(
    pos: Vec<AnnVal>,
    kwargs: Vec<(String, AnnVal)>,
    span: Option<&SourceSpan>,
) -> Result<AnnVal, PyAuthoringError> {
    let mut fmt_val = pos.into_iter().next();
    for (k, v) in kwargs {
        if k == "fmt" {
            fmt_val = Some(v);
        } else {
            return Err(PyAuthoringError::with_optional_span(
                CODE_UNSAFE_ANNOTATION,
                format!("unexpected keyword '{k}' in text()"),
                span.cloned(),
            ));
        }
    }
    let format = match fmt_val {
        None | Some(AnnVal::None) => None,
        Some(other) => Some(TextFormat::parse(
            &ann_str(other, "text format", span)?,
            span,
        )?),
    };
    Ok(AnnVal::PropertyTypeSpec(Box::new(PropertyTypeSpec::text(
        format,
    ))))
}

fn build_ann_timestamp(
    pos: Vec<AnnVal>,
    kwargs: Vec<(String, AnnVal)>,
    span: Option<&SourceSpan>,
) -> Result<AnnVal, PyAuthoringError> {
    let mut tz_val = pos.into_iter().next();
    for (k, v) in kwargs {
        if k == "timezone" {
            tz_val = Some(v);
        } else {
            return Err(PyAuthoringError::with_optional_span(
                CODE_UNSAFE_ANNOTATION,
                format!("unexpected keyword '{k}' in timestamp_type()"),
                span.cloned(),
            ));
        }
    }
    let timezone = match tz_val {
        None => TimezonePolicy::Utc,
        Some(other) => TimezonePolicy::parse(&ann_str(other, "timestamp timezone", span)?, span)?,
    };
    Ok(AnnVal::PropertyTypeSpec(Box::new(
        PropertyTypeSpec::Timestamp { timezone },
    )))
}

fn build_ann_ref_type(
    pos: Vec<AnnVal>,
    kwargs: Vec<(String, AnnVal)>,
    span: Option<&SourceSpan>,
) -> Result<AnnVal, PyAuthoringError> {
    let mut name_val = pos.into_iter().next();
    for (k, v) in kwargs {
        if k == "name" {
            name_val = Some(v);
        } else {
            return Err(PyAuthoringError::with_optional_span(
                CODE_UNSAFE_ANNOTATION,
                format!("unexpected keyword '{k}' in ref_type()"),
                span.cloned(),
            ));
        }
    }
    let name = ann_str(
        name_val.ok_or_else(|| {
            PyAuthoringError::with_optional_span(
                CODE_SCHEMA,
                "ref_type() requires a non-empty name",
                span.cloned(),
            )
        })?,
        "ref_type.name",
        span,
    )?;
    Ok(AnnVal::PropertyTypeSpec(Box::new(
        PropertyTypeSpec::ref_type(name, span)?,
    )))
}

fn build_ann_decimal_type(
    pos: Vec<AnnVal>,
    kwargs: Vec<(String, AnnVal)>,
    span: Option<&SourceSpan>,
) -> Result<AnnVal, PyAuthoringError> {
    let mut scale_val = pos.into_iter().next();
    let mut unit: Option<UnitRef> = None;
    let mut currency: Option<String> = None;
    let mut rounding: Option<Rounding> = None;

    for (k, v) in kwargs {
        match k.as_str() {
            "scale" => scale_val = Some(v),
            "unit" => {
                if matches!(v, AnnVal::None) {
                    unit = None;
                } else if let AnnVal::UnitRef(u) = v {
                    unit = Some(*u);
                } else {
                    return Err(PyAuthoringError::with_optional_span(
                        CODE_SCHEMA,
                        "decimal unit must be a UnitRef",
                        span.cloned(),
                    ));
                }
            }
            "currency" => {
                if matches!(v, AnnVal::None) {
                    currency = None;
                } else {
                    currency = Some(ann_str(v, "decimal currency", span)?);
                }
            }
            "rounding" => {
                if matches!(v, AnnVal::None) {
                    rounding = None;
                } else {
                    rounding = Some(Rounding::parse(
                        &ann_str(v, "decimal rounding", span)?,
                        span,
                    )?);
                }
            }
            _ => {
                return Err(PyAuthoringError::with_optional_span(
                    CODE_UNSAFE_ANNOTATION,
                    format!("unexpected keyword '{k}' in decimal_type()"),
                    span.cloned(),
                ));
            }
        }
    }
    let scale = ann_non_neg_u64(
        scale_val.ok_or_else(|| {
            PyAuthoringError::with_optional_span(
                CODE_SCHEMA,
                "decimal_type() requires scale",
                span.cloned(),
            )
        })?,
        "decimal scale",
        span,
    )?;
    let spec = PropertyTypeSpec::decimal_type(scale, unit, currency, rounding, span)?;
    Ok(AnnVal::PropertyTypeSpec(Box::new(spec)))
}

fn build_ann_rate_type(
    pos: Vec<AnnVal>,
    kwargs: Vec<(String, AnnVal)>,
    span: Option<&SourceSpan>,
) -> Result<AnnVal, PyAuthoringError> {
    let mut scale_val = pos.into_iter().next();
    let mut currency: Option<String> = None;
    let mut per_unit: Option<UnitRef> = None;
    let mut rounding: Option<Rounding> = None;

    for (k, v) in kwargs {
        match k.as_str() {
            "scale" => scale_val = Some(v),
            "currency" => currency = Some(ann_str(v, "rate currency", span)?),
            "per_unit" => {
                if let AnnVal::UnitRef(u) = v {
                    per_unit = Some(*u);
                } else {
                    return Err(PyAuthoringError::with_optional_span(
                        CODE_SCHEMA,
                        "rate per_unit must be a UnitRef",
                        span.cloned(),
                    ));
                }
            }
            "rounding" => {
                if matches!(v, AnnVal::None) {
                    rounding = None;
                } else {
                    rounding = Some(Rounding::parse(&ann_str(v, "rate rounding", span)?, span)?);
                }
            }
            _ => {
                return Err(PyAuthoringError::with_optional_span(
                    CODE_UNSAFE_ANNOTATION,
                    format!("unexpected keyword '{k}' in rate_type()"),
                    span.cloned(),
                ));
            }
        }
    }
    let scale = ann_non_neg_u64(
        scale_val.ok_or_else(|| {
            PyAuthoringError::with_optional_span(
                CODE_SCHEMA,
                "rate_type() requires scale",
                span.cloned(),
            )
        })?,
        "rate scale",
        span,
    )?;
    let curr = currency.ok_or_else(|| {
        PyAuthoringError::with_optional_span(
            CODE_SCHEMA,
            "rate_type() requires currency",
            span.cloned(),
        )
    })?;
    let pu = per_unit.ok_or_else(|| {
        PyAuthoringError::with_optional_span(
            CODE_SCHEMA,
            "rate_type() requires per_unit",
            span.cloned(),
        )
    })?;
    Ok(AnnVal::PropertyTypeSpec(Box::new(
        PropertyTypeSpec::rate_type(scale, curr, pu, rounding),
    )))
}

fn build_ann_enum_type(
    pos: Vec<AnnVal>,
    kwargs: Vec<(String, AnnVal)>,
    env: &ModuleEnv<'_>,
    span: Option<&SourceSpan>,
) -> Result<AnnVal, PyAuthoringError> {
    let mut cls_val = pos.into_iter().next();
    for (k, v) in kwargs {
        if k == "enum_cls" {
            cls_val = Some(v);
        } else {
            return Err(PyAuthoringError::with_optional_span(
                CODE_UNSAFE_ANNOTATION,
                format!("unexpected keyword '{k}' in enum_type()"),
                span.cloned(),
            ));
        }
    }
    if let Some(AnnVal::EnumClass(name)) = cls_val
        && let Some(decl) = env.enums.get(&name)
        && decl.is_str_enum
    {
        return Ok(AnnVal::PropertyTypeSpec(Box::new(PropertyTypeSpec::Enum {
            class_name: decl.py_name.clone(),
            values: decl.str_values(),
        })));
    }
    Err(PyAuthoringError::with_optional_span(
        CODE_SCHEMA,
        "enum_type expects a StrEnum subclass",
        span.cloned(),
    ))
}

fn build_ann_link(
    pos: Vec<AnnVal>,
    kwargs: Vec<(String, AnnVal)>,
    span: Option<&SourceSpan>,
) -> Result<AnnVal, PyAuthoringError> {
    let mut target_val = pos.into_iter().next();
    let mut to = Multiplicity::One;
    let mut from_ = Multiplicity::Many;
    let mut required = false;
    let mut inverse: Option<String> = None;
    let mut acyclic = false;
    let mut description: Option<String> = None;

    for (k, v) in kwargs {
        match k.as_str() {
            "target" => target_val = Some(v),
            "to" => to = Multiplicity::parse(&ann_str(v, "link.to", span)?, span)?,
            "from_" => from_ = Multiplicity::parse(&ann_str(v, "link.from_", span)?, span)?,
            "required" => {
                if let AnnVal::Bool(b) = v {
                    required = b;
                } else {
                    return Err(PyAuthoringError::with_optional_span(
                        CODE_SCHEMA,
                        "link.required must be a bool",
                        span.cloned(),
                    ));
                }
            }
            "inverse" => {
                if matches!(v, AnnVal::None) {
                    inverse = None;
                } else {
                    inverse = Some(ann_str(v, "link.inverse", span)?);
                }
            }
            "acyclic" => {
                if let AnnVal::Bool(b) = v {
                    acyclic = b;
                } else {
                    return Err(PyAuthoringError::with_optional_span(
                        CODE_SCHEMA,
                        "link.acyclic must be a bool",
                        span.cloned(),
                    ));
                }
            }
            "description" => {
                if matches!(v, AnnVal::None) {
                    description = None;
                } else {
                    description = Some(ann_str(v, "link.description", span)?);
                }
            }
            _ => {
                return Err(PyAuthoringError::with_optional_span(
                    CODE_UNSAFE_ANNOTATION,
                    format!("unexpected keyword '{k}' in link()"),
                    span.cloned(),
                ));
            }
        }
    }

    let target = if let Some(AnnVal::Str(s)) = target_val {
        s
    } else if let Some(AnnVal::ConceptClass { norn_name, .. }) = target_val
        && !norn_name.is_empty()
    {
        norn_name
    } else {
        return Err(PyAuthoringError::with_optional_span(
            CODE_SCHEMA,
            "link target must be a named Concept subclass or str",
            span.cloned(),
        ));
    };

    Ok(AnnVal::LinkSpec(Box::new(LinkSpec {
        target,
        to,
        from_,
        required,
        inverse,
        acyclic,
        description,
    })))
}
