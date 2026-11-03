//! CELL-QUALIFY-1: versioned namespaces for references leaving a bound cell.

use crate::{canonical::CanonicalJsonError, cell_id::CellId, function::Digest};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// The qualified commit provenance format. Legacy scalar positions have no cell.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
pub enum CommitFormat {
    /// Cell-qualified provenance version two.
    #[serde(rename = "norn.cell.commit/v2")]
    V2,
}

/// A committed position in exactly one logical cell and execution world.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct QualifiedCommit {
    /// Explicit format version.
    pub format: CommitFormat,
    /// Logical originating cell.
    pub cell: CellId,
    /// Execution world, never a global snapshot.
    pub world: String,
    /// Local ledger position.
    pub commit: u64,
}

impl QualifiedCommit {
    /// Qualify a local ledger position; this does not grant authority.
    #[must_use]
    pub fn new(cell: CellId, world: String, commit: u64) -> Self {
        Self {
            format: CommitFormat::V2,
            cell,
            world,
            commit,
        }
    }

    /// Whether authenticated host context permits using this origin locally.
    #[must_use]
    pub fn belongs_to(&self, cell: &CellId, world: &str) -> bool {
        crate::boundary_context::same_origin(
            cell.as_str().as_bytes(),
            world.as_bytes(),
            self.cell.as_str().as_bytes(),
            self.world.as_bytes(),
        )
    }
}

/// Old scalar provenance retains its original canonical bytes and stays local.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(untagged)]
pub enum CommitProvenance {
    /// Historical local-only position; never portable evidence.
    Legacy(u64),
    /// New cell/world-qualified origin.
    Qualified(QualifiedCommit),
}

impl CommitProvenance {
    /// Qualified origin, absent for historical scalar positions.
    #[must_use]
    pub const fn qualified(&self) -> Option<&QualifiedCommit> {
        match self {
            Self::Legacy(_) => None,
            Self::Qualified(origin) => Some(origin),
        }
    }
}

/// Portable identity format, independent of the local storage key format.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
pub enum ReferenceFormat {
    /// First qualified reference format.
    #[serde(rename = "norn.cell.reference/v1")]
    V1,
}

/// Distinct local key spaces may reuse the same text.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ReferenceNamespace {
    /// Object or fact key, including its declared type in the local key.
    Fact,
    /// Foreign operation deduplication key.
    Operation,
    /// Outgoing command identity.
    Command,
    /// Incoming foreign delivery identity.
    Inbox,
    /// Authenticated Function result receipt.
    FunctionReceipt,
}

/// A versioned exported reference. Local keys are unchanged inside the host.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CellReference {
    /// Explicit boundary format.
    pub format: ReferenceFormat,
    /// Issuing cell.
    pub cell: CellId,
    /// Issuing execution world.
    pub world: String,
    /// Local identity's namespace.
    pub namespace: ReferenceNamespace,
    /// Unchanged local key or digest.
    pub local: String,
}

impl CellReference {
    /// Qualify a local reference at a trusted host boundary.
    #[must_use]
    pub fn new(cell: CellId, world: String, namespace: ReferenceNamespace, local: String) -> Self {
        Self {
            format: ReferenceFormat::V1,
            cell,
            world,
            namespace,
            local,
        }
    }

    /// Hash the complete versioned reference, including namespace and origin.
    ///
    /// # Errors
    /// Canonical serialization failed.
    pub fn content_digest(&self) -> Result<Digest, CanonicalJsonError> {
        norn_canonical::domain_hash_hex(b"norn.cell.reference/v1\0", self)
            .map(|hex| Digest::from_hex(&hex))
    }

    /// Whether this reference belongs to the authenticated local context.
    #[must_use]
    pub fn belongs_to(&self, cell: &CellId, world: &str) -> bool {
        crate::boundary_context::same_origin(
            cell.as_str().as_bytes(),
            world.as_bytes(),
            self.cell.as_str().as_bytes(),
            self.world.as_bytes(),
        )
    }
}
