//! Shared checker/runtime diagnostic for the single-cell atomic profile.

/// TXN-CELL-1: emitted when atomic dependencies have foreign or unknown ownership.
pub const ATOMIC_BOUNDARY_CODE: &str = "core.txn.atomic_boundary";
