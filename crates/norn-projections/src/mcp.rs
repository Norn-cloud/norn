//! `ActionIR` → MCP tool projection (ruling 2026-09-25 §16: every ontology action
//! projects to an MCP tool with schemas derived from the type system).
//!
//! The target shape is [`McpTool`], the `Tool` definition of MCP schema
//! revision 2026-07-28.

use norn_ir::{
    ActionCommandOperation, ActionEffectProgram, ActionRoot, ActionUiEffect, JsonObject, JsonValue,
    McpTool, McpToolAnnotations, WriteChange,
};

/// The `_meta` key of the declared transition a tool fires.
pub const TRANSITION_META_KEY: &str = "norn/transition";

/// The `_meta` key describing ordered input derivations.
pub const INPUT_DERIVATIONS_META_KEY: &str = "norn/inputDerivations";

/// Why an action cannot be projected onto an MCP tool.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum McpProjectionError {
    /// MCP requires the tool `inputSchema` to be an object schema
    /// (`type: "object"` at the root); the action's input schema is not.
    InputSchemaNotObject,
}

impl std::fmt::Display for McpProjectionError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InputSchemaNotObject => {
                formatter.write_str("MCP tools require an object inputSchema (type: \"object\")")
            }
        }
    }
}

impl std::error::Error for McpProjectionError {}

/// Project a typed action root onto its MCP tool definition.
///
/// `name`, `title`, `description`, and `outputSchema` are copied from the root.
/// The input schema describes caller requirements: defaulted/computed fields
/// marked `inputRequired: false` are optional; normalized required inputs stay
/// required. Derivations are exposed in metadata in execution order.
/// The annotations are derived, never authored, so they cannot
/// contradict the definition:
///
/// - `readOnlyHint`: the action appends no ledger event, has no effect
///   program, declares no writes, and has no external, irreversible,
///   destructive, or monetary effects.
/// - `destructiveHint`: preserves the legacy local write/effect risk and adds
///   declared irreversible/destructive UI effects and non-create outbound
///   commands. Create commands do not weaken the existing risk classification.
/// - `idempotentHint`: whether the shared effective-effect set contains
///   `idempotent`.
/// - `openWorldHint`: whether the action declares outbound commands, which
///   execute separately through `perform` after the action commit.
///
/// A `fire_transition` action also carries `_meta["norn/transition"]`:
/// `{objectKind, lifecycle, transition}`, so an agent can see which declared
/// transition the tool fires (and expect a refusal from any other state)
/// without reading the IR. Actions with derivations also carry
/// `_meta["norn/inputDerivations"]`.
///
/// # Errors
///
/// [`McpProjectionError::InputSchemaNotObject`] when the action's input schema
/// is not an object schema; MCP tool arguments are always JSON objects.
pub fn action_tool(action: &ActionRoot) -> Result<McpTool, McpProjectionError> {
    if action
        .input_schema
        .get("type")
        .and_then(JsonValue::as_string)
        != Some("object")
    {
        return Err(McpProjectionError::InputSchemaNotObject);
    }
    let effects = action.effective_ui_effects();
    let external = effects.contains(&ActionUiEffect::External);
    let irreversible = effects.contains(&ActionUiEffect::Irreversible);
    let explicitly_destructive = effects.contains(&ActionUiEffect::Destructive);
    let has_money_effect = effects
        .iter()
        .any(|effect| matches!(effect, ActionUiEffect::Money { .. }));
    let read_only = !action.ledgered
        && action.effect_program.is_none()
        && action.writes.is_empty()
        && !external
        && !irreversible
        && !explicitly_destructive
        && !has_money_effect;
    // Keep the legacy local-risk predicate intact so newly understood
    // outbound commands can add risk without downgrading an existing hint.
    let legacy_read_only =
        !action.ledgered && action.effect_program.is_none() && action.writes.is_empty();
    let legacy_additive = action.effect_program.as_ref().map_or_else(
        || {
            !action.writes.is_empty()
                && action
                    .writes
                    .iter()
                    .all(|write| write.change == WriteChange::Created)
        },
        |program| !program.changes().contains(&WriteChange::Modified),
    );
    let legacy_destructive = !legacy_read_only && !legacy_additive;
    let has_non_create_command = action
        .commands
        .iter()
        .any(|command| command.operation != ActionCommandOperation::Create);
    let destructive =
        legacy_destructive || explicitly_destructive || irreversible || has_non_create_command;

    Ok(McpTool {
        name: action.action_id.clone(),
        title: action.title.clone(),
        description: action.description.clone(),
        input_schema: caller_input_schema(action),
        output_schema: Some(action.output_schema.clone()),
        annotations: Some(McpToolAnnotations {
            title: action.title.clone(),
            read_only_hint: Some(read_only),
            destructive_hint: Some(destructive),
            idempotent_hint: Some(effects.contains(&ActionUiEffect::Idempotent)),
            open_world_hint: Some(external),
        }),
        meta: action_meta(action),
    })
}

/// Project caller requirements without evaluating or parsing ZEN.
fn caller_input_schema(action: &ActionRoot) -> JsonObject {
    let mut input_schema = action.input_schema.clone();
    if action.derive.iter().any(|step| step.input_required) {
        let required = input_schema
            .entry("required".to_owned())
            .or_insert_with(|| JsonValue::Array(Vec::new()));
        if let JsonValue::Array(required) = required {
            for step in action.derive.iter().filter(|step| step.input_required) {
                if !required
                    .iter()
                    .any(|field| field.as_string() == Some(step.field.as_str()))
                {
                    required.push(JsonValue::String(step.field.clone()));
                }
            }
        }
    }
    if let Some(JsonValue::Array(required)) = input_schema.get_mut("required") {
        required.retain(|field| {
            !action.derive.iter().any(|derivation| {
                !derivation.input_required && field.as_string() == Some(derivation.field.as_str())
            })
        });
    }
    if let Some(JsonValue::Object(properties)) = input_schema.get_mut("properties") {
        let mut projected = std::collections::BTreeSet::new();
        for derivation in action.derive.iter().filter(|step| !step.input_required) {
            // Repeated steps for a field share one caller requirement.
            // A null source has the same meaning as an omitted source.
            if let Some(schema) = properties.get_mut(&derivation.field)
                && projected.insert(&derivation.field)
            {
                *schema = JsonValue::Object(JsonObject::from([(
                    "anyOf".to_owned(),
                    JsonValue::Array(vec![
                        schema.clone(),
                        JsonValue::Object(JsonObject::from([(
                            "type".to_owned(),
                            JsonValue::String("null".to_owned()),
                        )])),
                    ]),
                )]));
            }
        }
    }
    input_schema
}

/// Preserve transition metadata beside ordered input derivations.
fn action_meta(action: &ActionRoot) -> Option<JsonObject> {
    let mut meta = action
        .effect_program
        .as_ref()
        .and_then(transition_meta)
        .unwrap_or_default();
    if !action.derive.is_empty() {
        meta.insert(
            INPUT_DERIVATIONS_META_KEY.to_owned(),
            JsonValue::Array(
                action
                    .derive
                    .iter()
                    .map(|derivation| {
                        JsonValue::Object(JsonObject::from([
                            (
                                "field".to_owned(),
                                JsonValue::String(derivation.field.clone()),
                            ),
                            (
                                "expression".to_owned(),
                                JsonValue::String(derivation.expression.0.clone()),
                            ),
                            (
                                "inputRequired".to_owned(),
                                JsonValue::Bool(derivation.input_required),
                            ),
                        ]))
                    })
                    .collect(),
            ),
        );
    }

    (!meta.is_empty()).then_some(meta)
}

/// The `_meta` of a tool whose action fires a declared transition.
fn transition_meta(program: &ActionEffectProgram) -> Option<JsonObject> {
    let ActionEffectProgram::FireTransition {
        object_kind,
        lifecycle,
        transition,
        ..
    } = program
    else {
        return None;
    };
    let string = |value: &str| JsonValue::String(value.to_owned());
    let descriptor = JsonValue::Object(
        [
            ("objectKind", string(object_kind)),
            ("lifecycle", string(lifecycle.as_str())),
            ("transition", string(transition.as_str())),
        ]
        .into_iter()
        .map(|(key, value)| (key.to_owned(), value))
        .collect(),
    );
    Some(JsonObject::from([(
        TRANSITION_META_KEY.to_owned(),
        descriptor,
    )]))
}
