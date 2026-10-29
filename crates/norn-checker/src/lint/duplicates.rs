//! Exact-duplicate definitions (T8-LINT-1), an error: two object types, two
//! interfaces, or two shared property types under different names with the
//! same structure.
//!
//! The structure of a definition is its whole serialized form with the
//! naming and documentation members removed: the definition's own `name`
//! and `status`, and every `description` at any depth outside literal JSON
//! data (`default` values, `json-ext` schemas, extension slots). Two definitions are
//! exact duplicates when what remains is equal. An object type or interface
//! with no own property and no own link is never a duplicate (an empty
//! marker subtype is a classification, not a redefinition).
//!
//! A candidate is reported against the first definition with its structure:
//! another candidate earlier in the set or any base definition. Same-name
//! redefinitions are not duplicates here; they are the entity model's
//! `duplicate_name` findings.

use std::collections::BTreeMap;

use super::finding;
use super::model::{LintSet, Owned};
use crate::Diagnostic;

/// Diagnostic codes of the duplicate-definition rule.
pub mod codes {
    /// A definition has exactly the structure of another, differently named one.
    pub const DUPLICATE_DEFINITION: &str = "ontology.lint.duplicate_definition";
}

/// The duplicate-definition rule over every candidate object type,
/// interface, and shared property type.
#[must_use]
pub(crate) fn lint_duplicates(set: &LintSet<'_>) -> Vec<Diagnostic> {
    let mut out = Vec::new();
    duplicates_of(
        "object type",
        &set.object_types,
        |def| def.name.as_str(),
        |def| !def.properties.is_empty() || !def.links.is_empty(),
        |def| serde_json::to_value(def).ok(),
        &mut out,
    );
    duplicates_of(
        "interface",
        &set.interfaces,
        |def| def.name.as_str(),
        |def| !def.properties.is_empty() || !def.links.is_empty(),
        |def| serde_json::to_value(def).ok(),
        &mut out,
    );
    duplicates_of(
        "property type",
        &set.property_types,
        |def| def.name.as_str(),
        |_| true,
        |def| serde_json::to_value(def).ok(),
        &mut out,
    );
    out
}

fn duplicates_of<T>(
    what: &str,
    defs: &[Owned<'_, T>],
    name: impl Fn(&T) -> &str,
    eligible: impl Fn(&T) -> bool,
    serialize: impl Fn(&T) -> Option<serde_json::Value>,
    out: &mut Vec<Diagnostic>,
) {
    // Base definitions first, so a candidate that copies a base definition
    // is reported against it; then candidates in set order.
    let ordered = defs
        .iter()
        .filter(|owned| !owned.is_candidate())
        .chain(defs.iter().filter(|owned| owned.is_candidate()));
    let mut first_by_structure: BTreeMap<String, &Owned<'_, T>> = BTreeMap::new();
    for owned in ordered {
        if !eligible(&owned.def) {
            continue;
        }
        let Some(key) = serialize(&owned.def).and_then(structure_key) else {
            continue;
        };
        match first_by_structure.get(&key) {
            Some(first) if name(&first.def) != name(&owned.def) && owned.is_candidate() => {
                out.push(finding(
                    codes::DUPLICATE_DEFINITION,
                    format!(
                        "{what} {} has exactly the structure of {what} {} (artifact {}); reuse or extend it instead of redefining it",
                        name(&owned.def),
                        name(&first.def),
                        first.artifact.artifact_id
                    ),
                    &owned.artifact.artifact_id,
                    Some(owned.at("/name")),
                ));
            }
            Some(_) => {}
            None => {
                first_by_structure.insert(key, owned);
            }
        }
    }
}

/// The definition's structure as canonical JSON text (sorted keys, nulls
/// pruned: `norn_canonical`'s Legacy0 form, so the key does not depend on
/// `serde_json`'s map backend) with `name`, `status`, and every
/// `description` removed. `None` if it does not canonicalize.
fn structure_key(mut value: serde_json::Value) -> Option<String> {
    if let Some(object) = value.as_object_mut() {
        object.remove("name");
        object.remove("status");
    }
    strip_descriptions(&mut value);
    let bytes = norn_canonical::legacy_canonicalize_value(&value).ok()?;
    String::from_utf8(bytes).ok()
}

/// Remove every `description` member, at any depth, except inside literal
/// JSON the definition carries as data (`default` values, `json-ext`
/// `schema` documents, and `extensions` slots), where `description` is
/// content, not documentation.
pub(super) fn strip_descriptions(value: &mut serde_json::Value) {
    match value {
        serde_json::Value::Object(object) => {
            object.remove("description");
            for (key, child) in object.iter_mut() {
                if !DATA_MEMBERS.contains(&key.as_str()) {
                    strip_descriptions(child);
                }
            }
        }
        serde_json::Value::Array(items) => items.iter_mut().for_each(strip_descriptions),
        serde_json::Value::Null
        | serde_json::Value::Bool(_)
        | serde_json::Value::Number(_)
        | serde_json::Value::String(_) => {}
    }
}

/// Members whose value is literal JSON data rather than IR structure.
const DATA_MEMBERS: [&str; 3] = ["default", "schema", "extensions"];
