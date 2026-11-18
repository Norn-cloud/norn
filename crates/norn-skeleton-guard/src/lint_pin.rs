//! The lint set can only get stricter.
//!
//! `lint-pin.json` (next to this crate's manifest) records the floor:
//!
//! - `workspace_lints`: every lint of the root `[workspace.lints]`, with its
//!   level and other keys (`priority`, `check-cfg`). The root table and the
//!   guard's own `[lints]` (it is a separate workspace) must keep each pinned
//!   lint at its level or higher, with the same other keys. A lint the pin
//!   does not name may be added at any level.
//! - `clippy_toml`: the contents of `clippy.toml`. An `allow-*` switch may go
//!   from `true` to `false` (stricter) but not the other way, a `disallowed-*`
//!   list must keep every pinned entry and may gain more, and every other key
//!   must keep its pinned value.
//! - `inner_levels`: each `#![warn|deny|forbid(..)]` at the top of a file
//!   under `crates/`; the file must keep that lint at that level or higher.
//!
//! Every crate under `crates/` except this guard takes its lints from the
//! workspace: its `[lints]` table is exactly `workspace = true`.
//!
//! Crate- and module-level allows (`#![allow(..)]`) are the shrink-only
//! [`crate::ratchet::INNER_ALLOW`] ratchet. Changing `lint-pin.json` is a
//! gated edit (`/approve-gates`), so a floor is lowered only with approval.

use std::collections::BTreeMap;
use std::path::Path;

use crate::ratchet::lint_levels;
use crate::{BaselineError, ScanError, Violation, violation};

/// Lint levels, weakest first. `expect` reads as `allow`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Level {
    /// `allow` or `expect`.
    Allow,
    /// `warn`.
    Warn,
    /// `deny`.
    Deny,
    /// `forbid`.
    Forbid,
}

impl Level {
    /// Parses a Cargo or attribute level name.
    #[must_use]
    pub fn parse(name: &str) -> Option<Self> {
        match name {
            "allow" | "expect" => Some(Self::Allow),
            "warn" => Some(Self::Warn),
            "deny" => Some(Self::Deny),
            "forbid" => Some(Self::Forbid),
            _ => None,
        }
    }

    /// The level's name.
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Self::Allow => "allow",
            Self::Warn => "warn",
            Self::Deny => "deny",
            Self::Forbid => "forbid",
        }
    }
}

const PIN: &str = "crates/norn-skeleton-guard/lint-pin.json";

fn err(message: String) -> BaselineError {
    BaselineError(message)
}

fn read_toml(root: &Path, rel: &str) -> Result<toml::Table, BaselineError> {
    std::fs::read_to_string(root.join(rel))
        .map_err(|e| err(format!("read {rel}: {e}")))?
        .parse::<toml::Table>()
        .map_err(|e| err(format!("parse {rel}: {e}")))
}

fn to_json(value: &toml::Value) -> serde_json::Value {
    serde_json::to_value(value).unwrap_or(serde_json::Value::Null)
}

/// One lint entry as `{level, ..other keys}`; a bare level string becomes
/// `{level}`.
fn lint_entry(value: &toml::Value) -> serde_json::Value {
    match value {
        toml::Value::String(level) => serde_json::json!({ "level": level }),
        toml::Value::Integer(_)
        | toml::Value::Float(_)
        | toml::Value::Boolean(_)
        | toml::Value::Datetime(_)
        | toml::Value::Array(_)
        | toml::Value::Table(_) => to_json(value),
    }
}

/// A `[lints]`-shaped table as `{tool: {lint: {level, ..}}}`.
fn lints_json(table: Option<&toml::Value>) -> serde_json::Map<String, serde_json::Value> {
    let mut out = serde_json::Map::new();
    for (tool, lints) in table.and_then(toml::Value::as_table).into_iter().flatten() {
        let Some(lints) = lints.as_table() else {
            continue;
        };
        let entries: serde_json::Map<String, serde_json::Value> = lints
            .iter()
            .map(|(lint, value)| (lint.clone(), lint_entry(value)))
            .collect();
        out.insert(tool.clone(), serde_json::Value::Object(entries));
    }
    out
}

/// The top-level lint levels of each file under `crates/`: per path, per lint,
/// the highest `warn`/`deny`/`forbid` among the file's inner attributes.
///
/// # Errors
///
/// [`ScanError`] if a file cannot be walked, read or parsed.
pub fn inner_levels(root: &Path) -> Result<BTreeMap<String, BTreeMap<String, Level>>, ScanError> {
    let mut out: BTreeMap<String, BTreeMap<String, Level>> = BTreeMap::new();
    let walker = walkdir::WalkDir::new(root.join("crates"))
        .sort_by_file_name()
        .into_iter()
        .filter_entry(|entry| entry.file_name() != "target");
    for entry in walker {
        let entry = entry.map_err(ScanError::Walk)?;
        let path = entry.path();
        if !entry.file_type().is_file() || path.extension().is_none_or(|ext| ext != "rs") {
            continue;
        }
        let source =
            std::fs::read_to_string(path).map_err(|e| ScanError::Io(path.to_path_buf(), e))?;
        let file = syn::parse_file(&source).map_err(|e| ScanError::Parse(path.to_path_buf(), e))?;
        let rel = path
            .strip_prefix(root)
            .unwrap_or(path)
            .to_string_lossy()
            .replace('\\', "/");
        for attr in &file.attrs {
            if !matches!(attr.style, syn::AttrStyle::Inner(_)) {
                continue;
            }
            for (name, lint) in lint_levels(&attr.meta) {
                let Some(level) = Level::parse(name).filter(|level| *level > Level::Allow) else {
                    continue;
                };
                let slot = out
                    .entry(rel.clone())
                    .or_default()
                    .entry(lint)
                    .or_insert(level);
                *slot = (*slot).max(level);
            }
        }
    }
    Ok(out)
}

/// The pin that matches the tree at `root` exactly: what `lint-pin.json`
/// holds when nothing has changed.
///
/// # Errors
///
/// [`BaselineError`] if a manifest, `clippy.toml` or a source cannot be read.
pub fn current_pin(root: &Path) -> Result<serde_json::Value, BaselineError> {
    let manifest = read_toml(root, "Cargo.toml")?;
    let workspace_lints = lints_json(
        manifest
            .get("workspace")
            .and_then(toml::Value::as_table)
            .and_then(|workspace| workspace.get("lints")),
    );
    let clippy = read_toml(root, "clippy.toml")?;
    let levels = inner_levels(root).map_err(|e| err(e.to_string()))?;
    let inner: serde_json::Map<String, serde_json::Value> = levels
        .into_iter()
        .map(|(path, lints)| {
            let lints: serde_json::Map<String, serde_json::Value> = lints
                .into_iter()
                .map(|(lint, level)| (lint, serde_json::Value::from(level.name())))
                .collect();
            (path, serde_json::Value::Object(lints))
        })
        .collect();
    Ok(serde_json::json!({
        "workspace_lints": workspace_lints,
        "clippy_toml": to_json(&toml::Value::Table(clippy)),
        "inner_levels": inner,
    }))
}

fn level_of(entry: &serde_json::Value) -> Option<Level> {
    entry
        .get("level")
        .and_then(serde_json::Value::as_str)
        .and_then(Level::parse)
}

fn without_level(entry: &serde_json::Value) -> serde_json::Value {
    let mut entry = entry.clone();
    if let Some(map) = entry.as_object_mut() {
        map.remove("level");
    }
    entry
}

fn object<'a>(
    value: &'a serde_json::Value,
    key: &str,
) -> Result<&'a serde_json::Map<String, serde_json::Value>, BaselineError> {
    value
        .get(key)
        .and_then(serde_json::Value::as_object)
        .ok_or_else(|| err(format!("lint-pin.json: missing object `{key}`")))
}

/// Holds one `[lints]` table (`label`) to the pinned workspace lints.
fn check_lints(
    label: &str,
    pinned: &serde_json::Map<String, serde_json::Value>,
    found: &serde_json::Map<String, serde_json::Value>,
    out: &mut Vec<Violation>,
) {
    let empty = serde_json::Map::new();
    for (tool, lints) in pinned {
        let lints = lints.as_object().unwrap_or(&empty);
        let now = found
            .get(tool)
            .and_then(serde_json::Value::as_object)
            .unwrap_or(&empty);
        for (lint, entry) in lints {
            let name = format!("{tool}::{lint}");
            let Some(current) = now.get(lint) else {
                out.push(violation(
                    "lint-removed",
                    format!("{label} no longer sets `{name}` (pinned at {}). Put it back; a lint is never removed from the set.", entry.get("level").unwrap_or(&serde_json::Value::Null)),
                ));
                continue;
            };
            let (Some(want), Some(have)) = (level_of(entry), level_of(current)) else {
                out.push(violation(
                    "lint-pin-file",
                    format!("{label} `{name}`: unreadable level ({current}, pinned {entry})"),
                ));
                continue;
            };
            if have < want {
                out.push(violation(
                    "lint-lowered",
                    format!(
                        "{label} lowers `{name}` from {} to {}. Restore {} and fix what the lint reports; levels only go up.",
                        want.name(),
                        have.name(),
                        want.name()
                    ),
                ));
            }
            if without_level(current) != without_level(entry) {
                out.push(violation(
                    "lint-changed",
                    format!(
                        "{label} `{name}` changed {} to {}; only its level may rise. Restore the pinned keys.",
                        without_level(entry),
                        without_level(current)
                    ),
                ));
            }
        }
    }
}

fn disallowed_key(item: &serde_json::Value) -> String {
    item.get("path")
        .and_then(serde_json::Value::as_str)
        .map_or_else(|| item.to_string(), str::to_owned)
}

fn check_clippy_toml(
    pinned: &serde_json::Map<String, serde_json::Value>,
    found: &serde_json::Map<String, serde_json::Value>,
    out: &mut Vec<Violation>,
) {
    let keys: std::collections::BTreeSet<&String> = pinned.keys().chain(found.keys()).collect();
    for key in keys {
        let want = pinned.get(key);
        let have = found.get(key);
        if key.starts_with("allow-") {
            let on = |value: Option<&serde_json::Value>| {
                value.and_then(serde_json::Value::as_bool).unwrap_or(false)
            };
            if on(have) && !on(want) {
                out.push(violation(
                    "clippy-toml-weakened",
                    format!("clippy.toml turns on `{key}`, which the pin keeps off. Remove it; fix the code the lint reports instead."),
                ));
            }
        } else if key.starts_with("disallowed-") {
            let entries =
                |value: Option<&serde_json::Value>| -> std::collections::BTreeSet<String> {
                    value
                        .and_then(serde_json::Value::as_array)
                        .into_iter()
                        .flatten()
                        .map(disallowed_key)
                        .collect()
                };
            let now = entries(have);
            for entry in entries(want).difference(&now) {
                out.push(violation(
                    "clippy-toml-weakened",
                    format!("clippy.toml `{key}` no longer lists `{entry}`. Put it back; the ban list only grows."),
                ));
            }
        } else if want != have {
            out.push(violation(
                "clippy-toml-changed",
                format!(
                    "clippy.toml `{key}` is {} but the pin says {}. Restore it, or change {PIN} in a gated PR if it is stricter.",
                    have.map_or_else(|| "unset".to_owned(), ToString::to_string),
                    want.map_or_else(|| "unset".to_owned(), ToString::to_string)
                ),
            ));
        }
    }
}

fn check_member_lints(root: &Path, out: &mut Vec<Violation>) -> Result<(), BaselineError> {
    let crates =
        std::fs::read_dir(root.join("crates")).map_err(|e| err(format!("read crates/: {e}")))?;
    let mut manifests: Vec<String> = crates
        .filter_map(Result::ok)
        .filter(|entry| entry.file_name() != "norn-skeleton-guard")
        .map(|entry| format!("crates/{}/Cargo.toml", entry.file_name().to_string_lossy()))
        .filter(|rel| root.join(rel).is_file())
        .collect();
    manifests.sort();
    for rel in manifests {
        let manifest = read_toml(root, &rel)?;
        let mut expected = toml::Table::new();
        expected.insert("workspace".to_owned(), toml::Value::Boolean(true));
        if manifest.get("lints").and_then(toml::Value::as_table) != Some(&expected) {
            out.push(violation(
                "crate-lints",
                format!("{rel}: `[lints]` must be exactly `workspace = true`. Take the workspace lints and allow a lint on the item that needs it."),
            ));
        }
    }
    Ok(())
}

/// Checks the tree at `root` against `lint-pin.json` (see the module docs).
///
/// # Errors
///
/// [`BaselineError`] if the pin, a manifest, `clippy.toml` or a source
/// cannot be read.
pub fn check_lint_pin(root: &Path, pin_json: &str) -> Result<Vec<Violation>, BaselineError> {
    let pin: serde_json::Value =
        serde_json::from_str(pin_json).map_err(|e| err(format!("lint-pin.json: {e}")))?;
    let current = current_pin(root)?;
    let mut out = Vec::new();
    let pinned_lints = object(&pin, "workspace_lints")?;
    check_lints(
        "Cargo.toml [workspace.lints]",
        pinned_lints,
        object(&current, "workspace_lints")?,
        &mut out,
    );
    let guard = read_toml(root, "crates/norn-skeleton-guard/Cargo.toml")?;
    check_lints(
        "crates/norn-skeleton-guard/Cargo.toml [lints]",
        pinned_lints,
        &lints_json(guard.get("lints")),
        &mut out,
    );
    check_clippy_toml(
        object(&pin, "clippy_toml")?,
        object(&current, "clippy_toml")?,
        &mut out,
    );
    let now = object(&current, "inner_levels")?;
    let empty = serde_json::Map::new();
    for (path, lints) in object(&pin, "inner_levels")? {
        let found = now
            .get(path)
            .and_then(serde_json::Value::as_object)
            .unwrap_or(&empty);
        for (lint, level) in lints.as_object().unwrap_or(&empty) {
            let want = level.as_str().and_then(Level::parse);
            let have = found
                .get(lint)
                .and_then(serde_json::Value::as_str)
                .and_then(Level::parse);
            if have < want {
                out.push(violation(
                    "inner-level-lowered",
                    format!(
                        "{path}: `#![{}({lint})]` is gone or lowered to {}. Restore it at the top of the file.",
                        want.map_or("?", Level::name),
                        have.map_or("nothing", Level::name)
                    ),
                ));
            }
        }
    }
    check_member_lints(root, &mut out)?;
    Ok(out)
}
