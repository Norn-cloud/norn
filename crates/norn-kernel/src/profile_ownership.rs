//! TXN-CELL-1: unknown ownership never supports local atomic execution.

/// Ownership evidence supplied by the bound host, never by request JSON.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AtomicOwnership {
    /// The dependency belongs to this bound cell.
    Local,
    /// The dependency belongs to another cell.
    Foreign,
    /// Ownership or the dependency footprint cannot be established.
    Unknown,
}

/// Whether an ownership observation supports local atomic execution.
/// This predicate makes no splittability or distributed-commit claim.
#[must_use]
pub const fn supports_local(ownership: AtomicOwnership) -> bool {
    matches!(ownership, AtomicOwnership::Local)
}
