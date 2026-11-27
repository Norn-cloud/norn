//! Core 08 (E1, #1944): admitted `QuestionIR` evaluation under a fork's
//! `World` pin and exact host-side world comparison composition.
//!
//! - [`CellStore::evaluate_question_in_fork`] runs the real exact question
//!   evaluator over a forked store, enforces the same read PDP visibility and
//!   quarantine/exclusion rules, stamps `simulated: true`, rebinds
//!   `context.world` to the fork's `WorldId`, and domain-separates both
//!   `fork_query_pin` and `lineage_ref` with the durable [`crate::ForkPin`] so
//!   fork caches, subscriptions, and lineage handles can never collide with
//!   `main` or sibling/nested forks (`WORLD-ISOLATE-1`, `FORK-PIN-1`,
//!   `FORK-EVIDENCE-1`).
//! - [`compare_main_and_fork`] and [`compare_forks`] compose two validated
//!   world answers over the same admitted question identity (`base_query_pin`),
//!   definition pin, grain, scope, and effective window, returning both world
//!   frontiers, result pins, and exact per-bucket `Decimal` deltas without
//!   adding a diff opcode or leaking suppressed buckets (`PRIVACY-1` /
//!   `DATA-VALUES-1`).

use std::collections::{BTreeMap, BTreeSet};

use norn_checker::metric::compute::QuestionPlan;
use norn_ir::{Decimal, ObjectTypeName, RuntimeValue, UtcTimestamp};
use serde::{Deserialize, Serialize};

use super::{FindingEvidence, Frontier, QueryContext, QueryResult, QueryRow, refused};
use crate::engine::SqlEngine;
use crate::{CellError, CellStore, ForkPin, LedgerHead, MAIN_WORLD, ObjectRecord};

const FORK_QUERY_PIN_DOMAIN: &str = "norn.query.fork/1";
const FORK_LINEAGE_DOMAIN: &str = "norn.lineage.fork/1";
const FORK_RESULT_PIN_DOMAIN: &str = "norn.result.fork/1";
const MAIN_RESULT_PIN_DOMAIN: &str = "norn.result.main/1";

/// Admitted question answer evaluated inside a forked world under its durable
/// [`ForkPin`] (`WORLD-ISOLATE-1`, `FORK-PIN-1`, `FORK-EVIDENCE-1`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ForkQuestionResult {
    /// Fork execution world (`World::Fork { id }`), derived from the store pin.
    pub world: norn_kernel::World,
    /// Durable fork pin (`world`, `parent`, `base`, `main_horizon`).
    pub pin: ForkPin,
    /// Explicitly marks this answer as fork-simulated.
    pub simulated: bool,
    /// Pre-fork admitted question pin (`norn.query/1`), shared with `main`
    /// evaluations of the exact same question, model, and definition pin.
    pub base_query_pin: String,
    /// World-bound query pin incorporating the durable fork pin so fork caches
    /// and subscriptions never collide with `main` or sibling/nested forks.
    pub fork_query_pin: String,
    /// World-bound result digest incorporating `fork_query_pin`, `lineage_ref`,
    /// and `revision`.
    pub result_pin: String,
    /// Exact evaluated query result with `context.world`, `query_pin`, and
    /// `lineage_ref` rebound to this fork world.
    pub result: QueryResult,
}

/// Authenticated world frontier and result pin for one side of a world
/// comparison ([`compare_main_and_fork`] / [`compare_forks`]).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WorldFrontierPin {
    /// Execution world (`World::Main` or `World::Fork { id }`).
    pub world: norn_kernel::World,
    /// Whether this side was evaluated in a simulated fork world.
    pub simulated: bool,
    /// Verified ledger head of the store when the question was answered.
    pub revision: LedgerHead,
    /// Actual knowledge commit count.
    pub known_as_of: u64,
    /// Model/definition pin used by the evaluator.
    pub definition_pin: String,
    /// Admitted pre-fork question identity (`norn.query/1`) shared across worlds.
    pub base_query_pin: String,
    /// World-bound query pin.
    pub query_pin: String,
    /// Exact result pin.
    pub result_pin: String,
    /// Consumed source cursor frontier at evaluation time.
    pub source_cursors: Frontier,
}

/// Exact per-bucket comparison delta between a baseline world and a candidate
/// world (`candidate - baseline`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WorldCompareDelta {
    /// Canonical bucket key.
    pub bucket_key: String,
    /// Scalar grouping bucket value.
    pub bucket: RuntimeValue,
    /// Exact baseline value (`None` when absent, zero-denominator ratio, or suppressed).
    pub baseline_value: Option<Decimal>,
    /// Exact candidate value (`None` when absent, zero-denominator ratio, or suppressed).
    pub candidate_value: Option<Decimal>,
    /// Exact arithmetic difference `candidate - baseline` when both sides are
    /// unsuppressed and comparable (`None` when suppressed or ratio-undefined).
    pub delta_value: Option<Decimal>,
    /// Whether either side was suppressed under `PRIVACY-1` / `DATA-VALUES-1`.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub suppressed: bool,
}

/// Host composition result comparing one admitted question across two worlds
/// (e.g., `main` vs decision fork `D_i`, or `D_i` vs `D_j`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WorldCompareResult {
    /// Root contribution grain shared by both answers.
    pub grain: ObjectTypeName,
    /// Tenant scope shared by both answers.
    pub scope: String,
    /// Exclusive effective window end shared by both answers.
    pub effective_as_of: UtcTimestamp,
    /// Baseline world frontier and pins.
    pub baseline: WorldFrontierPin,
    /// Candidate world frontier and pins.
    pub candidate: WorldFrontierPin,
    /// Exact per-bucket deltas in canonical bucket-key order.
    pub deltas: BTreeMap<String, WorldCompareDelta>,
}

/// Domain-separated query pin for a forked world (`FORK-PIN-1`).
///
/// # Errors
/// Canonical JSON encoding errors.
pub fn fork_query_pin(base_query_pin: &str, pin: &ForkPin) -> Result<String, CellError> {
    crate::codec::hash_json(&(base_query_pin, pin, FORK_QUERY_PIN_DOMAIN))
}

/// Content-addressed result pin for a forked world (`FORK-PIN-1`).
///
/// # Errors
/// Canonical JSON encoding errors.
pub fn fork_result_pin(
    fork_query_pin: &str,
    lineage_ref: &str,
    revision: &LedgerHead,
) -> Result<String, CellError> {
    crate::codec::hash_json(&(
        fork_query_pin,
        lineage_ref,
        revision,
        FORK_RESULT_PIN_DOMAIN,
    ))
}

/// Content-addressed pin for a `main`-world [`QueryResult`].
///
/// # Errors
/// Canonical JSON encoding errors.
pub fn main_result_pin(result: &QueryResult) -> Result<String, CellError> {
    crate::codec::hash_json(&(
        &result.query_pin,
        &result.lineage_ref,
        &result.revision,
        MAIN_RESULT_PIN_DOMAIN,
    ))
}

impl<E: SqlEngine> CellStore<E> {
    /// Core 08 (`WORLD-ISOLATE-1`, `FORK-PIN-1`, `FORK-EVIDENCE-1`): evaluate
    /// an admitted [`QuestionPlan`] inside a forked store under its durable
    /// [`ForkPin`].
    ///
    /// Enforces the exact same scope check, read PDP `visible` predicate,
    /// quarantine exclusions, and exact arithmetic as [`Self::evaluate_question`],
    /// then stamps `simulated: true`, binds `context.world` to the fork's world
    /// id, and domain-separates `query_pin` and `lineage_ref` with the fork pin
    /// so fork caches, subscriptions, and lineage refs never collide with `main`
    /// or sibling/nested forks.
    ///
    /// # Errors
    /// `core.world.simulation_forbidden` when called on a non-fork (`main`)
    /// store; `core.world.fork_invalid_world` if `context.world` names an
    /// unrelated world; compute, scope, visibility, or engine errors otherwise.
    pub fn evaluate_question_in_fork(
        &self,
        plan: &QuestionPlan,
        context: QueryContext,
        mut visible: impl FnMut(&ObjectRecord) -> Result<bool, CellError>,
    ) -> Result<ForkQuestionResult, CellError> {
        let Some(pin) = self.fork_pin().cloned() else {
            return Err(CellError::WorldSimulationForbidden);
        };
        self.refuse_closed_fork()?;
        if context.world != MAIN_WORLD && context.world != pin.world {
            return Err(CellError::ForkInvalidWorld);
        }

        // Inside a forked store, inherited and fork-minted rows retain the
        // `MAIN_WORLD` storage label (FORK-PIN-1); every world-boundary
        // decision reads the store's `ForkPin` instead.
        let mut read_context = context;
        MAIN_WORLD.clone_into(&mut read_context.world);

        let mut result = self
            .question_snapshot_in_fork(plan, read_context, &mut visible)?
            .result;

        let base_query_pin = result.query_pin.clone();
        let fork_query_pin = fork_query_pin(&base_query_pin, &pin)?;
        pin.world.clone_into(&mut result.context.world);
        fork_query_pin.clone_into(&mut result.query_pin);
        let evidence = FindingEvidence {
            context: &result.context,
            exclusions: &result.exclusions,
        };
        result.lineage_ref = crate::codec::hash_json(&(
            &fork_query_pin,
            &result.revision,
            evidence,
            &result.rows,
            FORK_LINEAGE_DOMAIN,
        ))?;
        let result_pin = fork_result_pin(&fork_query_pin, &result.lineage_ref, &result.revision)?;

        Ok(ForkQuestionResult {
            world: self.world(),
            pin,
            simulated: true,
            base_query_pin,
            fork_query_pin,
            result_pin,
            result,
        })
    }
}

/// Compare a `main`-world [`QueryResult`] against a simulated
/// [`ForkQuestionResult`] evaluated for the same admitted question.
///
/// # Errors
/// `core.world.fork_invalid_world` if `main_result` is not in `main` or
/// `fork_result` is not a valid simulated fork answer; `core.compute.stale_input`
/// when admitted question identity, definition pin, grain, scope, effective
/// window, or bucket operand arity disagree; `core.compute.bound_exceeded` on
/// exact `Decimal` subtraction overflow.
pub fn compare_main_and_fork(
    main_result: &QueryResult,
    fork_result: &ForkQuestionResult,
) -> Result<WorldCompareResult, CellError> {
    if main_result.context.world != MAIN_WORLD {
        return Err(CellError::ForkInvalidWorld);
    }
    validate_fork_result(fork_result)?;
    let baseline = WorldFrontierPin {
        world: norn_kernel::World::Main,
        simulated: false,
        revision: main_result.revision.clone(),
        known_as_of: main_result.known_as_of,
        definition_pin: main_result.definition_pin.clone(),
        base_query_pin: main_result.query_pin.clone(),
        query_pin: main_result.query_pin.clone(),
        result_pin: main_result_pin(main_result)?,
        source_cursors: main_result.context.source_cursors.clone(),
    };
    let candidate = fork_frontier_pin(fork_result);
    compare_world_questions(baseline, main_result, candidate, &fork_result.result)
}

/// Compare two simulated [`ForkQuestionResult`] answers (for example sibling
/// decision options `D_1` vs `D_2`, or a decision fork `D_1` vs a nested
/// stress scenario `S_1`) for the same admitted question.
///
/// # Errors
/// `core.world.fork_invalid_world` if either answer is not a valid simulated
/// fork result; `core.compute.stale_input` when admitted question identity,
/// definition pin, grain, scope, effective window, or bucket operand arity
/// disagree; `core.compute.bound_exceeded` on exact `Decimal` subtraction
/// overflow.
pub fn compare_forks(
    baseline: &ForkQuestionResult,
    candidate: &ForkQuestionResult,
) -> Result<WorldCompareResult, CellError> {
    validate_fork_result(baseline)?;
    validate_fork_result(candidate)?;
    compare_world_questions(
        fork_frontier_pin(baseline),
        &baseline.result,
        fork_frontier_pin(candidate),
        &candidate.result,
    )
}

fn compare_world_questions(
    baseline: WorldFrontierPin,
    baseline_result: &QueryResult,
    candidate: WorldFrontierPin,
    candidate_result: &QueryResult,
) -> Result<WorldCompareResult, CellError> {
    if baseline.base_query_pin != candidate.base_query_pin
        || baseline_result.definition_pin != candidate_result.definition_pin
        || baseline.definition_pin != baseline_result.definition_pin
        || candidate.definition_pin != candidate_result.definition_pin
        || baseline.query_pin != baseline_result.query_pin
        || candidate.query_pin != candidate_result.query_pin
        || baseline.revision != baseline_result.revision
        || candidate.revision != candidate_result.revision
        || baseline.known_as_of != baseline_result.known_as_of
        || candidate.known_as_of != candidate_result.known_as_of
        || (!baseline.simulated && baseline.base_query_pin != baseline.query_pin)
        || (!candidate.simulated && candidate.base_query_pin != candidate.query_pin)
        || baseline_result.grain != candidate_result.grain
        || baseline_result.effective_as_of != candidate_result.effective_as_of
        || baseline_result.context.scope != candidate_result.context.scope
    {
        return Err(refused("core.compute.stale_input"));
    }

    let keys: BTreeSet<&String> = baseline_result
        .rows
        .keys()
        .chain(candidate_result.rows.keys())
        .collect();

    let mut deltas = BTreeMap::new();
    for key in keys {
        let base_row = baseline_result.rows.get(key);
        let cand_row = candidate_result.rows.get(key);
        let delta = bucket_delta(key, base_row, cand_row)?;
        deltas.insert(key.clone(), delta);
    }

    Ok(WorldCompareResult {
        grain: baseline_result.grain.clone(),
        scope: baseline_result.context.scope.clone(),
        effective_as_of: baseline_result.effective_as_of.clone(),
        baseline,
        candidate,
        deltas,
    })
}

fn validate_fork_result(fork: &ForkQuestionResult) -> Result<(), CellError> {
    let valid_world = matches!(
        &fork.world,
        norn_kernel::World::Fork { id } if id.0 == fork.pin.world
    );
    if !fork.simulated
        || !valid_world
        || fork.result.context.world != fork.pin.world
        || fork.result.query_pin != fork.fork_query_pin
    {
        return Err(CellError::ForkInvalidWorld);
    }
    if fork_query_pin(&fork.base_query_pin, &fork.pin)? != fork.fork_query_pin {
        return Err(CellError::ForkInvalidWorld);
    }
    if fork_result_pin(
        &fork.fork_query_pin,
        &fork.result.lineage_ref,
        &fork.result.revision,
    )? != fork.result_pin
    {
        return Err(CellError::ForkInvalidWorld);
    }
    Ok(())
}

fn fork_frontier_pin(fork: &ForkQuestionResult) -> WorldFrontierPin {
    WorldFrontierPin {
        world: fork.world.clone(),
        simulated: fork.simulated,
        revision: fork.result.revision.clone(),
        known_as_of: fork.result.known_as_of,
        definition_pin: fork.result.definition_pin.clone(),
        base_query_pin: fork.base_query_pin.clone(),
        query_pin: fork.fork_query_pin.clone(),
        result_pin: fork.result_pin.clone(),
        source_cursors: fork.result.context.source_cursors.clone(),
    }
}

fn bucket_delta(
    key: &str,
    base_row: Option<&QueryRow>,
    cand_row: Option<&QueryRow>,
) -> Result<WorldCompareDelta, CellError> {
    let bucket = cand_row
        .or(base_row)
        .map_or(RuntimeValue::Null, |row| row.bucket.clone());

    let suppressed =
        base_row.is_some_and(|r| r.suppressed) || cand_row.is_some_and(|r| r.suppressed);
    if suppressed {
        return Ok(WorldCompareDelta {
            bucket_key: key.to_owned(),
            bucket,
            baseline_value: None,
            candidate_value: None,
            delta_value: None,
            suppressed: true,
        });
    }

    if let (Some(base), Some(cand)) = (base_row, cand_row)
        && base.totals.len() != cand.totals.len()
    {
        return Err(refused("core.compute.stale_input"));
    }

    let baseline_value = base_row.and_then(|row| row.value);
    let candidate_value = cand_row.and_then(|row| row.value);
    let delta_value = match (base_row, cand_row) {
        (Some(base), Some(cand)) => match (base.value, cand.value) {
            (Some(b), Some(c)) => Some(
                c.checked_sub(b)
                    .ok_or_else(|| refused("core.compute.bound_exceeded"))?,
            ),
            _ => None,
        },
        // Absent baseline bucket on a single-operand sum metric has exact baseline 0.
        (None, Some(cand)) if cand.totals.len() == 1 => cand.value,
        // Absent candidate bucket on a single-operand sum metric has exact candidate 0.
        (Some(base), None) if base.totals.len() == 1 => match base.value {
            Some(b) => Some(
                Decimal::ZERO
                    .checked_sub(b)
                    .ok_or_else(|| refused("core.compute.bound_exceeded"))?,
            ),
            None => None,
        },
        _ => None,
    };

    Ok(WorldCompareDelta {
        bucket_key: key.to_owned(),
        bucket,
        baseline_value,
        candidate_value,
        delta_value,
        suppressed: false,
    })
}
