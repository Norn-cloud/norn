//! SOURCE-CAPTURE-1 helpers for durable source cursors, legacy upgrade
//! seeding, complete-page capture specs, and fenced-run report guards.

use std::collections::BTreeMap;

use norn_cell::MappingOutcome;
use norn_cell::engine::SqlEngine;
use norn_cell::source_cursor::{
    CursorAdvanceSpec, PendingSourceCapture, SourceCursorRecord, SourceLease,
};
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

/// Load the connector's `norn.pull/1` profile directly from the retained
/// canonical JSON text (preserving >53-bit decimal `initial.id` values), bind
/// the mapping's `norn.feed/1` declaration, and return both the profile and
/// the connector's parsed payload for auth resolution.
pub(super) fn load_connector_profile<E: SqlEngine>(
    host: &DevHost<E>,
    config: &ConnectionConfig,
) -> Result<(PullProfile, serde_json::Value, String), Failure> {
    #[derive(serde::Deserialize)]
    struct RawArtifact<'a> {
        #[serde(rename = "artifactId")]
        artifact_id: String,
        #[serde(borrow)]
        payload: &'a serde_json::value::RawValue,
    }
    let active = host
        .active_definitions()
        .map_err(|refusal| fail("source.connector_missing", refusal.message))?
        .ok_or_else(|| {
            fail(
                "source.connector_missing",
                format!("{} is not installed", config.connector),
            )
        })?;
    let set: Vec<RawArtifact<'_>> = serde_json::from_str(&active.canonical_set)
        .map_err(|e| fail("source.connector_missing", e.to_string()))?;
    let raw_payload = set
        .into_iter()
        .find(|artifact| artifact.artifact_id == config.connector)
        .ok_or_else(|| {
            fail(
                "source.connector_missing",
                format!("{} is not installed", config.connector),
            )
        })?
        .payload
        .get();
    let mut profile = PullProfile::from_connector_json(raw_payload)
        .map_err(|e| fail("source.profile_invalid", e.to_string()))?
        .ok_or_else(|| {
            fail(
                "source.profile_missing",
                format!("{} declares no norn.pull/1 profile", config.connector),
            )
        })?;
    let artifacts: Vec<norn_ir::ArtifactEnvelope> = serde_json::from_str(&active.canonical_set)
        .map_err(|e| fail("source.profile_invalid", e.to_string()))?;
    let expected_binding = if let Some(mapping) = artifacts.iter().find(|artifact| {
        artifact.kind() == norn_ir::ArtifactKind::Mapping && artifact.artifact_id == config.mapping
    }) {
        let mapping_payload = serde_json::to_value(mapping.payload.to_json_value())
            .map_err(|e| fail("source.profile_invalid", e.to_string()))?;
        crate::pull_feed::bind_mapping_feed_declaration(&mut profile, &mapping_payload)
            .map_err(|e| fail("source.profile_invalid", e.to_string()))?;
        norn_cell::compute::mapping_binding_key(mapping)
            .map_err(|e| fail("source.profile_invalid", e.to_string()))?
    } else {
        config.mapping.clone()
    };
    // SYNC-PAGE-1: the host's page size, when it sets one.
    if let Some(rows) = config.page_size {
        for feed in &mut profile.feeds {
            match &mut feed.paging {
                crate::pull::Paging::Keyset { size } | crate::pull::Paging::Cursor { size, .. } => {
                    *size = rows;
                }
            }
        }
    }
    let payload: serde_json::Value = serde_json::from_str(raw_payload)
        .map_err(|e| fail("source.profile_invalid", e.to_string()))?;
    Ok((profile, payload, expected_binding))
}

pub(super) fn running_summary(run_id: &str) -> super::RunSummary {
    super::RunSummary {
        run: run_id.to_owned(),
        outcome: "running",
        rows_read: 0,
        rows_landed: 0,
        rows_quarantined: 0,
        quarantine_reasons: BTreeMap::new(),
        error: None,
    }
}

pub(super) fn mark_fenced(summary: &mut super::RunSummary, fenced: Failure) {
    if summary.error.is_none() {
        summary.outcome = "failed";
        summary.error = Some(fenced);
    }
}

/// Parsed watermarks paired with their exact stored `_norn_source_cursor` JSON
/// strings, plus whether the run must drain prior-mapping pending captures
/// before resetting cursors for a rebound mapping.
pub(super) type LoadedWatermarks = (BTreeMap<String, Watermark>, BTreeMap<String, String>, bool);

fn decode_cursors(
    cursors: BTreeMap<String, SourceCursorRecord>,
) -> (BTreeMap<String, Watermark>, BTreeMap<String, String>) {
    let mut parsed = BTreeMap::new();
    let mut raw_cursors = BTreeMap::new();
    for (dataset, record) in cursors {
        if let Ok(watermark) = serde_json::from_str::<Watermark>(&record.cursor_json) {
            parsed.insert(dataset.clone(), watermark);
            raw_cursors.insert(dataset, record.cursor_json);
        }
    }
    (parsed, raw_cursors)
}

fn stored_connection_mapping<E: SqlEngine>(
    host: &DevHost<E>,
    connection_id: &str,
) -> Option<String> {
    host.host_object(CONNECTION_TYPE, connection_id)
        .ok()
        .flatten()
        .and_then(|object| {
            object
                .get("mapping_id")
                .and_then(RuntimeValue::as_str)
                .filter(|m| !m.is_empty())
                .map(str::to_owned)
        })
}

fn bind_mapping<E: SqlEngine>(
    host: &DevHost<E>,
    connection_id: &str,
    mapping_id: &str,
    lease: &SourceLease,
    now_ms: u64,
) -> Result<(), Failure> {
    host.bind_source_mapping(
        connection_id,
        mapping_id,
        &lease.lease_id,
        lease.generation,
        now_ms,
    )
    .map_err(|error| {
        let (code, message) = error.code_and_message();
        fail(code, message)
    })
}

/// Read the connection's durably committed watermarks alongside their exact
/// stored cursor JSON strings. Durable cursor reuse requires a proven mapping
/// match in `_norn_source_binding` (or, on first upgrade before a binding row
/// exists, an explicit matching `source.Connection.mapping_id`). When a binding
/// differs or is unproven:
/// - if pending captures remain, drain them first (`drain_only = true`);
/// - otherwise atomically clear old cursors/replays and bind `config.mapping`
///   so the rebound mapping pulls from `feed.initial`.
pub(super) fn load_and_seed_watermarks<E: SqlEngine>(
    host: &DevHost<E>,
    config: &ConnectionConfig,
    lease: &SourceLease,
    expected_binding: &str,
    known: Option<BTreeMap<String, Watermark>>,
    has_pending: bool,
    now_ms: u64,
) -> Result<LoadedWatermarks, Failure> {
    let connection_id = config.id.as_str();
    let bound_mapping = host
        .source_mapping_binding(connection_id)
        .map_err(|r| fail("source.ingest", r.message))?;
    let cursors = host
        .source_cursors(connection_id)
        .map_err(|r| fail("source.ingest", r.message))?;
    let stored_mapping = stored_connection_mapping(host, connection_id);
    let legacy_revision_matches = bound_mapping.is_none()
        && stored_mapping.as_deref() == Some(config.mapping.as_str())
        && host
            .connection_matches_mapping_binding(connection_id, &config.mapping, expected_binding)
            .unwrap_or(false);
    let proven_match = match bound_mapping.as_deref() {
        Some(bound) => bound == expected_binding,
        None => legacy_revision_matches,
    };
    let has_prior_state =
        !cursors.is_empty() || bound_mapping.is_some() || stored_mapping.is_some() || has_pending;
    if has_prior_state && !proven_match {
        if has_pending {
            let (parsed, raw_cursors) = decode_cursors(cursors);
            return Ok((parsed, raw_cursors, true));
        }
        host.reset_source_cursors(
            connection_id,
            expected_binding,
            &lease.lease_id,
            lease.generation,
            now_ms,
        )
        .map_err(|error| {
            let (code, message) = error.code_and_message();
            fail(code, message)
        })?;
        return Ok((BTreeMap::new(), BTreeMap::new(), false));
    }

    if !cursors.is_empty() {
        if bound_mapping.is_none() {
            bind_mapping(host, connection_id, expected_binding, lease, now_ms)?;
        }
        let (parsed, raw_cursors) = decode_cursors(cursors);
        return Ok((parsed, raw_cursors, false));
    }

    if bound_mapping.is_some() {
        return Ok((BTreeMap::new(), BTreeMap::new(), false));
    }
    let legacy = if legacy_revision_matches {
        legacy_connection_watermarks(host, connection_id)
            .or(known)
            .unwrap_or_default()
    } else {
        known.unwrap_or_default()
    };
    bind_mapping(host, connection_id, expected_binding, lease, now_ms)?;
    let mut encoded = BTreeMap::new();
    if !legacy.is_empty() {
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
    Ok((legacy, encoded, false))
}

/// Build the `source.Connection` and `source.SyncRun` observation rows for
/// `sources::report`.
pub(super) fn report_rows<E: SqlEngine>(
    host: &DevHost<E>,
    config: &ConnectionConfig,
    run: &super::Run,
    summary: &super::RunSummary,
    (started, ended, watermark): (&str, Option<&str>, &str),
) -> Vec<(&'static str, String, RuntimeValue)> {
    let reasons = summary
        .quarantine_reasons
        .iter()
        .map(|(code, n)| format!("{code}={n}"))
        .collect::<Vec<_>>()
        .join("; ");
    let (error_code, error_message) = summary
        .error
        .as_ref()
        .map_or((None, None), |(c, m)| (Some(c.as_str()), Some(m.as_str())));
    let mut rows = Vec::new();
    if !run.drain_only {
        let status = if ended.is_none() {
            "syncing"
        } else {
            summary.outcome
        };
        let auth = if config.connector.is_empty() {
            "presigned_upload"
        } else {
            "api_key"
        };
        let mapping_id = reported_mapping_id(host, config, run);
        let connection = norn_ir::runtime_object! {
            "id" => config.id.as_str(), "system" => config.system.as_str(), "label" => config.label.as_str(),
            "target" => config.base_url.as_str(), "auth" => auth, "status" => status,
            "last_sync_started_at" => started, "last_sync_ended_at" => ended,
            "last_trigger" => run.trigger.as_str(), "watermark" => watermark,
            "rows_read" => summary.rows_read, "rows_landed" => summary.rows_landed,
            "rows_quarantined" => summary.rows_quarantined, "quarantine_reasons" => reasons.as_str(),
            "last_error_code" => error_code, "last_error_message" => error_message,
            "connector_id" => config.connector.as_str(),
            "source_schema_id" => super::source_schema_id(&mapping_id),
            "mapping_id" => mapping_id.as_str(),
        };
        rows.push(("connection", config.id.clone(), connection));
    }
    if ended.is_some() {
        rows.push((
            "sync_run",
            run.id.clone(),
            norn_ir::runtime_object! {
                "id" => run.id.as_str(), "connection_id" => config.id.as_str(), "trigger" => run.trigger.as_str(),
                "run_by" => run.actor.as_str(), "started_at" => started, "ended_at" => ended,
                "outcome" => summary.outcome, "watermark_from" => run.watermark_from.as_str(), "watermark_to" => watermark,
                "rows_read" => summary.rows_read, "rows_landed" => summary.rows_landed,
                "rows_quarantined" => summary.rows_quarantined, "quarantine_reasons" => reasons.as_str(),
                "error_code" => error_code, "error_message" => error_message,
            },
        ));
    }
    rows
}

/// Mapping ID to record on `source.Connection`: preserve the prior stored
/// `mapping_id` when a connector sync fails before cursor reset/load completes,
/// so a failed rebind reset cannot stamp the new mapping onto surviving cursors
/// from the old mapping.
fn reported_mapping_id<E: SqlEngine>(
    host: &DevHost<E>,
    config: &ConnectionConfig,
    run: &super::Run,
) -> String {
    if run.pull.is_none()
        && !config.connector.is_empty()
        && let Some(existing) = stored_connection_mapping(host, &config.id)
    {
        return existing;
    }
    config.mapping.clone()
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

/// Build the atomic [`CursorAdvanceSpec`] for one complete accepted page,
/// preserving exact stored `expected_previous_cursor` bytes when present.
pub(super) fn cursor_advance_spec(
    config: &ConnectionConfig,
    lease: &SourceLease,
    pending: &CursorAdvance,
    committed_cursor_json: &BTreeMap<String, String>,
    now_ms: u64,
) -> Result<CursorAdvanceSpec, Failure> {
    let expected_previous_cursor = match &pending.expected_previous {
        Some(wm) => Some(match committed_cursor_json.get(&pending.dataset) {
            Some(raw) => raw.clone(),
            None => wm
                .cursor_json()
                .map_err(|e| fail("source.ingest", e.to_string()))?,
        }),
        None => None,
    };
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
/// expired, unreadable, superseded by another run's lease, or invalidated by a
/// concurrent mapping revision change so `finish` writes `source.Connection`
/// only while still holding active ownership for the same mapping revision.
pub(super) fn is_lease_superseded<E: SqlEngine>(
    host: &DevHost<E>,
    config: &ConnectionConfig,
    run: &super::Run,
    error: Option<&(String, String)>,
    now_ms: u64,
) -> bool {
    if error.is_some_and(|(code, _)| code == "source.lease_fenced") {
        return true;
    }
    if let Some(expected) = run.mapping_binding.as_deref() {
        let current = host
            .active_mapping_binding(&config.mapping)
            .ok()
            .flatten()
            .unwrap_or_else(|| config.mapping.clone());
        if current != expected {
            return true;
        }
    }
    let Some(held) = run.lease.as_ref() else {
        return false;
    };
    match host.source_lease(&config.id) {
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

/// Per-page capture context passed from `sources::reply` into [`capture_accepted_page`].
pub(super) struct CaptureTurn<'a> {
    pub(super) lease: &'a SourceLease,
    pub(super) mapping_binding: Option<&'a str>,
    pub(super) committed_cursor_json: &'a mut BTreeMap<String, String>,
    pub(super) step_rows: usize,
    pub(super) now_ms: u64,
}

/// Seal every row of an accepted HTTP page and atomically capture the complete
/// page + `_norn_source_cursor` + `_norn_source_pending` in one SQLite
/// transaction under `turn.lease`, or return a paced dispute-fallback plan when
/// the page spans an `OBS-LINK-DISPUTE-1` conflict or identity finding.
pub(super) fn capture_accepted_page<E: SqlEngine>(
    host: &DevHost<E>,
    config: &ConnectionConfig,
    profile: &PullProfile,
    accepted: AcceptedPage,
    turn: &mut CaptureTurn<'_>,
) -> Result<(Option<ActiveCapture>, CursorAdvance), Failure> {
    let recorded_at = norn_ir::provenance::UtcTimestamp(rfc3339(turn.now_ms));
    let batch = accepted
        .rows
        .iter()
        .map(|row| {
            observation(profile, row, turn.now_ms).map(|raw| {
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
    let spec = cursor_advance_spec(
        config,
        turn.lease,
        &accepted.advance,
        turn.committed_cursor_json,
        turn.now_ms,
    )?;
    let expected_binding = turn
        .mapping_binding
        .map(|binding| (config.mapping.as_str(), binding));
    let plan = host
        .capture_source_page_planned_bound(
            &batch,
            &spec,
            turn.step_rows.clamp(1, crate::host::PAGE_ROWS),
            expected_binding,
        )
        .map_err(|error| {
            let (code, message) = error.code_and_message();
            fail(code, message)
        })?;
    let advance = accepted.advance.clone();
    match plan {
        SourceCapturePlan::Captured(pending) => {
            if let Some(next_raw) = spec.next_cursor {
                turn.committed_cursor_json
                    .insert(advance.dataset.clone(), next_raw);
            }
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
