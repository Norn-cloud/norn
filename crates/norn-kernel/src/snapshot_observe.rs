//! Observed snapshot reconciliation (#2990, lead decision 2026-10-02).
//!
//! Observations record history, rather than authorizing commands. They use
//! the effective declared lifecycle, skip guards, and retain the snapshot's
//! provenance. An ambiguous or unreachable move emits a unified quality
//! finding, never a `FactChange::Set` of a lifecycle property. Hosts admit the
//! source and mapping before calling this path; command paths remain unchanged.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::convert::Infallible;
use std::ops::{Deref, DerefMut};

use norn_ir::mapping::{ObservedLifecycleSnapshot, SnapshotDeviationReason};
use norn_ir::provenance::InstanceProvenance;
use norn_ir::{
    ConstraintClass, DataQualityFinding, DataQualityFindingStatus, DataQualityLineage,
    DataQualityOutcome, DataQualityRuleRef, DataQualitySeverity, DataQualitySubject, FactChange,
    Lifecycle, PropertyId, Transition,
};
use serde::{Deserialize, Serialize};

use crate::cell_host::{
    CellCommit, CellHost, CommitError, CommitReceipt, Consumed, FactExpectation, FactKey,
    FactMutation, FactState, FactVersion, FactWrite, JsonObject, JsonValue, LinkMutation,
    LinkReadExpectation, LinkWrite,
};
use crate::commit_check::{InvariantEvaluationError, InvariantFailure, evaluate_invariants};
use crate::decision_commit::{
    DecisionRefusal, DecisionTurn, FactEvent, FactSlice, TypeCatalog, modified_event,
    prepare_observed_properties,
};
use crate::envelope::{EnvelopeCounts, check_decision_envelope};
use crate::limits::EnvelopeLimits;
use norn_expr::EvaluationBudget;

/// Why a declared observed transition was synthesized.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ObservationSynthesis {
    /// A unique shortest declared path catches up to an observed snapshot.
    SnapshotCatchUp,
}

/// Provenance-bearing ledger facts from an observed snapshot.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(
    tag = "op",
    rename_all = "snake_case",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum ObservationEvent {
    /// Ordinary property observations (creation still uses initial states).
    ObservedFact {
        /// The resolved fact change.
        event: FactEvent,
        /// The source snapshot.
        provenance: InstanceProvenance,
    },
    /// One step of the unique shortest declared path, with guards skipped.
    ObservedTransition {
        /// The object.
        key: FactKey,
        /// The lifecycle property.
        property: PropertyId,
        /// The declared transition identity.
        transition: norn_ir::TransitionId,
        /// The state it left.
        from: String,
        /// The state it entered.
        to: String,
        /// The snapshot reconciliation marker.
        synthesized: ObservationSynthesis,
        /// The same provenance and observed time on every synthesized step.
        provenance: InstanceProvenance,
    },
    /// One source observation violated a declared constraint or built-in rule.
    /// The source snapshot is retained as lineage; external state is committed.
    DataQualityFinding {
        /// The typed, versioned finding.
        fact: DataQualityFinding,
    },
    /// The source deleted the object (#3013). Nothing is erased: either its
    /// lifecycle reached the mapping's `onDelete` terminal state through
    /// observed transitions (emitted before this event), or it was
    /// tombstoned ([`OBSERVED_TOMBSTONE_TYPE`]).
    ObservedDeleted {
        /// The object.
        key: FactKey,
        /// Whether a tombstone records the delete (no declared path to an
        /// `onDelete` state).
        tombstoned: bool,
        /// The source delete.
        provenance: InstanceProvenance,
    },
    /// A tombstoned object was observed again under the same key: the
    /// tombstone is lifted (#3013).
    ObservedRecreated {
        /// The object.
        key: FactKey,
        /// The source record that recreated it.
        provenance: InstanceProvenance,
    },
}

/// The system type of observed-delete tombstones (#3013): one object per
/// tombstoned object, keyed by [`tombstone_key`], with `objectType`, `key`,
/// `at` (when the source deleted it), `source` (the source record's stable
/// id), `mappingVersion`, and `provenance` (the source delete's
/// `InstanceProvenance`). Hosts declare it in their store model.
pub const OBSERVED_TOMBSTONE_TYPE: &str = "norn.ObservedTombstone";

/// Host marker recording which projected objects are owned by source
/// observations. Its key uses the same composed `objectType/key` layout as a
/// tombstone identity.
pub const SOURCE_OWNERSHIP_TYPE: &str = "norn.ObservedSource";

/// The tombstone of `key`: `norn.ObservedTombstone/<objectType>/<key>`.
#[must_use]
pub fn tombstone_key(key: &FactKey) -> FactKey {
    FactKey::new(
        OBSERVED_TOMBSTONE_TYPE,
        format!("{}/{}", key.object_type.as_str(), key.key),
    )
}

/// A snapshot was invalid; nothing is committed.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum ObservationRefusal {
    /// Existing decision admission rejects the ordinary property payload.
    #[error("invalid observed properties: {0}")]
    Properties(#[source] Box<DecisionRefusal<Infallible>>),
    /// The object was not explicitly read at its current version.
    #[error("observed object {key} is not in the supplied slice")]
    NotInSlice {
        /// The object.
        key: FactKey,
    },
    /// A lifecycle is absent, unresolved, duplicated, or has an invalid state.
    #[error("invalid observed lifecycle {property:?}: {reason}")]
    InvalidLifecycle {
        /// The lifecycle property.
        property: String,
        /// The rejection reason.
        reason: String,
    },
    /// The finding or tombstone evidence does not encode canonically.
    #[error("observed evidence does not encode canonically: {0}")]
    Encoding(String),
    /// The complete observation exceeds the existing decision event cap.
    #[error("observed snapshot exceeds the event limit {limit} ({actual} events)")]
    EventLimit {
        /// The existing profile's event limit.
        limit: u32,
        /// The event count.
        actual: usize,
    },
    /// Runtime invariant evaluation exceeded the existing per-commit fuel
    /// budget; nothing was committed.
    #[error("invariant evaluation for {key} exceeded the {limit}-unit budget")]
    InvariantBudgetExceeded {
        /// The object being evaluated when the budget was exhausted.
        key: FactKey,
        /// The profile's invariant evaluation budget.
        limit: u64,
    },
    /// The admitted model references an interface absent from the runtime
    /// catalog, so its invariant set cannot be resolved fail-closed.
    #[error("{key}: invariant model is missing interface {interface}")]
    InvariantModel {
        /// The object whose model resolution failed.
        key: FactKey,
        /// The unresolved interface.
        interface: String,
    },
    /// An effective property default cannot be represented as an exact
    /// runtime value, so the observed object cannot be materialized.
    #[error("{key}: effective observed values were refused: {reason}")]
    EffectiveValues {
        /// The object whose effective values failed to materialize.
        key: FactKey,
        /// Why a declared default was not an exact runtime value.
        reason: String,
    },
    /// The object's version cannot advance without overflowing.
    #[error("observed object version overflow")]
    Overflow,
}

/// Durable authority over one observed output's outgoing relations.
/// Mapping versions may advance within the same admitted mapping identity.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ObservedSourceOwner {
    /// The source system admitted by this mapping. Individual source records
    /// carry lineage; the mapping declaration owns the relation namespace.
    pub source_system: String,
    /// The mapping artifact identity, without its version suffix.
    pub mapping: String,
}

impl ObservedSourceOwner {
    /// Decode retained ownership evidence; malformed markers never confer authority.
    #[must_use]
    pub fn from_value(value: &JsonValue) -> Option<Self> {
        norn_ir::runtime::decode(value).ok()
    }

    /// Derive authority from the admitted mapping's provenance.
    #[must_use]
    pub fn from_provenance(provenance: &InstanceProvenance) -> Self {
        Self {
            source_system: provenance.source.system.clone(),
            mapping: provenance
                .mapping_version
                .as_str()
                .split_once('@')
                .map_or(provenance.mapping_version.as_str(), |(id, _)| id)
                .to_owned(),
        }
    }
}

/// Opaque authority for exact observed writes, read-bound finding-only
/// evidence. Callers can combine authorities
/// returned by preparation, but
/// cannot construct one from serialized provenance or an arbitrary commit.
#[derive(Debug, Clone, PartialEq)]
pub struct ObservationAuthorization {
    reference_proof: Option<Box<ObservedReferenceReads>>,
    reference_targets: BTreeSet<FactKey>,
    source_scopes: Vec<(FactExpectation, ObservedSourceOwner)>,
    links: Vec<LinkWrite>,
    owner_recreations: Vec<(FactKey, ObservedSourceOwner, FactWrite)>,
    pending_writes: Vec<FactWrite>,
    pending_sources: BTreeSet<FactKey>,
    prepared_items: usize,
    snapshot_chunks: Option<Box<SnapshotEventChunks>>,
    writes: Vec<AuthorizedObservedWrite>,
    observed_reads: Vec<FactExpectation>,
    read_findings: Vec<AuthorizedObservedReadFinding>,
}

/// Trusted mapping preparation uses existing relation targets only as identities.
/// Bind that attribution to the complete prepared output before publication.
#[derive(Debug, Clone, PartialEq)]
struct ObservedReferenceReads {
    reads: Vec<FactExpectation>,
    references: Vec<FactExpectation>,
    writes: Vec<FactWrite>,
    links: Vec<LinkWrite>,
    link_reads: Vec<LinkReadExpectation>,
    consumes: Option<Consumed>,
    event_count: usize,
}

/// Maximum independently prepared objects in one atomic sealed snapshot batch.
pub const MAX_OBSERVATION_SNAPSHOT_ITEMS: usize = 20_000;

#[derive(Debug, Clone, PartialEq)]
struct SnapshotEventChunks {
    counts: Vec<usize>,
    consumes: Consumed,
    base_events: usize,
}

#[derive(Debug, Clone, PartialEq)]
struct AuthorizedObservedWrite {
    write: FactWrite,
    provenance: InstanceProvenance,
    observation_id: String,
    findings: Vec<DataQualityFinding>,
}

type PreparedFindingComparison<'a> = (
    Result<Vec<DataQualityFinding>, ObservationRefusal>,
    &'a [DataQualityFinding],
);

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct AuthorizedObservedReadFinding {
    pub(crate) key: FactKey,
    pub(crate) expected: FactVersion,
    pub(crate) values: JsonObject,
    pub(crate) provenance: InstanceProvenance,
    pub(crate) observation_id: String,
    pub(crate) findings: Vec<DataQualityFinding>,
}

impl ObservationAuthorization {
    /// An empty authorization with no prepared writes or read-bound findings.
    #[must_use]
    pub fn empty() -> Self {
        Self {
            reference_proof: None,
            reference_targets: BTreeSet::new(),
            source_scopes: Vec::new(),
            links: Vec::new(),
            owner_recreations: Vec::new(),
            pending_writes: Vec::new(),
            pending_sources: BTreeSet::new(),
            prepared_items: 0,
            snapshot_chunks: None,
            writes: Vec::new(),
            observed_reads: Vec::new(),
            read_findings: Vec::new(),
        }
    }

    fn for_source(key: &FactKey, expected: FactVersion, provenance: &InstanceProvenance) -> Self {
        let mut authorization = Self::empty();
        authorization.source_scopes.push((
            FactExpectation {
                key: key.clone(),
                expected,
            },
            ObservedSourceOwner::from_provenance(provenance),
        ));
        authorization
    }

    fn authorize(
        &mut self,
        catalog: &TypeCatalog<'_>,
        write: &FactWrite,
        provenance: &InstanceProvenance,
        observation_id: &str,
        failures: &[InvariantFailure],
    ) -> Result<(), ObservationRefusal> {
        let FactMutation::Put { values } = &write.mutation else {
            return Err(ObservationRefusal::Encoding(
                "observed invariant evidence requires a complete value write".to_owned(),
            ));
        };
        let findings = findings_for(
            catalog,
            &write.key,
            values,
            provenance,
            observation_id,
            failures,
        )?;
        self.writes.push(AuthorizedObservedWrite {
            write: write.clone(),
            provenance: provenance.clone(),
            observation_id: observation_id.to_owned(),
            findings,
        });
        Ok(())
    }

    fn authorize_read_findings(
        &mut self,
        catalog: &TypeCatalog<'_>,
        expectation: &FactExpectation,
        values: &JsonObject,
        provenance: &InstanceProvenance,
        observation_id: &str,
        failures: &[InvariantFailure],
    ) -> Result<(), ObservationRefusal> {
        if failures.is_empty() {
            return Ok(());
        }
        let findings = findings_for(
            catalog,
            &expectation.key,
            values,
            provenance,
            observation_id,
            failures,
        )?;
        if !findings.is_empty() {
            self.read_findings.push(AuthorizedObservedReadFinding {
                key: expectation.key.clone(),
                expected: expectation.expected,
                values: values.clone(),
                provenance: provenance.clone(),
                observation_id: observation_id.to_owned(),
                findings,
            });
        }
        Ok(())
    }

    /// Combine authorities produced by independent prepared source snapshots.
    /// Duplicate writes are rejected later when the complete commit is bound.
    pub fn merge(&mut self, other: Self) {
        self.reference_proof = None;
        self.reference_targets.extend(other.reference_targets);
        self.prepared_items = self.prepared_items.saturating_add(other.prepared_items);
        self.snapshot_chunks = None;
        self.source_scopes.extend(other.source_scopes);
        self.links.extend(other.links);
        self.owner_recreations.extend(other.owner_recreations);
        self.pending_writes.extend(other.pending_writes);
        self.pending_sources.extend(other.pending_sources);
        self.writes.extend(other.writes);
        self.observed_reads.extend(other.observed_reads);
        self.read_findings.extend(other.read_findings);
    }

    /// Seal identity-only dependencies at the trusted `MappingIR` publication
    /// boundary. Mapped values have already been evaluated from the source;
    /// these external targets were loaded solely to validate relation intent.
    /// Observed sources and finding inputs retain their complete value lineage.
    ///
    /// # Errors
    /// The commit does not match the opaque prepared observation authority.
    pub fn seal_link_reference_reads<E, C, T>(
        &mut self,
        commit: &CellCommit<E, C, T>,
    ) -> Result<(), ObservationRefusal> {
        self.reference_proof = None;
        if !self.validates(commit) {
            return Err(ObservationRefusal::Encoding(
                "reference attribution requires an authorized observation".to_owned(),
            ));
        }
        let references = commit
            .reads
            .iter()
            .filter(|read| {
                self.reference_targets.contains(&read.key)
                    && !self
                        .source_scopes
                        .iter()
                        .any(|(source, _)| source.key == read.key)
                    && !self
                        .read_findings
                        .iter()
                        .any(|finding| finding.key == read.key)
                    && !commit.writes.iter().any(|write| write.key == read.key)
            })
            .cloned()
            .collect();
        self.reference_proof = Some(Box::new(ObservedReferenceReads {
            reads: commit.reads.clone(),
            references,
            writes: commit.writes.clone(),
            links: commit.links.clone(),
            link_reads: commit.link_reads.clone(),
            consumes: commit.consumes.clone(),
            event_count: commit.events.len(),
        }));
        Ok(())
    }

    /// Return sealed references only while all prepared outputs and guards
    /// still match. Added guards or changed outputs restore full classification.
    #[must_use]
    pub fn reference_reads<E, C, T>(&self, commit: &CellCommit<E, C, T>) -> &[FactExpectation] {
        self.reference_proof
            .as_ref()
            .filter(|proof| {
                proof.reads == commit.reads
                    && proof.writes == commit.writes
                    && proof.links == commit.links
                    && proof.link_reads == commit.link_reads
                    && proof.consumes == commit.consumes
                    && proof.event_count == commit.events.len()
                    && commit.effects.is_empty()
            })
            .map_or(&[], |proof| proof.references.as_slice())
    }

    /// Keep one authority for each exact final aggregated write.
    ///
    /// When the same final write is prepared more than once while folding
    /// mapped items (for example A→B→A→B), the last matching authority wins.
    /// Its provenance and findings describe the item that produced the final
    /// stored value; intermediate writes do not get separate final-write
    /// findings.
    pub fn retain_writes(&mut self, writes: &[FactWrite]) {
        let mut retained = Vec::<AuthorizedObservedWrite>::new();
        for authorized in self
            .writes
            .drain(..)
            .filter(|authorized| writes.contains(&authorized.write))
        {
            if let Some(prior) = retained
                .iter_mut()
                .find(|prior| prior.write == authorized.write)
            {
                *prior = authorized;
            } else {
                retained.push(authorized);
            }
        }
        self.writes = retained;
        self.observed_reads
            .retain(|read| !writes.iter().any(|write| write.key == read.key));
        self.observed_reads.sort_by(|a, b| a.key.cmp(&b.key));
        self.observed_reads.dedup();
    }

    /// Keep findings bound to the final effective state of guarded reads,
    /// drop authorities superseded by writes or later values, and coalesce
    /// only exact duplicate sealed authorities.
    ///
    /// `final_slice` must carry the effective post-fold state and original
    /// expected version for each guarded key. This only filters authorities
    /// that were sealed by preparation; it cannot create one.
    pub fn retain_read_findings(
        &mut self,
        reads: &[FactExpectation],
        writes: &[FactWrite],
        final_slice: &FactSlice,
    ) {
        self.read_findings.retain(|authorized| {
            !writes.iter().any(|write| write.key == authorized.key)
                && reads
                    .iter()
                    .any(|read| read.key == authorized.key && read.expected == authorized.expected)
                && final_slice.get(&authorized.key).is_some_and(|state| {
                    state.version == authorized.expected
                        && state.values.as_ref() == Some(&authorized.values)
                })
        });
        let mut coalesced = Vec::<AuthorizedObservedReadFinding>::new();
        for authorized in self.read_findings.drain(..) {
            if !coalesced.contains(&authorized) {
                coalesced.push(authorized);
            }
        }
        self.read_findings = coalesced;
    }

    pub(crate) fn read_findings(&self) -> &[AuthorizedObservedReadFinding] {
        &self.read_findings
    }

    /// Seal an authorized successor mapping's recreation of an already-owned
    /// retired source. The host must establish that the previous mapping is
    /// withdrawn and the successor is admitted in the current definitions.
    /// This is an explicit ownership admission path, never inferred from a
    /// tombstone alone. Kernel admission verifies the retained owner, exact
    /// marker CAS/write, guarded source scope, and tombstone removal.
    ///
    /// # Errors
    /// Missing retained ownership, mismatched marker CAS or successor scope.
    pub fn seal_source_owner_recreation(
        &mut self,
        source: &FactKey,
        before: &FactState,
        write: &FactWrite,
    ) -> Result<(), ObservationRefusal> {
        let retained = before
            .values
            .as_ref()
            .and_then(|values| values.get("owner"))
            .and_then(ObservedSourceOwner::from_value);
        let proposed = match &write.mutation {
            FactMutation::Put { values } => values
                .get("owner")
                .and_then(ObservedSourceOwner::from_value),
            FactMutation::Delete => None,
        };
        let Some(retained) = retained else {
            return Err(ObservationRefusal::Encoding(
                "recreation requires retained source ownership".to_owned(),
            ));
        };
        if write.expected != before.version
            || write.key
                != FactKey::new(
                    SOURCE_OWNERSHIP_TYPE,
                    format!("{}/{}", source.object_type.as_str(), source.key),
                )
            || !self
                .source_scopes
                .iter()
                .any(|(scope, owner)| scope.key == *source && proposed.as_ref() == Some(owner))
        {
            return Err(ObservationRefusal::Encoding(
                "recreation does not match successor ownership scope".to_owned(),
            ));
        }
        self.owner_recreations
            .push((source.clone(), retained, write.clone()));
        Ok(())
    }

    pub(crate) fn permits_source_owner_recreation(
        &self,
        source: &FactKey,
        retained: &ObservedSourceOwner,
        write: &FactWrite,
    ) -> bool {
        self.owner_recreations
            .iter()
            .any(|(key, owner, proposed)| key == source && owner == retained && proposed == write)
    }

    /// Seal the exact writes produced by one admitted mapping relation intent.
    /// Additions name only its declared target; removals are allowed only for
    /// a mapped to-one replacement or explicit clear. The source must already
    /// have opaque snapshot authority. Hosts call this at trusted mapping
    /// preparation, or with a CAS-read pending intent at target resolution.
    ///
    /// # Errors
    /// Missing source authority, undeclared role, or an unrelated mutation.
    pub fn seal_link_intent(
        &mut self,
        catalog: &TypeCatalog<'_>,
        source: &FactKey,
        link: &str,
        target: Option<&FactKey>,
        writes: &[LinkWrite],
    ) -> Result<(), ObservationRefusal> {
        let model = crate::CommitModel::new(catalog);
        let canonical = model.canonical(source);
        if !self
            .source_scopes
            .iter()
            .any(|(scope, _)| model.canonical(&scope.key) == canonical)
        {
            return Err(ObservationRefusal::Encoding(
                "link intent has no prepared observed source".to_owned(),
            ));
        }
        // Retain unchanged declared intents too: a later source update may
        // reuse an existing relation without emitting a new LinkWrite.
        if let Some(target) = target
            && model
                .link(&source.object_type, link)
                .is_some_and(|declared| model.is_a(&target.object_type, &declared.target))
        {
            self.reference_targets.insert(target.clone());
            self.reference_targets.insert(model.canonical(target));
        }
        let mut mutations = writes
            .iter()
            .filter(|write| {
                model.canonical(&write.key.source) == canonical && write.key.link == link
            })
            .peekable();
        // A mapped clear or still-unresolved logical extension role may have
        // no physical mutation. It cannot confer link authority in this turn.
        if mutations.peek().is_none() {
            return Ok(());
        }
        let declaration = model.link(&source.object_type, link).ok_or_else(|| {
            ObservationRefusal::Encoding(format!(
                "link intent {source}.{link} has no declared role"
            ))
        })?;
        for write in mutations {
            let allowed = match &write.mutation {
                LinkMutation::Add { .. } => target.is_some_and(|target| {
                    model.canonical(target) == model.canonical(&write.key.target)
                }),
                LinkMutation::Remove => {
                    declaration.cardinality.to == norn_ir::Multiplicity::One
                        && target.is_none_or(|target| {
                            model.canonical(target) != model.canonical(&write.key.target)
                        })
                }
            };
            if !allowed {
                continue;
            }
            let mut sealed = write.clone();
            if let LinkMutation::Add { values } = &mut sealed.mutation {
                *values = catalog
                    .effective_link_values(&source.object_type, link, values)
                    .map_err(|error| ObservationRefusal::Encoding(error.to_string()))?;
            }
            if !self.links.contains(&sealed) {
                self.links.push(sealed);
            }
        }
        Ok(())
    }

    /// Seal pending-record changes from trusted mapping preparation. Retained
    /// entries may be copied; every added or removed intent must belong to an
    /// opaque prepared source scope. The original record is CAS-bound too.
    ///
    /// # Errors
    /// Invalid pending evidence or a change outside the prepared source scopes.
    pub fn seal_pending_link_changes(
        &mut self,
        before: &FactSlice,
        writes: &[FactWrite],
    ) -> Result<(), ObservationRefusal> {
        for write in writes
            .iter()
            .filter(|write| write.key.object_type.as_str() == "norn.PendingLinks")
        {
            let original = before.get(&write.key).ok_or_else(|| {
                ObservationRefusal::Encoding(
                    "pending record has no original CAS evidence".to_owned(),
                )
            })?;
            if original.version != write.expected {
                return Err(ObservationRefusal::Encoding(
                    "pending record CAS differs from its preparation".to_owned(),
                ));
            }
            let entries =
                |values: Option<&JsonObject>| -> Result<Vec<JsonValue>, ObservationRefusal> {
                    match values.and_then(|values| values.get("links")) {
                        Some(JsonValue::Array(links)) => Ok(links.clone()),
                        None => Ok(Vec::new()),
                        _ => Err(ObservationRefusal::Encoding(
                            "pending links must be an array".to_owned(),
                        )),
                    }
                };
            let old = entries(original.values.as_ref())?;
            let new = entries(match &write.mutation {
                FactMutation::Put { values } => Some(values),
                FactMutation::Delete => None,
            })?;
            for entry in old
                .iter()
                .filter(|entry| !new.contains(entry))
                .chain(new.iter().filter(|entry| !old.contains(entry)))
            {
                let field = |name| {
                    entry.get(name).and_then(JsonValue::as_str).ok_or_else(|| {
                        ObservationRefusal::Encoding(format!("pending intent is missing {name}"))
                    })
                };
                let source = FactKey::new(field("sourceType")?, field("sourceKey")?);
                let provenance: InstanceProvenance = entry
                    .get("provenance")
                    .ok_or_else(|| {
                        ObservationRefusal::Encoding("pending intent has no provenance".to_owned())
                    })
                    .and_then(|value| {
                        norn_ir::runtime::decode(value).map_err(ObservationRefusal::Encoding)
                    })?;
                let owner = ObservedSourceOwner::from_provenance(&provenance);
                if !self
                    .source_scopes
                    .iter()
                    .any(|(scope, scope_owner)| scope.key == source && *scope_owner == owner)
                {
                    return Err(ObservationRefusal::Encoding(
                        "pending intent changes another source's scope".to_owned(),
                    ));
                }
                self.pending_sources.insert(source);
            }
            self.pending_writes.push(write.clone());
        }
        Ok(())
    }

    pub(crate) fn pending_sources(&self) -> impl Iterator<Item = &FactKey> {
        self.pending_sources.iter()
    }

    pub(crate) fn source_scopes(&self) -> &[(FactExpectation, ObservedSourceOwner)] {
        &self.source_scopes
    }

    /// Whether this exact complete object write was prepared as an observation.
    #[must_use]
    pub(crate) fn authorizes_write(&self, write: &FactWrite) -> bool {
        self.writes
            .iter()
            .any(|authorized| authorized.write == *write)
    }

    pub(crate) fn findings_for_read(
        catalog: &TypeCatalog<'_>,
        authorized: &AuthorizedObservedReadFinding,
        failures: &[InvariantFailure],
    ) -> Result<Vec<DataQualityFinding>, ObservationRefusal> {
        findings_for(
            catalog,
            &authorized.key,
            &authorized.values,
            &authorized.provenance,
            &authorized.observation_id,
            failures,
        )
    }

    /// Seal bounded ledger event chunks for an atomic complete-source snapshot.
    /// Each prepared object still passes the ordinary observation envelope. The
    /// batch reserves one receipt, permits no effects, and binds its consumption
    /// and exact event count before and after host-appended quality findings.
    /// No writes or events become visible until the existing CAS commit succeeds.
    ///
    /// # Errors
    /// Refuses an oversized batch, missing observation delivery, or invalid authority.
    pub fn seal_snapshot_event_chunks<E, C, T>(
        &mut self,
        commit: &CellCommit<E, C, T>,
    ) -> Result<Vec<usize>, ObservationRefusal> {
        let per_item = usize::try_from(EnvelopeLimits::V1.max_events).unwrap_or(usize::MAX);
        let maximum = self
            .prepared_items
            .saturating_mul(per_item.saturating_sub(1))
            .saturating_add(1);
        let actual = commit.events.len().saturating_add(self.finding_count());
        if self.prepared_items > MAX_OBSERVATION_SNAPSHOT_ITEMS || actual > maximum {
            return Err(ObservationRefusal::EventLimit {
                limit: u32::try_from(maximum).unwrap_or(u32::MAX),
                actual,
            });
        }
        let Some(consumes @ Consumed::Observation { .. }) = commit.consumes.as_ref() else {
            return Err(ObservationRefusal::Encoding(
                "snapshot batch requires an observation delivery".to_owned(),
            ));
        };
        if !self.validates(commit) {
            return Err(ObservationRefusal::Encoding(
                "snapshot batch authority does not match its commit".to_owned(),
            ));
        }
        let counts = (0..actual)
            .step_by(per_item)
            .map(|start| actual.saturating_sub(start).min(per_item))
            .collect::<Vec<_>>();
        self.snapshot_chunks = Some(Box::new(SnapshotEventChunks {
            counts: counts.clone(),
            consumes: consumes.clone(),
            base_events: commit.events.len(),
        }));
        Ok(counts)
    }

    pub(crate) fn event_limit(&self) -> usize {
        self.snapshot_chunks.as_ref().map_or_else(
            || usize::try_from(EnvelopeLimits::V1.max_events).unwrap_or(usize::MAX),
            |chunks| chunks.counts.iter().sum(),
        )
    }

    /// Whether `commit` contains every exact prepared write and read-bound
    /// finding expectation. Effects remain forbidden. Every link mutation must
    /// match an exact sealed mapping or pending-resolution intent.
    #[must_use]
    pub fn validates<E, C, T>(&self, commit: &CellCommit<E, C, T>) -> bool {
        if !commit.effects.is_empty()
            || self.source_scopes.iter().any(|(scope, _)| {
                !commit.reads.contains(scope)
                    && !commit
                        .writes
                        .iter()
                        .any(|write| write.key == scope.key && write.expected == scope.expected)
            })
            || commit.links.iter().any(|write| !self.links.contains(write))
            || commit.writes.iter().any(|write| {
                write.key.object_type.as_str() == "norn.PendingLinks"
                    && !self.pending_writes.contains(write)
            })
        {
            return false;
        }
        if let Some(chunks) = &self.snapshot_chunks {
            let total: usize = chunks.counts.iter().sum();
            if commit.consumes.as_ref() != Some(&chunks.consumes)
                || (commit.events.len() != chunks.base_events && commit.events.len() != total)
                || chunks.base_events.saturating_add(self.finding_count()) != total
            {
                return false;
            }
        }
        if self.observed_reads.iter().any(|read| {
            !commit.reads.contains(read) || commit.writes.iter().any(|write| write.key == read.key)
        }) {
            return false;
        }
        for (index, authorized) in self.writes.iter().enumerate() {
            if self
                .writes
                .iter()
                .take(index)
                .any(|prior| prior.write == authorized.write)
                || commit
                    .writes
                    .iter()
                    .filter(|write| **write == authorized.write)
                    .count()
                    != 1
            {
                return false;
            }
        }
        for (index, authorized) in self.read_findings.iter().enumerate() {
            let expectation = FactExpectation {
                key: authorized.key.clone(),
                expected: authorized.expected,
            };
            if self
                .read_findings
                .iter()
                .take(index)
                .any(|prior| prior == authorized)
                || commit
                    .reads
                    .iter()
                    .filter(|read| **read == expectation)
                    .count()
                    != 1
                || commit
                    .writes
                    .iter()
                    .any(|write| write.key == authorized.key)
            {
                return false;
            }
        }
        true
    }

    /// The exact observed source writes and guarded unchanged sources may retain
    /// unresolved required links. Additional authored writes receive no exemption.
    pub(crate) fn observed_sources(&self) -> impl Iterator<Item = &FactKey> {
        self.writes
            .iter()
            .map(|authorized| &authorized.write.key)
            .chain(self.observed_reads.iter().map(|read| &read.key))
    }

    pub(crate) fn findings_for_write(
        &self,
        catalog: &TypeCatalog<'_>,
        write: &FactWrite,
        failures: &[InvariantFailure],
    ) -> Option<PreparedFindingComparison<'_>> {
        let authorized = self
            .writes
            .iter()
            .find(|authorized| authorized.write == *write)?;
        let FactMutation::Put { values } = &write.mutation else {
            return Some((
                Err(ObservationRefusal::Encoding(
                    "observed invariant evidence requires a complete value write".to_owned(),
                )),
                &authorized.findings,
            ));
        };
        Some((
            findings_for(
                catalog,
                &write.key,
                values,
                &authorized.provenance,
                &authorized.observation_id,
                failures,
            ),
            &authorized.findings,
        ))
    }

    /// Add this authority's provenance-bearing finding events to `commit`.
    pub fn append_findings<E, C, T>(&self, commit: &mut CellCommit<E, C, T>)
    where
        E: From<ObservationEvent>,
    {
        for authorized in &self.writes {
            commit.events.extend(
                authorized
                    .findings
                    .iter()
                    .cloned()
                    .map(|fact| ObservationEvent::DataQualityFinding { fact }.into()),
            );
        }
        for authorized in &self.read_findings {
            commit.events.extend(
                authorized
                    .findings
                    .iter()
                    .cloned()
                    .map(|fact| ObservationEvent::DataQualityFinding { fact }.into()),
            );
        }
    }

    pub(crate) fn has_findings(&self) -> bool {
        self.writes
            .iter()
            .any(|authorized| !authorized.findings.is_empty())
            || self
                .read_findings
                .iter()
                .any(|authorized| !authorized.findings.is_empty())
    }

    /// Number of data-quality events this authority will append after its
    /// final read/write bindings have been retained.
    #[must_use]
    pub fn finding_count(&self) -> usize {
        self.writes
            .iter()
            .map(|authorized| authorized.findings.len())
            .chain(
                self.read_findings
                    .iter()
                    .map(|authorized| authorized.findings.len()),
            )
            .sum()
    }
}

fn findings_for(
    catalog: &TypeCatalog<'_>,
    key: &FactKey,
    values: &JsonObject,
    provenance: &InstanceProvenance,
    observation_id: &str,
    failures: &[InvariantFailure],
) -> Result<Vec<DataQualityFinding>, ObservationRefusal> {
    if failures.is_empty() {
        return Ok(Vec::new());
    }
    let evidence_digest = norn_ir::canonical::canonical_json_of(values)
        .map(|bytes| {
            format!(
                "sha256:{}",
                norn_ir::canonical::HashProfile::Norn1.hash_canonical_bytes(&bytes)
            )
        })
        .map_err(|error| ObservationRefusal::Encoding(error.to_string()))?;
    failures
        .iter()
        .map(|failure| {
            let (rule_ref, outcome) = match &failure.finding_outcome {
                Some(outcome @ DataQualityOutcome::InvalidTemporalObservation { .. }) => (
                    DataQualityRuleRef::Builtin {
                        id: TEMPORAL_PROPERTY_RULE_ID.to_owned(),
                        version: "1".to_owned(),
                    },
                    outcome.clone(),
                ),
                Some(outcome) => {
                    return Err(ObservationRefusal::Encoding(format!(
                        "unsupported built-in invariant finding outcome: {outcome:?}"
                    )));
                }
                None => {
                    let version = catalog
                        .invariant_owner_version(&failure.owner)
                        .map_err(|error| ObservationRefusal::Encoding(error.to_string()))?
                        .ok_or_else(|| {
                            ObservationRefusal::Encoding(format!(
                                "invariant owner {:?} is absent from the catalog",
                                failure.owner
                            ))
                        })?;
                    (
                        DataQualityRuleRef::Invariant {
                            owner: failure.owner.clone(),
                            invariant_id: failure.id.clone(),
                            version,
                        },
                        failure.outcome.clone(),
                    )
                }
            };
            let severity = match failure.classification {
                ConstraintClass::Integrity => DataQualitySeverity::Error,
                ConstraintClass::Quality => DataQualitySeverity::Warning,
            };
            DataQualityFinding::v2(
                rule_ref,
                vec![DataQualitySubject::new(
                    key.object_type.clone(),
                    key.key.clone(),
                )],
                severity,
                failure.message.clone(),
                outcome,
                vec![DataQualityLineage::new(
                    provenance.clone(),
                    vec![observation_id.to_owned()],
                )],
                provenance.recorded_at.clone(),
                provenance.recorded_at.clone(),
                DataQualityFindingStatus::Open,
            )
            .and_then(|finding| finding.with_evidence_digest(evidence_digest.clone()))
            .map_err(|error| ObservationRefusal::Encoding(error.to_string()))
        })
        .collect()
}

/// Check a finding's value-bearing fields against a fresh bounded evaluation.
/// The caller must independently verify values, catalog identity and provenance.
///
/// # Errors
/// Bounded evaluation or canonical finding construction failed.
pub fn verify_finding_values(
    catalog: &TypeCatalog<'_>,
    key: &FactKey,
    values: &JsonObject,
    finding: &DataQualityFinding,
) -> Result<bool, ObservationRefusal> {
    let mut remaining = EvaluationBudget::default().units();
    let failures = evaluate_invariants(catalog, key, values, &mut remaining).map_err(|error| {
        ObservationRefusal::Encoding(format!(
            "historical invariant evaluation refused: {error:?}"
        ))
    })?;
    let provenance = finding.provenance.first().map_or_else(
        || InstanceProvenance {
            source: norn_ir::SourceRecordRef::new("norn.cell", "verified-history"),
            mapping_version: norn_ir::MappingVersion("verified-history/1".to_owned()),
            source_fields: Vec::new(),
            markings: Vec::new(),
            transform: None,
            observed_at: None,
            recorded_at: finding.first_seen.clone(),
            upstream: Vec::new(),
        },
        |lineage| lineage.provenance.clone(),
    );
    let expected = findings_for(
        catalog,
        key,
        values,
        &provenance,
        "verified-history",
        &failures,
    )?;
    Ok(expected.iter().any(|expected| {
        expected.rule_ref == finding.rule_ref
            && expected.subjects == finding.subjects
            && expected.evidence_digest == finding.evidence_digest
            && expected.message == finding.message
            && expected.outcome == finding.outcome
            && expected.severity == finding.severity
    }))
}

const TEMPORAL_PROPERTY_RULE_ID: &str = "norn.temporal.property_validity";
const LIFECYCLE_OUT_OF_BAND_RULE_ID: &str = "norn.lifecycle.observed_out_of_band";

/// A prepared observed snapshot, with its ordinary atomic write and a sealed
/// authorization scoped to the exact prepared writes.
#[derive(Debug, Clone, PartialEq)]
pub struct PreparedObservation {
    commit: CellCommit<ObservationEvent, JsonObject, JsonObject>,
    authorization: ObservationAuthorization,
}

impl PreparedObservation {
    /// Split the public commit from its opaque observation authorization.
    #[must_use]
    pub fn into_parts(
        mut self,
    ) -> (
        CellCommit<ObservationEvent, JsonObject, JsonObject>,
        ObservationAuthorization,
    ) {
        self.authorization.prepared_items = 1;
        (self.commit, self.authorization)
    }
}

impl Deref for PreparedObservation {
    type Target = CellCommit<ObservationEvent, JsonObject, JsonObject>;

    fn deref(&self) -> &Self::Target {
        &self.commit
    }
}

impl DerefMut for PreparedObservation {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.commit
    }
}

/// Admitted property and lifecycle observations for one object.
#[derive(Debug, Clone, Copy)]
pub struct ObservedSnapshot<'a> {
    /// The target object key.
    pub key: &'a FactKey,
    /// Ordinary properties, without the action input's objectId.
    pub values: &'a JsonObject,
    /// Desired lifecycle states and their per-rule provenance.
    pub lifecycles: &'a [ObservedLifecycleSnapshot],
    /// Source provenance for ordinary property observations.
    pub provenance: &'a InstanceProvenance,
}

/// Prepare property and lifecycle observations against the read slice.
///
/// `values` contains ordinary mapped properties, without `objectId`. Link
/// patches are a separate `MappingIR` output and are not accepted here. The
/// source/mapping ingress is trusted, while current state is CAS protected.
/// Re-observing unchanged valid properties and lifecycle states emits no
/// findings or writes; an unchanged invalid state emits a finding against the
/// current read version without bumping the object's version. Consumption of
/// a delivered observation still commits atomically.
///
/// # Errors
///
/// Returns an [`ObservationRefusal`] without modifying state.
#[allow(
    clippy::too_many_lines,
    reason = "property and lifecycle reconciliation share one final-state authorization"
)]
pub fn prepare_observed_snapshot(
    catalog: &TypeCatalog<'_>,
    slice: &FactSlice,
    turn: &DecisionTurn,
    snapshot: ObservedSnapshot<'_>,
) -> Result<PreparedObservation, ObservationRefusal> {
    let ObservedSnapshot {
        key,
        values,
        lifecycles,
        provenance,
    } = snapshot;
    let original = slice
        .get(key)
        .ok_or_else(|| ObservationRefusal::NotInSlice { key: key.clone() })?;
    let effective_original = original
        .values
        .as_ref()
        .map(|values| effective_observed_values(catalog, key, values))
        .transpose()?;
    let original_for_changes = FactState {
        version: original.version,
        values: effective_original.clone(),
    };
    for property in catalog.lifecycle_properties(&key.object_type) {
        if values.contains_key(property.as_str()) {
            return Err(invalid(
                &property,
                "ordinary observed properties may not contain a lifecycle state",
            ));
        }
    }
    for snapshot in lifecycles {
        if catalog
            .effective_type(&key.object_type)
            .and_then(|effective| effective.property(snapshot.property.as_str()))
            .is_some_and(|property| property.protected())
            && effective_original
                .as_ref()
                .and_then(|values| values.get(snapshot.property.as_str()))
                != Some(&JsonValue::String(snapshot.state.clone()))
        {
            return Err(invalid(
                &snapshot.property,
                "protected lifecycle properties are writable only by rules",
            ));
        }
    }
    let facts = property_changes(key, &original_for_changes, values);
    let ordinary = prepare_observed_properties::<Infallible>(catalog, slice, turn, &facts)
        .map_err(|error| ObservationRefusal::Properties(Box::new(error)))?;
    let mut current = effective_original.clone().unwrap_or_default();
    for write in &ordinary.writes {
        if let FactMutation::Put { values } = &write.mutation {
            current.clone_from(values);
        }
    }
    let mut events: Vec<_> = ordinary
        .events
        .into_iter()
        // `prepare_observed_properties` also emits the ordinary aggregate
        // modification event. This snapshot may add lifecycle changes below,
        // so retain only the final aggregate constructed after reconciliation.
        .filter(|event| !matches!(event, FactEvent::Modified { .. }))
        .map(|event| ObservationEvent::ObservedFact {
            event,
            provenance: provenance.clone(),
        })
        .collect();
    let mut seen = std::collections::BTreeSet::new();
    for snapshot in lifecycles {
        if snapshot.provenance.source != provenance.source
            || snapshot.provenance.mapping_version != provenance.mapping_version
            || snapshot.provenance.observed_at != provenance.observed_at
        {
            return Err(invalid(
                &snapshot.property,
                "lifecycle evidence must cite the same source snapshot",
            ));
        }
        if !seen.insert(snapshot.property.clone()) {
            return Err(invalid(
                &snapshot.property,
                "the snapshot repeats a lifecycle property",
            ));
        }
        reconcile(catalog, key, &mut current, snapshot, &turn.id, &mut events)?;
        check_observation_event_limit(events.len())?;
    }
    current = effective_observed_values(catalog, key, &current)?;
    if let Some(before) = effective_original
        && before != current
    {
        let after_version = original
            .version
            .checked_next()
            .ok_or(ObservationRefusal::Overflow)?;
        if let Some(event) = modified_event(
            key.clone(),
            original.version,
            after_version,
            before,
            current.clone(),
        ) {
            events.push(ObservationEvent::ObservedFact {
                event,
                provenance: provenance.clone(),
            });
        }
    }
    check_observation_event_limit(events.len())?;
    let failures = evaluate_observed_invariants(catalog, key, &current)?;
    let changed = original.values.as_ref() != Some(&current);
    let mut writes = Vec::new();
    let mut authorization = ObservationAuthorization::for_source(key, original.version, provenance);
    if changed {
        let write = FactWrite {
            key: key.clone(),
            expected: original.version,
            mutation: FactMutation::Put { values: current },
        };
        authorization.authorize(catalog, &write, provenance, &turn.id, &failures)?;
        writes.push(write);
    } else {
        authorization.observed_reads.push(FactExpectation {
            key: key.clone(),
            expected: original.version,
        });
        authorization.authorize_read_findings(
            catalog,
            &FactExpectation {
                key: key.clone(),
                expected: original.version,
            },
            &current,
            provenance,
            &turn.id,
            &failures,
        )?;
    }
    lift_tombstone(slice, key, provenance, &mut writes, &mut events);
    check_observation_event_limit(events.len().saturating_add(authorization.finding_count()))?;
    let reads = slice
        .iter()
        .filter(|(read, _)| !writes.iter().any(|write| write.key == **read))
        .map(|(key, state)| FactExpectation {
            key: key.clone(),
            expected: state.version,
        })
        .collect();
    Ok(PreparedObservation {
        commit: CellCommit {
            consumes: turn.consumes.clone(),
            reads,
            writes,
            events,
            effects: Vec::new(),
            links: Vec::new(),
            link_reads: Vec::new(),
        },
        authorization,
    })
}

fn effective_observed_values(
    catalog: &TypeCatalog<'_>,
    key: &FactKey,
    values: &JsonObject,
) -> Result<JsonObject, ObservationRefusal> {
    catalog
        .effective_object_values(&key.object_type, values)
        .map_err(|error| ObservationRefusal::EffectiveValues {
            key: key.clone(),
            reason: error.to_string(),
        })
}

fn evaluate_observed_invariants(
    catalog: &TypeCatalog<'_>,
    key: &FactKey,
    values: &JsonObject,
) -> Result<Vec<InvariantFailure>, ObservationRefusal> {
    let mut remaining = EvaluationBudget::default().units();
    evaluate_invariants(catalog, key, values, &mut remaining).map_err(|error| match error {
        InvariantEvaluationError::BudgetExceeded => ObservationRefusal::InvariantBudgetExceeded {
            key: key.clone(),
            limit: EvaluationBudget::default().units(),
        },
        InvariantEvaluationError::MissingInterface(interface) => {
            ObservationRefusal::InvariantModel {
                key: key.clone(),
                interface,
            }
        }
        InvariantEvaluationError::InvalidEffectiveValues(error) => {
            ObservationRefusal::EffectiveValues {
                key: key.clone(),
                reason: error.to_string(),
            }
        }
    })
}

/// Observed again under the same key: lift the tombstone (when the host read
/// it into the slice), as an observed recreate (#3013).
fn lift_tombstone(
    slice: &FactSlice,
    key: &FactKey,
    provenance: &InstanceProvenance,
    writes: &mut Vec<FactWrite>,
    events: &mut Vec<ObservationEvent>,
) {
    let tombstone = tombstone_key(key);
    if let Some(state) = slice.get(&tombstone).filter(|state| state.exists()) {
        writes.push(FactWrite {
            key: tombstone,
            expected: state.version,
            mutation: FactMutation::Delete,
        });
        events.push(ObservationEvent::ObservedRecreated {
            key: key.clone(),
            provenance: provenance.clone(),
        });
    }
}

/// An admitted source delete of one object (#3013).
#[derive(Debug, Clone, Copy)]
pub struct ObservedDelete<'a> {
    /// The deleted object.
    pub key: &'a FactKey,
    /// The mapping's `onDelete` terminal states, one per lifecycle that
    /// declares one.
    pub lifecycles: &'a [ObservedLifecycleSnapshot],
    /// The source delete.
    pub provenance: &'a InstanceProvenance,
}

/// Prepare a source delete against the read slice, which must hold the
/// object and its [`tombstone_key`] (#3013, lead decision 2026-10-02).
///
/// Nothing is ever erased:
///
/// - an object that does not exist, or is already tombstoned, changes
///   nothing (the delivery is still consumed by the host commit);
/// - when every `onDelete` state is reachable by a unique shortest declared
///   path, the object takes those paths as observed transitions
///   (`snapshot_catch_up`, guards skipped; none when it already sits in
///   them), one aggregate [`FactEvent::Modified`] is emitted when those
///   transitions changed its stored values, then [`ObservationEvent::ObservedDeleted`] with
///   `tombstoned: false`;
/// - otherwise the object is tombstoned: a [`OBSERVED_TOMBSTONE_TYPE`]
///   object records when and where it was deleted, and
///   [`ObservationEvent::ObservedDeleted`] has `tombstoned: true`. The
///   object keeps its values; hosts hide tombstoned objects from default
///   reads.
///
/// # Errors
///
/// Returns an [`ObservationRefusal`] without modifying state.
pub fn prepare_observed_delete(
    catalog: &TypeCatalog<'_>,
    slice: &FactSlice,
    turn: &DecisionTurn,
    delete: ObservedDelete<'_>,
) -> Result<PreparedObservation, ObservationRefusal> {
    let ObservedDelete {
        key,
        lifecycles,
        provenance,
    } = delete;
    let original = slice
        .get(key)
        .ok_or_else(|| ObservationRefusal::NotInSlice { key: key.clone() })?;
    let tombstone = tombstone_key(key);
    let buried = slice
        .get(&tombstone)
        .ok_or_else(|| ObservationRefusal::NotInSlice {
            key: tombstone.clone(),
        })?;
    let unchanged = || -> Result<PreparedObservation, ObservationRefusal> {
        let authorization = ObservationAuthorization::for_source(key, original.version, provenance);
        Ok(PreparedObservation {
            commit: CellCommit {
                consumes: turn.consumes.clone(),
                reads: expectations(slice, &[]),
                ..CellCommit::empty()
            },
            authorization,
        })
    };
    let Some(values) = original.values.as_ref() else {
        return unchanged();
    };
    if buried.exists() {
        return unchanged();
    }
    let before = effective_observed_values(catalog, key, values)?;
    let mut current = before.clone();
    let reached = reach_on_delete(catalog, key, &mut current, lifecycles)?;
    let reachable = reached.is_some();
    let mut events = reached.unwrap_or_default();
    let moved = !events.is_empty();
    let writes = if reachable {
        if moved {
            append_observed_modification(
                &mut events,
                key,
                original.version,
                before,
                &current,
                provenance,
            )?;
        }
        // The source delete is recorded even when the object already sits
        // in every onDelete state (cancelled by a command, say): its
        // provenance is kept and `deleted` rules fire. The host's inbox
        // ingests each delete event once.
        events.push(ObservationEvent::ObservedDeleted {
            key: key.clone(),
            tombstoned: false,
            provenance: provenance.clone(),
        });
        if moved {
            vec![FactWrite {
                key: key.clone(),
                expected: original.version,
                mutation: FactMutation::Put { values: current },
            }]
        } else {
            Vec::new()
        }
    } else {
        events.clear();
        events.push(ObservationEvent::ObservedDeleted {
            key: key.clone(),
            tombstoned: true,
            provenance: provenance.clone(),
        });
        vec![FactWrite {
            key: tombstone,
            expected: buried.version,
            mutation: FactMutation::Put {
                values: tombstone_values(key, provenance)?,
            },
        }]
    };
    let mut authorization = ObservationAuthorization::for_source(key, original.version, provenance);
    if moved && let Some(write) = writes.iter().find(|write| write.key == *key) {
        let FactMutation::Put { values } = &write.mutation else {
            unreachable!("observed lifecycle movement writes a complete value")
        };
        let failures = evaluate_observed_invariants(catalog, key, values)?;
        authorization.authorize(catalog, write, provenance, &turn.id, &failures)?;
    }
    check_observation_event_limit(events.len().saturating_add(authorization.finding_count()))?;
    Ok(PreparedObservation {
        commit: CellCommit {
            consumes: turn.consumes.clone(),
            reads: expectations(slice, &writes),
            writes,
            events,
            effects: Vec::new(),
            links: Vec::new(),
            link_reads: Vec::new(),
        },
        authorization,
    })
}

fn append_observed_modification(
    events: &mut Vec<ObservationEvent>,
    key: &FactKey,
    before_version: FactVersion,
    before: JsonObject,
    after: &JsonObject,
    provenance: &norn_ir::provenance::InstanceProvenance,
) -> Result<(), ObservationRefusal> {
    let after_version = before_version
        .checked_next()
        .ok_or(ObservationRefusal::Overflow)?;
    if let Some(event) = modified_event(
        key.clone(),
        before_version,
        after_version,
        before,
        after.clone(),
    ) {
        events.push(ObservationEvent::ObservedFact {
            event,
            provenance: provenance.clone(),
        });
    }
    Ok(())
}

/// The observed transitions that take `current` to every `onDelete` state,
/// or `None` when there are none to reach or one has no unique shortest
/// declared path (then the delete tombstones).
fn reach_on_delete(
    catalog: &TypeCatalog<'_>,
    key: &FactKey,
    current: &mut JsonObject,
    lifecycles: &[ObservedLifecycleSnapshot],
) -> Result<Option<Vec<ObservationEvent>>, ObservationRefusal> {
    if lifecycles.is_empty() {
        return Ok(None);
    }
    let mut events = Vec::new();
    for snapshot in lifecycles {
        let property = &snapshot.property;
        let lifecycle = catalog
            .lifecycle(&key.object_type, property)
            .ok_or_else(|| invalid(property, "the effective lifecycle does not resolve"))?;
        let Some(JsonValue::String(from)) = current.get(property.as_str()).cloned() else {
            return Err(invalid(
                property,
                "the current lifecycle state is absent or not text",
            ));
        };
        if from == snapshot.state {
            continue;
        }
        let Ok(path) = shortest_path(&lifecycle, &from, &snapshot.state) else {
            return Ok(None);
        };
        let mut left = from;
        for transition in path {
            events.push(ObservationEvent::ObservedTransition {
                key: key.clone(),
                property: property.clone(),
                transition: transition.id.clone(),
                from: std::mem::replace(&mut left, transition.to.clone()),
                to: transition.to.clone(),
                synthesized: ObservationSynthesis::SnapshotCatchUp,
                provenance: snapshot.provenance.clone(),
            });
        }
        current.insert(
            property.as_str().to_owned(),
            JsonValue::String(snapshot.state.clone()),
        );
    }
    Ok(Some(events))
}

/// Compare-and-swap expectations on every slice object the writes leave.
fn expectations(slice: &FactSlice, writes: &[FactWrite]) -> Vec<FactExpectation> {
    slice
        .iter()
        .filter(|(read, _)| !writes.iter().any(|write| write.key == **read))
        .map(|(key, state)| FactExpectation {
            key: key.clone(),
            expected: state.version,
        })
        .collect()
}

/// The tombstone object's values.
fn tombstone_values(
    key: &FactKey,
    provenance: &InstanceProvenance,
) -> Result<JsonObject, ObservationRefusal> {
    let at = provenance
        .observed_at
        .as_ref()
        .unwrap_or(&provenance.recorded_at)
        .as_str()
        .to_owned();
    let encoded = norn_ir::runtime::encode(provenance).map_err(ObservationRefusal::Encoding)?;
    Ok(JsonObject::from([
        (
            "objectType".to_owned(),
            JsonValue::String(key.object_type.as_str().to_owned()),
        ),
        ("key".to_owned(), JsonValue::String(key.key.clone())),
        ("at".to_owned(), JsonValue::String(at)),
        (
            "source".to_owned(),
            JsonValue::String(provenance.source.stable_id()),
        ),
        (
            "mappingVersion".to_owned(),
            JsonValue::String(provenance.mapping_version.as_str().to_owned()),
        ),
        ("provenance".to_owned(), encoded),
    ]))
}

fn property_changes(key: &FactKey, original: &FactState, values: &JsonObject) -> Vec<FactChange> {
    let Some(existing) = &original.values else {
        return vec![FactChange::Create {
            object_type: key.object_type.clone(),
            key: key.key.clone(),
            values: values.clone(),
        }];
    };
    values
        .iter()
        .filter(|(property, value)| existing.get(*property) != Some(*value))
        .map(|(property, value)| FactChange::Set {
            object_type: key.object_type.clone(),
            key: key.key.clone(),
            property: PropertyId(property.clone()),
            value: value.clone(),
        })
        .collect()
}

fn reconcile(
    catalog: &TypeCatalog<'_>,
    key: &FactKey,
    values: &mut JsonObject,
    snapshot: &ObservedLifecycleSnapshot,
    observation_id: &str,
    events: &mut Vec<ObservationEvent>,
) -> Result<(), ObservationRefusal> {
    let property = &snapshot.property;
    let lifecycle = catalog
        .lifecycle(&key.object_type, property)
        .ok_or_else(|| invalid(property, "the effective lifecycle does not resolve"))?;
    if !known_state(&lifecycle, &snapshot.state) {
        return Err(invalid(
            property,
            "the snapshot state is outside the effective lifecycle",
        ));
    }
    let Some(JsonValue::String(from)) = values.get(property.as_str()) else {
        return Err(invalid(
            property,
            "the current lifecycle state is absent or not text",
        ));
    };
    if from == &snapshot.state {
        return Ok(());
    }
    let from = from.clone();
    match shortest_path(&lifecycle, &from, &snapshot.state) {
        Ok(path) => {
            let mut left = from;
            for transition in path {
                events.push(ObservationEvent::ObservedTransition {
                    key: key.clone(),
                    property: property.clone(),
                    transition: transition.id.clone(),
                    from: std::mem::replace(&mut left, transition.to.clone()),
                    to: transition.to.clone(),
                    synthesized: ObservationSynthesis::SnapshotCatchUp,
                    provenance: snapshot.provenance.clone(),
                });
            }
        }
        Err(reason) => events.push(ObservationEvent::DataQualityFinding {
            fact: DataQualityFinding::v2(
                DataQualityRuleRef::Builtin {
                    id: LIFECYCLE_OUT_OF_BAND_RULE_ID.to_owned(),
                    version: "1".to_owned(),
                },
                vec![DataQualitySubject::new(
                    key.object_type.clone(),
                    key.key.clone(),
                )],
                DataQualitySeverity::Warning,
                format!(
                    "Observed state {:?} for lifecycle property {:?} has no unique declared path from {:?}.",
                    snapshot.state, property, from
                ),
                DataQualityOutcome::ObservedOutOfBand {
                    property: property.clone(),
                    from: Some(from),
                    state: snapshot.state.clone(),
                    reason,
                },
                vec![DataQualityLineage::new(
                    snapshot.provenance.clone(),
                    vec![observation_id.to_owned()],
                )],
                snapshot.provenance.recorded_at.clone(),
                snapshot.provenance.recorded_at.clone(),
                DataQualityFindingStatus::Open,
            )
            .map_err(|error| ObservationRefusal::Encoding(error.to_string()))?,
        }),
    }
    // Projection of the observed transition and any deviation finding, never
    // a direct lifecycle Set. The write and evidence commit together.
    values.insert(
        property.as_str().to_owned(),
        JsonValue::String(snapshot.state.clone()),
    );
    Ok(())
}

fn known_state(lifecycle: &Lifecycle, state: &str) -> bool {
    lifecycle
        .initial
        .iter()
        .chain(&lifecycle.terminal)
        .any(|s| s == state)
        || lifecycle
            .transitions
            .iter()
            .any(|t| t.to == state || t.from.iter().any(|s| s == state))
}

struct Route<'a> {
    distance: usize,
    ways: u8,
    path: Vec<&'a Transition>,
}

// Breadth-first distance with path counts capped at two. Queue order ensures
// every predecessor layer contributes before the next layer is visited;
// distinct transition IDs count as distinct paths, including parallel edges.
fn shortest_path<'a>(
    lifecycle: &'a Lifecycle,
    from: &str,
    to: &str,
) -> Result<Vec<&'a Transition>, SnapshotDeviationReason> {
    let mut routes = BTreeMap::from([(
        from.to_owned(),
        Route {
            distance: 0,
            ways: 1,
            path: Vec::new(),
        },
    )]);
    let mut queue = VecDeque::from([from.to_owned()]);
    while let Some(state) = queue.pop_front() {
        let Some(route) = routes.get(&state) else {
            continue;
        };
        let (distance, ways, path) = (route.distance, route.ways, route.path.clone());
        for transition in lifecycle
            .transitions
            .iter()
            .filter(|t| t.from.contains(&state))
        {
            let next_distance = distance.saturating_add(1);
            if let Some(next) = routes.get_mut(&transition.to) {
                if next.distance == next_distance {
                    next.ways = next.ways.saturating_add(ways).min(2);
                }
            } else {
                let mut next_path = path.clone();
                next_path.push(transition);
                routes.insert(
                    transition.to.clone(),
                    Route {
                        distance: next_distance,
                        ways,
                        path: next_path,
                    },
                );
                queue.push_back(transition.to.clone());
            }
        }
    }
    match routes.remove(to) {
        Some(route) if route.ways == 1 => Ok(route.path),
        Some(_) => Err(SnapshotDeviationReason::AmbiguousShortestPath),
        None => Err(SnapshotDeviationReason::NoDeclaredPath),
    }
}

fn invalid(property: &PropertyId, reason: &str) -> ObservationRefusal {
    ObservationRefusal::InvalidLifecycle {
        property: property.as_str().to_owned(),
        reason: reason.to_owned(),
    }
}

/// Check an observation's final event count, including its host-added
/// `observation.recorded` wrapper.
///
/// `events` includes prepared mapped events and any authorized findings, but
/// excludes the wrapper that the observation host prepends before commit.
///
/// # Errors
///
/// Returns [`ObservationRefusal::EventLimit`] when the full observation
/// exceeds the V1 event cap.
pub fn check_observation_event_limit(events: usize) -> Result<(), ObservationRefusal> {
    // The host wraps every mapped source event with its `observation.recorded`
    // receipt before committing the prepared events.
    let actual = events.saturating_add(1);
    check_decision_envelope(&EnvelopeLimits::V1, EnvelopeCounts::new(actual, 0, 0)).map_err(
        |exceeded| ObservationRefusal::EventLimit {
            limit: exceeded.limit,
            actual,
        },
    )
}

/// Failure to prepare or atomically commit an observed snapshot.
#[derive(Debug, thiserror::Error)]
pub enum ObservationCommitRefusal<X> {
    /// Invalid observation; no host commit occurred.
    #[error("{0}")]
    Observation(#[source] ObservationRefusal),
    /// Atomic commit/CAS or host failure; nothing applied.
    #[error("{0}")]
    Commit(#[source] CommitError<X>),
}

/// Prepare and commit through the existing `CellHost` atomic/CAS boundary.
///
/// The host supplies admitted source/mapping inputs and the current slice.
/// Commands and timers are never emitted. Observation-delivery consumption
/// is included even when its lifecycle reconciliation is a no-op.
///
/// # Errors
///
/// Returns the preparation or atomic commit refusal.
pub fn commit_observed_snapshot<H: CellHost + ?Sized>(
    host: &mut H,
    catalog: &TypeCatalog<'_>,
    slice: &FactSlice,
    turn: &DecisionTurn,
    snapshot: ObservedSnapshot<'_>,
) -> Result<CommitReceipt, ObservationCommitRefusal<H::Error>>
where
    H::Event: From<ObservationEvent>,
    H::Command: From<JsonObject>,
    H::Input: From<JsonObject> + Serialize,
{
    let prepared = prepare_observed_snapshot(catalog, slice, turn, snapshot)
        .map_err(ObservationCommitRefusal::Observation)?;
    let (prepared, authorization) = prepared.into_parts();
    host.commit_observed(
        CellCommit {
            consumes: prepared.consumes,
            reads: prepared.reads,
            writes: prepared.writes,
            events: prepared.events.into_iter().map(Into::into).collect(),
            effects: Vec::new(),
            links: prepared.links,
            link_reads: Vec::new(),
        },
        authorization,
    )
    .map_err(ObservationCommitRefusal::Commit)
}
