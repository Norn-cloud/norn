//! `Annotated[base, meta...]` consistency validation and property/link extraction
//! matching `tools/norn-py/norn_py/schema.py` (`_extract_property_and_meta`,
//! `_validate_base_matches_spec`, `_validate_base_matches_link`).

use crate::error::{CODE_SCHEMA, CODE_TYPE_MISMATCH, PyAuthoringError, SourceSpan};
use crate::schema::annotation::AnnVal;
use crate::schema::env::ModuleEnv;
use crate::schema::types::{FieldMeta, LinkSpec, PropertyTypeSpec, UnitRef};

/// Extracted property or link specification plus nullability, markings, and metadata.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum ExtractedFieldKind {
    Property(PropertyTypeSpec),
    Link(LinkSpec),
}

/// Complete resolved field declaration extracted from a `Concept` field annotation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ExtractedField {
    pub(crate) kind: ExtractedFieldKind,
    pub(crate) nullable: bool,
    pub(crate) markings: Vec<String>,
    pub(crate) meta: FieldMeta,
}

#[derive(Default)]
struct ExtractedMeta {
    markings: Vec<String>,
    explicit_meta: Option<FieldMeta>,
    explicit_prop: Option<PropertyTypeSpec>,
    explicit_link: Option<LinkSpec>,
}

pub(crate) fn extract_property_and_meta(
    field_name: &str,
    annotation: AnnVal,
    env: &ModuleEnv<'_>,
    span: Option<&SourceSpan>,
) -> Result<ExtractedField, PyAuthoringError> {
    let (unwrapped, mut nullable) = unwrap_nullable(annotation);
    let mut collected = ExtractedMeta::default();

    let base = if let AnnVal::Annotated(node) = unwrapped {
        collect_annotated_metadata(field_name, node.metadata, &mut collected, span)?;
        let (inner_base, inner_nullable) = unwrap_nullable(node.base);
        nullable = nullable || inner_nullable;
        inner_base
    } else {
        unwrapped
    };

    let meta = collected.explicit_meta.unwrap_or_default();
    let (base, field_nullable) = unwrap_field_wrapper(base);
    nullable = nullable || field_nullable;

    if collected.explicit_link.is_some() && collected.explicit_prop.is_some() {
        return Err(PyAuthoringError::with_optional_span(
            CODE_SCHEMA,
            format!("field '{field_name}' cannot combine both link(...) and a PropertyTypeSpec"),
            span.cloned(),
        ));
    }

    if let Some(link_spec) = collected.explicit_link {
        validate_base_matches_link(field_name, &base, &link_spec, span)?;
        return Ok(ExtractedField {
            kind: ExtractedFieldKind::Link(link_spec),
            nullable,
            markings: collected.markings,
            meta,
        });
    }

    if let Some(prop_spec) = collected.explicit_prop {
        validate_base_matches_spec(field_name, &base, &prop_spec, span)?;
        return Ok(ExtractedField {
            kind: ExtractedFieldKind::Property(prop_spec),
            nullable,
            markings: collected.markings,
            meta,
        });
    }

    let inferred = infer_plain_property_type(field_name, &base, env, span)?;
    Ok(ExtractedField {
        kind: ExtractedFieldKind::Property(inferred),
        nullable,
        markings: collected.markings,
        meta,
    })
}

fn collect_annotated_metadata(
    field_name: &str,
    metadata: Vec<AnnVal>,
    out: &mut ExtractedMeta,
    span: Option<&SourceSpan>,
) -> Result<(), PyAuthoringError> {
    for extra in metadata {
        match extra {
            AnnVal::Marking(id) => out.markings.push(id),
            AnnVal::PropertyTypeSpec(spec) => {
                if out.explicit_prop.is_some() {
                    return Err(PyAuthoringError::with_optional_span(
                        CODE_SCHEMA,
                        format!(
                            "field '{field_name}' declares multiple PropertyTypeSpec annotations"
                        ),
                        span.cloned(),
                    ));
                }
                out.explicit_prop = Some(*spec);
            }
            AnnVal::LinkSpec(spec) => {
                if out.explicit_link.is_some() {
                    return Err(PyAuthoringError::with_optional_span(
                        CODE_SCHEMA,
                        format!("field '{field_name}' declares multiple link() annotations"),
                        span.cloned(),
                    ));
                }
                out.explicit_link = Some(*spec);
            }
            AnnVal::FieldMeta(fm) => {
                if out.explicit_meta.is_some() {
                    return Err(PyAuthoringError::with_optional_span(
                        CODE_SCHEMA,
                        format!("field '{field_name}' declares multiple FieldMeta annotations"),
                        span.cloned(),
                    ));
                }
                out.explicit_meta = Some(*fm);
            }
            AnnVal::None
            | AnnVal::Bool(_)
            | AnnVal::Int(_)
            | AnnVal::Str(_)
            | AnnVal::Decimal(_)
            | AnnVal::Symbol(_)
            | AnnVal::ModuleNamespace(_)
            | AnnVal::EnumClass(_)
            | AnnVal::EnumMember(_)
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
            | AnnVal::UnitRef(_) => {
                return Err(PyAuthoringError::with_optional_span(
                    CODE_TYPE_MISMATCH,
                    format!(
                        "field '{field_name}' has unsupported Annotated metadata (expected Marking, FieldMeta, link(...), or a PropertyTypeSpec constructor)"
                    ),
                    span.cloned(),
                ));
            }
        }
    }
    Ok(())
}

fn infer_plain_property_type(
    field_name: &str,
    base: &AnnVal,
    env: &ModuleEnv<'_>,
    span: Option<&SourceSpan>,
) -> Result<PropertyTypeSpec, PyAuthoringError> {
    match base {
        AnnVal::Symbol("str") => Ok(PropertyTypeSpec::text(None)),
        AnnVal::Symbol("int") => Ok(PropertyTypeSpec::Integer),
        AnnVal::Symbol("bool") => Ok(PropertyTypeSpec::Boolean),
        AnnVal::EnumClass(cls_name) => {
            if let Some(decl) = env.enums.get(cls_name)
                && decl.is_str_enum
            {
                Ok(PropertyTypeSpec::Enum {
                    class_name: decl.py_name.clone(),
                    values: decl.str_values(),
                })
            } else {
                Err(PyAuthoringError::with_optional_span(
                    CODE_SCHEMA,
                    format!("field '{field_name}' enum annotation must be a StrEnum subclass"),
                    span.cloned(),
                ))
            }
        }
        AnnVal::None
        | AnnVal::Bool(_)
        | AnnVal::Int(_)
        | AnnVal::Str(_)
        | AnnVal::Decimal(_)
        | AnnVal::Symbol(_)
        | AnnVal::ModuleNamespace(_)
        | AnnVal::EnumMember(_)
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
            format!(
                "field '{field_name}' has unsupported type annotation. Use str, int, bool, StrEnum, Annotated[..., decimal_type(...)|date_type()|timestamp_type()|ref_type(...)], or Annotated[Field[Link[Target]], link(Target, ...)]."
            ),
            span.cloned(),
        )),
    }
}

fn unwrap_nullable(val: AnnVal) -> (AnnVal, bool) {
    if let AnnVal::Nullable(inner) = val {
        (*inner, true)
    } else if let AnnVal::Union(mut items) = val {
        let original_len = items.len();
        items.retain(|item| !matches!(item, AnnVal::None));
        let has_none = items.len() < original_len;
        if has_none
            && items.len() == 1
            && let Some(only) = items.pop()
        {
            return (only, true);
        }
        (AnnVal::Union(items), false)
    } else {
        (val, false)
    }
}

fn unwrap_field_wrapper(val: AnnVal) -> (AnnVal, bool) {
    if let AnnVal::Field(inner) = val {
        unwrap_nullable(*inner)
    } else {
        (val, false)
    }
}

fn extract_literal_tag(
    val: &AnnVal,
    kind_label: &str,
    field_name: &str,
    span: Option<&SourceSpan>,
) -> Result<String, PyAuthoringError> {
    if let AnnVal::Literal(args) = val
        && let [AnnVal::Str(tag)] = args.as_slice()
        && !tag.is_empty()
    {
        return Ok(tag.clone());
    }
    Err(PyAuthoringError::with_optional_span(
        CODE_TYPE_MISMATCH,
        format!(
            "field '{field_name}' {kind_label} type argument must be a single non-empty string Literal[...]"
        ),
        span.cloned(),
    ))
}

fn validate_quantity_base(
    field_name: &str,
    inner: &AnnVal,
    unit: Option<&UnitRef>,
    currency: Option<&str>,
    per_unit: Option<&UnitRef>,
    span: Option<&SourceSpan>,
) -> Result<(), PyAuthoringError> {
    let Some(u) = unit else {
        return Err(PyAuthoringError::with_optional_span(
            CODE_TYPE_MISMATCH,
            format!(
                "field '{field_name}' has base Quantity[...] but decimal spec has no matching unit"
            ),
            span.cloned(),
        ));
    };
    if currency.is_some() || per_unit.is_some() {
        return Err(PyAuthoringError::with_optional_span(
            CODE_TYPE_MISMATCH,
            format!(
                "field '{field_name}' has base Quantity[...] but decimal spec has no matching unit"
            ),
            span.cloned(),
        ));
    }
    let unit_tag = extract_literal_tag(inner, "Quantity[U]", field_name, span)?;
    if unit_tag != u.code {
        return Err(PyAuthoringError::with_optional_span(
            CODE_TYPE_MISMATCH,
            format!(
                "field '{field_name}' declares Quantity[Literal['{unit_tag}']] which conflicts with UnitRef code '{}'",
                u.code
            ),
            span.cloned(),
        ));
    }
    Ok(())
}

fn validate_money_base(
    field_name: &str,
    inner: &AnnVal,
    unit: Option<&UnitRef>,
    currency: Option<&str>,
    per_unit: Option<&UnitRef>,
    span: Option<&SourceSpan>,
) -> Result<(), PyAuthoringError> {
    let Some(curr) = currency else {
        return Err(PyAuthoringError::with_optional_span(
            CODE_TYPE_MISMATCH,
            format!(
                "field '{field_name}' has base Money[...] but decimal spec must declare currency (without unit/per_unit)"
            ),
            span.cloned(),
        ));
    };
    if per_unit.is_some() || unit.is_some() {
        return Err(PyAuthoringError::with_optional_span(
            CODE_TYPE_MISMATCH,
            format!(
                "field '{field_name}' has base Money[...] but decimal spec must declare currency (without unit/per_unit)"
            ),
            span.cloned(),
        ));
    }
    let curr_tag = extract_literal_tag(inner, "Money[C]", field_name, span)?;
    if curr_tag != curr {
        return Err(PyAuthoringError::with_optional_span(
            CODE_TYPE_MISMATCH,
            format!(
                "field '{field_name}' declares Money[Literal['{curr_tag}']] which conflicts with currency '{curr}'"
            ),
            span.cloned(),
        ));
    }
    Ok(())
}

fn validate_rate_base(
    field_name: &str,
    currency_node: &AnnVal,
    unit_node: &AnnVal,
    unit: Option<&UnitRef>,
    currency: Option<&str>,
    per_unit: Option<&UnitRef>,
    span: Option<&SourceSpan>,
) -> Result<(), PyAuthoringError> {
    let (Some(curr), Some(pu)) = (currency, per_unit) else {
        return Err(PyAuthoringError::with_optional_span(
            CODE_TYPE_MISMATCH,
            format!(
                "field '{field_name}' has base Rate[...] but spec must use rate_type(currency=..., per_unit=...)"
            ),
            span.cloned(),
        ));
    };
    if unit.is_some() {
        return Err(PyAuthoringError::with_optional_span(
            CODE_TYPE_MISMATCH,
            format!(
                "field '{field_name}' has base Rate[...] but spec must use rate_type(currency=..., per_unit=...)"
            ),
            span.cloned(),
        ));
    }
    let curr_tag = extract_literal_tag(currency_node, "Rate[C, U] currency", field_name, span)?;
    let unit_tag = extract_literal_tag(unit_node, "Rate[C, U] unit", field_name, span)?;
    if curr_tag != curr {
        return Err(PyAuthoringError::with_optional_span(
            CODE_TYPE_MISMATCH,
            format!(
                "field '{field_name}' declares Rate[Literal['{curr_tag}'], ...] which conflicts with currency '{curr}'"
            ),
            span.cloned(),
        ));
    }
    if unit_tag != pu.code {
        return Err(PyAuthoringError::with_optional_span(
            CODE_TYPE_MISMATCH,
            format!(
                "field '{field_name}' declares Rate[..., Literal['{unit_tag}']] which conflicts with per_unit code '{}'",
                pu.code
            ),
            span.cloned(),
        ));
    }
    Ok(())
}

fn validate_decimal_base(
    field_name: &str,
    base: &AnnVal,
    unit: Option<&UnitRef>,
    currency: Option<&str>,
    per_unit: Option<&UnitRef>,
    span: Option<&SourceSpan>,
) -> Result<(), PyAuthoringError> {
    if matches!(base, AnnVal::Symbol("Decimal")) {
        return Ok(());
    }
    if let AnnVal::Quantity(inner) = base {
        return validate_quantity_base(field_name, inner, unit, currency, per_unit, span);
    }
    if let AnnVal::Money(inner) = base {
        return validate_money_base(field_name, inner, unit, currency, per_unit, span);
    }
    if let AnnVal::Rate(rate_node) = base {
        return validate_rate_base(
            field_name,
            &rate_node.currency,
            &rate_node.unit,
            unit,
            currency,
            per_unit,
            span,
        );
    }
    Err(PyAuthoringError::with_optional_span(
        CODE_TYPE_MISMATCH,
        format!(
            "field '{field_name}' has base type incompatible with decimal/rate metadata; use Decimal, Quantity[...], Money[...], or Rate[..., ...]"
        ),
        span.cloned(),
    ))
}

fn validate_base_matches_spec(
    field_name: &str,
    base: &AnnVal,
    spec: &PropertyTypeSpec,
    span: Option<&SourceSpan>,
) -> Result<(), PyAuthoringError> {
    match spec {
        PropertyTypeSpec::Decimal {
            unit,
            currency,
            per_unit,
            ..
        } => validate_decimal_base(
            field_name,
            base,
            unit.as_ref(),
            currency.as_deref(),
            per_unit.as_ref(),
            span,
        ),
        PropertyTypeSpec::Text { .. } => {
            require_base_symbol(field_name, base, &["str"], "text() metadata", "str", span)
        }
        PropertyTypeSpec::Integer => require_base_symbol(
            field_name,
            base,
            &["int"],
            "integer() metadata",
            "int",
            span,
        ),
        PropertyTypeSpec::Boolean => require_base_symbol(
            field_name,
            base,
            &["bool"],
            "boolean() metadata",
            "bool",
            span,
        ),
        PropertyTypeSpec::Timestamp { .. } => require_base_symbol(
            field_name,
            base,
            &["Timestamp", "str"],
            "timestamp_type()",
            "Timestamp or str",
            span,
        ),
        PropertyTypeSpec::Date => require_base_symbol(
            field_name,
            base,
            &["Date", "str"],
            "date_type()",
            "Date or str",
            span,
        ),
        PropertyTypeSpec::Duration => require_base_symbol(
            field_name,
            base,
            &["Duration", "str"],
            "duration_type()",
            "Duration or str",
            span,
        ),
        PropertyTypeSpec::Enum { class_name, .. } => {
            if let AnnVal::EnumClass(base_cls) = base
                && base_cls == class_name
            {
                Ok(())
            } else {
                Err(PyAuthoringError::with_optional_span(
                    CODE_TYPE_MISMATCH,
                    format!(
                        "field '{field_name}' has base type that does not match enum_type({class_name})"
                    ),
                    span.cloned(),
                ))
            }
        }
        PropertyTypeSpec::Ref { .. } => {
            if matches!(base, AnnVal::Link(_)) {
                Err(PyAuthoringError::with_optional_span(
                    CODE_TYPE_MISMATCH,
                    format!(
                        "field '{field_name}' cannot combine Link[...] with ref_type(); use link(...)"
                    ),
                    span.cloned(),
                ))
            } else {
                Ok(())
            }
        }
    }
}

fn require_base_symbol(
    field_name: &str,
    base: &AnnVal,
    allowed: &[&str],
    spec_label: &str,
    expected_hint: &str,
    span: Option<&SourceSpan>,
) -> Result<(), PyAuthoringError> {
    if let AnnVal::Symbol(sym) = base
        && allowed.contains(sym)
    {
        return Ok(());
    }
    Err(PyAuthoringError::with_optional_span(
        CODE_TYPE_MISMATCH,
        format!(
            "field '{field_name}' has base type incompatible with {spec_label}; use {expected_hint}"
        ),
        span.cloned(),
    ))
}

fn validate_base_matches_link(
    field_name: &str,
    base: &AnnVal,
    spec: &LinkSpec,
    span: Option<&SourceSpan>,
) -> Result<(), PyAuthoringError> {
    let AnnVal::Link(target_arg) = base else {
        return Err(PyAuthoringError::with_optional_span(
            CODE_TYPE_MISMATCH,
            format!(
                "field '{field_name}' has link(...) metadata but base type is not Link[Target]"
            ),
            span.cloned(),
        ));
    };
    if let AnnVal::ConceptClass { py_name, norn_name } = target_arg.as_ref()
        && !norn_name.is_empty()
        && norn_name != &spec.target
    {
        return Err(PyAuthoringError::with_optional_span(
            CODE_TYPE_MISMATCH,
            format!(
                "field '{field_name}' declares Link[{py_name}] ('{norn_name}') which conflicts with link target '{}'",
                spec.target
            ),
            span.cloned(),
        ));
    }
    Ok(())
}
