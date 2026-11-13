//! `ModelIR` admission (#1171 M1): a model is a typed definition whose
//! internals stay opaque.
//!
//! Per model (`check_local`): the payload decodes into the strict
//! [`ModelRoot`]; the model id, the input type name, every feature, the
//! target, every output and every contract id are well-formed, and ids are
//! unique; the target is not a feature; every output is a `text`, `integer`,
//! `decimal`, `boolean` or `enum` column (a decimal with a static unit or
//! none) and its type members are valid; trust is `opaque` or
//! `contract_checked`, the latter with at least one contract; each contract
//! names an output and its rule applies to that output's type (`range` and
//! `finite` to numeric outputs, `oneOf` to categorical ones and only with
//! declared enum values).
//!
//! Over an artifact set (`check_model_set`): each candidate model's input
//! object type resolves in the candidate set or the installed base, and every
//! feature and the target names a property of it (own, inherited through
//! `extends`, from an implemented interface, or from an extension) whose kind
//! is admitted: numeric, boolean, `enum` or `text` for a feature, numeric,
//! boolean or `enum` for the target. A `ref` property is resolved through the
//! shared property types of the set and the base; one that does not resolve
//! there is refused rather than guessed. A model that names a predictions
//! type (#1171 M4) is admitted only when that type resolves, has text
//! properties `subject` and `model_version`, and declares every output as a
//! property of the same type (a decimal's scale and unit, an enum's values
//! as a superset) and the same nullability. A model never trains on
//! personal data that is not pseudonymized ([`crate::model_privacy`]).
use std::collections::{BTreeMap, BTreeSet};
use std::sync::LazyLock;

use norn_ir::model::MODEL_ID_PATTERN;
use norn_ir::{
    ArtifactEnvelope, ArtifactKind, ArtifactPayload, ContractRule, Diagnostic as LegacyDiagnostic,
    DiagnosticSeverity, EntityFieldV1, EntityRoot, ModelOutput, ModelRoot, PropertyType,
    PropertyTypeDef, TrustLevel,
};

use crate::envelope::invalid_diagnostic;
use crate::object_types::{MEMBER_ID_GRAMMAR, OBJECT_NAME_GRAMMAR};
use crate::property_types::{Decimal, matches_grammar};
use crate::{Diagnostic, DiagnosticCode, Span};

const INVALID_PAYLOAD: &str = "model.validation.invalid_payload";
const INVALID_ID: &str = "model.validation.invalid_id";
const DUPLICATE_ID: &str = "model.validation.duplicate_id";
const TARGET_IN_FEATURES: &str = "model.validation.target_in_features";
const INVALID_OUTPUT: &str = "model.validation.invalid_output";
const INVALID_CONTRACT: &str = "model.validation.invalid_contract";
const UNSUPPORTED_TRUST: &str = "model.validation.unsupported_trust";
const TRUST_WITHOUT_CONTRACT: &str = "model.validation.trust_without_contract";
const UNKNOWN_INPUT: &str = "model.admission.unknown_input";
const UNKNOWN_FEATURE: &str = "model.admission.unknown_feature";
const FEATURE_KIND: &str = "model.admission.feature_kind";
const PREDICTION_TYPE: &str = "model.admission.prediction_type_mismatch";

/// The predictions type's property holding the scored input object's id.
const PREDICTION_SUBJECT: &str = "subject";
/// The predictions type's property holding the `ml.ModelVersion` id.
const PREDICTION_VERSION: &str = "model_version";

/// Legacy path of the `method` member.
const METHOD: &str = "payload.method";

static MODEL_ID_GRAMMAR: LazyLock<Option<regex_lite::Regex>> =
    LazyLock::new(|| regex_lite::Regex::new(MODEL_ID_PATTERN).ok());

/// Decode a model payload into its strict root, refusing non-canonical forms.
pub(crate) fn decode(payload: &ArtifactPayload) -> Result<ModelRoot, String> {
    let bytes = norn_canonical::canonical_json_of(&payload.to_json_value())
        .map_err(|error| error.to_string())?;
    let root: ModelRoot = serde_json::from_slice(&bytes).map_err(|error| error.to_string())?;
    if norn_canonical::canonical_json_of(&root).map_err(|error| error.to_string())? != bytes {
        return Err("payload must use the declared object representation".into());
    }
    Ok(root)
}

/// Check one model payload on its own (see the module docs).
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
    check_header(&root, &mut push);
    check_features(&root, &mut push);
    let outputs = check_outputs(&root.outputs, &mut push);
    check_trust(&root, &mut push);
    check_contracts(&root, &outputs, &mut push);
}

fn check_header(root: &ModelRoot, push: &mut impl FnMut(&str, String, String)) {
    if !matches_grammar(&MODEL_ID_GRAMMAR, &root.model_id) {
        push(
            INVALID_ID,
            format!(
                "model id {:?} must be two or more dotted snake_case segments (sales.churn_model)",
                root.model_id
            ),
            "payload.modelId".into(),
        );
    }
    if root.title.trim().is_empty() {
        push(
            INVALID_PAYLOAD,
            "a model has a non-blank title".into(),
            "payload.title".into(),
        );
    }
    if !crate::ontology::valid_semver(&root.version) {
        push(
            INVALID_PAYLOAD,
            format!(
                "model version {:?} is not a SemVer 2.0.0 version",
                root.version
            ),
            "payload.version".into(),
        );
    }
    if root.method.label.trim().is_empty() {
        push(
            INVALID_PAYLOAD,
            "a model method has a non-blank label".into(),
            format!("{METHOD}.label"),
        );
    }
    if root
        .method
        .version
        .as_ref()
        .is_some_and(|version| version.trim().is_empty())
    {
        push(
            INVALID_PAYLOAD,
            "a model method version is non-blank when present".into(),
            format!("{METHOD}.version"),
        );
    }
    let input = root.input.object_type();
    if !matches_grammar(&OBJECT_NAME_GRAMMAR, input.as_str()) {
        push(
            INVALID_ID,
            format!(
                "input object type {:?} must be a qualified object type name (sales.Customer)",
                input.as_str()
            ),
            "payload.input.objectType".into(),
        );
    }
    if let Some(predictions) = &root.predictions
        && !matches_grammar(&OBJECT_NAME_GRAMMAR, predictions.object_type.as_str())
    {
        push(
            INVALID_ID,
            format!(
                "predictions object type {:?} must be a qualified object type name (sales.ChurnPrediction)",
                predictions.object_type.as_str()
            ),
            "payload.predictions.objectType".into(),
        );
    }
}

fn check_features(root: &ModelRoot, push: &mut impl FnMut(&str, String, String)) {
    if root.features.is_empty() {
        push(
            INVALID_PAYLOAD,
            "a model reads at least one feature".into(),
            "payload.features".into(),
        );
    }
    let mut seen = BTreeSet::new();
    for (index, feature) in root.features.iter().enumerate() {
        let path = format!("payload.features.{index}");
        if !matches_grammar(&MEMBER_ID_GRAMMAR, feature.as_str()) {
            push(
                INVALID_ID,
                format!(
                    "feature {:?} must be a property id ([a-z][a-z0-9_]*)",
                    feature.as_str()
                ),
                path.clone(),
            );
        }
        if !seen.insert(feature.as_str()) {
            push(
                DUPLICATE_ID,
                format!("feature {:?} is listed twice", feature.as_str()),
                path.clone(),
            );
        }
        if root.target.as_ref() == Some(feature) {
            push(
                TARGET_IN_FEATURES,
                format!(
                    "{:?} is the model's target and cannot also be a feature",
                    feature.as_str()
                ),
                path,
            );
        }
    }
    if let Some(target) = &root.target
        && !matches_grammar(&MEMBER_ID_GRAMMAR, target.as_str())
    {
        push(
            INVALID_ID,
            format!(
                "target {:?} must be a property id ([a-z][a-z0-9_]*)",
                target.as_str()
            ),
            "payload.target".into(),
        );
    }
}

/// Check every output; returns the outputs by id (the first of a duplicate).
fn check_outputs<'a>(
    outputs: &'a [ModelOutput],
    push: &mut impl FnMut(&str, String, String),
) -> BTreeMap<&'a str, &'a ModelOutput> {
    if outputs.is_empty() {
        push(
            INVALID_PAYLOAD,
            "a model writes at least one output".into(),
            "payload.outputs".into(),
        );
    }
    let mut by_id = BTreeMap::new();
    for (index, output) in outputs.iter().enumerate() {
        let position = index.to_string();
        let path = format!("payload.outputs.{index}");
        if !matches_grammar(&MEMBER_ID_GRAMMAR, output.id.as_str()) {
            push(
                INVALID_ID,
                format!(
                    "output id {:?} must match [a-z][a-z0-9_]*",
                    output.id.as_str()
                ),
                format!("{path}.id"),
            );
        }
        if by_id.contains_key(output.id.as_str()) {
            push(
                DUPLICATE_ID,
                format!("output {:?} is declared twice", output.id.as_str()),
                format!("{path}.id"),
            );
        } else {
            by_id.insert(output.id.as_str(), output);
        }
        if let Some(reason) = unsupported_output(&output.property_type) {
            push(INVALID_OUTPUT, reason, format!("{path}.type"));
            continue;
        }
        let mut members = Vec::new();
        crate::property_types::check_type_members(
            &["payload", "outputs", position.as_str()],
            &output.property_type,
            &mut members,
        );
        for finding in members {
            let path = finding.span.map_or_else(
                || path.clone(),
                |span| span.pointer.trim_start_matches('/').replace('/', "."),
            );
            push(finding.code.as_str(), finding.message, path);
        }
    }
    by_id
}

/// Why `property_type` cannot be a prediction column, if it cannot.
fn unsupported_output(property_type: &PropertyType) -> Option<String> {
    match property_type {
        PropertyType::Text { .. }
        | PropertyType::Integer { .. }
        | PropertyType::Boolean {}
        | PropertyType::Enum { .. } => None,
        PropertyType::Decimal {
            additivity: _additivity,
            scale: _scale,
            rounding: _rounding,
            unit: _unit,
            currency_source,
            per_unit,
            unit_source,
        } => (currency_source.is_some() || per_unit.is_some() || !unit_source.is_empty()).then(
            || {
                "a decimal output carries a static unit or none, never a currencySource, perUnit or unitSource".into()
            },
        ),
        PropertyType::DecimalBinding { .. }
        | PropertyType::Timestamp { .. }
        | PropertyType::Date {}
        | PropertyType::Duration {}
        | PropertyType::Record { .. }
        | PropertyType::JsonExt { .. }
        | PropertyType::Ref { .. } => Some(format!(
            "an output is an inline text, integer, decimal, boolean or enum column, not {}",
            property_type.kind().as_str()
        )),
    }
}

fn check_trust(root: &ModelRoot, push: &mut impl FnMut(&str, String, String)) {
    match root.trust {
        TrustLevel::Native | TrustLevel::Proven => push(
            UNSUPPORTED_TRUST,
            format!(
                "a model's internals are opaque, so its trust is opaque or contract_checked, not {}",
                root.trust.as_str()
            ),
            "payload.trust".into(),
        ),
        TrustLevel::ContractChecked if root.contracts.is_empty() => push(
            TRUST_WITHOUT_CONTRACT,
            "contract_checked trust needs at least one contract".into(),
            "payload.trust".into(),
        ),
        TrustLevel::ContractChecked | TrustLevel::Opaque => {}
    }
}

fn check_contracts(
    root: &ModelRoot,
    outputs: &BTreeMap<&str, &ModelOutput>,
    push: &mut impl FnMut(&str, String, String),
) {
    let mut seen = BTreeSet::new();
    for (index, contract) in root.contracts.iter().enumerate() {
        let path = format!("payload.contracts.{index}");
        if !matches_grammar(&MEMBER_ID_GRAMMAR, contract.id.as_str()) {
            push(
                INVALID_ID,
                format!(
                    "contract id {:?} must match [a-z][a-z0-9_]*",
                    contract.id.as_str()
                ),
                format!("{path}.id"),
            );
        }
        if !seen.insert(contract.id.as_str()) {
            push(
                DUPLICATE_ID,
                format!("contract {:?} is declared twice", contract.id.as_str()),
                format!("{path}.id"),
            );
        }
        let Some(output) = outputs.get(contract.output.as_str()) else {
            push(
                INVALID_CONTRACT,
                format!(
                    "contract {:?} checks {:?}, which is not an output of the model",
                    contract.id.as_str(),
                    contract.output.as_str()
                ),
                format!("{path}.output"),
            );
            continue;
        };
        if let Some(reason) = inapplicable(&contract.rule, &output.property_type) {
            push(
                INVALID_CONTRACT,
                format!(
                    "contract {:?} on {:?}: {reason}",
                    contract.id.as_str(),
                    contract.output.as_str()
                ),
                format!("{path}.rule"),
            );
        }
    }
}

/// Why `rule` does not apply to an output of `property_type`, if it does not.
fn inapplicable(rule: &ContractRule, property_type: &PropertyType) -> Option<String> {
    let numeric = matches!(
        property_type,
        PropertyType::Integer { .. } | PropertyType::Decimal { .. }
    );
    let kind = property_type.kind().as_str();
    match rule {
        ContractRule::NotNull {} => None,
        ContractRule::Finite {} => {
            (!numeric).then(|| format!("finite applies to a numeric output, not {kind}"))
        }
        ContractRule::Range { min, max } => {
            if !numeric {
                return Some(format!("range applies to a numeric output, not {kind}"));
            }
            match (bound(min.as_ref()), bound(max.as_ref())) {
                (None, None) => Some("a range declares min, max or both".into()),
                (Some(Err(text)), _) | (_, Some(Err(text))) => {
                    Some(format!("{text:?} is not an exact decimal literal"))
                }
                (Some(Ok(low)), Some(Ok(high))) if low > high => {
                    Some("range min is greater than max".into())
                }
                (Some(Ok(_)) | None, Some(Ok(_)) | None) => None,
            }
        }
        ContractRule::OneOf { values } => {
            let declared: Option<BTreeSet<&str>> = match property_type {
                PropertyType::Enum { values } => {
                    Some(values.iter().map(|value| value.value.as_str()).collect())
                }
                PropertyType::Text { .. } => None,
                PropertyType::Integer { .. }
                | PropertyType::Decimal { .. }
                | PropertyType::DecimalBinding { .. }
                | PropertyType::Boolean {}
                | PropertyType::Timestamp { .. }
                | PropertyType::Date {}
                | PropertyType::Duration {}
                | PropertyType::Record { .. }
                | PropertyType::JsonExt { .. }
                | PropertyType::Ref { .. } => {
                    return Some(format!(
                        "oneOf applies to an enum or text output, not {kind}"
                    ));
                }
            };
            if values.is_empty() {
                return Some("oneOf lists at least one value".into());
            }
            let mut seen = BTreeSet::new();
            if let Some(value) = values.iter().find(|value| !seen.insert(value.as_str())) {
                return Some(format!("oneOf lists {value:?} twice"));
            }
            declared.and_then(|declared| {
                values
                    .iter()
                    .find(|value| !declared.contains(value.as_str()))
                    .map(|value| format!("{value:?} is not a value of the enum output"))
            })
        }
    }
}

/// A range bound parsed for comparison, or its text when it is not an exact
/// decimal literal.
fn bound(literal: Option<&norn_ir::DecimalLiteral>) -> Option<Result<Decimal<'_>, &str>> {
    literal.map(|literal| Decimal::parse_str(literal.as_str()).ok_or(literal.as_str()))
}

/// The admitted-kind class of an input property.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Class {
    /// `integer` or `decimal` (v1 `integer` or `number`).
    Numeric,
    Boolean,
    /// `enum`.
    Enum,
    /// `text` (v1 `text` or `string`).
    Text,
    /// Any other kind, by its wire name.
    Other(&'static str),
    /// A link of the type, not a property.
    Link,
    /// A `ref` that names no shared property type of the set or the base.
    Unresolved,
}

const fn v2_class(property_type: &PropertyType) -> Class {
    match property_type {
        PropertyType::Integer { .. } | PropertyType::Decimal { .. } => Class::Numeric,
        PropertyType::Boolean {} => Class::Boolean,
        PropertyType::Enum { .. } => Class::Enum,
        PropertyType::Text { .. } => Class::Text,
        PropertyType::DecimalBinding { .. }
        | PropertyType::Timestamp { .. }
        | PropertyType::Date {}
        | PropertyType::Duration {}
        | PropertyType::Record { .. }
        | PropertyType::JsonExt { .. }
        | PropertyType::Ref { .. } => Class::Other(property_type.kind().as_str()),
    }
}

const fn v1_class(field: &EntityFieldV1) -> Class {
    match field {
        EntityFieldV1::Integer { .. } | EntityFieldV1::Number { .. } => Class::Numeric,
        EntityFieldV1::Boolean { .. } => Class::Boolean,
        EntityFieldV1::Enum { .. } => Class::Enum,
        EntityFieldV1::Text { .. } | EntityFieldV1::String { .. } => Class::Text,
        EntityFieldV1::Object { .. } => Class::Other("object"),
        EntityFieldV1::Array { .. } => Class::Other("array"),
    }
}

/// Object types, interfaces, extensions and shared property types of the
/// candidate set and the installed base, by name. A candidate definition
/// shadows an installed one of the same name.
#[derive(Default)]
struct Catalog {
    object_types: BTreeMap<String, EntityRoot>,
    interfaces: BTreeMap<String, EntityRoot>,
    extensions: BTreeMap<String, Vec<EntityRoot>>,
    property_types: Vec<PropertyTypeDef>,
}

impl Catalog {
    fn of<'a>(artifacts: impl IntoIterator<Item = &'a ArtifactEnvelope>) -> Self {
        let mut catalog = Self::default();
        let roots = artifacts
            .into_iter()
            .filter(|artifact| artifact.kind() == ArtifactKind::Entity)
            .filter_map(|artifact| {
                serde_json::to_value(artifact.payload.to_json_value())
                    .ok()
                    .and_then(|value| serde_json::from_value::<EntityRoot>(value).ok())
            });
        for root in roots {
            match &root {
                EntityRoot::V2(def) => {
                    let name = def.object_type.name.as_str().to_owned();
                    catalog.object_types.entry(name).or_insert(root);
                }
                EntityRoot::V1(def) => {
                    let name = def.object_kind.clone();
                    catalog.object_types.entry(name).or_insert(root);
                }
                EntityRoot::V2Interface(def) => {
                    let name = def.interface.name.as_str().to_owned();
                    catalog.interfaces.entry(name).or_insert(root);
                }
                EntityRoot::V2Extension(def) => {
                    let target = def.object_type_extension.target.as_str().to_owned();
                    catalog.extensions.entry(target).or_default().push(root);
                }
                EntityRoot::V2PropertyTypes(def) => {
                    catalog
                        .property_types
                        .extend(def.property_types.iter().cloned());
                }
                EntityRoot::V2Markings(_) => {}
            }
        }
        catalog
    }

    /// The type a property's type resolves to: itself, or the shared
    /// property type a `ref` names; `None` for a `ref` that does not resolve.
    fn resolve<'a>(&'a self, property_type: &'a PropertyType) -> Option<&'a PropertyType> {
        if matches!(property_type, PropertyType::Ref { .. }) {
            crate::property_types::resolved_property_type(property_type, &self.property_types)
        } else {
            Some(property_type)
        }
    }

    fn class_of(&self, property_type: &PropertyType) -> Class {
        self.resolve(property_type)
            .map_or(Class::Unresolved, v2_class)
    }

    fn class(&self, member: Member<'_>) -> Class {
        match member {
            Member::Property(property_type, _) => self.class_of(property_type),
            Member::Field(field) => v1_class(field),
            Member::Link => Class::Link,
        }
    }

    /// The admitted-kind class of every effective member of object type
    /// `name`, or `None` when no object type has that name.
    fn members(&self, name: &str) -> Option<BTreeMap<&str, Class>> {
        self.member_refs(name).map(|members| {
            members
                .into_iter()
                .map(|(id, member)| (id, self.class(member)))
                .collect()
        })
    }

    /// The effective members of object type `name` by id, most derived
    /// first, or `None` when no object type has that name.
    fn member_refs(&self, name: &str) -> Option<BTreeMap<&str, Member<'_>>> {
        let mut members = BTreeMap::new();
        let mut interfaces = Vec::new();
        let mut visited = BTreeSet::new();
        let mut next = Some(name.to_owned());
        let mut found = false;
        while let Some(current) = next.take() {
            if !visited.insert(current.clone()) {
                break;
            }
            let Some(root) = self.object_types.get(&current) else {
                break;
            };
            found = true;
            match root {
                EntityRoot::V2(def) => {
                    let def = &def.object_type;
                    for property in &def.properties {
                        members
                            .entry(property.id.as_str())
                            .or_insert(Member::Property(
                                &property.property_type,
                                property.constraints.nullable,
                            ));
                    }
                    for link in &def.links {
                        members.entry(link.id.as_str()).or_insert(Member::Link);
                    }
                    interfaces.extend(def.implements.iter().map(|name| name.as_str().to_owned()));
                    next = def.extends.as_ref().map(|name| name.as_str().to_owned());
                }
                EntityRoot::V1(def) => {
                    for (id, field) in &def.fields {
                        members.entry(id.as_str()).or_insert(Member::Field(field));
                    }
                }
                EntityRoot::V2Interface(_)
                | EntityRoot::V2Extension(_)
                | EntityRoot::V2PropertyTypes(_)
                | EntityRoot::V2Markings(_) => {}
            }
            for extension in self.extensions.get(&current).into_iter().flatten() {
                if let EntityRoot::V2Extension(def) = extension {
                    let def = &def.object_type_extension;
                    for property in &def.properties {
                        members
                            .entry(property.id.as_str())
                            .or_insert(Member::Property(
                                &property.property_type,
                                property.constraints.nullable,
                            ));
                    }
                    for link in &def.links {
                        members.entry(link.id.as_str()).or_insert(Member::Link);
                    }
                }
            }
        }
        if !found {
            return None;
        }
        let mut seen = BTreeSet::new();
        while let Some(interface) = interfaces.pop() {
            if !seen.insert(interface.clone()) {
                continue;
            }
            if let Some(EntityRoot::V2Interface(def)) = self.interfaces.get(&interface) {
                let def = &def.interface;
                for property in &def.properties {
                    members
                        .entry(property.id.as_str())
                        .or_insert(Member::Property(
                            &property.property_type,
                            property.constraints.nullable,
                        ));
                }
                interfaces.extend(def.extends.iter().map(|name| name.as_str().to_owned()));
            }
        }
        Some(members)
    }
}

/// One effective member of an object type.
#[derive(Debug, Clone, Copy)]
enum Member<'a> {
    /// An `EntityIR` 2.0 property: its declared type and whether it is nullable.
    Property(&'a PropertyType, bool),
    /// An `EntityIR` 1.0 field.
    Field(&'a EntityFieldV1),
    /// A link.
    Link,
}

/// Check the candidate models against the candidate definitions and the
/// installed base (see the module docs).
pub(crate) fn check_model_set(
    artifacts: &[ArtifactEnvelope],
    base: &[ArtifactEnvelope],
) -> Vec<Diagnostic> {
    let models: Vec<(&ArtifactEnvelope, ModelRoot)> = artifacts
        .iter()
        .filter(|artifact| artifact.kind() == ArtifactKind::Model)
        .filter_map(|artifact| decode(&artifact.payload).ok().map(|root| (artifact, root)))
        .collect();
    if models.is_empty() {
        return Vec::new();
    }
    let catalog = Catalog::of(artifacts.iter().chain(base));
    let mut findings = Vec::new();
    for (artifact, root) in &models {
        check_predictions(&catalog, root, artifact, &mut findings);
        let input = root.input.object_type().as_str();
        let Some(members) = catalog.members(input) else {
            findings.push(finding(
                UNKNOWN_INPUT,
                format!(
                    "no object type in the artifact set or the installed base is named {input}"
                ),
                "/payload/input/objectType".into(),
                artifact,
            ));
            continue;
        };
        let uses = root
            .features
            .iter()
            .enumerate()
            .map(|(index, id)| (id, format!("/payload/features/{index}"), false))
            .chain(
                root.target
                    .iter()
                    .map(|id| (id, "/payload/target".to_owned(), true)),
            );
        for (id, pointer, target) in uses {
            let role = if target { "target" } else { "feature" };
            let Some(class) = members.get(id.as_str()) else {
                findings.push(finding(
                    UNKNOWN_FEATURE,
                    format!(
                        "{input} has no property {:?} for the model's {role}",
                        id.as_str()
                    ),
                    pointer,
                    artifact,
                ));
                continue;
            };
            let admitted = match class {
                Class::Numeric | Class::Boolean | Class::Enum => true,
                Class::Text => !target,
                Class::Other(_) | Class::Link | Class::Unresolved => false,
            };
            if !admitted {
                let what = match class {
                    Class::Numeric => "a numeric property".to_owned(),
                    Class::Boolean => "a boolean property".to_owned(),
                    Class::Enum => "an enum property".to_owned(),
                    Class::Text => "a text property".to_owned(),
                    Class::Other(kind) => format!("a {kind} property"),
                    Class::Link => "a link, not a property".to_owned(),
                    Class::Unresolved => {
                        "a ref to a property type the set and the base do not declare".to_owned()
                    }
                };
                let admits = if target {
                    "a target is numeric, boolean or enum"
                } else {
                    "a feature is numeric, boolean, enum or text"
                };
                findings.push(finding(
                    FEATURE_KIND,
                    format!("{input}.{} is {what}; {admits}", id.as_str()),
                    pointer,
                    artifact,
                ));
            }
        }
    }
    findings.extend(crate::model_privacy::check(
        &models,
        artifacts,
        base,
        finding,
    ));
    findings
}

/// A named predictions type carries `subject` and `model_version` as text
/// properties and every output as a property of the same type and
/// nullability (see [`norn_ir::ModelPredictions`]).
fn check_predictions(
    catalog: &Catalog,
    root: &ModelRoot,
    artifact: &ArtifactEnvelope,
    findings: &mut Vec<Diagnostic>,
) {
    let Some(predictions) = &root.predictions else {
        return;
    };
    let name = predictions.object_type.as_str();
    let mut push = |message: String, pointer: String| {
        findings.push(finding(PREDICTION_TYPE, message, pointer, artifact));
    };
    let Some(members) = catalog.member_refs(name) else {
        push(
            format!(
                "no object type in the artifact set or the installed base is named {name} for the model's predictions"
            ),
            "/payload/predictions/objectType".into(),
        );
        return;
    };
    for (id, holds) in [
        (PREDICTION_SUBJECT, "the scored input object's id"),
        (PREDICTION_VERSION, "the ml.ModelVersion id"),
    ] {
        let text = match members.get(id) {
            Some(Member::Property(property_type, _)) => matches!(
                catalog.resolve(property_type),
                Some(PropertyType::Text { .. })
            ),
            Some(Member::Field(field)) => matches!(
                field,
                EntityFieldV1::Text { .. } | EntityFieldV1::String { .. }
            ),
            Some(Member::Link) | None => false,
        };
        if !text {
            push(
                format!("{name} has no text property {id:?} ({holds}) for the model's predictions"),
                "/payload/predictions/objectType".into(),
            );
        }
    }
    for (index, output) in root.outputs.iter().enumerate() {
        let id = output.id.as_str();
        let pointer = format!("/payload/outputs/{index}");
        let reason = match members.get(id) {
            None => Some(format!("{name} has no property {id:?}")),
            Some(Member::Link) => Some(format!("{name}.{id} is a link, not a property")),
            Some(Member::Field(_)) => Some(format!(
                "{name}.{id} is an EntityIR 1.0 field; a predictions type declares its columns as properties"
            )),
            Some(Member::Property(property_type, nullable)) => {
                match catalog.resolve(property_type) {
                    None => Some(format!(
                        "{name}.{id} is a ref to a property type the set and the base do not declare"
                    )),
                    Some(resolved) => column_mismatch(&output.property_type, resolved)
                        .map(|why| format!("{name}.{id} {why}"))
                        .or_else(|| {
                            (*nullable != output.nullable).then(|| {
                                format!(
                                    "{name}.{id} is {}nullable, the output is {}nullable",
                                    if *nullable { "" } else { "not " },
                                    if output.nullable { "" } else { "not " }
                                )
                            })
                        }),
                }
            }
        };
        if let Some(reason) = reason {
            push(
                format!("output {id:?} cannot be written to {name}: {reason}"),
                pointer,
            );
        }
    }
}

/// Why a predictions property of type `column` cannot hold values of an
/// output of type `output`, if it cannot: the same kind, a text's format, a
/// decimal's scale and unit (and no dynamic unit), and an enum's values as a
/// superset of the output's.
fn column_mismatch(output: &PropertyType, column: &PropertyType) -> Option<String> {
    let kinds = || {
        Some(format!(
            "is {}, the output is {}",
            column.kind().as_str(),
            output.kind().as_str()
        ))
    };
    match output {
        PropertyType::Text { format: wanted } => {
            let PropertyType::Text { format } = column else {
                return kinds();
            };
            (wanted != format).then(|| "has another text format than the output".into())
        }
        PropertyType::Integer { .. } => (!matches!(column, PropertyType::Integer { .. }))
            .then(kinds)
            .flatten(),
        PropertyType::Boolean {} => (!matches!(column, PropertyType::Boolean {}))
            .then(kinds)
            .flatten(),
        PropertyType::Decimal {
            additivity: _additivity,
            scale: wanted_scale,
            rounding: _rounding,
            unit: wanted_unit,
            currency_source: _currency_source,
            per_unit: _per_unit,
            unit_source: _unit_source,
        } => {
            let PropertyType::Decimal {
                additivity: _additivity,
                scale,
                rounding: _rounding,
                unit,
                currency_source,
                per_unit,
                unit_source,
            } = column
            else {
                return kinds();
            };
            if scale != wanted_scale {
                Some("has another decimal scale than the output".into())
            } else if unit != wanted_unit {
                Some("has another unit than the output".into())
            } else if currency_source.is_some() || per_unit.is_some() || !unit_source.is_empty() {
                Some("carries a currencySource, perUnit or unitSource the output does not".into())
            } else {
                None
            }
        }
        PropertyType::Enum { values: wanted } => {
            let PropertyType::Enum { values } = column else {
                return kinds();
            };
            let held: BTreeSet<&str> = values.iter().map(|value| value.value.as_str()).collect();
            wanted
                .iter()
                .find(|value| !held.contains(value.value.as_str()))
                .map(|value| format!("does not declare the output's value {:?}", value.value))
        }
        // Not an output column kind (refused locally); never a match.
        PropertyType::DecimalBinding { .. }
        | PropertyType::Timestamp { .. }
        | PropertyType::Date {}
        | PropertyType::Duration {}
        | PropertyType::Record { .. }
        | PropertyType::JsonExt { .. }
        | PropertyType::Ref { .. } => kinds(),
    }
}

fn finding(
    code: &str,
    message: String,
    pointer: String,
    artifact: &ArtifactEnvelope,
) -> Diagnostic {
    Diagnostic {
        code: DiagnosticCode::new(code),
        severity: DiagnosticSeverity::Error,
        message,
        span: Some(Span::pointer(pointer)),
        artifact_id: Some(artifact.artifact_id.clone()),
        remediation: Some(norn_ir::DiagnosticRemediation::FixDefinition),
        witness: None,
    }
}
