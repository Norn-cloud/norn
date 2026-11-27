//! Seeded multi-entity stateful DST over any admitted pack (#1656, #181, #176).
//!
//! Generates seeded, shrinkable operation sequences (`Create`, `Transition`,
//! `MutateNonLifecycle`, `AdvanceClock`, `CleanReopen`) via `proptest`, drives
//! a real [`PackHost`] (such as file-backed `ObservedSqliteCell`), independently
//! predicts guard, immutability, and governing-law outcomes, asserts complete
//! persisted facts, versions, links, and absences after **every** step, asserts
//! exact virtual-clock advancement, and runs `verify_ledger()` on clean reopen
//! and at run end.
//! Evidence level: [`EvidenceLevel::Sampled`].

use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};

use norn_kernel::cell_host::{
    FactKey, FactMutation, FactVersion, FactWrite, JsonObject, JsonValue, LinkKey,
};
use norn_kernel::pack_mbt_support::{
    DecisionOutput, FactChange, Lifecycle, ObjectTypeDef, ObjectTypeName, PropertyId, Transition,
    TransitionId,
};
use norn_kernel::{
    CellCommit, CommitError, DecisionRefusal, DecisionTurn, TypeCatalog, World, commit_decision,
    load_slice,
};
use proptest::prelude::*;
use proptest::test_runner::{Config, RngAlgorithm, TestError, TestRng, TestRunner};

use super::coverage::{DomainCoverage, DomainTransition};
use super::seed::{ensure_unchanged, entity_id, seed_stamped, stamp_link_targets};
use super::synth::{
    GuardEval, collect_required_link_graph, evaluate_guards, pick_non_lifecycle_mutation,
    synthesize_guard_satisfying,
};
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
            max_steps: 24,
            entities_per_type: 2,
        }
    }
}

/// One shrinkable operation step in a multi-entity DST sequence.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PackDstStep {
    /// Create entity `(type_idx, slot)` in its initial lifecycle state.
    Create {
        /// Index into the pack's lifecycle-bearing object types.
        type_idx: usize,
        /// Instance slot id.
        slot: u8,
        /// Deterministic synthesis nonce.
        nonce: u64,
    },
    /// Attempt to fire `transition_idx` on entity `(type_idx, slot)`.
    FireTransition {
        /// Index into the pack's lifecycle-bearing object types.
        type_idx: usize,
        /// Instance slot id.
        slot: u8,
        /// Index into the lifecycle's declared transitions.
        transition_idx: usize,
        /// Deterministic turn nonce.
        nonce: u64,
    },
    /// Attempt a non-lifecycle property mutation on `(type_idx, slot)`.
    MutateNonLifecycle {
        /// Index into the pack's lifecycle-bearing object types.
        type_idx: usize,
        /// Instance slot id.
        slot: u8,
        /// Deterministic mutation nonce.
        nonce: u64,
    },
    /// Advance the cell's virtual clock by `delta_millis` (`#176`).
    AdvanceClock {
        /// Milliseconds to advance.
        delta_millis: u64,
    },
    /// Perform a clean close and reopen of durable storage, verify the ledger,
    /// and re-check all persisted facts, versions, and links.
    CleanReopen,
}

/// Summary of a seeded multi-entity DST execution.
#[derive(Debug, Clone)]
pub struct PackDstReport {
    /// Honest evidence label ([`EvidenceLevel::Sampled`]).
    pub evidence: EvidenceLevel,
    /// Number of proptest cases completed.
    pub cases_run: u32,
    /// Total steps executed across all cases.
    pub steps_run: usize,
    /// Total clean-reopen + ledger-verification cycles executed.
    pub reopens_verified: usize,
    /// Object-level immutable refusals hit on transitions (including wrong-source / guarded).
    pub immutable_object_transition_refusals: usize,
    /// Object-level immutable refusals hit on property mutations.
    pub immutable_object_mutation_refusals: usize,
    /// Aggregate domain coverage hit during random walks.
    pub coverage: DomainCoverage,
}

#[derive(Clone)]
struct TargetSpec<'a> {
    object_type: &'a ObjectTypeDef,
    lifecycle: Lifecycle,
}

#[derive(Debug, Clone)]
struct RefEntity {
    state: String,
    values: JsonObject,
    version: FactVersion,
    required_links: Vec<LinkKey>,
}

struct DstCaseState<'a, H: PackHost> {
    pack: &'a AdmittedPack,
    catalog: &'a TypeCatalog<'a>,
    targets: &'a [TargetSpec<'a>],
    entities_per_type: u8,
    host: &'a mut H,
    model: BTreeMap<(usize, u8), RefEntity>,
    extra_facts: BTreeMap<FactKey, (JsonObject, FactVersion)>,
    extra_links: BTreeSet<LinkKey>,
    coverage: DomainCoverage,
    reopens: usize,
    imm_obj_tr_refusals: usize,
    imm_obj_mut_refusals: usize,
}

struct CreateStepOutcome<'a> {
    id_pair: (usize, u8),
    ty_name: &'a str,
    key: &'a FactKey,
    init_state: &'a str,
    values: JsonObject,
    required_links: Vec<LinkKey>,
    already_exists: bool,
    before: usize,
}

struct TransitionCheck<'a> {
    id_pair: (usize, u8),
    ty_name: &'a str,
    prop: &'a str,
    key: &'a FactKey,
    transition: &'a Transition,
    entry: RefEntity,
    before: usize,
}

#[derive(Default)]
struct DstRunAccum {
    cases_run: u32,
    steps_run: usize,
    reopens_verified: usize,
    imm_obj_tr_refusals: usize,
    imm_obj_mut_refusals: usize,
    coverage: DomainCoverage,
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
    make_host: F,
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
        .flat_map(|ty| {
            catalog
                .lifecycles(&ty.name)
                .into_values()
                .filter(|lc| !lc.initial.is_empty() && !lc.transitions.is_empty())
                .map(move |lifecycle| TargetSpec {
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
            reopens_verified: 0,
            immutable_object_transition_refusals: 0,
            immutable_object_mutation_refusals: 0,
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
    let factory = RefCell::new(make_host);
    let accum = RefCell::new(DstRunAccum::default());

    let run_result = runner.run(&strategy, |steps| {
        let case_id = {
            let mut acc = accum.borrow_mut();
            acc.cases_run = acc.cases_run.saturating_add(1);
            acc.cases_run
        };
        let mut host = (factory.borrow_mut())(case_id)
            .map_err(|err| TestCaseError::fail(format!("host factory: {err}")))?;
        let mut state = DstCaseState {
            pack,
            catalog: &catalog,
            targets: &targets,
            entities_per_type: config.entities_per_type,
            host: &mut host,
            model: BTreeMap::new(),
            extra_facts: BTreeMap::new(),
            extra_links: BTreeSet::new(),
            coverage: DomainCoverage::default(),
            reopens: 0,
            imm_obj_tr_refusals: 0,
            imm_obj_mut_refusals: 0,
        };
        state.execute_all(&steps).map_err(TestCaseError::fail)?;
        let mut acc = accum.borrow_mut();
        acc.steps_run = acc.steps_run.saturating_add(steps.len());
        acc.reopens_verified = acc.reopens_verified.saturating_add(state.reopens);
        acc.imm_obj_tr_refusals = acc
            .imm_obj_tr_refusals
            .saturating_add(state.imm_obj_tr_refusals);
        acc.imm_obj_mut_refusals = acc
            .imm_obj_mut_refusals
            .saturating_add(state.imm_obj_mut_refusals);
        acc.coverage.merge(&state.coverage);
        Ok(())
    });

    let acc = accum.into_inner();
    match run_result {
        Ok(()) => Ok(PackDstReport {
            evidence: EvidenceLevel::Sampled,
            cases_run: acc.cases_run,
            steps_run: acc.steps_run,
            reopens_verified: acc.reopens_verified,
            immutable_object_transition_refusals: acc.imm_obj_tr_refusals,
            immutable_object_mutation_refusals: acc.imm_obj_mut_refusals,
            coverage: acc.coverage,
        }),
        Err(TestError::Fail(reason, minimal_steps)) => Err(format!(
            "pack DST failed on pack '{}': {reason}; minimal sequence: {minimal_steps:?}",
            pack.name
        )),
        Err(TestError::Abort(reason)) => Err(format!(
            "pack DST aborted on pack '{}': {reason}",
            pack.name
        )),
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
        .map(|spec| spec.lifecycle.transitions.len())
        .max()
        .unwrap_or(1)
        .max(1);
    let slots = entities_per_type.max(1);

    let step_strat = prop_oneof![
        3 => (0..type_count, 0..slots, 1u64..10_000u64).prop_map(|(type_idx, slot, nonce)| {
            PackDstStep::Create { type_idx, slot, nonce }
        }),
        5 => (0..type_count, 0..slots, 0..max_transitions, 1u64..10_000u64).prop_map(
            |(type_idx, slot, transition_idx, nonce)| PackDstStep::FireTransition {
                type_idx,
                slot,
                transition_idx,
                nonce,
            },
        ),
        3 => (0..type_count, 0..slots, 1u64..10_000u64).prop_map(|(type_idx, slot, nonce)| {
            PackDstStep::MutateNonLifecycle { type_idx, slot, nonce }
        }),
        1 => (1_000u64..86_400_000u64).prop_map(|delta_millis| PackDstStep::AdvanceClock {
            delta_millis,
        }),
        1 => Just(PackDstStep::CleanReopen),
    ];

    prop::collection::vec(step_strat, 1..=max_steps.max(1)).boxed()
}

impl<H: PackHost> DstCaseState<'_, H> {
    fn verify_clock_advance(&mut self, delta_millis: u64) -> Result<(), String> {
        let before = self.host.logical_now().as_millis();
        self.host.advance_clock(delta_millis);
        let after = self.host.logical_now().as_millis();
        let expected = before.saturating_add(delta_millis);
        if after != expected {
            return Err(format!(
                "virtual clock did not advance by exact delta {delta_millis}: before={before}, after={after}, expected={expected}"
            ));
        }
        Ok(())
    }

    fn execute_all(&mut self, steps: &[PackDstStep]) -> Result<(), String> {
        for (idx, step) in steps.iter().enumerate() {
            let n = u64::try_from(idx).unwrap_or(0).saturating_add(1);
            match *step {
                PackDstStep::Create {
                    type_idx,
                    slot,
                    nonce,
                } => {
                    self.step_create(type_idx, slot, nonce.wrapping_add(n))?;
                }
                PackDstStep::FireTransition {
                    type_idx,
                    slot,
                    transition_idx,
                    nonce,
                } => {
                    self.step_transition(type_idx, slot, transition_idx, nonce.wrapping_add(n))?;
                }
                PackDstStep::MutateNonLifecycle {
                    type_idx,
                    slot,
                    nonce,
                } => {
                    self.step_mutate(type_idx, slot, nonce.wrapping_add(n))?;
                }
                PackDstStep::AdvanceClock { delta_millis } => {
                    self.verify_clock_advance(delta_millis)?;
                }
                PackDstStep::CleanReopen => {
                    self.host.reopen()?;
                    self.reopens = self.reopens.saturating_add(1);
                }
            }
            self.assert_complete_model_matches()?;
        }
        self.host.verify_ledger()
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
        let (pack, catalog, ty) = (self.pack, self.catalog, target.object_type);
        let already_exists = self.model.contains_key(&(type_idx, slot));
        let mut graph = collect_required_link_graph(pack, ty, &key, nonce)?;
        if !already_exists {
            stamp_link_targets(self.host, pack, catalog, &mut graph.setup_writes, nonce)?;
        }
        let before = self.host.ledger_len();
        let meta = CreateStepOutcome {
            id_pair: (type_idx, slot),
            ty_name,
            key: &key,
            init_state,
            values: values.clone(),
            required_links: Vec::new(),
            already_exists,
            before,
        };
        if let Some(outcome) = seed_stamped(self.host, pack, catalog, ty, &id, &values, nonce)? {
            return self.finish_create_step(
                CreateStepOutcome {
                    before: outcome.before_commit,
                    ..meta
                },
                outcome.result,
            );
        } else if graph.direct_links.is_empty() {
            let res = self.commit_single_fact(
                &key,
                format!("dst-create-{nonce}"),
                FactChange::Create {
                    object_type: target.object_type.name.clone(),
                    key: id,
                    values,
                },
            )?;
            return self.finish_create_step(meta, res);
        }

        self.step_create_linked(meta, graph)
    }

    fn commit_single_fact(
        &mut self,
        key: &FactKey,
        turn_id: String,
        change: FactChange,
    ) -> Result<Result<(), DecisionRefusal<H::Error>>, String> {
        let slice = load_slice(self.host, [key.clone()]).map_err(|e| format!("load_slice: {e}"))?;
        let output = DecisionOutput {
            facts: vec![change],
            links: Vec::new(),
            commands: Vec::new(),
            timers: Vec::new(),
        };
        let turn = DecisionTurn {
            id: turn_id,
            now: self.host.logical_now(),
            world: World::Main,
            consumes: None,
        };
        Ok(commit_decision(self.host, self.catalog, &slice, &turn, &output).map(|_| ()))
    }

    fn refuse_duplicate_linked_create(
        &mut self,
        ty_name: &str,
        key: &FactKey,
        values: JsonObject,
        before: usize,
    ) -> Result<(), String> {
        let loaded = self.host.load(key).map_err(|e| format!("load: {e}"))?;
        let res = self.host.commit(CellCommit {
            writes: vec![FactWrite {
                key: key.clone(),
                expected: FactVersion::ABSENT,
                mutation: FactMutation::Put { values },
            }],
            ..CellCommit::empty()
        });
        match res {
            Err(CommitError::Conflict { .. }) => {
                if !loaded.exists() {
                    return Err(format!("expected {key} to exist on duplicate create"));
                }
                ensure_unchanged(self.host, before, "duplicate linked create refusal")
            }
            other => Err(format!(
                "duplicate linked create for {ty_name} returned {other:?}"
            )),
        }
    }

    fn step_create_linked(
        &mut self,
        meta: CreateStepOutcome<'_>,
        mut graph: super::synth::RequiredLinkGraph,
    ) -> Result<(), String> {
        let CreateStepOutcome {
            id_pair,
            ty_name,
            key,
            init_state,
            values,
            already_exists,
            before,
            ..
        } = meta;
        if already_exists {
            return self.refuse_duplicate_linked_create(ty_name, key, values, before);
        }

        let predicted_subject = self
            .catalog
            .effective_object_values(&key.object_type, &values)
            .map_err(|err| format!("predict effective values for {key}: {err}"))?;
        let predicted_setup =
            super::law_parity::predict_setup_writes(self.catalog, &graph.setup_writes)?;
        let direct_link_keys: Vec<LinkKey> =
            graph.direct_links.iter().map(|lw| lw.key.clone()).collect();
        let all_link_keys: Vec<LinkKey> = graph
            .setup_links
            .iter()
            .chain(graph.direct_links.iter())
            .map(|lw| lw.key.clone())
            .collect();
        let expected_law_violation = super::law_parity::predict_linked_create_law_violation(
            self.pack,
            self.catalog,
            &self.snapshot_facts(),
            &self.extra_links,
            &predicted_setup,
            (key, &predicted_subject),
            &all_link_keys,
        );
        graph.setup_writes.push(FactWrite {
            key: key.clone(),
            expected: FactVersion::ABSENT,
            mutation: FactMutation::Put { values },
        });
        graph.setup_links.extend(graph.direct_links);
        let res = self.host.commit(CellCommit {
            writes: graph.setup_writes,
            links: graph.setup_links,
            ..CellCommit::empty()
        });
        match (expected_law_violation, res) {
            (false, Ok(_)) => {
                if self.host.ledger_len() != before.saturating_add(1) {
                    return Err("linked create did not append 1 ledger entry".to_owned());
                }
                for (setup_key, effective) in predicted_setup {
                    self.extra_facts
                        .insert(setup_key, (effective, FactVersion(1)));
                }
                for link_key in all_link_keys {
                    self.extra_links.insert(link_key);
                }
                self.model.insert(
                    id_pair,
                    RefEntity {
                        state: init_state.to_owned(),
                        values: predicted_subject,
                        version: FactVersion(1),
                        required_links: direct_link_keys,
                    },
                );
                Ok(())
            }
            (true, Err(CommitError::Law { .. } | CommitError::InvariantViolation { .. })) => {
                ensure_unchanged(self.host, before, "law/invariant refusal on linked create")
            }
            (law_viol, other) => Err(format!(
                "linked create mismatch for {ty_name} (law_violated={law_viol}): {other:?}"
            )),
        }
    }

    fn finish_create_step(
        &mut self,
        meta: CreateStepOutcome<'_>,
        res: Result<(), DecisionRefusal<H::Error>>,
    ) -> Result<(), String> {
        let CreateStepOutcome {
            id_pair,
            ty_name,
            key,
            init_state,
            values,
            required_links,
            already_exists,
            before,
        } = meta;
        match (already_exists, res) {
            (false, Ok(())) => {
                if self.host.ledger_len() != before.saturating_add(1) {
                    return Err("create did not append 1 ledger entry".to_owned());
                }
                let expected_values = self
                    .catalog
                    .effective_object_values(&key.object_type, &values)
                    .map_err(|err| format!("predict effective values for {key}: {err}"))?;
                self.model.insert(
                    id_pair,
                    RefEntity {
                        state: init_state.to_owned(),
                        values: expected_values,
                        version: FactVersion(1),
                        required_links,
                    },
                );
                Ok(())
            }
            (true, Err(DecisionRefusal::ObjectExists { .. })) => {
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
        let Some(transition) = transition_idx
            .checked_rem(len)
            .and_then(|idx| target.lifecycle.transitions.get(idx))
        else {
            return Ok(());
        };
        let ty_name = target.object_type.name.as_str();
        let prop = target.lifecycle.property.as_str();
        let id = entity_id(type_idx, slot);
        let key = FactKey::new(ty_name, &id);
        let before = self.host.ledger_len();
        let res = self.commit_single_fact(
            &key,
            format!("dst-tr-{nonce}"),
            FactChange::Transition {
                object_type: ObjectTypeName(ty_name.to_owned()),
                key: id,
                lifecycle: PropertyId(prop.to_owned()),
                transition: TransitionId(transition.id.as_str().to_owned()),
            },
        )?;
        let current = self.model.get(&(type_idx, slot)).cloned();

        let Some(entry) = current else {
            return match res {
                Err(DecisionRefusal::ObjectMissing { .. }) => {
                    ensure_unchanged(self.host, before, "transition on missing entity")
                }
                other => Err(format!(
                    "expected ObjectMissing on absent {ty_name}: {other:?}"
                )),
            };
        };

        // Kernel enforces object-level immutability before transition source or guards.
        if super::coverage::is_effectively_immutable(self.pack, target.object_type) {
            return match res {
                Err(DecisionRefusal::InvalidOutput { reason })
                    if reason.ends_with(" is immutable") =>
                {
                    ensure_unchanged(self.host, before, "immutable object transition refusal")?;
                    self.imm_obj_tr_refusals = self.imm_obj_tr_refusals.saturating_add(1);
                    self.coverage
                        .hit_immutable_refusal(ty_name, "<object>", "<immutable_object>");
                    Ok(())
                }
                other => Err(format!(
                    "expected immutable object refusal on transition for {ty_name}: {other:?}"
                )),
            };
        }

        let allowed_from = transition.from.iter().any(|src| src == &entry.state);
        if !allowed_from {
            return match res {
                Err(refusal @ DecisionRefusal::NotFromState { .. }) => {
                    self.host.record_decision_refusal(&refusal);
                    ensure_unchanged(self.host, before, "disallowed transition")?;
                    self.coverage.hit_disallowed_transition(DomainTransition {
                        object_type: ty_name.to_owned(),
                        lifecycle: prop.to_owned(),
                        transition: transition.id.as_str().to_owned(),
                        from: entry.state,
                        to: transition.to.clone(),
                    });
                    Ok(())
                }
                other => Err(format!(
                    "expected NotFromState for {ty_name} '{}' from '{}': {other:?}",
                    transition.id.as_str(),
                    entry.state
                )),
            };
        }

        self.verify_transition_outcome(
            TransitionCheck {
                id_pair: (type_idx, slot),
                ty_name,
                prop,
                key: &key,
                transition,
                entry,
                before,
            },
            res,
        )
    }

    fn verify_transition_outcome(
        &mut self,
        check: TransitionCheck<'_>,
        res: Result<(), DecisionRefusal<H::Error>>,
    ) -> Result<(), String> {
        let TransitionCheck {
            id_pair,
            ty_name,
            prop,
            key,
            transition,
            entry,
            before,
        } = check;
        let guard_eval = evaluate_guards(&transition.guards, &entry.values);
        let mut proposed_values = entry.values.clone();
        proposed_values.insert(prop.to_owned(), JsonValue::String(transition.to.clone()));
        let expected_law_violation = self.predict_law_violation(key, &proposed_values);

        match (guard_eval, expected_law_violation, res) {
            (GuardEval::False, _, Err(DecisionRefusal::GuardFalse { .. })) => {
                ensure_unchanged(self.host, before, "guard false refusal")
            }
            (GuardEval::Error(_), _, Err(DecisionRefusal::GuardError { .. })) => {
                ensure_unchanged(self.host, before, "guard error refusal")
            }
            (
                GuardEval::True,
                true,
                Err(DecisionRefusal::Law { .. } | DecisionRefusal::InvariantViolation { .. }),
            ) => ensure_unchanged(self.host, before, "post-transition law refusal"),
            (GuardEval::True, false, Ok(())) => {
                if self.host.ledger_len() != before.saturating_add(1) {
                    return Err("valid transition did not append 1 ledger entry".to_owned());
                }
                let persisted = self.host.load(key).map_err(|e| format!("load: {e}"))?;
                if persisted.version.0 != entry.version.0.saturating_add(1) {
                    return Err(format!(
                        "transition did not bump version of {key}: before={:?}, after={:?}",
                        entry.version, persisted.version
                    ));
                }
                if let Some(slot_entry) = self.model.get_mut(&id_pair) {
                    slot_entry.state.clone_from(&transition.to);
                    slot_entry.values = proposed_values;
                    slot_entry.version = persisted.version;
                }
                self.coverage.hit_transition(DomainTransition {
                    object_type: ty_name.to_owned(),
                    lifecycle: prop.to_owned(),
                    transition: transition.id.as_str().to_owned(),
                    from: entry.state,
                    to: transition.to.clone(),
                });
                Ok(())
            }
            (guard, law_viol, other) => Err(format!(
                "transition oracle mismatch for {ty_name} '{}' from '{}' (guard={guard:?}, law_violated={law_viol}): {other:?}",
                transition.id.as_str(),
                entry.state
            )),
        }
    }

    fn step_mutate(&mut self, type_idx: usize, slot: u8, nonce: u64) -> Result<(), String> {
        let Some(target) = self.targets.get(type_idx) else {
            return Ok(());
        };
        let Some(existing) = self.model.get(&(type_idx, slot)).cloned() else {
            return Ok(());
        };
        let Some((mut_prop, mut_val)) =
            pick_non_lifecycle_mutation(self.pack, target.object_type, &existing.values, nonce)
        else {
            return Ok(());
        };
        let ty_name = target.object_type.name.as_str();
        let id = entity_id(type_idx, slot);
        let key = FactKey::new(ty_name, &id);
        let before = self.host.ledger_len();
        let res = self.commit_single_fact(
            &key,
            format!("dst-mut-{nonce}"),
            FactChange::Set {
                object_type: ObjectTypeName(ty_name.to_owned()),
                key: id,
                property: PropertyId(mut_prop.clone()),
                value: mut_val.clone(),
            },
        )?;
        let imm_lc = super::law_parity::immutable_lifecycle_state(
            self.catalog,
            &target.object_type.name,
            &existing.values,
        );
        let obj_immutable =
            super::coverage::is_effectively_immutable(self.pack, target.object_type);
        let mut proposed_values = existing.values.clone();
        proposed_values.insert(mut_prop.clone(), mut_val.clone());
        let expected_law_violation = self.predict_law_violation(&key, &proposed_values);

        // Kernel enforces object-level immutability before lifecycle-state immutability or laws.
        match (obj_immutable, imm_lc, expected_law_violation, res) {
            (true, _, _, Err(DecisionRefusal::InvalidOutput { reason }))
                if reason.ends_with(" is immutable") =>
            {
                ensure_unchanged(self.host, before, "immutable object mutation refusal")?;
                self.imm_obj_mut_refusals = self.imm_obj_mut_refusals.saturating_add(1);
                self.coverage
                    .hit_immutable_refusal(ty_name, "<object>", "<immutable_object>");
                Ok(())
            }
            (
                false,
                Some((imm_prop, imm_state)),
                _,
                Err(refusal @ DecisionRefusal::ImmutableStateWrite { .. }),
            ) => {
                self.host.record_decision_refusal(&refusal);
                ensure_unchanged(self.host, before, "immutable state mutation refusal")?;
                self.coverage
                    .hit_immutable_refusal(ty_name, imm_prop, imm_state);
                Ok(())
            }
            (
                false,
                None,
                true,
                Err(DecisionRefusal::Law { .. } | DecisionRefusal::InvariantViolation { .. }),
            ) => ensure_unchanged(self.host, before, "law/invariant violation on mutate"),
            (false, None, false, Ok(())) => {
                if self.host.ledger_len() != before.saturating_add(1) {
                    return Err("valid mutation did not append 1 ledger entry".to_owned());
                }
                let persisted = self.host.load(&key).map_err(|e| format!("load: {e}"))?;
                if persisted.version.0 != existing.version.0.saturating_add(1) {
                    return Err(format!(
                        "mutation did not bump version of {key}: before={:?}, after={:?}",
                        existing.version, persisted.version
                    ));
                }
                let actual_val = persisted
                    .values
                    .as_ref()
                    .and_then(|vals| vals.get(&mut_prop));
                if actual_val != Some(&mut_val) {
                    return Err(format!(
                        "mutation of {key}.{mut_prop} did not persist: expected {mut_val:?}, got {actual_val:?}"
                    ));
                }
                if let Some(entry) = self.model.get_mut(&(type_idx, slot)) {
                    entry.values = proposed_values;
                    entry.version = persisted.version;
                }
                Ok(())
            }
            (imm_obj, imm_state, law_viol, other) => Err(format!(
                "mutation oracle mismatch for {ty_name}.{mut_prop} (obj_immutable={imm_obj}, immutable_state={imm_state:?}, law_violated={law_viol}): {other:?}"
            )),
        }
    }

    fn snapshot_facts(&self) -> BTreeMap<FactKey, (JsonObject, FactVersion)> {
        let mut facts = self.extra_facts.clone();
        for ((type_idx, slot), entity) in &self.model {
            if let Some(target) = self.targets.get(*type_idx) {
                let key = FactKey::new(
                    target.object_type.name.as_str(),
                    entity_id(*type_idx, *slot),
                );
                facts.insert(key, (entity.values.clone(), entity.version));
            }
        }
        facts
    }

    fn predict_law_violation(&self, key: &FactKey, proposed: &JsonObject) -> bool {
        let mut facts = self.snapshot_facts();
        let next_ver = facts.get(key).map_or(FactVersion(1), |(_, ver)| {
            FactVersion(ver.0.saturating_add(1))
        });
        facts.insert(key.clone(), (proposed.clone(), next_ver));
        super::law_parity::predict_oracle_law_violation(
            &super::law_parity::OracleGraph {
                pack: self.pack,
                catalog: self.catalog,
                facts: &facts,
                links: &self.extra_links,
            },
            std::slice::from_ref(key),
        )
    }

    fn assert_complete_model_matches(&self) -> Result<(), String> {
        let slots = self.entities_per_type.max(1);
        for (type_idx, target) in self.targets.iter().enumerate() {
            let ty_name = target.object_type.name.as_str();
            for slot in 0..slots {
                let key = FactKey::new(ty_name, entity_id(type_idx, slot));
                match self.model.get(&(type_idx, slot)) {
                    None => {
                        let loaded = self
                            .host
                            .load(&key)
                            .map_err(|e| format!("load {key}: {e}"))?;
                        if loaded.exists() {
                            return Err(format!(
                                "oracle divergence: {key} unexpectedly exists in host (version={:?})",
                                loaded.version
                            ));
                        }
                    }
                    Some(expected) => {
                        self.assert_fact_matches(&key, &expected.values, expected.version)?;
                        for link_key in &expected.required_links {
                            self.assert_link_exists(link_key)?;
                        }
                    }
                }
            }
        }
        for (key, (expected_values, expected_version)) in &self.extra_facts {
            self.assert_fact_matches(key, expected_values, *expected_version)?;
        }
        for link_key in &self.extra_links {
            self.assert_link_exists(link_key)?;
        }
        Ok(())
    }

    fn assert_fact_matches(
        &self,
        key: &FactKey,
        expected_values: &JsonObject,
        expected_version: FactVersion,
    ) -> Result<(), String> {
        let loaded = self
            .host
            .load(key)
            .map_err(|e| format!("load {key}: {e}"))?;
        if !loaded.exists()
            || loaded.version != expected_version
            || loaded.values.as_ref() != Some(expected_values)
        {
            return Err(format!(
                "oracle divergence on {key}: expected (ver={expected_version:?}, values={expected_values:?}), got (exists={}, ver={:?}, values={:?})",
                loaded.exists(),
                loaded.version,
                loaded.values
            ));
        }
        Ok(())
    }

    fn assert_link_exists(&self, link_key: &LinkKey) -> Result<(), String> {
        let state = self
            .host
            .load_link(link_key)
            .map_err(|e| format!("load_link {link_key}: {e}"))?;
        if !state.exists() {
            return Err(format!(
                "oracle divergence: link {link_key} missing from host"
            ));
        }
        Ok(())
    }
}
