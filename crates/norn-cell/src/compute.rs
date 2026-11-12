//! Result/frontier contracts; producing evaluators must qualify their snapshot pins.
use std::collections::{BTreeMap, BTreeSet};

mod historical;
/// Receipt-driven query maintenance.
pub mod ivm;
/// Exact result membership and native fact evidence.
pub mod lineage;
#[cfg(feature = "duckdb")]
mod olap;
/// Governed native restatement generations.
pub mod restatement;
mod restatement_state;
pub use historical::HistoricalQuestionResult;

use crate::{
    CellError, CellStore, LedgerHead, ObjectRecord,
    engine::SqlEngine,
    reactive::{BudgetedRead, DecodeBudget, ReactiveLimits},
};
use norn_checker::metric::compute::{
    QuestionPlan, UNSUPPORTED_ENGINE, arithmetic as math,
    contribution::{Contribution, project},
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
    const fn visible(version: u64, amounts: Vec<Option<Decimal>>) -> Self {
        Self {
            version,
            amounts: Some(amounts),
            restricted: false,
        }
    }
}

impl QueryRow {
    /// DATA-VALUES-1 (D52/D53): mark `restricted` members and, if any, withhold
    /// every member's amounts in this bucket, so no hidden amount follows from
    /// the totals. For such a reader the bucket aggregate is disclosed only
    /// when the bucket has at least `min_disclosed` members; a smaller bucket
    /// keeps its membership but is `suppressed`: no totals and a null value.
    pub fn withhold(&mut self, restricted: impl Fn(&str) -> bool, min_disclosed: usize) {
        let mut any = false;
        for (id, member) in &mut self.members {
            member.restricted = restricted(id);
            any |= member.restricted;
        }
        if any {
            for member in self.members.values_mut() {
                member.amounts = None;
            }
            if self.members.len() < min_disclosed {
                self.totals.clear();
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
}

#[derive(Serialize)]
struct FindingEvidence<'a> {
    #[serde(flatten)]
    context: &'a QueryContext,
    exclusions: &'a BTreeMap<String, String>,
}

pub(crate) type QuestionPopulation = (QueryContext, BTreeMap<String, String>);

type Bucket = (RuntimeValue, Vec<i128>, BTreeMap<String, Member>);

pub(crate) type Contributions = BTreeMap<String, (u64, Contribution)>;
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
fn refused(code: &'static str) -> CellError {
    CellError::ComputeRefused(code)
}
fn decimal(n: i128) -> Result<Decimal, CellError> {
    Decimal::parse(&format!("{n}e-6")).map_err(CellError::Value)
}

impl<E: SqlEngine> CellStore<E> {
    /// QUERY-FRESH-1: the bound scope must agree even for an empty population.
    fn check_question_scope(&self, scope: &str) -> Result<(), CellError> {
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
                if let Some(value) = project(plan, &record.values).map_err(refused)? {
                    contributions.insert(id, (record.version, value));
                }
            }
            let (query_pin, definition_pin) =
                self.question_pins(plan, cutoff.filter(|_| !answered))?;
            let result = materialize(
                plan,
                (context, exclusions),
                revision,
                query_pin,
                definition_pin,
                &contributions,
            )?;
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
    fn check_question_plan(&self, plan: &QuestionPlan) -> Result<usize, CellError> {
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
    fn question_pins(
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

#[derive(Clone, Default)]
pub(crate) struct Totals(BTreeMap<String, Vec<i128>>);
impl Totals {
    /// COMPUTE-DETERMINISTIC-1: apply one signed identity contribution with checked exact arithmetic.
    pub(crate) fn adjust(&mut self, value: &Contribution, retract: bool) -> Result<(), CellError> {
        let key = bucket_key(value)?;
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
fn bucket_key(value: &Contribution) -> Result<String, CellError> {
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
    for (_, value) in contributions.values() {
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
    let mut grouped: BTreeMap<String, Bucket> = BTreeMap::new();
    for (id, (version, contribution)) in contributions {
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
            Ok((
                key,
                QueryRow {
                    bucket,
                    totals: totals.into_iter().map(decimal).collect::<Result<_, _>>()?,
                    value,
                    members,
                    suppressed: false,
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
                totals.adjust(&Contribution { bucket: RuntimeValue::Null, amounts: vec![value.map(i64::from)] }, false).expect("bounded add");
            }
            for value in values.iter().rev() {
                totals.adjust(&Contribution { bucket: RuntimeValue::Null, amounts: vec![value.map(i64::from)] }, true).expect("bounded retract");
            }
            proptest::prop_assert!(totals.0.values().all(|sums| sums.iter().all(|sum| *sum == 0)));
        }
    }
}
