//! Pure machine-version successor shared by commit admission and proof tools.

/// Return the strictly next version, refusing exhaustion rather than wrapping.
#[must_use]
pub(crate) const fn checked_next_version(version: u64) -> Option<u64> {
    version.checked_add(1)
}
