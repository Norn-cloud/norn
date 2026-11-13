//! LABEL-LOCALE-1: localized labels are checked presentation metadata.
//!
//! Per catalog (`check_local`): the payload decodes into the strict
//! [`LabelCatalogRoot`]; every locale tag is well-formed BCP 47 and unique
//! ignoring case; every text is non-blank, in Unicode NFC as sent, and free
//! of bidi embedding, override and isolate controls (renderers isolate ids
//! themselves, and a stored control could reorder surrounding text); a
//! description or review entry names a locale that has a label; a target
//! appears once.
//!
//! Over an artifact set (`check_label_set`): every candidate target resolves
//! to a definition in the candidate set or the installed base, and no two
//! candidate catalogs label the same target in the same locale. A candidate
//! label replaces an installed one ([`norn_ir::labels`] states the precedence),
//! so a downstream pack overlays upstream labels without touching them.
use std::collections::{BTreeMap, BTreeSet};

use norn_ir::labels::is_valid_locale_tag;
use norn_ir::{
    ArtifactEnvelope, ArtifactKind, ArtifactPayload, Diagnostic as LegacyDiagnostic,
    DiagnosticSeverity, EntityRoot, LabelCatalogRoot, LabelTarget, LocaleText, PropertyType,
};

use crate::envelope::invalid_diagnostic;
use crate::{Diagnostic, DiagnosticCode, Span};

const INVALID_PAYLOAD: &str = "labels.validation.invalid_payload";
const INVALID_LOCALE: &str = "labels.validation.invalid_locale";
const DUPLICATE_LOCALE: &str = "labels.validation.duplicate_locale";
const BLANK_TEXT: &str = "labels.validation.blank_text";
const BIDI_CONTROL: &str = "labels.validation.bidi_control";
const NOT_NFC: &str = "labels.validation.not_nfc";
const UNLABELLED_LOCALE: &str = "labels.validation.unlabelled_locale";
const DUPLICATE_TARGET: &str = "labels.validation.duplicate_target";
const UNKNOWN_TARGET: &str = "labels.admission.unknown_target";
const CONFLICTING_LABEL: &str = "labels.admission.conflicting_label";
const MISSING_LABEL: &str = "labels.lint.missing_label";

/// Decode a labels payload into its strict root, refusing non-canonical forms.
pub(crate) fn decode(payload: &ArtifactPayload) -> Result<LabelCatalogRoot, String> {
    let bytes = norn_canonical::canonical_json_of(&payload.to_json_value())
        .map_err(|error| error.to_string())?;
    let root: LabelCatalogRoot =
        serde_json::from_slice(&bytes).map_err(|error| error.to_string())?;
    if norn_canonical::canonical_json_of(&root).map_err(|error| error.to_string())? != bytes {
        return Err("payload must use the declared object representation".into());
    }
    Ok(root)
}

/// Whether `c` is a bidi embedding, override or isolate control.
const fn is_bidi_control(c: char) -> bool {
    matches!(c, '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}')
}

/// Check one labels payload on its own (see the module docs).
pub(crate) fn check_local(
    payload: &ArtifactPayload,
    diagnostics: &mut Vec<LegacyDiagnostic>,
    artifact_id: Option<&str>,
) {
    let root = match decode(payload) {
        Ok(root) => root,
        Err(error) => {
            diagnostics.push(invalid_diagnostic(
                INVALID_PAYLOAD,
                error,
                "payload",
                artifact_id,
            ));
            return;
        }
    };
    let mut push = |code: &str, message: String, path: String| {
        diagnostics.push(invalid_diagnostic(code, message, path, artifact_id));
    };
    // `decode` reads the Norn1 canonical bytes, whose strings are already NFC:
    // the form of the text as it was sent is only visible on the wire value.
    for (path, tag, member) in non_nfc_texts(payload) {
        push(
            NOT_NFC,
            format!("{member} for {tag} is not in Unicode NFC form; send the normalized text"),
            path,
        );
    }
    check_tags(
        root.required_locales.iter(),
        "payload.requiredLocales",
        &mut push,
    );
    let mut targets = BTreeSet::new();
    for (index, entry) in root.entries.iter().enumerate() {
        let at = format!("payload.entries.{index}");
        if !targets.insert(&entry.target) {
            push(
                DUPLICATE_TARGET,
                format!("{} is labelled more than once", entry.target.key()),
                format!("{at}.target"),
            );
        }
        if entry.label.0.is_empty() {
            push(
                BLANK_TEXT,
                "a label names at least one locale".into(),
                format!("{at}.label"),
            );
        }
        let labelled: BTreeSet<String> = entry
            .label
            .0
            .keys()
            .map(|tag| tag.to_ascii_lowercase())
            .collect();
        let texts = [
            ("label", Some(&entry.label)),
            ("description", entry.description.as_ref()),
        ];
        for (member, text) in texts {
            let Some(LocaleText(text)) = text else {
                continue;
            };
            check_tags(text.keys(), &format!("{at}.{member}"), &mut push);
            for (tag, value) in text {
                let path = format!("{at}.{member}.{tag}");
                if value.trim().is_empty() {
                    push(
                        BLANK_TEXT,
                        format!("{member} for {tag} is blank"),
                        path.clone(),
                    );
                }
                if value.chars().any(is_bidi_control) {
                    push(
                        BIDI_CONTROL,
                        format!(
                            "{member} for {tag} contains a bidi embedding, override or isolate control"
                        ),
                        path.clone(),
                    );
                }
                if member == "description" && !labelled.contains(&tag.to_ascii_lowercase()) {
                    push(
                        UNLABELLED_LOCALE,
                        format!("a description for {tag} needs a label for {tag}"),
                        path,
                    );
                }
            }
        }
        check_tags(entry.review.keys(), &format!("{at}.review"), &mut push);
        for tag in entry.review.keys() {
            if !labelled.contains(&tag.to_ascii_lowercase()) {
                push(
                    UNLABELLED_LOCALE,
                    format!("a review state for {tag} needs a label for {tag}"),
                    format!("{at}.review.{tag}"),
                );
            }
        }
    }
}

/// Every label or description whose text, as sent, is not NFC: its path, tag
/// and member. Two canonically equivalent spellings would otherwise be
/// admitted as different texts, and hashing would then normalize them into one.
fn non_nfc_texts(payload: &ArtifactPayload) -> Vec<(String, String, &'static str)> {
    let wire = serde_json::to_value(payload.to_json_value()).unwrap_or_default();
    let entries = wire.get("entries").and_then(serde_json::Value::as_array);
    let mut found = Vec::new();
    for (index, entry) in entries.into_iter().flatten().enumerate() {
        for member in ["label", "description"] {
            let texts = entry.get(member).and_then(serde_json::Value::as_object);
            for (tag, text) in texts.into_iter().flatten() {
                if text
                    .as_str()
                    .is_some_and(|text| norn_canonical::nfc(text) != text)
                {
                    found.push((
                        format!("payload.entries.{index}.{member}.{tag}"),
                        tag.clone(),
                        member,
                    ));
                }
            }
        }
    }
    found
}

fn check_tags<'a>(
    tags: impl Iterator<Item = &'a String>,
    at: &str,
    push: &mut impl FnMut(&str, String, String),
) {
    let mut seen = BTreeSet::new();
    for tag in tags {
        if !is_valid_locale_tag(tag) {
            push(
                INVALID_LOCALE,
                format!("{tag:?} is not a BCP 47 language tag"),
                format!("{at}.{tag}"),
            );
        } else if !seen.insert(tag.to_ascii_lowercase()) {
            push(
                DUPLICATE_LOCALE,
                format!("{tag} appears twice, ignoring case"),
                format!("{at}.{tag}"),
            );
        }
    }
}

/// Every label target the definitions in `artifacts` declare: object types
/// and interfaces, their properties (including extension properties) and enum
/// values (inline or through a shared property type), actions, rules, laws and
/// widget pages. Artifacts that do not decode declare nothing; their own
/// checks report them.
#[must_use]
pub fn declared_targets<'a>(
    artifacts: impl IntoIterator<Item = &'a ArtifactEnvelope>,
) -> BTreeSet<LabelTarget> {
    let artifacts: Vec<_> = artifacts.into_iter().collect();
    let entity_roots: Vec<EntityRoot> = artifacts
        .iter()
        .filter(|artifact| artifact.kind() == ArtifactKind::Entity)
        .filter_map(|artifact| {
            serde_json::to_value(artifact.payload.to_json_value())
                .ok()
                .and_then(|value| serde_json::from_value(value).ok())
        })
        .collect();
    let mut targets = entity_targets(&entity_roots);
    for artifact in artifacts {
        definition_targets(artifact, &mut targets);
    }
    targets
}

/// The values of an `enum` property type; empty for every other kind.
fn enum_values(property_type: &PropertyType) -> Vec<&str> {
    if let PropertyType::Enum { values } = property_type {
        values.iter().map(|value| value.value.as_str()).collect()
    } else {
        Vec::new()
    }
}

/// Object types, interfaces, properties and enum values of entity roots.
fn entity_targets(entity_roots: &[EntityRoot]) -> BTreeSet<LabelTarget> {
    // Shared property types resolve `ref` enum properties to their values.
    let shared_enums: BTreeMap<&str, Vec<&str>> = entity_roots
        .iter()
        .filter_map(|root| {
            if let EntityRoot::V2PropertyTypes(catalog) = root {
                Some(&catalog.property_types)
            } else {
                None
            }
        })
        .flatten()
        .map(|def| (def.name.0.as_str(), enum_values(&def.property_type)))
        .collect();
    let mut targets = BTreeSet::new();
    let mut property = |owner: &str, id: &str, values: Vec<&str>| {
        targets.insert(LabelTarget::Property {
            object_type: owner.into(),
            property: id.into(),
        });
        for value in values {
            targets.insert(LabelTarget::EnumValue {
                object_type: owner.into(),
                property: id.into(),
                value: value.into(),
            });
        }
    };
    let values_of = |property_type: &PropertyType| -> Vec<String> {
        let values = if let PropertyType::Ref { name } = property_type {
            shared_enums
                .get(name.0.as_str())
                .cloned()
                .unwrap_or_default()
        } else {
            enum_values(property_type)
        };
        values.into_iter().map(str::to_owned).collect()
    };
    let mut named = BTreeSet::new();
    for root in entity_roots {
        match root {
            EntityRoot::V2(root) => {
                let def = &root.object_type;
                named.insert(def.name.0.clone());
                for prop in &def.properties {
                    let values = values_of(&prop.property_type);
                    property(
                        &def.name.0,
                        &prop.id.0,
                        values.iter().map(String::as_str).collect(),
                    );
                }
            }
            EntityRoot::V2Interface(root) => {
                let def = &root.interface;
                named.insert(def.name.0.clone());
                for prop in &def.properties {
                    let values = values_of(&prop.property_type);
                    property(
                        &def.name.0,
                        &prop.id.0,
                        values.iter().map(String::as_str).collect(),
                    );
                }
            }
            EntityRoot::V2Extension(root) => {
                let ext = &root.object_type_extension;
                for prop in &ext.properties {
                    let values = values_of(&prop.property_type);
                    property(
                        &ext.target.0,
                        &prop.id.0,
                        values.iter().map(String::as_str).collect(),
                    );
                }
            }
            EntityRoot::V1(root) => {
                named.insert(root.object_kind.clone());
                for (id, field) in &root.fields {
                    let values = if let norn_ir::EntityFieldV1::Enum { values, .. } = field {
                        values.iter().map(String::as_str).collect()
                    } else {
                        Vec::new()
                    };
                    property(&root.object_kind, id, values);
                }
            }
            EntityRoot::V2PropertyTypes(_) | EntityRoot::V2Markings(_) => {}
        }
    }
    targets.extend(
        named
            .into_iter()
            .map(|object_type| LabelTarget::ObjectType { object_type }),
    );
    targets
}

/// The action, rule, law or widget-page targets one artifact declares.
fn definition_targets(artifact: &ArtifactEnvelope, targets: &mut BTreeSet<LabelTarget>) {
    let JsonPayload(payload) = JsonPayload::of(artifact);
    let text = |key: &str| payload.get(key).and_then(serde_json::Value::as_str);
    match artifact.kind() {
        ArtifactKind::Action => targets.extend(text("actionId").map(|id| LabelTarget::Action {
            action_id: id.into(),
        })),
        ArtifactKind::Rule => {
            targets.extend(text("ruleId").map(|id| LabelTarget::Rule { rule_id: id.into() }));
        }
        ArtifactKind::Law => {
            targets.extend(text("id").map(|id| LabelTarget::Law { law_id: id.into() }));
        }
        ArtifactKind::Widget => {
            let Some(widget_id) = text("widgetId") else {
                return;
            };
            let pages = payload
                .get("ux")
                .and_then(|ux| ux.get("pages"))
                .and_then(serde_json::Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(|page| page.get("pageId").and_then(serde_json::Value::as_str));
            for page_id in pages {
                targets.insert(LabelTarget::Page {
                    widget_id: widget_id.into(),
                    page_id: page_id.into(),
                });
            }
        }
        ArtifactKind::Entity
        | ArtifactKind::Relation
        | ArtifactKind::Connector
        | ArtifactKind::Customization
        | ArtifactKind::Setting
        | ArtifactKind::BusinessPack
        | ArtifactKind::Policy
        | ArtifactKind::Derived
        | ArtifactKind::SourceSchema
        | ArtifactKind::Mapping
        | ArtifactKind::DefinedConcept
        | ArtifactKind::Labels => {}
    }
}

struct JsonPayload(serde_json::Map<String, serde_json::Value>);

impl JsonPayload {
    fn of(artifact: &ArtifactEnvelope) -> Self {
        match serde_json::to_value(artifact.payload.to_json_value()) {
            Ok(serde_json::Value::Object(object)) => Self(object),
            _ => Self(serde_json::Map::new()),
        }
    }
}

/// Check the candidate label catalogs against the candidate definitions and
/// the installed base (see the module docs).
pub(crate) fn check_label_set<'a>(
    artifacts: &'a [ArtifactEnvelope],
    base: &'a [ArtifactEnvelope],
) -> Vec<Diagnostic> {
    let decoded = |set: &'a [ArtifactEnvelope]| -> Vec<(&'a ArtifactEnvelope, LabelCatalogRoot)> {
        let mut catalogs: Vec<_> = set
            .iter()
            .filter(|artifact| artifact.kind() == ArtifactKind::Labels)
            .filter_map(|artifact| decode(&artifact.payload).ok().map(|root| (artifact, root)))
            .collect();
        catalogs.sort_by(|a, b| {
            (&a.0.artifact_id, &a.0.version).cmp(&(&b.0.artifact_id, &b.0.version))
        });
        catalogs
    };
    let catalogs = decoded(artifacts);
    let installed = decoded(base);
    let required = installed
        .iter()
        .chain(&catalogs)
        .any(|(_, root)| !root.required_locales.is_empty());
    if catalogs.is_empty() && !required {
        return Vec::new();
    }
    let declared = declared_targets(artifacts.iter().chain(base));
    let mut findings = missing_labels(&declared, &installed, &catalogs);
    let mut owners: BTreeMap<(String, String), &str> = BTreeMap::new();
    let mut sorted: Vec<_> = catalogs.iter().collect();
    sorted.sort_by(|a, b| (&a.0.artifact_id, &a.0.version).cmp(&(&b.0.artifact_id, &b.0.version)));
    for (artifact, root) in sorted {
        for (index, entry) in root.entries.iter().enumerate() {
            let key = entry.target.key();
            if !declared.contains(&entry.target) {
                findings.push(finding(
                    UNKNOWN_TARGET,
                    format!(
                        "no definition in the artifact set or the installed base declares {key}"
                    ),
                    format!("/payload/entries/{index}/target"),
                    artifact,
                ));
            }
            for tag in entry.label.0.keys() {
                let slot = (key.clone(), tag.to_ascii_lowercase());
                match owners.get(&slot) {
                    Some(owner) if *owner != artifact.artifact_id => findings.push(finding(
                        CONFLICTING_LABEL,
                        format!("{owner} already labels {key} in {tag}; one candidate layer labels a target once per locale"),
                        format!("/payload/entries/{index}/label/{}", escape_pointer(tag)),
                        artifact,
                    )),
                    Some(_) => {}
                    None => {
                        owners.insert(slot, &artifact.artifact_id);
                    }
                }
            }
        }
    }
    findings
}

/// LABEL-COVERAGE-1: for every locale an installed or candidate catalog
/// requires, list the declared definitions that have no label in it after
/// layering (installed catalogs first, then candidates). One warning per
/// locale, on the candidate catalog that requires it (or the first candidate
/// catalog, or no artifact when the set has none), naming every target.
fn missing_labels(
    declared: &BTreeSet<LabelTarget>,
    installed: &[(&ArtifactEnvelope, LabelCatalogRoot)],
    candidates: &[(&ArtifactEnvelope, LabelCatalogRoot)],
) -> Vec<Diagnostic> {
    let mut required: BTreeMap<String, Option<&ArtifactEnvelope>> = BTreeMap::new();
    for (artifact, root, candidate) in installed
        .iter()
        .map(|(a, r)| (*a, r, false))
        .chain(candidates.iter().map(|(a, r)| (*a, r, true)))
    {
        for tag in root
            .required_locales
            .iter()
            .filter(|tag| is_valid_locale_tag(tag))
        {
            let owner = required.entry(tag.to_ascii_lowercase()).or_insert(None);
            if candidate && owner.is_none() {
                *owner = Some(artifact);
            }
        }
    }
    let resolved =
        norn_ir::labels::resolve(installed.iter().chain(candidates).map(|(_, root)| root));
    required
        .into_iter()
        .filter_map(|(tag, owner)| {
            let missing: BTreeSet<String> = declared
                .iter()
                .filter(|target| {
                    !resolved.get(target).is_some_and(|locales| {
                        locales.keys().any(|key| key.eq_ignore_ascii_case(&tag))
                    })
                })
                .map(LabelTarget::key)
                .collect();
            if missing.is_empty() {
                return None;
            }
            let owner = owner.or_else(|| candidates.first().map(|(artifact, _)| *artifact));
            Some(Diagnostic {
                witness: None,
                code: DiagnosticCode::new(MISSING_LABEL),
                severity: DiagnosticSeverity::Warning,
                message: format!(
                    "{} definitions have no {tag} label: {}",
                    missing.len(),
                    missing.into_iter().collect::<Vec<_>>().join(", ")
                ),
                span: owner.map(|_| Span::pointer("/payload/requiredLocales")),
                artifact_id: owner.map(|artifact| artifact.artifact_id.clone()),
                remediation: Some(norn_ir::DiagnosticRemediation::FixDefinition),
            })
        })
        .collect()
}

fn escape_pointer(segment: &str) -> String {
    segment.replace('~', "~0").replace('/', "~1")
}

fn finding(
    code: &str,
    message: String,
    pointer: String,
    artifact: &ArtifactEnvelope,
) -> Diagnostic {
    Diagnostic {
        witness: None,
        code: DiagnosticCode::new(code),
        severity: DiagnosticSeverity::Error,
        message,
        span: Some(Span::pointer(pointer)),
        artifact_id: Some(artifact.artifact_id.clone()),
        remediation: Some(norn_ir::DiagnosticRemediation::FixDefinition),
    }
}
