//! Typed source-version ordering and `FeedMode::Patch` projection helpers for
//! [`crate::pull`] (`FEED-MODE-1`).

use std::cmp::Ordering;

use norn_ir::{RuntimeValue, SourceVersion, VersionKind};
use serde_json::Value;

use crate::pull::{PullError, Row, Watermark};

/// Update a cursor feed's greatest seen version using typed [`SourceVersion`]
/// ordering.
pub(crate) fn update_greatest_version(
    greatest: &mut Option<String>,
    kind: VersionKind,
    rows: &[Row],
    shape: &impl Fn(String) -> PullError,
) -> Result<(), PullError> {
    let mut best = greatest
        .as_deref()
        .map(|raw| SourceVersion::parse(kind, raw))
        .transpose()
        .map_err(|e| shape(e.to_string()))?;
    for row in rows {
        let cand = SourceVersion::parse(kind, &row.version).map_err(|e| shape(e.to_string()))?;
        let gt = match &best {
            None => true,
            Some(cur) => cand.try_cmp(cur).map_err(|e| shape(e.to_string()))? == Ordering::Greater,
        };
        if gt {
            best = Some(cand);
        }
    }
    if let Some(best) = best {
        *greatest = Some(best.raw().to_owned());
    }
    Ok(())
}

fn cmp_keyset_id(a: &Value, b: &Value) -> Ordering {
    match (a, b) {
        (Value::Number(x), Value::Number(y)) => match (x.as_i64(), y.as_i64()) {
            (Some(x), Some(y)) => x.cmp(&y),
            _ => match (x.as_u64(), y.as_u64()) {
                (Some(x), Some(y)) => x.cmp(&y),
                _ => x.to_string().cmp(&y.to_string()),
            },
        },
        (Value::String(x), Value::String(y)) => x.cmp(y),
        _ => a.to_string().cmp(&b.to_string()),
    }
}

/// Verify monotonic `(version, id)` progression across the saved watermark and
/// every adjacent row in a keyset page.
pub(crate) fn check_keyset_order(
    kind: VersionKind,
    current: &Watermark,
    rows: &[Row],
    ids: &[Value],
    dataset: &impl Fn() -> String,
) -> Result<(), PullError> {
    let bound = |reason: String| PullError::Bound {
        dataset: dataset(),
        reason,
    };
    let mut prev = if current.version.is_empty() {
        None
    } else {
        Some((
            SourceVersion::parse(kind, &current.version).map_err(|e| bound(e.to_string()))?,
            &current.id,
        ))
    };
    for (row, id) in rows.iter().zip(ids) {
        let next = SourceVersion::parse(kind, &row.version).map_err(|e| bound(e.to_string()))?;
        if let Some((prev_v, prev_id)) = &prev {
            let ord = next.try_cmp(prev_v).map_err(|e| bound(e.to_string()))?;
            if ord == Ordering::Less
                || (ord == Ordering::Equal && cmp_keyset_id(id, prev_id) == Ordering::Less)
            {
                return Err(bound("the keyset position moved backward".into()));
            }
        }
        prev = Some((next, id));
    }
    Ok(())
}

/// Extract and validate one item's source version under `kind`.
pub(crate) fn extract_version(
    item: &RuntimeValue,
    pointer: &str,
    kind: VersionKind,
    shape: &impl Fn(String) -> PullError,
) -> Result<String, PullError> {
    let raw = match kind {
        VersionKind::Uint64 | VersionKind::Int64 => item.pointer(pointer).and_then(scalar_text),
        VersionKind::Lexicographical | VersionKind::Rfc3339 | VersionKind::Opaque => item
            .pointer(pointer)
            .and_then(RuntimeValue::as_str)
            .map(str::to_owned),
    }
    .ok_or_else(|| shape(format!("an item has no version text at {pointer}")))?;
    let parsed = SourceVersion::parse(kind, &raw).map_err(|error| shape(error.to_string()))?;
    Ok(parsed.raw().to_owned())
}

/// Patch-mode projection (`FEED-MODE-1`): absent JSON pointers return `None`,
/// explicit JSON `null` returns `Some(Null)`, and non-null non-scalar values
/// under `text:` fail closed instead of fabricating an explicit clear.
pub(crate) fn project_patch(
    item: &RuntimeValue,
    field: &str,
    pointer: &str,
    shape: &impl Fn(String) -> PullError,
) -> Result<Option<RuntimeValue>, PullError> {
    let Some(ptr) = pointer.strip_prefix("text:") else {
        return Ok(item.pointer(pointer).cloned());
    };
    let Some(value) = item.pointer(ptr) else {
        return Ok(None);
    };
    if value.is_null() {
        return Ok(Some(RuntimeValue::Null));
    }
    scalar_text(value)
        .map(RuntimeValue::String)
        .map(Some)
        .ok_or_else(|| {
            shape(format!(
                "patch text: field {field:?} at {ptr} must be non-empty text, number, or null"
            ))
        })
}

/// A scalar's text: a string as is, a number as its exact decimal.
pub(crate) fn scalar_text(value: &RuntimeValue) -> Option<String> {
    match value {
        RuntimeValue::String(text) => (!text.is_empty()).then(|| text.clone()),
        RuntimeValue::Number(number) => Some(number.to_string()),
        RuntimeValue::Null
        | RuntimeValue::Bool(_)
        | RuntimeValue::Array(_)
        | RuntimeValue::Object(_) => None,
    }
}
