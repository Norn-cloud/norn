//! SOURCE-CAPTURE-1 helpers for durable source cursors, legacy upgrade
//! seeding, complete-page capture specs, and fenced-run report guards.

use std::collections::BTreeMap;

use norn_cell::MappingOutcome;
use norn_cell::engine::SqlEngine;
use norn_cell::source_cursor::{CursorAdvanceSpec, PendingSourceCapture, SourceLease};
use norn_ir::RuntimeValue;

use super::{CONNECTION_TYPE, ConnectionConfig, Failure, RUN_LEASE_MS, fail, rfc3339};
use crate::host::{DevHost, DisputeFallbackState, SourceCapturePlan};
use crate::pull::{AcceptedPage, CursorAdvance, PullProfile, Watermark, observation};
use norn_cell::mapping_bundle::MappingInput;

/// One active source page draining through paced processing (`advance` is
/// `None` for pages recovered from `_norn_source_pending` after a prior crash).
pub(super) enum ActiveCapture {
    Captured {
        pending: Box<PendingSourceCapture>,
        outcomes: BTreeMap<String, MappingOutcome>,
        advance: Option<CursorAdvance>,
    },
    DisputeFallback {
        state: Box<DisputeFallbackState>,
        advance: CursorAdvance,
    },
}

/// Read the connection's durably committed watermarks. `_norn_source_cursor` is
/// authoritative; when upgrading a populated legacy cell whose prior syncs only
/// wrote `source.Connection.watermark`, seed each legacy dataset cursor into
/// `_norn_source_cursor` under the active fencing lease before planning the
/// first request. If `source.Connection.mapping_id` differs from
/// `config.mapping` (a mapping rebind), clear inherited cursors under the
/// fencing lease so the rebound mapping pulls from `feed.initial` instead of
/// skipping rows and stamping an inherited cursor with the new mapping.
pub(super) fn load_and_seed_watermarks<E: SqlEngine>(
    host: &DevHost<E>,
    config: &ConnectionConfig,
    lease: &SourceLease,
    known: Option<BTreeMap<String, Watermark>>,
    now_ms: u64,
) -> Result<BTreeMap<String, Watermark>, Failure> {
    let connection_id = config.id.as_str();
    let stored_mapping = host
        .host_object(CONNECTION_TYPE, connection_id)
        .ok()
        .flatten()
        .and_then(|object| {
            object
                .get("mapping_id")
                .and_then(RuntimeValue::as_str)
                .map(str::to_owned)
        });
    if stored_mapping
        .as_deref()
        .is_some_and(|m| !m.is_empty() && m != config.mapping)
    {
        host.reset_source_cursors(connection_id, &lease.lease_id, lease.generation, now_ms)
            .map_err(|error| {
                let (code, message) = error.code_and_message();
                fail(code, message)
            })?;
        return Ok(BTreeMap::new());
    }
    let cursors = host
        .source_cursors(connection_id)
        .map_err(|r| fail("source.ingest", r.message))?;
    if !cursors.is_empty() {
        let mut parsed = BTreeMap::new();
        for (dataset, record) in cursors {
            if let Ok(watermark) = serde_json::from_str::<Watermark>(&record.cursor_json) {
                parsed.insert(dataset, watermark);
            }
        }
        return Ok(parsed);
    }
    let legacy = legacy_connection_watermarks(host, connection_id)
        .or(known)
        .unwrap_or_default();
    if !legacy.is_empty() {
        let mut encoded = BTreeMap::new();
        for (dataset, watermark) in &legacy {
            let cursor_json = watermark
                .cursor_json()
                .map_err(|e| fail("source.ingest", e.to_string()))?;
            encoded.insert(dataset.clone(), cursor_json);
        }
        host.seed_source_cursors(
            connection_id,
            &lease.lease_id,
            lease.generation,
            now_ms,
            &encoded,
        )
        .map_err(|error| {
            let (code, message) = error.code_and_message();
            fail(code, message)
        })?;
    }
    Ok(legacy)
}

fn legacy_connection_watermarks<E: SqlEngine>(
    host: &DevHost<E>,
    connection_id: &str,
) -> Option<BTreeMap<String, Watermark>> {
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

/// Build the atomic [`CursorAdvanceSpec`] for one complete accepted page.
pub(super) fn cursor_advance_spec(
    config: &ConnectionConfig,
    lease: &SourceLease,
    pending: &CursorAdvance,
    now_ms: u64,
) -> Result<CursorAdvanceSpec, Failure> {
    let expected_previous_cursor = pending
        .expected_previous
        .as_ref()
        .map(Watermark::cursor_json)
        .transpose()
        .map_err(|e| fail("source.ingest", e.to_string()))?;
    let next_cursor = pending
        .next
        .as_ref()
        .map(Watermark::cursor_json)
        .transpose()
        .map_err(|e| fail("source.ingest", e.to_string()))?;
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

/// Check (without releasing the lease yet) whether this run was fenced,
/// expired, unreadable, or superseded by another run's lease so `finish`
/// writes `source.Connection` only while still holding active ownership.
pub(super) fn is_lease_superseded<E: SqlEngine>(
    host: &DevHost<E>,
    connection_id: &str,
    lease: Option<&SourceLease>,
    error: Option<&(String, String)>,
    now_ms: u64,
) -> bool {
    if error.is_some_and(|(code, _)| code == "source.lease_fenced") {
        return true;
    }
    let Some(held) = lease else {
        return false;
    };
    match host.source_lease(connection_id) {
        Err(_) => true,
        Ok(Some(current)) => {
            current.lease_id != held.lease_id
                || current.generation != held.generation
                || now_ms >= current.expires_at_ms
        }
        Ok(None) => false,
    }
}

/// Load any durably captured pages left behind in `_norn_source_pending` by an
/// earlier crash or store error so the next sync processes them before pulling
/// further pages.
pub(super) fn load_pending_captures<E: SqlEngine>(
    host: &DevHost<E>,
    connection_id: &str,
) -> Result<Vec<ActiveCapture>, Failure> {
    let entries = host
        .pending_source_captures(connection_id)
        .map_err(|r| fail("source.ingest", r.message))?;
    Ok(entries
        .into_iter()
        .map(|pending| ActiveCapture::Captured {
            pending: Box::new(pending),
            outcomes: BTreeMap::new(),
            advance: None,
        })
        .collect())
}

/// Seal every row of an accepted HTTP page and atomically capture the complete
/// page + `_norn_source_cursor` + `_norn_source_pending` in one SQLite
/// transaction under `lease`, or return a paced dispute-fallback plan when the
/// page spans an `OBS-LINK-DISPUTE-1` conflict or identity finding.
pub(super) fn capture_accepted_page<E: SqlEngine>(
    host: &DevHost<E>,
    config: &ConnectionConfig,
    lease: &SourceLease,
    profile: &PullProfile,
    accepted: AcceptedPage,
    step_rows: usize,
    now_ms: u64,
) -> Result<(Option<ActiveCapture>, CursorAdvance), Failure> {
    let recorded_at = norn_ir::provenance::UtcTimestamp(rfc3339(now_ms));
    let batch = accepted
        .rows
        .iter()
        .map(|row| {
            observation(profile, row, now_ms).map(|raw| {
                (
                    raw,
                    MappingInput {
                        mapping: config.mapping.clone(),
                        dataset: row.dataset.clone(),
                        connector_instance: config.instance.clone(),
                        partition: row.dataset.clone(),
                        recorded_at: recorded_at.clone(),
                    },
                )
            })
        })
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| fail("source.seal", e))?;
    let spec = cursor_advance_spec(config, lease, &accepted.advance, now_ms)?;
    let plan = host
        .capture_source_page_planned(&batch, &spec, step_rows.clamp(1, crate::host::PAGE_ROWS))
        .map_err(|error| {
            let (code, message) = error.code_and_message();
            fail(code, message)
        })?;
    let advance = accepted.advance.clone();
    match plan {
        SourceCapturePlan::Captured(pending) => {
            if pending.pages.is_empty() && pending.records.is_empty() {
                return Ok((None, accepted.advance));
            }
            Ok((
                Some(ActiveCapture::Captured {
                    pending: Box::new(pending),
                    outcomes: BTreeMap::new(),
                    advance: Some(accepted.advance),
                }),
                advance,
            ))
        }
        SourceCapturePlan::DisputeFallback(state) => Ok((
            Some(ActiveCapture::DisputeFallback {
                state,
                advance: accepted.advance,
            }),
            advance,
        )),
    }
}
