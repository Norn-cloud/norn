//! PROOF-GUARANTEE-1 / PROOF-BIND-1: typed results, never proof authority.
use super::evidence::{EvidenceRecord, EvidenceTool};
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
    BoundedPass {},
    /// An actual checked counterexample refutes a generated goal.
    Refuted {},
    /// No qualified result is available.
    Unknown {
        /// Concrete unsupported, stale, failed or unavailable boundary.
        #[serde(deserialize_with = "nonblank_reason")]
        #[schemars(regex(pattern = r"\S"))]
        reason: String,
    },
}

/// Distinct assurance dimensions; none implies an unrestricted proof.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "kebab-case", deny_unknown_fields)]
pub enum GuaranteeLevel {
    /// Exhaustive search over the pinned finite profile.
    #[serde(rename = "exhaustive-finite/bounded")]
    ExhaustiveFiniteBounded {},
    /// Evidence of actual runtime enforcement, when separately supplied.
    RuntimeEnforced {},
    /// Evidence of observed reference parity, when separately supplied.
    ParityTested {},
    /// No evidence at this level is available.
    Unknown {
        /// Concrete reason, rather than an inferred guarantee.
        #[serde(deserialize_with = "nonblank_reason")]
        #[schemars(regex(pattern = r"\S"))]
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
    #[schemars(schema_with = "integer_text::schema")]
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
#[derive(Debug, Clone, PartialEq, Eq, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[schemars(transform = ineligible_is_unknown)]
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
    /// Actual bound result bytes, pins and digest; absent for unsupported goals.
    /// Presentation never replaces the controller's independent result pin.
    pub evidence: Option<EvidenceRecord>,
    /// Actual trace when a counterexample or reachability witness was returned.
    pub scenario: Option<GuaranteeScenario>,
}

#[derive(Serialize, Deserialize)]
#[serde(
    remote = "GoalGuarantee",
    rename_all = "camelCase",
    deny_unknown_fields
)]
struct GoalGuaranteeWire {
    index: u32,
    goal_kind: String,
    eligible: bool,
    verdict: GuaranteeVerdict,
    level: GuaranteeLevel,
    evidence: Option<EvidenceRecord>,
    scenario: Option<GuaranteeScenario>,
}

impl GoalGuarantee {
    fn eligibility_valid(&self) -> bool {
        self.eligible
            || (matches!(self.verdict, GuaranteeVerdict::Unknown { .. })
                && matches!(self.level, GuaranteeLevel::Unknown { .. }))
    }
}
impl Serialize for GoalGuarantee {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        if !self.eligibility_valid() {
            return Err(serde::ser::Error::custom(
                "ineligible goals require Unknown verdict and level",
            ));
        }
        GoalGuaranteeWire::serialize(self, serializer)
    }
}
impl<'de> Deserialize<'de> for GoalGuarantee {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let goal = GoalGuaranteeWire::deserialize(deserializer)?;
        if !goal.eligibility_valid() {
            return Err(serde::de::Error::custom(
                "ineligible goals require Unknown verdict and level",
            ));
        }
        Ok(goal)
    }
}
fn ineligible_is_unknown(schema: &mut schemars::Schema) {
    schema.insert(
        "oneOf".into(),
        serde_json::json!([
            {"properties":{"eligible":{"const":true}},"required":["eligible"]},
            {"properties":{
                "eligible":{"const":false},
                "verdict":{"properties":{"kind":{"const":"unknown"}}},
                "level":{"properties":{"kind":{"const":"unknown"}}}
            },"required":["eligible"]}
        ]),
    );
}

/// The presentation contract consumed by Studio; identity alone grants no verdict.
#[derive(Debug, Clone, PartialEq, Eq, JsonSchema)]
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

#[derive(Serialize, Deserialize)]
#[serde(
    remote = "GuaranteesDocument",
    rename_all = "camelCase",
    deny_unknown_fields
)]
struct GuaranteesDocumentWire {
    format: GuaranteesFormat,
    law_artifact_id: String,
    law_id: String,
    law_version: String,
    verdict: GuaranteeVerdict,
    level: GuaranteeLevel,
    challenge_sha256: String,
    bounds_json: String,
    tools: Vec<EvidenceTool>,
    goals: Vec<GoalGuarantee>,
    total: u32,
    eligible: u32,
    discharged: u32,
    runtime_enforcement: GuaranteeReference,
    parity: GuaranteeReference,
}
impl GuaranteesDocument {
    fn counts_valid(&self) -> bool {
        u32::try_from(self.goals.len()).ok() == Some(self.total)
            && u32::try_from(self.goals.iter().filter(|goal| goal.eligible).count()).ok()
                == Some(self.eligible)
            && self.discharged <= self.eligible
    }
}
impl Serialize for GuaranteesDocument {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        if !self.counts_valid() {
            return Err(serde::ser::Error::custom(
                "guarantees counts contradict goal inventory",
            ));
        }
        GuaranteesDocumentWire::serialize(self, serializer)
    }
}
impl<'de> Deserialize<'de> for GuaranteesDocument {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let document = GuaranteesDocumentWire::deserialize(deserializer)?;
        if !document.counts_valid() {
            return Err(serde::de::Error::custom(
                "guarantees counts contradict goal inventory",
            ));
        }
        Ok(document)
    }
}

/// Additive schema for the exact typed document; no existing artifact schema changes.
#[must_use]
pub fn guarantees_document_schema() -> schemars::Schema {
    schemars::schema_for!(GuaranteesDocument)
}

fn nonblank_reason<'de, D: serde::Deserializer<'de>>(deserializer: D) -> Result<String, D::Error> {
    let text = String::deserialize(deserializer)?;
    if text.trim().is_empty() {
        return Err(serde::de::Error::custom("nonblank Unknown reason required"));
    }
    Ok(text)
}

mod integer_text {
    use serde::{Deserialize, Deserializer, Serializer};
    pub(super) fn schema(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
        // Each branch fixes the first digit below the bound, retaining the prefix.
        fn magnitude(maximum: u64) -> String {
            let digits = maximum.to_string();
            let mut alternatives = vec!["[1-9][0-9]{0,17}".to_owned(), digits.clone()];
            let mut prefix = String::new();
            for (index, digit) in digits.bytes().enumerate() {
                let lower = if index == 0 { b'1' } else { b'0' };
                if digit > lower {
                    alternatives.push(format!(
                        "{}[{}-{}][0-9]{{{}}}",
                        prefix,
                        char::from(lower),
                        char::from(digit.saturating_sub(1)),
                        digits.len().saturating_sub(index).saturating_sub(1)
                    ));
                }
                prefix.push(char::from(digit));
            }
            format!("({})", alternatives.join("|"))
        }
        // ECMAScript `$` admits a final line terminator; require actual end of input.
        schemars::json_schema!({"type": "string", "pattern": format!("^(0|{}|-{})(?![\\s\\S])",
            magnitude(i64::MAX.unsigned_abs()), magnitude(i64::MIN.unsigned_abs()))})
    }
    pub(super) fn serialize<S: Serializer>(value: &i64, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&value.to_string())
    }
    pub(super) fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<i64, D::Error> {
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
            assert!(
                serde_json::from_value::<GuaranteeVerdict>(value.clone()).is_err(),
                "{value}"
            );
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
            json!("-0"),
            json!("1\n"),
            json!("9223372036854775808"),
            json!("-9223372036854775809"),
        ] {
            assert!(
                serde_json::from_value::<GuaranteeScalar>(
                    json!({"coefficient":coefficient,"scale":0,"unit":null})
                )
                .is_err()
            );
        }
    }

    #[test]
    fn unknown_reasons_refuse_empty_and_whitespace() {
        for reason in ["", " ", "\t\n", "\u{2003}"] {
            let value = json!({"kind":"unknown", "reason":reason});
            assert!(serde_json::from_value::<GuaranteeVerdict>(value.clone()).is_err());
            assert!(serde_json::from_value::<GuaranteeLevel>(value).is_err());
        }
        for coefficient in [i64::MIN, i64::MAX] {
            let value = GuaranteeScalar {
                coefficient,
                scale: 0,
                unit: None,
            };
            assert_eq!(
                serde_json::from_slice::<GuaranteeScalar>(
                    &norn_canonical::serialize_json_verbatim(&value).expect("encode")
                )
                .expect("decode"),
                value
            );
        }
    }

    #[test]
    fn ineligible_goals_require_unknown_on_both_wire_boundaries() {
        use super::GoalGuarantee;
        let unknown = json!({"kind":"unknown","reason":"outside qualified fragment"});
        for eligible in [false, true] {
            for verdict in [
                unknown.clone(),
                json!({"kind":"bounded-pass"}),
                json!({"kind":"refuted"}),
            ] {
                for level in [
                    unknown.clone(),
                    json!({"kind":"exhaustive-finite/bounded"}),
                    json!({"kind":"runtime-enforced"}),
                    json!({"kind":"parity-tested"}),
                ] {
                    let input = json!({"index":0,"goalKind":"preservation","eligible":eligible,"verdict":verdict,"level":level,"evidence":null,"scenario":null});
                    let valid = eligible || (verdict == unknown && level == unknown);
                    assert_eq!(
                        serde_json::from_value::<GoalGuarantee>(input.clone()).is_ok(),
                        valid
                    );
                    let mut constructed:GoalGuarantee=serde_json::from_value(json!({"eligible":true,"index":0,"goalKind":"preservation","verdict":verdict,"level":level,"evidence":null,"scenario":null})).expect("eligible fixture");
                    constructed.eligible = eligible;
                    assert_eq!(serde_json::to_value(&constructed).is_ok(), valid);
                }
            }
        }
    }

    fn valid_document() -> super::GuaranteesDocument {
        let unknown = json!({"kind":"unknown","reason":"outside qualified fragment"});
        let not_yet = json!({"kind":"not-yet","reason":"not supplied"});
        serde_json::from_value(json!({
            "format":"norn/guarantees@1","lawArtifactId":"test.law","lawId":"test",
            "lawVersion":"1","verdict":unknown,"level":unknown,"challengeSha256":"retained",
            "boundsJson":"{}","tools":[],"total":3,"eligible":2,"discharged":1,
            "runtimeEnforcement":not_yet,"parity":not_yet,"goals":[
                {"index":0,"goalKind":"initialization","eligible":true,"verdict":{"kind":"bounded-pass"},"level":{"kind":"exhaustive-finite/bounded"},"evidence":null,"scenario":null},
                {"index":1,"goalKind":"preservation","eligible":true,"verdict":unknown,"level":unknown,"evidence":null,"scenario":null},
                {"index":2,"goalKind":"unsupported","eligible":false,"verdict":unknown,"level":unknown,"evidence":null,"scenario":null}
            ]
        })).expect("coherent complete inventory")
    }
    fn count_refusal(field: &str, bad: u32, change: fn(&mut super::GuaranteesDocument, u32)) {
        let mut document = valid_document();
        let mut wire = serde_json::to_value(&document).expect("valid document encodes");
        change(&mut document, bad);
        *wire.get_mut(field).expect("retained count field") = json!(bad);
        assert!(serde_json::to_value(&document).is_err(), "{field}={bad}");
        assert!(
            serde_json::from_value::<super::GuaranteesDocument>(wire).is_err(),
            "{field}={bad}"
        );
    }
    #[test]
    fn document_total_matches_complete_goal_inventory_at_both_wire_boundaries() {
        for bad in [2, 4, u32::MAX] {
            count_refusal("total", bad, |document, count| document.total = count);
        }
    }
    #[test]
    fn document_eligible_matches_goal_flags_at_both_wire_boundaries() {
        for bad in [1, 3, u32::MAX] {
            count_refusal("eligible", bad, |document, count| document.eligible = count);
        }
    }
    #[test]
    fn document_discharged_cannot_exceed_eligible_at_both_wire_boundaries() {
        for bad in [3, u32::MAX] {
            count_refusal("discharged", bad, |document, count| {
                document.discharged = count
            });
        }
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(32))]
        #[test]
        fn document_count_boundaries_match_generated_inventories(
            flags in prop::collection::vec(any::<bool>(), 0..8),
            wrong_total in any::<bool>(), wrong_eligible in any::<bool>(), discharged in 0..10_u32,
        ) {
            let mut document = valid_document();
            let template = document.goals.get(1).expect("Unknown template").clone();
            document.goals = flags.iter().enumerate().map(|(index, eligible)| {
                let mut goal = template.clone();
                goal.index = u32::try_from(index).expect("bounded index");
                goal.eligible = *eligible;
                goal
            }).collect();
            document.total = u32::try_from(flags.len()).expect("bounded inventory");
            document.eligible = u32::try_from(flags.iter().filter(|flag| **flag).count()).expect("bounded inventory");
            document.discharged = 0;
            let mut wire = serde_json::to_value(&document).expect("valid generated inventory");
            let valid = !wrong_total && !wrong_eligible && discharged <= document.eligible;
            document.total = document.total.checked_add(u32::from(wrong_total)).expect("bounded count");
            document.eligible = document.eligible.checked_add(u32::from(wrong_eligible)).expect("bounded count");
            document.discharged = discharged;
            *wire.get_mut("total").expect("count") = json!(document.total);
            *wire.get_mut("eligible").expect("count") = json!(document.eligible);
            *wire.get_mut("discharged").expect("count") = json!(document.discharged);
            prop_assert_eq!(serde_json::to_value(&document).is_ok(), valid);
            prop_assert_eq!(serde_json::from_value::<super::GuaranteesDocument>(wire).is_ok(), valid);
        }

        #[test]
        fn guarantee_scalar_roundtrip_preserves_exact_coefficient(coefficient in any::<i64>(), scale in 0..=28_u32) {
            let value = GuaranteeScalar { coefficient, scale,
                unit: Some(crate::UnitRef { system: crate::UnitSystem::Ucum, code: "g".into() }) };
            let bytes = norn_canonical::serialize_json_verbatim(&value).expect("wire bytes");
            prop_assert_eq!(serde_json::from_slice::<GuaranteeScalar>(&bytes).expect("roundtrip"), value);
        }
    }
}
