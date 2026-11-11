//! The compare-and-swap decision of commit admission (#718 P8, K1/K3).
//!
//! A commit names, for each object it reads and then each object it writes,
//! the version the turn observed. [`first_conflict`] reads the current version
//! of each in that order and reports the first that differs; nothing after it
//! is read. The host lookup is the [`CurrentVersion`] parameter. This file
//! depends on `core` alone, so the proof tools extract this production file
//! itself (`scripts/verify-kernel-commit-aeneas.sh`).

/// The current committed version of a key, read from the host.
pub(crate) trait CurrentVersion<K, E> {
    /// The current version of `key`.
    ///
    /// # Errors
    ///
    /// The host's read failure.
    fn current(&mut self, key: &K) -> Result<u64, E>;
}

/// A stale expectation: the key, the version the turn read, and the version
/// now current.
pub(crate) type Conflict<K> = (K, u64, u64);

/// The first expectation in `reads`, then in `writes`, whose current version
/// differs from the version the turn read. Each current version is read once,
/// in that order, and none after the first conflict or the first host error.
///
/// # Errors
///
/// The first host read failure.
pub(crate) fn first_conflict<K: Copy, E, C: CurrentVersion<K, E>>(
    versions: &mut C,
    reads: &[(K, u64)],
    writes: &[(K, u64)],
) -> Result<Option<Conflict<K>>, E> {
    if let Some(conflict) = first_conflict_in(versions, reads)? {
        return Ok(Some(conflict));
    }
    first_conflict_in(versions, writes)
}

fn first_conflict_in<K: Copy, E, C: CurrentVersion<K, E>>(
    versions: &mut C,
    expectations: &[(K, u64)],
) -> Result<Option<Conflict<K>>, E> {
    let mut index: usize = 0;
    while let Some(expectation) = expectations.get(index) {
        let current = versions.current(&expectation.0)?;
        if current != expectation.1 {
            return Ok(Some((expectation.0, expectation.1, current)));
        }
        // `index < expectations.len()`, so the successor never saturates.
        index = index.checked_add(1).unwrap_or(usize::MAX);
    }
    Ok(None)
}
