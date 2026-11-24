//! Seeded multi-entity stateful DST over any admitted pack (#1656, #181, #176).
//!
//! Generates seeded, shrinkable operation sequences (`Create`, `Transition`,
//! `MutateNonLifecycle`, `AdvanceClock`, `CrashAndReopen`) via `proptest`,
//! drives a real [`PackHost`] (such as file-backed `SqliteCell`), compares every
//! step against an independent reference model, enforces zero ledger writes on
//! refusals, and runs `verify_ledger()` after every crash/reopen and at run end.
//! Evidence level: [`EvidenceLevel::Sampled`].

use std::collections::{BTreeMap, BTreeSet};

use norn_kernel::cell_host::{FactKey, JsonObject, JsonValue};
use norn_kernel::norn_ir::{
    DecisionOutput, FactChange, Lifecycle, ObjectTypeDef, ObjectTypeName, PropertyId, TransitionId,
    lifecycle_codes,
};
use norn_kernel::{DecisionTurn, TypeCatalog, World, commit_decision, load_slice};
use proptest::prelude::*;
use proptest::test_runner::{Config, RngAlgorithm, TestError, TestRng, TestRunner};

use super::coverage::{DomainCoverage, DomainTransition};
use super::synth::{pick_non_lifecycle_mutation, synthesize_guard_satisfying};
use super::{AdmittedPack, EvidenceLevel, PackHost, action_call_object_type};

/// Configuration for a seeded multi-entity DST run over an [`AdmittedPack`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PackDstConfig {
    /// Deterministic RNG seed.
    pub seed: [u8; 32],
    /// Number of randomized multi-entity cases.
    pub cases: u32,
    /// Maximum operations per case.
    pub max_steps: usize,
    /// Number of entity instances per lifecycle-bearing type.
    pub entities_per_type: u8,
}

impl Default for PackDstConfig {
    fn default() -> Self {
        Self {
            seed: [0xA5; 32],
            cases: 24,
            max_steps: 20,
            entities_per_type: 2,
        }
    }
}

/// One generated operation in a shrinkable multi-entity DST trace.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PackDstStep {
    /// Create entity `(type_index, slot)`.
    Create {
        /// Index into the pack's lifecycle-bearing types.
        type_idx: usize,
        /// Entity slot index.
        slot: u8,
    },
    /// Fire a declared transition `(type_index, slot, transition_index)`.
    FireTransition {
        /// Index into the pack's lifecycle-bearing types.
        type_idx: usize,
        /// Entity slot index.
        slot: u8,
        /// Index into the lifecycle's transitions.
        transition_idx: usize,
    },
    /// Attempt a non-lifecycle property mutation on `(type_index, slot)`.
    MutateNonLifecycle {
        /// Index into the pack's lifecycle-bearing types.
        type_idx: usize,
        /// Entity slot index.
        slot: u8,
        /// Deterministic value nonce.
        nonce: u64,
    },
    /// Advance the virtual cell clock (`#176`).
    AdvanceClock {
        /// Milliseconds to advance.
        delta_millis: u64,
    },
    /// Close and reopen durable storage, verifying `verify_ledger()`.
    CrashAndReopen,
}

/// Summary of one completed seeded DST run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PackDstReport {
    /// Evidence classification (`sampled`).
    pub evidence: EvidenceLevel,
    /// Cases executed.
    pub cases_run: u32,
    /// Total steps executed across all cases.
    pub steps_run: usize,
    /// Total crash/reopen + `verify_ledger()` cycles verified.
    pub crashes_recovered: usize,
    /// Domain coverage hits accumulated during random walks.
    pub coverage: DomainCoverage,
}

#[derive(Debug, Clone)]
struct TargetSpec<'a> {
    object_type: &'a ObjectTypeDef,
    lifecycle: Lifecycle,
}

#[derive(Debug, Clone)]
struct RefEntity {
    state: String,
    values: JsonObject,
}

struct DstCaseState<'a, H> {
    pack: &'a AdmittedPack,
    catalog: &'a TypeCatalog<'a>,
    targets: &'a [TargetSpec<'a>],
    host: &'a mut H,
    model: BTreeMap<(usize, u8), RefEntity>,
    coverage: DomainCoverage,
    crashes: usize,
}

/// Runs seeded, shrinkable multi-entity stateful DST on `pack`.
///
/// # Errors
///
/// Returns the minimized failing step sequence and divergence reason if the host
/// disagrees with the independent reference model or fails `verify_ledger()`.
pub fn run_pack_dst<H, F>(
    pack: &AdmittedPack,
    config: PackDstConfig,
    mut make_host: F,
) -> Result<PackDstReport, String>
where
    H: PackHost,
    F: FnMut(u32) -> Result<H, String>,
{
    let extra = [action_call_object_type()];
    let catalog = pack.type_catalog(&extra);
    let targets: Vec<TargetSpec<'_>> = pack
        .object_types
        .iter()
        .filter_map(|ty| {
            let lifecycles = catalog
                .effective_lifecycles(&ty.name)
                .unwrap_or_else(|_| ty.lifecycles.clone());
            lifecycles
                .into_iter()
                .next()
                .filter(|lc| !lc.initial.is_empty() && !lc.transitions.is_empty())
                .map(|lifecycle| TargetSpec {
                    object_type: ty,
                    lifecycle,
                })
        })
        .collect();

    if targets.is_empty() {
        return Ok(PackDstReport {
            evidence: EvidenceLevel::Sampled,
            cases_run: 0,
            steps_run: 0,
            crashes_recovered: 0,
            coverage: DomainCoverage::default(),
        });
    }

    let strategy = dst_sequence_strategy(&targets, config.entities_per_type, config.max_steps);
    let proptest_cfg = Config {
        cases: config.cases,
        failure_persistence: None,
        ..Config::default()
    };
    let rng = TestRng::from_seed(RngAlgorithm::ChaCha, &config.seed);
    let mut runner = TestRunner::new_with_rng(proptest_cfg, rng);
    let mut case_counter = 0u32;
    let mut total_steps = 0usize;
    let mut total_crashes = 0usize;
    let mut total_coverage = DomainCoverage::default();

    let run_result = runner.run(&strategy, |steps| {
        case_counter = case_counter.saturating_add(1);
        let mut host = make_host(case_counter)
            .map_err(|err| TestCaseError::fail(format!("host factory: {err}")))?;
        let mut state = DstCaseState {
            pack,
            catalog: &catalog,
            targets: &targets,
            host: &mut host,
            model: BTreeMap::new(),
            coverage: DomainCoverage::default(),
            crashes: 0,
        };
        state.execute_all(&steps).map_err(TestCaseError::fail)?;
        total_steps = total_steps.saturating_add(steps.len());
        total_crashes = total_crashes.saturating_add(state.crashes);
        total_coverage.merge(&state.coverage);
        Ok(())
    });

    match run_result {
        Ok(()) => Ok(PackDstReport {
            evidence: EvidenceLevel::Sampled,
            cases_run: case_counter,
            steps_run: total_steps,
            crashes_recovered: total_crashes,
            coverage: total_coverage,
        }),
        Err(TestError::Fail(reason, minimal_steps)) => Err(format!(
            "pack DST failed on pack '{}': {reason}; minimal sequence: {minimal_steps:?}",
            pack.name
        )),
        Err(TestError::Abort(reason)) => {
            Err(format!("pack DST aborted on pack '{}': {reason}", pack.name))
        }
    }
}

fn dst_sequence_strategy(
    targets: &[TargetSpec<'_>],
    entities_per_type: u8,
    max_steps: usize,
) -> BoxedStrategy<Vec<PackDstStep>> {
    let type_count = targets.len();
    let max_transitions = targets
        .iter()
        .map(|t| t.lifecycle.transitions.len())
        .max()
        .unwrap_or(1);
    let slots = entities_per_type.max(1);
    let step = prop_oneof![
        3 => (0..type_count, 0..slots).prop_map(|(type_idx, slot)| PackDstStep::Create { type_idx, slot }),
        5 => (0..type_count, 0..slots, 0..max_transitions).prop_map(|(type_idx, slot, transition_idx)| {
            PackDstStep::FireTransition { type_idx, slot, transition_idx }
        }),
        2 => (0..type_count, 0..slots, 1u64..1000u64).prop_map(|(type_idx, slot, nonce)| {
            PackDstStep::MutateNonLifecycle { type_idx, slot, nonce }
        }),
        1 => (1_000u64..600_000u64).prop_map(|delta_millis| PackDstStep::AdvanceClock { delta_millis }),
        1 => Just(PackDstStep::CrashAndReopen),
    ];
    proptest::collection::vec(step, 1..=max_steps.max(1)).boxed()
}

impl<H: PackHost> DstCaseState<'_, H> {
    fn execute_all(&mut self, steps: &[PackDstStep]) -> Result<(), String> {
        for (step_idx, step) in steps.iter().enumerate() {
            let nonce = u64::try_from(step_idx).unwrap_or(0).saturating_add(1);
            match step {
                PackDstStep::Create { type_idx, slot } => {
                    self.step_create(*type_idx, *slot, nonce)?;
                }
                PackDstStep::FireTransition {
                    type_idx,
                    slot,
                    transition_idx,
                } => {
                    self.step_transition(*type_idx, *slot, *transition_idx, nonce)?;
                }
                PackDstStep::MutateNonLifecycle {
                    type_idx,
                    slot,
                    nonce: val_nonce,
                } => {
                    self.step_mutate(*type_idx, *slot, *val_nonce, nonce)?;
                }
                PackDstStep::AdvanceClock { delta_millis } => {
                    let before = self.host.logical_now();
                    self.host.advance_clock(*delta_millis);
                    if self.host.logical_now() < before {
                        return Err("virtual clock moved backwards".to_owned());
                    }
                }
                PackDstStep::CrashAndReopen => {
                    let before_ledger = self.host.ledger_len();
                    self.host.crash_and_reopen()?;
                    self.host.verify_ledger()?;
                    if self.host.ledger_len() != before_ledger {
                        return Err(format!(
                            "ledger length changed across crash/reopen: before={before_ledger}, after={}",
                            self.host.ledger_len()
                        ));
                    }
                    self.verify_model_matches_host()?;
                    self.crashes = self.crashes.saturating_add(1);
                }
            }
        }
        self.host.verify_ledger()?;
        self.verify_model_matches_host()
    }

    fn step_create(&mut self, type_idx: usize, slot: u8, nonce: u64) -> Result<(), String> {
        let Some(target) = self.targets.get(type_idx) else {
            return Ok(());
        };
        let ty_name = target.object_type.name.as_str();
        let prop = target.lifecycle.property.as_str();
        let Some(init_state) = target.lifecycle.initial.first() else {
            return Ok(());
        };
        let id = entity_id(type_idx, slot);
        let all_guards: Vec<_> = target
            .lifecycle
            .transitions
            .iter()
            .flat_map(|tr| tr.guards.iter().cloned())
            .collect();
        let mut values = synthesize_guard_satisfying(
            self.pack,
            target.object_type,
            &target.lifecycle,
            &all_guards,
            &id,
            nonce,
        );
        values.insert(prop.to_owned(), JsonValue::String(init_state.clone()));

        let key = FactKey::new(ty_name, &id);
        let slice = load_slice(self.host, [key]).map_err(|e| format!("load_slice: {e}"))?;
        let output = DecisionOutput {
            facts: vec![FactChange::Create {
                object_type: target.object_type.name.clone(),
                key: id,
                values: values.clone(),
            }],
            commands: Vec::new(),
            timers: Vec::new(),
            links: Vec::new(),
        };
        let turn = DecisionTurn {
            id: format!("dst-create-{nonce}"),
            now: self.host.logical_now(),
            world: World::Main,
            consumes: None,
        };
        let before = self.host.ledger_len();
        let res = commit_decision(self.host, self.catalog, &slice, &turn, &output);
        let already_exists = self.model.contains_key(&(type_idx, slot));
        match (already_exists, res) {
            (false, Ok(_)) => {
                if self.host.ledger_len() != before.saturating_add(1) {
                    return Err("create did not append 1 ledger entry".to_owned());
                }
                self.model.insert(
                    (type_idx, slot),
                    RefEntity {
                        state: init_state.clone(),
                        values,
                    },
                );
                Ok(())
            }
            (true, Err(refusal)) if refusal.code() == "decision.object_exists" => {
                ensure_unchanged(self.host, before, "duplicate create refusal")
            }
            (exists, other) => Err(format!(
                "create mismatch for {ty_name} (exists={exists}): {other:?}"
            )),
        }
    }

    fn step_transition(
        &mut self,
        type_idx: usize,
        slot: u8,
        transition_idx: usize,
        nonce: u64,
    ) -> Result<(), String> {
        let Some(target) = self.targets.get(type_idx) else {
            return Ok(());
        };
        let len = target.lifecycle.transitions.len();
        let Some(transition) = target.lifecycle.transitions.get(transition_idx % len) else {
            return Ok(());
        };
        let ty_name = target.object_type.name.as_str();
        let prop = target.lifecycle.property.as_str();
        let id = entity_id(type_idx, slot);
        let key = FactKey::new(ty_name, &id);
        let slice = load_slice(self.host, [key]).map_err(|e| format!("load_slice: {e}"))?;
        let output = DecisionOutput {
            facts: vec![FactChange::Transition {
                object_type: ObjectTypeName(ty_name.to_owned()),
                key: id,
                lifecycle: PropertyId(prop.to_owned()),
                transition: TransitionId(transition.id.as_str().to_owned()),
            }],
            commands: Vec::new(),
            timers: Vec::new(),
            links: Vec::new(),
        };
        let turn = DecisionTurn {
            id: format!("dst-tr-{nonce}"),
            now: self.host.logical_now(),
            world: World::Main,
            consumes: None,
        };
        let before = self.host.ledger_len();
        let res = commit_decision(self.host, self.catalog, &slice, &turn, &output);
        let current = self.model.get(&(type_idx, slot)).map(|e| e.state.clone());

        match current {
            None => match res {
                Err(refusal) if refusal.code() == "decision.object_missing" => {
                    ensure_unchanged(self.host, before, "transition on missing entity")
                }
                other => Err(format!("expected object_missing on absent {ty_name}: {other:?}")),
            },
            Some(from_state) => {
                let allowed_from = transition.from.iter().any(|src| src == &from_state);
                match (allowed_from, res) {
                    (true, Ok(_)) => {
                        if self.host.ledger_len() != before.saturating_add(1) {
                            return Err("valid transition did not append 1 ledger entry".to_owned());
                        }
                        if let Some(entry) = self.model.get_mut(&(type_idx, slot)) {
                            entry.state = transition.to.clone();
                            entry
                                .values
                                .insert(prop.to_owned(), JsonValue::String(transition.to.clone()));
                        }
                        self.coverage.hit_transition(DomainTransition {
                            object_type: ty_name.to_owned(),
                            lifecycle: prop.to_owned(),
                            from: from_state,
                            to: transition.to.clone(),
                        });
                        Ok(())
                    }
                    (false, Err(refusal)) if refusal.code() == lifecycle_codes::UNDECLARED_WRITE => {
                        self.coverage.hit_disallowed_transition(DomainTransition {
                            object_type: ty_name.to_owned(),
                            lifecycle: prop.to_owned(),
                            from: from_state,
                            to: transition.to.clone(),
                        });
                        ensure_unchanged(self.host, before, "disallowed transition")
                    }
                    (true, Err(refusal)) if refusal.code().starts_with("decision.guard") => {
                        ensure_unchanged(self.host, before, "guard refusal")
                    }
                    (allowed, other) => Err(format!(
                        "transition mismatch for {ty_name}.{prop}.{} from '{from_state}' (allowed={allowed}): {other:?}",
                        transition.id.as_str()
                    )),
                }
            }
        }
    }

    fn step_mutate(
        &mut self,
        type_idx: usize,
        slot: u8,
        val_nonce: u64,
        nonce: u64,
    ) -> Result<(), String> {
        let Some(target) = self.targets.get(type_idx) else {
            return Ok(());
        };
        let Some(existing) = self.model.get(&(type_idx, slot)).cloned() else {
            return Ok(());
        };
        let Some((prop_name, new_val)) =
            pick_non_lifecycle_mutation(self.pack, target.object_type, &existing.values, val_nonce)
        else {
            return Ok(());
        };
        let ty_name = target.object_type.name.as_str();
        let id = entity_id(type_idx, slot);
        let key = FactKey::new(ty_name, &id);
        let slice = load_slice(self.host, [key]).map_err(|e| format!("load_slice: {e}"))?;
        let output = DecisionOutput {
            facts: vec![FactChange::Set {
                object_type: ObjectTypeName(ty_name.to_owned()),
                key: id,
                property: PropertyId(prop_name.clone()),
                value: new_val.clone(),
            }],
            commands: Vec::new(),
            timers: Vec::new(),
            links: Vec::new(),
        };
        let turn = DecisionTurn {
            id: format!("dst-set-{nonce}"),
            now: self.host.logical_now(),
            world: World::Main,
            consumes: None,
        };
        let before = self.host.ledger_len();
        let res = commit_decision(self.host, self.catalog, &slice, &turn, &output);
        let immutable_states: BTreeSet<&str> = target
            .lifecycle
            .immutable
            .iter()
            .map(String::as_str)
            .collect();
        let is_immutable =
            target.object_type.immutable || immutable_states.contains(existing.state.as_str());

        match (is_immutable, res) {
            (true, Err(refusal))
                if refusal.code() == lifecycle_codes::IMMUTABLE_STATE_WRITE
                    || refusal.code() == "commit.immutable_object" =>
            {
                self.coverage.hit_immutable_refusal(ty_name, existing.state);
                ensure_unchanged(self.host, before, "immutable state mutation")
            }
            (false, Ok(_)) => {
                if self.host.ledger_len() != before.saturating_add(1) {
                    return Err("valid mutation did not append 1 ledger entry".to_owned());
                }
                if let Some(entry) = self.model.get_mut(&(type_idx, slot)) {
                    entry.values.insert(prop_name, new_val);
                }
                Ok(())
            }
            (false, Err(refusal)) if refusal.code() == "core.laws.violated" => {
                ensure_unchanged(self.host, before, "law refusal on random mutation")
            }
            (imm, other) => Err(format!(
                "mutation mismatch for {ty_name} in state '{}' (immutable={imm}): {other:?}",
                existing.state
            )),
        }
    }

    fn verify_model_matches_host(&self) -> Result<(), String> {
        for (&(type_idx, slot), expected) in &self.model {
            let Some(target) = self.targets.get(type_idx) else {
                continue;
            };
            let ty_name = target.object_type.name.as_str();
            let prop = target.lifecycle.property.as_str();
            let id = entity_id(type_idx, slot);
            let loaded = self
                .host
                .load(&FactKey::new(ty_name, &id))
                .map_err(|e| format!("load {ty_name}/{id}: {e}"))?;
            let actual_state = loaded
                .values
                .as_ref()
                .and_then(|vals| vals.get(prop))
                .and_then(JsonValue::as_str);
            if actual_state != Some(expected.state.as_str()) {
                return Err(format!(
                    "refinement divergence on {ty_name}/{id}: model='{}', host={actual_state:?}",
                    expected.state
                ));
            }
        }
        Ok(())
    }
}

fn ensure_unchanged<H: PackHost>(host: &H, before: usize, what: &str) -> Result<(), String> {
    if host.ledger_len() != before {
        return Err(format!(
            "{what}: expected zero ledger writes, grew from {before} to {}",
            host.ledger_len()
        ));
    }
    Ok(())
}

fn entity_id(type_idx: usize, slot: u8) -> String {
    format!("dst-t{type_idx}-e{slot}")
}
