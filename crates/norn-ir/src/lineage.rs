//! The lineage input contract (T12-VIEW-2, #2718; ruling 2026-09-25 §18:
//! "Lineage is therefore a traversal over ordinary relations, not a separate
//! lineage store").
//!
//! A [`LineageInput`] is what a lineage traversal reads, assembled from data
//! the platform already keeps; nothing here is a second store:
//!
//! - [`LineageCommit`]: one cell ledger commit (the receipt): the delivery it
//!   consumed ([`LineageTrigger`]), the decision that produced it
//!   ([`DecisionRef`]), the fact versions it read (its compare-and-swap
//!   expectations) and wrote, and its ledger events.
//! - [`LineageObservation`]: one ingested observation (the cell inbox) with its
//!   [`InstanceProvenance`].
//! - [`ProvenancedInstance`]: provenance records stored with a fact.
//! - [`LineageObject`]: ontology objects and their links, technical-view ones
//!   (`technical.Mapping`, `technical.Column`, ...) included.
//!
//! The traversal and the business/technical projections live in
//! `norn-projections` (`lineage`); this module is data only.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::members::present_non_null;
use crate::rule::JsonObject;
use crate::{
    FactRef, InstanceProvenance, LinkId, ObjectTypeName, ProvenancedInstance, UtcTimestamp,
};

/// One outgoing link of a [`LineageObject`].
#[derive(
    Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LineageLink {
    /// The link id on the source object's type (`derived_from`, `table`).
    pub link: LinkId,
    /// The target object's type.
    pub target_type: ObjectTypeName,
    /// The target object's key.
    pub target_key: String,
}

/// One ontology object, with its outgoing links, as lineage reads it.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LineageObject {
    /// The object type.
    pub object_type: ObjectTypeName,
    /// The object's key.
    pub key: String,
    /// Its outgoing links. Omitted when empty.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub links: Vec<LineageLink>,
}

impl LineageObject {
    /// The stable lineage id: `object:{objectType}/{key}`.
    #[must_use]
    pub fn stable_id(&self) -> String {
        object_stable_id(&self.object_type, &self.key)
    }
}

/// The stable lineage id of object `key` of type `object_type`:
/// `object:{objectType}/{key}`.
#[must_use]
pub fn object_stable_id(object_type: &ObjectTypeName, key: &str) -> String {
    format!("object:{}/{key}", object_type.as_str())
}

/// The delivery a ledger commit consumed (the cell's consumption reference).
#[derive(
    Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
#[serde(
    tag = "kind",
    rename_all = "snake_case",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum LineageTrigger {
    /// An ingested observation.
    Observation {
        /// The observation id.
        id: String,
    },
    /// A due timer, scheduled by the commit at `incarnation` in the same
    /// world.
    Timer {
        /// The timer id.
        timer_id: String,
        /// The ledger position of the commit that scheduled it.
        incarnation: u64,
    },
    /// The result of a performed command.
    Result {
        /// The command id.
        command_id: String,
        /// The generation it was performed under.
        generation: u64,
    },
}

/// The decision graph (or action) that produced a commit.
#[derive(
    Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DecisionRef {
    /// The decision's artifact id (`credit.hold_policy`).
    pub artifact_id: String,
    /// The artifact version that decided.
    pub version: String,
}

impl DecisionRef {
    /// The stable lineage id: `decision:{artifactId}@{version}`.
    #[must_use]
    pub fn stable_id(&self) -> String {
        format!("decision:{}@{}", self.artifact_id, self.version)
    }
}

/// One cell ledger commit, as lineage reads it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LineageCommit {
    /// The world committed to (`main`).
    pub world: String,
    /// The commit's ledger position (1-based within the world's cell).
    pub seq: u64,
    /// When the commit was recorded (the turn's logical time as UTC).
    pub recorded_at: UtcTimestamp,
    /// The delivery the commit consumed. Omitted for a direct command.
    #[serde(
        default,
        deserialize_with = "present_non_null",
        skip_serializing_if = "Option::is_none"
    )]
    #[schemars(with = "LineageTrigger")]
    pub consumed: Option<LineageTrigger>,
    /// The decision that produced the commit, when one did.
    #[serde(
        default,
        deserialize_with = "present_non_null",
        skip_serializing_if = "Option::is_none"
    )]
    #[schemars(with = "DecisionRef")]
    pub decision: Option<DecisionRef>,
    /// The fact versions the commit read and was guarded on. Omitted when
    /// empty.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub reads: Vec<FactRef>,
    /// The fact versions the commit wrote. Omitted when empty.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub written: Vec<FactRef>,
    /// The commit's ledger events, as recorded. Omitted when empty.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub events: Vec<JsonObject>,
}

impl LineageCommit {
    /// The stable lineage id: `commit:{world}/{seq}`.
    #[must_use]
    pub fn stable_id(&self) -> String {
        commit_stable_id(&self.world, self.seq)
    }
}

/// The stable lineage id of commit `seq` of `world`: `commit:{world}/{seq}`.
#[must_use]
pub fn commit_stable_id(world: &str, seq: u64) -> String {
    format!("commit:{world}/{seq}")
}

/// One ingested observation, as lineage reads it.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LineageObservation {
    /// The observation id.
    pub id: String,
    /// Where it came from.
    pub provenance: InstanceProvenance,
}

impl LineageObservation {
    /// The stable lineage id: `observation:{id}`.
    #[must_use]
    pub fn stable_id(&self) -> String {
        format!("observation:{}", self.id)
    }
}

/// Everything a lineage traversal reads, scoped to exactly one world.
/// Observations, instance provenance, and objects must be assembled from
/// this world; commits naming another world are refused by the projection.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LineageInput {
    /// The explicit world of every fact and object in this input.
    /// Required on the wire; an empty world is refused by the projection.
    pub world: String,
    /// Ledger commits, in any order.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub commits: Vec<LineageCommit>,
    /// Ingested observations.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub observations: Vec<LineageObservation>,
    /// Provenance stored with facts.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub instances: Vec<ProvenancedInstance>,
    /// Ontology objects and their links.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub objects: Vec<LineageObject>,
}
