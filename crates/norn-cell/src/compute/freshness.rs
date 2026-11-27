//! QUERY-FRESH-1 (B6): model-derived required external sources and live
//! connector freshness snapshots.
//!
//! A question over a native-only type has an empty [`RequiredSources`] set and
//! keeps native-only `Exact` behavior. When one or more installed `Mapping`
//! artifacts target the question's grain (or its subtypes/supertypes or linked
//! time target), [`CellStore::required_sources_for_plan`] derives every
//! required `(source, dataset)` pair from the active definitions and live
//! `source.Connection` objects. [`evaluate_freshness`] fails closed to
//! [`Freshness::Incomplete`] whenever any required `(source, dataset)` is
//! missing from [`QueryContext::source_cursors`], lacks a matching complete
//! [`Watermark`] with heartbeat, or has an empty cursor set.

use std::collections::{BTreeMap, BTreeSet};

use norn_checker::metric::compute::QuestionPlan;
use norn_ir::mapping::MappingDocument;
use norn_ir::metric::FreshnessPolicy;
use norn_ir::{ArtifactEnvelope, ArtifactKind, EntityRoot, RuntimeValue};

use super::{Freshness, Frontier, QueryContext, Watermark};
use crate::engine::SqlEngine;
use crate::{CellError, CellStore, MAIN_WORLD, ObjectRecord};

/// Internal host-written sync-report mapping; never an external pull feed.
const HOST_REPORT_MAPPING: &str = "norn.connect.mapping";
/// Canonical connection object type written by the sources pack.
const CONNECTION_TYPE: &str = "source.Connection";
/// Page size for scanning `source.Connection` objects.
const CONNECTION_PAGE: usize = 256;

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
    let ts: jiff::Timestamp = text.parse().ok()?;
    u64::try_from(ts.as_millisecond()).ok()
}

fn legacy_connection_cursors(connection: &ObjectRecord) -> BTreeMap<String, String> {
    let Some(raw) = connection
        .values
        .get("watermark")
        .and_then(RuntimeValue::as_str)
    else {
        return BTreeMap::new();
    };
    let Ok(parsed) = serde_json::from_str::<BTreeMap<String, serde_json::Value>>(raw) else {
        return BTreeMap::new();
    };
    parsed
        .into_iter()
        .filter_map(|(dataset, value)| serde_json::to_string(&value).ok().map(|s| (dataset, s)))
        .collect()
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

impl<E: SqlEngine> CellStore<E> {
    fn connection_objects(&self, world: &str) -> Result<Vec<ObjectRecord>, CellError> {
        if self.model().object_type(CONNECTION_TYPE).is_err() {
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

    /// Derive every external `(source, dataset)` required to answer `plan`
    /// completely from the cell's active definitions and `source.Connection`
    /// objects.
    ///
    /// # Errors
    /// Active definitions or connection objects cannot be read.
    pub fn required_sources_for_plan(
        &self,
        world: &str,
        plan: &QuestionPlan,
    ) -> Result<RequiredSources, CellError> {
        let Some(active) = self.read().installed_definitions(None)? else {
            return Ok(RequiredSources::default());
        };
        let Ok(artifacts) = serde_json::from_str::<Vec<ArtifactEnvelope>>(&active.canonical_set)
        else {
            return Ok(RequiredSources::default());
        };
        let mapping_datasets = mapping_datasets_for_plan(self.model(), &artifacts, plan);
        if mapping_datasets.is_empty() {
            return Ok(RequiredSources::default());
        }
        let connections = self.connection_objects(world)?;
        let mut by_mapping: BTreeMap<String, Vec<String>> = BTreeMap::new();
        for conn in &connections {
            if let Some(mapping_id) = conn.values.get("mapping_id").and_then(RuntimeValue::as_str) {
                by_mapping
                    .entry(mapping_id.to_owned())
                    .or_default()
                    .push(conn.id.clone());
            }
        }
        let mut required: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
        for (mapping_id, datasets) in mapping_datasets {
            if let Some(conn_ids) = by_mapping.get(&mapping_id) {
                for conn_id in conn_ids {
                    required
                        .entry(conn_id.clone())
                        .or_default()
                        .extend(datasets.iter().cloned());
                }
            } else {
                required.entry(mapping_id).or_default().extend(datasets);
            }
        }
        Ok(RequiredSources(required))
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
        let required = self.required_sources_for_plan(world, plan)?;
        if required.is_empty() {
            return Ok(SourceFreshnessSnapshot::default());
        }
        let connections: BTreeMap<String, ObjectRecord> = self
            .connection_objects(world)?
            .into_iter()
            .map(|record| (record.id.clone(), record))
            .collect();
        let reader = self.read();
        let mut snapshot = SourceFreshnessSnapshot::default();
        for (source_id, datasets) in &required.0 {
            let conn = connections.get(source_id);
            let conn_healthy = conn
                .and_then(|c| c.values.get("status"))
                .and_then(RuntimeValue::as_str)
                == Some("ok");
            let conn_heartbeat_ms = conn
                .and_then(|c| c.values.get("last_sync_ended_at"))
                .and_then(RuntimeValue::as_str)
                .and_then(parse_rfc3339_ms);
            let cursors = reader.source_cursors(source_id)?;
            let legacy = if cursors.is_empty() {
                conn.map(legacy_connection_cursors).unwrap_or_default()
            } else {
                BTreeMap::new()
            };
            let pending: BTreeSet<String> = reader
                .pending_source_captures(source_id)?
                .into_iter()
                .map(|item| item.dataset)
                .collect();

            for dataset in datasets {
                let Some((cursor_json, cursor_updated_ms)) = cursors
                    .get(dataset)
                    .map(|record| (record.cursor_json.clone(), Some(record.updated_at_ms)))
                    .or_else(|| legacy.get(dataset).cloned().map(|raw| (raw, None)))
                else {
                    continue;
                };
                let has_pending = pending.contains(dataset);
                let complete = conn_healthy && !has_pending;
                let heartbeat_ms = if conn.is_some() {
                    conn_heartbeat_ms.or(cursor_updated_ms)
                } else {
                    None
                };
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
}
