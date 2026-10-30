//! Typed, closed input contract for theorem generation. This describes inputs;
//! it does not validate them or claim a proof has been produced.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::members::present_non_null;
use crate::{LawIR, LawResponse, LawScope, ZenExpression};

/// Version of the theorem input wire contract.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub enum TheoremInputSchemaVersion {
    /// The first theorem input contract.
    #[serde(rename = "norn.theorem-input/0.1")]
    NornTheoremInput01,
}

/// Supported semantic profile for theorem inputs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub enum TheoremSemanticProfile {
    /// Norn's deontic law semantics version 0.1.
    #[serde(rename = "norn.deontic/0.1")]
    NornDeontic01,
}

/// A typed request for proving a restricted law over an explicit transition model.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TheoremInput {
    /// Contract version.
    pub schema_version: TheoremInputSchemaVersion,
    /// Exact source law identity.
    pub law_id: String,
    /// Semantic interpretation profile.
    pub semantic_profile: TheoremSemanticProfile,
    /// Subject object type.
    pub subject_type: String,
    /// Applicability scope copied from the law.
    pub scope: LawScope,
    /// Exact state-transition model identities.
    pub state_model: TheoremStateModel,
    /// Variables exposed to expressions in the theorem context.
    pub variables: Vec<TheoremVariable>,
    /// Explicit premises with source attribution.
    pub premises: Vec<TheoremPremise>,
    /// Restricted deontic conclusion.
    pub conclusion: TheoremConclusion,
    /// Source location for this theorem input.
    pub source: SourceRef,
    /// Versioned authority artifact identity.
    pub authority: TheoremAuthority,
    /// Exact law being lowered, restricted to this contract's closed profile.
    #[serde(deserialize_with = "closed_source_law")]
    #[schemars(with = "LawIR", transform = closed_source_law_schema)]
    pub lowered_law: LawIR,
    /// Canonical exact-LawIR hash supplied by the producer.
    pub lowered_law_hash: String,
    /// Requested evidence form; this field grants no proof status.
    pub evidence_target: TheoremEvidenceTarget,
}

fn closed_source_law<'de, D>(deserializer: D) -> Result<LawIR, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let law = LawIR::deserialize(deserializer)?;
    if law.timing.is_some()
        || !matches!(
            law.modality,
            crate::LawModality::Always {} | crate::LawModality::Never {}
        )
    {
        return Err(serde::de::Error::custom(
            "theorem-input/0.1 supports only always/never source laws without timing",
        ));
    }
    Ok(law)
}

fn closed_source_law_schema(schema: &mut schemars::Schema) {
    // Keep the canonical LawIR reference, adding only this input profile's
    // restrictions. General LawIR continues to support its existing vocabulary.
    schema.insert(
        "allOf".to_owned(),
        serde_json::json!([{
            "properties": {"modality": {"properties": {"kind": {"enum": ["always", "never"]}}}},
            "not": {"required": ["timing"]}
        }]),
    );
}

/// Pinned state-model inputs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TheoremStateModel {
    /// State-model artifact identity.
    pub artifact_id: String,
    /// Hash of the exact definition set.
    pub definition_set_hash: String,
    /// Hash of the exact transition model.
    pub transition_model_hash: String,
    /// State semantics version.
    pub semantics_version: String,
}

/// Variable and its closed domain declaration.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TheoremVariable {
    /// Stable variable name.
    pub name: String,
    /// Variable type.
    #[serde(rename = "type")]
    pub variable_type: TheoremVariableType,
    /// Admitted state domain.
    pub domain: TheoremVariableDomain,
}

/// Variable type descriptor.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TheoremVariableType {
    /// Exact ontology object type.
    pub object_type: String,
}

/// Admitted theorem variable domain.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub enum TheoremVariableDomain {
    /// States reachable from a nonempty initial-state witness.
    #[serde(rename = "reachable_states")]
    ReachableStates,
}

/// The kind of a premise, retaining its explicit source and expression.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum TheoremPremiseKind {
    /// The condition attached to the source law.
    LawCondition,
    /// An existing action guard.
    ActionGuard,
    /// An existing model invariant.
    ModelInvariant,
    /// An explicit modeling assumption.
    Assumption,
}

/// One premise and its exact source reference.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TheoremPremise {
    /// Premise classification.
    pub kind: TheoremPremiseKind,
    /// Typed ZEN expression text.
    pub expression: ZenExpression,
    /// Artifact and source location that supplies this premise.
    pub source: SourceRef,
}

/// Restricted always/never conclusion.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TheoremConclusion {
    /// Pointwise deontic modality admitted by this contract.
    pub modality: TheoremConclusionModality,
    /// Existing law response IR.
    pub response: LawResponse,
}

/// Supported conclusion modalities.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum TheoremConclusionModality {
    /// Response holds in every applicable state.
    Always,
    /// Response holds in no applicable state.
    Never,
}

/// Reference to an artifact and optional source-text span.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SourceRef {
    /// Exact source artifact identity.
    pub artifact_id: String,
    /// JSON Pointer or source member pointer.
    pub pointer: String,
    /// Optional source-text coordinates.
    #[serde(
        default,
        deserialize_with = "present_non_null",
        skip_serializing_if = "Option::is_none"
    )]
    #[schemars(with = "SourceSpan")]
    pub span: Option<SourceSpan>,
}

/// One-based source-text span coordinates.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SourceSpan {
    /// Source file path.
    pub path: String,
    /// First line, one-based.
    pub start_line: u32,
    /// First column, one-based.
    pub start_column: u32,
    /// Last line, one-based.
    pub end_line: u32,
    /// Last column, one-based.
    pub end_column: u32,
}

/// Authority artifact version and digest.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TheoremAuthority {
    /// Exact authority artifact identity.
    pub artifact_id: String,
    /// Exact authority artifact version.
    pub version: String,
    /// Authority artifact digest.
    pub digest: String,
}

/// Requested evidence target; it is not a certificate or status.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TheoremEvidenceTarget {
    /// Requested evidence label.
    pub label: TheoremEvidenceLabel,
    /// Requested proof backend kind.
    pub kind: TheoremEvidenceKind,
}

/// Literal requested evidence label.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub enum TheoremEvidenceLabel {
    /// A proof request targeting a proved result.
    #[serde(rename = "proved")]
    Proved,
}

/// Literal requested evidence backend.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub enum TheoremEvidenceKind {
    /// Lean kernel evidence target.
    #[serde(rename = "lean-kernel")]
    LeanKernel,
}

#[cfg(test)]
mod tests {
    use schemars::schema_for;
    use serde_json::{Value, json};

    use super::*;

    fn fixture() -> Value {
        let laws: Value = serde_json::from_str(include_str!("../../../fixtures/law-ir/laws.json"))
            .expect("law fixtures");
        json!({
            "schemaVersion": "norn.theorem-input/0.1",
            "lawId": "erpnext.no_unpaid_ship",
            "semanticProfile": "norn.deontic/0.1",
            "subjectType": "erpnext.SalesOrder",
            "scope": laws[0]["payload"]["scope"],
            "stateModel": {
                "artifactId": "erpnext.SalesOrder.state-model",
                "definitionSetHash": "sha256:definitions",
                "transitionModelHash": "sha256:transitions",
                "semanticsVersion": "norn.state/0.1"
            },
            "variables": [{
                "name": "order",
                "type": {"objectType": "erpnext.SalesOrder"},
                "domain": "reachable_states"
            }],
            "premises": [{
                "kind": "model_invariant",
                "expression": "self.status != 'shipped' or self.paid == true",
                "source": {"artifactId": "erpnext.SalesOrder.entity", "pointer": "/invariants/0"}
            }],
            "conclusion": {
                "modality": "always",
                "response": laws[0]["payload"]["response"]
            },
            "source": {"artifactId": "erpnext.no_unpaid_ship.law", "pointer": "/payload"},
            "authority": {
                "artifactId": "erpnext.controls.authority",
                "version": "0.1.0",
                "digest": "sha256:authority"
            },
            "loweredLaw": laws[0]["payload"],
            "loweredLawHash": "sha256:law-ir",
            "evidenceTarget": {"label": "proved", "kind": "lean-kernel"}
        })
    }

    #[test]
    fn theorem_input_round_trips_and_schema_is_versioned() {
        let value = fixture();
        let input: TheoremInput = serde_json::from_value(value.clone()).expect("typed input");
        assert_eq!(serde_json::to_value(input).expect("encoded input"), value);
        let schema = serde_json::to_value(schema_for!(TheoremInput)).expect("schema");
        assert!(schema.to_string().contains("norn.theorem-input/0.1"));
        assert!(schema.to_string().contains("norn.deontic/0.1"));
        let source_law = &schema["properties"]["loweredLaw"];
        assert_eq!(source_law["allOf"][0]["not"]["required"], json!(["timing"]));
        assert_eq!(
            source_law["allOf"][0]["properties"]["modality"]["properties"]["kind"]["enum"],
            json!(["always", "never"])
        );
    }

    #[test]
    fn theorem_input_refuses_timing_eventually_and_unknown_members() {
        let mut invalid = fixture();
        invalid["conclusion"]["modality"] = json!("eventually");
        assert!(serde_json::from_value::<TheoremInput>(invalid).is_err());

        let mut invalid = fixture();
        invalid["conclusion"]["timing"] = json!({"kind": "within", "millis": "1000"});
        assert!(serde_json::from_value::<TheoremInput>(invalid).is_err());

        let mut invalid = fixture();
        invalid["loweredLaw"]["modality"] = json!({"kind": "eventually"});
        assert!(serde_json::from_value::<TheoremInput>(invalid).is_err());

        let mut invalid = fixture();
        invalid["loweredLaw"]["timing"] = json!({"kind": "within", "millis": "1000"});
        assert!(serde_json::from_value::<TheoremInput>(invalid).is_err());

        let mut invalid = fixture();
        invalid["unexpected"] = json!(true);
        assert!(serde_json::from_value::<TheoremInput>(invalid).is_err());

        let mut invalid = fixture();
        invalid["stateModel"]["unexpected"] = json!(true);
        assert!(serde_json::from_value::<TheoremInput>(invalid).is_err());
    }
}
