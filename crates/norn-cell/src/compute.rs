//! Result/frontier contracts; producing evaluators must qualify their snapshot pins.
use std::collections::BTreeMap;

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
    /// Core 07 disposition: excluded identity → finding/evidence pin.
    pub exclusions: BTreeMap<String, String>,
}
/// Declared execution strategy; qualification belongs to the producing evaluator.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Engine {
    /// Exact cell snapshot.
    Cell,
}
/// Snapshot freshness, independent of source cursor ordering.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Freshness {
    /// Complete and within declared source freshness.
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
    /// Operand contributions; none means its predicate excluded this identity.
    pub amounts: Vec<Option<Decimal>>,
}
/// One output bucket, retaining exact sums beside its ratio.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct QueryRow {
    /// Scalar grouping key.
    pub bucket: RuntimeValue,
    /// One sum, or numerator then denominator.
    pub totals: Vec<Decimal>,
    /// Null means a zero-denominator ratio is unavailable.
    pub value: Option<Decimal>,
    /// Exact aggregate-membership lineage.
    pub members: BTreeMap<String, Member>,
}
/// Result envelope for QUERY-ENGINE-1; construction alone grants no authority.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
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
}

pub(crate) type Contributions = BTreeMap<String, (u64, Contribution)>;
fn refused(code: &'static str) -> CellError {
    CellError::ComputeRefused(code)
}
fn decimal(n: i128) -> Result<Decimal, CellError> {
    Decimal::parse(&format!("{n}e-6")).map_err(CellError::Value)
}

impl<E: SqlEngine> CellStore<E> {
    /// COMPUTE-DETERMINISTIC-1 / QUERY-GRAIN-1: evaluate a full bounded current-world snapshot.
    /// Historical knowledge selection remains unsupported; the envelope reports the actual head.
    /// The host supplies server authorization, dependency pins and Core 07 dispositions.
    /// # Errors
    /// Refuses incomplete scans, unsupported semantics, model/scope mismatch or invalid values.
    pub fn evaluate_question(
        &self,
        plan: &QuestionPlan,
        context: QueryContext,
        mut visible: impl FnMut(&ObjectRecord) -> Result<bool, CellError>,
    ) -> Result<QueryResult, CellError> {
        self.conn.transaction(|| {
            if matches!(plan.question().freshness, FreshnessPolicy::AsOf { .. }) {
                return Err(refused(UNSUPPORTED_ENGINE));
            }
            let model = self.model().query_contract(plan.grain().as_str())?;
            if plan.properties().iter().any(|p| {
                model
                    .property(p.id.as_str())
                    .is_none_or(|actual| !p.same_contract(actual))
            }) {
                return Err(refused(UNSUPPORTED_ENGINE));
            }
            let revision = self.verified_head()?;
            let reader = self.read();
            let limits = ReactiveLimits::from_profile(&ResourceLimitProfile::norn_limits_v1());
            let mut budget = DecodeBudget::new(&limits);
            let max = usize::try_from(plan.max_items())
                .map_err(|_bound| refused("core.compute.bound_exceeded"))?;
            let (ids, complete) = reader.root_rows_with_budget(
                &context.world,
                plan.grain().as_str(),
                max,
                &mut budget,
            )?;
            if !complete {
                return Err(refused("core.compute.bound_exceeded"));
            }
            let mut contributions = BTreeMap::new();
            for (id, _) in ids {
                let BudgetedRead::Found(record) = reader.object_with_budget(
                    &context.world,
                    plan.grain().as_str(),
                    &id,
                    &mut budget,
                )?
                else {
                    return Err(refused("core.compute.bound_exceeded"));
                };
                if record.scope != context.scope {
                    return Err(refused("core.compute.stale_input"));
                }
                if !visible(&record)? || context.exclusions.contains_key(&id) {
                    continue;
                }
                if let Some(value) = project(plan, &record.values).map_err(refused)? {
                    contributions.insert(id, (record.version, value));
                }
            }
            let query_pin = crate::codec::hash_json(&(
                self.model().model_hash(),
                plan.question(),
                plan.definitions(),
                plan.max_items(),
                "norn.query/1",
            ))?;
            materialize(
                plan,
                context,
                revision,
                query_pin,
                self.model().model_hash().to_owned(),
                &contributions,
            )
        })
    }
}

pub(crate) fn materialize(
    plan: &QuestionPlan,
    context: QueryContext,
    revision: LedgerHead,
    query_pin: String,
    definition_pin: String,
    contributions: &Contributions,
) -> Result<QueryResult, CellError> {
    type Bucket = (RuntimeValue, Vec<i128>, BTreeMap<String, Member>);
    let mut grouped: BTreeMap<String, Bucket> = BTreeMap::new();
    for (id, (version, contribution)) in contributions {
        let key = String::from_utf8(contribution.bucket.canonical_json()?)
            .map_err(|e| CellError::Encoding(e.to_string()))?;
        let bucket = grouped.entry(key).or_insert_with(|| {
            (
                contribution.bucket.clone(),
                vec![0; plan.sums().len()],
                BTreeMap::new(),
            )
        });
        for (total, amount) in bucket.1.iter_mut().zip(&contribution.amounts) {
            *total = math::add(*total, amount.unwrap_or(0))
                .ok_or_else(|| refused("core.compute.bound_exceeded"))?;
        }
        let amounts = contribution
            .amounts
            .iter()
            .map(|n| n.map(|v| decimal(i128::from(v))).transpose())
            .collect::<Result<_, _>>()?;
        bucket.2.insert(
            id.clone(),
            Member {
                version: *version,
                amounts,
            },
        );
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
                },
            ))
        })
        .collect::<Result<_, CellError>>()?;
    let mut complete = context.exclusions.is_empty();
    let mut stale = false;
    let age = match plan.question().freshness {
        FreshnessPolicy::Live { max_age_ms } | FreshnessPolicy::Delayed { max_age_ms } => {
            max_age_ms.0
        }
        FreshnessPolicy::AsOf { .. } => return Err(refused(UNSUPPORTED_ENGINE)),
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
                    stale |= w
                        .heartbeat_ms
                        .and_then(|at| context.now_ms.checked_sub(at))
                        .is_none_or(|lag| lag > age);
                }
                None => complete = false,
            }
        }
    }
    let lineage_ref = crate::codec::hash_json(&(&query_pin, &revision, &context, &rows))?;
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
        } else {
            Freshness::Exact
        },
        complete,
        context,
    })
}
