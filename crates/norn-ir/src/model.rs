//! `ModelIR` (#1171 M1): a typed definition of a predictive model whose
//! internals stay opaque.
//!
//! A `model` artifact declares what a model reads and what it writes, never
//! how it computes: its input (an object type, for now), the input properties
//! it uses as features, an optional supervised target, the typed prediction
//! columns it outputs, the trust its outputs carry ([`TrustLevel`]) and the
//! closed contracts every output must meet before it is written. `method` is
//! display text only and carries no semantics.
//!
//! Admission (`norn-checker`): the payload decodes into the strict
//! [`ModelRoot`]; ids are well-formed and unique; trust is `opaque` or
//! `contract_checked`, the latter with at least one contract; each contract
//! applies to its output's type. Over an artifact set, the input object type
//! resolves in the candidate set or the installed base, and every feature and
//! the target is a numeric, boolean or categorical property of it; a named
//! predictions type ([`ModelPredictions`]) carries every output as a property
//! of the same type and nullability.
use crate::entity_v2::object::{ObjectTypeName, PropertyId};
use crate::entity_v2::property::{DecimalLiteral, PropertyType};
use crate::members::present_non_null;
use crate::trust::TrustLevel;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Grammar of a [`ModelRoot::model_id`]: two or more dotted snake_case
/// segments (`sales.churn_model`).
pub const MODEL_ID_PATTERN: &str = r"^[a-z][a-z0-9_]*(\.[a-z][a-z0-9_]*)+$";

/// What a model reads. A tagged union so later inputs (a transform, a
/// dataset) are additive variants.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum ModelInput {
    /// The live objects of one object type, by qualified name
    /// (`sales.Customer`). Features and the target are its properties.
    ObjectType {
        /// The input object type.
        object_type: ObjectTypeName,
    },
}

impl ModelInput {
    /// The input object type's qualified name.
    #[must_use]
    pub fn object_type(&self) -> &ObjectTypeName {
        match self {
            Self::ObjectType { object_type } => object_type,
        }
    }
}

/// One prediction column a model writes.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ModelOutput {
    /// Unique column id (`[a-z][a-z0-9_]*`).
    pub id: PropertyId,
    /// The column's value type, in the property type representation
    /// (`EntityIR`); a quantity carries its unit inside a `decimal` type, as a
    /// property does.
    #[serde(rename = "type")]
    pub property_type: PropertyType,
    /// Whether the model may leave the column empty for an object.
    pub nullable: bool,
}

/// A closed check on one output's values. There are no free expressions.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum ContractRule {
    /// Inclusive bounds on a numeric output; at least one bound.
    Range {
        /// Inclusive lower bound.
        #[serde(
            default,
            deserialize_with = "present_non_null",
            skip_serializing_if = "Option::is_none"
        )]
        #[schemars(with = "DecimalLiteral")]
        min: Option<DecimalLiteral>,
        /// Inclusive upper bound.
        #[serde(
            default,
            deserialize_with = "present_non_null",
            skip_serializing_if = "Option::is_none"
        )]
        #[schemars(with = "DecimalLiteral")]
        max: Option<DecimalLiteral>,
    },
    /// Every object gets a value.
    NotNull {},
    /// A numeric output is never NaN or infinite.
    Finite {},
    /// A categorical (`enum` or `text`) output takes one of these values.
    OneOf {
        /// The admitted values; non-empty and unique.
        values: Vec<String>,
    },
}

impl ContractRule {
    /// The wire `kind` tag.
    #[must_use]
    pub const fn kind(&self) -> &'static str {
        match self {
            Self::Range { .. } => "range",
            Self::NotNull {} => "notNull",
            Self::Finite {} => "finite",
            Self::OneOf { .. } => "oneOf",
        }
    }
}

/// A named contract on one output.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ModelContract {
    /// Unique contract id (`[a-z][a-z0-9_]*`).
    pub id: PropertyId,
    /// The output it checks, by [`ModelOutput::id`].
    pub output: PropertyId,
    /// The check.
    pub rule: ContractRule,
}

/// Where a model's predictions are written (#1171 M4): one object per scored
/// input object, of a typed object type.
///
/// Admission (`norn-checker`, over a set): the type resolves in the candidate
/// set or the installed base; it has a text property `subject` (the scored
/// input object's id) and a text property `model_version` (the
/// `ml.ModelVersion` id that produced it); and every output of the model is a
/// property of it with the same id, the same type (a decimal's scale and unit,
/// an enum's values as a superset) and the same nullability.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ModelPredictions {
    /// The predictions object type, by qualified name (`sales.ChurnPrediction`).
    pub object_type: ObjectTypeName,
}

/// How the model was built: display text only.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ModelMethod {
    /// The method (`logistic regression`).
    pub label: String,
    /// The tool or method version (`sklearn 1.6`).
    #[serde(
        default,
        deserialize_with = "present_non_null",
        skip_serializing_if = "Option::is_none"
    )]
    #[schemars(with = "String")]
    pub version: Option<String>,
}

/// The strict payload of a `model` artifact.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ModelRoot {
    /// Dotted model id ([`MODEL_ID_PATTERN`], `sales.churn_model`).
    pub model_id: String,
    /// Human-readable title.
    pub title: String,
    /// The model artifact's version (SemVer 2.0.0).
    pub version: String,
    /// What the model reads.
    pub input: ModelInput,
    /// Input properties the model uses, in order; non-empty and unique.
    pub features: Vec<PropertyId>,
    /// The supervised target property; omitted for a scoring-only model.
    #[serde(
        default,
        deserialize_with = "present_non_null",
        skip_serializing_if = "Option::is_none"
    )]
    #[schemars(with = "PropertyId")]
    pub target: Option<PropertyId>,
    /// Prediction columns; non-empty, ids unique.
    pub outputs: Vec<ModelOutput>,
    /// How far the outputs can be trusted without running the model.
    pub trust: TrustLevel,
    /// Checks every output must meet before it is written. Omitted when empty.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub contracts: Vec<ModelContract>,
    /// The typed object type predictions are written to; omitted when the
    /// model names none.
    #[serde(
        default,
        deserialize_with = "present_non_null",
        skip_serializing_if = "Option::is_none"
    )]
    #[schemars(with = "ModelPredictions")]
    pub predictions: Option<ModelPredictions>,
    /// Display-only description of how the model was built.
    pub method: ModelMethod,
    /// Reserved versioned extension slots, as on other artifact roots.
    #[serde(
        default,
        deserialize_with = "present_non_null",
        skip_serializing_if = "Option::is_none"
    )]
    #[schemars(with = "crate::JsonObject")]
    pub extensions: Option<crate::JsonObject>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_model_round_trips_in_its_declared_form() {
        let wire = serde_json::json!({
            "modelId": "sales.churn_model",
            "title": "Churn",
            "version": "1.0.0",
            "input": { "kind": "objectType", "objectType": "sales.Customer" },
            "features": ["tenure_months", "plan"],
            "target": "churned",
            "outputs": [
                { "id": "churn_probability", "type": { "kind": "decimal", "scale": 4 }, "nullable": false }
            ],
            "trust": "contract_checked",
            "contracts": [
                { "id": "probability", "output": "churn_probability",
                  "rule": { "kind": "range", "min": "0", "max": "1" } },
                { "id": "present", "output": "churn_probability", "rule": { "kind": "notNull" } }
            ],
            "predictions": { "objectType": "sales.ChurnPrediction" },
            "method": { "label": "logistic regression", "version": "sklearn 1.6" }
        });
        let root: Result<ModelRoot, _> = serde_json::from_value(wire.clone());
        let root = root.ok();
        assert_eq!(
            root.as_ref().map(|root| root.input.object_type().as_str()),
            Some("sales.Customer")
        );
        assert_eq!(
            root.and_then(|root| serde_json::to_value(root).ok()),
            Some(wire)
        );
    }

    #[test]
    fn an_unknown_member_is_refused() {
        let wire = serde_json::json!({
            "modelId": "sales.churn_model", "title": "Churn", "version": "1.0.0",
            "input": { "kind": "objectType", "objectType": "sales.Customer" },
            "features": ["plan"], "outputs": [], "trust": "opaque",
            "method": { "label": "rules" }, "script": "import os"
        });
        assert!(serde_json::from_value::<ModelRoot>(wire).is_err());
    }
}
