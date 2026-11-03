//! PROOF-GUARANTEE-1 / PROOF-BIND-1: typed results, never proof authority.
use super::evidence::{EvidencePins, EvidenceTool};
use crate::UnitRef;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Stable per-law guarantees document format.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub enum GuaranteesFormat {
    /// First typed guarantees format.
    #[serde(rename = "norn/guarantees@1")]
    V1,
}

/// A scoped result; Unknown always explains the missing evidence.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "kebab-case", deny_unknown_fields)]
pub enum GuaranteeVerdict {
    /// All eligible goals pass within the explicitly retained bounds.
    BoundedPass,
    /// An actual checked counterexample refutes a generated goal.
    Refuted,
    /// No qualified result is available.
    Unknown {
        /// Concrete unsupported, stale, failed or unavailable boundary.
        reason: String,
    },
}

/// Distinct assurance dimensions; none implies an unrestricted proof.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "kebab-case", deny_unknown_fields)]
pub enum GuaranteeLevel {
    /// Exhaustive search over the pinned finite profile.
    #[serde(rename = "exhaustive-finite/bounded")]
    ExhaustiveFiniteBounded,
    /// Evidence of actual runtime enforcement, when separately supplied.
    RuntimeEnforced,
    /// Evidence of observed reference parity, when separately supplied.
    ParityTested,
    /// No evidence at this level is available.
    Unknown {
        /// Concrete reason, rather than an inferred guarantee.
        reason: String,
    },
}

/// A real independently supplied link, or an explicit unavailable reference.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "kebab-case", deny_unknown_fields)]
pub enum GuaranteeReference {
    /// The server has not supplied this evidence.
    NotYet {
        /// Missing evidence or integration boundary.
        reason: String,
    },
    /// Immutable evidence the server actually supplied.
    Available {
        /// Evidence location, not an execution attestation by itself.
        uri: String,
        /// Exact referenced bytes.
        sha256: String,
    },
}

/// An exact scalar coefficient, interpreted at its declared decimal scale/unit.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct GuaranteeScalar {
    /// Signed integer text; never a binary64 quantity.
    #[serde(with = "integer_text")]
    #[schemars(with = "String", regex(pattern = r"^-?(0|[1-9][0-9]*)$"))]
    pub coefficient: i64,
    /// Fractional decimal digits from the declaration.
    pub scale: u32,
    /// Effective declared unit; absent means dimensionless.
    pub unit: Option<UnitRef>,
}

/// One typed counterexample state; absent objects have no scalar values.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct GuaranteeState {
    /// Whether the modeled object exists.
    pub exists: bool,
    /// Exact quantities keyed by source property name.
    pub values: BTreeMap<String, GuaranteeScalar>,
}

/// Distinguishes a refutation from a successful non-vacuity witness.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum GuaranteeScenarioKind {
    /// Actual invariant-violating writer proposal.
    Counterexample,
    /// Actual reachable law-satisfying state.
    NonVacuityWitness,
}

/// A generated writer proposal before runtime law enforcement.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct GuaranteeScenario {
    /// Why the solver returned this trace.
    pub kind: GuaranteeScenarioKind,
    /// Actual source writer artifact.
    pub writer_artifact_id: String,
    /// Singleton identity retained from the finite profile.
    pub identity: String,
    /// Actual checked trace, in transition order.
    pub states: Vec<GuaranteeState>,
}

/// One generated obligation, including every unsupported goal.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct GoalGuarantee {
    /// Index in the retained Challenge.
    pub index: u32,
    /// Actual generated obligation kind.
    pub goal_kind: String,
    /// Whether this goal is inside the declared backend fragment.
    pub eligible: bool,
    /// Scoped verdict or explicit Unknown reason.
    pub verdict: GuaranteeVerdict,
    /// Actual evidence level.
    pub level: GuaranteeLevel,
    /// Actual checked result pins; absent for unsupported goals.
    pub pins: Option<EvidencePins>,
    /// Exact independently retained result digest.
    pub result_sha256: Option<String>,
    /// Actual trace when a counterexample or reachability witness was returned.
    pub scenario: Option<GuaranteeScenario>,
}

/// The presentation contract consumed by Studio; identity alone grants no verdict.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct GuaranteesDocument {
    /// Versioned wire contract.
    pub format: GuaranteesFormat,
    /// Exact governing law artifact.
    pub law_artifact_id: String,
    /// Declared LawIR identifier.
    pub law_id: String,
    /// Governing artifact version.
    pub law_version: String,
    /// Result for the eligible bounded profile, not all possible application goals.
    pub verdict: GuaranteeVerdict,
    /// Independently qualified evidence level.
    pub level: GuaranteeLevel,
    /// Exact source/closure/statement identity.
    pub challenge_sha256: String,
    /// Complete pinned bounds/assumptions JSON bytes, without a lossy projection.
    pub bounds_json: String,
    /// Actual tools used by the trusted driver.
    pub tools: Vec<EvidenceTool>,
    /// Every generated obligation, including individually unsupported goals.
    pub goals: Vec<GoalGuarantee>,
    /// Complete generated denominator, including unsupported obligations.
    pub total: u32,
    /// Actual eligible denominator, independent of the total goal inventory.
    pub eligible: u32,
    /// Eligible goals discharged by the checked driver.
    pub discharged: u32,
    /// Separately supplied runtime enforcement evidence, or explicitly not yet.
    pub runtime_enforcement: GuaranteeReference,
    /// Separately supplied application/reference parity evidence, or not yet.
    pub parity: GuaranteeReference,
}

/// Additive schema for the exact typed document; no existing artifact schema changes.
#[must_use]
pub fn guarantees_document_schema() -> schemars::Schema {
    schemars::schema_for!(GuaranteesDocument)
}

mod integer_text {
    use serde::{Deserialize, Deserializer, Serializer};
    pub fn serialize<S: Serializer>(value: &i64, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&value.to_string())
    }
    pub fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<i64, D::Error> {
        let text = String::deserialize(deserializer)?;
        let value: i64 = text.parse().map_err(serde::de::Error::custom)?;
        if value.to_string() != text {
            return Err(serde::de::Error::custom("canonical integer text required"));
        }
        Ok(value)
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, reason = "typed wire contract fixtures")]
mod tests {
    use super::{GuaranteeLevel, GuaranteeScalar, GuaranteeVerdict};
    use proptest::prelude::*;
    use serde_json::json;

    #[test]
    fn unknown_requires_reason_and_levels_never_accept_proved() {
        for value in [
            json!({"kind":"unknown"}),
            json!({"kind":"proved"}),
            json!({"kind":"bounded-pass","reason":"extra"}),
        ] {
            assert!(serde_json::from_value::<GuaranteeVerdict>(value).is_err());
        }
        assert!(serde_json::from_value::<GuaranteeLevel>(json!({"kind":"proved"})).is_err());
        assert_eq!(
            serde_json::from_value::<GuaranteeVerdict>(
                json!({"kind":"unknown","reason":"compiler timeout"})
            )
            .expect("explicit reason"),
            GuaranteeVerdict::Unknown {
                reason: "compiler timeout".into()
            }
        );
        for coefficient in [
            json!(1),
            json!(1.25),
            json!("1.25"),
            json!("+1"),
            json!("01"),
        ] {
            assert!(
                serde_json::from_value::<GuaranteeScalar>(
                    json!({"coefficient":coefficient,"scale":0,"unit":null})
                )
                .is_err()
            );
        }
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(32))]
        #[test]
        fn guarantee_scalar_roundtrip_preserves_exact_coefficient(coefficient in any::<i64>(), scale in 0..=28_u32) {
            let value = GuaranteeScalar { coefficient, scale,
                unit: Some(crate::UnitRef { system: crate::UnitSystem::Ucum, code: "g".into() }) };
            let bytes = serde_json::to_vec(&value).expect("wire bytes");
            prop_assert_eq!(serde_json::from_slice::<GuaranteeScalar>(&bytes).expect("roundtrip"), value);
        }
    }
}
