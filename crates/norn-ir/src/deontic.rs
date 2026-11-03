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
pub struct RequirementWording(
    #[schemars(extend("minProperties" = 1))]
    pub  std::collections::BTreeMap<String, LocalizedRequirement>,
);

/// One complete translation, including every phrase used by the renderer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LocalizedRequirement {
    pub direction: TextDirection,
    #[schemars(pattern(r"\S"))]
    pub scope: String,
    #[schemars(pattern(r"\S"))]
    pub activation: String,
    #[schemars(pattern(r"\S"))]
    pub component: String,
    #[schemars(pattern(r"\S"))]
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
        crate::reviewed_locale::validate(&self.0, LocalizedRequirement::validate)
    }

    /// Select only the requested BCP 47 tag (case insensitive); never fall back.
    ///
    /// # Errors
    /// Refuses invalid wording or an unavailable requested locale.
    pub fn locale(&self, tag: &str) -> Result<&LocalizedRequirement, &'static str> {
        crate::reviewed_locale::select(&self.0, tag, LocalizedRequirement::validate)
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
        crate::reviewed_locale::deserialize(deserializer, LocalizedRequirement::validate).map(Self)
    }
}

/// Complete display vocabulary; missing fields refuse at decode.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RequirementLabels {
    #[schemars(pattern(r"\S"))]
    pub scope: String,
    #[schemars(pattern(r"\S"))]
    pub condition: String,
    #[schemars(pattern(r"\S"))]
    pub component: String,
    #[schemars(pattern(r"\S"))]
    pub grade: String,
    #[schemars(pattern(r"\S"))]
    pub timing: String,
    #[schemars(pattern(r"\S"))]
    pub response: String,
    #[schemars(pattern(r"\S"))]
    pub source: String,
    #[schemars(pattern(r"\S"))]
    pub preferences: String,
    #[schemars(pattern(r"\S"))]
    pub exception: String,
}

/// Complete display vocabulary; missing fields refuse at decode.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RequirementTerms {
    #[schemars(pattern(r"\S"))]
    pub company: String,
    #[schemars(pattern(r"\S"))]
    pub application: String,
    #[schemars(pattern(r"\S"))]
    pub cell: String,
    #[schemars(pattern(r"\S"))]
    pub entity: String,
    #[schemars(pattern(r"\S"))]
    pub commitments: String,
    #[schemars(pattern(r"\S"))]
    pub obligation: String,
    #[schemars(pattern(r"\S"))]
    pub permission: String,
    #[schemars(pattern(r"\S"))]
    pub prohibition: String,
    #[schemars(pattern(r"\S"))]
    pub same_commit: String,
    #[schemars(pattern(r"\S"))]
    pub every_committed_state: String,
    #[schemars(pattern(r"\S"))]
    pub grounding: String,
}

/// Public structured-requirement schema, including complete locale vocabulary.
#[must_use]
pub fn structured_requirement_schema() -> schemars::Schema {
    schemars::schema_for!(StructuredRequirement)
}

// RFC 5646 §§2.1, 2.2.9: syntactic well-formedness, not registry membership.
pub(crate) fn valid_locale_tag(tag: &str) -> bool {
    const IRREGULAR: &[&str] = &[
        "en-gb-oed",
        "i-ami",
        "i-bnn",
        "i-default",
        "i-enochian",
        "i-hak",
        "i-klingon",
        "i-lux",
        "i-mingo",
        "i-navajo",
        "i-pwn",
        "i-tao",
        "i-tay",
        "i-tsu",
        "sgn-be-fr",
        "sgn-be-nl",
        "sgn-ch-de",
    ];
    let lower = tag.to_ascii_lowercase();
    if IRREGULAR.contains(&lower.as_str()) {
        return true;
    }
    let mut parts = lower.split('-').peekable();
    let Some(language) = parts.next() else {
        return false;
    };
    let alnum =
        |s: &str| !s.is_empty() && s.len() <= 8 && s.bytes().all(|b| b.is_ascii_alphanumeric());
    let alpha = |s: &str| s.bytes().all(|b| b.is_ascii_alphabetic());
    if language == "x" {
        return parts.peek().is_some() && parts.all(alnum);
    }
    if !(2..=8).contains(&language.len()) || !alpha(language) {
        return false;
    }
    if language.len() <= 3 {
        for _ in 0..3 {
            if parts.peek().is_some_and(|s| s.len() == 3 && alpha(s)) {
                parts.next();
            } else {
                break;
            }
        }
    }
    if parts.peek().is_some_and(|s| s.len() == 4 && alpha(s)) {
        parts.next();
    }
    if parts.peek().is_some_and(|s| {
        (s.len() == 2 && alpha(s)) || (s.len() == 3 && s.bytes().all(|b| b.is_ascii_digit()))
    }) {
        parts.next();
    }
    let mut variants = std::collections::BTreeSet::new();
    while parts.peek().is_some_and(|s| {
        alnum(s) && (s.len() >= 5 || (s.len() == 4 && s.starts_with(|c: char| c.is_ascii_digit())))
    }) {
        if !variants.insert(parts.next()) {
            return false;
        }
    }
    let mut extensions = std::collections::BTreeSet::new();
    while let Some(singleton) = parts.next() {
        if singleton == "x" {
            return parts.peek().is_some() && parts.all(alnum);
        }
        if singleton.len() != 1 || !alnum(singleton) || !extensions.insert(singleton) {
            return false;
        }
        let mut has_subtag = false;
        while parts.peek().is_some_and(|s| s.len() >= 2 && alnum(s)) {
            parts.next();
            has_subtag = true;
        }
        if !has_subtag {
            return false;
        }
    }
    true
}

#[cfg(test)]
mod locale_properties {
    use super::valid_locale_tag;
    use proptest::prelude::*;

    proptest! {
        #[test]
        fn accepted_locale_tags_have_bcp47_syntax(tag in "[a-zA-Z0-9_-]{0,60}") {
            if valid_locale_tag(&tag) {
                prop_assert!(language_tags::LanguageTag::parse(&tag).is_ok());
            }
        }

        #[test]
        fn generated_language_tags_are_case_insensitive(language in "[a-z]{2,3}", extlang in "[a-z]{3}", script in "[A-Z][a-z]{3}", region in "[A-Z]{2}", variant in "[a-z]{5,8}", extension in "[a-z0-9]{2,8}", private in "[a-z0-9]{1,8}") {
            let base = format!("{language}-{extlang}-{script}-{region}-{variant}");
            let tag = format!("{base}-a-{extension}-x-{private}");
            prop_assert!(valid_locale_tag(&tag));
            prop_assert!(valid_locale_tag(&tag.to_ascii_uppercase()));
            prop_assert!(language_tags::LanguageTag::parse(&tag).is_ok());
            // The oracle's parse checks syntax; Norn additionally rejects duplicates.
            for duplicate in [format!("{base}-{variant}"), format!("{base}-a-{extension}-a-{extension}")] {
                prop_assert!(language_tags::LanguageTag::parse(&duplicate).is_ok());
                prop_assert!(!valid_locale_tag(&duplicate));
            }
        }
    }
}
