//! Versioned unit-agnostic quantity commits on the normalized cell path.
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::{LedgerPackIr, LedgerParameterValue};

/// Wire version of the current cell quantity boundary.
pub const QUANTITY_CELL_VERSION: &str = "norn.quantity.commit/2";

/// A signed amount selected from one pinned reservation leg.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct QuantitySlice {
    /// Identity in the original evaluated vector.
    pub leg_id: String,
    /// Canonical signed i128, with the original direction.
    pub amount_unscaled: String,
}

/// Exact optimistic fence for one declared balance partition.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct QuantityBalanceVersion {
    /// Declared account identity.
    pub account: String,
    /// Declared unit identity.
    pub unit: String,
    /// Declared conserved layer.
    pub layer: String,
    /// Zero denotes a balance with no prior mutation.
    pub version: u64,
}

/// Full resolution is a special case of a conserving subset.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum QuantityResolution {
    /// Resolve all remaining legs.
    All,
    /// Resolve the explicit nonempty conserving subset.
    Subset { legs: Vec<QuantitySlice> },
}

/// One bounded, version-fenced quantity operation. The cell supplies scope,
/// idempotency identity and trusted logical time, never the caller's wall clock.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct QuantityRequest {
    /// Exact boundary version.
    pub version: String,
    /// Cell-local ledger identity; a lot can be an account identity in the pack.
    pub ledger_id: String,
    /// Complete balance read set; no ledger-wide optimistic fence.
    pub balance_versions: Vec<QuantityBalanceVersion>,
    /// The closed operation, including conserving partial resolution.
    pub operation: QuantityOperation,
}

/// Closed quantity operations; costing and currency semantics belong in packs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum QuantityOperation {
    /// Pin an immutable, checker-admitted pack before the first posting.
    Define {
        /// Configured maximum declared accounts for this admission.
        account_limit: u32,
        /// Exact unit/dimension/precision, account, layer and template definitions.
        pack: LedgerPackIr,
        /// Explicit accounts that may have negative available quantity (e.g. issuance).
        unrestricted_accounts: Vec<String>,
    },
    /// Evaluate and append one complete conserved posting.
    Post {
        /// Immutable transaction identity.
        posting_id: String,
        /// Template in the pinned pack.
        template_id: String,
        /// Exact bounded inputs.
        parameters: Vec<LedgerParameterValue>,
    },
    /// Hold all negative legs of a complete evaluated vector.
    Reserve {
        /// Stable reservation identity.
        reservation_id: String,
        /// Template in the pinned pack.
        template_id: String,
        /// Exact bounded inputs.
        parameters: Vec<LedgerParameterValue>,
        /// Exclusive trusted-time deadline in milliseconds.
        expires_at: u64,
    },
    /// Post a conserving subset and retain the funded remainder atomically.
    PostReservation {
        /// The open reservation.
        reservation_id: String,
        /// Immutable transaction identity.
        posting_id: String,
        /// Conserving amounts from the remaining pinned vector.
        resolution: QuantityResolution,
    },
    /// Void a conserving subset without posting, before expiry.
    Void {
        /// The open reservation.
        reservation_id: String,
        /// Conserving amounts from the remaining pinned vector.
        resolution: QuantityResolution,
    },
    /// Release all holds at or after the trusted-time deadline.
    Expire {
        /// The open reservation.
        reservation_id: String,
    },
    /// Append an exact negation linked to one unreversed original posting.
    Reverse {
        /// Original, non-reversal transaction.
        original_id: String,
        /// New immutable transaction identity.
        posting_id: String,
    },
}
