//! `ActionIR` → MCP tool projection (ruling 2026-09-25 §16: every ontology action
//! projects to an MCP tool with schemas derived from the type system).
//!
//! The target shape is [`McpTool`], the `Tool` definition of MCP schema
//! revision 2026-07-28.

use std::collections::BTreeSet;

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
/// required; a field the server fills without reading any input (the actor,
/// the clock) is not in the schema at all. Derivations are exposed in metadata in execution order.
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
    action_tool_with_server_filled(action, &BTreeSet::new())
}

/// Project a typed action root onto its MCP tool definition, additionally
/// omitting caller-forbidden `extra_server_filled` fields (such as the kernel's
/// `actor_derived_fields` set for `ACTION-ACTOR-1`).
///
/// Keeping this parameter explicit allows Ring-0 `norn-projections` to stay
/// decoupled from `norn-expr` while hosted callers (`norn-cell-do`) supply the
/// exact kernel-evaluated actor-derived field set, including fail-closed
/// unparseable derivations.
///
/// # Errors
///
/// [`McpProjectionError::InputSchemaNotObject`] when the action's input schema
/// is not an object schema; MCP tool arguments are always JSON objects.
pub fn action_tool_with_server_filled(
    action: &ActionRoot,
    extra_server_filled: &BTreeSet<String>,
) -> Result<McpTool, McpProjectionError> {
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
    let legacy_read_only =
        !action.ledgered && action.effect_program.is_none() && action.writes.is_empty();
    let writes_additive = action
        .writes
        .iter()
        .all(|write| is_additive_write(write.change));
    let legacy_additive = match action.effect_program.as_ref() {
        None => !action.writes.is_empty() && writes_additive,
        Some(program) => {
            writes_additive && program.changes().iter().copied().all(is_additive_write)
        }
    };
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
        input_schema: caller_input_schema(action, extra_server_filled),
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

const fn is_additive_write(change: WriteChange) -> bool {
    match change {
        WriteChange::Created => true,
        WriteChange::Modified | WriteChange::Deleted => false,
    }
}

const MAX_SCHEMA_DEPTH: usize = 64;
const DECIMAL_WIRE_DESCRIPTION: &str = "Exact decimal text as a JSON string, for example \"125\". Use a dot decimal separator; do not send a JSON number.";

fn decimal_wire_schema(schema: &mut JsonObject) {
    decimal_wire_object(schema, 0);
}

fn decimal_wire_object(schema: &mut JsonObject, depth: usize) {
    if depth >= MAX_SCHEMA_DEPTH {
        return;
    }
    let is_decimal = schema
        .get("nornType")
        .and_then(JsonValue::as_object)
        .and_then(|site| site.get("kind"))
        .and_then(JsonValue::as_string)
        == Some("decimal");
    let mut rewrote_decimal = false;
    if is_decimal && let Some(kind) = schema.get_mut("type") {
        match kind {
            JsonValue::String(text) if text == "number" => {
                *text = "string".to_owned();
                rewrote_decimal = true;
            }
            JsonValue::Array(items) => {
                for item in items {
                    if let JsonValue::String(text) = item
                        && text == "number"
                    {
                        *text = "string".to_owned();
                        rewrote_decimal = true;
                    }
                }
            }
            JsonValue::Null
            | JsonValue::Bool(_)
            | JsonValue::Number(_)
            | JsonValue::String(_)
            | JsonValue::Object(_) => {}
        }
    }
    if rewrote_decimal {
        schema
            .entry("description".to_owned())
            .or_insert_with(|| JsonValue::String(DECIMAL_WIRE_DESCRIPTION.to_owned()));
    } else {
        for value in schema.values_mut() {
            decimal_wire_value(value, depth.saturating_add(1));
        }
    }
}

fn decimal_wire_value(value: &mut JsonValue, depth: usize) {
    if depth >= MAX_SCHEMA_DEPTH {
        return;
    }
    match value {
        JsonValue::Object(child) => decimal_wire_object(child, depth),
        JsonValue::Array(items) => {
            for item in items {
                decimal_wire_value(item, depth.saturating_add(1));
            }
        }
        JsonValue::Null | JsonValue::Bool(_) | JsonValue::Number(_) | JsonValue::String(_) => {}
    }
}

fn schema_allows_null(schema: &JsonValue) -> bool {
    let Some(object) = schema.as_object() else {
        return false;
    };
    if let Some(kind) = object.get("type") {
        if kind.as_string() == Some("null") {
            return true;
        }
        if kind
            .as_array()
            .is_some_and(|items| items.iter().any(|item| item.as_string() == Some("null")))
        {
            return true;
        }
    }
    for combiner in ["anyOf", "oneOf"] {
        if object
            .get(combiner)
            .and_then(JsonValue::as_array)
            .is_some_and(|branches| {
                branches.iter().any(|branch| {
                    branch
                        .as_object()
                        .and_then(|b| b.get("type"))
                        .and_then(JsonValue::as_string)
                        == Some("null")
                })
            })
        {
            return true;
        }
    }
    false
}

/// Project caller requirements without evaluating or parsing ZEN.
fn caller_input_schema(action: &ActionRoot, extra_server_filled: &BTreeSet<String>) -> JsonObject {
    let mut input_schema = action.input_schema.clone();
    decimal_wire_schema(&mut input_schema);
    // A field the server fills from its own context (the actor, the clock) is
    // not the caller's to send: it leaves the schema, so an agent never offers
    // a value the call would refuse.
    let filled = server_filled(action, extra_server_filled);
    let required_derived: BTreeSet<&str> = action
        .derive
        .iter()
        .filter(|step| step.input_required && !filled.contains(step.field.as_str()))
        .map(|step| step.field.as_str())
        .collect();
    let optional_only_derived: BTreeSet<&str> = action
        .derive
        .iter()
        .filter(|step| {
            !step.input_required
                && !filled.contains(step.field.as_str())
                && !required_derived.contains(step.field.as_str())
        })
        .map(|step| step.field.as_str())
        .collect();
    if !required_derived.is_empty() {
        let required = input_schema
            .entry("required".to_owned())
            .or_insert_with(|| JsonValue::Array(Vec::new()));
        if let JsonValue::Array(required) = required {
            for step in action
                .derive
                .iter()
                .filter(|step| required_derived.contains(step.field.as_str()))
            {
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
            field.as_string().is_some_and(|field| {
                !filled.contains(field) && !optional_only_derived.contains(field)
            })
        });
    }
    if let Some(JsonValue::Object(properties)) = input_schema.get_mut("properties") {
        properties.retain(|field, _| !filled.contains(field.as_str()));
        let mut projected = BTreeSet::new();
        for derivation in action
            .derive
            .iter()
            .filter(|step| optional_only_derived.contains(step.field.as_str()))
        {
            // Repeated steps for a field share one caller requirement.
            // A null source has the same meaning as an omitted source.
            if let Some(schema) = properties.get_mut(&derivation.field)
                && projected.insert(&derivation.field)
                && !schema_allows_null(schema)
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

/// Whether a ZEN expression references the root identifier `target` (or the
/// whole-environment root `$`) outside string literals and not as a member
/// after `.`. Mirrors `norn_expr::expression_read_paths`, where `Node::Root`
/// conservatively counts as reading every root variable.
fn expression_reads_root_ident(expression: &str, target: &str) -> bool {
    let mut chars = expression.chars().peekable();
    let mut prev_dot = false;
    while let Some(ch) = chars.next() {
        if ch == '\'' || ch == '"' {
            let quote = ch;
            while let Some(inner) = chars.next() {
                if inner == '\\' {
                    let _escaped = chars.next();
                } else if inner == quote {
                    break;
                }
            }
            prev_dot = false;
        } else if ch == '$' {
            if !prev_dot {
                return true;
            }
            prev_dot = false;
        } else if ch.is_ascii_alphabetic() || ch == '_' {
            let mut ident = String::new();
            ident.push(ch);
            while let Some(&next) = chars.peek() {
                if next.is_ascii_alphanumeric() || next == '_' {
                    ident.push(next);
                    let _consumed = chars.next();
                } else {
                    break;
                }
            }
            if !prev_dot && ident == target {
                return true;
            }
            prev_dot = false;
        } else if ch == '.' {
            prev_dot = true;
        } else if !ch.is_whitespace() {
            prev_dot = false;
        }
    }
    false
}

/// Fields the server fills and the caller must not supply:
/// - Any field in `extra_server_filled` (e.g. `norn_kernel::actor_derived_fields`).
/// - ACTION-ACTOR-1: any field with an `inputRequired: false` derivation that
///   reads `actor` (even if it also reads `input` or shares the field with
///   another step), because the kernel refuses caller-supplied values for it.
/// - Any field whose derivations are all `inputRequired: false` and never read
///   `input` (e.g. `now` or a constant).
fn server_filled<'a>(
    action: &'a ActionRoot,
    extra_server_filled: &'a BTreeSet<String>,
) -> BTreeSet<&'a str> {
    let mut filled: BTreeSet<&'a str> = extra_server_filled.iter().map(String::as_str).collect();
    let mut derived_fields = BTreeSet::new();
    for derivation in &action.derive {
        let field = derivation.field.as_str();
        derived_fields.insert(field);
        if !derivation.input_required
            && expression_reads_root_ident(&derivation.expression.0, "actor")
        {
            filled.insert(field);
        }
    }
    for field in derived_fields {
        if action
            .derive
            .iter()
            .filter(|derivation| derivation.field == field)
            .all(|derivation| {
                !derivation.input_required
                    && !expression_reads_root_ident(&derivation.expression.0, "input")
            })
        {
            filled.insert(field);
        }
    }
    filled
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
        accept_receipt,
        ..
    } = program
    else {
        return None;
    };
    let string = |value: &str| JsonValue::String(value.to_owned());
    let mut descriptor = JsonValue::Object(
        [
            ("objectKind", string(object_kind)),
            ("lifecycle", string(lifecycle.as_str())),
            ("transition", string(transition.as_str())),
        ]
        .into_iter()
        .map(|(key, value)| (key.to_owned(), value))
        .collect(),
    );
    if let (Some(binding), JsonValue::Object(fields)) = (accept_receipt, &mut descriptor) {
        fields.insert(
            "acceptReceipt".to_owned(),
            JsonValue::Object(JsonObject::from([
                (
                    "referenceField".to_owned(),
                    string(&binding.reference_field),
                ),
                (
                    "manifestDigest".to_owned(),
                    string(&binding.manifest_digest.0),
                ),
                (
                    "digestProperty".to_owned(),
                    string(binding.digest_property.as_str()),
                ),
                (
                    "resultBindings".to_owned(),
                    JsonValue::Object(
                        binding
                            .result_bindings
                            .iter()
                            .map(|(target, source)| (target.0.clone(), string(source.as_str())))
                            .collect(),
                    ),
                ),
            ])),
        );
    }
    Some(JsonObject::from([(
        TRANSITION_META_KEY.to_owned(),
        descriptor,
    )]))
}
