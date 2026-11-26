//! Conjunctive and projected-safety specialisation provenance; no cache, solver
//! or proof promotion.
//!
//! Reference rules:
//! - [`RULE`] (`specialisation-conjunction/1`): law-only conjunctive extensions
//!   over a single-layer base with an identical whole-set writer inventory.
//! - [`PROJECTED_RULE`] (`specialisation-projected/1`): reviewed adapter code
//!   relying on `Specialisation.lean::projected_safety_stuttering` (where
//!   disjoint extension writers induce stuttering no-change steps on the
//!   projected base state). Supports multi-layer bases and domain extensions
//!   adding new entities, interfaces, actions, decide rules, policies, mappings,
//!   labels, and laws. Reuses prior current safety evidence for a base
//!   `Always`/`Never` law `L` only when the extension neither modifies nor
//!   widens `L`'s governed subject/read footprint nor introduces any writer
//!   (closed or opaque) that may touch it. This adapter establishes reviewed
//!   identity provenance, not a machine-checked correspondence of the Rust
//!   footprint analysis.
use std::collections::{BTreeMap, BTreeSet};

use norn_expr::expression_reads;
use norn_ir::mapping::MappingDocument;
use norn_ir::{
    ActionEffectProgram, ActionRoot, ArtifactEnvelope, ArtifactKind, EntityRoot, LawIR,
    LawModality, LawResponse, PropertyTypeName,
};
use serde_json::Value;

use super::evidence::{EvidenceIdentity, EvidencePins, EvidenceRecord};
use super::{
    ApprovedContract, Challenge, ChallengeError, DefinitionSource, exact_artifact_digest,
    exact_commitment,
};
use crate::entity_artifacts::{Decoded, decode_v2};
use crate::lifecycle_writes::FactSchema;
use crate::verification::LawWriter;
use crate::{EffectiveCatalog, EffectiveLink, EffectiveObjectType, EffectiveProperty};

/// Versioned, conservative conjunction-only reuse rule.
pub const RULE: &str = "specialisation-conjunction/1";

/// Versioned projected-safety reuse rule relying on
/// `Specialisation.lean::projected_safety_stuttering` (reviewed adapter glue).
pub const PROJECTED_RULE: &str = "specialisation-projected/1";

/// Deterministic work budget for projected-safety catalog indexing and
/// transitive footprint analysis; exceeding it fails closed (no reuse).
const MAX_PROJECTED_WORK: usize = 262_144;

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
    /// Specialisation reuse rule that justified carrying this record ([`RULE`] or [`PROJECTED_RULE`]).
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
    /// Valid carried records under [`RULE`] or [`PROJECTED_RULE`], with no increase in assurance.
    pub reused: Vec<ReusedEvidence>,
    /// Every goal not covered by a carried record must be checked afresh.
    pub recheck: Vec<usize>,
}

impl ApprovedContract {
    /// Admit and generate the complete target, then apply [`RULE`] (for
    /// law-only conjunctive extensions over a single-layer base with unchanged
    /// writers) or [`PROJECTED_RULE`] (for multi-layer bases and domain
    /// extensions whose new writers are provably disjoint from a base safety
    /// law's governed footprint).
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

/// Static write footprint of one newly introduced writer in the extension.
enum WriterFootprint {
    /// Provably closed set of touched object/interface type names (expanded
    /// across supertypes, implemented interfaces, and subtypes/implementers).
    Closed(BTreeSet<String>),
    /// Writer whose write set cannot be statically closed; fails closed.
    Opaque,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct LawFootprint {
    /// Concrete object types governed or read by the law.
    concrete_types: BTreeSet<String>,
    /// All governed/read type names plus their supertypes and interfaces.
    subject_and_ancestor_names: BTreeSet<String>,
}

/// Precomputed catalog lookup tables so footprint and link-closure queries stay
/// linear in the catalog size under a bounded work budget.
struct CatalogIndex {
    catalog: EffectiveCatalog,
    target_types: BTreeMap<String, BTreeSet<String>>,
    forward_links: BTreeMap<String, BTreeSet<String>>,
    inverse_links: BTreeMap<String, BTreeSet<String>>,
}

impl CatalogIndex {
    fn build(
        pool: &[ArtifactEnvelope],
        registry: &BTreeSet<PropertyTypeName>,
        work: &mut usize,
    ) -> Option<Self> {
        let catalog = EffectiveCatalog::from_artifacts_with_registry(pool, registry);
        let mut target_types: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
        for name in catalog.object_type_names() {
            charge_work(work, 1)?;
            let obj = catalog.object_type_named(name)?.ok()?;
            for ancestor in &obj.lineage {
                charge_work(work, 1)?;
                target_types
                    .entry(ancestor.as_str().to_owned())
                    .or_default()
                    .insert(name.to_owned());
            }
            for iface in &obj.interfaces {
                charge_work(work, 1)?;
                target_types
                    .entry(iface.as_str().to_owned())
                    .or_default()
                    .insert(name.to_owned());
            }
        }
        // Ensure declared interfaces with zero implementers still resolve to `{}`.
        for artifact in pool {
            if let Decoded::Root(root) = decode_v2(artifact)
                && let EntityRoot::V2Interface(iface) = *root
            {
                charge_work(work, 1)?;
                target_types
                    .entry(iface.interface.name.as_str().to_owned())
                    .or_default();
            }
        }

        let mut forward_links: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
        let mut inverse_links: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
        for name in catalog.object_type_names() {
            let Some(links) = catalog.links_for_object_type(name) else {
                continue;
            };
            for link in links {
                let targets = target_types.get(link.target.as_str())?;
                for target in targets {
                    charge_work(work, 1)?;
                    forward_links
                        .entry(name.to_owned())
                        .or_default()
                        .insert(target.clone());
                    inverse_links
                        .entry(target.clone())
                        .or_default()
                        .insert(name.to_owned());
                }
            }
        }
        Some(Self {
            catalog,
            target_types,
            forward_links,
            inverse_links,
        })
    }

    fn target_types(&self, name: &str) -> Option<&BTreeSet<String>> {
        self.target_types.get(name)
    }
}

fn charge_work(work: &mut usize, cost: usize) -> Option<()> {
    *work = work.saturating_add(cost);
    (*work <= MAX_PROJECTED_WORK).then_some(())
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

fn effective_property_unchanged(left: &EffectiveProperty, right: &EffectiveProperty) -> bool {
    left.id == right.id
        && left.kind == right.kind
        && left.constraints == right.constraints
        && left.nullable == right.nullable
        && left.default == right.default
        && left.protected() == right.protected()
        && left.computed == right.computed
        && left.computed_inputs == right.computed_inputs
        && left.computed_reads_all == right.computed_reads_all
        && left.markings == right.markings
        && left.record_members().len() == right.record_members().len()
        && left
            .record_members()
            .iter()
            .zip(right.record_members())
            .all(|(a, b)| effective_property_unchanged(a, b))
}

fn effective_contract_unchanged(
    base_obj: &EffectiveObjectType,
    target_obj: &EffectiveObjectType,
    base_links: Option<&[EffectiveLink]>,
    target_links: Option<&[EffectiveLink]>,
) -> bool {
    if base_obj.name != target_obj.name
        || base_obj.lineage != target_obj.lineage
        || base_obj.interfaces != target_obj.interfaces
        || base_obj.lifecycles != target_obj.lifecycles
        || base_obj.owned_collections != target_obj.owned_collections
        || base_obj.has_links != target_obj.has_links
        || base_obj.has_invariants != target_obj.has_invariants
        || base_obj.markings != target_obj.markings
        || base_obj.referenced_property_types != target_obj.referenced_property_types
        || base_obj.properties.len() != target_obj.properties.len()
        || base_obj.keys.len() != target_obj.keys.len()
    {
        return false;
    }
    if !base_obj
        .properties
        .iter()
        .zip(&target_obj.properties)
        .all(|(left, right)| effective_property_unchanged(left, right))
    {
        return false;
    }
    for (left, right) in base_obj.keys.iter().zip(&target_obj.keys) {
        if left.name != right.name
            || left.declared_by != right.declared_by
            || left.components != right.components
            || left.normalize != right.normalize
            || left.match_policy != right.match_policy
            || left.naming_series != right.naming_series
        {
            return false;
        }
    }
    match (base_links, target_links) {
        (Some(left), Some(right)) => {
            left.len() == right.len()
                && left.iter().zip(right).all(|(a, b)| {
                    a.id == b.id
                        && a.target == b.target
                        && a.cardinality == b.cardinality
                        && a.acyclic == b.acyclic
                        && a.inverse == b.inverse
                        && a.properties.len() == b.properties.len()
                        && a.properties
                            .iter()
                            .zip(&b.properties)
                            .all(|(lp, rp)| effective_property_unchanged(lp, rp))
                })
        }
        (None, None) => true,
        _ => false,
    }
}

/// Expands a type name (object type or interface) across its complete type
/// hierarchy: every concrete subtype/implementer (`target_types`), plus every
/// ancestor (`lineage`) and implemented interface (`interfaces`) of those types.
fn expand_type_hierarchy(
    type_name: &str,
    index: &CatalogIndex,
    out: &mut BTreeSet<String>,
    work: &mut usize,
) -> bool {
    if charge_work(work, 1).is_none() {
        return false;
    }
    let Some(targets) = index.target_types(type_name) else {
        return false;
    };
    out.insert(type_name.to_owned());
    for concrete in targets {
        if charge_work(work, 1).is_none() {
            return false;
        }
        let Some(Ok(obj)) = index.catalog.object_type_named(concrete) else {
            return false;
        };
        out.insert(concrete.clone());
        for ancestor in &obj.lineage {
            out.insert(ancestor.as_str().to_owned());
        }
        for iface in &obj.interfaces {
            out.insert(iface.as_str().to_owned());
        }
    }
    true
}

fn writer_footprint(
    writer: &LawWriter,
    pool: &[ArtifactEnvelope],
    index: &CatalogIndex,
    work: &mut usize,
) -> WriterFootprint {
    match writer {
        LawWriter::Transition { object_type, .. } => {
            let mut touched = BTreeSet::new();
            if expand_type_hierarchy(object_type, index, &mut touched, work) {
                WriterFootprint::Closed(touched)
            } else {
                WriterFootprint::Opaque
            }
        }
        LawWriter::Artifact { artifact_id } => {
            let matching: Vec<&ArtifactEnvelope> = pool
                .iter()
                .filter(|artifact| artifact.artifact_id == *artifact_id)
                .collect();
            let [artifact] = matching.as_slice() else {
                return WriterFootprint::Opaque;
            };
            artifact_writer_footprint(artifact, pool, index, work)
        }
    }
}

fn artifact_writer_footprint(
    artifact: &ArtifactEnvelope,
    pool: &[ArtifactEnvelope],
    index: &CatalogIndex,
    work: &mut usize,
) -> WriterFootprint {
    match artifact.kind() {
        ArtifactKind::Action => action_writer_footprint(artifact, pool, index, work),
        ArtifactKind::Rule => rule_writer_footprint(artifact, index, work),
        ArtifactKind::Mapping => mapping_writer_footprint(artifact, pool, index, work),
        ArtifactKind::Connector
        | ArtifactKind::Entity
        | ArtifactKind::Relation
        | ArtifactKind::Widget
        | ArtifactKind::Customization
        | ArtifactKind::Setting
        | ArtifactKind::BusinessPack
        | ArtifactKind::Policy
        | ArtifactKind::Derived
        | ArtifactKind::SourceSchema
        | ArtifactKind::Law
        | ArtifactKind::DefinedConcept
        | ArtifactKind::Labels
        | ArtifactKind::Model => WriterFootprint::Opaque,
    }
}

fn action_writer_footprint(
    artifact: &ArtifactEnvelope,
    pool: &[ArtifactEnvelope],
    index: &CatalogIndex,
    work: &mut usize,
) -> WriterFootprint {
    let Ok(payload) = serde_json::to_value(artifact.payload.to_json_value()) else {
        return WriterFootprint::Opaque;
    };
    let Ok(action) = serde_json::from_value::<ActionRoot>(payload) else {
        return WriterFootprint::Opaque;
    };
    // Outbound commands and quantity-ledger effects reach outside the closed
    // EntityIR write footprint; fail closed to Opaque.
    if !action.commands.is_empty() || action.quantity_effect.is_some() {
        return WriterFootprint::Opaque;
    }
    let Some(program) = &action.effect_program else {
        return WriterFootprint::Opaque;
    };

    let mut raw_types = BTreeSet::new();
    let target_kind = program.target().object_kind;
    raw_types.insert(target_kind.to_owned());

    if matches!(program, ActionEffectProgram::MutateParentWithRows { .. }) {
        let Some(targets) = index.target_types(target_kind) else {
            return WriterFootprint::Opaque;
        };
        for concrete in targets {
            let Some(Ok(parent)) = index.catalog.object_type_named(concrete) else {
                return WriterFootprint::Opaque;
            };
            for col in &parent.owned_collections {
                raw_types.insert(col.child_type.as_str().to_owned());
            }
        }
    }
    for write in &action.writes {
        raw_types.insert(write.entity_kind.clone());
    }
    for link in &action.create_links {
        raw_types.insert(link.target_type.clone());
        if let Some(source) = &link.source {
            raw_types.insert(source.object_type.clone());
        }
    }

    let mut touched = BTreeSet::new();
    for name in &raw_types {
        if !expand_type_hierarchy(name, index, &mut touched, work) {
            return WriterFootprint::Opaque;
        }
    }

    if let Some(pin) = &action.synchronous_rule {
        let matched: Vec<&ArtifactEnvelope> = pool
            .iter()
            .filter(|candidate| {
                candidate.artifact_id == pin.artifact_id
                    && candidate.version == pin.version
                    && candidate.kind() == ArtifactKind::Rule
            })
            .collect();
        let [rule_artifact] = matched.as_slice() else {
            return WriterFootprint::Opaque;
        };
        match rule_writer_footprint(rule_artifact, index, work) {
            WriterFootprint::Closed(rule_types) => touched.extend(rule_types),
            WriterFootprint::Opaque => return WriterFootprint::Opaque,
        }
    }

    WriterFootprint::Closed(touched)
}

/// Whether `output_schema.properties.<field>` is guaranteed to be empty
/// (`maxItems == 0`, or omitted under `additionalProperties: false`).
fn schema_array_must_be_empty(output_schema: &norn_ir::JsonValue, field: &str) -> bool {
    let Some(root) = output_schema.as_object() else {
        return false;
    };
    let Some(properties) = root
        .get("properties")
        .and_then(norn_ir::JsonValue::as_object)
    else {
        return false;
    };
    let Some(entry) = properties.get(field) else {
        return matches!(
            root.get("additionalProperties"),
            Some(norn_ir::JsonValue::Bool(false))
        );
    };
    let Some(entry_obj) = entry.as_object() else {
        return false;
    };
    entry_obj
        .get("type")
        .and_then(norn_ir::JsonValue::as_string)
        == Some("array")
        && matches!(
            entry_obj.get("maxItems"),
            Some(norn_ir::JsonValue::Number(n)) if n.is_finite() && n.abs().to_bits() == 0
        )
}

fn schema_string_enum<'a>(schema: &'a norn_ir::JsonValue, field: &str) -> Option<Vec<&'a str>> {
    let prop = schema
        .as_object()?
        .get("properties")?
        .as_object()?
        .get(field)?
        .as_object()?;
    if prop.get("type")?.as_string()? != "string" {
        return None;
    }
    let values = prop.get("enum")?.as_array()?;
    let strings: Vec<&str> = values
        .iter()
        .filter_map(norn_ir::JsonValue::as_string)
        .collect();
    (!strings.is_empty() && strings.len() == values.len()).then_some(strings)
}

fn rule_writer_footprint(
    artifact: &ArtifactEnvelope,
    index: &CatalogIndex,
    work: &mut usize,
) -> WriterFootprint {
    let payload = artifact.payload.to_json_value();
    let Some(object) = payload.as_object() else {
        return WriterFootprint::Opaque;
    };
    let is_decide = object
        .get("contract")
        .and_then(norn_ir::JsonValue::as_string)
        == Some("decide")
        && object.contains_key("graph");
    if !is_decide {
        return WriterFootprint::Opaque;
    }
    // External write-back commands on a rule leave the closed entity footprint.
    if object
        .get("sourceCommands")
        .is_some_and(|cmds| !cmds.as_array().is_some_and(<[_]>::is_empty))
    {
        return WriterFootprint::Opaque;
    }
    let Some(output_schema) = object.get("outputSchema") else {
        return WriterFootprint::Opaque;
    };
    if !schema_array_must_be_empty(output_schema, "commands")
        || !schema_array_must_be_empty(output_schema, "timers")
    {
        return WriterFootprint::Opaque;
    }
    let Some(schema) = FactSchema::of(output_schema) else {
        return WriterFootprint::Opaque;
    };
    let Some(object_types) = schema.values("objectType") else {
        return WriterFootprint::Opaque;
    };
    if object_types.is_empty() {
        return WriterFootprint::Opaque;
    }
    let mut touched = BTreeSet::new();
    for type_name in object_types {
        if !expand_type_hierarchy(type_name, index, &mut touched, work) {
            return WriterFootprint::Opaque;
        }
    }
    // If `links` is declared and not guaranteed empty, include every sourceType
    // and targetType in its closed enum (fail closed to Opaque otherwise).
    if !schema_array_must_be_empty(output_schema, "links") {
        let Some(links_obj) = output_schema
            .as_object()
            .and_then(|root| root.get("properties"))
            .and_then(norn_ir::JsonValue::as_object)
            .and_then(|props| props.get("links"))
            .and_then(norn_ir::JsonValue::as_object)
        else {
            return WriterFootprint::Opaque;
        };
        if links_obj
            .get("type")
            .and_then(norn_ir::JsonValue::as_string)
            != Some("array")
        {
            return WriterFootprint::Opaque;
        }
        let Some(items) = links_obj.get("items") else {
            return WriterFootprint::Opaque;
        };
        let (Some(sources), Some(targets)) = (
            schema_string_enum(items, "sourceType"),
            schema_string_enum(items, "targetType"),
        ) else {
            return WriterFootprint::Opaque;
        };
        for type_name in sources.into_iter().chain(targets) {
            if !expand_type_hierarchy(type_name, index, &mut touched, work) {
                return WriterFootprint::Opaque;
            }
        }
    }
    WriterFootprint::Closed(touched)
}

fn mapping_writer_footprint(
    artifact: &ArtifactEnvelope,
    pool: &[ArtifactEnvelope],
    index: &CatalogIndex,
    work: &mut usize,
) -> WriterFootprint {
    let Ok(payload) = serde_json::to_value(artifact.payload.to_json_value()) else {
        return WriterFootprint::Opaque;
    };
    let Ok(MappingDocument::SourceRecord(mapping)) =
        serde_json::from_value::<MappingDocument>(payload)
    else {
        return WriterFootprint::Opaque;
    };
    if !mapping.entitlements.is_empty() || mapping.maps.is_empty() {
        return WriterFootprint::Opaque;
    }
    let mut touched = BTreeSet::new();
    for map in &mapping.maps {
        let pinned_version = artifact
            .dependencies
            .iter()
            .find(|dep| dep.required && dep.artifact_id == map.target.entity)
            .and_then(|dep| dep.version.as_deref());
        let matched: Vec<&ArtifactEnvelope> = pool
            .iter()
            .filter(|candidate| {
                candidate.artifact_id == map.target.entity
                    && pinned_version.is_none_or(|ver| candidate.version == ver)
            })
            .collect();
        let [entity_artifact] = matched.as_slice() else {
            return WriterFootprint::Opaque;
        };
        let Some(Ok(obj)) = index.catalog.object_type(&entity_artifact.artifact_id) else {
            return WriterFootprint::Opaque;
        };
        if !expand_type_hierarchy(obj.name.as_str(), index, &mut touched, work) {
            return WriterFootprint::Opaque;
        }
        if !map.links.is_empty() {
            let Some(declared_links) = index.catalog.links_for_object_type(obj.name.as_str())
            else {
                return WriterFootprint::Opaque;
            };
            for link_map in &map.links {
                let Some(link) = declared_links.iter().find(|l| l.id == link_map.link) else {
                    return WriterFootprint::Opaque;
                };
                if !expand_type_hierarchy(link.target.as_str(), index, &mut touched, work) {
                    return WriterFootprint::Opaque;
                }
            }
        }
    }
    WriterFootprint::Closed(touched)
}

/// Computes the governed and read type footprint of a safety law `L`.
///
/// Includes `L.subject`, every type named in `L.scope` and `L.response`, every
/// concrete subtype/implementer of those types, and—whenever any expression of
/// `L` reads beyond direct non-computed stored properties of `self`—the
/// transitive forward/inverse link closure of those concrete types. Returns
/// `None` (fail closed) on unresolved types, non-state responses, or work-budget
/// exhaustion.
fn law_footprint(
    law: &LawIR,
    pool: &[ArtifactEnvelope],
    index: &CatalogIndex,
    work: &mut usize,
) -> Option<LawFootprint> {
    let mut named_types = BTreeSet::new();
    named_types.insert(law.subject.clone());
    for ty in &law.scope.object_types {
        named_types.insert(ty.clone());
    }
    for ty in &law.scope.commitments {
        named_types.insert(ty.clone());
    }
    for state in &law.scope.lifecycle_states {
        named_types.insert(state.object_type.clone());
    }

    let mut expressions: Vec<String> = Vec::new();
    if let Some(cond) = &law.condition {
        expressions.push(cond.as_str().to_owned());
    }
    for exception in &law.exceptions {
        expressions.push(exception.condition.as_str().to_owned());
    }
    let mut conservative_links_required = false;
    match &law.response {
        LawResponse::Predicate { expression } => {
            expressions.push(expression.as_str().to_owned());
        }
        LawResponse::Invariant {
            object_type,
            invariant_id,
        } => {
            named_types.insert(object_type.clone());
            let found =
                collect_invariant_expressions(pool, &index.catalog, object_type, invariant_id);
            if found.is_empty() {
                conservative_links_required = true;
            } else {
                expressions.extend(found);
            }
        }
        LawResponse::Transition { object_type, .. } => {
            named_types.insert(object_type.clone());
            conservative_links_required = true;
        }
        LawResponse::TranslationPreserves { target_type, .. } => {
            named_types.insert(target_type.clone());
            conservative_links_required = true;
        }
        LawResponse::Action { .. } => return None,
    }

    let mut concrete_types = BTreeSet::new();
    for name in &named_types {
        charge_work(work, 1)?;
        concrete_types.extend(index.target_types(name)?.iter().cloned());
    }

    let linked_types = collect_linked_footprint_types(
        &concrete_types,
        &expressions,
        conservative_links_required,
        index,
        work,
    )?;
    concrete_types.extend(linked_types);

    let mut subject_and_ancestor_names = named_types;
    for concrete in &concrete_types {
        charge_work(work, 1)?;
        let obj = index.catalog.object_type_named(concrete)?.ok()?;
        subject_and_ancestor_names.insert(concrete.clone());
        for ancestor in &obj.lineage {
            subject_and_ancestor_names.insert(ancestor.as_str().to_owned());
        }
        for iface in &obj.interfaces {
            subject_and_ancestor_names.insert(iface.as_str().to_owned());
        }
    }

    Some(LawFootprint {
        concrete_types,
        subject_and_ancestor_names,
    })
}

fn collect_linked_footprint_types(
    concrete_types: &BTreeSet<String>,
    expressions: &[String],
    conservative_links_required: bool,
    index: &CatalogIndex,
    work: &mut usize,
) -> Option<BTreeSet<String>> {
    let mut visited = BTreeSet::new();
    let mut pending = Vec::new();
    for concrete in concrete_types {
        charge_work(work, 1)?;
        let obj = index.catalog.object_type_named(concrete)?.ok()?;
        let direct_only = !conservative_links_required
            && expressions
                .iter()
                .all(|expr| reads_only_direct_stored_properties(expr, obj));
        if !direct_only && visited.insert(concrete.clone()) {
            pending.push(concrete.clone());
        }
    }
    // Transitive fixed-point closure over precomputed forward and inverse links
    // so multi-hop reads (via computed properties, invariants, or conditions)
    // are covered in linear time under `MAX_PROJECTED_WORK`.
    let mut linked_types = BTreeSet::new();
    while let Some(current) = pending.pop() {
        for neighbors in [
            index.forward_links.get(&current),
            index.inverse_links.get(&current),
        ]
        .into_iter()
        .flatten()
        {
            for neighbor in neighbors {
                charge_work(work, 1)?;
                linked_types.insert(neighbor.clone());
                if visited.insert(neighbor.clone()) {
                    pending.push(neighbor.clone());
                }
            }
        }
    }
    Some(linked_types)
}

fn reads_only_direct_stored_properties(expression: &str, object: &EffectiveObjectType) -> bool {
    let Ok(reads) = expression_reads(expression, "self") else {
        return false;
    };
    if reads.whole_root {
        return false;
    }
    reads.fields.iter().all(|field| {
        object
            .property(field)
            .is_some_and(|prop| !prop.computed && prop.unsupported().is_none())
    })
}

fn collect_invariant_expressions(
    pool: &[ArtifactEnvelope],
    catalog: &EffectiveCatalog,
    object_type: &str,
    invariant_id: &str,
) -> Vec<String> {
    let mut owners = BTreeSet::from([object_type.to_owned()]);
    if let Some(Ok(obj)) = catalog.object_type_named(object_type) {
        for ancestor in &obj.lineage {
            owners.insert(ancestor.as_str().to_owned());
        }
        for iface in &obj.interfaces {
            owners.insert(iface.as_str().to_owned());
        }
    }
    let mut out = Vec::new();
    for artifact in pool {
        if let Decoded::Root(root) = decode_v2(artifact) {
            match *root {
                EntityRoot::V2(entity) if owners.contains(entity.object_type.name.as_str()) => {
                    for inv in &entity.object_type.invariants {
                        if inv.id.as_str() == invariant_id {
                            out.push(inv.expression.as_str().to_owned());
                        }
                    }
                }
                EntityRoot::V2Interface(iface)
                    if owners.contains(iface.interface.name.as_str()) =>
                {
                    for inv in &iface.interface.invariants {
                        if inv.id.as_str() == invariant_id {
                            out.push(inv.expression.as_str().to_owned());
                        }
                    }
                }
                EntityRoot::V2(_)
                | EntityRoot::V2Interface(_)
                | EntityRoot::V2Extension(_)
                | EntityRoot::V2PropertyTypes(_)
                | EntityRoot::V2Markings(_)
                | EntityRoot::V1(_) => {}
            }
        }
    }
    out
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
