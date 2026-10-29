//! The canonical fact change (T3-LIFE-2, #2673): the item of a decide graph's
//! `facts` output list (ruling 2026-09-25 §3,
//! `decide(slice, event) -> { facts: [FactChange], commands, timers }`).
//!
//! A [`FactChange`] is one closed record discriminated by `op`:
//!
//! | `op` | members | meaning |
//! | --- | --- | --- |
//! | `create` | `objectType`, `key`, `values` | create the object with these property values |
//! | `set` | `objectType`, `key`, `property`, `value` | set one stored property |
//! | `delete` | `objectType`, `key` | delete the object |
//! | `transition` | `objectType`, `key`, `lifecycle`, `transition` | fire a declared transition of the lifecycle of property `lifecycle` |
//!
//! A decide graph may also emit **`patch`** records (#3080): `objectType`,
//! `key`, and `patch`, a closed record of stored property values. A patch is
//! not a canonical [`FactChange`]: the evaluator (`norn-decisions`) lowers
//! it to one `set` per property, in property-id order, so the kernel, the
//! hosts, and the ledger keep the four canonical ops. Unlike `set`, whose one `value`
//! member has one schema type for every property a decision sets, a patch
//! carries each property at its own type.
//!
//! **A lifecycle property changes only through `transition`** (ruling
//! 2026-09-25 §2: the declared transition list "is the single owner of the
//! state machine"). A decision names the transition by its id, which the
//! lifecycle owns; `set` of a lifecycle property is rejected by the checker,
//! and `create` may only write an initial state. The kernel resolves the
//! transition against the object's current state and guards at commit.
//!
//! The decide graph's `outputSchema` declares its `facts` item schema as one
//! closed record over [`FACT_CHANGE_MEMBERS`] (the closed schema subset has
//! no unions). Which ops, object types, properties, and transitions the
//! decision can emit are string `enum`s in that schema, so the checker knows
//! them statically; `docs/platform/rule-ir-v2.md` has the rules.

use schemars::{JsonSchema, Schema, schema_for};
use serde::{Deserialize, Serialize};

use crate::runtime::{Member, RuntimeObject, RuntimeValue};
use crate::{ObjectTypeName, PropertyId, TransitionId};

/// Every member a [`FactChange`] record may carry.
pub const FACT_CHANGE_MEMBERS: [&str; 9] = [
    "op",
    "objectType",
    "key",
    "values",
    "property",
    "value",
    "lifecycle",
    "transition",
    "patch",
];

/// The members every [`FactChange`] carries.
pub const FACT_CHANGE_REQUIRED_MEMBERS: [&str; 3] = ["op", "objectType", "key"];

// Values are exact runtime values (#2770). Decoding goes through
// `FactChangeWire` (a plain record), not a derived internally tagged decoder,
// which would buffer numbers through `f64`.
/// One fact change a decision emits. See the module docs.
#[derive(Debug, Clone, PartialEq, Serialize, JsonSchema)]
#[serde(
    tag = "op",
    rename_all = "snake_case",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum FactChange {
    /// Create an object. A lifecycle property it writes must hold an initial
    /// state; one it omits takes its default, which is an initial state.
    Create {
        object_type: ObjectTypeName,
        /// The object's identity key value.
        key: String,
        /// Property values by property id.
        values: RuntimeObject,
    },
    /// Set one stored property. Never a lifecycle property.
    Set {
        object_type: ObjectTypeName,
        key: String,
        property: PropertyId,
        value: RuntimeValue,
    },
    /// Delete an object.
    Delete {
        object_type: ObjectTypeName,
        key: String,
    },
    /// Fire a declared transition of the lifecycle of property `lifecycle`.
    Transition {
        object_type: ObjectTypeName,
        key: String,
        /// The lifecycle property (it names the lifecycle: one per property).
        lifecycle: PropertyId,
        /// The transition id, declared by that lifecycle.
        transition: TransitionId,
    },
}

/// The `op` of a [`FactChange`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum FactChangeOp {
    /// [`FactChange::Create`].
    Create,
    /// [`FactChange::Set`].
    Set,
    /// [`FactChange::Delete`].
    Delete,
    /// [`FactChange::Transition`].
    Transition,
    /// A multi-property `set` (#3080), lowered to [`FactChange::Set`]s by the
    /// evaluator (`norn-decisions`); never a [`FactChange`] itself.
    Patch,
}

impl FactChangeOp {
    /// Every op, in wire order.
    pub const ALL: [Self; 5] = [
        Self::Create,
        Self::Set,
        Self::Delete,
        Self::Transition,
        Self::Patch,
    ];

    /// The wire value of `op`.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Create => "create",
            Self::Set => "set",
            Self::Delete => "delete",
            Self::Transition => "transition",
            Self::Patch => "patch",
        }
    }

    /// The op with this wire value.
    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|op| op.as_str() == value)
    }

    /// The members this op carries besides [`FACT_CHANGE_REQUIRED_MEMBERS`].
    #[must_use]
    pub const fn members(self) -> &'static [&'static str] {
        match self {
            Self::Create => &["values"],
            Self::Set => &["property", "value"],
            Self::Delete => &[],
            Self::Transition => &["lifecycle", "transition"],
            Self::Patch => &["patch"],
        }
    }
}

impl FactChange {
    /// The record's op.
    #[must_use]
    pub const fn op(&self) -> FactChangeOp {
        match self {
            Self::Create { .. } => FactChangeOp::Create,
            Self::Set { .. } => FactChangeOp::Set,
            Self::Delete { .. } => FactChangeOp::Delete,
            Self::Transition { .. } => FactChangeOp::Transition,
        }
    }
}

/// The decoded record of a [`FactChange`]: every member, present (even as
/// `null`) or absent.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct FactChangeWire {
    op: String,
    object_type: ObjectTypeName,
    key: String,
    #[serde(default)]
    values: Member,
    #[serde(default)]
    property: Member,
    #[serde(default)]
    value: Member,
    #[serde(default)]
    lifecycle: Member,
    #[serde(default)]
    transition: Member,
}

impl<'de> Deserialize<'de> for FactChange {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        use serde::de::Error as _;
        use serde::de::Unexpected;
        let wire = FactChangeWire::deserialize(deserializer)?;
        let op = FactChangeOp::parse(&wire.op).ok_or_else(|| {
            D::Error::unknown_variant(&wire.op, &["create", "set", "delete", "transition"])
        })?;
        // Exactly the members of `op`: a member of another op is unknown,
        // also when it is `null`.
        let present = [
            ("values", &wire.values),
            ("property", &wire.property),
            ("value", &wire.value),
            ("lifecycle", &wire.lifecycle),
            ("transition", &wire.transition),
        ];
        if let Some((member, _)) = present
            .iter()
            .find(|(member, value)| **value != Member::Absent && !op.members().contains(member))
        {
            return Err(D::Error::unknown_field(member, op.members()));
        }
        let invalid = |name: &'static str, expected: &'static str| {
            D::Error::invalid_type(Unexpected::Other(name), &expected)
        };
        let required = |member: Member, name: &'static str| match member {
            Member::Present(value) => Ok(value),
            Member::Absent => Err(D::Error::missing_field(name)),
        };
        let text = |member: Member, name: &'static str| match required(member, name)? {
            RuntimeValue::String(text) => Ok(text),
            RuntimeValue::Null
            | RuntimeValue::Bool(_)
            | RuntimeValue::Number(_)
            | RuntimeValue::Array(_)
            | RuntimeValue::Object(_) => Err(invalid(name, "a string")),
        };
        let object = |member: Member, name: &'static str| match required(member, name)? {
            RuntimeValue::Object(entries) => Ok(entries),
            RuntimeValue::Null
            | RuntimeValue::Bool(_)
            | RuntimeValue::Number(_)
            | RuntimeValue::String(_)
            | RuntimeValue::Array(_) => Err(invalid(name, "an object")),
        };
        let (object_type, key) = (wire.object_type, wire.key);
        Ok(match op {
            FactChangeOp::Create => Self::Create {
                object_type,
                key,
                values: object(wire.values, "values")?,
            },
            FactChangeOp::Set => Self::Set {
                object_type,
                key,
                property: PropertyId(text(wire.property, "property")?),
                value: required(wire.value, "value")?,
            },
            FactChangeOp::Delete => Self::Delete { object_type, key },
            FactChangeOp::Transition => Self::Transition {
                object_type,
                key,
                lifecycle: PropertyId(text(wire.lifecycle, "lifecycle")?),
                transition: TransitionId(text(wire.transition, "transition")?),
            },
            FactChangeOp::Patch => {
                return Err(D::Error::custom(
                    "a patch is not a canonical fact change; the evaluator lowers it to sets",
                ));
            }
        })
    }
}

/// Emit the JSON Schema of one [`FactChange`] record.
#[must_use]
pub fn fact_change_schema() -> Schema {
    schema_for!(FactChange)
}
