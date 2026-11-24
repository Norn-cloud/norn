//! VIEW-DEF-1: retained `view` definitions — declarative saved views over one
//! declared object type.
//!
//! A saved view is a pure projection over an admitted pack with zero business
//! authority: it names a title, optional description and workspace `space` to
//! pin into, an `objectType`, a `filter` and `sort` checked by the grid's own
//! parser (`object_query::parse`), and visible `columns` (declared properties
//! of the type in display order). At runtime the cell still decides every read
//! and every action offer (`GET /v1/actions`) and enforces laws and
//! authorization on every call.

use std::collections::BTreeSet;

use norn_cell::engine::SqlEngine;
use serde_json::Value;

use crate::definition_routes::{MAX_TITLE_CHARS, codes};
use crate::host::DevHost;
use crate::refusal::{Refusal, RefusalDiagnostic};

/// Maximum characters in a saved view's optional Space name.
pub const MAX_SPACE_CHARS: usize = 60;
/// Maximum characters in a saved view's optional description.
pub const MAX_DESCRIPTION_CHARS: usize = 280;
/// Maximum columns a saved view may list.
pub const MAX_VIEW_COLUMNS: usize = 64;

const MEMBERS: [&str; 7] = [
    "title",
    "description",
    "space",
    "objectType",
    "filter",
    "sort",
    "columns",
];

fn pointer_token(key: &str) -> String {
    key.replace('~', "~0").replace('/', "~1")
}

/// Check a retained `kind: "view"` payload against the cell's active model.
///
/// # Errors
/// A host failure reading the cell's model.
pub(crate) fn check<E: SqlEngine>(
    host: &DevHost<E>,
    value: &Value,
    findings: &mut Vec<RefusalDiagnostic>,
) -> Result<(), Refusal> {
    let Some(object) = value.as_object() else {
        findings.push(RefusalDiagnostic::error(
            codes::INVALID_PAYLOAD,
            "expected an object",
            "",
        ));
        return Ok(());
    };
    for key in object.keys().filter(|key| !MEMBERS.contains(&key.as_str())) {
        findings.push(RefusalDiagnostic::error(
            codes::INVALID_PAYLOAD,
            format!("unknown member {key:?}"),
            format!("/{}", pointer_token(key)),
        ));
    }
    check_title(object.get("title"), "/title", findings);
    check_optional_text(
        object.get("description"),
        "/description",
        (0, MAX_DESCRIPTION_CHARS),
        "description",
        findings,
    );
    check_optional_text(
        object.get("space"),
        "/space",
        (1, MAX_SPACE_CHARS),
        "space",
        findings,
    );
    let object_type_name = object.get("objectType").and_then(Value::as_str);
    let declared = match object_type_name {
        Some(object_type) => {
            let cell = host.cell()?;
            let reader = cell.store().read();
            if reader.model().has_type(object_type) {
                Some(
                    reader
                        .model()
                        .effective_property_types(object_type)
                        .map_err(Refusal::host)?,
                )
            } else {
                None
            }
        }
        None => None,
    };
    let Some(properties) = declared else {
        findings.push(RefusalDiagnostic::error(
            codes::INVALID_PAYLOAD,
            "objectType names a declared object type",
            "/objectType",
        ));
        return Ok(());
    };
    let sort = match object.get("sort") {
        None => None,
        Some(Value::String(text)) => Some(text.clone()),
        Some(_) => {
            findings.push(RefusalDiagnostic::error(
                codes::INVALID_PAYLOAD,
                "sort is the grid's `property:asc|desc` list, comma separated",
                "/sort",
            ));
            None
        }
    };
    let query = crate::object_query::ObjectQueryParams {
        filter: object.get("filter").map(Value::to_string),
        sort,
        ..crate::object_query::ObjectQueryParams::default()
    };
    if let Err(refusal) = crate::object_query::parse(&query, &properties) {
        findings.extend(refusal.diagnostics);
    }
    check_columns(object.get("columns"), &properties, findings);
    Ok(())
}

/// Plain-words restatement of an admitted saved view payload.
///
/// # Errors
/// The canonical JSON cannot be parsed as a view object.
pub fn restate(canonical: &str) -> Result<String, String> {
    let value: Value = serde_json::from_str(canonical).map_err(|error| error.to_string())?;
    let title = value
        .get("title")
        .and_then(Value::as_str)
        .map_or("Untitled view", str::trim);
    let object_type = value
        .get("objectType")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    let mut parts = vec![format!("Saved view \"{title}\" over {object_type}")];
    if let Some(space) = value
        .get("space")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        parts.push(format!("pinned in Space \"{space}\""));
    }
    if let Some(clauses) = value
        .pointer("/filter/all")
        .and_then(Value::as_array)
        .filter(|c| !c.is_empty())
    {
        parts.push(format!("with {} filter condition(s)", clauses.len()));
    }
    Ok(format!("{}.", parts.join(", ")))
}

fn check_title(value: Option<&Value>, at: &str, findings: &mut Vec<RefusalDiagnostic>) {
    let ok = value
        .and_then(Value::as_str)
        .map(str::trim)
        .is_some_and(|t| {
            let chars = t.chars().count();
            (1..=MAX_TITLE_CHARS).contains(&chars) && !t.chars().any(char::is_control)
        });
    if !ok {
        findings.push(RefusalDiagnostic::error(
            codes::TILE_TITLE_INVALID,
            format!(
                "a title is 1 to {MAX_TITLE_CHARS} characters after trimming, without control characters or newlines"
            ),
            at,
        ));
    }
}

fn check_optional_text(
    value: Option<&Value>,
    at: &str,
    (min, max): (usize, usize),
    label: &str,
    findings: &mut Vec<RefusalDiagnostic>,
) {
    let Some(value) = value else {
        return;
    };
    let ok = value.as_str().map(str::trim).is_some_and(|text| {
        let chars = text.chars().count();
        (min..=max).contains(&chars) && !text.chars().any(char::is_control)
    });
    if !ok {
        findings.push(RefusalDiagnostic::error(
            codes::INVALID_PAYLOAD,
            format!(
                "{label} is {min} to {max} characters after trimming, without control characters"
            ),
            at,
        ));
    }
}

fn check_columns(
    value: Option<&Value>,
    properties: &[(String, Option<norn_ir::PropertyType>)],
    findings: &mut Vec<RefusalDiagnostic>,
) {
    match value {
        None => {}
        Some(Value::Array(columns)) => {
            if columns.len() > MAX_VIEW_COLUMNS {
                findings.push(RefusalDiagnostic::error(
                    codes::INVALID_PAYLOAD,
                    format!("a view lists at most {MAX_VIEW_COLUMNS} columns"),
                    "/columns",
                ));
                return;
            }
            let mut seen = BTreeSet::new();
            for (index, column) in columns.iter().enumerate() {
                match column.as_str() {
                    Some(id) if properties.iter().any(|(name, _)| name == id) => {
                        if !seen.insert(id) {
                            findings.push(RefusalDiagnostic::error(
                                codes::INVALID_PAYLOAD,
                                format!("column {id:?} is listed more than once"),
                                format!("/columns/{index}"),
                            ));
                        }
                    }
                    _ => findings.push(RefusalDiagnostic::error(
                        codes::INVALID_PAYLOAD,
                        "a column is a declared property of the view's type",
                        format!("/columns/{index}"),
                    )),
                }
            }
        }
        Some(_) => findings.push(RefusalDiagnostic::error(
            codes::INVALID_PAYLOAD,
            "columns is a list of property ids",
            "/columns",
        )),
    }
}
