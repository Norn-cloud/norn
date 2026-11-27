//! QUERY-FRESH-1 (B6): model-derived required external sources and live or
//! historical connector freshness snapshots.
//!
//! A question over a native-only type has an empty [`RequiredSources`] set and
//! keeps native-only `Exact` behavior. When one or more installed `Mapping`
//! artifacts target the question's grain (or its subtypes/supertypes or linked
//! time target), [`CellStore::required_sources_for_plan_at`] derives every
//! required `(source, dataset)` pair from the definitions and `source.Connection`
//! objects active at that frontier (never reading live connections during
//! historical evaluation). [`evaluate_freshness`] fails closed to
//! [`Freshness::Incomplete`] whenever any required `(source, dataset)` is
//! missing from [`QueryContext::source_cursors`], lacks a matching complete
//! [`Watermark`] with heartbeat, or has an empty cursor set.

use std::collections::{BTreeMap, BTreeSet};

use norn_checker::metric::compute::QuestionPlan;
use norn_ir::mapping::MappingDocument;
use norn_ir::metric::FreshnessPolicy;
use norn_ir::{ArtifactEnvelope, ArtifactKind, EntityRoot, RuntimeValue};
use norn_kernel::LogicalTime;

use super::{AsOfReader, Freshness, Frontier, QueryContext, Watermark};
use crate::engine::SqlEngine;
use crate::reactive::{BudgetedRead, DecodeBudget, ReactiveLimits};
use crate::{CellError, CellStore, KnowledgeCutoff, MAIN_WORLD, ObjectRecord};

/// Internal host-written sync-report mapping; never an external pull feed.
const HOST_REPORT_MAPPING: &str = "norn.connect.mapping";
/// Canonical connection object type written by the sources pack.
const CONNECTION_TYPE: &str = "source.Connection";
/// Page size for scanning live `source.Connection` objects.
const CONNECTION_PAGE: usize = 256;
/// Bound for scanning historical `source.Connection` objects at a cutoff.
const MAX_HISTORICAL_CONNECTIONS: usize = 4_096;

/// Model-derived external source dependencies for a [`QuestionPlan`]:
/// `source_id -> required dataset/partition names`.
///
/// Empty for native-only questions. Non-empty when installed mappings target
/// the question's grain or linked time target: missing or empty
/// [`QueryContext::source_cursors`] then yields [`Freshness::Incomplete`],
/// never [`Freshness::Exact`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RequiredSources(pub BTreeMap<String, BTreeSet<String>>);

impl RequiredSources {
    /// Whether this question has no external source dependencies.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

/// Durable connector cursors, watermarks, and latest healthy heartbeat derived
/// from `_norn_source_cursor`, `_norn_source_pending`, and `source.Connection`
/// for one [`QuestionPlan`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SourceFreshnessSnapshot {
    /// Committed cursors for required `(source, dataset)` pairs.
    pub source_cursors: Frontier,
    /// Observed watermarks, heartbeats, and completion flags for required pairs.
    pub watermarks: BTreeMap<String, BTreeMap<String, Watermark>>,
    /// Highest heartbeat timestamp (ms) observed across healthy required sources.
    pub max_heartbeat_ms: u64,
}

/// Evaluate `(Freshness, complete)` for `plan` and `context` against
/// model-derived `required` external sources and `has_exclusions`.
#[must_use]
pub(crate) fn evaluate_freshness(
    plan: &QuestionPlan,
    context: &QueryContext,
    required: &RequiredSources,
    has_exclusions: bool,
) -> (Freshness, bool) {
    let mut complete = !has_exclusions;
    let (mut stale, mut aged) = (false, false);
    let age = match plan.question().freshness {
        FreshnessPolicy::Live { max_age_ms } | FreshnessPolicy::Delayed { max_age_ms } => {
            Some(max_age_ms.0)
        }
        FreshnessPolicy::AsOf { .. } => None,
    };
    for (source, datasets) in &required.0 {
        complete &= !datasets.is_empty();
        let Some(provided) = context.source_cursors.get(source) else {
            complete = false;
            continue;
        };
        for dataset in datasets {
            if !provided.contains_key(dataset) {
                complete = false;
            }
        }
    }
    for (source, partitions) in &context.source_cursors {
        complete &= !partitions.is_empty();
        for (partition, consumed) in partitions {
            match context
                .watermarks
                .get(source)
                .and_then(|w| w.get(partition))
            {
                Some(w) => {
                    complete &= w.complete && &w.observed == consumed && w.heartbeat_ms.is_some();
                    let lag = w.heartbeat_ms.and_then(|at| context.now_ms.checked_sub(at));
                    stale |= lag.is_none_or(|lag| age.is_some_and(|age| lag > age));
                    aged |= age.is_some() && lag.is_some_and(|lag| lag > 0);
                }
                None => complete = false,
            }
        }
    }
    let freshness = if !complete {
        Freshness::Incomplete
    } else if stale {
        Freshness::Stale
    } else if aged {
        Freshness::Fresh
    } else {
        Freshness::Exact
    };
    (freshness, complete)
}

fn parse_rfc3339_ms(text: &str) -> Option<u64> {
    let (seconds, nanos) = norn_checker::transform::instant(text)?;
    let secs = u64::try_from(seconds).ok()?;
    secs.checked_mul(1_000)?
        .checked_add(u64::from(nanos.div_euclid(1_000_000)))
}

/// Strict validator for one connector cursor JSON value: only accepts
/// `{"version": "<string>", "id": <string|number>}` with no unknown fields,
/// rejecting `null`, booleans, arrays, objects, or malformed entries while
/// allowing valid empty-feed initial cursors (e.g. `{"version":"","id":""}`)
/// and preserving exact raw numeric `id` tokens matching `Watermark::cursor_json()`.
fn validate_cursor_json(raw: &str) -> Option<String> {
    #[derive(serde::Deserialize)]
    #[serde(rename_all = "camelCase", deny_unknown_fields)]
    struct StoredWatermark<'a> {
        version: String,
        #[serde(borrow)]
        id: &'a serde_json::value::RawValue,
    }
    let wm = serde_json::from_str::<StoredWatermark<'_>>(raw).ok()?;
    let id_value: serde_json::Value = serde_json::from_str(wm.id.get()).ok()?;
    if !matches!(
        id_value,
        serde_json::Value::String(_) | serde_json::Value::Number(_)
    ) {
        return None;
    }
    let encoded_version = serde_json::to_string(&wm.version).ok()?;
    Some(format!(
        r#"{{"version":{encoded_version},"id":{}}}"#,
        wm.id.get()
    ))
}

/// Strict parser for `source.Connection.watermark` JSON: only accepts a map of
/// `dataset -> {"version": "<nonblank>", "id": <string|number>}` validated by
/// [`validate_cursor_json`].
fn strict_connection_cursors(connection: &ObjectRecord) -> BTreeMap<String, String> {
    let Some(raw) = connection
        .values
        .get("watermark")
        .and_then(RuntimeValue::as_str)
    else {
        return BTreeMap::new();
    };
    let Ok(entries) = serde_json::from_str::<BTreeMap<String, &serde_json::value::RawValue>>(raw)
    else {
        return BTreeMap::new();
    };
    let mut out = BTreeMap::new();
    for (dataset, value) in entries {
        if let Some(canonical) = validate_cursor_json(value.get()) {
            out.insert(dataset, canonical);
        }
    }
    out
}

fn mapping_datasets_for_plan(
    model: &crate::Model,
    artifacts: &[ArtifactEnvelope],
    plan: &QuestionPlan,
) -> BTreeMap<String, BTreeSet<String>> {
    let mut entity_types = BTreeMap::new();
    for artifact in artifacts
        .iter()
        .filter(|artifact| artifact.kind() == ArtifactKind::Entity)
    {
        if let Ok(wire) = serde_json::to_value(artifact.payload.to_json_value())
            && let Ok(EntityRoot::V2(root)) = serde_json::from_value::<EntityRoot>(wire)
        {
            entity_types.insert(
                artifact.artifact_id.clone(),
                root.object_type.name.as_str().to_owned(),
            );
        }
    }
    let grain = plan.grain().as_str();
    let link_targets: Vec<&str> = plan
        .sums()
        .iter()
        .filter_map(|sum| sum.time_link.as_ref().map(|link| link.target_type.as_str()))
        .collect();
    let related = |a: &str, b: &str| model.is_a(a, b) || model.is_a(b, a);
    let touches_plan = |target_type: &str| {
        related(target_type, grain)
            || link_targets
                .iter()
                .any(|link_target| related(target_type, link_target))
    };

    let mut required: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for artifact in artifacts.iter().filter(|artifact| {
        artifact.kind() == ArtifactKind::Mapping && artifact.artifact_id != HOST_REPORT_MAPPING
    }) {
        let Ok(wire) = serde_json::to_value(artifact.payload.to_json_value()) else {
            continue;
        };
        let Ok(MappingDocument::SourceRecord(mapping)) =
            serde_json::from_value::<MappingDocument>(wire)
        else {
            continue;
        };
        for map in &mapping.maps {
            let Some(target_type) = entity_types.get(&map.target.entity) else {
                continue;
            };
            if touches_plan(target_type) {
                let dataset = map
                    .source
                    .nested_in
                    .as_ref()
                    .map_or(map.source.dataset.as_str(), |nested| {
                        nested.dataset.as_str()
                    });
                required
                    .entry(artifact.artifact_id.clone())
                    .or_default()
                    .insert(dataset.to_owned());
            }
        }
    }
    required
}

/// Disjoint required source bindings for one plan: real connections (each
/// pinned to its expected `mapping_id`) kept strictly separate from unconnected
/// fallback mappings so a real connection whose ID equals another mapping's ID
/// can never swallow or satisfy that unconnected mapping.
#[derive(Default)]
struct RequiredBindings {
    connected: BTreeMap<String, (String, BTreeSet<String>)>,
    unconnected: BTreeMap<String, BTreeSet<String>>,
}

impl RequiredBindings {
    fn is_empty(&self) -> bool {
        self.connected.is_empty() && self.unconnected.is_empty()
    }

    fn unconnected_keys(&self) -> BTreeMap<&str, String> {
        let mut used: BTreeSet<String> = self.connected.keys().cloned().collect();
        let mut out = BTreeMap::new();
        for mapping_id in self.unconnected.keys() {
            let mut key = mapping_id.clone();
            while !used.insert(key.clone()) {
                key = format!("unconnected:{key}");
            }
            out.insert(mapping_id.as_str(), key);
        }
        out
    }
}

fn assemble_required_bindings(
    mapping_datasets: BTreeMap<String, BTreeSet<String>>,
    connections: &[ObjectRecord],
    configured: &BTreeMap<String, String>,
) -> RequiredBindings {
    let mut by_mapping: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for (conn_id, mapping_id) in configured {
        by_mapping
            .entry(mapping_id.clone())
            .or_default()
            .insert(conn_id.clone());
    }
    for conn in connections {
        if configured.contains_key(&conn.id) {
            continue;
        }
        if let Some(mapping_id) = conn.values.get("mapping_id").and_then(RuntimeValue::as_str) {
            by_mapping
                .entry(mapping_id.to_owned())
                .or_default()
                .insert(conn.id.clone());
        }
    }
    let mut required = RequiredBindings::default();
    for (mapping_id, datasets) in mapping_datasets {
        if let Some(conn_ids) = by_mapping.get(&mapping_id) {
            for conn_id in conn_ids {
                required
                    .connected
                    .entry(conn_id.clone())
                    .or_insert_with(|| (mapping_id.clone(), BTreeSet::new()))
                    .1
                    .extend(datasets.iter().cloned());
            }
        } else {
            required
                .unconnected
                .entry(mapping_id)
                .or_default()
                .extend(datasets);
        }
    }
    required
}

fn bindings_to_required(bindings: &RequiredBindings) -> RequiredSources {
    let mut out: BTreeMap<String, BTreeSet<String>> = bindings
        .connected
        .iter()
        .map(|(source, (_, datasets))| (source.clone(), datasets.clone()))
        .collect();
    for (mapping_id, key) in bindings.unconnected_keys() {
        if let Some(datasets) = bindings.unconnected.get(mapping_id) {
            out.entry(key).or_default().extend(datasets.iter().cloned());
        }
    }
    RequiredSources(out)
}

impl<E: SqlEngine> CellStore<E> {
    fn has_connection_type(&self) -> bool {
        self.model()
            .object_types()
            .any(|object_type| object_type.name.as_str() == CONNECTION_TYPE)
    }

    fn live_connection_objects(&self, world: &str) -> Result<Vec<ObjectRecord>, CellError> {
        if !self.has_connection_type() {
            return Ok(Vec::new());
        }
        let reader = self.read();
        let read_world = |target_world: &str| -> Result<Vec<ObjectRecord>, CellError> {
            let mut out = Vec::new();
            let mut after: Option<String> = None;
            loop {
                let page = reader.list_after(
                    target_world,
                    CONNECTION_TYPE,
                    after.as_deref(),
                    CONNECTION_PAGE,
                )?;
                let count = page.len();
                if let Some(last) = page.last() {
                    after = Some(last.id.clone());
                }
                out.extend(page);
                if count < CONNECTION_PAGE {
                    break;
                }
            }
            Ok(out)
        };
        let mut objects = read_world(world)?;
        if objects.is_empty() && world != MAIN_WORLD {
            objects = read_world(MAIN_WORLD)?;
        }
        Ok(objects)
    }

    fn historical_connection_objects(
        &self,
        world: &str,
        cutoff: KnowledgeCutoff,
        now_ms: u64,
    ) -> Result<Vec<ObjectRecord>, CellError> {
        if !self.has_connection_type() || cutoff.frontier == 0 {
            return Ok(Vec::new());
        }
        let reader = self.read();
        let limits = ReactiveLimits::from_profile(&norn_ir::ResourceLimitProfile::norn_limits_v1());
        let mut budget = DecodeBudget::new(&limits);
        let mut as_of = AsOfReader::open(&reader, cutoff, now_ms, &mut budget)?;
        let read_world = |as_of: &mut AsOfReader<'_, '_, E>,
                          target_world: &str,
                          budget: &mut DecodeBudget|
         -> Result<Vec<ObjectRecord>, CellError> {
            let (ids, complete) = as_of.distinct_ids(
                target_world,
                CONNECTION_TYPE,
                MAX_HISTORICAL_CONNECTIONS,
                budget,
            )?;
            if !complete {
                return Err(super::refused("core.compute.bound_exceeded"));
            }
            let mut out = Vec::with_capacity(ids.len());
            for id in ids {
                match as_of.record(target_world, CONNECTION_TYPE, &id, budget)? {
                    BudgetedRead::Found(record) => out.push(record),
                    BudgetedRead::Missing => {}
                    BudgetedRead::Exhausted => {
                        return Err(super::refused("core.compute.bound_exceeded"));
                    }
                }
            }
            Ok(out)
        };
        let mut objects = read_world(&mut as_of, world, &mut budget)?;
        if objects.is_empty() && world != MAIN_WORLD {
            objects = read_world(&mut as_of, MAIN_WORLD, &mut budget)?;
        }
        Ok(objects)
    }

    /// Read one `source.Connection` record at live head (`historical == None`)
    /// or authenticated cutoff `Some((frontier, now_ms))` for `Sources` cursor
    /// disclosure authorization. Returns `Ok(None)` when absent or over budget.
    ///
    /// # Errors
    /// Store or historical-proof verification errors.
    pub fn connection_record_at(
        &self,
        world: &str,
        source_id: &str,
        historical: Option<(u64, u64)>,
    ) -> Result<Option<ObjectRecord>, CellError> {
        if !self.has_connection_type() {
            return Ok(None);
        }
        let reader = self.read();
        let Some((frontier, now_ms)) = historical else {
            if let Some(rec) = reader.object(world, CONNECTION_TYPE, source_id)? {
                return Ok(Some(rec));
            }
            return if world != MAIN_WORLD {
                reader.object(MAIN_WORLD, CONNECTION_TYPE, source_id)
            } else {
                Ok(None)
            };
        };
        if frontier == 0 {
            return Ok(None);
        }
        let limits = ReactiveLimits::from_profile(&norn_ir::ResourceLimitProfile::norn_limits_v1());
        let mut budget = DecodeBudget::new(&limits);
        let cutoff = KnowledgeCutoff {
            frontier,
            recorded_at: LogicalTime::from_millis(now_ms),
        };
        let mut as_of = AsOfReader::open(&reader, cutoff, now_ms, &mut budget)?;
        match as_of.record(world, CONNECTION_TYPE, source_id, &mut budget)? {
            BudgetedRead::Found(record) => return Ok(Some(record)),
            BudgetedRead::Exhausted => return Ok(None),
            BudgetedRead::Missing => {}
        }
        if world != MAIN_WORLD
            && let BudgetedRead::Found(record) =
                as_of.record(MAIN_WORLD, CONNECTION_TYPE, source_id, &mut budget)?
        {
            return Ok(Some(record));
        }
        Ok(None)
    }

    fn definitions_at(
        &self,
        cutoff: Option<KnowledgeCutoff>,
    ) -> Result<Option<crate::InstalledDefinitionSet>, CellError> {
        let reader = self.read();
        match cutoff {
            Some(point) => {
                let version = reader.definitions_version_at(point.frontier)?;
                match version {
                    Some(v) => reader.installed_definitions(Some(v)),
                    None => Ok(None),
                }
            }
            None => reader.installed_definitions(None),
        }
    }

    /// Derive every external `(source, dataset)` required to answer `plan`
    /// completely from the cell's active definitions and live `source.Connection`
    /// objects.
    ///
    /// # Errors
    /// Active definitions or connection objects cannot be read.
    pub fn required_sources_for_plan(
        &self,
        world: &str,
        plan: &QuestionPlan,
    ) -> Result<RequiredSources, CellError> {
        self.required_sources_for_plan_at(world, plan, None, 0, &BTreeMap::new())
    }

    fn required_bindings_at(
        &self,
        world: &str,
        plan: &QuestionPlan,
        cutoff: Option<KnowledgeCutoff>,
        now_ms: u64,
        configured: &BTreeMap<String, String>,
    ) -> Result<(RequiredBindings, Vec<ObjectRecord>), CellError> {
        let Some(active) = self.definitions_at(cutoff)? else {
            return Ok((RequiredBindings::default(), Vec::new()));
        };
        let artifacts: Vec<ArtifactEnvelope> = serde_json::from_str(&active.canonical_set)?;
        let mapping_datasets = mapping_datasets_for_plan(self.model(), &artifacts, plan);
        if mapping_datasets.is_empty() {
            return Ok((RequiredBindings::default(), Vec::new()));
        }
        let connections = match cutoff {
            Some(point) => self.historical_connection_objects(world, point, now_ms)?,
            None => self.live_connection_objects(world)?,
        };
        let bindings = assemble_required_bindings(mapping_datasets, &connections, configured);
        Ok((bindings, connections))
    }

    /// Derive required external `(source, dataset)` pairs at either live state
    /// (`cutoff == None`) or an authenticated historical cutoff (`cutoff == Some`),
    /// unioned with any host-configured `(connection_id -> mapping_id)` bindings.
    ///
    /// # Errors
    /// Definitions or connection objects cannot be read.
    pub fn required_sources_for_plan_at(
        &self,
        world: &str,
        plan: &QuestionPlan,
        cutoff: Option<KnowledgeCutoff>,
        now_ms: u64,
        configured: &BTreeMap<String, String>,
    ) -> Result<RequiredSources, CellError> {
        let (bindings, _) = self.required_bindings_at(world, plan, cutoff, now_ms, configured)?;
        Ok(bindings_to_required(&bindings))
    }

    /// Build the live connector frontier (`source_cursors` and `watermarks`)
    /// for `plan`'s required external sources from `_norn_source_cursor`,
    /// `_norn_source_pending`, and `source.Connection`.
    ///
    /// # Errors
    /// Active definitions, connections, cursors, or pending captures cannot be read.
    pub fn source_freshness_snapshot(
        &self,
        world: &str,
        plan: &QuestionPlan,
    ) -> Result<SourceFreshnessSnapshot, CellError> {
        self.source_freshness_snapshot_with_configured(world, plan, &BTreeMap::new())
    }

    /// Like [`Self::source_freshness_snapshot`], also requiring any
    /// host-configured `(connection_id -> mapping_id)` connections even before
    /// their first sync report.
    ///
    /// # Errors
    /// Active definitions, connections, cursors, or pending captures cannot be read.
    pub fn source_freshness_snapshot_with_configured(
        &self,
        world: &str,
        plan: &QuestionPlan,
        configured: &BTreeMap<String, String>,
    ) -> Result<SourceFreshnessSnapshot, CellError> {
        let (bindings, conn_list) = self.required_bindings_at(world, plan, None, 0, configured)?;
        if bindings.is_empty() {
            return Ok(SourceFreshnessSnapshot::default());
        }
        let connections: BTreeMap<String, ObjectRecord> = conn_list
            .into_iter()
            .map(|record| (record.id.clone(), record))
            .collect();
        self.build_live_snapshot(&bindings, &connections)
    }

    /// Reconstruct the connector frontier (`source_cursors` and `watermarks`)
    /// strictly as of commit `frontier` and clock `now_ms`, using historical
    /// `source.Connection` snapshots so pinned `:trace` stays valid even after
    /// later syncs advance live cursors.
    ///
    /// # Errors
    /// Historical definitions or connections cannot be read or authenticated.
    pub fn source_freshness_snapshot_at(
        &self,
        world: &str,
        plan: &QuestionPlan,
        frontier: u64,
        now_ms: u64,
        configured: &BTreeMap<String, String>,
    ) -> Result<SourceFreshnessSnapshot, CellError> {
        let cutoff = KnowledgeCutoff {
            frontier,
            recorded_at: LogicalTime::from_millis(now_ms),
        };
        let (bindings, conn_list) =
            self.required_bindings_at(world, plan, Some(cutoff), now_ms, configured)?;
        if bindings.is_empty() {
            return Ok(SourceFreshnessSnapshot::default());
        }
        let connections: BTreeMap<String, ObjectRecord> = conn_list
            .into_iter()
            .map(|record| (record.id.clone(), record))
            .collect();
        let mut snapshot = SourceFreshnessSnapshot::default();
        for key in bindings.unconnected_keys().into_values() {
            snapshot.source_cursors.entry(key).or_default();
        }
        for (source_id, (expected_mapping, datasets)) in &bindings.connected {
            snapshot
                .source_cursors
                .entry(source_id.clone())
                .or_default();
            let Some(conn) = connections.get(source_id) else {
                continue;
            };
            let mapping_matches = conn.values.get("mapping_id").and_then(RuntimeValue::as_str)
                == Some(expected_mapping.as_str());
            let conn_healthy = mapping_matches
                && conn.values.get("status").and_then(RuntimeValue::as_str) == Some("ok");
            let heartbeat_ms = conn
                .values
                .get("last_sync_ended_at")
                .and_then(RuntimeValue::as_str)
                .and_then(parse_rfc3339_ms);
            let cursors = strict_connection_cursors(conn);
            for dataset in datasets {
                let Some(cursor_json) = cursors.get(dataset).cloned() else {
                    continue;
                };
                if conn_healthy && let Some(hb) = heartbeat_ms {
                    snapshot.max_heartbeat_ms = snapshot.max_heartbeat_ms.max(hb);
                }
                snapshot
                    .source_cursors
                    .entry(source_id.clone())
                    .or_default()
                    .insert(dataset.clone(), cursor_json.clone());
                snapshot
                    .watermarks
                    .entry(source_id.clone())
                    .or_default()
                    .insert(
                        dataset.clone(),
                        Watermark {
                            observed: cursor_json,
                            heartbeat_ms,
                            complete: conn_healthy,
                        },
                    );
            }
        }
        Ok(snapshot)
    }

    fn build_live_snapshot(
        &self,
        bindings: &RequiredBindings,
        connections: &BTreeMap<String, ObjectRecord>,
    ) -> Result<SourceFreshnessSnapshot, CellError> {
        let reader = self.read();
        let mut snapshot = SourceFreshnessSnapshot::default();
        for key in bindings.unconnected_keys().into_values() {
            snapshot.source_cursors.entry(key).or_default();
        }
        for (source_id, (expected_mapping, datasets)) in &bindings.connected {
            snapshot
                .source_cursors
                .entry(source_id.clone())
                .or_default();
            let conn = connections.get(source_id);
            let bound_mapping = reader.source_mapping_binding(source_id)?;
            let binding_matches = bound_mapping
                .as_deref()
                .is_none_or(|bound| bound == expected_mapping.as_str());
            let mapping_matches = binding_matches
                && conn
                    .and_then(|c| c.values.get("mapping_id"))
                    .and_then(RuntimeValue::as_str)
                    == Some(expected_mapping.as_str());
            let conn_healthy = mapping_matches
                && conn
                    .and_then(|c| c.values.get("status"))
                    .and_then(RuntimeValue::as_str)
                    == Some("ok");
            let conn_heartbeat_ms = conn
                .and_then(|c| c.values.get("last_sync_ended_at"))
                .and_then(RuntimeValue::as_str)
                .and_then(parse_rfc3339_ms);
            let cursors = reader.source_cursors(source_id)?;
            let conn_cursors = conn.map_or_else(BTreeMap::new, strict_connection_cursors);
            let pending = reader.pending_source_datasets(source_id)?;
            for dataset in datasets {
                let Some(cursor_json) = cursors
                    .get(dataset)
                    .and_then(|record| validate_cursor_json(&record.cursor_json))
                    .or_else(|| {
                        (cursors.is_empty() && bound_mapping.is_none())
                            .then(|| conn_cursors.get(dataset).cloned())
                            .flatten()
                    })
                else {
                    continue;
                };
                // A live answer reports `complete` (and thus may become `Exact`
                // and traceable) only when the durable `source.Connection` report
                // at the current frontier matches the expected mapping and
                // validated cursor and records a valid `last_sync_ended_at`
                // heartbeat, so pinned historical `:trace` reconstructs the exact
                // same frontier.
                let report_matches = conn_cursors.get(dataset) == Some(&cursor_json);
                let heartbeat_ms = conn_heartbeat_ms;
                let complete = conn_healthy
                    && !pending.contains(dataset)
                    && report_matches
                    && heartbeat_ms.is_some();
                if complete && let Some(hb) = heartbeat_ms {
                    snapshot.max_heartbeat_ms = snapshot.max_heartbeat_ms.max(hb);
                }
                snapshot
                    .source_cursors
                    .entry(source_id.clone())
                    .or_default()
                    .insert(dataset.clone(), cursor_json.clone());
                snapshot
                    .watermarks
                    .entry(source_id.clone())
                    .or_default()
                    .insert(
                        dataset.clone(),
                        Watermark {
                            observed: cursor_json,
                            heartbeat_ms,
                            complete,
                        },
                    );
            }
        }
        Ok(snapshot)
    }

    /// Persist `connection_id -> mapping_id` in `_norn_source_binding` under
    /// the active fencing lease so cursor reuse requires a proven mapping match
    /// even when `source.Connection` reports are absent or unreadable.
    ///
    /// # Errors
    /// [`CellError::SourceLeaseFenced`], [`CellError::OutOfRange`], or store errors.
    pub fn bind_source_mapping(
        &mut self,
        connection_id: &str,
        mapping_id: &str,
        lease_id: &str,
        generation: u64,
        now_ms: u64,
    ) -> Result<(), CellError> {
        use crate::engine::params;
        use crate::source_cursor::{SOURCE_BINDING, verify_active_lease};
        let gen_i64 =
            i64::try_from(generation).map_err(|_| CellError::OutOfRange("binding generation"))?;
        let now_i64 =
            i64::try_from(now_ms).map_err(|_| CellError::OutOfRange("binding updated_at_ms"))?;
        self.write_tx(|| {
            verify_active_lease(&self.conn, connection_id, lease_id, generation, now_ms)?;
            self.conn.execute(
                &format!(
                    "INSERT OR REPLACE INTO \"{SOURCE_BINDING}\" \
                     (\"connection_id\", \"mapping_id\", \"generation\", \"updated_at_ms\") \
                     VALUES (?1, ?2, ?3, ?4)"
                ),
                params![connection_id, mapping_id, gen_i64, now_i64],
            )?;
            Ok(())
        })
    }

    /// Clear durable cursors/replays and rebind `connection_id` to `mapping_id`
    /// under the active lease. Refuses if unprocessed pending captures remain.
    ///
    /// # Errors
    /// [`CellError::SourceLeaseFenced`], [`CellError::SourceCursorConflict`],
    /// [`CellError::OutOfRange`], or store errors.
    pub fn reset_source_cursors(
        &mut self,
        connection_id: &str,
        mapping_id: &str,
        lease_id: &str,
        generation: u64,
        now_ms: u64,
    ) -> Result<(), CellError> {
        use crate::engine::params;
        use crate::source_cursor::{
            SOURCE_BINDING, SOURCE_CURSOR, SOURCE_PENDING, verify_active_lease,
        };
        let gen_i64 =
            i64::try_from(generation).map_err(|_| CellError::OutOfRange("binding generation"))?;
        let now_i64 =
            i64::try_from(now_ms).map_err(|_| CellError::OutOfRange("binding updated_at_ms"))?;
        self.write_tx(|| {
            verify_active_lease(&self.conn, connection_id, lease_id, generation, now_ms)?;
            let pending: Option<String> = self.conn.query_optional(
                &format!("SELECT \"dataset\" FROM \"{SOURCE_PENDING}\" WHERE \"connection_id\" = ?1 AND \"processed\" = 0 LIMIT 1"),
                params![connection_id],
                |row| row.get(0),
            )?;
            if let Some(dataset) = pending {
                return Err(CellError::SourceCursorConflict {
                    connection_id: connection_id.to_owned(),
                    dataset,
                    expected: None,
                    actual: Some("pending".to_owned()),
                });
            }
            self.conn.execute(
                &format!("DELETE FROM \"{SOURCE_CURSOR}\" WHERE \"connection_id\" = ?1"),
                params![connection_id],
            )?;
            self.conn.execute(
                &format!("DELETE FROM \"{SOURCE_PENDING}\" WHERE \"connection_id\" = ?1"),
                params![connection_id],
            )?;
            self.conn.execute(
                &format!(
                    "INSERT OR REPLACE INTO \"{SOURCE_BINDING}\" \
                     (\"connection_id\", \"mapping_id\", \"generation\", \"updated_at_ms\") \
                     VALUES (?1, ?2, ?3, ?4)"
                ),
                params![connection_id, mapping_id, gen_i64, now_i64],
            )?;
            Ok(())
        })
    }
}

impl<E: SqlEngine> crate::StoreReader<'_, E> {
    /// Durably bound `mapping_id` for `connection_id` from `_norn_source_binding`.
    ///
    /// # Errors
    /// Store errors.
    pub fn source_mapping_binding(&self, connection_id: &str) -> Result<Option<String>, CellError> {
        use crate::engine::params;
        use crate::source_cursor::SOURCE_BINDING;
        self.conn.query_optional(
            &format!(
                "SELECT \"mapping_id\" FROM \"{SOURCE_BINDING}\" WHERE \"connection_id\" = ?1"
            ),
            params![connection_id],
            |row| row.get(0),
        )
    }
}

impl<E, C, I, S: SqlEngine> crate::host::SqliteCell<E, C, I, S> {
    /// See [`CellStore::bind_source_mapping`].
    ///
    /// # Errors
    /// Lease fencing, out-of-range timestamp, or store errors.
    pub fn bind_source_mapping(
        &mut self,
        connection_id: &str,
        mapping_id: &str,
        lease_id: &str,
        generation: u64,
        now_ms: u64,
    ) -> Result<(), CellError> {
        self.store_mut().bind_source_mapping(
            connection_id,
            mapping_id,
            lease_id,
            generation,
            now_ms,
        )
    }

    /// See [`CellStore::reset_source_cursors`].
    ///
    /// # Errors
    /// Lease fencing, pending captures conflict, out-of-range timestamp, or store errors.
    pub fn reset_source_cursors(
        &mut self,
        connection_id: &str,
        mapping_id: &str,
        lease_id: &str,
        generation: u64,
        now_ms: u64,
    ) -> Result<(), CellError> {
        self.store_mut().reset_source_cursors(
            connection_id,
            mapping_id,
            lease_id,
            generation,
            now_ms,
        )
    }
}
