//! What a tool binding does at its provider (#1099, piece 6 of #772).
//!
//! A binding is a **query** (it reads; retrying or replaying it is harmless) or
//! a **command** (it changes something at the provider). A command runs as a
//! Core 07 outbound intent, so an ambiguous outcome may be retried only under
//! an explicit provider dedup contract (`docs/spec/core/07-world-boundary.md`,
//! "Perform and recovery semantics"). [`check_effect`] admits a binding's
//! declared effect against its tool's input schema and fixed inputs, and
//! [`check_reconcile`] admits the query binding a command names to settle an
//! unknown outcome.
//!
//! The check is pure and sans-IO: it reads only the values it is given.

use serde_json::{Map, Value};

/// The declared effect is missing, not an object, of an unknown kind, or has unknown fields.
pub const EFFECT_INVALID: &str = "connector.effect.invalid";
/// A query declares a dedup contract, which only commands take.
pub const EFFECT_QUERY_CONTRACT: &str = "connector.effect.query_contract";
/// A command declares neither a provider key nor a reconcile query.
pub const EFFECT_NO_DEDUP_CONTRACT: &str = "connector.effect.no_dedup_contract";
/// The provider key is not one of the tool's own string inputs.
pub const EFFECT_KEY_NOT_INPUT: &str = "connector.effect.key_not_input";
/// The provider key is an input the binding fixes, so the cell could not fill it.
pub const EFFECT_KEY_FIXED: &str = "connector.effect.key_fixed";
/// The reconcile binding is unknown or is not a query.
pub const EFFECT_RECONCILE_NOT_QUERY: &str = "connector.effect.reconcile_not_query";

/// A binding's admitted effect.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Effect {
    /// Reads only: retrying or replaying it is harmless.
    Query,
    /// Changes something at the provider; at least one of the two contracts is set.
    Command {
        /// The tool input the cell fills with the operation's idempotency key.
        provider_key: Option<String>,
        /// The query binding that settles an unknown outcome.
        reconcile: Option<String>,
    },
}

impl Effect {
    /// The effect kind as written in the payload.
    #[must_use]
    pub const fn kind(&self) -> &'static str {
        match self {
            Self::Query => "query",
            Self::Command { .. } => "command",
        }
    }
}

/// Why a declared effect was refused, at a JSON Pointer into the binding payload.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EffectRefusal {
    /// The stable code (registered in `conformance/diagnostic-codes/`).
    pub code: &'static str,
    /// Where in the binding payload, for example `/effect/providerKey`.
    pub pointer: String,
    /// What is wrong, for the person editing the binding.
    pub message: String,
}

fn refusal(code: &'static str, pointer: &str, message: impl Into<String>) -> EffectRefusal {
    EffectRefusal {
        code,
        pointer: pointer.to_owned(),
        message: message.into(),
    }
}

const QUERY_FIELDS: &[&str] = &["kind"];
const COMMAND_FIELDS: &[&str] = &["kind", "providerKey", "reconcile"];

/// An optional non-blank string field of the effect.
fn name_field(
    effect: &Map<String, Value>,
    key: &str,
    refusals: &mut Vec<EffectRefusal>,
) -> Option<String> {
    match effect.get(key) {
        None => None,
        Some(Value::String(name)) if !name.trim().is_empty() => Some(name.clone()),
        Some(_) => {
            refusals.push(refusal(
                EFFECT_INVALID,
                &format!("/effect/{key}"),
                format!("{key} must be a non-blank string."),
            ));
            None
        }
    }
}

/// Whether a property schema admits only strings (`"string"` or `["string"]`).
fn is_string_schema(property: &Value) -> bool {
    match property.get("type") {
        Some(Value::String(kind)) => kind == "string",
        Some(Value::Array(kinds)) => {
            matches!(kinds.as_slice(), [Value::String(kind)] if kind == "string")
        }
        _ => false,
    }
}

/// Admit a binding's declared `effect` against its tool's `input_schema` and
/// the binding's `input_mapping` (`{field: {fixed: value}}`).
///
/// Every refusal is reported, not only the first.
///
/// # Errors
/// The refusals, each with its stable code and payload pointer.
pub fn check_effect(
    effect: &Value,
    input_schema: &Value,
    input_mapping: &Value,
) -> Result<Effect, Vec<EffectRefusal>> {
    let Some(effect) = effect.as_object() else {
        return Err(vec![refusal(
            EFFECT_INVALID,
            "/effect",
            "effect must be {kind: \"query\"} or {kind: \"command\", …}.",
        )]);
    };
    let kind = effect.get("kind").and_then(Value::as_str);
    let allowed = match kind {
        Some("query") => QUERY_FIELDS,
        Some("command") => COMMAND_FIELDS,
        _ => {
            return Err(vec![refusal(
                EFFECT_INVALID,
                "/effect/kind",
                "kind must be \"query\" or \"command\".",
            )]);
        }
    };
    let mut refusals = Vec::new();
    for key in effect.keys() {
        if allowed.contains(&key.as_str()) {
            continue;
        }
        let contract = COMMAND_FIELDS.contains(&key.as_str());
        refusals.push(if contract {
            refusal(
                EFFECT_QUERY_CONTRACT,
                &format!("/effect/{key}"),
                format!("A query is safe to retry and takes no {key}; only a command does."),
            )
        } else {
            refusal(
                EFFECT_INVALID,
                &format!("/effect/{key}"),
                format!("{key} is not an effect field."),
            )
        });
    }
    if kind == Some("query") {
        return if refusals.is_empty() {
            Ok(Effect::Query)
        } else {
            Err(refusals)
        };
    }

    let provider_key = name_field(effect, "providerKey", &mut refusals);
    let reconcile = name_field(effect, "reconcile", &mut refusals);
    if !effect.contains_key("providerKey") && !effect.contains_key("reconcile") {
        refusals.push(refusal(
            EFFECT_NO_DEDUP_CONTRACT,
            "/effect",
            "A command needs providerKey (an input the provider deduplicates by) or reconcile (a query that settles an unknown outcome), so an unknown outcome is never retried blind.",
        ));
    }
    if let Some(field) = &provider_key {
        let property = input_schema
            .get("properties")
            .and_then(Value::as_object)
            .and_then(|properties| properties.get(field));
        match property {
            Some(property) if is_string_schema(property) => {}
            Some(_) => refusals.push(refusal(
                EFFECT_KEY_NOT_INPUT,
                "/effect/providerKey",
                format!("{field} must be a string input to carry the idempotency key."),
            )),
            None => refusals.push(refusal(
                EFFECT_KEY_NOT_INPUT,
                "/effect/providerKey",
                format!("{field} is not an input of this tool."),
            )),
        }
        if input_mapping
            .as_object()
            .is_some_and(|mapping| mapping.contains_key(field))
        {
            refusals.push(refusal(
                EFFECT_KEY_FIXED,
                "/effect/providerKey",
                format!("{field} is fixed by this binding; the cell fills the provider key for each operation."),
            ));
        }
    }
    if refusals.is_empty() {
        Ok(Effect::Command {
            provider_key,
            reconcile,
        })
    } else {
        Err(refusals)
    }
}

/// Admit the binding a command names to settle unknown outcomes: `target` is
/// that binding's admitted effect, or `None` when no such binding exists.
///
/// # Errors
/// [`EFFECT_RECONCILE_NOT_QUERY`] when the binding is unknown or not a query.
pub fn check_reconcile(reconcile: &str, target: Option<&Effect>) -> Result<(), EffectRefusal> {
    match target {
        Some(Effect::Query) => Ok(()),
        Some(Effect::Command { .. }) => Err(refusal(
            EFFECT_RECONCILE_NOT_QUERY,
            "/effect/reconcile",
            format!(
                "{reconcile} is a command; settling an outcome must not change anything at the provider."
            ),
        )),
        None => Err(refusal(
            EFFECT_RECONCILE_NOT_QUERY,
            "/effect/reconcile",
            format!("{reconcile} is not a tool binding."),
        )),
    }
}
