//! RECEIPT-ONCE-1: the accepting transaction claims fresh evidence once.

/// The ledger frontier must still equal the authenticated intake frontier.
/// A consumed reference never becomes usable again.
pub(crate) const fn usable(consumed: bool, authenticated: u64, current: u64) -> bool {
    !consumed && authenticated == current
}
