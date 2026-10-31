//! `OpenAPI` projections of canonical IR and checker wire contracts.
//!
//! Definition artifacts stay as raw JSON at runtime so exact numeric literals
//! survive reads. This module only supplies schemas for those bytes and typed
//! DTOs for the checker's stable JSON documents.

use serde::Serialize;
use serde_json::{Value, json};
use utoipa::openapi::schema::{
    AdditionalProperties, AllOf, AnyOf, Array, ArrayItems, Object, OneOf, Ref, Schema, SchemaType,
    Type,
};
use utoipa::{Modify, ToSchema};

/// The schemas that are not expressible by `ToSchema` alone: canonical Norn IR
/// definitions and the install response unions.
pub(crate) struct ContractSchemas;

impl Modify for ContractSchemas {
    fn modify(&self, openapi: &mut utoipa::openapi::OpenApi) {
        let components = openapi
            .components
            .get_or_insert_with(utoipa::openapi::Components::new);
        for (name, schema) in artifact_envelope_schemas()
            .into_iter()
            .chain(project_schemars_components(
                norn_ir::layout_sidecar_schema(),
                "LayoutSidecar",
                "NornIrLayout",
            ))
            .chain(project_schemars_components(
                norn_ir::limit_profile_ref_schema(),
                "NornIrLimitProfileRef",
                "NornIr",
            ))
            .chain(project_schemars_components(
                norn_ir::diagnostic_severity_schema(),
                "NornIrDiagnosticSeverity",
                "NornIr",
            ))
            .chain(project_schemars_components(
                norn_ir::diagnostic_remediation_schema(),
                "NornIrDiagnosticRemediation",
                "NornIr",
            ))
            .chain(project_schemars_components(
                norn_ir::flow_source_map_schema(),
                "NornIrFlowSourceMap",
                "NornIr",
            ))
            .chain(project_schemars_components(
                json!({
                    "title": "NornExprTypeEnvRef",
                    "description": "The identity of the ZEN Expression type environment recorded in a checker receipt.",
                    "type": "object",
                    "required": ["id", "version", "sha256"],
                    "properties": {
                        "id": { "type": "string" },
                        "version": { "type": "integer", "minimum": 0 },
                        "sha256": { "type": "string" }
                    },
                    "additionalProperties": false
                }),
                "NornExprTypeEnvRef",
                "NornExpr",
            ))
        {
            components
                .schemas
                .insert(name, utoipa::openapi::RefOr::T(schema));
        }
    }
}

/// The checker's stable diagnostics JSON document (`norn-checker/diagnostics@1`).
#[derive(Debug, Clone, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct NornCheckerDiagnostics {
    /// Versioned diagnostics document format.
    pub format: NornCheckerDiagnosticsFormat,
    /// The checker version that produced these findings.
    pub checker_version: String,
    /// Findings, in checker-reported order.
    pub diagnostics: Vec<NornCheckerDiagnostic>,
}

/// The current stable checker diagnostics format.
#[derive(Debug, Clone, Copy, ToSchema)]
pub enum NornCheckerDiagnosticsFormat {
    /// `norn-checker/diagnostics@1`.
    #[serde(rename = "norn-checker/diagnostics@1")]
    V1,
}

/// One finding in the checker's stable diagnostics document.
#[derive(Debug, Clone, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct NornCheckerDiagnostic {
    /// Stable machine-readable checker code.
    pub code: String,
    /// The checker tier that emitted this finding, when its code is registered.
    #[schema(required = true, nullable = true)]
    pub tier: Option<NornCheckerTier>,
    /// Severity, using the canonical Norn IR enum schema.
    #[schema(schema_with = diagnostic_severity_schema)]
    pub severity: String,
    /// Human-readable explanation.
    pub message: String,
    /// Source location in the artifact JSON, when known.
    #[schema(required = true, nullable = true)]
    pub span: Option<NornCheckerDiagnosticSpan>,
    /// The artifact that owns this finding, when known.
    #[schema(required = true, nullable = true)]
    pub artifact_id: Option<String>,
    /// Suggested remediation, using the canonical Norn IR enum schema.
    #[schema(required = true, nullable = true, schema_with = diagnostic_remediation_schema)]
    pub remediation: Option<String>,
}

/// Checker tier labels are wire strings such as `T2` and `T3`.
#[derive(Debug, Clone, Copy, ToSchema)]
pub enum NornCheckerTier {
    /// Starlark syntax and load resolution.
    #[serde(rename = "T0")]
    T0,
    /// Builder construction.
    #[serde(rename = "T1")]
    T1,
    /// Per-artifact IR validation.
    #[serde(rename = "T2")]
    T2,
    /// Artifact-set and graph semantics.
    #[serde(rename = "T3")]
    T3,
    /// Compatibility planning.
    #[serde(rename = "T4")]
    T4,
    /// Activation checks.
    #[serde(rename = "T5")]
    T5,
    /// Runtime and commit checks.
    #[serde(rename = "T6")]
    T6,
    /// Gateway and edge checks.
    #[serde(rename = "T7")]
    T7,
}

/// A JSON pointer and optional source-text range for a checker finding.
#[derive(Debug, Clone, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct NornCheckerDiagnosticSpan {
    /// RFC 6901 JSON pointer into the artifact.
    pub pointer: String,
    /// Byte range in the original source text, when available.
    #[schema(required = true, nullable = true)]
    pub source: Option<NornCheckerSourceSpan>,
}

/// Start-inclusive, end-exclusive byte offsets in the original source text.
#[derive(Debug, Clone, ToSchema)]
pub struct NornCheckerSourceSpan {
    /// First byte of the source span.
    pub start: u64,
    /// Byte immediately after the source span.
    pub end: u64,
}

/// A current checker admission receipt.
#[derive(Debug, Clone, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct NornCheckReceipt {
    /// Versioned receipt format.
    pub format: NornCheckReceiptFormat,
    /// Checker identity.
    pub checker: NornCheckReceiptChecker,
    /// Candidate and installed-base artifact identities.
    pub artifact_set: NornCheckReceiptArtifactSet,
    /// Canonical resource-limit profile reference.
    #[schema(schema_with = receipt_profile_schema)]
    pub limit_profile: Value,
    /// ZEN expression profile used by the checker.
    pub expr_profile: String,
    /// Digest of all findings in this check.
    pub diagnostics: NornCheckReceiptDiagnosticsDigest,
    /// Whether the candidate set passed admission.
    pub admitted: bool,
    /// Identifier admission profile applied to the set.
    pub identifier_profile: String,
    /// Derived ZEN type environment, omitted when no environment was produced.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schema(schema_with = type_env_ref_schema)]
    pub type_env: Option<Value>,
    /// Non-executable authoring projection bound to the admitted artifacts.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schema(schema_with = flow_source_map_schema)]
    pub flow_source_map: Option<norn_ir::FlowSourceMap>,
    /// Origins of laws inherited from installed versioned composition packs.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub law_enrollment: Option<Vec<NornLawEnrollment>>,
}

/// One receipt-bound inherited law; this conveys provenance, not proof status.
#[derive(Debug, Clone, ToSchema)]
pub struct NornLawEnrollment {
    /// Pack, artifact digest and stable law member identity.
    pub inherited_from: NornInheritedLawOrigin,
}

/// Exact origin of a law inherited from an installed composition pack.
#[derive(Debug, Clone, ToSchema)]
pub struct NornInheritedLawOrigin {
    /// Pack identity.
    pub pack_id: String,
    /// Pack version.
    pub pack_version: String,
    /// Artifact envelope identity.
    pub artifact_id: String,
    /// Norn1 semantic artifact digest.
    pub artifact_digest: String,
    /// Stable law member identity.
    pub law_id: String,
}

/// Receipt format for a newly issued checker receipt.
#[derive(Debug, Clone, Copy, ToSchema)]
pub enum NornCheckReceiptFormat {
    /// `norn-checker/receipt@2`.
    #[serde(rename = "norn-checker/receipt@2")]
    V2,
    /// `norn-checker/receipt@3`.
    #[serde(rename = "norn-checker/receipt@3")]
    V3,
    /// `norn-checker/receipt@4`, with inherited-law origins.
    #[serde(rename = "norn-checker/receipt@4")]
    V4,
}

/// Checker name and package version in a receipt.
#[derive(Debug, Clone, ToSchema)]
pub struct NornCheckReceiptChecker {
    /// Checker name.
    pub name: String,
    /// Checker package version.
    pub version: String,
}

/// The candidate and registry-base identities attested by a receipt.
#[derive(Debug, Clone, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct NornCheckReceiptArtifactSet {
    /// Candidate artifact identities.
    pub artifacts: Vec<NornCheckReceiptArtifactRef>,
    /// Registry-base artifact identities.
    pub base: Vec<NornCheckReceiptArtifactRef>,
    /// Norn1 hash of both ordered identity lists.
    pub sha256: String,
}

/// One artifact identity in a checker receipt.
#[derive(Debug, Clone, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct NornCheckReceiptArtifactRef {
    /// Canonical artifact id.
    #[schema(rename = "artifactId")]
    pub artifact_id: String,
    /// Artifact version.
    pub version: String,
    /// Lower-case hexadecimal Norn1 content hash.
    pub sha256: String,
}

/// The finding count, error count, and content hash attested by a receipt.
#[derive(Debug, Clone, ToSchema)]
pub struct NornCheckReceiptDiagnosticsDigest {
    /// Number of findings.
    pub count: u64,
    /// Number of error findings.
    pub errors: u64,
    /// Lower-case hexadecimal Norn1 hash of the diagnostics document.
    pub sha256: String,
}

fn diagnostic_severity_schema() -> Ref {
    Ref::from_schema_name("NornIrDiagnosticSeverity")
}

fn diagnostic_remediation_schema() -> Schema {
    nullable_reference_schema(Ref::from_schema_name("NornIrDiagnosticRemediation"))
}

fn receipt_profile_schema() -> Ref {
    Ref::from_schema_name("NornIrLimitProfileRef")
}

fn type_env_ref_schema() -> Ref {
    Ref::from_schema_name("NornExprTypeEnvRef")
}

pub(crate) fn flow_source_map_schema() -> Ref {
    Ref::from_schema_name("NornIrFlowSourceMap")
}

fn nullable_reference_schema(reference: Ref) -> Schema {
    let mut schema = AnyOf::new();
    schema.items.push(utoipa::openapi::RefOr::Ref(reference));
    schema
        .items
        .push(utoipa::openapi::RefOr::T(null_receipt_schema()));
    Schema::AnyOf(schema)
}

fn null_receipt_schema() -> Schema {
    Object::with_type(Type::Null).into()
}

#[allow(
    clippy::expect_used,
    clippy::too_many_lines,
    reason = "canonical IR schemas are projected during OpenAPI document generation"
)]
fn artifact_envelope_schemas() -> Vec<(String, Schema)> {
    let mut root = serde_json::to_value(norn_ir::artifact_envelope_schema())
        .expect("the canonical artifact envelope schema is serializable");
    let mut definitions = root
        .as_object_mut()
        .and_then(|schema| schema.remove("$defs"))
        .and_then(|definitions| definitions.as_object().cloned())
        .expect("the canonical artifact envelope schema has nested definitions");
    if let Some(root) = root.as_object_mut() {
        root.remove("$schema");
    }
    normalize_json_definitions(&mut definitions);
    adapt_schemars_schema(&mut root, "NornIr", &definitions);

    let root_object = root
        .as_object_mut()
        .expect("the canonical artifact envelope schema is an object");
    let variants = root_object
        .remove("oneOf")
        .and_then(|variants| variants.as_array().cloned())
        .expect("the canonical artifact envelope schema has kind variants");
    let common_schema = Value::Object(root_object.clone());
    let common_name = "NornArtifactEnvelopeCommon";
    let mut schemas = Vec::with_capacity(
        definitions
            .len()
            .saturating_add(variants.len())
            .saturating_add(2),
    );
    schemas.push((common_name.to_owned(), parse_openapi_schema(common_schema)));

    let mut one_of = Vec::with_capacity(variants.len());
    let mut mapping = serde_json::Map::new();
    for mut variant in variants {
        adapt_schemars_schema(&mut variant, "NornIr", &definitions);
        let kind = variant
            .pointer("/properties/kind/enum/0")
            .or_else(|| variant.pointer("/properties/kind/const"))
            .and_then(Value::as_str)
            .expect("each artifact envelope branch has one literal kind");
        let variant_name = format!("NornArtifactEnvelope{}", pascal_case(kind));
        let component_ref = format!("#/components/schemas/{variant_name}");
        schemas.push((
            variant_name.clone(),
            parse_openapi_schema(json!({
                "title": variant_name,
                "allOf": [
                    { "$ref": format!("#/components/schemas/{common_name}") },
                    variant
                ]
            })),
        ));
        one_of.push(json!({ "$ref": component_ref }));
        mapping.insert(kind.to_owned(), Value::String(component_ref));
    }

    schemas.push((
        "NornArtifactEnvelope".to_owned(),
        parse_openapi_schema(json!({
            "title": "NornArtifactEnvelope",
            "description": "A canonical Norn IR artifact. Each branch pairs the envelope kind with its matching closed payload schema.",
            "oneOf": one_of,
            "discriminator": { "propertyName": "kind", "mapping": mapping }
        })),
    ));
    for (name, definition) in &definitions {
        let mut definition = definition.clone();
        adapt_schemars_schema(&mut definition, "NornIr", &definitions);
        let name = format!("NornIr{name}");
        let mut hoisted = Vec::new();
        hoist_inline_objects(&mut definition, &name, false, &mut hoisted);
        schemas.push((name, parse_component_schema(definition)));
        schemas.extend(
            hoisted
                .into_iter()
                .map(|(name, schema)| (name, parse_component_schema(schema))),
        );
    }
    schemas
}

/// Project a canonical Schemars root and its `$defs` into `OpenAPI` components.
/// Callers choose stable root and nested names so they can compose definitions
/// from multiple canonical sources without collisions.
#[allow(
    clippy::expect_used,
    reason = "canonical IR schemas are projected during OpenAPI document generation"
)]
pub(crate) fn project_schemars_components(
    schema: impl Serialize,
    root_name: &str,
    nested_prefix: &str,
) -> Vec<(String, Schema)> {
    let mut root = serde_json::to_value(schema)
        .expect("canonical IR JSON Schemas are serializable during OpenAPI generation");
    let mut definitions = root
        .as_object_mut()
        .and_then(|schema| schema.remove("$defs"))
        .and_then(|definitions| definitions.as_object().cloned())
        .unwrap_or_default();
    if let Some(root) = root.as_object_mut() {
        root.remove("$schema");
    }
    normalize_json_definitions(&mut definitions);
    adapt_schemars_schema(&mut root, nested_prefix, &definitions);
    let mut schemas = Vec::with_capacity(definitions.len().saturating_add(1));
    schemas.push((root_name.to_owned(), parse_component_schema(root)));
    for (name, definition) in &definitions {
        let mut definition = definition.clone();
        adapt_schemars_schema(&mut definition, nested_prefix, &definitions);
        schemas.push((
            format!("{nested_prefix}{name}"),
            parse_component_schema(definition),
        ));
    }
    schemas
}

// Reuse canonical inline object schemas as components. The Python generator
// retries forward union references, and reparsing inline children on each retry
// otherwise creates duplicate model names and silently drops those branches.
fn hoist_inline_objects(
    schema: &mut Value,
    name: &str,
    inline: bool,
    components: &mut Vec<(String, Value)>,
) {
    let Some(object) = schema.as_object_mut() else {
        return;
    };
    if let Some(properties) = object.get_mut("properties").and_then(Value::as_object_mut) {
        for (property, child) in properties {
            hoist_inline_objects(
                child,
                &format!("{name}Property{}", pascal_case(property)),
                true,
                components,
            );
        }
    }
    for field in ["items", "additionalProperties", "propertyNames"] {
        if let Some(child) = object.get_mut(field) {
            hoist_inline_objects(
                child,
                &format!("{name}{}", pascal_case(field)),
                true,
                components,
            );
        }
    }
    for field in ["oneOf", "anyOf", "allOf", "prefixItems"] {
        if let Some(children) = object.get_mut(field).and_then(Value::as_array_mut) {
            for (index, child) in children.iter_mut().enumerate() {
                hoist_inline_objects(
                    child,
                    &format!("{name}{}{index}", pascal_case(field)),
                    true,
                    components,
                );
            }
        }
    }
    let nullable = match object.get("type") {
        Some(Value::String(kind)) if kind == "object" => Some(false),
        Some(Value::Array(kinds))
            if kinds.iter().any(|kind| kind.as_str() == Some("object"))
                && kinds
                    .iter()
                    .all(|kind| matches!(kind.as_str(), Some("object" | "null"))) =>
        {
            Some(kinds.iter().any(|kind| kind.as_str() == Some("null")))
        }
        _ => None,
    };
    if inline && let Some(nullable) = nullable {
        let mut component = std::mem::take(schema);
        let mut annotations = serde_json::Map::new();
        if nullable && let Some(object) = component.as_object_mut() {
            object.insert("type".to_owned(), json!("object"));
            for field in ["title", "description", "default", "examples", "example"] {
                if let Some(annotation) = object.remove(field) {
                    annotations.insert(field.to_owned(), annotation);
                }
            }
        }
        components.push((name.to_owned(), component));
        let reference = json!({ "$ref": format!("#/components/schemas/{name}") });
        *schema = if nullable {
            json!({ "anyOf": [reference, { "type": "null" }] })
        } else {
            reference
        };
        if let Some(wrapper) = schema.as_object_mut() {
            wrapper.extend(annotations);
        }
    }
}

// These are equivalent normalizations of the canonical JSON-value domain.
// The pinned Python generator cannot resolve recursive unions of primitives
// or an allOf intersection between those unions. No IR fields are restated.
fn normalize_json_definitions(definitions: &mut serde_json::Map<String, Value>) {
    let Some(mut json_value) = definitions.get("JsonValue").cloned() else {
        return;
    };
    remove_json_self_references(&mut json_value);
    definitions.insert("JsonValue".to_owned(), json_value.clone());
    for definition in definitions.values_mut() {
        if definition.get("$ref").and_then(Value::as_str) == Some("#/$defs/JsonValue")
            && definition.pointer("/not/type").and_then(Value::as_str) == Some("null")
        {
            let mut non_null = json_value.clone();
            if let Some(variants) = non_null.get_mut("anyOf").and_then(Value::as_array_mut) {
                variants
                    .retain(|variant| variant.get("type").and_then(Value::as_str) != Some("null"));
            }
            if let (Some(source), Some(target)) = (definition.as_object(), non_null.as_object_mut())
            {
                target.extend(
                    source
                        .iter()
                        .filter(|(key, _)| key.as_str() != "$ref" && key.as_str() != "not")
                        .map(|(key, value)| (key.clone(), value.clone())),
                );
            }
            *definition = non_null;
        }
    }
}

fn remove_json_self_references(schema: &mut Value) {
    match schema {
        Value::Object(object) => {
            if object.get("$ref").and_then(Value::as_str) == Some("#/$defs/JsonValue") {
                object.remove("$ref");
            }
            for child in object.values_mut() {
                remove_json_self_references(child);
            }
        }
        Value::Array(values) => {
            for child in values {
                remove_json_self_references(child);
            }
        }
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => {}
    }
}

fn pascal_case(value: &str) -> String {
    value
        .split('_')
        .filter(|part| !part.is_empty())
        .map(|part| {
            let mut chars = part.chars();
            chars.next().map_or_else(String::new, |first| {
                first.to_ascii_uppercase().to_string() + chars.as_str()
            })
        })
        .collect()
}

#[allow(
    clippy::expect_used,
    reason = "unrepresentable canonical schema is a contract-generation defect"
)]
fn parse_openapi_schema(value: Value) -> Schema {
    if let Some(object) = value.as_object() {
        if object.contains_key("oneOf") {
            return parse_composite_schema(&value, "oneOf");
        }
        if object.contains_key("allOf") {
            return parse_composite_schema(&value, "allOf");
        }
        if object.contains_key("anyOf") {
            return parse_composite_schema(&value, "anyOf");
        }
        if object.get("type").and_then(Value::as_str) == Some("array") {
            return parse_array_schema(&value);
        }
    }
    if value == Value::Bool(false) {
        let mut schema = Object::default();
        schema.enum_values = Some(Vec::new());
        return Schema::Object(schema);
    }
    if value == Value::Bool(true) {
        return Schema::Object(Object::with_type(SchemaType::AnyValue));
    }
    let object = serde_json::from_value(value).expect("a canonical JSON Schema is an object");
    Schema::Object(parse_object_schema(object))
}

#[allow(
    clippy::expect_used,
    reason = "canonical references are validated while generating OpenAPI components"
)]
fn parse_component_schema(value: Value) -> Schema {
    match parse_ref_or_schema(value) {
        utoipa::openapi::RefOr::T(schema) => schema,
        utoipa::openapi::RefOr::Ref(reference) => {
            let mut schema = AllOf::new();
            schema.items.push(utoipa::openapi::RefOr::Ref(reference));
            Schema::AllOf(schema)
        }
    }
}

#[allow(
    clippy::expect_used,
    clippy::too_many_lines,
    reason = "recursive conversion validates canonical composition shapes during generation"
)]
fn parse_composite_schema(value: &Value, composition: &str) -> Schema {
    let mut object = value
        .as_object()
        .cloned()
        .expect("a JSON Schema composition is an object");
    let members = object
        .remove(composition)
        .and_then(|members| members.as_array().cloned())
        .expect("a JSON Schema composition contains an array of schemas");

    // Keep the fields supported directly by Utoipa's composition structs in
    // place. Move every other sibling constraint into an allOf member instead
    // of silently discarding it (for example, `required` beside `oneOf`).
    let mut metadata = serde_json::Map::new();
    let mut siblings = serde_json::Map::new();
    for (key, value) in object {
        if matches!(
            key.as_str(),
            "type" | "title" | "description" | "default" | "example" | "examples" | "discriminator"
        ) || key.starts_with("x-")
        {
            metadata.insert(key, value);
        } else {
            siblings.insert(key, value);
        }
    }

    metadata.insert(composition.to_owned(), Value::Array(Vec::new()));
    let shell = Value::Object(metadata);
    let parsed_members = members.into_iter().map(parse_ref_or_schema).collect();
    let composite = match composition {
        "oneOf" => {
            let mut schema: OneOf = serde_json::from_value(shell)
                .expect("canonical JSON Schema oneOf metadata projects into OpenAPI");
            schema.items = parsed_members;
            Schema::OneOf(schema)
        }
        "allOf" => {
            let mut schema: AllOf = serde_json::from_value(shell)
                .expect("canonical JSON Schema allOf metadata projects into OpenAPI");
            schema.items = parsed_members;
            Schema::AllOf(schema)
        }
        "anyOf" => {
            let mut schema: AnyOf = serde_json::from_value(shell)
                .expect("canonical JSON Schema anyOf metadata projects into OpenAPI");
            schema.items = parsed_members;
            Schema::AnyOf(schema)
        }
        _ => {
            let mut schema: AnyOf = serde_json::from_value(shell)
                .expect("known composition metadata projects into OpenAPI");
            schema.items = parsed_members;
            Schema::AnyOf(schema)
        }
    };

    if siblings.is_empty() {
        return composite;
    }
    let sibling_schema = match parse_ref_or_schema(Value::Object(siblings)) {
        utoipa::openapi::RefOr::T(schema) => schema,
        utoipa::openapi::RefOr::Ref(reference) => {
            let mut schema = AllOf::new();
            schema.items.push(utoipa::openapi::RefOr::Ref(reference));
            Schema::AllOf(schema)
        }
    };
    if let Schema::AllOf(mut schema) = composite {
        schema
            .items
            .insert(0, utoipa::openapi::RefOr::T(sibling_schema));
        Schema::AllOf(schema)
    } else {
        let mut schema = AllOf::new();
        schema.items.push(utoipa::openapi::RefOr::T(sibling_schema));
        schema.items.push(utoipa::openapi::RefOr::T(composite));
        Schema::AllOf(schema)
    }
}

#[allow(
    clippy::expect_used,
    reason = "canonical references are validated while generating the OpenAPI document"
)]
fn parse_ref_or_schema(value: Value) -> utoipa::openapi::RefOr<Schema> {
    if let Some(mut object) = value.as_object().cloned()
        && let Some(reference) = object
            .remove("$ref")
            .and_then(|reference| reference.as_str().map(str::to_owned))
    {
        let mut ref_fields = serde_json::Map::new();
        ref_fields.insert("$ref".to_owned(), Value::String(reference));
        for field in ["description", "summary"] {
            if let Some(value) = object.remove(field) {
                ref_fields.insert(field.to_owned(), value);
            }
        }
        let reference: Ref = serde_json::from_value(Value::Object(ref_fields))
            .expect("JSON Schema references project into OpenAPI references");
        if object.is_empty() {
            return utoipa::openapi::RefOr::Ref(reference);
        }
        let mut all_of = AllOf::new();
        all_of.items.push(utoipa::openapi::RefOr::Ref(reference));
        all_of
            .items
            .push(utoipa::openapi::RefOr::T(parse_openapi_schema(
                Value::Object(object),
            )));
        return utoipa::openapi::RefOr::T(Schema::AllOf(all_of));
    }
    utoipa::openapi::RefOr::T(parse_openapi_schema(value))
}

#[allow(
    clippy::expect_used,
    reason = "canonical arrays are validated while generating the OpenAPI document"
)]
fn parse_array_schema(value: &Value) -> Schema {
    let mut object = value
        .as_object()
        .cloned()
        .expect("a JSON Schema array is an object");
    // JSON Schema permits omitted `items`, which means any item is valid.
    // Utoipa requires the field, so `{}` is the equivalent unconstrained item.
    let items = object.remove("items").unwrap_or_else(|| json!({}));
    let prefix_items = object
        .remove("prefixItems")
        .and_then(|items| items.as_array().cloned())
        .unwrap_or_default();
    object.insert("items".to_owned(), json!({ "type": "object" }));
    object.insert("prefixItems".to_owned(), Value::Array(Vec::new()));
    let mut array: Array = serde_json::from_value(Value::Object(object))
        .expect("canonical JSON Schema array metadata projects into OpenAPI");
    array.items = match items {
        Value::Bool(false) => ArrayItems::False,
        Value::Bool(true) => ArrayItems::RefOrSchema(Box::new(utoipa::openapi::RefOr::T(
            Schema::Object(Object::with_type(SchemaType::AnyValue)),
        ))),
        schema @ (Value::Null
        | Value::Number(_)
        | Value::String(_)
        | Value::Array(_)
        | Value::Object(_)) => ArrayItems::RefOrSchema(Box::new(parse_ref_or_schema(schema))),
    };
    array.prefix_items = prefix_items.into_iter().map(parse_openapi_schema).collect();
    Schema::Array(array)
}

#[allow(
    clippy::expect_used,
    reason = "canonical objects are validated while generating the OpenAPI document"
)]
fn parse_object_schema(mut object: serde_json::Map<String, Value>) -> Object {
    let properties = object
        .remove("properties")
        .and_then(|properties| properties.as_object().cloned())
        .unwrap_or_default();
    let additional_properties = object.remove("additionalProperties");
    let property_names = object.remove("propertyNames");
    let constant = object.remove("const");
    object.insert("properties".to_owned(), json!({}));
    let missing_type = !object.contains_key("type");
    object.entry("type").or_insert_with(|| json!("object"));
    let mut schema: Object = serde_json::from_value(Value::Object(object))
        .expect("canonical JSON Schema object metadata projects into OpenAPI");
    if missing_type {
        schema.schema_type = SchemaType::AnyValue;
    }
    if let Some(constant) = constant {
        // Utoipa has no const field; its flattened map preserves this OAS 3.1 keyword.
        schema
            .extensions
            .get_or_insert_with(Default::default)
            .insert("const".to_owned(), constant);
    }
    for (name, property_schema) in properties {
        schema
            .properties
            .insert(name, parse_ref_or_schema(property_schema));
    }
    if let Some(additional_properties) = additional_properties {
        schema.additional_properties = Some(Box::new(match additional_properties {
            Value::Bool(allowed) => AdditionalProperties::FreeForm(allowed),
            schema @ (Value::Null
            | Value::Number(_)
            | Value::String(_)
            | Value::Array(_)
            | Value::Object(_)) => AdditionalProperties::RefOr(parse_ref_or_schema(schema)),
        }));
    }
    if let Some(property_names) = property_names {
        schema.property_names = Some(Box::new(parse_openapi_schema(property_names)));
    }
    schema
}

fn adapt_schemars_schema(
    value: &mut Value,
    nested_prefix: &str,
    definitions: &serde_json::Map<String, Value>,
) {
    match value {
        Value::Array(values) => {
            for value in values {
                adapt_schemars_schema(value, nested_prefix, definitions);
            }
        }
        Value::Object(object) => {
            for value in object.values_mut() {
                adapt_schemars_schema(value, nested_prefix, definitions);
            }
            normalize_scalar_literal_union(object);
            if let Some(reference) = object
                .get("$ref")
                .and_then(Value::as_str)
                .and_then(|reference| reference.strip_prefix("#/$defs/"))
                && object.len() > 1
                && let Some(inlined) =
                    inline_non_object_ref_siblings(object, reference, definitions)
            {
                *value = inlined;
                adapt_schemars_schema(value, nested_prefix, definitions);
                return;
            }
            adapt_closed_pattern_map(object);
            if let Some(Value::String(reference)) = object.get_mut("$ref")
                && let Some(name) = reference.strip_prefix("#/$defs/")
            {
                *reference = format!("#/components/schemas/{nested_prefix}{name}");
            }
            if let Some(constant) = object.remove("const") {
                if constant.is_boolean() {
                    // The Python generator supports boolean const, but not boolean enums.
                    object.insert("type".to_owned(), json!("boolean"));
                    object.insert("const".to_owned(), constant);
                } else {
                    object.insert("enum".to_owned(), Value::Array(vec![constant]));
                }
            }
            let excludes_null = object
                .get("not")
                .and_then(|schema| schema.get("type"))
                .and_then(Value::as_str)
                == Some("null");
            if excludes_null {
                let mut constrained = object.clone();
                constrained.remove("not");
                *object = serde_json::Map::from_iter([(
                    "allOf".to_owned(),
                    json!([
                        constrained,
                        {
                            "anyOf": [
                                { "type": "boolean" },
                                { "type": "number" },
                                { "type": "string" },
                                { "type": "array" },
                                { "type": "object" }
                            ]
                        }
                    ]),
                )]);
            }
        }
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => {}
    }
}

// Utoipa has no patternProperties field. A closed map with one key pattern
// has the equivalent additionalProperties + propertyNames representation,
// which also lets both SDK generators retain the typed map values.
fn adapt_closed_pattern_map(object: &mut serde_json::Map<String, Value>) {
    if object.get("additionalProperties") != Some(&Value::Bool(false))
        || object.contains_key("propertyNames")
        || object
            .get("properties")
            .and_then(Value::as_object)
            .is_some_and(|properties| !properties.is_empty())
    {
        return;
    }
    let Some(patterns) = object.get("patternProperties").and_then(Value::as_object) else {
        return;
    };
    if patterns.len() != 1 {
        return;
    }
    let Some((pattern, values)) = patterns.iter().next() else {
        return;
    };
    let property_names = json!({"type": "string", "pattern": pattern});
    let values = values.clone();
    object.remove("patternProperties");
    object.insert("additionalProperties".to_owned(), values);
    object.insert("propertyNames".to_owned(), property_names);
}

// OpenAPI clients commonly interpret `allOf` as object inheritance. A
// Schemars `$ref` to a scalar/enum with a field default or constraint sibling
// must therefore be projected as that scalar schema with the sibling merged
// in, rather than as an `allOf` between non-object schemas.
fn inline_non_object_ref_siblings(
    reference_schema: &serde_json::Map<String, Value>,
    reference: &str,
    definitions: &serde_json::Map<String, Value>,
) -> Option<Value> {
    let mut definition = definitions.get(reference)?.clone();
    normalize_scalar_literal_unions(&mut definition);
    let definition = definition.as_object()?;
    if !is_non_object_schema(definition) {
        return None;
    }
    let mut inlined = definition.clone();
    for (key, sibling) in reference_schema {
        if key == "$ref" {
            continue;
        }
        if let Some(existing) = inlined.get(key).cloned() {
            if &existing == sibling || is_annotation_keyword(key) {
                inlined.insert(key.clone(), sibling.clone());
            } else if key == "enum" {
                let existing_values = existing.as_array()?;
                let sibling_values = sibling.as_array()?;
                inlined.insert(
                    key.clone(),
                    Value::Array(
                        existing_values
                            .iter()
                            .filter(|value| sibling_values.contains(value))
                            .cloned()
                            .collect(),
                    ),
                );
            } else {
                // Keep the reference composition intact when flattening would
                // otherwise lose or weaken a conflicting schema constraint.
                return None;
            }
        } else {
            inlined.insert(key.clone(), sibling.clone());
        }
    }
    Some(Value::Object(inlined))
}

fn is_non_object_schema(schema: &serde_json::Map<String, Value>) -> bool {
    match schema.get("type") {
        Some(Value::String(kind)) => kind != "object",
        Some(Value::Array(kinds)) => {
            !kinds.is_empty()
                && kinds
                    .iter()
                    .all(|kind| kind.as_str().is_some_and(|kind| kind != "object"))
        }
        _ => schema
            .get("enum")
            .and_then(Value::as_array)
            .is_some_and(|values| {
                values
                    .iter()
                    .all(|value| !value.is_object() && !value.is_array())
            }),
    }
}

fn is_annotation_keyword(key: &str) -> bool {
    matches!(
        key,
        "title"
            | "description"
            | "default"
            | "deprecated"
            | "readOnly"
            | "writeOnly"
            | "example"
            | "examples"
            | "externalDocs"
    ) || key.starts_with("x-")
}

fn normalize_scalar_literal_unions(value: &mut Value) {
    match value {
        Value::Array(values) => {
            for value in values {
                normalize_scalar_literal_unions(value);
            }
        }
        Value::Object(object) => {
            for value in object.values_mut() {
                normalize_scalar_literal_unions(value);
            }
            normalize_scalar_literal_union(object);
        }
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => {}
    }
}

// A oneOf of distinct, unconstrained string literals has the same accepted
// values as a string enum. Projecting it this way keeps scalar ref siblings
// (for example, a default) out of non-object allOf schemas that SDK generators
// commonly interpret as model inheritance. Keep constrained branches and
// duplicate literals as oneOf: flattening either case could change its meaning.
fn normalize_scalar_literal_union(object: &mut serde_json::Map<String, Value>) {
    if object
        .get("type")
        .is_some_and(|kind| kind.as_str() != Some("string"))
        || object.contains_key("enum")
        || object.contains_key("const")
        || object.contains_key("x-enumDescriptions")
    {
        return;
    }
    let Some(variants) = object.get("oneOf").and_then(Value::as_array) else {
        return;
    };
    if variants.is_empty() {
        return;
    }

    let mut values = Vec::with_capacity(variants.len());
    let mut descriptions = Vec::with_capacity(variants.len());
    for variant in variants {
        let Some(variant) = variant.as_object() else {
            return;
        };
        if variant
            .keys()
            .any(|key| !matches!(key.as_str(), "type" | "const" | "enum" | "description"))
            || variant.get("type").and_then(Value::as_str) != Some("string")
        {
            return;
        }
        let Some(literal) = (match (variant.get("const"), variant.get("enum")) {
            (Some(Value::String(literal)), None) => Some(Value::String(literal.clone())),
            (None, Some(Value::Array(literals))) if literals.len() == 1 => literals
                .first()
                .filter(|literal| literal.is_string())
                .cloned(),
            _ => None,
        }) else {
            return;
        };
        if values.contains(&literal) {
            return;
        }
        values.push(literal);
        let description = match variant.get("description") {
            Some(Value::String(description)) => Value::String(description.clone()),
            Some(_) => return,
            None => Value::Null,
        };
        descriptions.push(description);
    }

    object.remove("oneOf");
    object.insert("type".to_owned(), Value::String("string".to_owned()));
    object.insert("enum".to_owned(), Value::Array(values));
    if descriptions
        .iter()
        .any(|description| !description.is_null())
    {
        object.insert("x-enumDescriptions".to_owned(), Value::Array(descriptions));
    }
}

#[cfg(test)]
#[allow(
    clippy::expect_used,
    reason = "unit tests assert schema conversion invariants"
)]
mod tests {
    use super::{
        adapt_schemars_schema, hoist_inline_objects, parse_openapi_schema,
        project_schemars_components,
    };
    use serde_json::{Value, json};

    #[test]
    fn projection_preserves_owned_row_derivation_map_values_and_key_pattern() {
        let source = serde_json::to_value(norn_ir::artifact_payload_schema(
            &norn_ir::ArtifactKind::Action,
        ))
        .expect("action schema");
        let mut source = source
            .pointer("/$defs/OwnedCollectionAction")
            .expect("owned collection action definition")
            .clone();
        adapt_schemars_schema(&mut source, "NornIr", &serde_json::Map::default());
        let projected = serde_json::to_value(parse_openapi_schema(source)).expect("schema json");
        assert_eq!(
            projected.pointer("/properties/derive/additionalProperties/$ref"),
            Some(&json!("#/components/schemas/NornIrZenExpression"))
        );
        assert_eq!(
            projected.pointer("/properties/derive/propertyNames/pattern"),
            Some(&json!("^[a-z][a-z0-9_]*$"))
        );
        assert_eq!(projected.get("additionalProperties"), Some(&json!(false)));
    }

    #[test]
    fn nullable_object_hoists_keep_annotations_on_the_nullable_wrapper() {
        let mut source = json!({
            "type": "object",
            "properties": {
                "config": {
                    "type": ["object", "null"],
                    "additionalProperties": {"type": "string"},
                    "description": "optional config",
                    "default": null
                }
            }
        });
        let mut components = Vec::new();
        hoist_inline_objects(&mut source, "Example", false, &mut components);
        assert_eq!(
            source.pointer("/properties/config/description"),
            Some(&json!("optional config"))
        );
        assert_eq!(
            source.pointer("/properties/config/default"),
            Some(&Value::Null)
        );
        assert_eq!(
            source.pointer("/properties/config/anyOf/1/type"),
            Some(&json!("null"))
        );
        let (_, component) = components
            .iter()
            .find(|(name, _)| name == "ExamplePropertyConfig")
            .expect("object component");
        assert_eq!(component.get("type"), Some(&json!("object")));
        assert_eq!(
            component.pointer("/additionalProperties/type"),
            Some(&json!("string"))
        );
        assert!(component.get("default").is_none());
    }

    #[test]
    fn projection_keeps_all_schema_compositions_and_rewrites_nested_refs() {
        let mut source = json!({
            "oneOf": [
                { "allOf": [{ "$ref": "#/$defs/Common" }, { "anyOf": [{ "const": "rule" }, { "const": "entity" }] }] },
                { "$ref": "#/$defs/Other" }
            ]
        });
        adapt_schemars_schema(&mut source, "NornIr", &serde_json::Map::default());
        let projected = serde_json::to_value(parse_openapi_schema(source)).expect("schema json");
        assert!(projected.get("oneOf").is_some());
        assert!(projected.pointer("/oneOf/0/allOf/1/anyOf").is_some());
        assert_eq!(
            projected.pointer("/oneOf/0/allOf/0/$ref"),
            Some(&Value::String(
                "#/components/schemas/NornIrCommon".to_owned()
            ))
        );
        assert_eq!(
            projected.pointer("/oneOf/0/allOf/1/anyOf/0/enum/0"),
            Some(&Value::String("rule".to_owned()))
        );
    }

    #[test]
    fn projection_keeps_composition_siblings_and_nested_property_unions() {
        let mut source = json!({
            "type": "object",
            "properties": {
                "state": {
                    "anyOf": [
                        { "type": "string", "const": "ready" },
                        { "type": "null" }
                    ]
                },
                "nullableVersion": { "type": ["string", "null"], "minLength": 1 },
                "children": {
                    "type": "array",
                    "items": { "allOf": [{ "$ref": "#/$defs/Node" }, { "type": "object" }] }
                },
                "empty": { "type": "array", "maxItems": 0 },
                "blocked": false
            },
            "required": ["state"],
            "additionalProperties": false,
            "oneOf": [
                { "properties": { "kind": { "const": "a" } } },
                { "properties": { "kind": { "const": "b" } } }
            ]
        });
        adapt_schemars_schema(&mut source, "NornIr", &serde_json::Map::default());
        let projected = serde_json::to_value(parse_openapi_schema(source)).expect("schema json");

        assert_eq!(
            projected.pointer("/allOf/0/required/0"),
            Some(&Value::String("state".to_owned()))
        );
        assert_eq!(
            projected.pointer("/allOf/0/additionalProperties"),
            Some(&Value::Bool(false))
        );
        assert!(projected.pointer("/allOf/1/oneOf").is_some());
        assert!(
            projected
                .pointer("/allOf/0/properties/state/anyOf")
                .is_some()
        );
        assert_eq!(
            projected.pointer("/allOf/0/properties/nullableVersion/type/0"),
            Some(&Value::String("string".to_owned()))
        );
        assert_eq!(
            projected.pointer("/allOf/0/properties/nullableVersion/type/1"),
            Some(&Value::String("null".to_owned()))
        );
        assert_eq!(
            projected.pointer("/allOf/0/properties/nullableVersion/minLength"),
            Some(&Value::from(1))
        );
        assert_eq!(
            projected.pointer("/allOf/0/properties/children/items/allOf/0/$ref"),
            Some(&Value::String("#/components/schemas/NornIrNode".to_owned()))
        );
        assert_eq!(
            projected.pointer("/allOf/0/properties/blocked/enum"),
            Some(&Value::Array(Vec::new()))
        );
        assert_eq!(
            projected.pointer("/allOf/0/properties/empty/maxItems"),
            Some(&Value::from(0))
        );
        assert!(
            projected
                .pointer("/allOf/0/properties/empty/items")
                .is_some()
        );
    }

    #[test]
    fn projection_keeps_the_canonical_non_null_default_constraint() {
        let mut source = json!({
            "$ref": "#/$defs/JsonValue",
            "description": "non-null JSON value",
            "not": { "type": "null" }
        });
        adapt_schemars_schema(&mut source, "NornIr", &serde_json::Map::default());
        let projected = serde_json::to_value(parse_openapi_schema(source)).expect("schema json");

        assert_eq!(
            projected.pointer("/allOf/0/$ref"),
            Some(&Value::String(
                "#/components/schemas/NornIrJsonValue".to_owned()
            ))
        );
        assert_eq!(
            projected.pointer("/allOf/0/description"),
            Some(&Value::String("non-null JSON value".to_owned()))
        );
        let non_null_types = projected
            .pointer("/allOf/1/anyOf")
            .and_then(Value::as_array)
            .expect("non-null alternatives");
        assert_eq!(non_null_types.len(), 5);
        assert!(
            !non_null_types
                .iter()
                .any(|schema| schema.get("type").and_then(Value::as_str) == Some("null"))
        );
    }

    #[test]
    fn projection_keeps_scalar_union_enum_contracts() {
        let mut source = json!({
            "description": "status selector",
            "default": "active",
            "minLength": 1,
            "oneOf": [
                { "description": "currently active", "type": "string", "const": "active" },
                { "description": "inactive", "type": "string", "enum": ["inactive"] }
            ]
        });
        adapt_schemars_schema(&mut source, "NornIr", &serde_json::Map::default());
        let projected = serde_json::to_value(parse_openapi_schema(source)).expect("schema json");
        assert_eq!(
            projected.pointer("/type"),
            Some(&Value::String("string".to_owned()))
        );
        assert_eq!(
            projected.pointer("/enum"),
            Some(&json!(["active", "inactive"]))
        );
        assert_eq!(
            projected.get("description"),
            Some(&json!("status selector"))
        );
        assert_eq!(projected.get("default"), Some(&json!("active")));
        assert_eq!(projected.get("minLength"), Some(&json!(1)));
        assert_eq!(
            projected.get("x-enumDescriptions"),
            Some(&json!(["currently active", "inactive"]))
        );
    }

    #[test]
    fn projection_keeps_one_of_when_literal_branches_overlap_or_are_constrained() {
        let mut duplicate = json!({
            "oneOf": [
                { "type": "string", "const": "active" },
                { "type": "string", "enum": ["active"] }
            ]
        });
        adapt_schemars_schema(&mut duplicate, "NornIr", &serde_json::Map::default());
        assert_eq!(
            duplicate
                .pointer("/oneOf")
                .and_then(Value::as_array)
                .map(Vec::len),
            Some(2),
            "duplicate branches make oneOf stricter than enum"
        );
        assert!(duplicate.get("enum").is_none());

        let mut constrained = json!({
            "oneOf": [
                { "type": "string", "const": "active", "minLength": 4 },
                { "type": "string", "const": "inactive" }
            ]
        });
        adapt_schemars_schema(&mut constrained, "NornIr", &serde_json::Map::default());
        assert_eq!(
            constrained
                .pointer("/oneOf")
                .and_then(Value::as_array)
                .map(Vec::len),
            Some(2),
            "branch validation constraints are retained"
        );
    }

    #[test]
    fn mapping_entitlement_join_cardinality_stays_a_scalar_enum_with_default() {
        let schemas = project_schemars_components(
            norn_ir::mapping::mapping_ir_schema(),
            "NornIrMappingIr",
            "NornIr",
        );
        let join = schemas
            .iter()
            .find(|(name, _)| name == "NornIrEntitlementJoin")
            .map(|(_, schema)| schema)
            .expect("canonical MappingIR exposes EntitlementJoin");
        let projected = serde_json::to_value(join).expect("projects join schema");
        let cardinality = projected
            .pointer("/properties/cardinality")
            .expect("join cardinality is part of the schema");

        assert_eq!(cardinality.get("type"), Some(&json!("string")));
        assert_eq!(cardinality.get("enum"), Some(&json!(["many", "atMostOne"])));
        assert_eq!(cardinality.get("default"), Some(&json!("many")));
        assert!(
            cardinality.get("allOf").is_none(),
            "primitive ref metadata must not become a non-object allOf"
        );
    }

    #[test]
    fn primitive_ref_projection_keeps_nonconflicting_constraints_and_enum() {
        let definitions = serde_json::Map::from_iter([(
            "State".to_owned(),
            json!({ "type": "string", "enum": ["active", "inactive"] }),
        )]);
        let mut source = json!({
            "$ref": "#/$defs/State",
            "default": "active",
            "minLength": 1
        });
        adapt_schemars_schema(&mut source, "NornIr", &definitions);
        let projected = serde_json::to_value(parse_openapi_schema(source)).expect("schema json");

        assert_eq!(projected.get("type"), Some(&json!("string")));
        assert_eq!(projected.get("enum"), Some(&json!(["active", "inactive"])));
        assert_eq!(projected.get("default"), Some(&json!("active")));
        assert_eq!(projected.get("minLength"), Some(&json!(1)));
        assert!(projected.get("allOf").is_none());
    }
}
