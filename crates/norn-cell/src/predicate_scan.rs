//! Predicate reads over the normalized store (#2770 split from `host`): the
//! bounded enumeration a [`norn_kernel::PredicateQuery`] admits against.

use norn_kernel::cell_host::JsonObject;
use norn_kernel::{FactKey, FactVersion, PredicateObject, PredicateQuery};

use crate::engine::SqlEngine;
use crate::error::CellError;
use crate::reactive::{DecodeBudget, ReactiveLimits};
use crate::read::StoreReader;
use crate::store::MAIN_WORLD;

/// The slice profile's canonical byte ceiling, the read bound for a predicate
/// no caller narrowed.
pub(crate) fn slice_bytes() -> u64 {
    norn_ir::ResourceLimitProfile::norn_limits_v1()
        .slice
        .max_canonical_bytes
}

/// The live objects of `predicate`'s type that match it, in key order, at
/// most `limit` ([`norn_kernel::predicate_read`]). The cell has no property
/// index for arbitrary predicates, so it examines at most the slice
/// profile's record count of objects of the type, and at most `max_bytes`
/// of stored candidate bytes; a larger type is an incomplete enumeration
/// (`None`), which fails the read closed.
pub(crate) fn predicate_objects<S: SqlEngine>(
    reader: &StoreReader<'_, S>,
    predicate: &PredicateQuery,
    limit: usize,
    max_bytes: u64,
) -> Result<Option<Vec<PredicateObject>>, CellError> {
    reader.check_live()?;
    let scan_cap = usize::try_from(
        norn_ir::ResourceLimitProfile::norn_limits_v1()
            .slice
            .max_records,
    )
    .unwrap_or(usize::MAX);
    // Interfaces have no stored table chain; a predicate over one is
    // unsupported on every host.
    if !reader
        .model()
        .object_types()
        .any(|defined| defined.name == predicate.object_type)
    {
        return Ok(None);
    }
    // Candidate bytes are charged before each object is decoded, matching
    // or not: the scan stops at the byte bound instead of loading every
    // candidate first.
    let mut limits = ReactiveLimits::from_profile(&norn_ir::ResourceLimitProfile::norn_limits_v1());
    limits.max_candidate_bytes_per_commit = max_bytes;
    let mut budget = DecodeBudget::new(&limits);
    let Some(records) = reader.list_after_with_budget(
        MAIN_WORLD,
        predicate.object_type.as_str(),
        scan_cap.saturating_add(1),
        &mut budget,
    )?
    else {
        return Ok(None);
    };
    if records.len() > scan_cap {
        return Ok(None);
    }
    let candidates: Vec<(FactKey, FactVersion, JsonObject)> = records
        .into_iter()
        .map(|record| {
            (
                FactKey::new(record.object_type, record.id),
                FactVersion(record.version),
                record.values,
            )
        })
        .collect();
    Ok(Some(norn_kernel::predicate_read::select_members(
        predicate,
        candidates
            .iter()
            .map(|(key, version, values)| (key, *version, values)),
        limit,
    )))
}
