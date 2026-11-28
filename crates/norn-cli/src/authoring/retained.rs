//! Structural validation for retained JSON definitions (`question`, `dashboard`, `approval`)
//! declared inside manifest `sources` directories.

use std::collections::BTreeSet;

use serde_json::{Map, Value};

use super::AuthoringFailure;

const MAX_DASHBOARD_TILES: usize = 64;
const MAX_TITLE_CHARS: usize = 120;
const MAX_LAYOUT_COORD: u64 = 1000;
const MAX_REQUEST_CHARS: usize = 2000;
const MAX_APPROVAL_QUESTION_CHARS: usize = 500;
const MAX_APPROVAL_EXPIRES_MS: u64 = 604_800_000;
const APPROVAL_MEMBERS: [&str; 6] = [
    "action",
    "decider",
    "question",
    "risk",
    "expiresInMs",
    "when",
];
const WHEN_OPS: [&str; 6] = ["eq", "ne", "lt", "le", "gt", "ge"];

fn invalid(shown: &str, kind: &str, id: &str, detail: &str) -> AuthoringFailure {
    AuthoringFailure::Io(format!(
        "{shown}: invalid retained {kind} definition `{id}`: {detail}"
    ))
}

fn check_closed_object<'a>(
    value: &'a Value,
    allowed: &[&str],
    shown: &str,
    kind: &str,
    id: &str,
    context: &str,
) -> Result<&'a Map<String, Value>, AuthoringFailure> {
    let Some(object) = value.as_object() else {
        return Err(invalid(
            shown,
            kind,
            id,
            &format!("{context} must be an object"),
        ));
    };
    for key in object.keys() {
        if !allowed.contains(&key.as_str()) {
            return Err(invalid(
                shown,
                kind,
                id,
                &format!("{context} has unknown member `{key}`"),
            ));
        }
    }
    Ok(object)
}

fn check_title(
    value: Option<&Value>,
    shown: &str,
    kind: &str,
    id: &str,
    context: &str,
) -> Result<(), AuthoringFailure> {
    let ok = value
        .and_then(Value::as_str)
        .map(str::trim)
        .is_some_and(|text| {
            let chars = text.chars().count();
            (1..=MAX_TITLE_CHARS).contains(&chars) && !text.chars().any(char::is_control)
        });
    if !ok {
        return Err(invalid(
            shown,
            kind,
            id,
            &format!(
                "{context} must be 1 to {MAX_TITLE_CHARS} characters without control characters"
            ),
        ));
    }
    Ok(())
}

fn validate_question_payload(
    shown: &str,
    id: &str,
    payload: &Value,
) -> Result<(), AuthoringFailure> {
    let parsed: norn_ir::QuestionIR = serde_json::from_value(payload.clone()).map_err(|err| {
        invalid(
            shown,
            "question",
            id,
            &format!("not a valid QuestionIR: {err}"),
        )
    })?;
    if parsed.name.trim().is_empty() {
        return Err(invalid(
            shown,
            "question",
            id,
            "QuestionIR `name` must be non-empty",
        ));
    }
    if parsed.metrics.is_empty() {
        return Err(invalid(
            shown,
            "question",
            id,
            "QuestionIR `aggregate` must select at least one metric",
        ));
    }
    Ok(())
}

fn validate_dashboard_origin(shown: &str, id: &str, value: &Value) -> Result<(), AuthoringFailure> {
    let object = check_closed_object(
        value,
        &["by", "proposal", "model", "request"],
        shown,
        "dashboard",
        id,
        "origin",
    )?;
    let by = object.get("by").and_then(Value::as_str);
    if !matches!(by, Some("studio" | "code" | "assistant")) {
        return Err(invalid(
            shown,
            "dashboard",
            id,
            "origin.by must be `studio`, `code`, or `assistant`",
        ));
    }
    for (key, limit) in [
        ("proposal", 128_usize),
        ("model", 128_usize),
        ("request", MAX_REQUEST_CHARS),
    ] {
        let Some(field) = object.get(key) else {
            continue;
        };
        let ok = field.as_str().is_some_and(|text| {
            !text.trim().is_empty()
                && text.chars().count() <= limit
                && !text.chars().any(|ch| ch.is_control() && ch != '\n')
        });
        if !ok || by != Some("assistant") {
            return Err(invalid(
                shown,
                "dashboard",
                id,
                &format!("origin.{key} is only valid on assistant origins with 1..={limit} chars"),
            ));
        }
    }
    Ok(())
}

fn validate_dashboard_tile_view(
    shown: &str,
    id: &str,
    ctx: &str,
    view_opt: Option<&Value>,
) -> Result<(), AuthoringFailure> {
    let Some(view_val) = view_opt else {
        return Err(invalid(
            shown,
            "dashboard",
            id,
            &format!("{ctx}.view is required"),
        ));
    };
    let view_obj = check_closed_object(
        view_val,
        &["kind", "chart"],
        shown,
        "dashboard",
        id,
        &format!("{ctx}.view"),
    )?;
    let view_kind = view_obj.get("kind").and_then(Value::as_str);
    let chart_val = view_obj.get("chart");
    let view_ok = match (view_kind, chart_val) {
        (Some("table" | "number" | "chart"), None) => true,
        (Some("chart"), Some(val)) => matches!(val.as_str(), Some("line" | "bar" | "area" | "pie")),
        _ => false,
    };
    if !view_ok {
        return Err(invalid(
            shown,
            "dashboard",
            id,
            &format!("{ctx}.view must be table, number, or chart (line|bar|area|pie)"),
        ));
    }
    Ok(())
}

fn validate_dashboard_tile_layout(
    shown: &str,
    id: &str,
    ctx: &str,
    layout_opt: Option<&Value>,
) -> Result<(), AuthoringFailure> {
    let Some(layout_val) = layout_opt else {
        return Err(invalid(
            shown,
            "dashboard",
            id,
            &format!("{ctx}.layout is required"),
        ));
    };
    let layout_obj = check_closed_object(
        layout_val,
        &["x", "y", "w", "h"],
        shown,
        "dashboard",
        id,
        &format!("{ctx}.layout"),
    )?;
    for (coord, min_val) in [("x", 0_u64), ("y", 0_u64), ("w", 1_u64), ("h", 1_u64)] {
        let coord_ok = layout_obj
            .get(coord)
            .and_then(Value::as_u64)
            .is_some_and(|val| (min_val..=MAX_LAYOUT_COORD).contains(&val));
        if !coord_ok {
            return Err(invalid(
                shown,
                "dashboard",
                id,
                &format!(
                    "{ctx}.layout.{coord} must be an integer in {min_val}..={MAX_LAYOUT_COORD}"
                ),
            ));
        }
    }
    Ok(())
}

fn validate_dashboard_tile(
    shown: &str,
    id: &str,
    index: usize,
    tile: &Value,
    tile_ids: &mut BTreeSet<String>,
) -> Result<(), AuthoringFailure> {
    let ctx = format!("tiles[{index}]");
    let object = check_closed_object(
        tile,
        &["id", "title", "question", "view", "layout"],
        shown,
        "dashboard",
        id,
        &ctx,
    )?;
    let Some(tile_id) = object
        .get("id")
        .and_then(Value::as_str)
        .filter(|val| !val.trim().is_empty() && val.len() <= 128)
    else {
        return Err(invalid(
            shown,
            "dashboard",
            id,
            &format!("{ctx}.id must be a non-blank string of at most 128 bytes"),
        ));
    };
    if !tile_ids.insert(tile_id.to_owned()) {
        return Err(invalid(
            shown,
            "dashboard",
            id,
            &format!("duplicate tile id `{tile_id}`"),
        ));
    }
    check_title(
        object.get("title"),
        shown,
        "dashboard",
        id,
        &format!("{ctx}.title"),
    )?;
    let Some(question) = object.get("question") else {
        return Err(invalid(
            shown,
            "dashboard",
            id,
            &format!("{ctx}.question is required"),
        ));
    };
    validate_question_payload(shown, id, question)?;
    validate_dashboard_tile_view(shown, id, &ctx, object.get("view"))?;
    validate_dashboard_tile_layout(shown, id, &ctx, object.get("layout"))
}

fn validate_dashboard_payload(
    shown: &str,
    id: &str,
    payload: &Value,
) -> Result<(), AuthoringFailure> {
    let object = check_closed_object(
        payload,
        &["title", "tiles", "origin"],
        shown,
        "dashboard",
        id,
        "payload",
    )?;
    check_title(object.get("title"), shown, "dashboard", id, "payload.title")?;
    if let Some(origin) = object.get("origin") {
        validate_dashboard_origin(shown, id, origin)?;
    }
    let Some(tiles) = object.get("tiles").and_then(Value::as_array) else {
        return Err(invalid(
            shown,
            "dashboard",
            id,
            "payload.tiles must be an array",
        ));
    };
    if tiles.len() > MAX_DASHBOARD_TILES {
        return Err(invalid(
            shown,
            "dashboard",
            id,
            &format!("payload.tiles exceeds {MAX_DASHBOARD_TILES} tile limit"),
        ));
    }
    let mut tile_ids = BTreeSet::new();
    for (idx, tile) in tiles.iter().enumerate() {
        validate_dashboard_tile(shown, id, idx, tile, &mut tile_ids)?;
    }
    Ok(())
}

fn validate_approval_when(shown: &str, id: &str, when: &Value) -> Result<(), AuthoringFailure> {
    let object = check_closed_object(
        when,
        &["property", "op", "value"],
        shown,
        "approval",
        id,
        "payload.when",
    )?;
    let valid_prop = object
        .get("property")
        .and_then(Value::as_str)
        .is_some_and(|prop| {
            prop.chars()
                .next()
                .is_some_and(|first| first.is_ascii_alphabetic() || first == '_')
                && prop
                    .chars()
                    .all(|ch| ch.is_ascii_alphanumeric() || ch == '_')
        });
    if !valid_prop {
        return Err(invalid(
            shown,
            "approval",
            id,
            "payload.when.property must be a valid identifier",
        ));
    }
    let valid_op = object
        .get("op")
        .and_then(Value::as_str)
        .is_some_and(|op| WHEN_OPS.contains(&op));
    if !valid_op {
        return Err(invalid(
            shown,
            "approval",
            id,
            "payload.when.op must be eq, ne, lt, le, gt, or ge",
        ));
    }
    let valid_val = object
        .get("value")
        .is_some_and(|val| val.is_string() || val.is_i64() || val.is_u64() || val.is_boolean());
    if !valid_val {
        return Err(invalid(
            shown,
            "approval",
            id,
            "payload.when.value must be a string, integer, or boolean",
        ));
    }
    Ok(())
}

fn validate_approval_decider(
    shown: &str,
    id: &str,
    decider_opt: Option<&Value>,
) -> Result<(), AuthoringFailure> {
    let Some(decider_val) = decider_opt else {
        return Err(invalid(
            shown,
            "approval",
            id,
            "payload.decider is required",
        ));
    };
    let decider = check_closed_object(
        decider_val,
        &["roles", "law"],
        shown,
        "approval",
        id,
        "payload.decider",
    )?;
    if decider.get("law").is_some_and(|val| !val.is_string()) {
        return Err(invalid(
            shown,
            "approval",
            id,
            "payload.decider.law must be a string",
        ));
    }
    let roles_ok = decider
        .get("roles")
        .and_then(Value::as_array)
        .is_some_and(|roles| {
            !roles.is_empty()
                && roles
                    .iter()
                    .all(|role| role.as_str().is_some_and(|text| !text.trim().is_empty()))
        });
    if !roles_ok {
        return Err(invalid(
            shown,
            "approval",
            id,
            "payload.decider.roles must be a non-empty array of role names",
        ));
    }
    Ok(())
}

fn validate_approval_payload(
    shown: &str,
    id: &str,
    payload: &Value,
) -> Result<(), AuthoringFailure> {
    let object = check_closed_object(payload, &APPROVAL_MEMBERS, shown, "approval", id, "payload")?;
    let action_ok = object
        .get("action")
        .and_then(Value::as_str)
        .is_some_and(|action| !action.trim().is_empty());
    if !action_ok {
        return Err(invalid(
            shown,
            "approval",
            id,
            "payload.action must be a non-empty action id",
        ));
    }
    validate_approval_decider(shown, id, object.get("decider"))?;
    let question_ok = object
        .get("question")
        .and_then(Value::as_str)
        .is_some_and(|text| {
            let trimmed = text.trim();
            !trimmed.is_empty() && text.chars().count() <= MAX_APPROVAL_QUESTION_CHARS
        });
    if !question_ok {
        return Err(invalid(
            shown,
            "approval",
            id,
            &format!("payload.question must be 1 to {MAX_APPROVAL_QUESTION_CHARS} characters"),
        ));
    }
    if !matches!(
        object.get("risk").and_then(Value::as_str),
        Some("low" | "medium" | "high")
    ) {
        return Err(invalid(
            shown,
            "approval",
            id,
            "payload.risk must be `low`, `medium`, or `high`",
        ));
    }
    if let Some(expires) = object.get("expiresInMs")
        && expires
            .as_u64()
            .is_none_or(|ms| ms == 0 || ms > MAX_APPROVAL_EXPIRES_MS)
    {
        return Err(invalid(
            shown,
            "approval",
            id,
            "payload.expiresInMs must be 1 ms to seven days",
        ));
    }
    if let Some(when) = object.get("when") {
        validate_approval_when(shown, id, when)?;
    }
    Ok(())
}

/// Validates a retained JSON definition (`kind` in `"question" | "dashboard" | "approval"`)
/// structurally against the real Rust IR / admission schemas.
pub(crate) fn validate_retained_definition(
    shown: &str,
    obj: &Map<String, Value>,
    kind: &str,
    seen_retained_ids: &mut BTreeSet<String>,
) -> Result<(), AuthoringFailure> {
    let Some(id) = obj
        .get("id")
        .and_then(Value::as_str)
        .filter(|val| !val.trim().is_empty())
    else {
        return Err(AuthoringFailure::Io(format!(
            "{shown}: retained {kind} definition must have a non-empty string `id`"
        )));
    };
    let Some(payload) = obj.get("payload").filter(|val| val.is_object()) else {
        return Err(AuthoringFailure::Io(format!(
            "{shown}: retained {kind} definition `{id}` must have an object `payload`"
        )));
    };
    match kind {
        "question" => validate_question_payload(shown, id, payload)?,
        "dashboard" => validate_dashboard_payload(shown, id, payload)?,
        "approval" => validate_approval_payload(shown, id, payload)?,
        _ => {
            return Err(AuthoringFailure::Io(format!(
                "{shown}: unsupported retained definition kind `{kind}`"
            )));
        }
    }
    if !seen_retained_ids.insert(id.to_owned()) {
        return Err(AuthoringFailure::Io(format!(
            "{shown}: duplicate retained definition id `{id}`"
        )));
    }
    Ok(())
}
