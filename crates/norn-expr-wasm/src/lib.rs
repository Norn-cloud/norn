//! The ZEN engine alone (D105, UI-MACHINE-1): `evaluate_expression_json`, the one evaluation entry
//! point of the checker ABI, without the rest of the checker. `norn-checker-wasm` re-exports this
//! very function, so the checker package and this engine package evaluate with the same code from
//! the same `norn-expr`; a page machine's guards and assignments load this small package instead
//! of the whole checker. `tests/parity/engine-parity.mjs` holds the two built packages to the same
//! output on a shared corpus.

use norn_expr::{
    EvaluationBudget, ExprError, Expression, Value as ExprValue, validate_evaluation_source,
};
use norn_ir::ResourceLimitProfile;
use serde_json::{Value, json};

#[cfg(all(target_arch = "wasm32", feature = "bindings"))]
pub mod wasm;

/// Format tag of the [`evaluate_expression_json`] document.
pub const EVALUATION_FORMAT: &str = "norn-checker-abi/evaluation@1";

/// An argument that is not of the expected shape.
struct Invalid {
    argument: &'static str,
    detail: String,
}

fn invalid(argument: &'static str, detail: impl Into<String>) -> Invalid {
    Invalid {
        argument,
        detail: detail.into(),
    }
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
) -> Result<ExprValue, Invalid> {
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
            // `fields` and `event` are a page machine's guard and assignment inputs
            // (UI-MACHINE-1): the runtime evaluates them with this same engine.
            "record" | "row" | "draft" | "context" | "fields" | "event" => {
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
fn evaluation_abi_error_json(profile: &str, error: Invalid) -> String {
    let Invalid { argument, detail } = error;
    let (code, message) = (
        if argument == "profile" {
            "expr.profile.invalid"
        } else {
            "expr.input.invalid"
        },
        detail,
    );
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
