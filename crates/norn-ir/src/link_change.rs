//! The canonical link change: an item of a decide graph's optional `links`
//! output list (`decide(slice, event) -> { facts, links?, commands, timers }`).
//!
//! A [`LinkChange`] is one closed record discriminated by `op`:
//!
//! | `op` | members | meaning |
//! | --- | --- | --- |
//! | `add` | `sourceType`, `sourceKey`, `link`, `targetType`, `targetKey`, optional `values` | add a declared typed link between two objects |
//! | `remove` | `sourceType`, `sourceKey`, `link`, `targetType`, `targetKey` | remove a declared typed link between two objects |
//!
//! Like [`crate::FactChange`], values preserve exact decimal/integer numbers
//! without buffering through `f64`. The decide graph's `outputSchema` constrains
//! `op`, `sourceType`, `link`, and `targetType` to finite string `enum`s so the
//! checker verifies link targets statically before execution.

use schemars::{JsonSchema, Schema, schema_for};
use serde::{Deserialize, Serialize};

use crate::runtime::{Member, RuntimeObject, RuntimeValue};
use crate::{LinkId, ObjectTypeName};

/// Every member a [`LinkChange`] record may carry.
pub const LINK_CHANGE_MEMBERS: [&str; 7] = [
    "op",
    "sourceType",
    "sourceKey",
    "link",
    "targetType",
    "targetKey",
    "values",
];

/// The members every [`LinkChange`] carries.
pub const LINK_CHANGE_REQUIRED_MEMBERS: [&str; 6] = [
    "op",
    "sourceType",
    "sourceKey",
    "link",
    "targetType",
    "targetKey",
];

/// One typed link change a decision emits. See the module docs.
#[derive(Debug, Clone, PartialEq, Serialize, JsonSchema)]
#[serde(
    tag = "op",
    rename_all = "snake_case",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum LinkChange {
    /// Add a declared typed link between `source` and `target`.
    Add {
        /// Object type of the link's source endpoint.
        source_type: ObjectTypeName,
        /// Identity key of the source object.
        source_key: String,
        /// Declared link identifier on `source_type` (or a supertype).
        link: LinkId,
        /// Object type of the link's target endpoint.
        target_type: ObjectTypeName,
        /// Identity key of the target object.
        target_key: String,
        /// Optional link property values by property id.
        #[serde(default, skip_serializing_if = "RuntimeObject::is_empty")]
        values: RuntimeObject,
    },
    /// Remove a declared typed link between `source` and `target`.
    Remove {
        /// Object type of the link's source endpoint.
        source_type: ObjectTypeName,
        /// Identity key of the source object.
        source_key: String,
        /// Declared link identifier on `source_type` (or a supertype).
        link: LinkId,
        /// Object type of the link's target endpoint.
        target_type: ObjectTypeName,
        /// Identity key of the target object.
        target_key: String,
    },
}

/// The `op` of a [`LinkChange`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum LinkChangeOp {
    /// [`LinkChange::Add`].
    Add,
    /// [`LinkChange::Remove`].
    Remove,
}

impl LinkChangeOp {
    /// Every op, in wire order.
    pub const ALL: [Self; 2] = [Self::Add, Self::Remove];

    /// The wire value of `op`.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Add => "add",
            Self::Remove => "remove",
        }
    }

    /// The op with this wire value.
    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|op| op.as_str() == value)
    }

    /// Optional extra members this op admits besides [`LINK_CHANGE_REQUIRED_MEMBERS`].
    #[must_use]
    pub const fn optional_members(self) -> &'static [&'static str] {
        match self {
            Self::Add => &["values"],
            Self::Remove => &[],
        }
    }
}

impl LinkChange {
    /// The record's op.
    #[must_use]
    pub const fn op(&self) -> LinkChangeOp {
        match self {
            Self::Add { .. } => LinkChangeOp::Add,
            Self::Remove { .. } => LinkChangeOp::Remove,
        }
    }

    /// Decode a canonical [`LinkChange`] directly from an exact [`RuntimeValue`].
    ///
    /// # Errors
    ///
    /// Returns a reason when the value is not a closed [`LinkChange`] record
    /// matching its `op`'s exact member contract.
    pub fn from_runtime(value: &RuntimeValue) -> Result<Self, String> {
        let RuntimeValue::Object(entries) = value else {
            return Err("expected a record".to_owned());
        };
        for key in entries.keys() {
            if !LINK_CHANGE_MEMBERS.contains(&key.as_str()) {
                return Err(format!("unknown link change member {key:?}"));
            }
        }
        let text = |name: &'static str| match entries.get(name) {
            Some(RuntimeValue::String(text)) => Ok(text.clone()),
            Some(_) => Err(format!("member {name:?} must be a string")),
            None => Err(format!("missing required member {name:?}")),
        };
        let op_str = text("op")?;
        let op = LinkChangeOp::parse(&op_str)
            .ok_or_else(|| format!("unknown link change op {op_str:?}; expected add or remove"))?;
        let source_type = ObjectTypeName(text("sourceType")?);
        let source_key = text("sourceKey")?;
        let link = LinkId(text("link")?);
        let target_type = ObjectTypeName(text("targetType")?);
        let target_key = text("targetKey")?;
        let values = match entries.get("values") {
            None => Member::Absent,
            Some(value) => Member::Present(value.clone()),
        };
        decode_link_change_parts(
            op,
            source_type,
            source_key,
            link,
            target_type,
            target_key,
            values,
        )
    }
}

fn decode_link_change_parts(
    op: LinkChangeOp,
    source_type: ObjectTypeName,
    source_key: String,
    link: LinkId,
    target_type: ObjectTypeName,
    target_key: String,
    values: Member,
) -> Result<LinkChange, String> {
    match op {
        LinkChangeOp::Add => {
            let values = match values {
                Member::Absent => RuntimeObject::new(),
                Member::Present(RuntimeValue::Object(object)) => object,
                Member::Present(_) => return Err("member \"values\" must be an object".to_owned()),
            };
            Ok(LinkChange::Add {
                source_type,
                source_key,
                link,
                target_type,
                target_key,
                values,
            })
        }
        LinkChangeOp::Remove => {
            if values != Member::Absent {
                return Err("unknown field \"values\" for remove link change".to_owned());
            }
            Ok(LinkChange::Remove {
                source_type,
                source_key,
                link,
                target_type,
                target_key,
            })
        }
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct LinkChangeWire {
    op: String,
    source_type: ObjectTypeName,
    source_key: String,
    link: LinkId,
    target_type: ObjectTypeName,
    target_key: String,
    #[serde(default)]
    values: Member,
}

impl<'de> Deserialize<'de> for LinkChange {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        use serde::de::Error as _;
        let wire = LinkChangeWire::deserialize(deserializer)?;
        let op = LinkChangeOp::parse(&wire.op)
            .ok_or_else(|| D::Error::unknown_variant(&wire.op, &["add", "remove"]))?;
        decode_link_change_parts(
            op,
            wire.source_type,
            wire.source_key,
            wire.link,
            wire.target_type,
            wire.target_key,
            wire.values,
        )
        .map_err(D::Error::custom)
    }
}

/// Emit the JSON Schema of one [`LinkChange`] record.
#[must_use]
pub fn link_change_schema() -> Schema {
    schema_for!(LinkChange)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Decimal;

    #[test]
    fn add_and_remove_round_trip_with_exact_values() {
        let mut values = RuntimeObject::new();
        values.insert(
            "weight".to_owned(),
            RuntimeValue::Number(Decimal::parse("12.3450000000000000001").expect("decimal")),
        );
        let add = LinkChange::Add {
            source_type: ObjectTypeName("schedule.Commitment".to_owned()),
            source_key: "c-1".to_owned(),
            link: LinkId("for_agent".to_owned()),
            target_type: ObjectTypeName("schedule.Agent".to_owned()),
            target_key: "a-1".to_owned(),
            values,
        };
        let encoded = serde_json::to_string(&add).expect("encode");
        let decoded: LinkChange = serde_json::from_str(&encoded).expect("decode");
        assert_eq!(decoded, add);

        let remove = LinkChange::Remove {
            source_type: ObjectTypeName("schedule.Commitment".to_owned()),
            source_key: "c-1".to_owned(),
            link: LinkId("for_agent".to_owned()),
            target_type: ObjectTypeName("schedule.Agent".to_owned()),
            target_key: "a-1".to_owned(),
        };
        let encoded_remove = serde_json::to_string(&remove).expect("encode remove");
        let decoded_remove: LinkChange = serde_json::from_str(&encoded_remove).expect("decode");
        assert_eq!(decoded_remove, remove);
    }

    #[test]
    fn remove_rejects_values_and_add_rejects_null_values() {
        let with_values = serde_json::json!({
            "op": "remove",
            "sourceType": "a.Source",
            "sourceKey": "s-1",
            "link": "rel",
            "targetType": "a.Target",
            "targetKey": "t-1",
            "values": {}
        });
        assert!(serde_json::from_value::<LinkChange>(with_values).is_err());

        let with_null_values = serde_json::json!({
            "op": "add",
            "sourceType": "a.Source",
            "sourceKey": "s-1",
            "link": "rel",
            "targetType": "a.Target",
            "targetKey": "t-1",
            "values": null
        });
        assert!(serde_json::from_value::<LinkChange>(with_null_values).is_err());
    }
}
