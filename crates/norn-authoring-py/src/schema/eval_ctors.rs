//! Declarative schema, property-type, and link constructor evaluators used by
//! [`crate::schema::eval`].

use crate::canonical::DependencyPin;
use crate::error::{CODE_SCHEMA, PyAuthoringError, SourceSpan};
use crate::schema::env::{EvalValue, ModuleEnv};
use crate::schema::types::{
    FieldDefault, FieldMeta, KeySpec, LinkSpec, Multiplicity, PropertyTypeSpec, Rounding,
    TextFormat, TimezonePolicy, UnitRef, UnitSystem,
};

pub(crate) fn expect_str(
    val: EvalValue<'_>,
    label: &str,
    span: &SourceSpan,
) -> Result<String, PyAuthoringError> {
    if let EvalValue::Str(s) = val {
        Ok(s)
    } else {
        Err(PyAuthoringError::spanned(
            CODE_SCHEMA,
            format!("{label} must be a string"),
            span.clone(),
        ))
    }
}

pub(crate) fn expect_bool(
    val: EvalValue<'_>,
    label: &str,
    span: &SourceSpan,
) -> Result<bool, PyAuthoringError> {
    if let EvalValue::Bool(b) = val {
        Ok(b)
    } else {
        Err(PyAuthoringError::spanned(
            CODE_SCHEMA,
            format!("{label} must be a bool"),
            span.clone(),
        ))
    }
}

pub(crate) fn expect_non_neg_u64(
    val: EvalValue<'_>,
    label: &str,
    span: &SourceSpan,
) -> Result<u64, PyAuthoringError> {
    if let EvalValue::Int(digits) = val
        && let Ok(parsed) = digits.parse::<u64>()
    {
        return Ok(parsed);
    }
    Err(PyAuthoringError::spanned(
        CODE_SCHEMA,
        format!("{label} must be a non-negative int"),
        span.clone(),
    ))
}

pub(crate) fn expect_seq<'a>(
    val: EvalValue<'a>,
    label: &str,
    span: &SourceSpan,
) -> Result<Vec<EvalValue<'a>>, PyAuthoringError> {
    if let EvalValue::Tuple(items) | EvalValue::List(items) = val {
        Ok(items)
    } else {
        Err(PyAuthoringError::spanned(
            CODE_SCHEMA,
            format!("{label} must be a tuple or list"),
            span.clone(),
        ))
    }
}

pub(crate) fn expect_str_seq(
    val: EvalValue<'_>,
    label: &str,
    span: &SourceSpan,
) -> Result<Vec<String>, PyAuthoringError> {
    let items = expect_seq(val, label, span)?;
    let mut out = Vec::with_capacity(items.len());
    for item in items {
        out.push(expect_str(item, label, span)?);
    }
    Ok(out)
}

pub(crate) fn eval_value_to_field_default(
    val: EvalValue<'_>,
    span: &SourceSpan,
) -> Result<FieldDefault, PyAuthoringError> {
    match val {
        EvalValue::None => Ok(FieldDefault::Null),
        EvalValue::Str(s) => Ok(FieldDefault::Str(s)),
        EvalValue::Bool(b) => Ok(FieldDefault::Bool(b)),
        EvalValue::Int(digits) => Ok(FieldDefault::Int(digits)),
        EvalValue::Decimal(dec) => Ok(FieldDefault::Decimal(dec)),
        EvalValue::EnumMember(m) => m.as_str_value().map_or_else(
            || {
                Err(PyAuthoringError::spanned(
                    CODE_SCHEMA,
                    "enum field default must be a StrEnum member",
                    span.clone(),
                ))
            },
            |s| Ok(FieldDefault::Str(s.to_owned())),
        ),
        EvalValue::FieldDefault(fd) => Ok(*fd),
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
            CODE_SCHEMA,
            "unsupported default value on Concept field",
            span.clone(),
        )),
    }
}

pub(crate) fn eval_unit_ref<'a>(
    pos: Vec<EvalValue<'a>>,
    kwargs: Vec<(String, EvalValue<'a>)>,
    span: &SourceSpan,
) -> Result<EvalValue<'a>, PyAuthoringError> {
    let mut system_val = None;
    let mut code_val = None;
    for (idx, v) in pos.into_iter().enumerate() {
        match idx {
            0 => system_val = Some(v),
            1 => code_val = Some(v),
            _ => {
                return Err(PyAuthoringError::spanned(
                    CODE_SCHEMA,
                    "UnitRef() takes at most 2 positional arguments",
                    span.clone(),
                ));
            }
        }
    }
    for (k, v) in kwargs {
        match k.as_str() {
            "system" => system_val = Some(v),
            "code" => code_val = Some(v),
            _ => {
                return Err(PyAuthoringError::spanned(
                    CODE_SCHEMA,
                    format!("unexpected keyword '{k}' in UnitRef()"),
                    span.clone(),
                ));
            }
        }
    }
    let sys_raw = expect_str(
        system_val.ok_or_else(|| {
            PyAuthoringError::spanned(CODE_SCHEMA, "UnitRef() missing 'system'", span.clone())
        })?,
        "UnitRef.system",
        span,
    )?;
    let code = expect_str(
        code_val.ok_or_else(|| {
            PyAuthoringError::spanned(CODE_SCHEMA, "UnitRef() missing 'code'", span.clone())
        })?,
        "UnitRef.code",
        span,
    )?;
    let system = UnitSystem::parse(&sys_raw, Some(span))?;
    Ok(EvalValue::UnitRef(Box::new(UnitRef::new(system, code))))
}

pub(crate) fn eval_dependency_pin<'a>(
    pos: Vec<EvalValue<'a>>,
    kwargs: Vec<(String, EvalValue<'a>)>,
    span: &SourceSpan,
) -> Result<EvalValue<'a>, PyAuthoringError> {
    let mut artifact_id = None;
    let mut version = None;
    let mut kind = "entity".to_owned();
    let mut required = true;
    for (idx, v) in pos.into_iter().enumerate() {
        match idx {
            0 => artifact_id = Some(expect_str(v, "DependencyPin.artifact_id", span)?),
            1 => version = Some(expect_str(v, "DependencyPin.version", span)?),
            2 => kind = expect_str(v, "DependencyPin.kind", span)?,
            3 => required = expect_bool(v, "DependencyPin.required", span)?,
            _ => {
                return Err(PyAuthoringError::spanned(
                    CODE_SCHEMA,
                    "DependencyPin() takes at most 4 positional arguments",
                    span.clone(),
                ));
            }
        }
    }
    for (k, v) in kwargs {
        match k.as_str() {
            "artifact_id" => artifact_id = Some(expect_str(v, "DependencyPin.artifact_id", span)?),
            "version" => version = Some(expect_str(v, "DependencyPin.version", span)?),
            "kind" => kind = expect_str(v, "DependencyPin.kind", span)?,
            "required" => required = expect_bool(v, "DependencyPin.required", span)?,
            _ => {
                return Err(PyAuthoringError::spanned(
                    CODE_SCHEMA,
                    format!("unexpected keyword '{k}' in DependencyPin()"),
                    span.clone(),
                ));
            }
        }
    }
    Ok(EvalValue::DependencyPin(Box::new(DependencyPin {
        artifact_id: artifact_id.ok_or_else(|| {
            PyAuthoringError::spanned(
                CODE_SCHEMA,
                "DependencyPin() missing 'artifact_id'",
                span.clone(),
            )
        })?,
        version: version.ok_or_else(|| {
            PyAuthoringError::spanned(
                CODE_SCHEMA,
                "DependencyPin() missing 'version'",
                span.clone(),
            )
        })?,
        kind,
        required,
    })))
}

pub(crate) fn eval_key_spec<'a>(
    pos: Vec<EvalValue<'a>>,
    kwargs: Vec<(String, EvalValue<'a>)>,
    span: &SourceSpan,
) -> Result<EvalValue<'a>, PyAuthoringError> {
    let mut name = None;
    let mut properties = None;
    for (idx, v) in pos.into_iter().enumerate() {
        match idx {
            0 => name = Some(expect_str(v, "KeySpec.name", span)?),
            1 => properties = Some(expect_str_seq(v, "KeySpec.properties", span)?),
            _ => {
                return Err(PyAuthoringError::spanned(
                    CODE_SCHEMA,
                    "KeySpec() takes at most 2 positional arguments",
                    span.clone(),
                ));
            }
        }
    }
    for (k, v) in kwargs {
        match k.as_str() {
            "name" => name = Some(expect_str(v, "KeySpec.name", span)?),
            "properties" => properties = Some(expect_str_seq(v, "KeySpec.properties", span)?),
            _ => {
                return Err(PyAuthoringError::spanned(
                    CODE_SCHEMA,
                    format!("unexpected keyword '{k}' in KeySpec()"),
                    span.clone(),
                ));
            }
        }
    }
    Ok(EvalValue::KeySpec(Box::new(KeySpec {
        name: name.ok_or_else(|| {
            PyAuthoringError::spanned(CODE_SCHEMA, "KeySpec() missing 'name'", span.clone())
        })?,
        properties: properties.ok_or_else(|| {
            PyAuthoringError::spanned(CODE_SCHEMA, "KeySpec() missing 'properties'", span.clone())
        })?,
    })))
}

pub(crate) fn eval_marking<'a>(
    pos: Vec<EvalValue<'a>>,
    kwargs: Vec<(String, EvalValue<'a>)>,
    span: &SourceSpan,
) -> Result<EvalValue<'a>, PyAuthoringError> {
    let mut id_val = pos.into_iter().next();
    for (k, v) in kwargs {
        if k == "id" {
            id_val = Some(v);
        } else {
            return Err(PyAuthoringError::spanned(
                CODE_SCHEMA,
                format!("unexpected keyword '{k}' in Marking()"),
                span.clone(),
            ));
        }
    }
    let id = expect_str(
        id_val.ok_or_else(|| {
            PyAuthoringError::spanned(CODE_SCHEMA, "Marking() missing 'id'", span.clone())
        })?,
        "Marking.id",
        span,
    )?;
    Ok(EvalValue::Marking(id))
}

pub(crate) fn eval_field_meta<'a>(
    pos: Vec<EvalValue<'a>>,
    kwargs: Vec<(String, EvalValue<'a>)>,
    span: &SourceSpan,
) -> Result<EvalValue<'a>, PyAuthoringError> {
    let mut desc_val = None;
    let mut default_val = None;
    for (idx, v) in pos.into_iter().enumerate() {
        match idx {
            0 => desc_val = Some(v),
            1 => default_val = Some(v),
            _ => {
                return Err(PyAuthoringError::spanned(
                    CODE_SCHEMA,
                    "FieldMeta() takes at most 2 positional arguments",
                    span.clone(),
                ));
            }
        }
    }
    for (k, v) in kwargs {
        match k.as_str() {
            "description" => desc_val = Some(v),
            "default" => default_val = Some(v),
            _ => {
                return Err(PyAuthoringError::spanned(
                    CODE_SCHEMA,
                    format!("unexpected keyword '{k}' in FieldMeta()"),
                    span.clone(),
                ));
            }
        }
    }
    let description = match desc_val {
        None | Some(EvalValue::None) => None,
        Some(other) => Some(expect_str(other, "FieldMeta.description", span)?),
    };
    let default = match default_val {
        None => None,
        Some(other) => Some(eval_value_to_field_default(other, span)?),
    };
    Ok(EvalValue::FieldMeta(Box::new(FieldMeta {
        description,
        default,
    })))
}

pub(crate) fn eval_field_ctor<'a>(
    pos: &[EvalValue<'a>],
    kwargs: Vec<(String, EvalValue<'a>)>,
    span: &SourceSpan,
) -> Result<EvalValue<'a>, PyAuthoringError> {
    if !pos.is_empty() {
        return Err(PyAuthoringError::spanned(
            CODE_SCHEMA,
            "Field() accepts keyword arguments only",
            span.clone(),
        ));
    }
    let mut default = None;
    for (k, v) in kwargs {
        match k.as_str() {
            "name" => {
                let _name = expect_str(v, "Field.name", span)?;
            }
            "default" => default = Some(eval_value_to_field_default(v, span)?),
            _ => {
                return Err(PyAuthoringError::spanned(
                    CODE_SCHEMA,
                    format!("unexpected keyword '{k}' in Field()"),
                    span.clone(),
                ));
            }
        }
    }
    match default {
        Some(d) => Ok(EvalValue::FieldDefault(Box::new(d))),
        None => Ok(EvalValue::None),
    }
}

pub(crate) fn eval_field_default_fn<'a>(
    pos: Vec<EvalValue<'a>>,
    kwargs: &[(String, EvalValue<'a>)],
    span: &SourceSpan,
) -> Result<EvalValue<'a>, PyAuthoringError> {
    if !kwargs.is_empty() || pos.len() != 1 {
        return Err(PyAuthoringError::spanned(
            CODE_SCHEMA,
            "field_default() requires exactly 1 positional argument",
            span.clone(),
        ));
    }
    let Some(val) = pos.into_iter().next() else {
        return Err(PyAuthoringError::spanned(
            CODE_SCHEMA,
            "field_default() requires a value",
            span.clone(),
        ));
    };
    let default = eval_value_to_field_default(val, span)?;
    Ok(EvalValue::FieldDefault(Box::new(default)))
}

pub(crate) fn eval_property_type_ctor<'a>(
    sym: &str,
    pos: Vec<EvalValue<'a>>,
    kwargs: Vec<(String, EvalValue<'a>)>,
    env: &ModuleEnv<'a>,
    span: &SourceSpan,
) -> Result<EvalValue<'a>, PyAuthoringError> {
    let spec = match sym {
        "integer" => PropertyTypeSpec::Integer,
        "boolean" => PropertyTypeSpec::Boolean,
        "date_type" => PropertyTypeSpec::Date,
        "duration_type" => PropertyTypeSpec::Duration,
        "text" => build_text_spec(pos, kwargs, span)?,
        "timestamp_type" => build_timestamp_spec(pos, kwargs, span)?,
        "ref_type" => build_ref_spec(pos, kwargs, span)?,
        "decimal_type" => build_decimal_spec(pos, kwargs, span)?,
        "rate_type" => build_rate_spec(pos, kwargs, span)?,
        "enum_type" => build_enum_spec(pos, kwargs, env, span)?,
        _ => {
            return Err(PyAuthoringError::spanned(
                CODE_SCHEMA,
                format!("unsupported property type constructor '{sym}'"),
                span.clone(),
            ));
        }
    };
    Ok(EvalValue::PropertyTypeSpec(Box::new(spec)))
}

fn build_text_spec<'a>(
    pos: Vec<EvalValue<'a>>,
    kwargs: Vec<(String, EvalValue<'a>)>,
    span: &SourceSpan,
) -> Result<PropertyTypeSpec, PyAuthoringError> {
    let mut fmt_val = pos.into_iter().next();
    for (k, v) in kwargs {
        if k == "fmt" {
            fmt_val = Some(v);
        } else {
            return Err(PyAuthoringError::spanned(
                CODE_SCHEMA,
                format!("unexpected keyword '{k}' in text()"),
                span.clone(),
            ));
        }
    }
    let fmt = match fmt_val {
        None | Some(EvalValue::None) => None,
        Some(other) => Some(TextFormat::parse(
            &expect_str(other, "text format", span)?,
            Some(span),
        )?),
    };
    Ok(PropertyTypeSpec::text(fmt))
}

fn build_timestamp_spec<'a>(
    pos: Vec<EvalValue<'a>>,
    kwargs: Vec<(String, EvalValue<'a>)>,
    span: &SourceSpan,
) -> Result<PropertyTypeSpec, PyAuthoringError> {
    let mut tz_val = pos.into_iter().next();
    for (k, v) in kwargs {
        if k == "timezone" {
            tz_val = Some(v);
        } else {
            return Err(PyAuthoringError::spanned(
                CODE_SCHEMA,
                format!("unexpected keyword '{k}' in timestamp_type()"),
                span.clone(),
            ));
        }
    }
    let tz = match tz_val {
        None => TimezonePolicy::Utc,
        Some(other) => {
            TimezonePolicy::parse(&expect_str(other, "timestamp timezone", span)?, Some(span))?
        }
    };
    Ok(PropertyTypeSpec::Timestamp { timezone: tz })
}

fn build_ref_spec<'a>(
    pos: Vec<EvalValue<'a>>,
    kwargs: Vec<(String, EvalValue<'a>)>,
    span: &SourceSpan,
) -> Result<PropertyTypeSpec, PyAuthoringError> {
    let mut name_val = pos.into_iter().next();
    for (k, v) in kwargs {
        if k == "name" {
            name_val = Some(v);
        } else {
            return Err(PyAuthoringError::spanned(
                CODE_SCHEMA,
                format!("unexpected keyword '{k}' in ref_type()"),
                span.clone(),
            ));
        }
    }
    let name = expect_str(
        name_val.ok_or_else(|| {
            PyAuthoringError::spanned(CODE_SCHEMA, "ref_type() requires name", span.clone())
        })?,
        "ref_type.name",
        span,
    )?;
    PropertyTypeSpec::ref_type(name, Some(span))
}

fn build_enum_spec<'a>(
    pos: Vec<EvalValue<'a>>,
    kwargs: Vec<(String, EvalValue<'a>)>,
    env: &ModuleEnv<'a>,
    span: &SourceSpan,
) -> Result<PropertyTypeSpec, PyAuthoringError> {
    let mut cls_val = pos.into_iter().next();
    for (k, v) in kwargs {
        if k == "enum_cls" {
            cls_val = Some(v);
        } else {
            return Err(PyAuthoringError::spanned(
                CODE_SCHEMA,
                format!("unexpected keyword '{k}' in enum_type()"),
                span.clone(),
            ));
        }
    }
    if let Some(EvalValue::EnumClass(name)) = cls_val
        && let Some(decl) = env.enums.get(&name)
        && decl.is_str_enum
    {
        return Ok(PropertyTypeSpec::Enum {
            class_name: decl.py_name.clone(),
            values: decl.str_values(),
        });
    }
    Err(PyAuthoringError::spanned(
        CODE_SCHEMA,
        "enum_type expects a StrEnum subclass",
        span.clone(),
    ))
}

fn build_decimal_spec<'a>(
    pos: Vec<EvalValue<'a>>,
    kwargs: Vec<(String, EvalValue<'a>)>,
    span: &SourceSpan,
) -> Result<PropertyTypeSpec, PyAuthoringError> {
    let mut scale_val = pos.into_iter().next();
    let mut unit = None;
    let mut currency = None;
    let mut rounding = None;
    for (k, v) in kwargs {
        match k.as_str() {
            "scale" => scale_val = Some(v),
            "unit" => {
                if matches!(v, EvalValue::None) {
                    unit = None;
                } else if let EvalValue::UnitRef(u) = v {
                    unit = Some(*u);
                } else {
                    return Err(PyAuthoringError::spanned(
                        CODE_SCHEMA,
                        "decimal unit must be a UnitRef",
                        span.clone(),
                    ));
                }
            }
            "currency" => {
                if matches!(v, EvalValue::None) {
                    currency = None;
                } else {
                    currency = Some(expect_str(v, "decimal currency", span)?);
                }
            }
            "rounding" => {
                if matches!(v, EvalValue::None) {
                    rounding = None;
                } else {
                    rounding = Some(Rounding::parse(
                        &expect_str(v, "decimal rounding", span)?,
                        Some(span),
                    )?);
                }
            }
            _ => {
                return Err(PyAuthoringError::spanned(
                    CODE_SCHEMA,
                    format!("unexpected keyword '{k}' in decimal_type()"),
                    span.clone(),
                ));
            }
        }
    }
    let scale = expect_non_neg_u64(
        scale_val.ok_or_else(|| {
            PyAuthoringError::spanned(CODE_SCHEMA, "decimal_type() requires scale", span.clone())
        })?,
        "decimal scale",
        span,
    )?;
    PropertyTypeSpec::decimal_type(scale, unit, currency, rounding, Some(span))
}

fn build_rate_spec<'a>(
    pos: Vec<EvalValue<'a>>,
    kwargs: Vec<(String, EvalValue<'a>)>,
    span: &SourceSpan,
) -> Result<PropertyTypeSpec, PyAuthoringError> {
    let mut scale_val = pos.into_iter().next();
    let mut currency = None;
    let mut per_unit = None;
    let mut rounding = None;
    for (k, v) in kwargs {
        match k.as_str() {
            "scale" => scale_val = Some(v),
            "currency" => currency = Some(expect_str(v, "rate currency", span)?),
            "per_unit" => {
                if let EvalValue::UnitRef(u) = v {
                    per_unit = Some(*u);
                } else {
                    return Err(PyAuthoringError::spanned(
                        CODE_SCHEMA,
                        "rate per_unit must be a UnitRef",
                        span.clone(),
                    ));
                }
            }
            "rounding" => {
                if matches!(v, EvalValue::None) {
                    rounding = None;
                } else {
                    rounding = Some(Rounding::parse(
                        &expect_str(v, "rate rounding", span)?,
                        Some(span),
                    )?);
                }
            }
            _ => {
                return Err(PyAuthoringError::spanned(
                    CODE_SCHEMA,
                    format!("unexpected keyword '{k}' in rate_type()"),
                    span.clone(),
                ));
            }
        }
    }
    let scale = expect_non_neg_u64(
        scale_val.ok_or_else(|| {
            PyAuthoringError::spanned(CODE_SCHEMA, "rate_type() requires scale", span.clone())
        })?,
        "rate scale",
        span,
    )?;
    let curr = currency.ok_or_else(|| {
        PyAuthoringError::spanned(CODE_SCHEMA, "rate_type() requires currency", span.clone())
    })?;
    let pu = per_unit.ok_or_else(|| {
        PyAuthoringError::spanned(CODE_SCHEMA, "rate_type() requires per_unit", span.clone())
    })?;
    Ok(PropertyTypeSpec::rate_type(scale, curr, pu, rounding))
}

pub(crate) fn eval_link<'a>(
    pos: Vec<EvalValue<'a>>,
    kwargs: Vec<(String, EvalValue<'a>)>,
    env: &ModuleEnv<'a>,
    span: &SourceSpan,
) -> Result<EvalValue<'a>, PyAuthoringError> {
    let mut target_val = pos.into_iter().next();
    let mut to = Multiplicity::One;
    let mut from_ = Multiplicity::Many;
    let mut required = false;
    let mut inverse = None;
    let mut acyclic = false;
    let mut description = None;
    for (k, v) in kwargs {
        match k.as_str() {
            "target" => target_val = Some(v),
            "to" => to = Multiplicity::parse(&expect_str(v, "link.to", span)?, Some(span))?,
            "from_" => {
                from_ = Multiplicity::parse(&expect_str(v, "link.from_", span)?, Some(span))?;
            }
            "required" => required = expect_bool(v, "link.required", span)?,
            "inverse" => {
                if matches!(v, EvalValue::None) {
                    inverse = None;
                } else {
                    inverse = Some(expect_str(v, "link.inverse", span)?);
                }
            }
            "acyclic" => acyclic = expect_bool(v, "link.acyclic", span)?,
            "description" => {
                if matches!(v, EvalValue::None) {
                    description = None;
                } else {
                    description = Some(expect_str(v, "link.description", span)?);
                }
            }
            _ => {
                return Err(PyAuthoringError::spanned(
                    CODE_SCHEMA,
                    format!("unexpected keyword '{k}' in link()"),
                    span.clone(),
                ));
            }
        }
    }
    let target = resolve_link_target(target_val, env, span)?;
    Ok(EvalValue::LinkSpec(Box::new(LinkSpec {
        target,
        to,
        from_,
        required,
        inverse,
        acyclic,
        description,
    })))
}

fn resolve_link_target(
    target_val: Option<EvalValue<'_>>,
    env: &ModuleEnv<'_>,
    span: &SourceSpan,
) -> Result<String, PyAuthoringError> {
    if let Some(EvalValue::Str(s)) = target_val {
        return Ok(s);
    }
    if let Some(EvalValue::ConceptClass(py_name)) = target_val {
        let norn = env
            .concepts
            .get(&py_name)
            .map_or("", |c| c.norn_name.as_str());
        if norn.is_empty() {
            return Err(PyAuthoringError::spanned(
                CODE_SCHEMA,
                format!("link target Concept '{py_name}' has no declared Norn name"),
                span.clone(),
            ));
        }
        return Ok(norn.to_owned());
    }
    Err(PyAuthoringError::spanned(
        CODE_SCHEMA,
        "link target must be a named Concept subclass or str",
        span.clone(),
    ))
}
