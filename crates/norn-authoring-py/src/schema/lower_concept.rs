//! `EntityIR` v2 object-type and interface lowering for declared `Concept` classes.

use std::collections::BTreeMap;

use crate::canonical::{EnvelopeHeader, EnvelopeOptions, JsonValue, build_envelope};
use crate::error::{CODE_FLOW_LITERAL, CODE_INTERFACE_LIFECYCLE, CODE_SCHEMA, PyAuthoringError};
use crate::expr::{LowerContext, ZenLiteral};
use crate::expr_lower::{lower_function_def, lower_lambda};
use crate::line_table::LineTable;
use crate::schema::annotation::extract_field_from_ast;
use crate::schema::annotation_check::{ExtractedField, ExtractedFieldKind};
use crate::schema::env::{CallbackRef, ConceptDecl, ConceptFieldAst, ModuleEnv};
use crate::schema::types::{InvariantClassification, KeySpec, LinkSpec, PropertyTypeSpec};

/// Lowers a declared `Concept` or interface into canonical `EntityIR` v2 [`JsonValue`].
pub(crate) fn lower_concept(
    concept: &ConceptDecl<'_>,
    env: &ModuleEnv<'_>,
    lines: &LineTable<'_>,
    constants: &BTreeMap<String, ZenLiteral>,
    options: &EnvelopeOptions,
) -> Result<JsonValue, PyAuthoringError> {
    validate_concept_structure(concept)?;

    let mut properties_ir = Vec::new();
    let mut links_ir = Vec::new();
    for field in &concept.fields {
        lower_concept_field(concept, field, env, &mut properties_ir, &mut links_ir)?;
    }

    let mut ctx = LowerContext::with_root("self")
        .with_allowed_fields(env.declared_field_names(&concept.py_name, &concept.span)?);
    ctx.constants = constants.clone();

    let lifecycles_ir = lower_concept_lifecycles(concept, lines, &ctx)?;
    let invariants_ir = lower_concept_invariants(concept, lines, &ctx)?;

    let namespace = concept.norn_name.rsplit_once('.').map_or("", |(ns, _)| ns);

    let (payload, suffix) = if concept.is_interface {
        let body = build_interface_body(concept, properties_ir, links_ir, invariants_ir);
        let mut payload = BTreeMap::new();
        payload.insert(
            "entityVersion".to_owned(),
            JsonValue::String("2.0".to_owned()),
        );
        payload.insert("interface".to_owned(), JsonValue::Object(body));
        (payload, "interface")
    } else {
        let body = build_object_type_body(
            concept,
            properties_ir,
            links_ir,
            lifecycles_ir,
            invariants_ir,
        );
        let mut payload = BTreeMap::new();
        payload.insert(
            "entityVersion".to_owned(),
            JsonValue::String("2.0".to_owned()),
        );
        payload.insert("objectType".to_owned(), JsonValue::Object(body));
        (payload, "entity")
    };

    let artifact_id = format!("{}.{suffix}", concept.norn_name);
    let header = EnvelopeHeader {
        kind: "entity",
        artifact_id: &artifact_id,
        namespace,
        name: &concept.title,
        version: &concept.version,
    };
    build_envelope(&header, payload, options, &concept.dependencies)
}

fn validate_concept_structure(concept: &ConceptDecl<'_>) -> Result<(), PyAuthoringError> {
    if concept.is_interface {
        if !concept.identity.is_empty() {
            return Err(PyAuthoringError::spanned(
                CODE_SCHEMA,
                format!(
                    "interface '{}' cannot declare identity keys",
                    concept.norn_name
                ),
                concept.span.clone(),
            ));
        }
        if concept.extends.is_some() {
            return Err(PyAuthoringError::spanned(
                CODE_SCHEMA,
                format!(
                    "interface '{}' must use `interface_extends=(...)`, not `extends=...`",
                    concept.norn_name
                ),
                concept.span.clone(),
            ));
        }
        if !concept.implements.is_empty() {
            return Err(PyAuthoringError::spanned(
                CODE_SCHEMA,
                format!(
                    "interface '{}' cannot declare `implements`; use `interface_extends`",
                    concept.norn_name
                ),
                concept.span.clone(),
            ));
        }
    } else if !concept.interface_extends.is_empty() {
        return Err(PyAuthoringError::spanned(
            CODE_SCHEMA,
            format!(
                "object type '{}' cannot declare `interface_extends`; use `implements`",
                concept.norn_name
            ),
            concept.span.clone(),
        ));
    }
    Ok(())
}

fn lower_concept_field(
    concept: &ConceptDecl<'_>,
    field: &ConceptFieldAst<'_>,
    env: &ModuleEnv<'_>,
    properties_ir: &mut Vec<JsonValue>,
    links_ir: &mut Vec<JsonValue>,
) -> Result<(), PyAuthoringError> {
    let extracted = extract_field_from_ast(&field.name, field.annotation, env, Some(&field.span))?;
    match extracted.kind.clone() {
        ExtractedFieldKind::Link(link_spec) => {
            links_ir.push(lower_link_field(concept, field, &extracted, &link_spec)?);
        }
        ExtractedFieldKind::Property(prop_spec) => {
            properties_ir.push(lower_property_field(
                concept, field, &extracted, &prop_spec,
            )?);
        }
    }
    Ok(())
}

fn lower_link_field(
    concept: &ConceptDecl<'_>,
    field: &ConceptFieldAst<'_>,
    extracted: &ExtractedField,
    link_spec: &LinkSpec,
) -> Result<JsonValue, PyAuthoringError> {
    let owner_field = format!("{}.{}", concept.norn_name, field.name);
    if !extracted.markings.is_empty() {
        return Err(PyAuthoringError::spanned(
            CODE_SCHEMA,
            format!(
                "link {owner_field} cannot declare property markings {:?}",
                extracted.markings
            ),
            field.span.clone(),
        ));
    }
    if extracted.meta.default.is_some() || field.default.is_some() {
        return Err(PyAuthoringError::spanned(
            CODE_SCHEMA,
            format!("link {owner_field} cannot declare a default value"),
            field.span.clone(),
        ));
    }
    if extracted.meta.description.is_some() {
        return Err(PyAuthoringError::spanned(
            CODE_SCHEMA,
            format!("link {owner_field} must pass description=... to link(...), not FieldMeta"),
            field.span.clone(),
        ));
    }
    if extracted.nullable && link_spec.required {
        return Err(PyAuthoringError::spanned(
            CODE_SCHEMA,
            format!("link {owner_field} cannot be both nullable (`| None`) and `required=True`"),
            field.span.clone(),
        ));
    }
    link_spec.to_ir(&field.name, concept.is_interface, Some(&field.span))
}

fn lower_property_field(
    concept: &ConceptDecl<'_>,
    field: &ConceptFieldAst<'_>,
    extracted: &ExtractedField,
    prop_spec: &PropertyTypeSpec,
) -> Result<JsonValue, PyAuthoringError> {
    let owner_field = format!("{}.{}", concept.norn_name, field.name);
    if extracted.meta.default.is_some() && field.default.is_some() {
        return Err(PyAuthoringError::spanned(
            CODE_SCHEMA,
            format!(
                "field {owner_field} cannot declare both FieldMeta(default=...) and a class-body default"
            ),
            field.span.clone(),
        ));
    }
    let raw_default = extracted.meta.default.as_ref().or(field.default.as_ref());
    let mut prop_obj = BTreeMap::new();
    if extracted.nullable {
        let mut constraints = BTreeMap::new();
        constraints.insert("nullable".to_owned(), JsonValue::Bool(true));
        prop_obj.insert("constraints".to_owned(), JsonValue::Object(constraints));
    }
    if let Some(def) = raw_default {
        if concept.is_interface {
            return Err(PyAuthoringError::spanned(
                CODE_FLOW_LITERAL,
                format!(
                    "interface property '{}' cannot declare a default",
                    field.name
                ),
                field.span.clone(),
            ));
        }
        prop_obj.insert(
            "default".to_owned(),
            def.to_ir(&owner_field, Some(&field.span))?,
        );
    }
    if let Some(desc) = &extracted.meta.description {
        prop_obj.insert("description".to_owned(), JsonValue::String(desc.clone()));
    }
    prop_obj.insert("id".to_owned(), JsonValue::String(field.name.clone()));
    if !extracted.markings.is_empty() {
        prop_obj.insert(
            "markings".to_owned(),
            JsonValue::Array(
                extracted
                    .markings
                    .iter()
                    .cloned()
                    .map(JsonValue::String)
                    .collect(),
            ),
        );
    }
    prop_obj.insert("type".to_owned(), prop_spec.to_ir());
    Ok(JsonValue::Object(prop_obj))
}

fn lower_concept_lifecycles(
    concept: &ConceptDecl<'_>,
    lines: &LineTable<'_>,
    ctx: &LowerContext,
) -> Result<Vec<JsonValue>, PyAuthoringError> {
    let mut out = Vec::with_capacity(concept.lifecycles.len());
    for lc in &concept.lifecycles {
        if concept.is_interface {
            return Err(PyAuthoringError::spanned(
                CODE_INTERFACE_LIFECYCLE,
                format!(
                    "interface '{}' cannot declare lifecycles (EntityIR v2 InterfaceDef has no lifecycles slot; declare lifecycles on implementing object types)",
                    concept.norn_name
                ),
                concept.span.clone(),
            ));
        }
        let mut transitions_ir = Vec::with_capacity(lc.transitions.len());
        for tr in &lc.transitions {
            transitions_ir.push(lower_transition(tr, lines, ctx)?);
        }
        let mut lc_obj = BTreeMap::new();
        if let Some(desc) = &lc.description {
            lc_obj.insert("description".to_owned(), JsonValue::String(desc.clone()));
        }
        if !lc.immutable.is_empty() {
            lc_obj.insert(
                "immutable".to_owned(),
                JsonValue::Array(
                    lc.immutable
                        .iter()
                        .cloned()
                        .map(JsonValue::String)
                        .collect(),
                ),
            );
        }
        lc_obj.insert(
            "initial".to_owned(),
            JsonValue::Array(lc.initial.iter().cloned().map(JsonValue::String).collect()),
        );
        lc_obj.insert(
            "property".to_owned(),
            JsonValue::String(lc.property.clone()),
        );
        if !lc.terminal.is_empty() {
            lc_obj.insert(
                "terminal".to_owned(),
                JsonValue::Array(lc.terminal.iter().cloned().map(JsonValue::String).collect()),
            );
        }
        if !transitions_ir.is_empty() {
            lc_obj.insert("transitions".to_owned(), JsonValue::Array(transitions_ir));
        }
        out.push(JsonValue::Object(lc_obj));
    }
    Ok(out)
}

fn lower_transition(
    tr: &crate::schema::env::TransitionAst<'_>,
    lines: &LineTable<'_>,
    ctx: &LowerContext,
) -> Result<JsonValue, PyAuthoringError> {
    let mut guard_zens = Vec::with_capacity(tr.guards.len());
    for &guard in &tr.guards {
        let expr = match guard {
            CallbackRef::Func(f) => lower_function_def(f, lines, ctx)?,
            CallbackRef::Lambda(l) => lower_lambda(l, lines, ctx)?,
        };
        guard_zens.push(JsonValue::String(expr.to_zen()?));
    }
    let mut tr_obj = BTreeMap::new();
    if let Some(desc) = &tr.description {
        tr_obj.insert("description".to_owned(), JsonValue::String(desc.clone()));
    }
    tr_obj.insert(
        "from".to_owned(),
        JsonValue::Array(
            tr.from_states
                .iter()
                .cloned()
                .map(JsonValue::String)
                .collect(),
        ),
    );
    if let Some(code) = &tr.guard_refusal_code {
        tr_obj.insert(
            "guardRefusalCode".to_owned(),
            JsonValue::String(code.clone()),
        );
    }
    if !guard_zens.is_empty() {
        tr_obj.insert("guards".to_owned(), JsonValue::Array(guard_zens));
    }
    tr_obj.insert("id".to_owned(), JsonValue::String(tr.id.clone()));
    tr_obj.insert("to".to_owned(), JsonValue::String(tr.to_state.clone()));
    Ok(JsonValue::Object(tr_obj))
}

fn lower_concept_invariants(
    concept: &ConceptDecl<'_>,
    lines: &LineTable<'_>,
    ctx: &LowerContext,
) -> Result<Vec<JsonValue>, PyAuthoringError> {
    let mut out = Vec::with_capacity(concept.invariants.len());
    for inv in &concept.invariants {
        let expr = lower_function_def(inv.func, lines, ctx)?;
        let zen = expr.to_zen()?;
        let mut obj = BTreeMap::new();
        if inv.classification != InvariantClassification::Integrity {
            obj.insert(
                "classification".to_owned(),
                JsonValue::String(inv.classification.as_str().to_owned()),
            );
        }
        if let Some(desc) = &inv.description {
            obj.insert("description".to_owned(), JsonValue::String(desc.clone()));
        }
        obj.insert("expression".to_owned(), JsonValue::String(zen));
        obj.insert("id".to_owned(), JsonValue::String(inv.id.clone()));
        obj.insert("message".to_owned(), JsonValue::String(inv.message.clone()));
        out.push(JsonValue::Object(obj));
    }
    Ok(out)
}

fn build_interface_body(
    concept: &ConceptDecl<'_>,
    properties_ir: Vec<JsonValue>,
    links_ir: Vec<JsonValue>,
    invariants_ir: Vec<JsonValue>,
) -> BTreeMap<String, JsonValue> {
    let mut body = BTreeMap::new();
    if let Some(desc) = &concept.description {
        body.insert("description".to_owned(), JsonValue::String(desc.clone()));
    }
    if !concept.interface_extends.is_empty() {
        body.insert(
            "extends".to_owned(),
            JsonValue::Array(
                concept
                    .interface_extends
                    .iter()
                    .cloned()
                    .map(JsonValue::String)
                    .collect(),
            ),
        );
    }
    if !invariants_ir.is_empty() {
        body.insert("invariants".to_owned(), JsonValue::Array(invariants_ir));
    }
    if !links_ir.is_empty() {
        body.insert("links".to_owned(), JsonValue::Array(links_ir));
    }
    if !concept.markings.is_empty() {
        body.insert(
            "markings".to_owned(),
            JsonValue::Array(
                concept
                    .markings
                    .iter()
                    .cloned()
                    .map(JsonValue::String)
                    .collect(),
            ),
        );
    }
    body.insert(
        "name".to_owned(),
        JsonValue::String(concept.norn_name.clone()),
    );
    if !properties_ir.is_empty() {
        body.insert("properties".to_owned(), JsonValue::Array(properties_ir));
    }
    if let Some(role) = concept.role {
        body.insert(
            "role".to_owned(),
            JsonValue::String(role.as_str().to_owned()),
        );
    }
    body
}

fn build_object_type_body(
    concept: &ConceptDecl<'_>,
    properties_ir: Vec<JsonValue>,
    links_ir: Vec<JsonValue>,
    lifecycles_ir: Vec<JsonValue>,
    invariants_ir: Vec<JsonValue>,
) -> BTreeMap<String, JsonValue> {
    let mut body = BTreeMap::new();
    if let Some(desc) = &concept.description {
        body.insert("description".to_owned(), JsonValue::String(desc.clone()));
    }
    if let Some(ext) = &concept.extends {
        body.insert("extends".to_owned(), JsonValue::String(ext.clone()));
    }
    if !concept.identity.is_empty() {
        body.insert(
            "identity".to_owned(),
            JsonValue::Array(concept.identity.iter().map(KeySpec::to_ir).collect()),
        );
    }
    if !concept.implements.is_empty() {
        body.insert(
            "implements".to_owned(),
            JsonValue::Array(
                concept
                    .implements
                    .iter()
                    .cloned()
                    .map(JsonValue::String)
                    .collect(),
            ),
        );
    }
    if !invariants_ir.is_empty() {
        body.insert("invariants".to_owned(), JsonValue::Array(invariants_ir));
    }
    if !lifecycles_ir.is_empty() {
        body.insert("lifecycles".to_owned(), JsonValue::Array(lifecycles_ir));
    }
    if !links_ir.is_empty() {
        body.insert("links".to_owned(), JsonValue::Array(links_ir));
    }
    if !concept.markings.is_empty() {
        body.insert(
            "markings".to_owned(),
            JsonValue::Array(
                concept
                    .markings
                    .iter()
                    .cloned()
                    .map(JsonValue::String)
                    .collect(),
            ),
        );
    }
    body.insert(
        "name".to_owned(),
        JsonValue::String(concept.norn_name.clone()),
    );
    if !properties_ir.is_empty() {
        body.insert("properties".to_owned(), JsonValue::Array(properties_ir));
    }
    if let Some(role) = concept.role {
        body.insert(
            "role".to_owned(),
            JsonValue::String(role.as_str().to_owned()),
        );
    }
    body
}
