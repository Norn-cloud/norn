//! Protected application challenges. Identity checking is distinct from proof checking.
//!
//! The installation authority retains an approved contract outside the candidate's
//! workspace. A worker receives only a challenge. Its returned inventory is compared
//! with a reconstruction, never used as the source of required obligations.
use std::collections::{BTreeMap, BTreeSet};

use norn_graph::Budget;
use norn_ir::{ArtifactEnvelope, ArtifactKind, DiagnosticSeverity, ResourceLimitProfile};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use crate::{EntityArtifactSetContext, check_artifact_set_with_receipt};

mod induction;

/// Generator semantics; changing the obligation rules invalidates all challenges.
/// Profile 8 admits verbatim Unicode and binds exact source/expression bytes
/// and induction statements; profile 7 is the distinct inventory-only format.
/// Profiles 5/6 required NFC at proof ingress.
pub const GENERATOR_PROFILE: &str = "norn.application-obligations/8";

/// Why a fixed challenge could not be reconstructed or a submission was rejected.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChallengeError {
    /// The candidate or independent base did not pass ordinary structural admission.
    NotAdmitted,
    /// The flat base needs verified retained-layer authority, outside this profile (#3604).
    MultiLayerBaseRequiresRetainedInput,
    /// The authority supplied an empty semantics, assumptions or assurance identity.
    MissingProfile,
    /// A protected law was removed, added or edited without a new approval.
    LawChanged,
    /// Canonical identity or complete writer closure could not be reconstructed.
    Closure,
    /// The worker omitted, duplicated, substituted or altered a challenge member.
    InvalidSubmission,
    /// JSON is unsafe or malformed, or exact numbers cannot survive the typed projection.
    LossyInput,
    /// Aggregate challenge goals or bytes exceed the portable checker budget.
    Limit,
}

impl std::fmt::Display for ChallengeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::NotAdmitted => "candidate or independent base failed structural admission",
            Self::MultiLayerBaseRequiresRetainedInput => {
                "multi-layer base requires a verified retained-layer input (#3604)"
            }
            Self::MissingProfile => "semantics, assumptions or assurance profile is missing",
            Self::LawChanged => "protected laws or reference contracts changed without approval",
            Self::Closure => "exact identity or complete writer closure could not be reconstructed",
            Self::InvalidSubmission => "submission does not match the protected challenge",
            Self::LossyInput => "source JSON is unsafe, malformed or numerically lossy",
            Self::Limit => "protected challenge resource limit exceeded",
        })
    }
}

impl std::error::Error for ChallengeError {}

/// Exact source documents retained before any floating-point payload projection.
#[derive(Debug, Clone)]
pub struct DefinitionSource {
    artifacts: Vec<ArtifactEnvelope>,
    base: Vec<ArtifactEnvelope>,
    exact_digest: String,
}

impl DefinitionSource {
    /// Parse complete candidate/base documents, preserving submitted Unicode spelling.
    /// Source identity retains exact bytes separately from the ordinary Norn1 receipt.
    /// # Errors
    /// Refuses malformed, duplicate-key, unsafe, oversized or numerically lossy documents.
    pub fn parse(artifacts: &str, base: &str) -> Result<Self, ChallengeError> {
        if artifacts.len().saturating_add(base.len()) > 8_388_608 {
            return Err(ChallengeError::Limit);
        }
        let decode = |text: &str| -> Result<Vec<ArtifactEnvelope>, ChallengeError> {
            numeric_budget(text)?;
            let exact = norn_canonical::parse_json_verbatim(text.as_bytes())
                .map_err(|_error| ChallengeError::LossyInput)?;
            let typed: Vec<ArtifactEnvelope> =
                serde_json::from_str(text).map_err(|_error| ChallengeError::NotAdmitted)?;
            let projected_bytes = norn_canonical::serialize_json_verbatim(&typed)
                .map_err(|_error| ChallengeError::LossyInput)?;
            let projected = norn_canonical::parse_json_verbatim(&projected_bytes)
                .map_err(|_error| ChallengeError::LossyInput)?;
            check_numbers(&exact, &projected)?;
            Ok(typed)
        };
        // Commit the original UTF-8 documents, including escapes, whitespace and
        // exact number spellings. Never pass executable text through Norn1 first.
        let exact_digest = exact_commitment(
            b"definition-source",
            &[artifacts.as_bytes(), base.as_bytes()],
        )?;
        let artifacts = decode(artifacts)?;
        let base = decode(base)?;
        // Qualify inputs before ordinary admission can construct diagnostics or
        // dependency closures. This is a proof-ingress resource profile, not an
        // expansion of the ordinary definition admission surface.
        let mut identity_work = 0_usize;
        for artifact in artifacts.iter().chain(&base) {
            if artifact.artifact_id.len() > 1_024 {
                return Err(ChallengeError::Limit);
            }
            identity_work = identity_work.saturating_add(
                artifact
                    .artifact_id
                    .len()
                    .saturating_mul(artifact.dependencies.len().saturating_add(1)),
            );
            if identity_work > 8_388_608 {
                return Err(ChallengeError::Limit);
            }
        }
        Ok(Self {
            artifacts,
            base,
            exact_digest,
        })
    }
}

// Protected identities are deliberately separate from ordinary ArtifactRef/Norn1.
// Full typed envelopes conservatively bind every expression's literal/identifier
// bytes, including fields ordinary receipts omit. Source ingress separately binds
// the original documents and refuses lossy projection before any approval.
pub(crate) fn exact_artifact_digest(artifact: &ArtifactEnvelope) -> Result<String, ChallengeError> {
    let bytes = norn_canonical::serialize_json_verbatim(artifact)
        .map_err(|_error| ChallengeError::Closure)?;
    // Retain the previous canonical profile's depth/number safety checks while
    // hashing the original serialization, never the parser's output.
    norn_canonical::parse_json_verbatim(&bytes).map_err(|_error| ChallengeError::Closure)?;
    exact_commitment(b"artifact-envelope", &[&bytes])
}

// SHA-256 over a fixed versioned prefix followed by u64 big-endian length-framed
// domain and members. Framing is portable across native/WASM hosts and distinguishes
// member splits, empty members, and domains without normalizing any input bytes.
fn exact_commitment(domain: &[u8], members: &[&[u8]]) -> Result<String, ChallengeError> {
    let mut hash = Sha256::new();
    hash.update(b"norn.obligation-exact/1\0");
    for bytes in std::iter::once(domain).chain(members.iter().copied()) {
        let len = u64::try_from(bytes.len()).map_err(|_error| ChallengeError::Limit)?;
        hash.update(len.to_be_bytes());
        hash.update(bytes);
    }
    Ok(format!("{:x}", hash.finalize()))
}

// A conservative allocation preflight, not a JSON parser. Quoted text is
// skipped with JSON escape handling; the ordinary parsers still validate syntax.
// f64-backed payloads cannot represent nonzero magnitudes beyond this exponent
// range; refusing even zero with such spelling is intentional in this profile.
fn numeric_budget(text: &str) -> Result<(), ChallengeError> {
    let mut quoted = false;
    let mut escaped = false;
    let mut cost = text.len();
    let mut token = String::new();
    let charge = |token: &mut String, cost: &mut usize| -> Result<(), ChallengeError> {
        if let Some((_, exponent)) = token.split_once(['e', 'E']) {
            let exponent = exponent
                .parse::<i32>()
                .map_err(|_error| ChallengeError::LossyInput)?;
            if !(-400..=400).contains(&exponent) {
                return Err(ChallengeError::LossyInput);
            }
            *cost = cost.saturating_add(400);
        }
        token.clear();
        if *cost > 8_388_608 {
            return Err(ChallengeError::Limit);
        }
        Ok(())
    };
    for byte in text.bytes() {
        if quoted {
            if escaped {
                escaped = false;
            } else if byte == b'\\' {
                escaped = true;
            } else if byte == b'"' {
                quoted = false;
            }
        } else if byte == b'"' {
            charge(&mut token, &mut cost)?;
            quoted = true;
        } else if byte.is_ascii_digit()
            || byte == b'-'
            || (!token.is_empty() && matches!(byte, b'e' | b'E' | b'+' | b'.'))
        {
            token.push(char::from(byte));
        } else {
            charge(&mut token, &mut cost)?;
        }
    }
    charge(&mut token, &mut cost)
}

fn check_numbers(
    exact: &norn_canonical::JsonValue,
    projected: &norn_canonical::JsonValue,
) -> Result<(), ChallengeError> {
    use norn_canonical::JsonValue as J;
    match exact {
        J::Number(_) if exact != projected => return Err(ChallengeError::LossyInput),
        J::Object(fields) => {
            for (key, value) in fields {
                if let J::Object(other) = projected {
                    if let Some(projected) = other.get(key) {
                        check_numbers(value, projected)?;
                    } else if contains_number(value) {
                        return Err(ChallengeError::LossyInput);
                    }
                } else if contains_number(value) {
                    return Err(ChallengeError::LossyInput);
                }
            }
        }
        J::Array(items) => {
            for (i, value) in items.iter().enumerate() {
                if let J::Array(other) = projected {
                    if let Some(projected) = other.get(i) {
                        check_numbers(value, projected)?;
                    } else if contains_number(value) {
                        return Err(ChallengeError::LossyInput);
                    }
                } else if contains_number(value) {
                    return Err(ChallengeError::LossyInput);
                }
            }
        }
        J::Null | J::Bool(_) | J::String(_) | J::Number(_) => (),
    }
    Ok(())
}
fn contains_number(value: &norn_canonical::JsonValue) -> bool {
    use norn_canonical::JsonValue as J;
    match value {
        J::Number(_) => true,
        J::Object(fields) => fields.values().any(contains_number),
        J::Array(items) => items.iter().any(contains_number),
        J::Null | J::Bool(_) | J::String(_) => false,
    }
}

/// Authority-owned, immutable approval. This value has no deserializer: callers
/// must obtain it through an explicit approval operation, never from worker output.
#[derive(Debug, Clone)]
pub struct ApprovedContract {
    laws: BTreeMap<String, String>,
    reference_contracts: BTreeMap<String, String>,
    semantics: Value,
    assumptions: Value,
    assurance: Value,
}

impl ApprovedContract {
    /// Approve the laws and exact profiles of an admitted effective set.
    ///
    /// This is an authority operation, not a proof-submission operation. The host
    /// must authorize and retain this value separately from candidate edits.
    /// Profiles are exact retained JSON values, including bounds, axioms, numeric/time/
    /// refusal semantics and checker identities, rather than producer-chosen names.
    /// # Errors
    /// Returns [`ChallengeError::MultiLayerBaseRequiresRetainedInput`] when the
    /// independent base check reports only same-layer extension errors. Other
    /// base errors and candidate errors return [`ChallengeError::NotAdmitted`].
    /// Also refuses absent/unsafe/oversized profiles or unhashable law identities.
    pub fn approve(
        source: &DefinitionSource,
        registry: &BTreeSet<norn_ir::PropertyTypeName>,
        semantics: Value,
        assumptions: Value,
        assurance: Value,
    ) -> Result<Self, ChallengeError> {
        let artifacts = &source.artifacts;
        let context = &EntityArtifactSetContext {
            registry,
            base: &source.base,
            tenant: None,
        };
        admit(artifacts, context)?;
        for profile in [&semantics, &assumptions, &assurance] {
            if profile.as_object().is_none_or(serde_json::Map::is_empty) {
                return Err(ChallengeError::MissingProfile);
            }
            let bytes = norn_canonical::serialize_json_verbatim(profile)
                .map_err(|_error| ChallengeError::LossyInput)?;
            if bytes.len() > 65_536 {
                return Err(ChallengeError::Limit);
            }
            // Check the retained bytes, including depth and exact-number safety,
            // without NFC canonicalization of text or keys before commitment.
            norn_canonical::parse_json_verbatim(&bytes)
                .map_err(|_error| ChallengeError::LossyInput)?;
        }
        Ok(Self {
            laws: law_refs(artifacts, context.base)?,
            reference_contracts: reference_contracts(artifacts, context.base)?,
            semantics,
            assumptions,
            assurance,
        })
    }

    /// Reconstruct every obligation from the complete admitted candidate and base.
    /// Whole-set invalidation is intentionally conservative. Opaque runtime entrances
    /// stay explicit obligations, so they cannot silently become certified writers.
    /// # Errors
    /// Returns [`ChallengeError::MultiLayerBaseRequiresRetainedInput`] when the
    /// independent base check reports only same-layer extension errors. Other
    /// base errors and candidate errors return [`ChallengeError::NotAdmitted`].
    /// Also refuses unauthorized law/reference changes, incomplete closure or
    /// challenge resource/identity failures.
    pub fn challenge(
        &self,
        source: &DefinitionSource,
        registry: &BTreeSet<norn_ir::PropertyTypeName>,
    ) -> Result<Challenge, ChallengeError> {
        let artifacts = &source.artifacts;
        let context = &EntityArtifactSetContext {
            registry,
            base: &source.base,
            tenant: None,
        };
        let receipt = admit(artifacts, context)?;
        if law_refs(artifacts, context.base)? != self.laws
            || reference_contracts(artifacts, context.base)? != self.reference_contracts
        {
            return Err(ChallengeError::LawChanged);
        }
        let pool: Vec<_> = context.base.iter().chain(artifacts).cloned().collect();
        let closure = pool
            .iter()
            .find(|a| a.kind() == ArtifactKind::Law)
            .map(|law| {
                crate::verification::conservative_closure(
                    law,
                    &pool,
                    registry,
                    Budget {
                        max_nodes: 16_384,
                        max_edges: 131_072,
                        max_steps: 1_048_576,
                    },
                )
                .map_err(|_error| ChallengeError::Closure)
            })
            .transpose()?;
        let writers = closure
            .as_ref()
            .map(|c| c.writers.clone())
            .unwrap_or_default();
        let boundaries = closure
            .as_ref()
            .map(|c| c.open_boundaries.clone())
            .unwrap_or_default();
        let per_law = writers
            .len()
            .saturating_add(boundaries.len())
            .saturating_add(3);
        if self.laws.len().saturating_mul(per_law) > 8_192 {
            return Err(ChallengeError::Limit);
        }
        preflight_goal_bytes(&self.laws, &writers, &boundaries)?;
        let induction = induction::build(&pool, &self.laws, &writers, &boundaries, registry)?;
        let body = json!({
            "generator": GENERATOR_PROFILE,
            "admissionReceipt": receipt,
            "exactSourceDigest": source.exact_digest,
            "closure": closure.as_ref().map(|c| &c.content_digests),
            "writers": writers.iter().map(crate::verification::LawWriter::to_json).collect::<Vec<_>>(),
            "openBoundaries": boundaries,
            "semantics": self.semantics,
            "assumptions": self.assumptions,
            "assurance": self.assurance,
            "inductionRules": induction::rules(),
            "lawStatements": induction.laws,
            "writerStatements": induction.writers,
            "obligations": induction.goals,
        });
        let bytes = norn_canonical::serialize_json_verbatim(&body)
            .map_err(|_error| ChallengeError::Closure)?;
        if bytes.len() > 8_388_608 {
            return Err(ChallengeError::Limit);
        }
        norn_canonical::parse_json_verbatim(&bytes).map_err(|_error| ChallengeError::Closure)?;
        let sha256 = exact_commitment(b"challenge", &[&bytes])?;
        Ok(Challenge {
            body,
            sha256,
            writers,
        })
    }
}

fn preflight_goal_bytes(
    laws: &BTreeMap<String, String>,
    writers: &BTreeSet<crate::verification::LawWriter>,
    boundaries: &BTreeSet<String>,
) -> Result<(), ChallengeError> {
    let mut total = 0_usize;
    for law in laws.keys() {
        let common = law.len().saturating_mul(6).saturating_add(256);
        total = total.saturating_add(common.saturating_mul(3));
        for writer in writers {
            total = total
                .saturating_add(common)
                .saturating_add(writer.encoded_byte_bound());
            if total > 8_388_608 {
                return Err(ChallengeError::Limit);
            }
        }
        for boundary in boundaries {
            total = total
                .saturating_add(common)
                .saturating_add(boundary.len().saturating_mul(6));
        }
        if total > 8_388_608 {
            return Err(ChallengeError::Limit);
        }
    }
    Ok(())
}

fn admit(
    artifacts: &[ArtifactEnvelope],
    context: &EntityArtifactSetContext<'_>,
) -> Result<String, ChallengeError> {
    if !context.base.is_empty() {
        // This profile accepts only independently admissible single-layer bases.
        // Candidate definitions cannot repair them. A flat installed set cannot
        // supply retained-layer authority; do not infer its previous layers.
        let full_context = EntityArtifactSetContext {
            base: &[],
            ..*context
        };
        let checked = check_artifact_set_with_receipt(
            context.base,
            &full_context,
            &ResourceLimitProfile::norn_limits_v1(),
        )
        .map_err(|_error| ChallengeError::NotAdmitted)?;
        let mut errors = checked
            .diagnostics
            .iter()
            .filter(|d| d.severity == DiagnosticSeverity::Error);
        if let Some(first) = errors.next() {
            let layer_authority_only = first.code.as_str()
                == crate::object_types::codes::EXTENSION_OF_LOCAL_TYPE
                && errors.all(|d| {
                    d.code.as_str() == crate::object_types::codes::EXTENSION_OF_LOCAL_TYPE
                });
            return Err(if layer_authority_only {
                ChallengeError::MultiLayerBaseRequiresRetainedInput
            } else {
                ChallengeError::NotAdmitted
            });
        }
        if !checked.receipt.admitted {
            return Err(ChallengeError::NotAdmitted);
        }
    }
    let checked = check_artifact_set_with_receipt(
        artifacts,
        context,
        &ResourceLimitProfile::norn_limits_v1(),
    )
    .map_err(|_error| ChallengeError::NotAdmitted)?;
    if !checked.receipt.admitted
        || checked
            .diagnostics
            .iter()
            .any(|d| d.severity == DiagnosticSeverity::Error)
    {
        return Err(ChallengeError::NotAdmitted);
    }
    checked
        .receipt
        .content_sha256()
        .map_err(|_error| ChallengeError::Closure)
}

// Conservative approved reference model: entity contracts carry property domains,
// inherited invariants, defaults and null/unit semantics. Until a qualified semantic
// dependency analysis exists, edits to any of them require separate law approval.
fn reference_contracts(
    artifacts: &[ArtifactEnvelope],
    base: &[ArtifactEnvelope],
) -> Result<BTreeMap<String, String>, ChallengeError> {
    base.iter()
        .chain(artifacts)
        .filter(|a| a.kind() == ArtifactKind::Entity)
        .map(|a| {
            exact_artifact_digest(a)
                .map(|r| (a.artifact_id.clone(), r))
                .map_err(|_error| ChallengeError::Closure)
        })
        .collect()
}

fn law_refs(
    artifacts: &[ArtifactEnvelope],
    base: &[ArtifactEnvelope],
) -> Result<BTreeMap<String, String>, ChallengeError> {
    base.iter()
        .chain(artifacts)
        .filter(|a| a.kind() == ArtifactKind::Law)
        .map(|law| {
            exact_artifact_digest(law)
                .map(|identity| (law.artifact_id.clone(), identity))
                .map_err(|_error| ChallengeError::Closure)
        })
        .collect()
}

/// Immutable fixed challenge, suitable for sending to an untrusted worker.
#[derive(Debug, Clone, PartialEq)]
pub struct Challenge {
    body: Value,
    sha256: String,
    writers: BTreeSet<crate::verification::LawWriter>,
}

impl Challenge {
    /// Exact generator, law, program, assumptions, profile and required goals.
    #[must_use]
    pub const fn body(&self) -> &Value {
        &self.body
    }

    /// Exact-byte challenge identity; not itself a positive assurance verdict.
    #[must_use]
    pub fn sha256(&self) -> &str {
        &self.sha256
    }

    /// Validate a worker's exact echoed challenge before inspecting any proof.
    /// This accepts no proof and grants no activation authority.
    /// # Errors
    /// Rejects missing, additional, changed, reordered or stale members.
    pub fn bind_submission(&self, echoed_challenge: &Value) -> Result<(), ChallengeError> {
        if echoed_challenge != &self.body {
            return Err(ChallengeError::InvalidSubmission);
        }
        Ok(())
    }

    /// Required writer identities, reconstructed by the checker.
    #[must_use]
    pub const fn writers(&self) -> &BTreeSet<crate::verification::LawWriter> {
        &self.writers
    }
}

#[cfg(test)]
#[allow(
    clippy::expect_used,
    clippy::indexing_slicing,
    reason = "private protected identity regression tests"
)]
mod exact_identity_tests {
    use super::*;

    fn documents() -> Vec<Value> {
        let mut values: Vec<Value> =
            serde_json::from_str(include_str!("../../../fixtures/law-ir/entities.json"))
                .expect("entities");
        values.extend(
            serde_json::from_str::<Vec<Value>>(include_str!("../../../fixtures/law-ir/laws.json"))
                .expect("laws"),
        );
        values
    }

    fn typed(values: Vec<Value>) -> Vec<ArtifactEnvelope> {
        values
            .into_iter()
            .map(|v| serde_json::from_value(v).expect("artifact"))
            .collect()
    }

    #[test]
    fn actual_law_reference_and_closure_commitments_preserve_expression_bytes() {
        let composed = "len('é') == 2";
        let decomposed = "len('e\u{301}') == 2";
        assert_eq!(
            norn_canonical::canonical_json_of(&json!(composed)).expect("Norn1"),
            norn_canonical::canonical_json_of(&json!(decomposed)).expect("Norn1")
        );
        for (expression, expected) in [(composed, true), (decomposed, false)] {
            assert_eq!(
                norn_expr::Expression::compile(expression)
                    .expect("compile")
                    .evaluate(
                        &norn_expr::Value::Null,
                        norn_expr::EvaluationBudget::new(1_000)
                    )
                    .expect("evaluate")
                    .value(),
                &norn_expr::Value::Bool(expected)
            );
        }
        let make = |expression: &str| {
            let mut values = documents();
            values[3]["payload"]["response"]["expression"] = json!(expression);
            values[1]["payload"]["objectType"]["invariants"][0]["expression"] = json!(expression);
            typed(values)
        };
        let left = make(composed);
        let right = make(decomposed);
        // Call the exact selectors used by approve/challenge, not just a hash helper.
        assert_eq!(
            crate::ArtifactRef::of(&left[3]).expect("ordinary"),
            crate::ArtifactRef::of(&right[3]).expect("ordinary")
        );
        assert_eq!(
            crate::ArtifactRef::of(&left[1]).expect("ordinary"),
            crate::ArtifactRef::of(&right[1]).expect("ordinary")
        );
        assert_ne!(
            law_refs(&left, &[]).expect("laws"),
            law_refs(&right, &[]).expect("laws")
        );
        assert_ne!(
            reference_contracts(&left, &[]).expect("contracts"),
            reference_contracts(&right, &[]).expect("contracts")
        );
        let closure = |pool: &[ArtifactEnvelope]| {
            crate::verification::conservative_closure(
                &pool[3],
                pool,
                &BTreeSet::new(),
                Budget {
                    max_nodes: 16_384,
                    max_edges: 131_072,
                    max_steps: 1_048_576,
                },
            )
            .expect("closure")
            .content_digests
        };
        let left_closure = closure(&left);
        let right_closure = closure(&right);
        assert_ne!(
            left_closure[&left[3].artifact_id],
            right_closure[&right[3].artifact_id]
        );
        assert_ne!(
            left_closure[&left[1].artifact_id],
            right_closure[&right[1].artifact_id]
        );
        // Exercise ordinary admission and the public approval/challenge path as
        // well as the selectors: spelling differences are admitted and bound.
        let challenge = |artifacts: &[ArtifactEnvelope]| {
            let source =
                DefinitionSource::parse(&serde_json::to_string(artifacts).expect("source"), "[]")
                    .expect("verbatim source");
            ApprovedContract::approve(
                &source,
                &BTreeSet::new(),
                json!({"zen":"6"}),
                json!({"axioms":[]}),
                json!({"proof":"checked"}),
            )
            .expect("ordinarily admitted")
            .challenge(&source, &BTreeSet::new())
            .expect("challenge")
        };
        assert_ne!(challenge(&left).sha256(), challenge(&right).sha256());
    }

    #[test]
    fn protected_envelope_commitments_preserve_identifier_bytes() {
        let mut left = typed(documents()).remove(3);
        left.artifact_id = "é.law".into();
        let mut right = left.clone();
        right.artifact_id = "e\u{301}.law".into();
        assert_eq!(
            crate::ArtifactRef::of(&left).expect("ordinary").sha256,
            crate::ArtifactRef::of(&right).expect("ordinary").sha256
        );
        assert_ne!(
            exact_artifact_digest(&left).expect("exact"),
            exact_artifact_digest(&right).expect("exact")
        );
    }

    #[test]
    fn protected_commitment_frames_members_and_domains() {
        let digest =
            |domain: &[u8], parts: &[&[u8]]| exact_commitment(domain, parts).expect("digest");
        assert_ne!(
            digest(b"definition-source", &[b"ab", b"c"]),
            digest(b"definition-source", &[b"a", b"bc"])
        );
        assert_ne!(
            digest(b"challenge", &[b"x"]),
            digest(b"artifact-envelope", &[b"x"])
        );
        assert_ne!(digest(b"a", &[b"bc"]), digest(b"ab", &[b"c"]));
        assert_ne!(digest(b"challenge", &[]), digest(b"challenge", &[b""]));
        assert_ne!(
            digest(b"definition-source", &[b"[]", b"[ ]"]),
            digest(b"definition-source", &[b"[ ]", b"[]"])
        );
    }
}
