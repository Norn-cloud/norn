//! Norn object types → a Frappe projection bundle (#1085 §2, unit B3).
//!
//! A pure, one-way projection. For each object type it emits a standard,
//! virtual `DocType` in the `Norn Generated` module, a Workflow for its first
//! lifecycle and the `DocPerm` rows that let Frappe show what a role may do.
//! The generic bridge app (`bridges/frappe/norn_bridge`) applies the bundle
//! at `bench migrate`; no Python is written per app and nobody edits the
//! generated records by hand.
//!
//! Every projected permission is UI affordance only. The cell decides every
//! read and write; a Frappe role that sees a button still gets the cell's
//! refusal.
//!
//! - Properties become read-only fields: Norn writes go through actions, not
//!   field edits. A single-property primary key is the document `name`.
//! - Each lifecycle transition that a `fire_transition` action fires, and that
//!   an access rule grants to a role, becomes one Workflow Transition per
//!   source state and role. A transition no person may send gets no row.
//! - The `reader_roles` (the host's membership roles, strongest first) each
//!   become a Frappe role with read `DocPerm` on every generated `DocType`, and
//!   the bundle's `roleMap` in the same order, so the bridge asserts the
//!   strongest role a Frappe user holds.
//!
//! The bundle is canonical JSON with a domain-separated content hash
//! (`bundleHash`), so re-generating unchanged definitions gives the same
//! bytes and hash.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use norn_ir::access::AccessCondition;
use norn_ir::canonical::domain_hash_hex;
use norn_ir::{AccessIr, ActionEffectProgram, ActionRoot, JsonValue, ObjectTypeDef, PropertyType};

/// The bundle format the bridge accepts.
pub const FRAPPE_BUNDLE_FORMAT: &str = "norn.frappe-bundle/1";
/// Domain separator of `bundleHash`: SHA-256 over this, then the canonical
/// JSON of the bundle without `bundleHash`.
pub const FRAPPE_BUNDLE_DOMAIN: &[u8] = b"norn.frappe-bundle/1\n";
/// The Frappe module every generated `DocType` belongs to.
pub const FRAPPE_MODULE: &str = "Norn Generated";

/// Fieldnames Frappe reserves on every document; a property with one of
/// these ids is projected as `norn_<id>`.
const RESERVED_FIELDNAMES: &[&str] = &[
    "name",
    "owner",
    "creation",
    "modified",
    "modified_by",
    "parent",
    "parentfield",
    "parenttype",
    "idx",
    "docstatus",
    "doctype",
];

/// Properties used as the document title, in preference order.
const TITLE_PROPERTIES: &[&str] = &["label", "title", "display_name", "name"];

/// How many non-state properties appear as list columns.
const LIST_COLUMNS: usize = 3;

/// What the projection reads.
#[derive(Debug, Clone, Copy)]
pub struct FrappeProjectionInput<'a> {
    /// The object types to project, in bundle order.
    pub object_types: &'a [&'a ObjectTypeDef],
    /// Admitted actions; `fire_transition` effects name lifecycle transitions.
    pub actions: &'a [&'a ActionRoot],
    /// Admitted access policies; role conditions name who may fire an action.
    pub access: &'a [&'a AccessIr],
    /// The host's membership roles that may read, strongest first.
    pub reader_roles: &'a [&'a str],
}

/// Why a bundle cannot be projected.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FrappeProjectionError {
    /// Two object types project to the same `DocType` name.
    DuplicateDocType(String),
    /// Two transitions of one lifecycle share a source and a target state, so
    /// a Workflow save cannot tell which Norn action it means.
    AmbiguousTransition {
        /// The object type.
        object_type: String,
        /// The shared source state.
        from: String,
        /// The shared target state.
        to: String,
    },
    /// The bundle could not be canonicalized.
    Canonical(String),
}

impl fmt::Display for FrappeProjectionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::DuplicateDocType(name) => {
                write!(f, "two object types project to the DocType {name:?}")
            }
            Self::AmbiguousTransition {
                object_type,
                from,
                to,
            } => write!(
                f,
                "{object_type}: two transitions go from {from:?} to {to:?}; a Workflow save cannot tell them apart"
            ),
            Self::Canonical(error) => write!(f, "bundle canonicalization failed: {error}"),
        }
    }
}

impl std::error::Error for FrappeProjectionError {}

/// The `DocType` name of an object type: its namespace and local name in
/// title case, words split at `.`, `_`, `-` and lower-to-upper case changes
/// (`core.analytics.Restatement` → `Core Analytics Restatement`). The
/// namespace keeps it apart from native `DocTypes` such as `Item`.
#[must_use]
pub fn doctype_name(object_type: &str) -> String {
    words(object_type).join(" ")
}

/// The Frappe role of a Norn role (`AnalyticsReviewer` → `Norn Analytics Reviewer`).
#[must_use]
pub fn frappe_role(role: &str) -> String {
    format!("Norn {}", words(role).join(" "))
}

fn words(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    for part in text
        .split(['.', '_', '-', ' '])
        .filter(|part| !part.is_empty())
    {
        let mut word = String::new();
        let mut previous_lower = false;
        for character in part.chars() {
            if character.is_uppercase() && previous_lower && !word.is_empty() {
                out.push(capitalize(&word));
                word.clear();
            }
            previous_lower = character.is_lowercase() || character.is_ascii_digit();
            word.push(character);
        }
        if !word.is_empty() {
            out.push(capitalize(&word));
        }
    }
    out
}

fn capitalize(word: &str) -> String {
    let mut characters = word.chars();
    characters.next().map_or_else(String::new, |first| {
        first.to_uppercase().chain(characters).collect()
    })
}

fn fieldname(property: &str) -> String {
    if RESERVED_FIELDNAMES.contains(&property) {
        format!("norn_{property}")
    } else {
        property.to_owned()
    }
}

fn string(value: impl Into<String>) -> JsonValue {
    JsonValue::String(value.into())
}

fn int(value: u8) -> JsonValue {
    JsonValue::Number(f64::from(value))
}

fn object<const N: usize>(members: [(&str, JsonValue); N]) -> JsonValue {
    JsonValue::Object(
        members
            .into_iter()
            .map(|(name, value)| (name.to_owned(), value))
            .collect(),
    )
}

fn strings<'a>(values: impl IntoIterator<Item = &'a String>) -> JsonValue {
    JsonValue::Array(values.into_iter().cloned().map(JsonValue::String).collect())
}

/// The Frappe fieldtype and options of a property, or `None` when it has no
/// stored value to show (a decimal binding). Decimals stay text so their
/// exact digits are shown; Frappe's numeric fields are floats.
fn fieldtype(property_type: &PropertyType) -> Option<(&'static str, Option<String>)> {
    Some(match property_type {
        PropertyType::Integer { .. } => ("Int", None),
        PropertyType::Boolean {} => ("Check", None),
        PropertyType::Date {} => ("Date", None),
        PropertyType::Enum { values } => (
            "Select",
            Some(
                values
                    .iter()
                    .map(|value| value.value.as_str())
                    .collect::<Vec<_>>()
                    .join("\n"),
            ),
        ),
        PropertyType::Record { .. } | PropertyType::JsonExt { .. } => ("JSON", None),
        PropertyType::Text { .. }
        | PropertyType::Decimal { .. }
        | PropertyType::Timestamp { .. }
        | PropertyType::Duration {}
        | PropertyType::Ref { .. } => ("Data", None),
        PropertyType::DecimalBinding { .. } => return None,
    })
}

/// Norn roles granted each action, from the access rules on `object_type`.
fn action_roles(access: &[&AccessIr], object_type: &str) -> BTreeMap<String, BTreeSet<String>> {
    let mut out: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for rule in access
        .iter()
        .flat_map(|policy| &policy.rules)
        .filter(|rule| rule.resource_type.as_str() == object_type)
    {
        let roles: BTreeSet<String> = rule
            .all_of
            .iter()
            .filter_map(|condition| {
                if let AccessCondition::Role(role) = condition {
                    Some(role.any_of.iter().cloned())
                } else {
                    None
                }
            })
            .flatten()
            .collect();
        for action in &rule.actions {
            out.entry(action.clone())
                .or_default()
                .extend(roles.iter().cloned());
        }
    }
    out
}

/// The action that fires `(lifecycle, transition)` on `object_type`, if any.
fn transition_actions<'a>(
    actions: &[&'a ActionRoot],
    object_type: &str,
) -> BTreeMap<(String, String), &'a str> {
    let mut out = BTreeMap::new();
    for action in actions {
        if let Some(ActionEffectProgram::FireTransition {
            object_kind,
            lifecycle,
            transition,
            ..
        }) = &action.effect_program
            && object_kind == object_type
        {
            out.entry((
                lifecycle.as_str().to_owned(),
                transition.as_str().to_owned(),
            ))
            .or_insert(action.action_id.as_str());
        }
    }
    out
}

struct Projected {
    doctype: JsonValue,
    workflow: Option<JsonValue>,
    entry: JsonValue,
    roles: BTreeSet<String>,
    states: BTreeSet<String>,
    actions: BTreeSet<String>,
}

#[allow(
    clippy::too_many_lines,
    reason = "one projection maps each part of an object type to its Frappe record"
)]
fn project_type(
    def: &ObjectTypeDef,
    input: &FrappeProjectionInput<'_>,
) -> Result<Projected, FrappeProjectionError> {
    let type_name = def.name.as_str();
    let name = doctype_name(type_name);
    let lifecycle = def.lifecycles.first();
    let state_property = lifecycle.map(|lifecycle| lifecycle.property.as_str());
    let id_property = def
        .identity
        .iter()
        .find(|key| key.properties.len() == 1)
        .and_then(|key| key.properties.first())
        .map(norn_ir::PropertyId::as_str)
        .filter(|id| {
            def.properties
                .iter()
                .any(|property| property.id.as_str() == *id)
        });

    let mut fields = Vec::new();
    let mut field_map = BTreeMap::new();
    let mut enums = BTreeMap::new();
    let mut listed = 0;
    for property in &def.properties {
        let id = property.id.as_str();
        if Some(id) == id_property {
            continue;
        }
        let Some((kind, options)) = fieldtype(&property.property_type) else {
            continue;
        };
        let field = fieldname(id);
        let is_state = Some(id) == state_property;
        let mut doc: BTreeMap<String, JsonValue> = BTreeMap::from([
            ("doctype".to_owned(), string("DocField")),
            ("fieldname".to_owned(), string(&field)),
            ("fieldtype".to_owned(), string(kind)),
            ("label".to_owned(), string(words(id).join(" "))),
            ("read_only".to_owned(), int(1)),
        ]);
        if let Some(options) = options {
            doc.insert("options".to_owned(), string(options));
        }
        if let Some(description) = &property.description {
            doc.insert("description".to_owned(), string(description));
        }
        if is_state || listed < LIST_COLUMNS {
            doc.insert("in_list_view".to_owned(), int(1));
            if !is_state {
                listed = listed.saturating_add(1);
            }
        }
        if let PropertyType::Enum { values } = &property.property_type {
            doc.insert("in_standard_filter".to_owned(), int(1));
            enums.insert(
                field.clone(),
                JsonValue::Array(values.iter().map(|value| string(&value.value)).collect()),
            );
        }
        fields.push(JsonValue::Object(doc));
        field_map.insert(field, string(id));
    }
    let title_field = TITLE_PROPERTIES
        .iter()
        .map(|property| fieldname(property))
        .find(|field| field_map.contains_key(field));

    let readers: Vec<String> = input
        .reader_roles
        .iter()
        .map(|role| frappe_role(role))
        .collect();
    let mut roles: BTreeSet<String> = readers.iter().cloned().collect();
    let mut permissions = BTreeMap::new();
    for role in &readers {
        permissions.insert(role.clone(), [("read", 1), ("report", 1)].to_vec());
    }

    let mut workflow = None;
    let mut transitions_map = BTreeMap::new();
    let mut states = BTreeSet::new();
    let mut labels = BTreeSet::new();
    if let Some(lifecycle) = lifecycle {
        let fired = transition_actions(input.actions, type_name);
        let granted = action_roles(input.access, type_name);
        let property = lifecycle.property.as_str();
        let state_values: Vec<String> = def
            .properties
            .iter()
            .find(|candidate| candidate.id.as_str() == property)
            .and_then(|candidate| {
                if let PropertyType::Enum { values } = &candidate.property_type {
                    Some(values.iter().map(|value| value.value.clone()).collect())
                } else {
                    None
                }
            })
            .unwrap_or_default();
        let editor = readers.first().cloned().unwrap_or_default();
        let mut rows = Vec::new();
        let mut seen = BTreeSet::new();
        for transition in &lifecycle.transitions {
            let id = transition.id.as_str();
            let action = fired.get(&(property.to_owned(), id.to_owned())).copied();
            let allowed: BTreeSet<String> = action
                .and_then(|action| granted.get(action))
                .map(|roles| roles.iter().map(|role| frappe_role(role)).collect())
                .unwrap_or_default();
            let label = words(id).join(" ");
            for from in &transition.from {
                if !seen.insert((from.clone(), transition.to.clone())) {
                    return Err(FrappeProjectionError::AmbiguousTransition {
                        object_type: type_name.to_owned(),
                        from: from.clone(),
                        to: transition.to.clone(),
                    });
                }
                if let Some(action) = action {
                    transitions_map.insert(format!("{from}->{}", transition.to), string(action));
                }
                for role in &allowed {
                    rows.push(object([
                        ("action", string(&label)),
                        ("allow_self_approval", int(1)),
                        ("allowed", string(role)),
                        ("doctype", string("Workflow Transition")),
                        ("next_state", string(&transition.to)),
                        ("state", string(from)),
                    ]));
                }
            }
            if !allowed.is_empty() {
                labels.insert(label);
            }
            for role in allowed {
                roles.insert(role.clone());
                permissions
                    .entry(role)
                    .or_insert_with(Vec::new)
                    .extend([("read", 1), ("write", 1)]);
            }
        }
        states.extend(state_values.iter().cloned());
        workflow = Some(object([
            ("document_type", string(&name)),
            ("is_active", int(1)),
            ("override_status", int(1)),
            ("send_email_alert", int(0)),
            (
                "states",
                JsonValue::Array(
                    state_values
                        .iter()
                        .map(|state| {
                            object([
                                ("allow_edit", string(&editor)),
                                ("doc_status", string("0")),
                                ("doctype", string("Workflow Document State")),
                                ("state", string(state)),
                            ])
                        })
                        .collect(),
                ),
            ),
            ("transitions", JsonValue::Array(rows)),
            (
                "workflow_name",
                string(format!("{name} {property} lifecycle")),
            ),
            ("workflow_state_field", string(fieldname(property))),
        ]));
    }

    let permissions = JsonValue::Array(
        permissions
            .into_iter()
            .map(|(role, flags)| {
                let mut doc: BTreeMap<String, JsonValue> = BTreeMap::from([
                    ("doctype".to_owned(), string("DocPerm")),
                    ("permlevel".to_owned(), int(0)),
                    ("role".to_owned(), string(role)),
                ]);
                for (flag, value) in flags {
                    doc.insert(flag.to_owned(), int(value));
                }
                JsonValue::Object(doc)
            })
            .collect(),
    );
    let description = def.description.as_deref().map_or_else(
        || format!("Generated by Norn from {type_name}. Managed by Norn: change it in Norn."),
        |text| {
            format!(
                "{text} (Generated by Norn from {type_name}. Managed by Norn: change it in Norn.)"
            )
        },
    );
    let mut doctype: BTreeMap<String, JsonValue> = BTreeMap::from([
        ("autoname".to_owned(), string("prompt")),
        ("custom".to_owned(), int(0)),
        ("description".to_owned(), string(description)),
        ("doctype".to_owned(), string("DocType")),
        ("fields".to_owned(), JsonValue::Array(fields)),
        ("in_create".to_owned(), int(1)),
        ("is_virtual".to_owned(), int(1)),
        ("issingle".to_owned(), int(0)),
        ("istable".to_owned(), int(0)),
        ("module".to_owned(), string(FRAPPE_MODULE)),
        ("name".to_owned(), string(&name)),
        ("naming_rule".to_owned(), string("Set by user")),
        ("permissions".to_owned(), permissions),
        ("sort_field".to_owned(), string("modified")),
        ("sort_order".to_owned(), string("DESC")),
        ("track_changes".to_owned(), int(0)),
    ]);
    if let Some(title) = title_field {
        doctype.insert("title_field".to_owned(), string(title));
    }
    let mut entry: BTreeMap<String, JsonValue> = BTreeMap::from([
        ("enums".to_owned(), JsonValue::Object(enums)),
        ("fields".to_owned(), JsonValue::Object(field_map)),
        ("objectType".to_owned(), string(type_name)),
        ("transitions".to_owned(), JsonValue::Object(transitions_map)),
    ]);
    if let Some(id) = id_property {
        entry.insert("idProperty".to_owned(), string(id));
    }
    if let Some(property) = state_property {
        entry.insert("stateField".to_owned(), string(fieldname(property)));
    }
    Ok(Projected {
        doctype: JsonValue::Object(doctype),
        workflow,
        entry: JsonValue::Object(entry),
        roles,
        states,
        actions: labels,
    })
}

/// Project `input` to a Frappe bundle carrying its own `bundleHash`. Write it
/// out as canonical JSON (`norn_ir::canonical::canonical_json_of`), or any
/// JSON with integer flags: the bridge re-canonicalizes it to verify the hash.
///
/// # Errors
/// Two object types share a `DocType` name, a lifecycle has two transitions
/// with the same source and target, or the bundle cannot be canonicalized.
pub fn project_frappe_bundle(
    input: &FrappeProjectionInput<'_>,
) -> Result<JsonValue, FrappeProjectionError> {
    let mut doctypes = Vec::new();
    let mut workflows = Vec::new();
    let mut entries = BTreeMap::new();
    let (mut roles, mut states, mut actions) = (BTreeSet::new(), BTreeSet::new(), BTreeSet::new());
    for def in input.object_types {
        let projected = project_type(def, input)?;
        let name = doctype_name(def.name.as_str());
        if entries.insert(name.clone(), projected.entry).is_some() {
            return Err(FrappeProjectionError::DuplicateDocType(name));
        }
        doctypes.push(projected.doctype);
        workflows.extend(projected.workflow);
        roles.extend(projected.roles);
        states.extend(projected.states);
        actions.extend(projected.actions);
    }
    let role_map = JsonValue::Array(
        input
            .reader_roles
            .iter()
            .map(|role| JsonValue::Array(vec![string(frappe_role(role)), string(*role)]))
            .collect(),
    );
    let mut bundle: BTreeMap<String, JsonValue> = BTreeMap::from([
        ("actions".to_owned(), JsonValue::Object(BTreeMap::new())),
        ("doctypes".to_owned(), JsonValue::Array(doctypes)),
        ("format".to_owned(), string(FRAPPE_BUNDLE_FORMAT)),
        ("objectTypes".to_owned(), JsonValue::Object(entries)),
        ("principal".to_owned(), object([("roleMap", role_map)])),
        ("roles".to_owned(), strings(&roles)),
        ("workflowActions".to_owned(), strings(&actions)),
        ("workflowStates".to_owned(), strings(&states)),
        ("workflows".to_owned(), JsonValue::Array(workflows)),
    ]);
    let hash = domain_hash_hex(FRAPPE_BUNDLE_DOMAIN, &JsonValue::Object(bundle.clone()))
        .map_err(|error| FrappeProjectionError::Canonical(error.to_string()))?;
    bundle.insert("bundleHash".to_owned(), string(hash));
    Ok(JsonValue::Object(bundle))
}
