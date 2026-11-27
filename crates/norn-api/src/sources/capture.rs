//! SOURCE-CAPTURE-1 helpers for durable source cursors and page capture specs.

use std::collections::BTreeMap;

use norn_cell::engine::SqlEngine;
use norn_cell::source_cursor::{CursorAdvanceSpec, SourceLease};
use norn_ir::RuntimeValue;

use super::{CONNECTION_TYPE, ConnectionConfig, Failure, RUN_LEASE_MS, fail};
use crate::host::DevHost;
use crate::pull::{CursorAdvance, Watermark};

/// Read the connection's durably committed watermarks: `_norn_source_cursor`
/// is authoritative; falls back to `source.Connection.watermark` only for cells
/// whose prior syncs predated `_norn_source_cursor`.
pub(super) fn durable_watermarks<E: SqlEngine>(
    host: &DevHost<E>,
    connection_id: &str,
) -> Option<BTreeMap<String, Watermark>> {
    if let Ok(cursors) = host.source_cursors(connection_id)
        && !cursors.is_empty()
    {
        let mut parsed = BTreeMap::new();
        for (dataset, record) in cursors {
            if let Ok(watermark) = serde_json::from_str::<Watermark>(&record.cursor_json) {
                parsed.insert(dataset, watermark);
            }
        }
        if !parsed.is_empty() {
            return Some(parsed);
        }
    }
    host.host_object(CONNECTION_TYPE, connection_id)
        .ok()
        .flatten()
        .and_then(|object| {
            object
                .get("watermark")
                .and_then(RuntimeValue::as_str)
                .map(str::to_owned)
        })
        .and_then(|text| serde_json::from_str::<BTreeMap<String, Watermark>>(&text).ok())
}

/// Build the atomic [`CursorAdvanceSpec`] for one chunk of an accepted page.
/// Non-final chunks omit `next_cursor` while still enforcing lease fencing and
/// the cursor precondition.
pub(super) fn cursor_advance_spec(
    config: &ConnectionConfig,
    lease: &SourceLease,
    pending: &CursorAdvance,
    now_ms: u64,
    is_final_chunk: bool,
) -> Result<CursorAdvanceSpec, Failure> {
    let expected_previous_cursor = pending
        .expected_previous
        .as_ref()
        .map(Watermark::cursor_json)
        .transpose()
        .map_err(|e| fail("source.ingest", e.to_string()))?;
    let next_cursor = if is_final_chunk {
        pending
            .next
            .as_ref()
            .map(Watermark::cursor_json)
            .transpose()
            .map_err(|e| fail("source.ingest", e.to_string()))?
    } else {
        None
    };
    Ok(CursorAdvanceSpec {
        connection_id: config.id.clone(),
        dataset: pending.dataset.clone(),
        lease_id: lease.lease_id.clone(),
        generation: lease.generation,
        now_ms,
        expected_previous_cursor,
        next_cursor,
        renew_ttl_ms: Some(RUN_LEASE_MS),
    })
}
