//! Versioned, provenance-bearing data quality findings.
//!
//! A finding records a constraint or expectation that observed or projected
//! data did not meet. It identifies the rule and its version, affected objects, typed
//! evidence, source lineage, and its stable identity and lifecycle state.

use schemars::{JsonSchema, Schema, schema_for};
use serde::{Deserialize, Serialize};

use crate::canonical::{CanonicalJsonError, domain_hash_hex};
use crate::mapping::SnapshotDeviationReason;
use crate::provenance::{InstanceProvenance, MappingVersion, SourceRecordRef, UtcTimestamp};
use crate::{InterfaceName, InvariantId, ObjectTypeName, PropertyId};

/// The wire format version of [`DataQualityFinding`].
pub const DATA_QUALITY_FINDING_VERSION: u16 = 2;

const DATA_QUALITY_FINDING_DEDUPE_DOMAIN: &[u8] = b"norn.data_quality_finding.dedupe.v2\0";

/// Which declaration owns an invariant.
#[derive(
    Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
#[serde(tag = "kind", content = "name", rename_all = "snake_case")]
pub enum InvariantOwner {
    /// An object type or one of its supertypes.
    Object(ObjectTypeName),
    /// An interface implemented by the object type or one of its supertypes.
    Interface(InterfaceName),
}

/// A stable reference to the constraint or rule that produced a finding.
#[derive(
    Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
#[serde(
    tag = "kind",
    rename_all = "snake_case",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum DataQualityRuleRef {
    /// An EntityIR invariant pinned by artifact version or declaration digest.
    Invariant {
        /// The object type or interface that declares this invariant.
        owner: InvariantOwner,
        /// The invariant identity within its owner.
        invariant_id: InvariantId,
        /// The pinned declaration version or canonical content digest.
        version: String,
    },
    /// A built-in rule, whose version identifies its predicate semantics.
    Builtin {
        /// The globally qualified built-in rule id.
        id: String,
        /// The built-in rule semantics version.
        version: String,
    },
}

/// An object affected by a finding.
#[derive(
    Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DataQualitySubject {
    /// The concrete object type.
    pub object_type: ObjectTypeName,
    /// The object's identity key.
    pub key: String,
}

impl DataQualitySubject {
    /// The object identified by `object_type` and `key`.
    #[must_use]
    pub fn new(object_type: ObjectTypeName, key: impl Into<String>) -> Self {
        Self {
            object_type,
            key: key.into(),
        }
    }
}

/// One source and observation lineage contributing evidence to a finding.
#[derive(
    Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DataQualityLineage {
    /// The provenance-bearing source fact.
    pub provenance: InstanceProvenance,
    /// Stable ids of observations that contributed to this evidence.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub observation_ids: Vec<String>,
}

impl DataQualityLineage {
    /// One provenance record plus the observations that contributed it.
    #[must_use]
    pub fn new(provenance: InstanceProvenance, mut observation_ids: Vec<String>) -> Self {
        observation_ids.sort();
        observation_ids.dedup();
        Self {
            provenance,
            observation_ids,
        }
    }
}

/// The result of evaluating a rule or constraint against data.
#[derive(
    Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
#[serde(
    tag = "kind",
    rename_all = "snake_case",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum DataQualityOutcome {
    /// An invariant expression evaluated to `false`.
    NotSatisfied,
    /// The expression produced a non-boolean value.
    NonBoolean {
        /// The expression value's stable debug form.
        actual: String,
    },
    /// The expression could not be evaluated under the admitted runtime.
    EvaluationFailed {
        /// The bounded evaluator's explanation.
        reason: String,
    },
    /// An observed lifecycle state was outside the declared transition graph.
    ObservedOutOfBand {
        /// The lifecycle property.
        property: PropertyId,
        /// The current lifecycle state, or `None` when starting from initial.
        from: Option<String>,
        /// The state reported by the source.
        state: String,
        /// Why no unique shortest path reached the observed state.
        reason: SnapshotDeviationReason,
    },
    /// An external observation contained a value that violates temporal rules.
    InvalidTemporalObservation {
        /// The temporal property.
        property: PropertyId,
        /// The exact invalid value reported by the source.
        value: String,
        /// The stable temporal validation reason.
        reason: String,
    },
    /// A row was excluded from the trusted identity projection.
    IdentityProjectionIsolation {
        /// Stable code for the identity projection defect.
        code: String,
        /// Concise reason that omits property values.
        detail: String,
    },
}

/// How serious a data quality finding is.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum DataQualitySeverity {
    /// Informational evidence that needs no immediate action.
    Info,
    /// An expectation failed and should be reviewed.
    Warning,
    /// The data blocks a normal workflow or projection.
    Error,
    /// The data presents an urgent integrity or authorization concern.
    Critical,
}

/// The lifecycle state of a data quality finding.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum DataQualityFindingStatus {
    /// The finding has not been reviewed or resolved.
    Open,
    /// A person or process accepted responsibility for the finding.
    Acknowledged,
    /// The latest data no longer exhibits the finding.
    Resolved,
    /// A newer finding or rule version replaced this finding.
    Superseded,
}

/// The identity fields used to compute a replay-stable finding key.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct FindingDedupeIdentity {
    schema_version: u16,
    rule_ref: DataQualityRuleRef,
    subjects: Vec<DataQualitySubject>,
    evidence_digest: Option<String>,
    outcome: DataQualityOutcome,
    provenance: Vec<FindingLineageIdentity>,
}

/// Stable lineage evidence included in a finding's dedupe key.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "camelCase")]
struct FindingLineageIdentity {
    source: SourceRecordRef,
    mapping_version: MappingVersion,
    transform: Option<String>,
}

/// One persisted, versioned finding about observed or projected data.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DataQualityFinding {
    /// The finding schema version ([`DATA_QUALITY_FINDING_VERSION`]).
    pub schema_version: u16,
    /// Stable id derived from the dedupe key.
    pub id: String,
    /// Replay-stable identity of the rule failure and its evidence.
    pub dedupe_key: String,
    /// Optional digest of candidate values or other stable evidence context.
    /// Including this distinguishes changed invalid values for the same rule
    /// and subject while leaving volatile timestamps out of the key.
    #[serde(
        default,
        deserialize_with = "crate::members::present_non_null",
        skip_serializing_if = "Option::is_none"
    )]
    #[schemars(with = "String")]
    pub evidence_digest: Option<String>,
    /// The declared constraint or built-in rule, including its version.
    pub rule_ref: DataQualityRuleRef,
    /// Objects affected by the finding, sorted by type and key.
    pub subjects: Vec<DataQualitySubject>,
    /// The finding's severity.
    pub severity: DataQualitySeverity,
    /// Human-readable summary of the finding.
    pub message: String,
    /// Typed result and evidence for the failed expectation.
    pub outcome: DataQualityOutcome,
    /// Source records and observations that contributed to the finding.
    pub provenance: Vec<DataQualityLineage>,
    /// When this finding was first observed.
    pub first_seen: UtcTimestamp,
    /// When this finding was most recently observed.
    pub last_seen: UtcTimestamp,
    /// Current state of the finding.
    pub status: DataQualityFindingStatus,
}

impl DataQualityFinding {
    /// Build a version-2 finding with a deterministic id and dedupe key.
    ///
    /// Subjects, provenance, and observation ids are sorted before storage.
    /// The dedupe key includes the rule version, subjects, typed outcome,
    /// optional evidence digest, and source/mapping identity; it excludes
    /// observation ids, status, and timestamps so replay can extend lineage.
    ///
    /// # Errors
    ///
    /// Returns [`CanonicalJsonError`] if the canonical identity cannot be
    /// serialized.
    #[allow(
        clippy::too_many_arguments,
        reason = "explicit fields keep the versioned finding contract visible"
    )]
    pub fn v2(
        rule_ref: DataQualityRuleRef,
        mut subjects: Vec<DataQualitySubject>,
        severity: DataQualitySeverity,
        message: impl Into<String>,
        outcome: DataQualityOutcome,
        mut provenance: Vec<DataQualityLineage>,
        first_seen: UtcTimestamp,
        last_seen: UtcTimestamp,
        status: DataQualityFindingStatus,
    ) -> Result<Self, CanonicalJsonError> {
        subjects.sort();
        subjects.dedup();
        for lineage in &mut provenance {
            lineage.observation_ids.sort();
            lineage.observation_ids.dedup();
        }
        provenance.sort();

        let mut finding = Self {
            schema_version: DATA_QUALITY_FINDING_VERSION,
            id: String::new(),
            dedupe_key: String::new(),
            evidence_digest: None,
            rule_ref,
            subjects,
            severity,
            message: message.into(),
            outcome,
            provenance,
            first_seen,
            last_seen,
            status,
        };
        finding.refresh_identity()?;
        Ok(finding)
    }

    /// Add a stable digest of the evidence values and recompute the finding
    /// identity. Use this when the typed outcome alone does not distinguish
    /// changed bad values.
    ///
    /// # Errors
    ///
    /// Returns [`CanonicalJsonError`] if the canonical identity cannot be
    /// serialized.
    pub fn with_evidence_digest(
        mut self,
        evidence_digest: impl Into<String>,
    ) -> Result<Self, CanonicalJsonError> {
        self.evidence_digest = Some(evidence_digest.into());
        self.refresh_identity()?;
        Ok(self)
    }

    /// Return the stable id assigned by [`Self::v2`].
    #[must_use]
    pub fn stable_id(&self) -> &str {
        &self.id
    }

    /// Return the replay-stable key assigned by [`Self::v2`].
    #[must_use]
    pub fn dedupe_key(&self) -> &str {
        &self.dedupe_key
    }

    fn dedupe_digest(&self) -> Result<String, CanonicalJsonError> {
        let mut subjects = self.subjects.clone();
        subjects.sort();
        subjects.dedup();

        let mut provenance: Vec<FindingLineageIdentity> = self
            .provenance
            .iter()
            .map(|lineage| FindingLineageIdentity {
                source: lineage.provenance.source.clone(),
                mapping_version: lineage.provenance.mapping_version.clone(),
                transform: lineage.provenance.transform.clone(),
            })
            .collect();
        provenance.sort();
        provenance.dedup();

        let identity = FindingDedupeIdentity {
            schema_version: self.schema_version,
            rule_ref: self.rule_ref.clone(),
            subjects,
            evidence_digest: self.evidence_digest.clone(),
            outcome: self.outcome.clone(),
            provenance,
        };
        domain_hash_hex(DATA_QUALITY_FINDING_DEDUPE_DOMAIN, &identity)
    }

    fn refresh_identity(&mut self) -> Result<(), CanonicalJsonError> {
        let digest = self.dedupe_digest()?;
        self.dedupe_key = format!("sha256:{digest}");
        self.id = format!("data_quality_finding:{digest}");
        Ok(())
    }
}

/// Emit the JSON Schema for [`DataQualityFinding`].
#[must_use]
pub fn data_quality_finding_schema() -> Schema {
    schema_for!(DataQualityFinding)
}

/// Emit the JSON Schema for [`DataQualityLineage`].
#[must_use]
pub fn data_quality_lineage_schema() -> Schema {
    schema_for!(DataQualityLineage)
}

/// Emit the JSON Schema for [`DataQualitySubject`].
#[must_use]
pub fn data_quality_subject_schema() -> Schema {
    schema_for!(DataQualitySubject)
}
