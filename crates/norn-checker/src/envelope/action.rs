//! Action artifact payload, input UI metadata, and effect-program validation.

use std::collections::{BTreeMap, BTreeSet};

use norn_ir::identifier_limits::{MAX_ACTION_ID_BYTES, MAX_OBJECT_TYPE_NAME_BYTES};
use norn_ir::{ActionEffectProgram, Diagnostic, JsonValue};

use super::closed_schema::{ClosedSchemaProfile, ClosedSchemaUnions, validate_closed_schema};
use super::helpers::{
    expect_object, identifier_too_long, invalid_diagnostic, is_exact_semantic_identity,
    kind_invalid, reject_unknown_object_fields, require_boolean_field, require_literal_field,
    require_non_empty_field,
};

pub(super) fn validate_action_payload(
    value: &JsonValue,
    diagnostics: &mut Vec<Diagnostic>,
    artifact_id: Option<&str>,
) {
    const CODE: &str = "action.validation.invalid_payload";
    let Some(payload) = expect_object(value, "payload", diagnostics, artifact_id, CODE) else {
        return;
    };

    reject_unknown_object_fields(
        payload,
        &[
            "actionId",
            "scopeRequired",
            "idempotent",
            "ledgered",
            "inputSchema",
            "outputSchema",
            "effectProgram",
            "title",
            "description",
            "ledgerCategory",
            "preconditions",
            "slice",
            "aggregates",
            "derive",
            "commands",
            "writes",
            "compensation",
            "access",
            "uiEffects",
            "quantityEffect",
            "createLinks",
            "synchronousRule",
            "independentOf",
        ],
        "payload",
        diagnostics,
        artifact_id,
        CODE,
    );

    require_non_empty_field(
        payload,
        "actionId",
        "payload.actionId",
        diagnostics,
        artifact_id,
        CODE,
    );
    validate_action_identifier_limits(payload, diagnostics, artifact_id);
    validate_action_scope_required(payload.get("scopeRequired"), diagnostics, artifact_id);
    require_boolean_field(
        payload,
        "idempotent",
        "payload.idempotent",
        diagnostics,
        artifact_id,
        CODE,
    );
    require_boolean_field(
        payload,
        "ledgered",
        "payload.ledgered",
        diagnostics,
        artifact_id,
        CODE,
    );
    validate_closed_action_schema(
        payload.get("inputSchema"),
        "payload.inputSchema",
        0,
        true,
        diagnostics,
        artifact_id,
    );
    validate_closed_action_schema(
        payload.get("outputSchema"),
        "payload.outputSchema",
        0,
        false,
        diagnostics,
        artifact_id,
    );
    if let Some(effect_program) = payload.get("effectProgram") {
        validate_action_effect_program(
            effect_program,
            payload.get("inputSchema"),
            diagnostics,
            artifact_id,
        );
    }
    if let Some(ui_effects) = payload.get("uiEffects") {
        validate_action_ui_effects(ui_effects, diagnostics, artifact_id);
    }
    crate::typed_roots::validate_action_typed_members(payload, diagnostics, artifact_id);
}

fn validate_action_identifier_limits(
    payload: &BTreeMap<String, JsonValue>,
    diagnostics: &mut Vec<Diagnostic>,
    artifact_id: Option<&str>,
) {
    if let Some(JsonValue::String(action_id)) = payload.get("actionId")
        && action_id.len() > MAX_ACTION_ID_BYTES
    {
        diagnostics.push(identifier_too_long(
            "action id",
            action_id.len(),
            MAX_ACTION_ID_BYTES,
            "payload.actionId",
            artifact_id,
        ));
    }
    if let Some(JsonValue::Object(effect)) = payload.get("effectProgram")
        && let Some(JsonValue::String(object_kind)) = effect.get("objectKind")
        && object_kind.len() > MAX_OBJECT_TYPE_NAME_BYTES
    {
        diagnostics.push(identifier_too_long(
            "effect object type",
            object_kind.len(),
            MAX_OBJECT_TYPE_NAME_BYTES,
            "payload.effectProgram.objectKind",
            artifact_id,
        ));
    }
    if let Some(JsonValue::Array(writes)) = payload.get("writes") {
        for (index, write) in writes.iter().enumerate() {
            if let JsonValue::Object(write) = write
                && let Some(JsonValue::String(object_kind)) = write.get("entityKind")
                && object_kind.len() > MAX_OBJECT_TYPE_NAME_BYTES
            {
                diagnostics.push(identifier_too_long(
                    "write object type",
                    object_kind.len(),
                    MAX_OBJECT_TYPE_NAME_BYTES,
                    format!("payload.writes[{index}].entityKind"),
                    artifact_id,
                ));
            }
        }
    }
    if let Some(JsonValue::Object(compensation)) = payload.get("compensation")
        && let Some(JsonValue::String(action_id)) = compensation.get("actionId")
        && action_id.len() > MAX_ACTION_ID_BYTES
    {
        diagnostics.push(identifier_too_long(
            "compensation action id",
            action_id.len(),
            MAX_ACTION_ID_BYTES,
            "payload.compensation.actionId",
            artifact_id,
        ));
    }
}

fn validate_action_ui_effects(
    value: &JsonValue,
    diagnostics: &mut Vec<Diagnostic>,
    artifact_id: Option<&str>,
) {
    const CODE: &str = "action.validation.invalid_payload";
    let Some(ui_effects) = value.as_object() else {
        diagnostics.push(invalid_diagnostic(
            CODE,
            "payload.uiEffects must be an object",
            "payload.uiEffects",
            artifact_id,
        ));
        return;
    };

    reject_unknown_object_fields(
        ui_effects,
        &[
            "effectsVersion",
            "irreversible",
            "destructive",
            "money",
            "errorCodes",
            "resultCodes",
        ],
        "payload.uiEffects",
        diagnostics,
        artifact_id,
        CODE,
    );

    match ui_effects.get("effectsVersion") {
        Some(JsonValue::String(version)) if version == "0.1" => {}
        Some(_) => diagnostics.push(invalid_diagnostic(
            CODE,
            "payload.uiEffects.effectsVersion must be \"0.1\"",
            "payload.uiEffects.effectsVersion",
            artifact_id,
        )),
        None => diagnostics.push(invalid_diagnostic(
            CODE,
            "payload.uiEffects.effectsVersion is required",
            "payload.uiEffects.effectsVersion",
            artifact_id,
        )),
    }

    for field in ["irreversible", "destructive"] {
        if let Some(value) = ui_effects.get(field)
            && !matches!(value, JsonValue::Bool(_))
        {
            diagnostics.push(invalid_diagnostic(
                CODE,
                format!("payload.uiEffects.{field} must be a boolean"),
                format!("payload.uiEffects.{field}"),
                artifact_id,
            ));
        }
    }

    for field in ["errorCodes", "resultCodes"] {
        if let Some(value) = ui_effects.get(field) {
            validate_unique_exact_string_array(
                value,
                &format!("payload.uiEffects.{field}"),
                "must be a non-empty exact code",
                diagnostics,
                artifact_id,
                CODE,
            );
        }
    }

    if let Some(value) = ui_effects.get("money") {
        validate_unique_exact_string_array(
            value,
            "payload.uiEffects.money",
            "must be a non-empty exact currency reference",
            diagnostics,
            artifact_id,
            CODE,
        );
    }
}

fn validate_unique_exact_string_array(
    value: &JsonValue,
    path: &str,
    invalid_item_suffix: &str,
    diagnostics: &mut Vec<Diagnostic>,
    artifact_id: Option<&str>,
    code: &str,
) {
    match value {
        JsonValue::Array(items) => {
            let mut seen = BTreeSet::new();
            for (index, item) in items.iter().enumerate() {
                let item_path = format!("{path}[{index}]");
                match item {
                    JsonValue::String(text) if is_exact_semantic_identity(text) => {
                        if !seen.insert(text) {
                            diagnostics.push(invalid_diagnostic(
                                code,
                                format!("{item_path} duplicates an earlier value"),
                                item_path,
                                artifact_id,
                            ));
                        }
                    }
                    JsonValue::Null
                    | JsonValue::Bool(_)
                    | JsonValue::Number(_)
                    | JsonValue::String(_)
                    | JsonValue::Array(_)
                    | JsonValue::Object(_) => diagnostics.push(invalid_diagnostic(
                        code,
                        format!("{item_path} {invalid_item_suffix}"),
                        item_path,
                        artifact_id,
                    )),
                }
            }
        }
        JsonValue::Null
        | JsonValue::Bool(_)
        | JsonValue::Number(_)
        | JsonValue::String(_)
        | JsonValue::Object(_) => diagnostics.push(invalid_diagnostic(
            code,
            format!("{path} must be an array"),
            path,
            artifact_id,
        )),
    }
}

pub(super) fn validate_closed_action_schema(
    value: Option<&JsonValue>,
    path: &str,
    depth: usize,
    action_input: bool,
    diagnostics: &mut Vec<Diagnostic>,
    artifact_id: Option<&str>,
) {
    if action_input
        && value
            .and_then(JsonValue::as_object)
            .and_then(|schema| schema.get("type"))
            .is_some_and(|schema_type| schema_type.as_array().is_some())
    {
        diagnostics.push(invalid_diagnostic(
            "action.validation.invalid_schema",
            "ActionIR type arrays are admitted only as input leaf schemas",
            format!("{path}.type"),
            artifact_id,
        ));
        return;
    }
    validate_closed_schema(
        value,
        path,
        depth,
        diagnostics,
        artifact_id,
        ClosedSchemaProfile {
            code: "action.validation.invalid_schema",
            subject: "ActionIR",
            string_enums: false,
            unions: if action_input {
                ClosedSchemaUnions::ActionInput
            } else {
                ClosedSchemaUnions::None
            },
        },
    );
}

/// The extension member an `ActionIR` input root may carry.
pub(super) const X_NORN: &str = "x-norn";
/// The longest input title or group title, in characters after trimming.
const MAX_INPUT_TITLE_CHARS: usize = 120;
/// The longest input description, in characters after trimming.
const MAX_INPUT_DESCRIPTION_CHARS: usize = 2000;

/// Stable `ActionIR` input-dialect codes (D66).
pub mod input_codes {
    /// A `title` or `description` is not a bounded display string.
    pub const LABEL_INVALID: &str = "action.validation.input_label_invalid";
    /// `x-norn.groups` is not a list of `{id, title, fields}`.
    pub const GROUP_INVALID_SHAPE: &str = "action.validation.input_group_invalid_shape";
    /// Two groups share an id.
    pub const GROUP_DUPLICATE_ID: &str = "action.validation.input_group_duplicate_id";
    /// A group title is not a bounded display string.
    pub const GROUP_TITLE_INVALID: &str = "action.validation.input_group_title_invalid";
    /// A group field does not resolve to an input property.
    pub const GROUP_UNKNOWN_FIELD: &str = "action.validation.input_group_unknown_field";
    /// A field appears in more than one group.
    pub const GROUP_FIELD_REGROUPED: &str = "action.validation.input_group_field_regrouped";
}

/// Whether `text` is a display string of 1 to `max` characters after
/// trimming; `multiline` admits line breaks, never other control characters.
fn display_text(text: Option<&str>, max: usize, multiline: bool) -> bool {
    text.map(str::trim).is_some_and(|text| {
        (1..=max).contains(&text.chars().count())
            && !text
                .chars()
                .any(|c| c.is_control() && !(multiline && matches!(c, '\n' | '\r' | '\t')))
    })
}

/// D66: an input node's optional `title` and `description` are display-only
/// strings; they never change what the action admits.
pub(super) fn validate_input_labels(
    schema: &BTreeMap<String, JsonValue>,
    path: &str,
    diagnostics: &mut Vec<Diagnostic>,
    artifact_id: Option<&str>,
) {
    for (key, max, multiline) in [
        ("title", MAX_INPUT_TITLE_CHARS, false),
        ("description", MAX_INPUT_DESCRIPTION_CHARS, true),
    ] {
        if let Some(value) = schema.get(key)
            && !display_text(value.as_string(), max, multiline)
        {
            diagnostics.push(invalid_diagnostic(
                input_codes::LABEL_INVALID,
                format!("{key} is a string of 1 to {max} characters after trimming, without control characters"),
                format!("{path}.{key}"),
                artifact_id,
            ));
        }
    }
}

/// The property schema an RFC 6901 `pointer` names through `properties`.
fn input_property<'s>(
    schema: &'s BTreeMap<String, JsonValue>,
    pointer: &str,
) -> Option<&'s JsonValue> {
    let rest = pointer.strip_prefix('/')?;
    let mut node = schema;
    let mut found = None;
    for token in rest.split('/') {
        let name = token.replace("~1", "/").replace("~0", "~");
        let property = node
            .get("properties")
            .and_then(JsonValue::as_object)
            .and_then(|properties| properties.get(&name))?;
        found = Some(property);
        node = property.as_object().unwrap_or(&EMPTY_SCHEMA);
    }
    found
}

static EMPTY_SCHEMA: BTreeMap<String, JsonValue> = BTreeMap::new();

/// D66: `x-norn.groups: [{id, title, fields: [pointer]}]` on the input root.
/// Ids are unique, titles bounded, every pointer names an input property, and
/// no field is in two groups.
pub(super) fn validate_input_groups(
    schema: &BTreeMap<String, JsonValue>,
    path: &str,
    diagnostics: &mut Vec<Diagnostic>,
    artifact_id: Option<&str>,
) {
    let Some(extension) = schema.get(X_NORN) else {
        return;
    };
    let at = format!("{path}.{X_NORN}");
    let Some(extension) = extension.as_object() else {
        diagnostics.push(invalid_diagnostic(
            input_codes::GROUP_INVALID_SHAPE,
            "x-norn is an object",
            at,
            artifact_id,
        ));
        return;
    };
    if let Some(unknown) = extension.keys().find(|key| key.as_str() != "groups") {
        diagnostics.push(invalid_diagnostic(
            input_codes::GROUP_INVALID_SHAPE,
            format!("x-norn admits only groups, not {unknown:?}"),
            format!("{at}.{unknown}"),
            artifact_id,
        ));
    }
    let Some(groups) = extension.get("groups") else {
        return;
    };
    let Some(groups) = groups.as_array() else {
        diagnostics.push(invalid_diagnostic(
            input_codes::GROUP_INVALID_SHAPE,
            "groups is an array",
            format!("{at}.groups"),
            artifact_id,
        ));
        return;
    };
    let mut ids = BTreeSet::new();
    let mut grouped = BTreeSet::new();
    for (index, group) in groups.iter().enumerate() {
        let group_at = format!("{at}.groups[{index}]");
        validate_input_group_entry(
            schema,
            group,
            &group_at,
            &mut ids,
            &mut grouped,
            diagnostics,
            artifact_id,
        );
    }
}

fn validate_input_group_entry<'a>(
    schema: &BTreeMap<String, JsonValue>,
    group: &'a JsonValue,
    group_at: &str,
    ids: &mut BTreeSet<&'a str>,
    grouped: &mut BTreeSet<&'a str>,
    diagnostics: &mut Vec<Diagnostic>,
    artifact_id: Option<&str>,
) {
    let push = |diagnostics: &mut Vec<Diagnostic>, code: &str, message: String, at: String| {
        diagnostics.push(invalid_diagnostic(code, message, at, artifact_id));
    };
    let Some(group) = group.as_object() else {
        push(
            diagnostics,
            input_codes::GROUP_INVALID_SHAPE,
            "a group is {id, title, fields}".to_owned(),
            group_at.to_owned(),
        );
        return;
    };
    if let Some(unknown) = group
        .keys()
        .find(|key| !matches!(key.as_str(), "id" | "title" | "fields"))
    {
        push(
            diagnostics,
            input_codes::GROUP_INVALID_SHAPE,
            format!("a group has id, title and fields, not {unknown:?}"),
            format!("{group_at}.{unknown}"),
        );
    }
    match group.get("id").and_then(JsonValue::as_string) {
        Some(id) if !id.trim().is_empty() => {
            if !ids.insert(id) {
                push(
                    diagnostics,
                    input_codes::GROUP_DUPLICATE_ID,
                    format!("group id {id:?} is used twice"),
                    format!("{group_at}.id"),
                );
            }
        }
        _ => push(
            diagnostics,
            input_codes::GROUP_INVALID_SHAPE,
            "a group id is a non-blank string".to_owned(),
            format!("{group_at}.id"),
        ),
    }
    if !display_text(
        group.get("title").and_then(JsonValue::as_string),
        MAX_INPUT_TITLE_CHARS,
        false,
    ) {
        push(
            diagnostics,
            input_codes::GROUP_TITLE_INVALID,
            format!(
                "a group title is 1 to {MAX_INPUT_TITLE_CHARS} characters after trimming, without control characters"
            ),
            format!("{group_at}.title"),
        );
    }
    let Some(fields) = group.get("fields").and_then(JsonValue::as_array) else {
        push(
            diagnostics,
            input_codes::GROUP_INVALID_SHAPE,
            "a group's fields is an array of pointers".to_owned(),
            format!("{group_at}.fields"),
        );
        return;
    };
    for (f, field) in fields.iter().enumerate() {
        let field_at = format!("{group_at}.fields[{f}]");
        let Some(pointer) = field.as_string() else {
            push(
                diagnostics,
                input_codes::GROUP_INVALID_SHAPE,
                "a field is an RFC 6901 pointer".to_owned(),
                field_at,
            );
            continue;
        };
        if input_property(schema, pointer).is_none() {
            push(
                diagnostics,
                input_codes::GROUP_UNKNOWN_FIELD,
                format!("{pointer:?} is not an input property"),
                field_at,
            );
        } else if !grouped.insert(pointer) {
            push(
                diagnostics,
                input_codes::GROUP_FIELD_REGROUPED,
                format!("{pointer:?} is already in a group"),
                field_at,
            );
        }
    }
}

#[expect(
    clippy::too_many_lines,
    reason = "the closed effect whitelist validates common target members and the three admitted variant shapes together"
)]
fn validate_action_effect_program(
    value: &JsonValue,
    input_schema: Option<&JsonValue>,
    diagnostics: &mut Vec<Diagnostic>,
    artifact_id: Option<&str>,
) {
    const CODE: &str = "action.validation.invalid_effect_program";
    // The members every effect carries; `fire_transition` adds the
    // transition it fires (T3-LIFE-3, #2867).
    const TARGET_MEMBERS: [&str; 5] = [
        "kind",
        "objectIdField",
        "objectKind",
        "entityArtifactId",
        "entityArtifactVersion",
    ];
    const TRANSITION_MEMBERS: [&str; 2] = ["lifecycle", "transition"];
    const ROW_MEMBERS: [&str; 5] = [
        "parentValuesField",
        "rowOpsField",
        "parentVersionField",
        "parentDeleteField",
        "collections",
    ];
    let Some(program) = expect_object(
        value,
        "payload.effectProgram",
        diagnostics,
        artifact_id,
        CODE,
    ) else {
        return;
    };
    let fires_transition =
        program.get("kind").and_then(JsonValue::as_string) == Some("fire_transition");
    let mutates_rows =
        program.get("kind").and_then(JsonValue::as_string) == Some("mutate_parent_with_rows");
    let allowed: Vec<&str> = if fires_transition {
        TARGET_MEMBERS
            .into_iter()
            .chain(TRANSITION_MEMBERS)
            .chain(["acceptReceipt"])
            .collect()
    } else if mutates_rows {
        TARGET_MEMBERS.into_iter().chain(ROW_MEMBERS).collect()
    } else if program.get("kind").and_then(JsonValue::as_string) == Some("upsert_input_object") {
        TARGET_MEMBERS.into_iter().chain(["writeMode"]).collect()
    } else {
        TARGET_MEMBERS.to_vec()
    };
    reject_unknown_object_fields(
        program,
        &allowed,
        "payload.effectProgram",
        diagnostics,
        artifact_id,
        CODE,
    );
    require_literal_field(
        program,
        "kind",
        &[
            "upsert_input_object",
            "fire_transition",
            "mutate_parent_with_rows",
        ],
        "payload.effectProgram.kind",
        diagnostics,
        artifact_id,
        CODE,
    );
    if fires_transition {
        for member in TRANSITION_MEMBERS {
            let path = format!("payload.effectProgram.{member}");
            if let Some(id) =
                require_non_empty_field(program, member, &path, diagnostics, artifact_id, CODE)
                && !crate::property_types::matches_grammar(
                    &crate::object_types::MEMBER_ID_GRAMMAR,
                    &id,
                )
            {
                diagnostics.push(kind_invalid(
                    &format!("{path} must match [a-z][a-z0-9_]*"),
                    &path,
                    artifact_id,
                    CODE,
                ));
            }
        }
        if program.contains_key("acceptReceipt")
            && let Err(error) =
                serde_json::to_value(value).and_then(serde_json::from_value::<ActionEffectProgram>)
        {
            diagnostics.push(invalid_diagnostic(
                CODE,
                format!("invalid acceptReceipt declaration: {error}"),
                "payload.effectProgram.acceptReceipt",
                artifact_id,
            ));
        }
    } else if mutates_rows {
        for member in ["parentValuesField", "rowOpsField", "parentVersionField"]
            .into_iter()
            .chain(
                program
                    .contains_key("parentDeleteField")
                    .then_some("parentDeleteField"),
            )
        {
            let path = format!("payload.effectProgram.{member}");
            require_non_empty_field(program, member, &path, diagnostics, artifact_id, CODE);
        }
        // Use the canonical closed enum for nested collection declarations,
        // including their row guard and derivation shapes.
        if let Err(error) =
            serde_json::to_value(value).and_then(serde_json::from_value::<ActionEffectProgram>)
        {
            diagnostics.push(invalid_diagnostic(
                CODE,
                format!("payload.effectProgram is invalid: {error}"),
                "payload.effectProgram",
                artifact_id,
            ));
        }
    } else if program.get("kind").and_then(JsonValue::as_string) == Some("upsert_input_object") {
        validate_upsert_write_mode(program, diagnostics, artifact_id, CODE);
    }
    for (field, path) in [
        ("objectIdField", "payload.effectProgram.objectIdField"),
        ("entityArtifactId", "payload.effectProgram.entityArtifactId"),
        (
            "entityArtifactVersion",
            "payload.effectProgram.entityArtifactVersion",
        ),
    ] {
        require_non_empty_field(program, field, path, diagnostics, artifact_id, CODE);
    }
    if let Some(object_kind) = require_non_empty_field(
        program,
        "objectKind",
        "payload.effectProgram.objectKind",
        diagnostics,
        artifact_id,
        CODE,
    ) && !is_exact_semantic_identity(&object_kind)
    {
        diagnostics.push(kind_invalid(
            "payload.effectProgram.objectKind must be an exact identity without whitespace, control characters, or wildcards",
            "payload.effectProgram.objectKind",
            artifact_id,
            CODE,
        ));
    }

    let Some(object_id_field) = program.get("objectIdField").and_then(JsonValue::as_string) else {
        return;
    };
    let Some(input_schema) = input_schema.and_then(JsonValue::as_object) else {
        return;
    };
    let required = input_schema
        .get("required")
        .and_then(JsonValue::as_array)
        .is_some_and(|fields| {
            fields
                .iter()
                .any(|field| field.as_string() == Some(object_id_field))
        });
    let create_only_generated_id = program.get("kind").and_then(JsonValue::as_string)
        == Some("upsert_input_object")
        && program
            .get("writeMode")
            .and_then(JsonValue::as_object)
            .and_then(|mode| mode.get("kind"))
            .and_then(JsonValue::as_string)
            == Some("create_only");
    let string_field = input_schema
        .get("properties")
        .and_then(JsonValue::as_object)
        .and_then(|properties| properties.get(object_id_field))
        .and_then(JsonValue::as_object)
        .and_then(|field| field.get("type"))
        .and_then(JsonValue::as_string)
        == Some("string");
    if (!required && !create_only_generated_id) || !string_field {
        diagnostics.push(invalid_diagnostic(
            CODE,
            "objectIdField must name a string input field; it may be optional only for create_only upserts",
            "payload.effectProgram.objectIdField",
            artifact_id,
        ));
    }
}

/// Check the closed tagged shape of an optional upsert write mode (T2).
fn validate_upsert_write_mode(
    program: &BTreeMap<String, JsonValue>,
    diagnostics: &mut Vec<Diagnostic>,
    artifact_id: Option<&str>,
    code: &str,
) {
    let Some(value) = program.get("writeMode") else {
        return;
    };
    let Some(mode) = expect_object(
        value,
        "payload.effectProgram.writeMode",
        diagnostics,
        artifact_id,
        code,
    ) else {
        return;
    };
    let kind = mode.get("kind").and_then(JsonValue::as_string);
    let allowed: &[&str] = match kind {
        Some("upsert" | "create_only" | "update_only") => &["kind"],
        Some("draft_only") => &["kind", "lifecycle", "state"],
        _ => {
            diagnostics.push(invalid_diagnostic(
                code,
                "writeMode.kind must be upsert, create_only, update_only, or draft_only",
                "payload.effectProgram.writeMode.kind",
                artifact_id,
            ));
            return;
        }
    };
    reject_unknown_object_fields(
        mode,
        allowed,
        "payload.effectProgram.writeMode",
        diagnostics,
        artifact_id,
        code,
    );
    require_literal_field(
        mode,
        "kind",
        &["upsert", "create_only", "update_only", "draft_only"],
        "payload.effectProgram.writeMode.kind",
        diagnostics,
        artifact_id,
        code,
    );
    if kind == Some("draft_only") {
        let lifecycle = require_non_empty_field(
            mode,
            "lifecycle",
            "payload.effectProgram.writeMode.lifecycle",
            diagnostics,
            artifact_id,
            code,
        );
        if let Some(lifecycle) = lifecycle
            && !crate::property_types::matches_grammar(
                &crate::object_types::MEMBER_ID_GRAMMAR,
                &lifecycle,
            )
        {
            diagnostics.push(kind_invalid(
                "payload.effectProgram.writeMode.lifecycle must match [a-z][a-z0-9_]*",
                "payload.effectProgram.writeMode.lifecycle",
                artifact_id,
                code,
            ));
        }
        require_non_empty_field(
            mode,
            "state",
            "payload.effectProgram.writeMode.state",
            diagnostics,
            artifact_id,
            code,
        );
    }
}
fn validate_action_scope_required(
    value: Option<&JsonValue>,
    diagnostics: &mut Vec<Diagnostic>,
    artifact_id: Option<&str>,
) {
    const CODE: &str = "action.validation.invalid_payload";
    match value {
        Some(JsonValue::Array(fields)) if !fields.is_empty() => {
            let mut seen = BTreeSet::new();
            for (index, field) in fields.iter().enumerate() {
                match field {
                    JsonValue::String(value) if !value.trim().is_empty() => {
                        if !seen.insert(value.clone()) {
                            diagnostics.push(invalid_diagnostic(
                                CODE,
                                format!("duplicate required scope field '{value}'"),
                                format!("payload.scopeRequired[{index}]"),
                                artifact_id,
                            ));
                        }
                    }
                    JsonValue::Null
                    | JsonValue::Bool(_)
                    | JsonValue::Number(_)
                    | JsonValue::String(_)
                    | JsonValue::Array(_)
                    | JsonValue::Object(_) => diagnostics.push(invalid_diagnostic(
                        CODE,
                        format!("payload.scopeRequired[{index}] must be a non-empty string"),
                        format!("payload.scopeRequired[{index}]"),
                        artifact_id,
                    )),
                }
            }
        }
        Some(JsonValue::Array(_)) => diagnostics.push(invalid_diagnostic(
            CODE,
            "scopeRequired must be a non-empty array",
            "payload.scopeRequired",
            artifact_id,
        )),
        Some(_) => diagnostics.push(invalid_diagnostic(
            CODE,
            "scopeRequired must be an array",
            "payload.scopeRequired",
            artifact_id,
        )),
        None => diagnostics.push(invalid_diagnostic(
            CODE,
            "scopeRequired is required",
            "payload.scopeRequired",
            artifact_id,
        )),
    }
}
