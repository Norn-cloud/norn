//! SOURCE-CAPTURE-1 helpers for durable source cursors, legacy upgrade
//! seeding, complete-page capture specs, and fenced-run report guards.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};

use norn_cell::MappingOutcome;
use norn_cell::engine::SqlEngine;
use norn_cell::source_cursor::{
    CursorAdvanceSpec, PendingSourceCapture, SourceCursorRecord, SourceLease,
};
use norn_ir::RuntimeValue;

use super::{
    CELL_INSTANCE, CONNECTION_TYPE, ConnectionConfig, Failure, REPORT_MAPPING, RUN_LEASE_MS,
    connect_profile, fail, rfc3339,
};
use crate::host::{DevHost, DisputeFallbackState, SourceCapturePlan};
use crate::pull::{AcceptedPage, CursorAdvance, PullProfile, Watermark, observation};
use norn_cell::mapping_bundle::MappingInput;

/// Generate a distinct worker lease identity per claim so concurrent `Sources`
/// instances claiming at the exact same millisecond are mutually fenced under
/// `PullCursorAtomicity::acquireLease` (`current.lease_id != lease_id`).
pub(super) fn next_lease_id(base: &str) -> String {
    static NEXT_LEASE_SEQ: AtomicU64 = AtomicU64::new(1);
    let seq = NEXT_LEASE_SEQ.fetch_add(1, Ordering::Relaxed);
    #[cfg(not(target_arch = "wasm32"))]
    {
        use std::hash::BuildHasher;

        let pid = std::process::id();
        let salt = std::collections::hash_map::RandomState::new().hash_one((pid, seq));
        format!("{base}/l{seq}-{pid}-{salt:016x}")
    }
    #[cfg(target_arch = "wasm32")]
    {
        format!("{base}/l{seq}")
    }
}

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

pub(super) fn drain_fence(
    lease: Option<&norn_cell::source_cursor::SourceLease>,
    now_ms: u64,
) -> norn_cell::source_cursor::SourceReportFence<'_> {
    norn_cell::source_cursor::SourceReportFence {
        lease: lease.map(|lease| {
            (
                lease.connection_id.as_str(),
                lease.lease_id.as_str(),
                lease.generation,
                now_ms,
            )
        }),
        expected_binding: None,
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
    mapping: (&str, &str),
    lease: &SourceLease,
    now_ms: u64,
    seed_cursors: &BTreeMap<String, String>,
) -> Result<(), Failure> {
    host.bind_source_mapping(connection_id, mapping, lease, now_ms, seed_cursors)
        .map_err(|error| {
            let (code, message) = error.code_and_message();
            fail(code, message)
        })
}

/// Read the connection's durably committed watermarks alongside their exact
/// stored cursor JSON strings. Durable cursor reuse requires a proven mapping
/// revision match in `_norn_source_binding` (or, on first upgrade before a
/// binding row exists, direct cursor-level proof against `expected_binding`).
/// When a binding differs or is unproven:
/// - if pending captures remain, drain them first (`drain_only = true`);
/// - otherwise atomically clear old cursors/replays and bind `expected_binding`
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
    let current_binding = host
        .active_mapping_binding(&config.mapping)
        .map_err(|r| fail("source.ingest", r.message))?
        .unwrap_or_else(|| config.mapping.clone());
    if current_binding != expected_binding {
        return Err(fail(
            "source.lease_fenced",
            "active mapping revision changed before sync started",
        ));
    }
    let connection_id = config.id.as_str();
    let mapping = (config.mapping.as_str(), expected_binding);
    let bound_mapping = host
        .source_mapping_binding(connection_id)
        .map_err(|r| fail("source.ingest", r.message))?;
    let cursors = host
        .source_cursors(connection_id)
        .map_err(|r| fail("source.ingest", r.message))?;
    let stored_mapping = stored_connection_mapping(host, connection_id);
    let legacy_revision_matches = bound_mapping.is_none()
        && host
            .legacy_cursors_match_mapping_binding(connection_id, &config.mapping, expected_binding)
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
        host.reset_source_cursors(connection_id, mapping, lease, now_ms)
            .map_err(|error| {
                let (code, message) = error.code_and_message();
                fail(code, message)
            })?;
        return Ok((BTreeMap::new(), BTreeMap::new(), false));
    }

    if !cursors.is_empty() {
        if bound_mapping.is_none() {
            bind_mapping(
                host,
                connection_id,
                mapping,
                lease,
                now_ms,
                &BTreeMap::new(),
            )?;
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
    let mut encoded = BTreeMap::new();
    for (dataset, watermark) in &legacy {
        let cursor_json = watermark
            .cursor_json()
            .map_err(|e| fail("source.ingest", e.to_string()))?;
        encoded.insert(dataset.clone(), cursor_json);
    }
    bind_mapping(host, connection_id, mapping, lease, now_ms, &encoded)?;
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
    // Emit `sync_run` before `connection` so a fault or quarantine on the run
    // record never leaves a final `ok` heartbeat published on `source.Connection`.
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
        return !config.connector.is_empty();
    };
    match host.source_lease(&config.id) {
        Err(_) | Ok(None) => true,
        Ok(Some(current)) => {
            current.lease_id != held.lease_id
                || current.generation != held.generation
                || now_ms >= current.expires_at_ms
        }
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

/// The object type of a completed upload's durable record.
pub const UPLOAD_TYPE: &str = "source.Upload";

/// UPLOAD-RECORD-1: a completed upload as the cell keeps it, written through
/// its own `norn-connect` mapping when `:complete` reads the object back, so
/// it outlives the host. Every value is what the cell read or decided,
/// never what a client claimed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeptUpload {
    /// The sealed upload id.
    pub upload_id: String,
    /// The store the object is in.
    pub store: crate::uploads::Store,
    /// The object's URI.
    pub uri: String,
    /// The SHA-256 of the bytes the cell read.
    pub sha256: String,
    /// Their size.
    pub size: u64,
    /// The content type the upload began with.
    pub content_type: String,
    /// The file name the upload began with.
    pub file_name: String,
    /// Whether the bytes are CSV the cell profiled.
    pub csv: bool,
    /// The profile's columns.
    pub columns: Vec<crate::uploads::Column>,
    /// Who completed it.
    pub owner: String,
    /// When, as RFC 3339.
    pub completed_at: String,
}

/// The record key of the upload `upload_id`: its SHA-256.
#[must_use]
pub fn kept_upload_key(upload_id: &str) -> String {
    crate::uploads::sha256_hex(upload_id.as_bytes())
}

/// Keep `upload` in the cell (UPLOAD-RECORD-1). The same bytes kept again
/// are already kept; new bytes for the same upload are its next version.
/// A cell whose definitions predate `source.Upload` first gets the cell's
/// own sources pack through the governed install ([`ensure_upload_records`]).
///
/// # Errors
/// The pack could not be installed, the cell refused the record, or the
/// write failed.
pub fn keep_upload<E: SqlEngine>(
    host: &DevHost<E>,
    upload: &KeptUpload,
    now_ms: u64,
) -> Result<(), String> {
    ensure_upload_records(host, now_ms)?;
    let key = kept_upload_key(&upload.upload_id);
    let columns = serde_json::to_string(&upload.columns).map_err(|e| e.to_string())?;
    let store = match upload.store {
        crate::uploads::Store::Uploads => "uploads",
        crate::uploads::Store::Documents => "documents",
    };
    let payload = norn_ir::runtime_object! {
        "id" => key.as_str(), "upload_id" => upload.upload_id.as_str(), "store" => store,
        "uri" => upload.uri.as_str(), "sha256" => upload.sha256.as_str(), "size" => upload.size,
        "content_type" => upload.content_type.as_str(), "file_name" => upload.file_name.as_str(),
        "format" => if upload.csv { "csv" } else { "other" }, "columns" => columns.as_str(),
        "owner" => upload.owner.as_str(), "completed_at" => upload.completed_at.as_str(),
    };
    let profile = connect_profile();
    let row = crate::pull::Row {
        dataset: "upload".to_owned(),
        source_id: key,
        version: upload.sha256.clone(),
        payload,
    };
    let raw = observation(&profile, &row, now_ms)?;
    let input = MappingInput {
        mapping: REPORT_MAPPING.to_owned(),
        dataset: "upload".to_owned(),
        connector_instance: CELL_INSTANCE.to_owned(),
        partition: "upload".to_owned(),
        recorded_at: norn_ir::provenance::UtcTimestamp(rfc3339(now_ms)),
    };
    let ingested = host
        .ingest(&[(raw, input)])
        .map_err(|refusal| refusal.message)?;
    match ingested.first().map(|i| &i.outcome) {
        Some(MappingOutcome::Landed | MappingOutcome::AlreadyLanded) => Ok(()),
        Some(MappingOutcome::Quarantined(reasons)) => Err(format!(
            "the cell refused the upload record: {}",
            reasons
                .iter()
                .map(|r| format!("{}: {}", r.code, r.reason))
                .collect::<Vec<_>>()
                .join(", ")
        )),
        None => Err("the cell captured no upload record".to_owned()),
    }
}

/// The cell's own sources pack: connections, sync runs and upload records,
/// and the mapping that lands the cell's reports as them.
const SOURCES_PACK: [&str; 6] = [
    include_str!("../../../../registry/packs/sources/artifacts/source.Synced.interface.json"),
    include_str!("../../../../registry/packs/sources/artifacts/source.Connection.entity.json"),
    include_str!("../../../../registry/packs/sources/artifacts/source.SyncRun.entity.json"),
    include_str!("../../../../registry/packs/sources/artifacts/source.Upload.entity.json"),
    include_str!("../../../../registry/packs/sources/artifacts/norn.connect.source.json"),
    include_str!("../../../../registry/packs/sources/artifacts/norn.connect.mapping.json"),
];

/// UPLOAD-RECORD-1 for a cell provisioned before `source.Upload`: install the
/// cell's own sources pack as the next governed definitions version (the
/// shared install path, DEFS-REOPEN-1), once. `Ok(true)` when it installed.
///
/// # Errors
/// The pack is refused or the install fails; the upload is then refused,
/// never completed without its record.
pub fn ensure_upload_records<E: SqlEngine>(
    host: &DevHost<E>,
    installed_at: u64,
) -> Result<bool, String> {
    if host
        .serves_object_type(UPLOAD_TYPE)
        .map_err(|refusal| refusal.message)?
    {
        return Ok(false);
    }
    let pack = SOURCES_PACK
        .iter()
        .map(|text| serde_json::from_str::<norn_ir::ArtifactEnvelope>(text))
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| format!("the sources pack: {e}"))?;
    host.install_artifacts(&pack, installed_at)
        .map_err(|reason| format!("the sources pack was not installed: {reason}"))?;
    Ok(true)
}

/// The kept record of the upload `upload_id`, if the cell completed it.
///
/// # Errors
/// A store failure, or a kept record the cell cannot read back.
pub fn kept_upload<E: SqlEngine>(
    host: &DevHost<E>,
    upload_id: &str,
) -> Result<Option<KeptUpload>, String> {
    if !host
        .serves_object_type(UPLOAD_TYPE)
        .map_err(|refusal| refusal.message)?
    {
        return Ok(None);
    }
    let Some(object) = host
        .host_object(UPLOAD_TYPE, &kept_upload_key(upload_id))
        .map_err(|refusal| refusal.message)?
    else {
        return Ok(None);
    };
    let text = |name: &str| {
        object
            .get(name)
            .and_then(RuntimeValue::as_str)
            .map(str::to_owned)
            .ok_or_else(|| format!("a kept upload has no {name}"))
    };
    if text("upload_id")? != upload_id {
        return Ok(None);
    }
    let store = match text("store")?.as_str() {
        "documents" => crate::uploads::Store::Documents,
        _ => crate::uploads::Store::Uploads,
    };
    Ok(Some(KeptUpload {
        upload_id: upload_id.to_owned(),
        store,
        uri: text("uri")?,
        sha256: text("sha256")?,
        size: object
            .get("size")
            .and_then(RuntimeValue::as_i64)
            .and_then(|n| u64::try_from(n).ok())
            .ok_or("a kept upload has no size")?,
        content_type: text("content_type")?,
        file_name: text("file_name")?,
        csv: text("format")? == "csv",
        columns: serde_json::from_str(&text("columns")?).map_err(|e| e.to_string())?,
        owner: text("owner")?,
        completed_at: text("completed_at").unwrap_or_default(),
    }))
}
