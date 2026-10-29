//! Canonical Norn entitlement data (#3244).
//!
//! Entitlements are serializable permission facts. They do not contain
//! executable predicates: the closed condition variants describe the small
//! relationship checks understood by the authorization kernel.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::members::IrVersion01;
use crate::{LinkId, ObjectTypeName};

/// Canonical entitlement record version 0.1.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Entitlement {
    /// The entitlement format version.
    #[serde(rename = "entitlement_version")]
    pub entitlement_version: IrVersion01,
    /// Stable identity within the source system and scope.
    pub id: String,
    /// The principal, role, or group to which this entitlement applies.
    pub grantee: EntitlementGrantee,
    /// Norn object type to which the entitlement applies.
    #[serde(rename = "target_type")]
    pub target_type: ObjectTypeName,
    /// Exact Norn action ids permitted by this entitlement.
    pub actions: Vec<String>,
    /// Exact scalar resource attributes that must match.
    #[serde(
        rename = "scope_attributes",
        default,
        skip_serializing_if = "std::collections::BTreeMap::is_empty"
    )]
    pub scope_attributes: std::collections::BTreeMap<String, String>,
    /// The closed relationship condition.
    pub condition: EntitlementCondition,
    /// Source provenance for review and explain output.
    pub source: EntitlementSource,
    /// Whether this row is explanatory only or can grant access.
    pub status: EntitlementStatus,
}

/// A typed entity reference. Keys are canonical object keys, not display names.
#[derive(
    Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ObjectRef {
    /// Referenced Norn object type.
    pub object_type: ObjectTypeName,
    /// Canonical object key.
    pub key: String,
}

/// The typed recipient of an entitlement.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum EntitlementGrantee {
    /// A role entity.
    Role {
        /// The referenced Norn object type (`identity.Role`).
        object_type: ObjectTypeName,
        /// Canonical role object key.
        key: String,
    },
    /// A group entity.
    Group {
        /// The referenced Norn object type (`identity.Group`).
        object_type: ObjectTypeName,
        /// Canonical group object key.
        key: String,
    },
    /// A principal entity, normally a `party.Person`.
    Principal {
        /// The referenced Norn object type.
        object_type: ObjectTypeName,
        /// Canonical principal object key.
        key: String,
    },
}

impl EntitlementGrantee {
    /// The referenced Norn object type.
    #[must_use]
    pub fn object_type(&self) -> &ObjectTypeName {
        match self {
            Self::Role { object_type, .. }
            | Self::Group { object_type, .. }
            | Self::Principal { object_type, .. } => object_type,
        }
    }

    /// The canonical referenced object key.
    #[must_use]
    pub fn key(&self) -> &str {
        match self {
            Self::Role { key, .. } | Self::Group { key, .. } | Self::Principal { key, .. } => key,
        }
    }
}

/// A closed relationship constraint on the target resource.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum EntitlementCondition {
    /// The target has no additional relationship constraint.
    None,
    /// The target's named link must point to the acting principal's represented
    /// identity.
    OwnerOnly {
        /// Target-side link to the acting principal's identity.
        link: LinkId,
    },
    /// The target's named link must point to this exact object.
    LinkedRecord {
        /// Target-side link to inspect.
        link: LinkId,
        /// Exact linked object.
        target: ObjectRef,
    },
}

/// Provenance identifying the permission row that produced this entitlement.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EntitlementSource {
    /// Source system identifier.
    pub system: String,
    /// Exact source record id.
    pub record_id: String,
    /// Source record version or revision token.
    pub version: String,
}

/// Whether an entitlement participates in production authorization.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum EntitlementStatus {
    /// Explain-only import candidate.
    Shadow,
    /// May grant access when all entitlement predicates match.
    Enforced,
}
