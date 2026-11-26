//! Conjunctive and projected-safety specialisation provenance; no cache, solver
//! or proof promotion.
//!
//! Reference rules:
//! - [`RULE`](crate::obligations::specialisation::RULE) (`specialisation-conjunction/1`):
//!   law-only conjunctive extensions over a single-layer base with an identical
//!   whole-set writer inventory.
//! - [`PROJECTED_RULE`](crate::obligations::specialisation::PROJECTED_RULE)
//!   (`specialisation-projected/1`): reviewed adapter code relying on
//!   `Specialisation.lean::projected_safety_stuttering` (where disjoint
//!   extension writers induce stuttering no-change steps on the projected base
//!   state). Supports multi-layer bases and domain extensions adding new
//!   entities, interfaces, actions, decide rules, policies, mappings, labels,
//!   and laws. Reuses prior current safety evidence for a base `Always`/`Never`
//!   law `L` only when the extension neither modifies nor widens `L`'s governed
//!   subject/read footprint nor introduces any writer (closed or opaque) that
//!   may touch it. This adapter establishes reviewed identity provenance, not a
//!   machine-checked correspondence of the Rust footprint analysis.
use std::collections::{BTreeMap, BTreeSet};

use norn_ir::{ArtifactEnvelope, ArtifactKind, EntityRoot, LawIR, LawModality, PropertyTypeName};
use serde_json::Value;

use self::footprint::{
    CatalogIndex, WriterFootprint, charge_work, effective_contract_unchanged,
    expand_type_hierarchy, law_footprint, writer_footprint,
};
use super::evidence::{EvidenceIdentity, EvidencePins, EvidenceRecord};
use super::{
    ApprovedContract, Challenge, ChallengeError, DefinitionSource, exact_artifact_digest,
    exact_commitment,
};
use crate::entity_artifacts::{Decoded, decode_v2};

mod footprint;

/// Versioned, conservative conjunction-only reuse rule.
pub const RULE: &str = "specialisation-conjunction/1";

/// Versioned projected-safety reuse rule relying on
/// `Specialisation.lean::projected_safety_stuttering` (reviewed adapter glue).
pub const PROJECTED_RULE: &str = "specialisation-projected/1";

/// Independently retained runner identity and its corresponding evidence record.
/// The caller supplies trusted pins/digest, never values taken from the candidate.
pub struct PriorEvidence<'a> {
    /// Controller-owned run pins.
    pub pins: &'a EvidencePins,
    /// Controller-owned checked result digest.
    pub result_sha256: &'a str,
    /// Untrusted record, checked against the other fields before reuse.
    pub record: &'a EvidenceRecord,
}

/// Origin of a carried record. Its assurance stays exactly that of the base run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReusedEvidence {
    /// Specialisation reuse rule that justified carrying this record
    /// ([`RULE`](crate::obligations::specialisation::RULE) or
    /// [`PROJECTED_RULE`](crate::obligations::specialisation::PROJECTED_RULE)).
    pub rule: &'static str,
    /// Target goal index in the freshly admitted specialisation.
    pub goal_index: usize,
    /// Complete prior evidence record commitment, not merely its result digest.
    pub base_evidence_sha256: String,
    /// Exact candidate/base source commitment.
    pub extension_sha256: String,
    /// Commitment to rule, source evidence, extension and target challenge/goal.
    pub sha256: String,
}

/// A live-admitted challenge and a partition of its required proof work.
/// This is identity provenance only: customer checks still run in admission.
#[derive(Debug, Clone, PartialEq)]
pub struct SpecialisationChallenge {
    /// Complete generated target; no obligation is deleted from the inventory.
    pub challenge: Challenge,
    /// Valid carried records under
    /// [`RULE`](crate::obligations::specialisation::RULE) or
    /// [`PROJECTED_RULE`](crate::obligations::specialisation::PROJECTED_RULE),
    /// with no increase in assurance.
    pub reused: Vec<ReusedEvidence>,
    /// Every goal not covered by a carried record must be checked afresh.
    pub recheck: Vec<usize>,
}

impl ApprovedContract {
    /// Admit and generate the complete target, then apply
    /// [`RULE`](crate::obligations::specialisation::RULE) (for law-only
    /// conjunctive extensions over a single-layer base with unchanged writers)
    /// or [`PROJECTED_RULE`](crate::obligations::specialisation::PROJECTED_RULE)
    /// (for multi-layer bases and domain extensions whose new writers are
    /// provably disjoint from a base safety law's governed footprint).
    ///
    /// Any base goal whose subject gains a new writer, a new concrete
    /// subtype/implementer, a widening extension, or an opaque writer stays in
    /// `recheck` (fail-closed). No caller-provided closure or eligibility
    /// assertion is trusted.
    ///
    /// # Errors
    ///
    /// Same structural, protected identity and resource errors as
    /// [`Self::challenge`].
    pub fn challenge_under_specialisation(
        &self,
        source: &DefinitionSource,
        registry: &BTreeSet<PropertyTypeName>,
        base_contract: &Self,
        base_source: &DefinitionSource,
        prior: &[PriorEvidence<'_>],
    ) -> Result<SpecialisationChallenge, ChallengeError> {
        let challenge = self.challenge(source, registry)?;
        let goal_count = goals(&challenge).len();
        let mut result = SpecialisationChallenge {
            recheck: (0..goal_count).collect(),
            reused: Vec::new(),
            challenge,
        };
        // Bound input work and never use a failed reconstruction to skip work.
        if prior.iter().fold(0_usize, |total, evidence| {
            total
                .saturating_add(evidence.record.challenge_json.len())
                .saturating_add(evidence.record.result_json.len())
                .saturating_add(evidence.pins.bounds_json.len())
        }) > 8_388_608
            || prior.len() > 8_192
            || !base_matches_flattened(source, base_source)
            || self.semantics != base_contract.semantics
            || self.assumptions != base_contract.assumptions
            || self.assurance != base_contract.assurance
        {
            return Ok(result);
        }
        let Ok(base) = base_contract.challenge(base_source, registry) else {
            return Ok(result);
        };
        if base.body()["openBoundaries"] != result.challenge.body()["openBoundaries"] {
            return Ok(result);
        }

        let base_pool: Vec<ArtifactEnvelope> = base_source
            .base
            .iter()
            .chain(&base_source.artifacts)
            .cloned()
            .collect();

        let conjunction_only = base_source.base.is_empty()
            && source
                .artifacts
                .iter()
                .all(|a| a.kind() == ArtifactKind::Law)
            && base.body()["writerStatements"] == result.challenge.body()["writerStatements"];

        let mut projected = if conjunction_only {
            None
        } else {
            let Some(ctx) =
                ProjectedContext::build(&base_pool, source, &base, &result.challenge, registry)
            else {
                return Ok(result);
            };
            Some(ctx)
        };

        let rule = if conjunction_only {
            RULE
        } else {
            PROJECTED_RULE
        };

        for evidence in prior {
            if base.evidence_identity(evidence.pins, evidence.result_sha256, evidence.record)
                != EvidenceIdentity::Current
            {
                continue;
            }
            let Some(index) = matching_goal(
                &base_pool,
                &base,
                &result.challenge,
                evidence.pins.goal_index,
                projected.as_mut(),
            ) else {
                continue;
            };
            if result.reused.iter().any(|r| r.goal_index == index) {
                continue;
            }
            result.reused.push(build_reused_evidence(
                rule,
                index,
                evidence.record,
                &source.exact_digest,
                result.challenge.sha256(),
            )?);
        }
        result
            .recheck
            .retain(|index| !result.reused.iter().any(|r| r.goal_index == *index));
        Ok(result)
    }
}

fn build_reused_evidence(
    rule: &'static str,
    goal_index: usize,
    record: &EvidenceRecord,
    extension_digest: &str,
    target_challenge_sha256: &str,
) -> Result<ReusedEvidence, ChallengeError> {
    let bytes = norn_canonical::serialize_json_verbatim(record)
        .map_err(|_error| ChallengeError::Closure)?;
    let base_evidence_sha256 = exact_commitment(b"specialisation-evidence", &[&bytes])?;
    let extension_sha256 = extension_digest.to_owned();
    let index_bytes = goal_index.to_string();
    let sha256 = exact_commitment(
        b"specialisation-reuse",
        &[
            rule.as_bytes(),
            base_evidence_sha256.as_bytes(),
            extension_sha256.as_bytes(),
            target_challenge_sha256.as_bytes(),
            index_bytes.as_bytes(),
        ],
    )?;
    Ok(ReusedEvidence {
        rule,
        goal_index,
        base_evidence_sha256,
        extension_sha256,
        sha256,
    })
}

/// Exact flattened base identity: `source.base_sources` must equal
/// `base_source.base_sources` followed by `base_source.receipt_sources`.
fn base_matches_flattened(source: &DefinitionSource, base_source: &DefinitionSource) -> bool {
    let expected_len = base_source
        .base_sources
        .len()
        .saturating_add(base_source.receipt_sources.len());
    source.base_sources.len() == expected_len
        && source.base_sources.iter().eq(base_source
            .base_sources
            .iter()
            .chain(&base_source.receipt_sources))
}

/// Every base closure content digest must be preserved verbatim in `target`.
fn base_closure_digests_unchanged(base: &Challenge, target: &Challenge) -> bool {
    let Some(base_map) = base.body()["closure"].as_object() else {
        return false;
    };
    let Some(target_map) = target.body()["closure"].as_object() else {
        return false;
    };
    base_map
        .iter()
        .all(|(artifact_id, digest)| target_map.get(artifact_id) == Some(digest))
}

struct ProjectedContext {
    base_pool: Vec<ArtifactEnvelope>,
    target_pool: Vec<ArtifactEnvelope>,
    base_index: CatalogIndex,
    target_index: CatalogIndex,
    extended_targets: BTreeSet<String>,
    new_writers: Vec<WriterFootprint>,
    base_writers_unchanged: bool,
    preserved_cache: BTreeMap<(String, String), bool>,
    work: usize,
}

fn collect_raw_extended_targets(artifacts: &[ArtifactEnvelope]) -> Option<BTreeSet<String>> {
    let mut raw_extended_targets = BTreeSet::new();
    for artifact in artifacts {
        match artifact.kind() {
            ArtifactKind::Entity => match decode_v2(artifact) {
                Decoded::Root(root) => match *root {
                    EntityRoot::V2Extension(ext) => {
                        raw_extended_targets
                            .insert(ext.object_type_extension.target.as_str().to_owned());
                        // Narrower-scoped extensions can add links targeting a
                        // base type without appearing in the declaring-scope
                        // catalog of the extended type; track their targets too.
                        for link in &ext.object_type_extension.links {
                            raw_extended_targets.insert(link.target.as_str().to_owned());
                        }
                    }
                    EntityRoot::V2(_)
                    | EntityRoot::V2Interface(_)
                    | EntityRoot::V2PropertyTypes(_)
                    | EntityRoot::V2Markings(_) => {}
                    EntityRoot::V1(_) => return None,
                },
                Decoded::NotV2 | Decoded::Invalid => return None,
            },
            ArtifactKind::Action
            | ArtifactKind::Rule
            | ArtifactKind::Policy
            | ArtifactKind::Mapping
            | ArtifactKind::Law
            | ArtifactKind::Labels => {}
            // Artifact kinds without a qualified projected-safety footprint
            // fail closed until each is analyzed independently.
            ArtifactKind::Connector
            | ArtifactKind::Relation
            | ArtifactKind::Customization
            | ArtifactKind::Setting
            | ArtifactKind::BusinessPack
            | ArtifactKind::Derived
            | ArtifactKind::SourceSchema
            | ArtifactKind::DefinedConcept
            | ArtifactKind::Model
            | ArtifactKind::Widget => return None,
        }
    }
    Some(raw_extended_targets)
}

impl ProjectedContext {
    fn build(
        base_pool: &[ArtifactEnvelope],
        source: &DefinitionSource,
        base: &Challenge,
        target: &Challenge,
        registry: &BTreeSet<PropertyTypeName>,
    ) -> Option<Self> {
        if !base_closure_digests_unchanged(base, target) {
            return None;
        }
        let raw_extended_targets = collect_raw_extended_targets(&source.artifacts)?;

        let target_pool: Vec<ArtifactEnvelope> = source
            .base
            .iter()
            .chain(&source.artifacts)
            .cloned()
            .collect();
        let mut work = 0_usize;
        let base_index = CatalogIndex::build(base_pool, registry, &mut work)?;
        let target_index = CatalogIndex::build(&target_pool, registry, &mut work)?;

        let mut extended_targets = BTreeSet::new();
        for target_name in &raw_extended_targets {
            if !expand_type_hierarchy(target_name, &target_index, &mut extended_targets, &mut work)
            {
                return None;
            }
        }

        let mut new_writers = Vec::new();
        for writer in target.writers() {
            if !base.writers().contains(writer) {
                new_writers.push(writer_footprint(
                    writer,
                    &target_pool,
                    &target_index,
                    &mut work,
                ));
            }
        }

        let base_writer_statements = writer_statement_map(base)?;
        let target_writer_statements = writer_statement_map(target)?;
        let base_writers_unchanged = base.writers().iter().all(|base_writer| {
            let key = writer_identity_key(&base_writer.to_json());
            base_writer_statements
                .get(&key)
                .is_some_and(|base_stmt| target_writer_statements.get(&key) == Some(base_stmt))
        });

        Some(Self {
            base_pool: base_pool.to_vec(),
            target_pool,
            base_index,
            target_index,
            extended_targets,
            new_writers,
            base_writers_unchanged,
            preserved_cache: BTreeMap::new(),
            work,
        })
    }

    fn law_is_preserved(&mut self, cache_key: (String, String), law: &LawIR) -> bool {
        if let Some(&cached) = self.preserved_cache.get(&cache_key) {
            return cached;
        }
        let preserved = self.compute_law_preserved(law);
        self.preserved_cache.insert(cache_key, preserved);
        preserved
    }

    fn compute_law_preserved(&mut self, law: &LawIR) -> bool {
        if !self.base_writers_unchanged {
            return false;
        }
        let Some(base_fp) = law_footprint(law, &self.base_pool, &self.base_index, &mut self.work)
        else {
            return false;
        };
        let Some(target_fp) =
            law_footprint(law, &self.target_pool, &self.target_index, &mut self.work)
        else {
            return false;
        };
        if base_fp != target_fp
            || !self
                .extended_targets
                .is_disjoint(&base_fp.subject_and_ancestor_names)
        {
            return false;
        }
        for type_name in &base_fp.concrete_types {
            if charge_work(&mut self.work, 1).is_none() {
                return false;
            }
            let Some(Ok(base_obj)) = self.base_index.catalog.object_type_named(type_name) else {
                return false;
            };
            let Some(Ok(target_obj)) = self.target_index.catalog.object_type_named(type_name)
            else {
                return false;
            };
            if !effective_contract_unchanged(
                base_obj,
                target_obj,
                self.base_index.catalog.links_for_object_type(type_name),
                self.target_index.catalog.links_for_object_type(type_name),
            ) {
                return false;
            }
        }
        for new_writer in &self.new_writers {
            if charge_work(&mut self.work, 1).is_none() {
                return false;
            }
            match new_writer {
                WriterFootprint::Opaque => return false,
                WriterFootprint::Closed(touched) => {
                    if !touched.is_disjoint(&base_fp.subject_and_ancestor_names) {
                        return false;
                    }
                }
            }
        }
        true
    }
}

fn writer_identity_key(identity: &Value) -> String {
    norn_canonical::canonical_json_exact_of(identity)
        .ok()
        .and_then(|bytes| String::from_utf8(bytes).ok())
        .unwrap_or_default()
}

fn writer_statement_map(challenge: &Challenge) -> Option<BTreeMap<String, Value>> {
    let list = challenge.body()["writerStatements"].as_array()?;
    let mut map = BTreeMap::new();
    for entry in list {
        let key = writer_identity_key(entry.get("identity")?);
        if key.is_empty() || map.insert(key, entry.clone()).is_some() {
            return None;
        }
    }
    Some(map)
}

fn matching_goal(
    base_pool: &[ArtifactEnvelope],
    base: &Challenge,
    target_challenge: &Challenge,
    goal_index: u32,
    projected: Option<&mut ProjectedContext>,
) -> Option<usize> {
    let old = usize::try_from(goal_index)
        .ok()
        .and_then(|index| goals(base).get(index))?;
    let kind = old
        .pointer("/goal/kind")
        .and_then(Value::as_str)
        .unwrap_or("");
    if !matches!(kind, "initialization" | "preservation") {
        return None;
    }
    let law_index = old["law"].as_u64().and_then(|i| usize::try_from(i).ok())?;
    let law_identity = base.body()["lawStatements"]
        .as_array()
        .and_then(|a| a.get(law_index))?;
    let law_artifact_id = law_identity["artifactId"].as_str()?;
    let law_sha256 = law_identity["sha256"].as_str()?;
    let law_artifact = base_pool
        .iter()
        .find(|a| a.artifact_id == law_artifact_id)?;
    if exact_artifact_digest(law_artifact).ok().as_deref() != Some(law_sha256) {
        return None;
    }
    let law: LawIR = serde_json::to_value(law_artifact.payload.to_json_value())
        .ok()
        .and_then(|v| serde_json::from_value(v).ok())?;
    if !matches!(law.modality, LawModality::Always {} | LawModality::Never {})
        || law.timing.is_some()
    {
        return None;
    }
    if let Some(ctx) = projected
        && !ctx.law_is_preserved((law_artifact_id.to_owned(), law_sha256.to_owned()), &law)
    {
        return None;
    }
    let target_law = target_challenge.body()["lawStatements"]
        .as_array()
        .and_then(|laws| laws.iter().position(|entry| entry == law_identity))?;
    let mut target = old.clone();
    *target.get_mut("law")? = target_law.into();
    *target.pointer_mut("/statement/law")? = target_law.into();

    if kind == "preservation" {
        let old_writer_idx = old
            .pointer("/goal/writer")
            .and_then(Value::as_u64)
            .and_then(|i| usize::try_from(i).ok())?;
        let writer_identity = base.body()["writerStatements"]
            .as_array()
            .and_then(|writers| writers.get(old_writer_idx))?;
        let target_writer = target_challenge.body()["writerStatements"]
            .as_array()
            .and_then(|writers| writers.iter().position(|entry| entry == writer_identity))?;
        *target.pointer_mut("/goal/writer")? = target_writer.into();
        *target.pointer_mut("/statement/writer")? = target_writer.into();
    }

    let index = goals(target_challenge)
        .iter()
        .position(|goal| goal == &target)?;
    Some(index)
}

fn goals(challenge: &Challenge) -> &[Value] {
    challenge.body()["obligations"]
        .as_array()
        .map_or(&[], Vec::as_slice)
}
