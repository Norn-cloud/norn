//! SOURCE-AGREE-1 (07-world-boundary): two sources report different values
//! of a measure's partition source (its unit, currency or basis) for one
//! subject. The finding is recomputed from verified history at a read's own
//! frontier, never stored: consecutive versions whose partition value changed
//! and whose field provenance names two different source systems. It never
//! excludes its subject; an answer counts its disputed members instead.

use std::collections::BTreeMap;

use norn_checker::source_agreement as rule;
use norn_ir::{DataQualityFinding, DataQualityFindingStatus, DataQualitySubject, FactRef};
use norn_ir::{PropertyId, RuntimeValue, SourceClaim};
use serde::{Deserialize, Serialize};

use crate::engine::{SqlEngine, SqlRow, params};
use crate::read::{StoreReader, to_i64, to_u64};
use crate::source_provenance::InstanceSourceProvenance;
use crate::system::FACT_VALUES;
use crate::{CellError, ObjectRecord};

/// One derived disagreement and the two versions it reads, so the caller can
/// disclose it only where it may read both.
#[derive(Debug, Clone, PartialEq)]
pub struct SourceDisagreement {
    /// The warning, open or resolved at the read's frontier.
    pub finding: DataQualityFinding,
    /// The earlier version.
    pub earlier: ObjectRecord,
    /// The later version.
    pub later: ObjectRecord,
}

/// SOURCE-AGREE-1 on an answer: how many of its members two sources disagree
/// on, per partition source, counted as HOST-READ-4b counts links.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SourceDisagreementCounts {
    /// One entry per partition source with at least one disputed member, in
    /// property order.
    pub groups: Vec<SourceDisagreementCount>,
    /// True when every candidate was examined; false when the read stopped at
    /// `bound` subjects, and each count is then a lower bound.
    pub complete: bool,
    /// The most subjects one read examines.
    pub bound: u64,
}

/// The disputed members of one partition source.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SourceDisagreementCount {
    /// The partition-source property (`quantity_basis`).
    pub property: String,
    /// Members with an open disagreement on it.
    pub subjects: u64,
}

/// A steward's review of one disagreement (the `sources` pack).
const REVIEW: &str = "source.AgreementReview";

/// One version's partition values, by property.
type Reports = BTreeMap<PropertyId, Option<String>>;

/// Shared read context for resolving a subject's reported partition claims.
struct ClaimContext<'a> {
    world: &'a str,
    key: &'a str,
    records: &'a [Option<ObjectRecord>],
    values: &'a [Reports],
}

/// Bounded per-subject memo of decoded source provenance across versions.
struct ProvenanceMemo {
    budget: crate::reactive::DecodeBudget,
    entries: BTreeMap<usize, Option<InstanceSourceProvenance>>,
}

/// One resolved mapped report of a partition-source property.
struct ReportedClaim {
    index: usize,
    claim: SourceClaim,
    lineage: Option<norn_ir::InstanceProvenance>,
}

/// A partition value as the rule compares it: a code, or `None` for null.
fn code(value: Option<&RuntimeValue>) -> Option<String> {
    match value {
        None | Some(RuntimeValue::Null) => None,
        Some(RuntimeValue::String(text)) => Some(text.clone()),
        Some(other) => serde_json::to_string(other).ok(),
    }
}

impl<E: SqlEngine> StoreReader<'_, E> {
    /// The partition sources `object_type` declares.
    fn partition_of(&self, object_type: &str) -> Vec<PropertyId> {
        self.model()
            .object_types()
            .find(|def| def.name.as_str() == object_type)
            .map_or_default(rule::partition_sources)
    }

    /// Subjects of `object_type` whose versions up to `frontier` hold more than
    /// one value of a partition source, in id order, at most `limit`.
    fn changed_subjects(
        &self,
        world: &str,
        object_type: &str,
        partition: &[PropertyId],
        frontier: u64,
        limit: usize,
    ) -> Result<Vec<String>, CellError> {
        // S7/S8 (#1551 round 5): scanning `_norn_fact_values` requires verified history.
        self.check_history("source disagreement history")?;
        let mut ids = std::collections::BTreeSet::new();
        let bound = i64::try_from(limit).unwrap_or(i64::MAX);
        for property in partition {
            let path = format!("$.values.{}", property.as_str());
            let rows = self.conn.query(
                &format!("SELECT \"id\" FROM \"{FACT_VALUES}\" WHERE \"world\" = ?1 AND (\"root_type\" = ?2 OR \"object_type\" = ?2) AND \"commit_seq\" <= ?3 AND json_extract(\"snapshot\", '$.values') IS NOT NULL GROUP BY \"id\" HAVING count(DISTINCT json_quote(json_extract(\"snapshot\", ?4))) > 1 ORDER BY \"id\" LIMIT ?5"),
                params![world, object_type, to_i64(frontier, "frontier")?, path, bound],
            )?;
            for row in rows {
                let id: String = SqlRow(row).get(0)?;
                ids.insert(id);
            }
        }
        Ok(ids.into_iter().take(limit).collect())
    }

    /// Whether a steward's review accepts finding `id` at `frontier`
    /// (`source.AgreementReview`, keyed by the finding id).
    fn accepted(&self, world: &str, id: &str, frontier: Option<u64>) -> Result<bool, CellError> {
        self.check_history("source disagreement history")?;
        if !self.model().has_type(REVIEW) {
            return Ok(false);
        }
        let frontier = frontier.map(|f| to_i64(f, "frontier")).transpose()?;
        let rows = self.conn.query(
            &format!("SELECT \"version\" FROM \"{FACT_VALUES}\" WHERE \"world\" = ?1 AND \"root_type\" = ?2 AND \"id\" = ?3 AND (?4 IS NULL OR \"commit_seq\" <= ?4) ORDER BY \"version\" DESC LIMIT 1"),
            params![world, REVIEW, id, frontier],
        )?;
        let Some(row) = rows.into_iter().next() else {
            return Ok(false);
        };
        let version = to_u64(SqlRow(row).get(0)?, "review version")?;
        Ok(self
            .object_at_version(world, REVIEW, id, version)?
            .is_some_and(|review| {
                review.values.get("status") == Some(&RuntimeValue::String("accepted".to_owned()))
            }))
    }

    /// A subject's verified versions up to `frontier`, oldest first, with their
    /// partition values. Deleted tombstones are preserved as `None` (empty
    /// reports) so neither change detection nor gap walkback crosses a deletion
    /// boundary.
    fn partition_history(
        &self,
        world: &str,
        object_type: &str,
        key: &str,
        frontier: Option<u64>,
        partition: &[PropertyId],
    ) -> Result<(Vec<Option<ObjectRecord>>, Vec<Reports>), CellError> {
        // S7/S8 (#1551 round 5): scanning `_norn_fact_values` requires verified history.
        self.check_history("source disagreement history")?;
        let frontier = frontier.map(|f| to_i64(f, "frontier")).transpose()?;
        let rows = self.conn.query(
            &format!("SELECT \"version\" FROM \"{FACT_VALUES}\" WHERE \"world\" = ?1 AND (\"root_type\" = ?2 OR \"object_type\" = ?2) AND \"id\" = ?3 AND (?4 IS NULL OR \"commit_seq\" <= ?4) ORDER BY \"version\" LIMIT 4097"),
            params![world, object_type, key, frontier],
        )?;
        if rows.len() > 4096 {
            return Err(CellError::ComputeRefused("core.compute.bound_exceeded"));
        }
        let mut records = Vec::with_capacity(rows.len());
        for row in rows {
            let version = to_u64(SqlRow(row).get(0)?, "fact version")?;
            records.push(self.object_at_version(world, object_type, key, version)?);
        }
        let values = records
            .iter()
            .map(|record| match record {
                Some(record) => partition
                    .iter()
                    .map(|p| (p.clone(), code(record.values.get(p.as_str()))))
                    .collect(),
                None => BTreeMap::new(),
            })
            .collect();
        Ok((records, values))
    }

    fn load_provenance<'a>(
        &self,
        ctx: &ClaimContext<'_>,
        memo: &'a mut ProvenanceMemo,
        index: usize,
    ) -> Result<Option<&'a InstanceSourceProvenance>, CellError> {
        let Some(record) = ctx.records.get(index).and_then(Option::as_ref) else {
            return Ok(None);
        };
        let entry = match memo.entries.entry(index) {
            std::collections::btree_map::Entry::Occupied(occupied) => occupied.into_mut(),
            std::collections::btree_map::Entry::Vacant(vacant) => {
                let fact = FactRef::new(record.object_type.as_str(), ctx.key, record.version);
                vacant.insert(self.instance_source_provenance(
                    ctx.world,
                    &fact,
                    &mut memo.budget,
                    |_| Ok(true),
                )?)
            }
        };
        Ok(entry.as_ref())
    }

    /// Resolve the mapped report of `property` at `index`. When a source
    /// declared a mapping gap (`field_provenance` omits `property`), the stored
    /// row inherited its prior value rather than reporting a new one; when
    /// `walk_back` is true, walk back across consecutive gap versions carrying
    /// that same value to find the actual reporting source (SOURCE-AGREE-1).
    /// Tombstones (`None` records) terminate the walkback.
    fn reported_claim(
        &self,
        ctx: &ClaimContext<'_>,
        memo: &mut ProvenanceMemo,
        index: usize,
        property: &PropertyId,
        walk_back: bool,
    ) -> Result<Option<ReportedClaim>, CellError> {
        let target = ctx.values.get(index).and_then(|v| v.get(property)).cloned();
        let mut cursor = index;
        loop {
            if ctx.values.get(cursor).and_then(|v| v.get(property)) != target.as_ref() {
                return Ok(None);
            }
            let Some(record) = ctx.records.get(cursor).and_then(Option::as_ref) else {
                return Ok(None);
            };
            let Some(source) = self.load_provenance(ctx, memo, cursor)? else {
                return Ok(None);
            };
            if let Some(lineage) = source.field_provenance.get(property).cloned() {
                let system = lineage.source.system.clone();
                let value = target.flatten();
                let fact = FactRef::new(record.object_type.as_str(), ctx.key, record.version);
                return Ok(Some(ReportedClaim {
                    index: cursor,
                    claim: SourceClaim {
                        system,
                        value,
                        fact,
                    },
                    lineage: Some(lineage),
                }));
            }
            if !walk_back || cursor == 0 {
                return Ok(None);
            }
            cursor = cursor.saturating_sub(1);
        }
    }

    /// SOURCE-AGREE-1 for one subject at `frontier` (a commit; the current
    /// state when `None`), oldest first, resolved where the subject has
    /// returned to the earlier report.
    ///
    /// # Errors
    /// More than 4096 versions (`core.compute.bound_exceeded`), an exhausted
    /// provenance budget, or unproven history.
    pub fn source_disagreements(
        &self,
        world: &str,
        object_type: &str,
        key: &str,
        frontier: Option<u64>,
    ) -> Result<Vec<SourceDisagreement>, CellError> {
        self.check_live()?;
        let partition = self.partition_of(object_type);
        if partition.is_empty() {
            return Ok(Vec::new());
        }
        let (records, values) =
            self.partition_history(world, object_type, key, frontier, &partition)?;
        let changes = rule::changes(&values, &partition);
        if changes.is_empty() {
            return Ok(Vec::new());
        }
        let ctx = ClaimContext {
            world,
            key,
            records: &records,
            values: &values,
        };
        let mut memo = ProvenanceMemo {
            budget: crate::compute::question_budget()?,
            entries: BTreeMap::new(),
        };
        let mut found = Vec::new();
        for change in changes {
            let (Some(earlier), Some(later)) = (
                self.reported_claim(&ctx, &mut memo, change.earlier, &change.property, true)?,
                self.reported_claim(&ctx, &mut memo, change.later, &change.property, false)?,
            ) else {
                continue;
            };
            if earlier.claim.system == later.claim.system {
                continue;
            }
            let current = values
                .last()
                .and_then(|v| v.get(&change.property).cloned())
                .flatten();
            let seen = later.lineage.as_ref().map_or_else(
                || norn_ir::provenance::UtcTimestamp("1970-01-01T00:00:00Z".to_owned()),
                |lineage| lineage.recorded_at.clone(),
            );
            let mut finding = rule::finding(
                DataQualitySubject::new(norn_ir::ObjectTypeName(object_type.to_owned()), key),
                change.property.clone(),
                (earlier.claim, earlier.lineage),
                (later.claim, later.lineage),
                current.as_deref(),
                seen,
            )?;
            // A steward's accepted review acknowledges an open disagreement.
            if finding.status == DataQualityFindingStatus::Open
                && self.accepted(world, &finding.id, frontier)?
            {
                finding.status = DataQualityFindingStatus::Acknowledged;
            }
            let (Some(earlier), Some(later)) = (
                records.get(earlier.index).and_then(Option::as_ref).cloned(),
                records.get(later.index).and_then(Option::as_ref).cloned(),
            ) else {
                continue;
            };
            found.push(SourceDisagreement {
                finding,
                earlier,
                later,
            });
        }
        Ok(found)
    }

    /// SOURCE-AGREE-1 on an answer at `frontier`: per partition source, the
    /// members (`member`) with an open disagreement whose two versions the
    /// reader may see (`visible`). At most [`rule::BOUND`] candidates are
    /// examined; past it the counts are lower bounds. `None` when no member
    /// is disputed.
    ///
    /// # Errors
    /// As [`Self::source_disagreements`], or a gate's error.
    pub(crate) fn answer_disagreements(
        &self,
        world: &str,
        grain: &str,
        frontier: u64,
        member: &mut dyn FnMut(&str) -> Result<bool, CellError>,
        visible: &mut dyn FnMut(&ObjectRecord) -> Result<bool, CellError>,
    ) -> Result<Option<SourceDisagreementCounts>, CellError> {
        self.check_live()?;
        let partition = self.partition_of(grain);
        if partition.is_empty() {
            return Ok(None);
        }
        let bound = usize::try_from(rule::BOUND).unwrap_or(usize::MAX);
        let candidates =
            self.changed_subjects(world, grain, &partition, frontier, bound.saturating_add(1))?;
        let complete = candidates.len() <= bound;
        let mut counts: BTreeMap<String, u64> = BTreeMap::new();
        for id in candidates.iter().take(bound) {
            if !member(id)? {
                continue;
            }
            let mut disputed = std::collections::BTreeSet::new();
            for disagreement in self.source_disagreements(world, grain, id, Some(frontier))? {
                if disagreement.finding.status != DataQualityFindingStatus::Open
                    || !visible(&disagreement.earlier)?
                    || !visible(&disagreement.later)?
                {
                    continue;
                }
                if let norn_ir::DataQualityOutcome::SourceDisagreement { property, .. } =
                    &disagreement.finding.outcome
                {
                    disputed.insert(property.as_str().to_owned());
                }
            }
            for property in disputed {
                let entry = counts.entry(property).or_default();
                *entry = entry.saturating_add(1);
            }
        }
        if counts.is_empty() && complete {
            return Ok(None);
        }
        let order: Vec<String> = partition.iter().map(|p| p.as_str().to_owned()).collect();
        let mut groups: Vec<SourceDisagreementCount> = counts
            .into_iter()
            .map(|(property, subjects)| SourceDisagreementCount { property, subjects })
            .collect();
        groups.sort_by_key(|group| order.iter().position(|p| p == &group.property));
        Ok(Some(SourceDisagreementCounts {
            groups,
            complete,
            bound: rule::BOUND,
        }))
    }
}
