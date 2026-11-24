//! Pure machine-version successor shared by commit admission and proof tools.

#[cfg(verus_keep_ghost)]
use vstd::prelude::*;

#[cfg(verus_keep_ghost)]
verus! {
    pub(crate) open spec fn successor(version: u64, result: Option<u64>) -> bool {
        match result {
            Some(next) => next as int == version as int + 1,
            None => version == u64::MAX,
        }
    }

    pub(crate) open spec fn deleted(version: u64, live: bool) -> bool {
        version != 0 && !live
    }

    proof fn successor_strict(version: u64, next: u64)
        requires successor(version, Some(next)),
        ensures next > version,
    {}
}

/// Return the strictly next version, refusing exhaustion rather than wrapping.
#[must_use]
#[cfg_attr(verus_keep_ghost, verus_spec(r => ensures
    #![verifier::proof_note("successor")]
    successor(version, r),
))]
pub(crate) const fn checked_next_version(version: u64) -> Option<u64> {
    version.checked_add(1)
}

/// ID-STABLE-1: absence at a retained nonzero version records a committed deletion.
#[must_use]
#[cfg_attr(verus_keep_ghost, verus_spec(r => ensures
    #![verifier::proof_note("deleted")]
    r == deleted(version, live),
))]
pub(crate) const fn has_committed_deletion(version: u64, live: bool) -> bool {
    version != 0 && !live
}
