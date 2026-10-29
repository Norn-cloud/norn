//! Typed RuleIR roots.
//!
//! [`RuleRoot`] is the strict, typed contract of a `rule` artifact payload. It
//! covers both executable generations:
//!
//! - **RuleIR v1** (`ruleVersion` [`RULE_IR_V1_VERSION`]): the closed decision
//!   table ([`RuleDecisionV1`]) that `norn-engine`'s closed evaluator
//!   (`RuleCondition`) still runs until T1-EXPR-5 replaces it.
//! - **RuleIR v2** (`ruleVersion` [`RULE_IR_V2_VERSION`]): a GoRules JDM graph
//!   ([`JdmGraph`]) under the Norn profile, with ZEN Expression cells (ruling
//!   2026-09-25 §3), and the [`RuleContractV2`] it implements. Evaluation
//!   lands with `norn-decisions` (T3-DEC-2).
//!
//! # The decision contract (T3-DEC-1)
//!
//! A [`RuleContractV2::Decide`] graph is the decision function
//! `decide(slice, event) -> { facts, commands, timers }`: its `inputSchema` is
//! a closed record of exactly [`DECIDE_INPUT_MEMBERS`] (the declared input
//! slice and the triggering event, each a closed record) and its
//! `outputSchema` a closed record of exactly [`DECIDE_OUTPUT_MEMBERS`], each a
//! list of closed records: the canonical [`crate::FactChange`] (T3-LIFE-2),
//! then the `Command` and `Timer` item schemas of this decision. A
//! [`RuleContractV2::SubDecision`] graph is a helper evaluated only through
//! another graph's `decisionNode`; its closed
//! input and output records are free. The ontology selection behind `slice`
//! (object types, links, bounded depth) is T3-DEC-3.
//!
//! The envelope's [`crate::RulePayload`] stays the lenient wire root (unknown
//! or malformed members become checker diagnostics); `norn-checker` decodes it
//! into [`RuleRoot`]. The pre-closed legacy form (`appliesTo`, `expression`,
//! `severity`) exists only on the lenient root.

use std::collections::BTreeMap;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::JsonValue;
use crate::ZenExpression;
use crate::action::ActionCommandChildMerge;
use crate::jdm::JdmGraph;
use crate::members::{EmptyList, JsonFalse, WholeNumber, present_non_null};

/// `ruleVersion` of a closed decision-table rule (RuleIR v1).
pub const RULE_IR_V1_VERSION: &str = "0.1";
/// `ruleVersion` of a JDM decision-graph rule (RuleIR v2).
pub const RULE_IR_V2_VERSION: &str = "2.0";

/// Members of a decide graph's `inputSchema`: the declared input slice and the
/// triggering event (ruling 2026-09-25 §3).
pub const DECIDE_INPUT_MEMBERS: [&str; 2] = ["slice", "event"];
/// Members of a decide graph's `outputSchema`: the fact changes, commands, and
/// timers the decision emits (ruling 2026-09-25 §3).
pub const DECIDE_OUTPUT_MEMBERS: [&str; 3] = ["facts", "commands", "timers"];

/// Maximum causal depth admitted for rule-to-rule cascades.
pub const MAX_RULE_CAUSAL_DEPTH: u8 = 8;

/// A JSON object (for example a closed JSON Schema document or a record of
/// output defaults).
pub type JsonObject = BTreeMap<String, JsonValue>;

/// Typed payload of a `rule` artifact (RuleIR v1 or v2).
///
/// Exactly one of `decision` (v1) and `graph` (v2) is present; the checker
/// enforces that, the `ruleVersion` pairing, and that `contract` is present
/// exactly with `graph`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RuleRoot {
    pub rule_id: String,
    pub rule_version: String,
    /// Closed JSON Schema of the decision input.
    pub input_schema: JsonObject,
    /// Closed JSON Schema of the decision output.
    pub output_schema: JsonObject,
    pub scope_required: Vec<String>,
    /// RuleIR v1 closed decision.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub decision: Option<RuleDecisionV1>,
    /// RuleIR v2 JDM decision graph.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub graph: Option<JdmGraph>,
    /// RuleIR v2: the contract the graph implements.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub contract: Option<RuleContractV2>,
    /// RuleIR v2 `decide`: the committed ledger events that trigger the
    /// decision (T3-DEC-3, #2671). A host runs one turn per matching event.
    /// Omitted when empty (a decision no host triggers).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub triggers: Vec<RuleTrigger>,
    /// RuleIR v2 `decide`: the declared read slice, loaded in order (T3-DEC-3,
    /// #2671). Each entry is one object, named like the member of the
    /// `slice` input record it fills. Omitted when empty.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub slice: Vec<SliceDeclaration>,
    /// RuleIR v2 guarded source updates. A graph can activate only one of
    /// these declarations by emitting `{ "sourceCommandId": "<id>" }` in
    /// `commands`; other command records cannot acquire this authority.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub source_commands: Vec<RuleSourceCommand>,
    /// RuleIR v2 output defaults: values for declared output fields that the
    /// graph leaves unset (for example when no decision-table row matched).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub defaults: Option<JsonObject>,
    /// Deterministic `RuleIR` has no side effects: only `false` is admitted.
    #[serde(
        default,
        deserialize_with = "present_non_null",
        skip_serializing_if = "Option::is_none"
    )]
    #[schemars(with = "JsonFalse")]
    pub side_effects: Option<JsonFalse>,
    /// Deterministic `RuleIR` requires no services: only `[]` is admitted.
    #[serde(
        default,
        deserialize_with = "present_non_null",
        skip_serializing_if = "Option::is_none"
    )]
    #[schemars(with = "EmptyList")]
    pub required_services: Option<EmptyList>,
    /// Reserved extension slots keyed by `<name>/<semver>`; `null` is rejected.
    #[serde(
        default,
        deserialize_with = "present_non_null",
        skip_serializing_if = "Option::is_none"
    )]
    #[schemars(with = "JsonObject")]
    pub extensions: Option<JsonObject>,
}

/// A guarded update capability for one declared mapped, observed object.
///
/// The command always updates the exact object identified by `slice`, and
/// takes its optimistic-concurrency version from `modifiedProperty` in that
/// object. The checker binds the connector, mapping, map, source doctype and
/// raw source `modified` field before admitting the declaration.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RuleSourceCommand {
    /// Stable local id used by the graph's `sourceCommandId` activation.
    pub id: String,
    /// Connector artifact id that grants a generic update capability.
    pub connector: String,
    /// Mapping artifact id that produces the observed target.
    pub mapping: String,
    /// Object map id in the mapping artifact.
    pub map: String,
    /// Exact source document type allowed by this declaration.
    pub doctype: String,
    /// Name of the single-object slice member containing the observed target.
    pub slice: String,
    /// Mapped target property carrying the source's raw `modified` value.
    pub modified_property: String,
    /// Source fields whose values are derived from the decide input.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub fields: BTreeMap<String, ZenExpression>,
    /// Owner-scoped child-table desired sets derived from the decide input.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub child_merges: Vec<ActionCommandChildMerge>,
}

/// The contract a RuleIR v2 graph implements. See the module docs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum RuleContractV2 {
    /// `decide(slice, event) -> { facts, commands, timers }`: a decision the
    /// kernel invokes.
    Decide,
    /// A helper graph, evaluated only as another graph's sub-decision.
    SubDecision,
}

/// RuleIR v1 closed decision.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(
    tag = "kind",
    rename_all = "snake_case",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum RuleDecisionV1 {
    /// A constant output record.
    Literal { value: JsonObject },
    /// An ordered decision table.
    DecisionTable {
        hit_policy: RuleHitPolicyV1,
        inputs: Vec<RuleInputV1>,
        rows: Vec<RuleRowV1>,
        /// Output when no row matches.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        default_output: Option<JsonObject>,
    },
}

/// RuleIR v1 hit policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum RuleHitPolicyV1 {
    First,
    Unique,
}

/// RuleIR v1 input column.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RuleInputV1 {
    pub field: String,
    #[serde(rename = "type")]
    pub value_type: RuleValueTypeV1,
}

/// RuleIR v1 input column type.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum RuleValueTypeV1 {
    String,
    Number,
    Boolean,
    Json,
}

/// RuleIR v1 row with a stable id.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RuleRowV1 {
    pub row_id: String,
    pub when: Vec<RuleConditionV1>,
    pub output: JsonObject,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
}

/// RuleIR v1 closed-operator condition, mirroring the conditions the
/// `norn-engine` closed evaluator runs (`RuleCondition`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
pub enum RuleConditionV1 {
    Exists {
        field: String,
    },
    Eq {
        field: String,
        value: JsonValue,
    },
    Neq {
        field: String,
        value: JsonValue,
    },
    Lt {
        field: String,
        value: JsonValue,
    },
    Lte {
        field: String,
        value: JsonValue,
    },
    Gt {
        field: String,
        value: JsonValue,
    },
    Gte {
        field: String,
        value: JsonValue,
    },
    In {
        field: String,
        values: Vec<JsonValue>,
    },
    Between {
        field: String,
        min: JsonValue,
        max: JsonValue,
    },
}

/// A committed ledger event that triggers a `decide` rule (T3-DEC-3, #2671).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(
    tag = "on",
    rename_all = "snake_case",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum RuleTrigger {
    /// A declared lifecycle transition fired on an object of `objectType`.
    Transitioned {
        /// The object type.
        object_type: crate::ObjectTypeName,
        /// The transition id, declared by a lifecycle of the type.
        transition: crate::TransitionId,
        /// Maximum causal depth accepted when this rule already appears in the
        /// event's rule lineage. Omitted suppresses repeats of this rule.
        #[serde(
            default,
            deserialize_with = "deserialize_optional_u8",
            skip_serializing_if = "Option::is_none"
        )]
        #[schemars(with = "u8")]
        max_causal_depth: Option<u8>,
    },
    /// An object of `objectType` was created.
    Created {
        /// The object type.
        object_type: crate::ObjectTypeName,
        /// Maximum causal depth accepted when this rule already appears in the
        /// event's rule lineage. Omitted suppresses repeats of this rule.
        #[serde(
            default,
            deserialize_with = "deserialize_optional_u8",
            skip_serializing_if = "Option::is_none"
        )]
        #[schemars(with = "u8")]
        max_causal_depth: Option<u8>,
    },
    /// An object of `objectType` was deleted in its source: an observed
    /// delete, tombstoned or reached as a terminal lifecycle state (#3013).
    Deleted {
        /// The object type.
        object_type: crate::ObjectTypeName,
        /// Maximum causal depth accepted when this rule already appears in the
        /// event's rule lineage. Omitted suppresses repeats of this rule.
        #[serde(
            default,
            deserialize_with = "deserialize_optional_u8",
            skip_serializing_if = "Option::is_none"
        )]
        #[schemars(with = "u8")]
        max_causal_depth: Option<u8>,
    },
    /// An existing object changed one or more stored properties.
    Modified {
        /// The object type.
        object_type: crate::ObjectTypeName,
        /// Match when any named property changed; omitted matches any net
        /// property change.
        #[serde(
            default,
            deserialize_with = "present_non_null",
            skip_serializing_if = "Option::is_none"
        )]
        #[schemars(with = "Vec<crate::PropertyId>")]
        changed_properties: Option<Vec<crate::PropertyId>>,
        /// Maximum causal depth accepted when this rule already appears in the
        /// event's rule lineage. Omitted suppresses repeats of this rule.
        #[serde(
            default,
            deserialize_with = "deserialize_optional_u8",
            skip_serializing_if = "Option::is_none"
        )]
        #[schemars(with = "u8")]
        max_causal_depth: Option<u8>,
    },
    /// An observation from source system `source` was recorded.
    Observed {
        /// The source system (`ERPNext`).
        source: String,
    },
}

// Lenient IR numbers are binary64, so a typed trigger must accept an
// integral value after a JsonValue round trip while still rejecting null,
// fractions, negative numbers and values outside its public u8 contract.
fn deserialize_optional_u8<'de, D>(deserializer: D) -> Result<Option<u8>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    u8::try_from(WholeNumber::deserialize(deserializer)?.0)
        .map(Some)
        .map_err(serde::de::Error::custom)
}

impl RuleTrigger {
    /// Object type named by an object-specific trigger.
    #[must_use]
    pub fn object_type(&self) -> Option<&crate::ObjectTypeName> {
        match self {
            Self::Transitioned { object_type, .. }
            | Self::Created { object_type, .. }
            | Self::Deleted { object_type, .. }
            | Self::Modified { object_type, .. } => Some(object_type),
            Self::Observed { .. } => None,
        }
    }

    /// Maximum causal depth explicitly admitted for a repeated rule.
    #[must_use]
    pub const fn max_causal_depth(&self) -> Option<u8> {
        match self {
            Self::Transitioned {
                max_causal_depth, ..
            }
            | Self::Created {
                max_causal_depth, ..
            }
            | Self::Deleted {
                max_causal_depth, ..
            }
            | Self::Modified {
                max_causal_depth, ..
            } => *max_causal_depth,
            Self::Observed { .. } => None,
        }
    }

    /// Whether this trigger requires an object-modification snapshot.
    #[must_use]
    pub const fn is_modified(&self) -> bool {
        matches!(self, Self::Modified { .. })
    }
}

/// One entry of a `decide` rule's declared read slice: one object, or (a
/// [`SliceKey::Link`] key) the bounded list of objects a link reaches
/// (#3079).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SliceDeclaration {
    /// The member of the `slice` input record this entry fills.
    pub name: String,
    /// The type of the object (or of every listed object).
    pub object_type: crate::ObjectTypeName,
    /// How its key (or keys) are found.
    pub key: SliceKey,
    /// A [`SliceKey::Property`] entry whose key property is absent or null
    /// is omitted instead of refusing the turn (#3079). A list entry is never
    /// refused for being empty, so it needs no flag. Defaults to `false`; the
    /// canonical form omits `false`.
    #[serde(default, skip_serializing_if = "crate::entity_v2::property::is_false")]
    pub optional: bool,
}

impl SliceDeclaration {
    /// Whether the entry loads a list ([`SliceKey::Link`]).
    #[must_use]
    pub const fn is_list(&self) -> bool {
        matches!(self.key, SliceKey::Link { .. })
    }
}

/// Where a [`SliceDeclaration`]'s key comes from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub enum SliceKey {
    /// The object the triggering event names (a `transitioned` or `created`
    /// trigger).
    TriggerObject,
    /// The immutable before snapshot of a `modified` trigger, as
    /// `{version, values}`. This is decision input and never authorizes writes.
    TriggerBefore,
    /// The immutable after snapshot of a `modified` trigger, as
    /// `{version, values}`. This is decision input and never authorizes writes.
    TriggerAfter,
    /// The sorted property ids changed by a `modified` trigger.
    TriggerChanges,
    /// A text property of an earlier slice entry (a link by key).
    Property {
        /// The earlier entry's name.
        from: String,
        /// The property holding the key.
        property: crate::PropertyId,
    },
    /// Every target of link `link`, declared on the type of earlier
    /// single-object entry `from`, loaded as a list ordered by target key
    /// and bounded by the limit profile's slice records (#3079).
    Link {
        /// The earlier (single-object) entry's name.
        from: String,
        /// The link id, declared on that entry's type or a supertype.
        link: String,
    },
}
