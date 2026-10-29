//! The checker ABI (T1-CHK-4, #2527): the one JSON-in/JSON-out surface the
//! TypeScript extractor, jdm-editor, the playground, and SDKs call instead of
//! linking checker internals.
//!
//! Every function takes JSON text and returns JSON text, so no `serde`,
//! `norn-ir`, or `zen-*` type crosses the boundary. The functions here are
//! plain Rust; on `wasm32` the `wasm` module exports each one to
//! JavaScript under its camel-case name (`checkArtifactSet`,
//! `lifecycleDiagrams`, `checkExpression`, `version`). A native C ABI over the
//! same functions is follow-up work (see the crate README).
//!
//! Output is byte-identical natively and on `wasm32`: diagnostics are
//! [`norn_checker::diagnostics_to_json`] (`norn-checker/diagnostics@1`), the
//! receipt is the checker's Norn1 canonical receipt, and the whole document
//! is compact `serde_json` text (sorted keys). The CI job `checker-wasm`
//! asserts the equality on the fixture cases in `tests/parity/cases.json`.
//!
//! Invalid *input* (text that is not JSON, a wrong shape, an unknown expected
//! type) is an [`AbiError`] (a thrown `Error` in JavaScript). Invalid
//! *artifacts* are findings in the output, never an error.

use std::collections::BTreeSet;
use std::fmt;

use norn_checker::function::{
    admit_function_manifest, check_function_invocation, check_function_receipt,
    decode_function_invocation, decode_function_manifest, decode_function_receipt,
};
use norn_checker::receipt::{CHECKER_VERSION, EXPR_PROFILE, RECEIPT_FORMAT};
use norn_checker::{
    DIAGNOSTICS_FORMAT, Diagnostic, EntityArtifactSetContext, TenantLayer, check_artifact_set,
    check_artifact_set_with_flow_source_map, declared_lifecycles, decode_artifact_json,
    derive_type_env, diagnostics_to_json,
};
use norn_expr::{
    EvaluationBudget, ExprError, Expression, Type, Value as ExprValue, ZenTypeEnv,
    validate_evaluation_source,
};
use norn_ir::canonical::CanonicalJsonError;
use norn_ir::function::Digest;
use norn_ir::{
    ArtifactEnvelope, DiagnosticSeverity, ExtensionNamespace, FlowSourceMap, ObjectTypeName,
    PropertyTypeName, ResourceLimitProfile,
};
use serde::Deserialize;
use serde_json::{Value, json};

#[cfg(target_arch = "wasm32")]
pub mod wasm;

/// The ABI version [`version_json`] reports. A change to a function's
/// arguments or to an output document's fields is a new version.
pub const ABI_VERSION: &str = "norn-checker-abi/1";

/// Format tag of the [`check_artifact_set_json`] document.
pub const CHECK_FORMAT: &str = "norn-checker-abi/check@1";

/// Format tag of the [`canonical_runtime_value_json`] document.
pub const RUNTIME_VALUE_FORMAT: &str = "norn-checker-abi/runtime-value@1";

/// Format tag of the [`check_expression_json`] document.
pub const EXPRESSION_FORMAT: &str = "norn-checker-abi/expression@1";

/// Format tag of the [`evaluate_expression_json`] document.
pub const EVALUATION_FORMAT: &str = "norn-checker-abi/evaluation@1";

/// Format tag of the [`function_contract_json`] document.
pub const FUNCTION_FORMAT: &str = "norn-checker-abi/function@1";

/// How deeply `list<…>` and `?` may nest in an expected type.
const MAX_TYPE_DEPTH: u32 = 16;

/// Invalid input to an ABI function.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum AbiError {
    /// An argument is not JSON of the expected shape.
    InvalidInput {
        /// The argument's name.
        argument: &'static str,
        /// What is wrong with it.
        detail: String,
    },
    /// The checker could not issue a receipt (a value outside the Norn1
    /// canonical JSON profile, for example nested too deeply).
    Receipt(String),
}

impl fmt::Display for AbiError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidInput { argument, detail } => write!(f, "invalid {argument}: {detail}"),
            Self::Receipt(detail) => write!(f, "no receipt could be issued: {detail}"),
        }
    }
}

impl std::error::Error for AbiError {}

fn invalid(argument: &'static str, detail: impl Into<String>) -> AbiError {
    AbiError::InvalidInput {
        argument,
        detail: detail.into(),
    }
}

/// The `context` argument of [`check_artifact_set_json`].
#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ContextInput {
    /// The installed, read-only base layer (artifact envelopes).
    #[serde(default)]
    base: Vec<Value>,
    /// Registry (Norn Core) value type names a `ref` may name.
    #[serde(default)]
    registry: BTreeSet<PropertyTypeName>,
    /// Present when the artifacts are a tenant layer.
    #[serde(default)]
    tenant: Option<TenantInput>,
    /// The resource-limit profile; `norn.limits/1` when absent.
    #[serde(default)]
    limits: Option<ResourceLimitProfile>,
    /// Optional non-executable flow projection bound into the checker receipt.
    #[serde(default)]
    flow_source_map: Option<FlowSourceMap>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct TenantInput {
    tenant_id: String,
    namespace: ExtensionNamespace,
}

fn parse_json(argument: &'static str, text: &str) -> Result<Value, AbiError> {
    serde_json::from_str(text).map_err(|error| invalid(argument, error.to_string()))
}

fn parse_context(text: &str) -> Result<ContextInput, AbiError> {
    match parse_json("context", text)? {
        Value::Null => Ok(ContextInput::default()),
        value @ (Value::Bool(_)
        | Value::Number(_)
        | Value::String(_)
        | Value::Array(_)
        | Value::Object(_)) => {
            serde_json::from_value(value).map_err(|error| invalid("context", error.to_string()))
        }
    }
}

/// The envelopes of a JSON array, or the findings of every element that is
/// not a decodable envelope.
fn decode_envelopes(values: &[Value]) -> Result<Vec<ArtifactEnvelope>, Vec<Diagnostic>> {
    let mut envelopes = Vec::with_capacity(values.len());
    let mut findings = Vec::new();
    for value in values {
        match decode_artifact_json(value) {
            Ok(envelope) => envelopes.push(envelope),
            Err(legacy) => findings.extend(
                legacy
                    .into_iter()
                    .map(|diagnostic| Diagnostic::from_legacy_in(diagnostic, value)),
            ),
        }
    }
    if findings.is_empty() {
        Ok(envelopes)
    } else {
        Err(findings)
    }
}

fn parse_array(argument: &'static str, text: &str) -> Result<Vec<Value>, AbiError> {
    match parse_json(argument, text)? {
        Value::Array(values) => Ok(values),
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) | Value::Object(_) => {
            Err(invalid(
                argument,
                "expected a JSON array of artifact envelopes",
            ))
        }
    }
}

/// `argument` as envelopes; an element that does not decode is invalid
/// input.
fn envelopes_or_invalid(
    argument: &'static str,
    values: &[Value],
) -> Result<Vec<ArtifactEnvelope>, AbiError> {
    decode_envelopes(values).map_err(|findings| {
        invalid(
            argument,
            format!(
                "an element is not a decodable artifact envelope: {}",
                diagnostics_to_json(&findings)
            ),
        )
    })
}

fn has_errors(diagnostics: &[Diagnostic]) -> bool {
    diagnostics
        .iter()
        .any(|diagnostic| diagnostic.severity == DiagnosticSeverity::Error)
}

/// Check a candidate artifact set and issue its receipt
/// (`norn_checker::check_artifact_set_with_flow_source_map`).
///
/// `artifacts_json` is a JSON array of artifact envelopes. `context_json` is
/// `null` or an object with the optional members `base` (the installed base
/// layer: an array of envelopes), `registry` (registry value type names a
/// `ref` may name), `tenant` (`{"tenantId", "namespace"}` when the artifacts
/// are a tenant layer), `limits` (a resource-limit profile; default
/// `norn.limits/1`), and `flowSourceMap` (an optional checked, non-executable
/// authoring projection).
///
/// Returns `{"format", "admitted", "diagnostics", "receipt",
/// "receiptSha256"}`: `diagnostics` is the `norn-checker/diagnostics@1`
/// document, `receipt` the `norn-checker/receipt@2` document when the map is
/// absent and `@3` when present. When an artifact is not a decodable envelope,
/// `diagnostics` holds the decode findings and `receipt`/`receiptSha256` are
/// `null` (fail closed).
///
/// # Errors
///
/// [`AbiError`] when an argument is not JSON of that shape, a base envelope
/// does not decode, or the receipt cannot be issued.
pub fn check_artifact_set_json(
    artifacts_json: &str,
    context_json: &str,
) -> Result<String, AbiError> {
    let artifacts = parse_array("artifacts", artifacts_json)?;
    let context = parse_context(context_json)?;
    let base = envelopes_or_invalid("context.base", &context.base)?;
    let limits = context
        .limits
        .unwrap_or_else(ResourceLimitProfile::norn_limits_v1);
    let tenant = context.tenant.as_ref().map(|tenant| TenantLayer {
        tenant_id: &tenant.tenant_id,
        namespace: &tenant.namespace,
    });
    let set_context = EntityArtifactSetContext {
        registry: &context.registry,
        base: &base,
        tenant,
    };
    let output = match decode_envelopes(&artifacts) {
        Err(findings) => json!({
            "format": CHECK_FORMAT,
            "admitted": false,
            "diagnostics": diagnostics_to_json(&findings),
            "receipt": null,
            "receiptSha256": null,
        }),
        Ok(envelopes) => {
            let receipt_error = |error: &dyn fmt::Display| AbiError::Receipt(error.to_string());
            let checked = check_artifact_set_with_flow_source_map(
                &envelopes,
                &set_context,
                &limits,
                context.flow_source_map.as_ref(),
            )
            .map_err(|error| receipt_error(&error))?;
            let bytes = checked
                .receipt
                .canonical_json()
                .map_err(|error| receipt_error(&error))?;
            let receipt: Value =
                serde_json::from_slice(&bytes).map_err(|error| receipt_error(&error))?;
            let sha256 = checked
                .receipt
                .content_sha256()
                .map_err(|error| receipt_error(&error))?;
            json!({
                "format": CHECK_FORMAT,
                "admitted": checked.receipt.admitted,
                "diagnostics": diagnostics_to_json(&checked.diagnostics),
                "receipt": receipt,
                "receiptSha256": sha256,
            })
        }
    };
    Ok(output.to_string())
}

/// Render every lifecycle the `EntityIR` v2 object types in `artifacts_json`
/// declare as a Mermaid `stateDiagram-v2`
/// (`norn_projections::lifecycle_state_diagram`).
///
/// Returns a JSON array of `{"type", "artifactId", "property", "mermaid"}` in
/// artifact then declaration order. Artifacts that are not `EntityIR` v2
/// object types are skipped; this is a projection, so check the set first
/// ([`check_artifact_set_json`]) for findings.
///
/// # Errors
///
/// [`AbiError`] when `artifacts_json` is not a JSON array of decodable
/// artifact envelopes.
pub fn lifecycle_diagrams_json(artifacts_json: &str) -> Result<String, AbiError> {
    let artifacts = parse_array("artifacts", artifacts_json)?;
    let envelopes = envelopes_or_invalid("artifacts", &artifacts)?;
    let diagrams: Vec<Value> = declared_lifecycles(&envelopes)
        .iter()
        .map(|declared| {
            let states: Vec<&str> = declared.states.iter().map(String::as_str).collect();
            json!({
                "type": declared.object_type.as_str(),
                "artifactId": declared.artifact_id,
                "property": declared.lifecycle.property.as_str(),
                "mermaid": norn_projections::lifecycle_state_diagram(&states, &declared.lifecycle),
            })
        })
        .collect();
    Ok(Value::Array(diagrams).to_string())
}

/// One expression finding as JSON; `span` is `null` for a finding about the
/// environment rather than the source.
fn expression_finding(code: &str, message: &str, span: Option<(u32, u32)>) -> Value {
    json!({
        "code": code,
        "message": message,
        "span": span.map(|(start, end)| json!({ "start": start, "end": end })),
    })
}

/// Type-check the ZEN Expression `expr` under `norn-expr/4`
/// (`norn_expr::check_expression`) in the type environment of the `EntityIR`
/// v2 artifact set `env_artifacts_json`, against `expected_type`, with
/// `self` bound to the object type `self_type` when given.
///
/// `expected_type` is `bool`, `text`, `integer`, `number`, `date`,
/// `duration`, `json`, an object type name (a record, e.g. `retail.Sku`),
/// `list<T>`, or `T?` (nullable).
///
/// Returns `{"format", "profile", "ok", "inferred", "diagnostics",
/// "environment"}`: `inferred` is the proved result type when `ok`;
/// `diagnostics` are `{"code", "message", "span": {"start", "end"} | null}`
/// with `expr.*` codes; `environment` is the `norn-checker/diagnostics@1`
/// document of checking the environment set itself. A set with error
/// findings has no environment, so the expression is not checked.
///
/// # Errors
///
/// [`AbiError`] when `env_artifacts_json` is not a JSON array or
/// `expected_type` is outside the grammar above.
pub fn check_expression_json(
    expr: &str,
    env_artifacts_json: &str,
    expected_type: &str,
    self_type: Option<&str>,
) -> Result<String, AbiError> {
    let expected = parse_type(expected_type, 0)?;
    let artifacts = parse_array("envArtifacts", env_artifacts_json)?;
    let (environment, outcome) = match decode_envelopes(&artifacts) {
        Err(findings) => (findings, None),
        Ok(envelopes) => {
            let registry = BTreeSet::new();
            let findings = check_artifact_set(
                &envelopes,
                &EntityArtifactSetContext {
                    registry: &registry,
                    base: &[],
                    tenant: None,
                },
            );
            let outcome = (!has_errors(&findings))
                .then(|| check_in_set(expr, &envelopes, &expected, self_type));
            (findings, outcome)
        }
    };
    let (ok, inferred, diagnostics) = match outcome {
        None => (false, None, Vec::new()),
        Some(Ok(inferred)) => (true, Some(inferred), Vec::new()),
        Some(Err(diagnostics)) => (false, None, diagnostics),
    };
    Ok(json!({
        "format": EXPRESSION_FORMAT,
        "profile": EXPR_PROFILE,
        "ok": ok,
        "inferred": inferred,
        "diagnostics": diagnostics,
        "environment": diagnostics_to_json(&environment),
    })
    .to_string())
}

/// Evaluate an already-admitted `WidgetIR` ZEN expression against its explicit
/// `record`, `recordKey`, `row`, `draft`, `selection`, and `context` runtime inputs.
///
/// `input_json` is an object whose optional members are exactly those six
/// slots. `recordKey` must be a nonempty string paired with `record`; `record`, `row`, `draft`, and `context` must be records; `selection`
/// must be a bounded list of records. Omitted slots remain omitted. Numbers are parsed
/// from raw JSON token text into exact runtime decimals, and the result embeds
/// exact JSON number tokens. The optional `profile` names the expression
/// profile; it defaults to and currently accepts only `norn-zen/6`. Evaluation
/// always uses the shipped `norn.limits/1` decide fuel.
///
/// The source and input contract must already have passed `WidgetIR` admission
/// (`checkArtifactSet`), and runtime inputs must conform to their declared types.
/// This function reuses that
/// profile's source bounds and exact-number token checks, but it does not
/// repeat environment-dependent type or grammar admission. Runtime failures
/// and invalid input are returned as `{"format","ok":false,"profile",
/// "unitsUsed","error"}` with a stable typed error code. This function does
/// not throw for expected refusal modes.
#[must_use]
pub fn evaluate_expression_json(expr: &str, input_json: &str, profile: Option<&str>) -> String {
    let profile = profile.unwrap_or(norn_expr::check::CHECKER_PROFILE);
    if profile != norn_expr::check::CHECKER_PROFILE {
        return evaluation_abi_error_json(
            profile,
            invalid(
                "profile",
                format!("unsupported expression profile {profile:?}"),
            ),
        );
    }

    let limits = ResourceLimitProfile::norn_limits_v1();
    let context = match parse_expression_inputs(input_json, &limits) {
        Ok(context) => context,
        Err(error) => return evaluation_abi_error_json(profile, error),
    };
    if let Err(diagnostics) = validate_evaluation_source(expr, &limits) {
        let first_code = diagnostics
            .first()
            .map_or(norn_expr::check::codes::SYNTAX, |diagnostic| {
                diagnostic.code
            });
        let diagnostics: Vec<Value> = diagnostics
            .into_iter()
            .map(|diagnostic| {
                json!({
                    "code": diagnostic.code,
                    "message": diagnostic.message,
                    "span": {
                        "start": diagnostic.span.0,
                        "end": diagnostic.span.1,
                    },
                })
            })
            .collect();
        return json!({
            "format": EVALUATION_FORMAT,
            "ok": false,
            "profile": profile,
            "unitsUsed": 0,
            "error": {
                "code": first_code,
                "diagnostics": diagnostics,
            },
        })
        .to_string();
    }

    let expression = match Expression::compile(expr) {
        Ok(expression) => expression,
        Err(error) => return evaluation_error_json(profile, error, 0),
    };
    let attempt = expression.evaluate_metered(&context, EvaluationBudget::decide(&limits));
    let evaluation = match attempt.result {
        Ok(evaluation) => evaluation,
        Err(error) => return evaluation_error_json(profile, error, attempt.units_used),
    };
    let result = match expr_value_to_runtime(evaluation.into_value()) {
        Ok(value) => value,
        Err(error) => return evaluation_error_json(profile, error, attempt.units_used),
    };
    let result_json = match result.canonical_json() {
        Ok(result_json) => result_json,
        Err(error) => {
            return evaluation_error_json(
                profile,
                ExprError::Evaluation {
                    message: error.to_string(),
                },
                attempt.units_used,
            );
        }
    };
    let result_json = match String::from_utf8(result_json) {
        Ok(result_json) => result_json,
        Err(error) => {
            return evaluation_error_json(
                profile,
                ExprError::Evaluation {
                    message: error.to_string(),
                },
                attempt.units_used,
            );
        }
    };
    format!(
        "{{\"format\":\"{EVALUATION_FORMAT}\",\"ok\":true,\"profile\":\"{profile}\",\"unitsUsed\":{},\"value\":{result_json}}}",
        attempt.units_used
    )
}

/// Decode the `WidgetIR` runtime input envelope without passing JSON numbers
/// through `serde_json::Value`'s binary-number representation.
fn parse_expression_inputs(
    text: &str,
    limits: &ResourceLimitProfile,
) -> Result<ExprValue, AbiError> {
    let max_bytes = usize::try_from(limits.slice.max_canonical_bytes).unwrap_or(usize::MAX);
    if text.len() > max_bytes {
        return Err(invalid(
            "inputJson",
            format!("input is {} bytes, more than {max_bytes}", text.len()),
        ));
    }
    let slots = norn_ir::RuntimeValue::from_json_text(text)
        .map_err(|error| invalid("inputJson", error.to_string()))?;
    let norn_ir::RuntimeValue::Object(slots) = slots else {
        return Err(invalid(
            "inputJson",
            "expected an object of WidgetIR input slots",
        ));
    };

    if slots.contains_key("recordKey") && !slots.contains_key("record") {
        return Err(invalid("inputJson", "recordKey requires record"));
    }
    let max_records = usize::try_from(limits.slice.max_records).unwrap_or(usize::MAX);
    let mut inputs = std::collections::BTreeMap::new();
    for (name, value) in slots {
        match name.as_str() {
            "recordKey" => {
                if !matches!(&value, norn_ir::RuntimeValue::String(key) if !key.is_empty()) {
                    return Err(invalid("inputJson", "recordKey must be a nonempty string"));
                }
            }
            "record" | "row" | "draft" | "context" => {
                if !matches!(&value, norn_ir::RuntimeValue::Object(_)) {
                    return Err(invalid(
                        "inputJson",
                        format!("{name} must be a record when present"),
                    ));
                }
            }
            "selection" => match &value {
                norn_ir::RuntimeValue::Array(records)
                    if records.len() <= max_records
                        && records
                            .iter()
                            .all(|record| matches!(record, norn_ir::RuntimeValue::Object(_))) => {}
                norn_ir::RuntimeValue::Array(records) if records.len() > max_records => {
                    return Err(invalid(
                        "inputJson",
                        format!(
                            "selection has {} records, more than {max_records}",
                            records.len()
                        ),
                    ));
                }
                norn_ir::RuntimeValue::Array(_) => {
                    return Err(invalid("inputJson", "selection must contain only records"));
                }
                norn_ir::RuntimeValue::Null
                | norn_ir::RuntimeValue::Bool(_)
                | norn_ir::RuntimeValue::Number(_)
                | norn_ir::RuntimeValue::String(_)
                | norn_ir::RuntimeValue::Object(_) => {
                    return Err(invalid(
                        "inputJson",
                        "selection must be a list of records when present",
                    ));
                }
            },
            _ => {
                return Err(invalid(
                    "inputJson",
                    format!("unknown WidgetIR input slot {name:?}"),
                ));
            }
        }
        let value =
            ExprValue::try_from(&value).map_err(|error| invalid("inputJson", error.to_string()))?;
        inputs.insert(name, value);
    }
    Ok(ExprValue::object([("input", ExprValue::Object(inputs))]))
}

/// Project the evaluator's Norn value to the exact runtime JSON type.
fn expr_value_to_runtime(value: ExprValue) -> Result<norn_ir::RuntimeValue, ExprError> {
    Ok(match value {
        ExprValue::Null => norn_ir::RuntimeValue::Null,
        ExprValue::Bool(value) => norn_ir::RuntimeValue::Bool(value),
        ExprValue::Number(number) => {
            let input = number.as_str().to_owned();
            let decimal =
                norn_ir::Decimal::parse(&input).map_err(|error| ExprError::InvalidNumber {
                    input,
                    reason: error.to_string(),
                })?;
            norn_ir::RuntimeValue::Number(decimal)
        }
        ExprValue::String(value) => norn_ir::RuntimeValue::String(value),
        ExprValue::Array(values) => norn_ir::RuntimeValue::Array(
            values
                .into_iter()
                .map(expr_value_to_runtime)
                .collect::<Result<_, _>>()?,
        ),
        ExprValue::Object(values) => norn_ir::RuntimeValue::Object(
            values
                .into_iter()
                .map(|(key, value)| Ok((key, expr_value_to_runtime(value)?)))
                .collect::<Result<_, ExprError>>()?,
        ),
    })
}

/// Serialize an evaluator failure as a stable, discriminated ABI result.
fn evaluation_abi_error_json(profile: &str, error: AbiError) -> String {
    let (code, argument, message) = match error {
        AbiError::InvalidInput { argument, detail } => (
            if argument == "profile" {
                "expr.profile.invalid"
            } else {
                "expr.input.invalid"
            },
            argument,
            detail,
        ),
        AbiError::Receipt(detail) => ("expr.output.invalid", "result", detail),
    };
    json!({
        "format": EVALUATION_FORMAT,
        "ok": false,
        "profile": profile,
        "unitsUsed": 0,
        "error": {
            "code": code,
            "argument": argument,
            "message": message,
        },
    })
    .to_string()
}

/// Serialize an evaluator failure as a stable, discriminated ABI result.
fn evaluation_error_json(profile: &str, error: ExprError, units_used: u64) -> String {
    let error = match error {
        ExprError::Compile { message } => json!({
            "code": "expr.syntax.invalid",
            "message": message,
        }),
        ExprError::Evaluation { message } => json!({
            "code": "expr.evaluation.failed",
            "message": message,
        }),
        ExprError::BudgetExhausted { limit, used } => json!({
            "code": "expr.budget.exhausted",
            "limit": limit,
            "used": used,
            "message": format!(
                "expression evaluation budget exhausted: {used} units needed, limit {limit}"
            ),
        }),
        ExprError::InvalidNumber { input, reason } => json!({
            "code": "expr.number.invalid",
            "input": input,
            "reason": reason,
        }),
        ExprError::UnsupportedResult { type_name } => json!({
            "code": "expr.result.unsupported",
            "typeName": type_name,
        }),
        _ => json!({
            "code": "expr.error.unknown",
            "message": "expression evaluation failed with an unclassified error",
        }),
    };
    json!({
        "format": EVALUATION_FORMAT,
        "ok": false,
        "profile": profile,
        "unitsUsed": units_used,
        "error": error,
    })
    .to_string()
}

/// Check `expr` in the environment of a clean set.
fn check_in_set(
    expr: &str,
    envelopes: &[ArtifactEnvelope],
    expected: &Type,
    self_type: Option<&str>,
) -> Result<String, Vec<Value>> {
    let env = environment(envelopes, self_type)?;
    let expression = Expression::compile(expr).map_err(|error| {
        vec![expression_finding(
            norn_expr::check::codes::SYNTAX,
            &error.to_string(),
            Some((0, 0)),
        )]
    })?;
    norn_expr::check_expression(&expression, &env, expected)
        .map(|inferred| inferred.to_string())
        .map_err(|diagnostics| {
            diagnostics
                .iter()
                .map(|diagnostic| {
                    expression_finding(diagnostic.code, &diagnostic.message, Some(diagnostic.span))
                })
                .collect()
        })
}

/// The set's type environment under `norn.limits/1`, with `self` bound.
fn environment(
    envelopes: &[ArtifactEnvelope],
    self_type: Option<&str>,
) -> Result<ZenTypeEnv, Vec<Value>> {
    let limits = ResourceLimitProfile::norn_limits_v1();
    let env = derive_type_env(envelopes, &[], &limits).map_err(|errors| {
        errors
            .iter()
            .map(|error| {
                expression_finding(
                    norn_checker::receipt::codes::ENVIRONMENT_UNDERIVABLE,
                    &error.to_string(),
                    None,
                )
            })
            .collect::<Vec<_>>()
    })?;
    match self_type {
        None => Ok(env),
        Some(name) => env
            .bind_self(&ObjectTypeName(name.to_owned()))
            .map_err(|error| {
                vec![expression_finding(
                    norn_expr::check::codes::UNKNOWN_VARIABLE,
                    &format!("`self` cannot be bound to {name:?}: {error}"),
                    None,
                )]
            }),
    }
}

/// Parse the expected-type grammar of [`check_expression_json`].
fn parse_type(text: &str, depth: u32) -> Result<Type, AbiError> {
    if depth > MAX_TYPE_DEPTH {
        return Err(invalid(
            "expectedType",
            format!("nests deeper than {MAX_TYPE_DEPTH}"),
        ));
    }
    let text = text.trim();
    let next = depth.saturating_add(1);
    if let Some(inner) = text.strip_suffix('?') {
        return Ok(parse_type(inner, next)?.optional());
    }
    if let Some(inner) = text
        .strip_prefix("list<")
        .and_then(|rest| rest.strip_suffix('>'))
    {
        return Ok(parse_type(inner, next)?.list());
    }
    Ok(match text {
        "bool" => Type::Bool,
        "text" => Type::text(),
        "integer" => Type::Integer,
        "number" => Type::Number,
        "date" => Type::Date,
        "duration" => Type::Duration,
        "json" => Type::Json,
        name if name.contains('.') && !name.contains(char::is_whitespace) => {
            Type::Record(name.to_owned())
        }
        other => {
            return Err(invalid(
                "expectedType",
                format!(
                    "{other:?} is not bool, text, integer, number, date, duration, json, an object type name, list<T>, or T?"
                ),
            ));
        }
    })
}

/// The exact runtime value (#2770, `norn_ir::RuntimeValue`) of one JSON
/// document: its Norn1 canonical text and Norn1 hash. Numbers are exact
/// decimals (96-bit, scale <= 28) parsed from their token text, so the same
/// input gives byte-identical output natively and on `wasm32`; JavaScript
/// callers pass the raw JSON text, never a parsed `number`.
///
/// # Errors
///
/// [`AbiError::InvalidInput`] for text that is not JSON or holds a number
/// outside the exact decimal range (never rounded).
pub fn canonical_runtime_value_json(text: &str) -> Result<String, AbiError> {
    let invalid = |detail: String| AbiError::InvalidInput {
        argument: "value",
        detail,
    };
    let value =
        norn_ir::RuntimeValue::from_json_text(text).map_err(|error| invalid(error.to_string()))?;
    let canonical = value
        .canonical_json()
        .map_err(|error| invalid(error.to_string()))?;
    let sha256 = norn_ir::canonical::HashProfile::Norn1.hash_canonical_bytes(&canonical);
    let canonical = String::from_utf8(canonical).map_err(|error| invalid(error.to_string()))?;
    Ok(json!({
        "format": RUNTIME_VALUE_FORMAT,
        "canonical": canonical,
        "sha256": sha256,
    })
    .to_string())
}

/// Prefix every finding's pointer with the record it was found in.
fn in_record(record: &str, findings: Vec<Diagnostic>, out: &mut Vec<Diagnostic>) {
    out.extend(findings.into_iter().map(|mut finding| {
        if let Some(span) = &mut finding.span {
            span.pointer = format!("/{record}{}", span.pointer);
        }
        finding
    }));
}

fn digest_text(digest: Result<Digest, CanonicalJsonError>) -> Value {
    digest.map_or(Value::Null, |digest| Value::String(digest.0))
}

/// Admit a Function contract (T1-FN-1, #2776): a manifest, and optionally
/// the invocation command dispatched under it and the receipt returned for
/// that command (`norn_checker::function`), with every canonical identity.
///
/// Returns `{"format", "admitted", "manifest", "invocation", "receipt",
/// "diagnostics"}`. `manifest` is `{"digest", "executionProfile"}`,
/// `invocation` `{"digest", "inputIdentity", "dependencyToken",
/// "actionKey"}`, and `receipt` `{"digest", "resultIdentity"}`; each is
/// `null` when its record is absent or does not decode, and the invocation
/// and receipt are checked only against an admitted manifest. Finding
/// pointers are prefixed with `/manifest`, `/invocation`, or `/receipt`.
/// The identities are Norn1 domain hashes, byte-identical natively and on
/// `wasm32`.
///
/// # Errors
///
/// [`AbiError::InvalidInput`] when a receipt is supplied without the
/// invocation it is checked against. Undecodable records are findings.
pub fn function_contract_json(
    manifest_json: &str,
    invocation_json: Option<&str>,
    receipt_json: Option<&str>,
) -> Result<String, AbiError> {
    if receipt_json.is_some() && invocation_json.is_none() {
        return Err(invalid(
            "receiptJson",
            "a receipt is admitted only against the invocation it is bound to; pass invocationJson",
        ));
    }
    let mut findings = Vec::new();
    let manifest = match decode_function_manifest(manifest_json) {
        Ok(manifest) => Some(manifest),
        Err(decode) => {
            in_record("manifest", decode, &mut findings);
            None
        }
    };
    let manifest_doc = manifest.as_ref().map_or(Value::Null, |manifest| {
        json!({
            "digest": digest_text(manifest.content_digest()),
            "executionProfile": digest_text(manifest.execution_profile_digest()),
        })
    });
    let admitted = manifest
        .as_ref()
        .and_then(|manifest| match admit_function_manifest(manifest) {
            Ok(admitted) => Some(admitted),
            Err(rejected) => {
                in_record("manifest", rejected, &mut findings);
                None
            }
        });
    let invocation = invocation_json.and_then(|text| match decode_function_invocation(text) {
        Ok(invocation) => Some(invocation),
        Err(decode) => {
            in_record("invocation", decode, &mut findings);
            None
        }
    });
    let invocation_doc = invocation.as_ref().map_or(Value::Null, |invocation| {
        json!({
            "digest": digest_text(invocation.content_digest()),
            "inputIdentity": digest_text(invocation.input_identity()),
            "dependencyToken": digest_text(invocation.dependency.digest()),
            "actionKey": manifest
                .as_ref()
                .map_or(Value::Null, |manifest| digest_text(invocation.action_key(manifest))),
        })
    });
    if let (Some(invocation), Some(admitted)) = (&invocation, &admitted) {
        in_record(
            "invocation",
            check_function_invocation(invocation, admitted),
            &mut findings,
        );
    }
    let receipt = receipt_json.and_then(|text| match decode_function_receipt(text) {
        Ok(receipt) => Some(receipt),
        Err(decode) => {
            in_record("receipt", decode, &mut findings);
            None
        }
    });
    let receipt_doc = receipt.as_ref().map_or(Value::Null, |receipt| {
        json!({
            "digest": digest_text(receipt.content_digest()),
            "resultIdentity": digest_text(receipt.result_identity()),
        })
    });
    if let (Some(receipt), Some(invocation), Some(admitted)) = (&receipt, &invocation, &admitted) {
        in_record(
            "receipt",
            check_function_receipt(receipt, invocation, admitted),
            &mut findings,
        );
    }
    Ok(json!({
        "format": FUNCTION_FORMAT,
        "admitted": admitted.is_some() && !has_errors(&findings),
        "manifest": manifest_doc,
        "invocation": invocation_doc,
        "receipt": receipt_doc,
        "diagnostics": diagnostics_to_json(&findings),
    })
    .to_string())
}

/// The ABI and the formats and profiles it speaks, as JSON:
/// `{"abi", "crateVersion", "checkerVersion", "diagnosticsFormat",
/// "receiptFormat", "exprProfile"}`.
#[must_use]
pub fn version_json() -> String {
    json!({
        "abi": ABI_VERSION,
        "crateVersion": env!("CARGO_PKG_VERSION"),
        "checkerVersion": CHECKER_VERSION,
        "diagnosticsFormat": DIAGNOSTICS_FORMAT,
        "receiptFormat": RECEIPT_FORMAT,
        "exprProfile": EXPR_PROFILE,
    })
    .to_string()
}
