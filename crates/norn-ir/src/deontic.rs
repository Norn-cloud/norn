//! Structured normative declarations. Data only; these are not authorization grants.
use crate::{IrVersion01, LawIR, ZenExpression};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Reviewed structured language; predicates retain the existing ZEN semantics.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DeonticIR {
    pub deontic_version: IrVersion01,
    pub authorities: Vec<AuthoritySource>,
    pub rules: Vec<DeonticRule>,
}

/// A declared signed source, not a claim that its signature has been verified.
/// Verification and tenant selection belong to the authority admission boundary.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AuthoritySource {
    pub id: String,
    pub version: String,
    pub document_sha256: String,
    pub signer: String,
    pub signature: String,
    pub source: RuleSourceSpan,
}

/// Source JSON pointer plus an optional half-open UTF-8 byte range.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RuleSourceSpan {
    pub artifact_id: String,
    pub pointer: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bytes: Option<RuleByteRange>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RuleByteRange {
    pub start: u32,
    pub end: u32,
}

/// The normative grade is separate from the temporal wrapper in `law`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum DeonticModality {
    Obligation,
    Permission,
    Prohibition,
}

/// A law template with explicit authority, exceptions and preferences.
/// `law.condition`, `law.timing` and `law.response` supply typed slots;
/// `law.modality` must agree with the normative grade before law lowering.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DeonticRule {
    pub law: LawIR,
    pub modality: DeonticModality,
    pub authority_id: String,
    pub authority_version: String,
    pub source: RuleSourceSpan,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub exceptions: Vec<RuleException>,
    /// Explicit rule identities preferred to this rule; not numeric authority ranking.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub defeated_by: Vec<String>,
}

/// A reviewed conditional exception. No implicit necessity or legal judgement.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RuleException {
    pub id: String,
    pub condition: ZenExpression,
    pub source: RuleSourceSpan,
}

/// Structured authoring over the existing law and normative source model.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct StructuredRequirement {
    pub norm: DeonticRule,
    pub timing: RequirementTiming,
    pub wording: RequirementWording,
}

/// Closed safety fragment. No event activation, clocks, fairness or progress.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum RequirementTiming {
    /// Condition and response are evaluated at every applicable committed state.
    EveryCommittedState,
    /// Condition and response refer to the same proposed commit's state.
    /// This is not an edge trigger or a promise to respond in a later commit.
    SameCommit,
}

/// LAW-WORDING-LOCALE-1: complete reviewed display data, ordered by BCP 47 tag.
/// Wording is never executable and does not change the law's semantic identity.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(transparent)]
pub struct RequirementWording(pub std::collections::BTreeMap<String, LocalizedRequirement>);

/// One complete translation, including every phrase used by the renderer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LocalizedRequirement {
    pub direction: TextDirection,
    pub scope: String,
    pub activation: String,
    pub component: String,
    pub response: String,
    pub labels: RequirementLabels,
    pub terms: RequirementTerms,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum TextDirection {
    Ltr,
    Rtl,
}

impl TextDirection {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Ltr => "ltr",
            Self::Rtl => "rtl",
        }
    }
}

impl RequirementWording {
    /// LAW-WORDING-LOCALE-1 also applies to programmatically constructed values.
    ///
    /// # Errors
    /// Refuses malformed/duplicate tags and incomplete or blank translations.
    pub fn validate(&self) -> Result<(), &'static str> {
        use std::collections::BTreeSet;
        let mut tags = BTreeSet::new();
        if self.0.is_empty() {
            return Err("law.wording.incomplete_locale");
        }
        for (tag, entry) in &self.0 {
            if language_tags::LanguageTag::parse(tag).is_err() {
                return Err("law.wording.invalid_locale");
            }
            if !tags.insert(tag.to_ascii_lowercase()) {
                return Err("law.wording.duplicate_locale");
            }
            entry.validate()?;
        }
        Ok(())
    }

    /// Select only the requested BCP 47 tag (case insensitive); never fall back.
    ///
    /// # Errors
    /// Refuses invalid wording or an unavailable requested locale.
    pub fn locale(&self, tag: &str) -> Result<&LocalizedRequirement, &'static str> {
        self.validate()?;
        if language_tags::LanguageTag::parse(tag).is_err() {
            return Err("law.wording.invalid_locale");
        }
        self.0
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(tag))
            .map(|(_, entry)| entry)
            .ok_or("law.wording.unknown_locale")
    }
}

impl LocalizedRequirement {
    fn validate(&self) -> Result<(), &'static str> {
        if [
            &self.scope,
            &self.activation,
            &self.component,
            &self.response,
            &self.labels.scope,
            &self.labels.condition,
            &self.labels.component,
            &self.labels.grade,
            &self.labels.timing,
            &self.labels.response,
            &self.labels.source,
            &self.labels.preferences,
            &self.labels.exception,
            &self.terms.company,
            &self.terms.application,
            &self.terms.cell,
            &self.terms.entity,
            &self.terms.commitments,
            &self.terms.obligation,
            &self.terms.permission,
            &self.terms.prohibition,
            &self.terms.same_commit,
            &self.terms.every_committed_state,
            &self.terms.grounding,
        ]
        .iter()
        .any(|s| s.trim().is_empty())
        {
            return Err("law.wording.incomplete_locale");
        }
        Ok(())
    }
}

impl<'de> Deserialize<'de> for RequirementWording {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct WordingVisitor;
        impl<'de> serde::de::Visitor<'de> for WordingVisitor {
            type Value = RequirementWording;
            fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str("an ordered map of complete BCP 47 translations")
            }
            fn visit_map<M: serde::de::MapAccess<'de>>(
                self,
                mut map: M,
            ) -> Result<Self::Value, M::Error> {
                use serde::de::Error as _;
                let mut entries = std::collections::BTreeMap::new();
                let mut tags = std::collections::BTreeSet::new();
                while let Some(tag) = map.next_key::<String>()? {
                    if language_tags::LanguageTag::parse(&tag).is_err() {
                        return Err(M::Error::custom("law.wording.invalid_locale"));
                    }
                    if !tags.insert(tag.to_ascii_lowercase()) {
                        return Err(M::Error::custom("law.wording.duplicate_locale"));
                    }
                    let entry = map.next_value::<LocalizedRequirement>().map_err(|e| {
                        M::Error::custom(format!("law.wording.incomplete_locale: {e}"))
                    })?;
                    entries.insert(tag, entry);
                }
                let wording = RequirementWording(entries);
                wording.validate().map_err(M::Error::custom)?;
                Ok(wording)
            }
        }
        deserializer.deserialize_map(WordingVisitor)
    }
}

/// Complete display vocabulary; missing fields refuse at decode.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RequirementLabels {
    pub scope: String,
    pub condition: String,
    pub component: String,
    pub grade: String,
    pub timing: String,
    pub response: String,
    pub source: String,
    pub preferences: String,
    pub exception: String,
}

/// Complete display vocabulary; missing fields refuse at decode.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RequirementTerms {
    pub company: String,
    pub application: String,
    pub cell: String,
    pub entity: String,
    pub commitments: String,
    pub obligation: String,
    pub permission: String,
    pub prohibition: String,
    pub same_commit: String,
    pub every_committed_state: String,
    pub grounding: String,
}

/// Public structured-requirement schema, including complete locale vocabulary.
#[must_use]
pub fn structured_requirement_schema() -> schemars::Schema {
    schemars::schema_for!(StructuredRequirement)
}
