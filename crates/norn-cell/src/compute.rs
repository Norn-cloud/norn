//! Result/frontier contracts; producing evaluators must qualify their snapshot pins.
use std::collections::{BTreeMap, BTreeSet};

mod historical;
/// Receipt-driven query maintenance.
pub mod ivm;
/// Exact result membership and native fact evidence.
pub mod lineage;
/// VIEW-MAT-1: a materialized transform's stored result.
pub mod materialize;
/// VIEW-ENGINE-1: admitted transforms lowered to embedded `DuckDB`.
#[cfg(feature = "duckdb")]
mod olap_transform;
/// Governed native restatement generations.
pub mod restatement;
mod restatement_state;
/// VIEW-ROWS-1 / VIEW-ENGINE-1: admitted transforms over one authorized snapshot.
pub mod transform;
pub use historical::HistoricalQuestionResult;

use crate::{
    CellError, CellStore, LedgerHead, ObjectRecord,
    engine::SqlEngine,
    reactive::{BudgetedRead, DecodeBudget, ReactiveLimits},
};
use norn_checker::metric::compute::{
    QuestionPlan, UNSUPPORTED_ENGINE, arithmetic as math,
    contribution::{Contribution, Linked, as_of, project, project_linked},
};
use norn_ir::{Decimal, ObjectTypeName, RuntimeValue, UtcTimestamp};
use norn_ir::{ResourceLimitProfile, metric::FreshnessPolicy};
use serde::{Deserialize, Serialize};

/// Connector instance → partition → opaque consumed cursor.
pub type Frontier = BTreeMap<String, BTreeMap<String, String>>;
/// Observed source progress, supplied by the trusted connector host.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Watermark {
    /// Connector's observed high-water cursor; only equality is used here.
    pub observed: String,
    /// Last authenticated heartbeat; absent means unavailable coverage.
    pub heartbeat_ms: Option<u64>,
    /// Source coverage is known through the consumed cursor.
    pub complete: bool,
}
/// Host-resolved query dependencies and authorized population, never client authority.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct QueryContext {
    /// Selected execution world.
    pub world: String,
    /// Cell-bound tenant scope.
    pub scope: String,
    /// Authorized population identity and revocation epoch.
    pub population_pin: String,
    /// Server authorization epoch.
    pub authorization_epoch: u64,
    /// Admitted mapping version/digest.
    pub mapping_pin: String,
    /// Host-observed clock, not render time.
    pub now_ms: u64,
    /// Required source frontier; unrelated sources are omitted.
    pub source_cursors: Frontier,
    /// Watermarks for required dependencies.
    pub watermarks: BTreeMap<String, BTreeMap<String, Watermark>>,
}
impl QueryContext {
    // DATA-QUARANTINE: shared cell/IVM population selection pins retained evidence.
    fn include_record<E: SqlEngine>(
        &self,
        reader: &crate::StoreReader<'_, E>,
        record: &ObjectRecord,
        frontier: Option<u64>,
        exclusions: &mut BTreeMap<String, String>,
        finding_members: &mut BTreeSet<String>,
        visible: &mut impl FnMut(&ObjectRecord) -> Result<bool, CellError>,
    ) -> Result<bool, CellError> {
        if record.scope != self.scope {
            return Err(refused("core.compute.stale_input"));
        }
        if !visible(record)? {
            return Ok(false);
        }
        finding_members.insert(record.id.clone());
        if let Some(finding) = reader
            .question_findings(&self.world, record, frontier)?
            .first()
        {
            exclusions.insert(record.id.clone(), finding.id.clone());
            return Ok(false);
        }
        Ok(true)
    }
}
/// Declared execution strategy; qualification belongs to the producing evaluator.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Engine {
    /// Exact maintained contribution totals.
    Ivm,
    /// Exact cell snapshot.
    Cell,
    /// Embedded SQL over the exact authorized contribution snapshot.
    Duckdb,
}
/// Snapshot freshness, independent of source cursor ordering.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Freshness {
    /// Complete analytical coverage with admitted source heartbeat lag.
    Fresh,
    /// Complete coverage with zero heartbeat lag.
    Exact,
    /// Known coverage at an aged frontier.
    Stale,
    /// Missing coverage or explicit exclusions.
    Incomplete,
}
/// One numeric contribution with original object-version identity.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Member {
    /// Original cell object version.
    pub version: u64,
    /// Operand contributions; a null operand means its predicate excluded this
    /// identity. `None` (wire `null`) when the reader may not see every member
    /// of the bucket's values (DATA-VALUES-1): amounts are row values, and
    /// one withheld member among visible ones would follow from the totals.
    pub amounts: Option<Vec<Option<Decimal>>>,
    /// The reader holds no Values grant on this member.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub restricted: bool,
}

impl Member {
    /// A member whose amounts the evaluation discloses.
    pub(crate) const fn visible(version: u64, amounts: Vec<Option<Decimal>>) -> Self {
        Self {
            version,
            amounts: Some(amounts),
            restricted: false,
        }
    }
}

/// D102: the identity a host's row gate is asked for when it decides every
/// row of a type alike; a bucket whose members are not listed is gated by it.
pub const ALL_MEMBERS: &str = ":all";

impl QueryRow {
    /// DATA-VALUES-1 (D52/D53): mark `restricted` members and, if any, withhold
    /// every member's amounts in this bucket, so no hidden amount follows from
    /// the totals. For such a reader the bucket aggregate is disclosed only
    /// when the bucket has at least `min_disclosed` members; a smaller bucket
    /// keeps its membership but is `suppressed`: no totals and a null value.
    /// A bucket whose members are not listed (D102) is restricted when
    /// [`ALL_MEMBERS`] is, and counts its `member_count`.
    pub fn withhold(&mut self, restricted: impl Fn(&str) -> bool, min_disclosed: usize) {
        let mut any = self.member_count.is_some() && restricted(ALL_MEMBERS);
        for (id, member) in &mut self.members {
            member.restricted = restricted(id);
            any |= member.restricted;
        }
        if any {
            for member in self.members.values_mut() {
                member.amounts = None;
            }
            let members = self.member_count.map_or(self.members.len(), |n| {
                usize::try_from(n).unwrap_or(usize::MAX)
            });
            if members < min_disclosed {
                self.totals.clear();
                self.null_counts.clear();
                self.value = None;
                self.suppressed = true;
            }
        }
    }
}
/// One output bucket, retaining exact sums beside its ratio.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct QueryRow {
    /// Scalar grouping key.
    pub bucket: RuntimeValue,
    /// One sum, or numerator then denominator.
    pub totals: Vec<Decimal>,
    /// Null means a zero-denominator ratio is unavailable, or the bucket is `suppressed`.
    pub value: Option<Decimal>,
    /// Exact aggregate-membership lineage.
    pub members: BTreeMap<String, Member>,
    /// DATA-VALUES-1 (D53): the reader lacks Values on a member and the bucket
    /// has fewer members than the declared minimum, so `totals` is empty and
    /// `value` null. Never set on an unredacted result.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub suppressed: bool,
    /// Per operand, in `totals` order: how many members had a null measured
    /// value, skipped by the sum (never read as 0). Empty when there were none,
    /// and when the bucket is `suppressed`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub null_counts: Vec<u64>,
    /// D102: contributing members, when `members` is not inlined because the
    /// population exceeds the plan's bound (the analytics engine answered).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub member_count: Option<u64>,
    /// MEMBERSHIP-DIGEST-1 over the members in identity order, present with
    /// `member_count`; the trace pages the members it commits to.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub membership_digest: Option<String>,
}
/// Result envelope for QUERY-ENGINE-1; construction alone grants no authority.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct QueryResult {
    /// Content identity of model, declarations, bounds and arithmetic profile.
    pub query_pin: String,
    /// Actual model/definition pin.
    pub definition_pin: String,
    /// Physical evaluator.
    pub engine: Engine,
    /// Actual verified ledger head.
    pub revision: LedgerHead,
    /// Exclusive effective cutoff of the admitted question window.
    pub effective_as_of: UtcTimestamp,
    /// Actual knowledge revision; not a fabricated historical timestamp.
    pub known_as_of: u64,
    /// Root contribution identity type.
    pub grain: ObjectTypeName,
    /// Content-addressed membership and source lineage.
    pub lineage_ref: String,
    /// All output buckets.
    pub rows: BTreeMap<String, QueryRow>,
    /// Source freshness and coverage label.
    pub freshness: Freshness,
    /// Known coverage with no quarantined exclusions.
    pub complete: bool,
    /// Server-resolved scope, mappings, frontier and watermarks.
    #[serde(flatten)]
    pub context: QueryContext,
    /// DATA-QUARANTINE: engine-resolved identity → durable finding ID.
    pub exclusions: BTreeMap<String, String>,
    /// METRIC-GRAIN-LINK-1: rows that would count but whose optional time
    /// link is absent, so they have no time: skipped, never bucketed.
    #[serde(default, skip_serializing_if = "crate::store::is_zero")]
    pub unlinked: u64,
}

#[derive(Serialize)]
pub(crate) struct FindingEvidence<'a> {
    #[serde(flatten)]
    pub(crate) context: &'a QueryContext,
    pub(crate) exclusions: &'a BTreeMap<String, String>,
}

pub(crate) type QuestionPopulation = (QueryContext, BTreeMap<String, String>);

type Bucket = (RuntimeValue, Vec<i128>, BTreeMap<String, Member>);

/// Each root identity's versioned contributions: one for an ordinary sum, one
/// per bucket for a snapshot sum (METRIC-SNAPSHOT-ASOF-1).
pub(crate) type Contributions = BTreeMap<String, Vec<(u64, Contribution)>>;
/// ASOF-KNOWLEDGE-1: an `AsOf` plan admits only a cutoff recorded exactly at its declared
/// instant; sub-millisecond declarations never match a millisecond clock.
fn asof_matches(plan: &QuestionPlan, millis: u64) -> bool {
    plan.as_of_instant()
        .is_some_and(|instant| millis_coordinates(millis) == Some(instant))
}
/// A question scans under its own bound (ruling 2026-11-12), not the bound
/// on records a slice returns.
fn question_budget() -> Result<DecodeBudget, CellError> {
    let profile = ResourceLimitProfile::norn_limits_v1();
    let question = profile.question_limits();
    let limits = ReactiveLimits {
        max_scan: usize::try_from(question.max_scanned_records)
            .map_err(|_bound| refused("core.compute.bound_exceeded"))?,
        max_candidate_bytes_per_commit: question.max_scanned_bytes,
        ..ReactiveLimits::from_profile(&profile)
    };
    Ok(DecodeBudget::new(&limits))
}

fn check_question_cutoff(
    plan: &QuestionPlan,
    cutoff: Option<crate::KnowledgeCutoff>,
) -> Result<(), CellError> {
    if matches!(plan.question().freshness, FreshnessPolicy::AsOf { .. }) {
        let point = cutoff.ok_or_else(|| refused(UNSUPPORTED_ENGINE))?;
        if !asof_matches(plan, point.recorded_at.as_millis()) {
            return Err(refused("core.compute.stale_input"));
        }
    }
    Ok(())
}
fn millis_coordinates(millis: u64) -> Option<(i64, u32)> {
    let seconds = i64::try_from(millis.div_euclid(1000)).ok()?;
    let nanos = millis.rem_euclid(1000).checked_mul(1_000_000)?;
    Some((seconds, u32::try_from(nanos).ok()?))
}
pub(crate) fn refused(code: &'static str) -> CellError {
    CellError::ComputeRefused(code)
}
pub(crate) fn decimal(n: i128) -> Result<Decimal, CellError> {
    Decimal::parse(&format!("{n}e-6")).map_err(CellError::Value)
}

impl<E: SqlEngine> CellStore<E> {
    /// QUERY-FRESH-1: the bound scope must agree even for an empty population.
    pub(crate) fn check_question_scope(&self, scope: &str) -> Result<(), CellError> {
        let bound = self.conn.query_optional(
            r#"SELECT "value" FROM "_norn_meta" WHERE "key"='binding:scope'"#,
            &[],
            |row| row.get::<String>(0),
        )?;
        if bound.is_some_and(|bound| bound != scope) {
            return Err(refused("core.compute.stale_input"));
        }
        Ok(())
    }

    /// COMPUTE-DETERMINISTIC-1 / QUERY-GRAIN-1: evaluate a full bounded current-world snapshot.
    /// This reads the current world; use [`Self::evaluate_question_known_at`] for a knowledge cutoff.
    /// The envelope reports the actual verified head.
    /// The host supplies server authorization, dependency pins and Core 07 dispositions.
    /// # Errors
    /// Refuses incomplete scans, unsupported semantics, model/scope mismatch or invalid values.
    pub fn evaluate_question(
        &self,
        plan: &QuestionPlan,
        context: QueryContext,
        mut visible: impl FnMut(&ObjectRecord) -> Result<bool, CellError>,
    ) -> Result<QueryResult, CellError> {
        self.question_snapshot(plan, context, &mut visible)
            .map(|snapshot| snapshot.result)
    }

    fn question_snapshot(
        &self,
        plan: &QuestionPlan,
        context: QueryContext,
        visible: &mut impl FnMut(&ObjectRecord) -> Result<bool, CellError>,
    ) -> Result<Snapshot, CellError> {
        self.question_snapshot_at(plan, context, None, visible)
    }
    fn question_snapshot_at(
        &self,
        plan: &QuestionPlan,
        context: QueryContext,
        cutoff: Option<crate::KnowledgeCutoff>,
        visible: &mut impl FnMut(&ObjectRecord) -> Result<bool, CellError>,
    ) -> Result<Snapshot, CellError> {
        self.question_snapshot_at_with(plan, context, cutoff, false, visible, |snapshot, _| {
            Ok(snapshot)
        })
    }
    /// The question's snapshot, read live or as of `cutoff`. An `answered`
    /// read is a live answer recomputed from the commit it was given at: its
    /// rows are read as of `cutoff`, but it is pinned as the live answer was
    /// (no slice pin, no historical receipt), so it reproduces that answer's
    /// lineage pin exactly when nothing it read has changed since.
    #[allow(
        clippy::too_many_lines,
        reason = "one snapshot: read, gate, project, pin, in order"
    )]
    fn question_snapshot_at_with<T>(
        &self,
        plan: &QuestionPlan,
        context: QueryContext,
        cutoff: Option<crate::KnowledgeCutoff>,
        answered: bool,
        visible: &mut impl FnMut(&ObjectRecord) -> Result<bool, CellError>,
        consume: impl FnOnce(Snapshot, &mut DecodeBudget) -> Result<T, CellError>,
    ) -> Result<T, CellError> {
        self.conn.transaction(|| {
            let mut exclusions = BTreeMap::new();
            self.check_question_scope(&context.scope)?;
            check_question_cutoff(plan, cutoff)?;
            let max = self.check_question_plan(plan)?;
            let mut revision = self.verified_head()?;
            let reader = self.read();
            let mut budget = question_budget()?;
            let mut historical_receipt = None;
            let mut frontier_receipt = None;
            let (ids, complete) = if let Some(cutoff) = cutoff {
                (revision, frontier_receipt) =
                    historical::frame(&reader, cutoff, context.now_ms, &mut budget)?;
                if !answered {
                    historical_receipt = frontier_receipt.as_ref().map(|r| r.hash.clone());
                }
                historical::history_ids(
                    &reader,
                    &context.world,
                    plan.grain().as_str(),
                    cutoff.frontier,
                    max,
                    &mut budget,
                )?
            } else {
                let (rows, complete) = reader.root_rows_with_budget(
                    &context.world,
                    plan.grain().as_str(),
                    max,
                    &mut budget,
                )?;
                (rows.into_iter().map(|(id, _)| id).collect(), complete)
            };
            if !complete {
                return Err(refused("core.compute.bound_exceeded"));
            }
            let mut history = reader.history_read_seeded(frontier_receipt);
            let mut contributions = BTreeMap::new();
            let mut unlinked = 0u64;
            let mut members = BTreeSet::new();
            let mut finding_members = BTreeSet::new();
            for id in ids {
                let selected = if let Some(cutoff) = cutoff {
                    historical::historical_record(
                        &mut history,
                        &context.world,
                        plan.grain().as_str(),
                        &id,
                        cutoff,
                        self.model().model_hash(),
                        &mut budget,
                    )?
                } else {
                    reader.object_with_budget(
                        &context.world,
                        plan.grain().as_str(),
                        &id,
                        &mut budget,
                    )?
                };
                let record = match selected {
                    BudgetedRead::Found(record) => record,
                    BudgetedRead::Missing if cutoff.is_some() => continue,
                    BudgetedRead::Missing | BudgetedRead::Exhausted => {
                        return Err(refused("core.compute.bound_exceeded"));
                    }
                };
                members.insert(id.clone());
                if !context.include_record(
                    &reader,
                    &record,
                    cutoff.map(|c| c.frontier),
                    &mut exclusions,
                    &mut finding_members,
                    visible,
                )? {
                    continue;
                }
                let (entries, timeless) = subject_contributions(
                    &reader,
                    plan,
                    &context,
                    (&id, &record),
                    cutoff,
                    &mut budget,
                )?;
                unlinked = unlinked.saturating_add(u64::from(timeless));
                if !entries.is_empty() {
                    contributions.insert(id, entries);
                }
            }
            let (query_pin, definition_pin) =
                self.question_pins(plan, cutoff.filter(|_| !answered))?;
            let mut result = materialize(
                plan,
                (context, exclusions),
                revision,
                query_pin,
                definition_pin,
                &contributions,
            )?;
            result.unlinked = unlinked;
            consume(
                Snapshot {
                    historical_receipt,
                    result,
                    contributions,
                    members,
                    finding_members,
                },
                &mut budget,
            )
        })
    }
    pub(crate) fn check_question_plan(&self, plan: &QuestionPlan) -> Result<usize, CellError> {
        let model = self.model().query_contract(plan.grain().as_str())?;
        if plan.properties().iter().any(|p| {
            model
                .property(p.id.as_str())
                .is_none_or(|actual| !p.same_contract(actual))
        }) {
            return Err(refused(UNSUPPORTED_ENGINE));
        }
        usize::try_from(plan.max_items()).map_err(|_bound| refused("core.compute.bound_exceeded"))
    }
    pub(crate) fn question_pins(
        &self,
        plan: &QuestionPlan,
        cutoff: Option<crate::KnowledgeCutoff>,
    ) -> Result<(String, String), CellError> {
        let reader = self.read();
        let definition_pin = match reader.active_definitions_version()? {
            Some(version) => {
                reader
                    .installed_definitions(Some(version))?
                    .ok_or_else(|| refused("core.compute.stale_input"))?
                    .receipt_sha256
            }
            None => self.model().model_hash().to_owned(),
        };
        let pin = historical::query_pin(self.model().model_hash(), plan, &definition_pin, cutoff)?;
        Ok((pin, definition_pin))
    }
}

struct Snapshot {
    historical_receipt: Option<String>,
    result: QueryResult,
    contributions: Contributions,
    members: BTreeSet<String>,
    finding_members: BTreeSet<String>,
}

/// Exact sums per bucket, and per operand the count of null measured values skipped.
#[derive(Clone, Default)]
pub(crate) struct Totals(BTreeMap<String, Vec<i128>>, BTreeMap<String, Vec<u64>>);
impl Totals {
    /// COMPUTE-DETERMINISTIC-1: apply one signed identity contribution with checked exact arithmetic.
    pub(crate) fn adjust(&mut self, value: &Contribution, retract: bool) -> Result<(), CellError> {
        let key = bucket_key(value)?;
        let nulls = self
            .1
            .entry(key.clone())
            .or_insert_with(|| vec![0; value.amounts.len()]);
        for (count, null) in nulls.iter_mut().zip(&value.nulls) {
            if *null {
                *count = if retract {
                    count.checked_sub(1)
                } else {
                    count.checked_add(1)
                }
                .ok_or_else(|| refused("core.compute.bound_exceeded"))?;
            }
        }
        let totals = self
            .0
            .entry(key)
            .or_insert_with(|| vec![0; value.amounts.len()]);
        for (total, amount) in totals.iter_mut().zip(&value.amounts) {
            let amount = amount.unwrap_or(0);
            let delta = if retract {
                amount
                    .checked_neg()
                    .ok_or_else(|| refused(UNSUPPORTED_ENGINE))?
            } else {
                amount
            };
            *total =
                math::add(*total, delta).ok_or_else(|| refused("core.compute.bound_exceeded"))?;
        }
        Ok(())
    }
}
/// A bucket's skipped-null counts, or none when nothing was null: answers over
/// complete data are unchanged.
fn null_counts(counts: Option<&Vec<u64>>) -> Vec<u64> {
    counts
        .filter(|counts| counts.iter().any(|count| *count > 0))
        .cloned()
        .unwrap_or_default()
}

/// One root identity's contributions: its current row's, one per bucket from
/// every stored version for a snapshot sum (METRIC-SNAPSHOT-ASOF-1), or its
/// row's at its linked object's time (METRIC-GRAIN-LINK-1); `true` when the
/// row would count but its optional time link is absent.
fn subject_contributions<E: SqlEngine>(
    reader: &crate::read::StoreReader<'_, E>,
    plan: &QuestionPlan,
    context: &QueryContext,
    (id, record): (&str, &ObjectRecord),
    cutoff: Option<crate::KnowledgeCutoff>,
    budget: &mut DecodeBudget,
) -> Result<(Vec<(u64, Contribution)>, bool), CellError> {
    if let Some(link) = plan.time_link() {
        let targets = match reader.link_targets_with_budget(
            &context.world,
            plan.grain().as_str(),
            link.link.as_str(),
            id,
            budget,
        )? {
            crate::reactive::BudgetedLinksRead::Complete(targets) => targets,
            crate::reactive::BudgetedLinksRead::Exhausted => {
                return Err(refused("core.compute.bound_exceeded"));
            }
        };
        let linked = match targets.as_slice() {
            [] => None,
            [target] => match reader.object_with_budget(
                &context.world,
                link.target_type.as_str(),
                target,
                budget,
            )? {
                BudgetedRead::Found(linked) => Some(linked),
                BudgetedRead::Missing => None,
                BudgetedRead::Exhausted => return Err(refused("core.compute.bound_exceeded")),
            },
            _ => return Err(refused(UNSUPPORTED_ENGINE)),
        };
        let projected = project_linked(plan, &record.values, linked.as_ref().map(|l| &l.values))
            .map_err(refused)?;
        return Ok(match projected {
            Linked::Contribution(value) => (vec![(record.version, value)], false),
            Linked::Excluded => (Vec::new(), false),
            Linked::Unlinked => (Vec::new(), true),
        });
    }
    if !plan.as_of() {
        return Ok((
            project(plan, &record.values)
                .map_err(refused)?
                .map(|value| vec![(record.version, value)])
                .unwrap_or_default(),
            false,
        ));
    }
    let versions = reader
        // METRIC-SNAPSHOT-ASOF-1 at a knowledge cutoff: the versions known then.
        .object_versions_with_budget(
            &context.world,
            plan.grain().as_str(),
            id,
            cutoff.map(|c| c.frontier),
            budget,
        )?
        .ok_or_else(|| refused("core.compute.bound_exceeded"))?
        .into_iter()
        .map(|version| (version.version, version.values))
        .collect::<Vec<_>>();
    Ok((as_of(plan, &versions).map_err(refused)?, false))
}

/// Every `(root identity, versioned contribution)` pair.
fn entries(contributions: &Contributions) -> impl Iterator<Item = (&String, &(u64, Contribution))> {
    contributions
        .iter()
        .flat_map(|(id, entries)| entries.iter().map(move |entry| (id, entry)))
}

pub(crate) fn bucket_key(value: &Contribution) -> Result<String, CellError> {
    String::from_utf8(value.bucket.canonical_json()?)
        .map_err(|e| CellError::Encoding(e.to_string()))
}

pub(crate) fn materialize(
    plan: &QuestionPlan,
    population: QuestionPopulation,
    revision: LedgerHead,
    query_pin: String,
    definition_pin: String,
    contributions: &Contributions,
) -> Result<QueryResult, CellError> {
    let mut totals = Totals::default();
    for (_, value) in contributions.values().flatten() {
        totals.adjust(value, false)?;
    }
    finish(
        plan,
        population,
        revision,
        query_pin,
        definition_pin,
        contributions,
        &totals,
    )
}

#[allow(
    clippy::too_many_lines,
    reason = "one envelope: buckets, ratios, null counts, coverage and lineage"
)]
pub(crate) fn finish(
    plan: &QuestionPlan,
    population: QuestionPopulation,
    revision: LedgerHead,
    query_pin: String,
    definition_pin: String,
    contributions: &Contributions,
    totals: &Totals,
) -> Result<QueryResult, CellError> {
    let (context, exclusions) = population;
    let totals_in = totals;
    let mut grouped: BTreeMap<String, Bucket> = BTreeMap::new();
    for (id, (version, contribution)) in entries(contributions) {
        let key = bucket_key(contribution)?;
        let bucket = grouped.entry(key.clone()).or_insert_with(|| {
            (
                contribution.bucket.clone(),
                totals.0.get(&key).cloned().unwrap_or_default(),
                BTreeMap::new(),
            )
        });
        let amounts = contribution
            .amounts
            .iter()
            .map(|n| n.map(|v| decimal(i128::from(v))).transpose())
            .collect::<Result<_, _>>()?;
        bucket
            .2
            .insert(id.clone(), Member::visible(*version, amounts));
    }
    let rows = grouped
        .into_iter()
        .map(|(key, (bucket, totals, members))| {
            let value = match totals.as_slice() {
                [sum] => Some(decimal(*sum)?),
                [_, 0] => None,
                [n, d] => Some(decimal(
                    math::ratio(*n, *d).ok_or_else(|| refused("core.compute.bound_exceeded"))?,
                )?),
                _ => return Err(refused(UNSUPPORTED_ENGINE)),
            };
            let null_counts = null_counts(totals_in.1.get(&key));
            Ok((
                key,
                QueryRow {
                    bucket,
                    totals: totals.into_iter().map(decimal).collect::<Result<_, _>>()?,
                    value,
                    members,
                    suppressed: false,
                    null_counts,
                    member_count: None,
                    membership_digest: None,
                },
            ))
        })
        .collect::<Result<_, CellError>>()?;
    let mut complete = exclusions.is_empty();
    let (mut stale, mut aged) = (false, false);
    let age = match plan.question().freshness {
        FreshnessPolicy::Live { max_age_ms } | FreshnessPolicy::Delayed { max_age_ms } => {
            Some(max_age_ms.0)
        }
        FreshnessPolicy::AsOf { .. } => None,
    };
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
    let evidence = FindingEvidence {
        context: &context,
        exclusions: &exclusions,
    };
    let lineage_ref = crate::codec::hash_json(&(&query_pin, &revision, evidence, &rows))?;
    Ok(QueryResult {
        query_pin,
        definition_pin,
        engine: Engine::Cell,
        known_as_of: revision.commits,
        revision,
        effective_as_of: plan.question().time_window.end.clone(),
        grain: plan.grain().clone(),
        lineage_ref,
        rows,
        freshness: if !complete {
            Freshness::Incomplete
        } else if stale {
            Freshness::Stale
        } else if aged {
            Freshness::Fresh
        } else {
            Freshness::Exact
        },
        complete,
        unlinked: 0,
        context,
        exclusions,
    })
}

#[cfg(test)]
#[allow(clippy::expect_used, reason = "bounded delta property assertions")]
mod tests {
    use super::*;
    proptest::proptest! {
        #[test]
        fn asof_millisecond_coordinates_are_exact(offset in 0u32..86_400_000) {
            let millis=1_792_972_800_000u64.checked_add(u64::from(offset)).expect("bounded timestamp");
            let at=chrono::DateTime::from_timestamp_millis(i64::try_from(millis).expect("bounded")).expect("valid timestamp");
            let expected=(at.timestamp(),at.timestamp_subsec_nanos());
            proptest::prop_assert_eq!(millis_coordinates(millis),Some(expected));
            proptest::prop_assert_ne!(millis_coordinates(millis.checked_add(1).expect("bounded successor")),Some(expected));
        }
    }

    proptest::proptest! {
        #[test]
        fn delta_retraction_restores_exact_totals(values in proptest::collection::vec(proptest::option::of(proptest::num::i32::ANY), 0..100)) {
            let mut totals = Totals::default();
            for value in &values {
                totals.adjust(&Contribution { bucket: RuntimeValue::Null, amounts: vec![value.map(i64::from)], nulls: vec![false] }, false).expect("bounded add");
            }
            for value in values.iter().rev() {
                totals.adjust(&Contribution { bucket: RuntimeValue::Null, amounts: vec![value.map(i64::from)], nulls: vec![false] }, true).expect("bounded retract");
            }
            proptest::prop_assert!(totals.0.values().all(|sums| sums.iter().all(|sum| *sum == 0)));
        }
    }
}
