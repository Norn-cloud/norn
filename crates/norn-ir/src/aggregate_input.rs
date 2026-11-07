//! Bounded aggregate inputs of an action (G-QUESTION-FOLD, #1105).
//!
//! An [`ActionAggregateInput`] declares one exact fold over the stored
//! objects of a type: the objects whose properties equal values from the
//! action input, optionally inside a time window, summed over one property
//! or counted. The action's commit evaluates it against the snapshot it
//! commits on, and the matched set joins the commit's read set, so a
//! concurrent write that changes the match set conflicts. More than
//! `maxItems` matches refuses; a fold never truncates.

use std::collections::BTreeMap;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// One bounded aggregate an action's preconditions read by `name`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ActionAggregateInput {
    /// The precondition environment member holding the result. Unique among
    /// the action's aggregates and slice entries.
    pub name: String,
    /// The type whose stored objects are folded.
    pub object_type: crate::ObjectTypeName,
    /// Equality filter: each scalar property must equal the named input
    /// field. An empty filter matches every object of the type.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub filter: BTreeMap<crate::PropertyId, String>,
    /// Optional window over one date or timestamp property.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub window: Option<AggregateWindow>,
    /// What is computed over the matched objects.
    pub aggregate: AggregateFunction,
    /// Most objects the fold may match; one more refuses the action.
    pub max_items: u32,
}

/// A window over one date or timestamp property. Both edges and their
/// inclusivity are declared; there is no default edge semantics.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AggregateWindow {
    /// The date or timestamp property the window ranges over.
    pub property: crate::PropertyId,
    /// The input field holding the start edge.
    pub start: String,
    /// The input field holding the end edge.
    pub end: String,
    /// Whether an object exactly at `start` matches.
    pub start_inclusive: bool,
    /// Whether an object exactly at `end` matches.
    pub end_inclusive: bool,
}

/// The fold computed over the matched objects.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(
    tag = "kind",
    rename_all = "kebab-case",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum AggregateFunction {
    /// The exact sum of a non-nullable decimal or integer property, held at
    /// the property's declared scale. An empty match set sums to zero.
    Sum {
        /// The summed property.
        property: crate::PropertyId,
    },
    /// The number of matched objects.
    Count {},
}

impl ActionAggregateInput {
    /// Every input field the aggregate reads, filter fields first in property
    /// order, then the window edges.
    #[must_use]
    pub fn input_fields(&self) -> Vec<&str> {
        let mut fields: Vec<&str> = self.filter.values().map(String::as_str).collect();
        if let Some(window) = &self.window {
            fields.push(&window.start);
            fields.push(&window.end);
        }
        fields
    }
}
