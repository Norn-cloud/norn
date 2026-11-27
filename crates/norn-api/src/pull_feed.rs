//! Typed source-version ordering, mapping-contract binding, exact-decimal
//! template rendering, and `FeedMode::Patch` projection helpers for
//! [`crate::pull`] (`FEED-MODE-1`).

use std::cmp::Ordering;
use std::collections::BTreeMap;

use norn_ir::feed::{FEED_EXTENSION_KEY, FeedDeclaration};
use norn_ir::{RuntimeValue, SourceVersion, VersionKind};
use serde_json::Value;

use crate::pull::{PullError, PullProfile, Row, Watermark};

/// Bind a mapping's `norn.feed/1` declaration onto `profile` before projection
/// and pagination. Omitted connector settings (`None`) adopt the mapping's
/// contract; explicitly declared settings (`Some`) that contradict the mapping
/// refuse closed before the first request.
pub(crate) fn bind_feed_declaration(
    profile: &mut PullProfile,
    declaration: &FeedDeclaration,
) -> Result<(), PullError> {
    for feed in &mut profile.feeds {
        if let Some(contract) = declaration.feeds.get(&feed.dataset) {
            if let Some(explicit) = feed.mode
                && explicit != contract.mode
            {
                return Err(PullError::Profile(format!(
                    "feed {:?} mode {} contradicts mapping norn.feed/1 mode {}",
                    feed.dataset,
                    explicit.as_str(),
                    contract.mode.as_str()
                )));
            }
            if let Some(explicit) = feed.version_kind
                && explicit != contract.version_kind
            {
                return Err(PullError::Profile(format!(
                    "feed {:?} versionKind {} contradicts mapping norn.feed/1 versionKind {}",
                    feed.dataset,
                    explicit.as_str(),
                    contract.version_kind.as_str()
                )));
            }
            feed.mode = Some(contract.mode);
            feed.version_kind = Some(contract.version_kind);
        }
    }
    profile.validate()
}

/// Bind `extensions["norn.feed/1"]` from a mapping artifact's JSON payload when
/// present.
pub(crate) fn bind_mapping_feed_declaration(
    profile: &mut PullProfile,
    mapping_payload: &Value,
) -> Result<(), PullError> {
    let Some(raw) = mapping_payload
        .get("extensions")
        .and_then(|ext| ext.get(FEED_EXTENSION_KEY))
    else {
        return Ok(());
    };
    let declaration: FeedDeclaration = serde_json::from_value(raw.clone())
        .map_err(|e| PullError::Profile(format!("invalid {FEED_EXTENSION_KEY}: {e}")))?;
    bind_feed_declaration(profile, &declaration)
}

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

fn cmp_keyset_id(a: &RuntimeValue, b: &RuntimeValue) -> Ordering {
    match (a, b) {
        (RuntimeValue::Number(x), RuntimeValue::Number(y)) => x.cmp(y),
        (RuntimeValue::String(x), RuntimeValue::String(y)) => x.cmp(y),
        _ => serde_json::to_string(a)
            .unwrap_or_default()
            .cmp(&serde_json::to_string(b).unwrap_or_default()),
    }
}

/// Verify monotonic `(version, id)` progression across the saved watermark and
/// every adjacent row in a keyset page, and verify strict typed advance of the
/// page's final position past `current`.
pub(crate) fn check_keyset_order(
    kind: VersionKind,
    current: &Watermark,
    rows: &[Row],
    ids: &[RuntimeValue],
    dataset: &impl Fn() -> String,
) -> Result<(), PullError> {
    let bound = |reason: String| PullError::Bound {
        dataset: dataset(),
        reason,
    };
    let start = if current.version.is_empty() {
        None
    } else {
        Some((
            SourceVersion::parse(kind, &current.version).map_err(|e| bound(e.to_string()))?,
            &current.id,
        ))
    };
    let mut prev = start.clone();
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
    if let (Some((start_v, start_id)), Some((last_v, last_id))) = (&start, &prev) {
        let ord = last_v.try_cmp(start_v).map_err(|e| bound(e.to_string()))?;
        if ord == Ordering::Equal && cmp_keyset_id(last_id, start_id) != Ordering::Greater {
            return Err(bound("the keyset position did not advance".into()));
        }
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

/// Exact-decimal request template tokens for [`crate::pull::Pull::next_request`].
pub(crate) struct Tokens<'t> {
    pub(crate) version: &'t str,
    pub(crate) id: &'t RuntimeValue,
    pub(crate) cursor: &'t Value,
    pub(crate) size: u32,
}

impl Tokens<'_> {
    fn runtime_value(&self, name: &str) -> Option<RuntimeValue> {
        match name {
            "version" => Some(RuntimeValue::String(self.version.to_owned())),
            "id" => Some(self.id.clone()),
            "cursor" => Some(RuntimeValue::from_json(self.cursor.clone()).unwrap_or_default()),
            "size" => Some(RuntimeValue::Number(norn_ir::Decimal::from(self.size))),
            _ => None,
        }
    }

    fn text(&self, name: &str) -> Option<String> {
        if let Some(inner) = name.strip_prefix("json:") {
            return self
                .runtime_value(inner)
                .map(|value| serde_json::to_string(&value).unwrap_or_default());
        }
        self.runtime_value(name).map(|value| match value {
            RuntimeValue::String(text) => text,
            RuntimeValue::Null => String::new(),
            RuntimeValue::Number(number) => number.to_string(),
            RuntimeValue::Bool(_) | RuntimeValue::Array(_) | RuntimeValue::Object(_) => {
                serde_json::to_string(&value).unwrap_or_default()
            }
        })
    }

    fn write_json_str(&self, template: &str, out: &mut String) {
        if let Some(name) = template
            .strip_prefix("{{")
            .and_then(|rest| rest.strip_suffix("}}"))
            .filter(|name| !name.contains("{{") && !name.starts_with("json:"))
            && let Some(value) = self.runtime_value(name)
        {
            out.push_str(&serde_json::to_string(&value).unwrap_or_default());
            return;
        }
        out.push_str(&serde_json::to_string(&self.splice(template)).unwrap_or_default());
    }

    pub(crate) fn splice(&self, template: &str) -> String {
        let mut out = String::with_capacity(template.len());
        let mut rest = template;
        while let Some((before, after)) = rest.split_once("{{") {
            out.push_str(before);
            if let Some((text, tail)) = after
                .split_once("}}")
                .and_then(|(name, tail)| Some((self.text(name)?, tail)))
            {
                out.push_str(&text);
                rest = tail;
            } else {
                out.push_str("{{");
                rest = after;
            }
        }
        out.push_str(rest);
        out
    }

    fn write_json(&self, template: &Value, out: &mut String) {
        match template {
            Value::String(text) => self.write_json_str(text, out),
            Value::Array(items) => {
                out.push('[');
                for (index, item) in items.iter().enumerate() {
                    if index > 0 {
                        out.push(',');
                    }
                    self.write_json(item, out);
                }
                out.push(']');
            }
            Value::Object(entries) => {
                out.push('{');
                for (index, (key, value)) in entries.iter().enumerate() {
                    if index > 0 {
                        out.push(',');
                    }
                    out.push_str(&serde_json::to_string(key).unwrap_or_default());
                    out.push(':');
                    self.write_json(value, out);
                }
                out.push('}');
            }
            Value::Null | Value::Bool(_) | Value::Number(_) => {
                out.push_str(&serde_json::to_string(template).unwrap_or_default());
            }
        }
    }

    /// Render a JSON request body preserving both exact decimal keyset IDs and
    /// arbitrary fixed JSON literals (such as `1e100`).
    pub(crate) fn fill_json(&self, template: &Value) -> String {
        let mut out = String::new();
        self.write_json(template, &mut out);
        out
    }
}

/// `application/x-www-form-urlencoded` query text, in declared key order.
pub(crate) fn form_urlencoded_pairs(
    query: &BTreeMap<String, String>,
    tokens: &Tokens<'_>,
) -> String {
    let mut out = String::new();
    for (key, template) in query {
        if !out.is_empty() {
            out.push('&');
        }
        out.push_str(&crate::uploads::encode(key, false));
        out.push('=');
        out.push_str(&crate::uploads::encode(&tokens.splice(template), false));
    }
    out
}
