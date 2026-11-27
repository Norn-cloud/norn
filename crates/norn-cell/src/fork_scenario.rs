//! Core 08 (E2, #1945, `WORLD-FORK`): explicit virtual scenario clock
//! (effective vs. recorded time), recorded deterministic entropy, pinned
//! definitions/parameters, and external-result fixture dispatch for forked
//! worlds.
//!
//! [`ForkScenario`] wraps a forked [`SqliteCell`] with an explicit
//! [`WorldContext`] so scenario runs:
//! - separate virtual effective business time from virtual recorded ledger
//!   time ([`ScenarioClock`]), propagating `effective` into both
//!   [`ScenarioObservations`] and [`QuestionPlan::with_effective_end`] while
//!   keeping `recorded` strictly synchronized with the underlying cell clock;
//! - bind `definition_pin` directly from the store's active definition pin and
//!   bind `parameter_pins` into deterministic recorded [`Entropy`] derivation;
//! - resolve pending outbox commands atomically from declared external-result
//!   fixtures (`FORK-SIMULATE-1`), refusing `core.world.perform_forbidden`
//!   whenever no fixture is registered so no live responder is ever invoked
//!   (`WORLD-ISOLATE-1`);
//! - support nested scenario forks (`D_i -> S_i`) inheriting `main_horizon`
//!   unchanged (`FORK-PIN-1`, `FORK-EVIDENCE-1`) while keeping exploratory
//!   clock advances, entropy draws, and fixtures strictly isolated in `S_i`.

use std::collections::BTreeMap;

use norn_checker::metric::compute::QuestionPlan;
use norn_ir::{RuntimeValue, UtcTimestamp};
use norn_kernel::{
    CellCommit, CellHost, Command, CommandId, CommitError, CommitReceipt, DueTimer, Entropy,
    Generation, LogicalTime, Performed, ScenarioClock, ScenarioObservations, World, WorldId,
};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use transparency_sha2::{Digest, Sha256};

use crate::compute::{ForkQuestionResult, QueryContext};
use crate::engine::{DefaultEngine, SqlEngine};
use crate::error::CellError;
use crate::fork::{ForkEngine, ForkPin};
use crate::host::SqliteCell;
use crate::read::{ObjectRecord, PendingCommand};
use crate::store::{CellStore, MAIN_WORLD};

const FORK_ENTROPY_DOMAIN: &[u8] = b"norn.fork.entropy/1";

/// Canonical map key for one external-result fixture (`<command_id>@<generation>`).
#[must_use]
pub fn fixture_key(command_id: &str, generation: u64) -> String {
    format!("{command_id}@{generation}")
}

/// One deterministically derived entropy sample recorded during a fork
/// scenario (`WORLD-FORK`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RecordedEntropy {
    /// Zero-based monotonic draw index within this fork scenario.
    pub step: u64,
    /// Caller-supplied semantic label bound into the entropy derivation.
    pub label: String,
    /// Derived 32-byte entropy value.
    pub entropy: Entropy,
}

/// Core 08 `World context` record for a forked execution world: world ID/kind,
/// scope, immutable base revision (`ForkPin`), pinned definitions/parameters,
/// virtual effective + recorded clocks, recorded entropy, and external-result
/// fixtures.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WorldContext {
    /// Fork execution world (`World::Fork { id }`), derived from the store pin.
    pub world: World,
    /// Immutable durable fork pin (`world`, `parent`, `base`, `main_horizon`).
    pub pin: ForkPin,
    /// Tenant scope bound to the cell.
    pub scope: String,
    /// Store-authenticated active definition set / model digest.
    pub definition_pin: String,
    /// Pinned scenario parameter overrides (canonical key order).
    pub parameter_pins: BTreeMap<String, RuntimeValue>,
    /// Explicit virtual effective business clock and recorded ledger clock.
    pub clock: ScenarioClock,
    /// Base 32-byte seed from which every step's recorded entropy is derived.
    pub entropy_seed: Entropy,
    /// Next monotonic entropy derivation index.
    pub entropy_step: u64,
    /// Ordered audit log of every entropy sample derived in this scenario.
    pub recorded_entropy: Vec<RecordedEntropy>,
    /// Registered external-result fixtures keyed by [`fixture_key`].
    pub fixtures: BTreeMap<String, RuntimeValue>,
}

/// Deterministic scenario driver over a forked [`SqliteCell`] (`WORLD-FORK`,
/// `WORLD-ISOLATE-1`, `FORK-PIN-1`, `FORK-SIMULATE-1`).
pub struct ForkScenario<E, C, I, S: SqlEngine = DefaultEngine> {
    cell: SqliteCell<E, C, I, S>,
    context: WorldContext,
}

impl<E, C, I, S: SqlEngine> ForkScenario<E, C, I, S> {
    /// Open a simulated scenario over a forked [`SqliteCell`].
    ///
    /// Authenticates `definition_pin` from the store's active definition set
    /// and aligns the cell's recorded clock to `clock.recorded` without
    /// allowing recorded time to move backward behind either the persisted
    /// clock floor or the cell's current logical clock.
    ///
    /// # Errors
    /// `core.world.simulation_forbidden` if `cell` is not a fork (`main`);
    /// `core.compute.stale_input` if `clock.recorded` precedes the store's
    /// clock floor or `cell.now()`; store read errors otherwise.
    pub fn open(
        mut cell: SqliteCell<E, C, I, S>,
        parameter_pins: BTreeMap<String, RuntimeValue>,
        clock: ScenarioClock,
        entropy_seed: Entropy,
    ) -> Result<Self, CellError> {
        let Some(pin) = cell.store().fork_pin().cloned() else {
            return Err(CellError::WorldSimulationForbidden);
        };
        cell.store().refuse_closed_fork()?;
        let min_recorded = cell
            .store()
            .read()
            .clock_floor()?
            .max(cell.now().as_millis());
        if clock.recorded.as_millis() < min_recorded {
            return Err(CellError::ComputeRefused("core.compute.stale_input"));
        }
        let definition_pin = cell.store().current_definition_pin()?;
        cell.advance_to(clock.recorded);
        let context = WorldContext {
            world: cell.store().world(),
            pin,
            scope: cell.scope.clone(),
            definition_pin,
            parameter_pins,
            clock,
            entropy_seed,
            entropy_step: 0,
            recorded_entropy: Vec::new(),
            fixtures: BTreeMap::new(),
        };
        Ok(Self { cell, context })
    }

    /// Read-only view of this scenario's [`WorldContext`].
    #[must_use]
    pub const fn context(&self) -> &WorldContext {
        &self.context
    }

    /// Current virtual effective and recorded clocks.
    #[must_use]
    pub const fn clock(&self) -> ScenarioClock {
        self.context.clock
    }

    /// Look up one pinned scenario parameter.
    #[must_use]
    pub fn parameter(&self, key: &str) -> Option<&RuntimeValue> {
        self.context.parameter_pins.get(key)
    }

    /// Read-only borrow of the underlying forked [`SqliteCell`].
    #[must_use]
    pub const fn cell(&self) -> &SqliteCell<E, C, I, S> {
        &self.cell
    }

    /// Read-only borrow of the underlying forked [`CellStore`].
    #[must_use]
    pub const fn store(&self) -> &CellStore<S> {
        self.cell.store()
    }

    /// Unwrap the underlying forked [`SqliteCell`].
    #[must_use]
    pub fn into_cell(self) -> SqliteCell<E, C, I, S> {
        self.cell
    }

    /// FORK-LIFE-1: durable lifecycle state of this scenario's fork.
    ///
    /// # Errors
    /// As [`SqliteCell::fork_state`].
    pub fn fork_state(&self) -> Result<Option<crate::fork::ForkState>, CellError> {
        self.cell.fork_state()
    }

    /// FORK-LIFE-1: close this scenario's fork as `discarded`.
    ///
    /// # Errors
    /// As [`SqliteCell::discard_fork`].
    pub fn discard_fork(&mut self) -> Result<(), CellError> {
        self.cell.discard_fork()
    }

    /// FORK-LIFE-1: close this scenario's fork as `expired`.
    ///
    /// # Errors
    /// As [`SqliteCell::expire_fork`].
    pub fn expire_fork(&mut self) -> Result<(), CellError> {
        self.cell.expire_fork()
    }

    /// Monotonically advance the virtual effective clock without moving
    /// recorded ledger time.
    ///
    /// # Errors
    /// `core.compute.stale_input` if `effective` precedes the current effective clock.
    pub fn advance_effective_to(
        &mut self,
        effective: LogicalTime,
    ) -> Result<ScenarioClock, CellError> {
        let next = self
            .context
            .clock
            .advance_effective_to(effective)
            .ok_or(CellError::ComputeRefused("core.compute.stale_input"))?;
        self.context.clock = next;
        Ok(next)
    }

    /// Advance the virtual effective clock by `delta_ms` without moving
    /// recorded ledger time.
    ///
    /// # Errors
    /// `core.compute.bound_exceeded` on timestamp overflow.
    pub fn advance_effective_by(&mut self, delta_ms: u64) -> Result<ScenarioClock, CellError> {
        let next = self
            .context
            .clock
            .advance_effective_by(delta_ms)
            .ok_or(CellError::ComputeRefused("core.compute.bound_exceeded"))?;
        self.context.clock = next;
        Ok(next)
    }

    /// Monotonically advance the virtual recorded clock and synchronize the
    /// underlying [`SqliteCell`]'s logical clock for commits and due timers.
    ///
    /// # Errors
    /// `core.compute.stale_input` if `recorded` precedes the current recorded clock.
    pub fn advance_recorded_to(
        &mut self,
        recorded: LogicalTime,
    ) -> Result<ScenarioClock, CellError> {
        let next = self
            .context
            .clock
            .advance_recorded_to(recorded)
            .ok_or(CellError::ComputeRefused("core.compute.stale_input"))?;
        self.context.clock = next;
        self.cell.advance_to(recorded);
        Ok(next)
    }

    /// Advance the virtual recorded clock by `delta_ms` and synchronize the
    /// underlying [`SqliteCell`]'s logical clock.
    ///
    /// # Errors
    /// `core.compute.bound_exceeded` on timestamp overflow.
    pub fn advance_recorded_by(&mut self, delta_ms: u64) -> Result<ScenarioClock, CellError> {
        let next = self
            .context
            .clock
            .advance_recorded_by(delta_ms)
            .ok_or(CellError::ComputeRefused("core.compute.bound_exceeded"))?;
        self.context.clock = next;
        self.cell.advance_to(next.recorded);
        Ok(next)
    }

    /// Advance both effective and recorded clocks by `delta_ms` and synchronize
    /// the underlying [`SqliteCell`]'s logical clock.
    ///
    /// # Errors
    /// `core.compute.bound_exceeded` on timestamp overflow.
    pub fn advance_both_by(&mut self, delta_ms: u64) -> Result<ScenarioClock, CellError> {
        let next = self
            .context
            .clock
            .advance_both_by(delta_ms)
            .ok_or(CellError::ComputeRefused("core.compute.bound_exceeded"))?;
        self.context.clock = next;
        self.cell.advance_to(next.recorded);
        Ok(next)
    }

    /// Deterministically derive and record the next 32 bytes of scenario
    /// entropy bound to `(world, base.hash, definition_pin, parameter_pins,
    /// entropy_seed, step, label)`.
    ///
    /// # Errors
    /// `core.compute.bound_exceeded` if `entropy_step` overflows `u64`, or
    /// canonical parameter-pin encoding errors.
    pub fn next_entropy(&mut self, label: &str) -> Result<Entropy, CellError> {
        let step = self.context.entropy_step;
        let next_step = step
            .checked_add(1)
            .ok_or(CellError::ComputeRefused("core.compute.bound_exceeded"))?;
        let params_digest = crate::codec::hash_json(&self.context.parameter_pins)?;
        let entropy = derive_step_entropy(
            &self.context.pin.world,
            &self.context.pin.base.hash,
            &self.context.definition_pin,
            &params_digest,
            self.context.entropy_seed,
            step,
            label,
        );
        self.context.entropy_step = next_step;
        self.context.recorded_entropy.push(RecordedEntropy {
            step,
            label: label.to_owned(),
            entropy,
        });
        Ok(entropy)
    }

    /// Produce replayable bitemporal [`ScenarioObservations`] for the next fork
    /// turn under this scenario's virtual effective + recorded clocks, derived
    /// recorded entropy, and fork world.
    ///
    /// # Errors
    /// As [`Self::next_entropy`].
    pub fn observations(&mut self, label: &str) -> Result<ScenarioObservations, CellError> {
        let entropy = self.next_entropy(label)?;
        Ok(ScenarioObservations {
            clock: self.context.clock,
            entropy,
            world: World::Fork {
                id: WorldId(self.context.pin.world.clone()),
            },
        })
    }

    /// Pending outbox commands in this fork store.
    ///
    /// # Errors
    /// Store read errors.
    pub fn pending_commands(&self) -> Result<Vec<PendingCommand>, CellError> {
        self.cell.store().read().pending_commands()
    }

    /// Validate a caller-supplied [`QueryContext`] against this scenario's
    /// bound scope and fork world, then bind `now_ms` to the scenario's virtual
    /// recorded clock without relabelling foreign scopes or sibling worlds.
    ///
    /// # Errors
    /// `core.compute.stale_input` if `base.scope` disagrees with this cell's
    /// scope; `core.world.fork_invalid_world` if `base.world` names an
    /// unrelated world.
    pub fn query_context(&self, mut base: QueryContext) -> Result<QueryContext, CellError> {
        if base.scope != self.context.scope {
            return Err(CellError::ComputeRefused("core.compute.stale_input"));
        }
        if base.world != MAIN_WORLD && base.world != self.context.pin.world {
            return Err(CellError::ForkInvalidWorld);
        }
        base.now_ms = self.context.clock.recorded.as_millis();
        Ok(base)
    }

    /// Evaluate an admitted [`QuestionPlan`] inside this scenario fork under
    /// its durable [`ForkPin`], authenticated `definition_pin`, virtual
    /// effective cutoff (`min(clock.effective, plan.time_window.end)`), and
    /// virtual recorded clock (`now_ms = clock.recorded`).
    ///
    /// # Errors
    /// `core.compute.stale_input` if the store's active definition pin drifted,
    /// if `base_context.scope` mismatches, or if `clock.effective` precedes
    /// `plan.question().time_window.start`; `core.world.fork_invalid_world` on
    /// an unrelated `base_context.world`; evaluator errors otherwise.
    pub fn evaluate_question(
        &self,
        plan: &QuestionPlan,
        base_context: QueryContext,
        visible: impl FnMut(&ObjectRecord) -> Result<bool, CellError>,
    ) -> Result<ForkQuestionResult, CellError> {
        if self.cell.store().current_definition_pin()? != self.context.definition_pin {
            return Err(CellError::ComputeRefused("core.compute.stale_input"));
        }
        let context = self.query_context(base_context)?;
        let effective_ts = millis_to_utc_timestamp(self.context.clock.effective.as_millis())?;
        let effective_plan = plan
            .with_effective_end(effective_ts)
            .map_err(CellError::ComputeRefused)?;
        self.cell
            .store()
            .evaluate_question_in_fork(&effective_plan, context, visible)
    }
}

impl<E, C, I, S: SqlEngine> ForkScenario<E, C, I, S>
where
    I: Serialize,
{
    /// Register an exact simulated external-result fixture for `(command_id, generation)`.
    ///
    /// # Errors
    /// Exact runtime encoding errors.
    pub fn register_fixture(
        &mut self,
        command_id: &str,
        generation: u64,
        outcome: &I,
    ) -> Result<(), CellError> {
        let encoded = norn_ir::runtime::encode(outcome).map_err(CellError::Encoding)?;
        self.context
            .fixtures
            .insert(fixture_key(command_id, generation), encoded);
        Ok(())
    }
}

impl<E, C, I, S: SqlEngine> ForkScenario<E, C, I, S>
where
    E: Serialize + 'static,
    C: Serialize + DeserializeOwned,
    I: Serialize + DeserializeOwned,
{
    /// Commit a fork turn stamped at this scenario's virtual recorded clock.
    ///
    /// # Errors
    /// As [`CellHost::commit`].
    pub fn commit(
        &mut self,
        commit: CellCommit<E, C, I>,
    ) -> Result<CommitReceipt, CommitError<CellError>> {
        self.cell.commit(commit)
    }

    /// Peek the next pending command result through [`CellHost::perform`].
    ///
    /// Refuses `core.world.perform_forbidden` before any claim when the head
    /// command has no recorded simulated result; redelivers the simulated
    /// result once [`Self::apply_fixture_perform`] has recorded it.
    ///
    /// # Errors
    /// As [`CellHost::perform`].
    pub fn perform(&mut self) -> Result<Option<Performed<I>>, CellError> {
        self.cell.perform()
    }

    /// Peek the next timer due at this scenario's virtual recorded clock.
    ///
    /// # Errors
    /// As [`CellHost::clock`].
    pub fn clock_due(&mut self) -> Result<Option<DueTimer<I>>, CellError> {
        self.cell.clock()
    }

    /// `WORLD-FORK` / `FORK-SIMULATE-1`: resolve the oldest pending outbox
    /// command atomically from this scenario's registered fixtures and record
    /// it for that exact `(command_id, generation)` without claiming,
    /// dispatching, or re-reading the head.
    ///
    /// Returns `Ok(None)` when the outbox is empty, the head already has a
    /// recorded simulated result (which [`Self::perform`] redelivers), or the
    /// guarded store write observes a concurrent head change. Refuses
    /// `core.world.perform_forbidden` if a pending command has no registered
    /// fixture so a fork can never fall back to a live responder.
    ///
    /// # Errors
    /// `core.world.perform_forbidden` if no fixture is registered for the
    /// pending head command; `core.world.simulation_forbidden` outside a fork;
    /// decode or store errors otherwise.
    pub fn apply_fixture_perform(&mut self) -> Result<Option<Performed<I>>, CellError> {
        let Some(head) = self.cell.store().read().head_command()? else {
            return Ok(None);
        };
        if head.result.is_some() {
            return Ok(None);
        }
        let key = fixture_key(&head.command.command_id, head.command.generation);
        let Some(encoded) = self.context.fixtures.get(&key).cloned() else {
            return Err(CellError::WorldPerformForbidden);
        };
        let outcome: I = norn_ir::runtime::decode(&encoded).map_err(CellError::Encoding)?;
        if !self.cell.record_simulated_head(
            &head.command.command_id,
            head.command.generation,
            &encoded,
        )? {
            return Ok(None);
        }
        Ok(Some(Performed {
            command: CommandId(head.command.command_id),
            generation: Generation(head.command.generation),
            input: outcome,
        }))
    }
}

impl<E, C, I, S: ForkEngine> ForkScenario<E, C, I, S>
where
    E: Serialize + 'static,
    C: Serialize + DeserializeOwned,
    I: Serialize + DeserializeOwned,
{
    /// Fork an isolated nested scenario (`D_i -> S_i`) at this scenario's
    /// current verified head, inheriting `main_horizon` unchanged
    /// (`FORK-PIN-1`, `FORK-EVIDENCE-1`), along with `parameter_pins` and
    /// registered fixtures.
    ///
    /// Exploratory clock advances, entropy draws, timers, and additional
    /// fixtures applied to `S_i` stay strictly inside `S_i` and never
    /// contaminate `D_i` or `main`.
    ///
    /// # Errors
    /// As [`crate::CellStore::fork`] and [`Self::open`].
    pub fn fork_nested(
        &self,
        world: &str,
        clock: Option<ScenarioClock>,
        entropy_seed: Option<Entropy>,
        responder: impl FnMut(&Command<C>) -> I + Send + 'static,
    ) -> Result<Self, CellError> {
        let head = self.cell.store().verified_head()?;
        let nested_store = self.cell.store().fork(world, &head)?;
        let nested_cell = SqliteCell::new(nested_store, &self.context.scope, responder)?;
        let mut nested = Self::open(
            nested_cell,
            self.context.parameter_pins.clone(),
            clock.unwrap_or(self.context.clock),
            entropy_seed.unwrap_or(self.context.entropy_seed),
        )?;
        self.context
            .fixtures
            .clone_into(&mut nested.context.fixtures);
        Ok(nested)
    }
}

fn millis_to_utc_timestamp(millis: u64) -> Result<UtcTimestamp, CellError> {
    let signed =
        i64::try_from(millis).map_err(|_| CellError::ComputeRefused("core.compute.stale_input"))?;
    let ts = jiff::Timestamp::from_millisecond(signed)
        .map_err(|_| CellError::ComputeRefused("core.compute.stale_input"))?;
    Ok(UtcTimestamp(ts.to_string()))
}

fn derive_step_entropy(
    world: &str,
    base_hash: &str,
    definition_pin: &str,
    params_digest: &str,
    seed: Entropy,
    step: u64,
    label: &str,
) -> Entropy {
    let mut hasher = Sha256::new();
    hasher.update(FORK_ENTROPY_DOMAIN);
    hasher.update(0_u8.to_le_bytes());
    for part in [world, base_hash, definition_pin, params_digest] {
        hasher.update(u64::try_from(part.len()).unwrap_or(u64::MAX).to_le_bytes());
        hasher.update(part.as_bytes());
    }
    hasher.update(seed.as_bytes());
    hasher.update(step.to_le_bytes());
    hasher.update(u64::try_from(label.len()).unwrap_or(u64::MAX).to_le_bytes());
    hasher.update(label.as_bytes());
    let digest = hasher.finalize();
    let mut bytes = [0_u8; 32];
    bytes.copy_from_slice(&digest);
    Entropy::from_bytes(bytes)
}
