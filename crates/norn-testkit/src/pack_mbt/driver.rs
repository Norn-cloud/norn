//! Generic IR-driven Model-Based Testing (MBT) driver (#1656).
//!
//! Given *any* [`AdmittedPack`] and a [`PackHost`] factory (for example, a real
//! `ObservedSqliteCell` on disk), executes:
//! 1. **All-states & all-transitions lifecycle walk** (`exhaustive-finite` when
//!    every transition and guard witness is covered, else `bounded`): reaches
//!    every reachable state, executes every valid transition, and verifies
//!    false guards refuse strictly with `DecisionRefusal::GuardFalse` + zero
//!    ledger writes.
//! 2. **Disallowed transitions & immutable state/entity refusals**
//!    (`exhaustive-finite` when every immutable target is covered): asserts
//!    `NotFromState`, `LifecyclePropertySet`, `ImmutableStateWrite`, and
//!    `CommitError::ImmutableObject` with zero ledger writes.
//! 3. **Full Cedar authorization & `SoD` matrix** (`exhaustive-finite` when
//!    every rule is covered): runs authorized controls plus unauthorized-role,
//!    foreign-scope, and self-approver denials through real `commit_action`.
//! 4. **`LAW-EXAMPLE-1` → real cell runtime parity** (`runtime-enforced`):
//!    replays every `LawExampleCase` through both `norn_checker::law_examples`
//!    and real `PackHost::commit`, matching exact law pin, subject, and
//!    `DataQualityOutcome::NotSatisfied`.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::sync::{Arc, Mutex};

use norn_kernel::cell_host::{
    FactKey, FactMutation, FactVersion, FactWrite, JsonObject, JsonValue,
};
use norn_kernel::pack_mbt_support::{
    DecisionOutput, FactChange, Lifecycle, ObjectTypeDef, ObjectTypeName, PropertyId, Transition,
    TransitionId, ZenExpression,
};
use norn_kernel::{
    CellCommit, CommitError, DecisionRefusal, DecisionTurn, TypeCatalog, World, commit_decision,
    load_slice,
};

use super::cedar::run_cedar_and_sod_phase;
use super::coverage::{DomainCoverage, DomainCoveragePlan, DomainCoverageReport, DomainTransition};
use super::law_parity::run_law_example_parity_phase;
use super::synth::{
    collect_required_link_graph, pick_non_lifecycle_mutation, synthesize_guard_satisfying,
    synthesize_guard_violating_at_state, synthesize_object,
};
use super::{AdmittedPack, EvidenceLevel, PackHost, action_call_object_type};

/// Summary of one full IR-driven MBT execution against a [`PackHost`].
#[derive(Debug, Clone)]
pub struct PackMbtReport {
    /// Pack identifier.
    pub pack_name: String,
    /// Honest evidence levels achieved across executed phases.
    pub evidence: Vec<(&'static str, EvidenceLevel)>,
    /// Number of valid lifecycle transitions executed.
    pub valid_transitions_run: usize,
    /// Number of disallowed transitions and guard refusals verified with zero ledger writes.
    pub disallowed_transitions_checked: usize,
    /// Number of immutable-state and immutable-entity refusals verified.
    pub immutable_refusals_checked: usize,
    /// Number of Cedar allow/deny/`SoD` matrix checks executed through `commit_action`.
    pub cedar_checks_run: usize,
    /// Number of `LAW-EXAMPLE-1` examples replayed for checker-vs-runtime parity.
    pub law_examples_checked: usize,
    /// Guarded transitions for which no valid false-guard witness at the source
    /// state could be constructed (withholds `exhaustive-finite` when non-empty).
    pub uncovered_guard_refusals: Vec<String>,
    /// Unified domain-model coverage report.
    pub coverage: DomainCoverageReport,
}

#[derive(Default)]
struct LifecycleStats {
    valid: usize,
    disallowed: usize,
    immutable: usize,
    uncovered_guard_refusals: Vec<String>,
}

struct LifecycleCtx<'a> {
    pack: &'a AdmittedPack,
    catalog: &'a TypeCatalog<'a>,
    object_type: &'a ObjectTypeDef,
    lifecycle: &'a Lifecycle,
    coverage: &'a Arc<Mutex<DomainCoverage>>,
}

struct GuardCheckSpec<'a> {
    transition: &'a Transition,
    from_state: &'a str,
    init_state: &'a str,
    prefix: &'a [String],
    id: &'a str,
    seq: u64,
}

/// Runs the complete generic IR-driven MBT suite on `pack` using `make_host` to
/// open fresh isolated cells per verification phase.
///
/// # Errors
///
/// Returns a detailed error if any lifecycle walk, guard refusal, immutability
/// invariant, Cedar/`SoD` action check, ledger-zero-write check, `verify_ledger()`,
/// or `LAW-EXAMPLE-1` parity assertion fails.
pub fn run_pack_mbt<H, F>(pack: &AdmittedPack, mut make_host: F) -> Result<PackMbtReport, String>
where
    H: PackHost,
    F: FnMut(&str) -> Result<H, String>,
{
    let plan = DomainCoveragePlan::from_pack(pack);
    let driver_cov = Arc::new(Mutex::new(DomainCoverage::default()));
    let extra = [action_call_object_type()];
    let catalog = pack.type_catalog(&extra);

    let lc = run_lifecycle_and_immutability_phase(pack, &catalog, &mut make_host, &driver_cov)?;
    let cedar_checks_run = run_cedar_and_sod_phase(pack, &catalog, &mut make_host, &driver_cov)?;
    let law_examples_checked =
        run_law_example_parity_phase(pack, &catalog, &mut make_host, &driver_cov)?;

    // Prefer the host's unified cell-level observer (ObservedSqliteCell +
    // record_decision_refusal) when attached, combining with Cedar PDP hits.
    let probe_host = make_host("coverage-probe")?;
    let unified = build_unified_coverage(&driver_cov, probe_host.read_observed_coverage());
    let report = unified.report(pack.name.clone(), &plan);

    let lc_evidence =
        if report.uncovered_transitions.is_empty() && lc.uncovered_guard_refusals.is_empty() {
            EvidenceLevel::ExhaustiveFinite
        } else {
            EvidenceLevel::Bounded
        };
    let imm_evidence = if report.uncovered_immutable_refusals.is_empty() {
        EvidenceLevel::ExhaustiveFinite
    } else {
        EvidenceLevel::Bounded
    };
    let cedar_evidence =
        if report.uncovered_cedar_allows.is_empty() && report.uncovered_cedar_denies.is_empty() {
            EvidenceLevel::ExhaustiveFinite
        } else {
            EvidenceLevel::Bounded
        };

    Ok(PackMbtReport {
        pack_name: pack.name.clone(),
        evidence: vec![
            ("lifecycle_walks_and_refusals", lc_evidence),
            ("immutable_state_refusals", imm_evidence),
            ("cedar_and_sod_matrix", cedar_evidence),
            ("law_example_runtime_parity", EvidenceLevel::RuntimeEnforced),
        ],
        valid_transitions_run: lc.valid,
        disallowed_transitions_checked: lc.disallowed,
        immutable_refusals_checked: lc.immutable,
        cedar_checks_run,
        law_examples_checked,
        uncovered_guard_refusals: lc.uncovered_guard_refusals,
        coverage: report,
    })
}

fn build_unified_coverage(
    driver_cov: &Arc<Mutex<DomainCoverage>>,
    host_observed: Option<DomainCoverage>,
) -> DomainCoverage {
    let driver_snap = match driver_cov.lock() {
        Ok(guard) => guard.clone(),
        Err(poisoned) => poisoned.into_inner().clone(),
    };
    let Some(mut observed) = host_observed else {
        return driver_snap;
    };
    // Cedar allow/deny decisions are recorded at the ActionAuthorizer boundary
    // (ObservedPdpAuthorizer); merge them into the single host observer report.
    for (rule, count) in driver_snap.cedar_allows {
        let entry = observed.cedar_allows.entry(rule).or_insert(0);
        *entry = entry.saturating_add(count);
    }
    for (rule, count) in driver_snap.cedar_denies {
        let entry = observed.cedar_denies.entry(rule).or_insert(0);
        *entry = entry.saturating_add(count);
    }
    observed
}

fn run_lifecycle_and_immutability_phase<H, F>(
    pack: &AdmittedPack,
    catalog: &TypeCatalog<'_>,
    make_host: &mut F,
    coverage: &Arc<Mutex<DomainCoverage>>,
) -> Result<LifecycleStats, String>
where
    H: PackHost,
    F: FnMut(&str) -> Result<H, String>,
{
    let mut totals = LifecycleStats::default();
    let mut seq = 0u64;
    for object_type in &pack.object_types {
        if super::coverage::is_effectively_immutable(pack, object_type) {
            seq = seq.saturating_add(1);
            verify_immutable_entity(pack, object_type, make_host, coverage, seq)?;
            totals.immutable = totals.immutable.saturating_add(1);
            continue;
        }
        for lifecycle in catalog.lifecycles(&object_type.name).into_values() {
            let paths = bfs_paths_to_states(&lifecycle);
            let ctx = LifecycleCtx {
                pack,
                catalog,
                object_type,
                lifecycle: &lifecycle,
                coverage,
            };
            exercise_valid_transitions(&ctx, &paths, make_host, &mut seq, &mut totals)?;
            exercise_negative_and_immutable(&ctx, &paths, make_host, &mut seq, &mut totals)?;
        }
    }
    Ok(totals)
}

fn verify_immutable_entity<H, F>(
    pack: &AdmittedPack,
    object_type: &ObjectTypeDef,
    make_host: &mut F,
    coverage: &Arc<Mutex<DomainCoverage>>,
    seq: u64,
) -> Result<(), String>
where
    H: PackHost,
    F: FnMut(&str) -> Result<H, String>,
{
    let ty_name = object_type.name.as_str();
    let mut host = make_host(&format!("immutable-entity-{ty_name}-{seq}"))?;
    let id = format!("imm-entity-{seq}");
    let values = synthesize_object(pack, object_type, &id, seq);
    let key = FactKey::new(ty_name, &id);
    let mut graph = collect_required_link_graph(pack, object_type, &key, seq)?;
    graph.setup_writes.push(FactWrite {
        key: key.clone(),
        expected: FactVersion::ABSENT,
        mutation: FactMutation::Put {
            values: values.clone(),
        },
    });
    graph.setup_links.extend(graph.direct_links);
    host.commit(CellCommit {
        writes: graph.setup_writes,
        links: graph.setup_links,
        ..CellCommit::empty()
    })
    .map_err(|err| format!("immutable entity {ty_name}: seed commit failed: {err}"))?;

    let mut mutated = values;
    if let Some((prop_id, alt)) = pick_non_lifecycle_mutation(pack, object_type, &mutated, seq) {
        mutated.insert(prop_id, alt);
    }
    let loaded = host
        .load(&key)
        .map_err(|err| format!("load immutable entity {ty_name}: {err}"))?;
    let ledger_before = host.ledger_len();
    let update_commit = CellCommit {
        writes: vec![FactWrite {
            key,
            expected: loaded.version,
            mutation: FactMutation::Put { values: mutated },
        }],
        ..CellCommit::empty()
    };
    match host.commit(update_commit) {
        Err(CommitError::ImmutableObject { .. }) => {
            assert_zero_ledger_writes(&host, ledger_before, ty_name, "immutable entity write")?;
            with_cov(coverage, |cov| {
                cov.hit_immutable_refusal(ty_name, "<object>", "<immutable_object>");
            });
            host.verify_ledger()
        }
        other => Err(format!(
            "immutable entity {ty_name} did not refuse mutation with ImmutableObject: {other:?}"
        )),
    }
}

pub(crate) fn bfs_paths_to_states(
    lifecycle: &Lifecycle,
) -> BTreeMap<String, (String, Vec<String>)> {
    let mut paths: BTreeMap<String, (String, Vec<String>)> = BTreeMap::new();
    let mut queue = VecDeque::new();
    for init in &lifecycle.initial {
        if !paths.contains_key(init) {
            paths.insert(init.clone(), (init.clone(), Vec::new()));
            queue.push_back(init.clone());
        }
    }
    while let Some(state) = queue.pop_front() {
        let Some((init, prefix)) = paths.get(&state).cloned() else {
            continue;
        };
        for transition in &lifecycle.transitions {
            if transition.from.iter().any(|src| src == &state)
                && !paths.contains_key(&transition.to)
            {
                let mut next_path = prefix.clone();
                next_path.push(transition.id.as_str().to_owned());
                paths.insert(transition.to.clone(), (init.clone(), next_path));
                queue.push_back(transition.to.clone());
            }
        }
    }
    paths
}

pub(crate) fn collect_prefix_guards(
    lifecycle: &Lifecycle,
    prefix: &[String],
) -> Vec<ZenExpression> {
    let mut guards = Vec::new();
    for tr_id in prefix {
        if let Some(tr) = lifecycle
            .transitions
            .iter()
            .find(|t| t.id.as_str() == tr_id)
        {
            guards.extend(tr.guards.iter().cloned());
        }
    }
    guards
}

fn exercise_valid_transitions<H, F>(
    ctx: &LifecycleCtx<'_>,
    paths: &BTreeMap<String, (String, Vec<String>)>,
    make_host: &mut F,
    seq: &mut u64,
    totals: &mut LifecycleStats,
) -> Result<(), String>
where
    H: PackHost,
    F: FnMut(&str) -> Result<H, String>,
{
    let ty_name = ctx.object_type.name.as_str();
    let prop = ctx.lifecycle.property.as_str();

    for transition in &ctx.lifecycle.transitions {
        for from in &transition.from {
            let Some((init_state, prefix)) = paths.get(from) else {
                continue;
            };
            *seq = seq.saturating_add(1);
            let mut host = make_host(&format!("lc-{ty_name}-{prop}-{}", *seq))?;
            let id = format!("mbt-{}", *seq);
            let mut path_guards = collect_prefix_guards(ctx.lifecycle, prefix);
            path_guards.extend(transition.guards.iter().cloned());
            let mut init_vals = synthesize_guard_satisfying(
                ctx.pack,
                ctx.object_type,
                ctx.lifecycle,
                &path_guards,
                &id,
                *seq,
            );
            init_vals.insert(prop.to_owned(), JsonValue::String(init_state.clone()));
            seed_and_walk(&mut host, ctx, &id, init_vals, prefix)?;
            fire_transition_expect_ok(
                &mut host,
                ctx,
                &id,
                transition.id.as_str(),
                &transition.to,
                seq.saturating_add(1000),
            )?;
            with_cov(ctx.coverage, |cov| {
                cov.hit_transition(DomainTransition {
                    object_type: ty_name.to_owned(),
                    lifecycle: prop.to_owned(),
                    transition: transition.id.as_str().to_owned(),
                    from: from.clone(),
                    to: transition.to.clone(),
                });
            });
            totals.valid = totals.valid.saturating_add(1);
            host.verify_ledger()?;
            let spec = GuardCheckSpec {
                transition,
                from_state: from,
                init_state,
                prefix,
                id: &id,
                seq: *seq,
            };
            exercise_guard_refusal(ctx, &spec, make_host, totals)?;
        }
    }
    Ok(())
}

fn exercise_guard_refusal<H, F>(
    ctx: &LifecycleCtx<'_>,
    spec: &GuardCheckSpec<'_>,
    make_host: &mut F,
    totals: &mut LifecycleStats,
) -> Result<(), String>
where
    H: PackHost,
    F: FnMut(&str) -> Result<H, String>,
{
    if spec.transition.guards.is_empty() {
        return Ok(());
    }
    let ty_name = ctx.object_type.name.as_str();
    let prop = ctx.lifecycle.property.as_str();
    let tr_id = spec.transition.id.as_str();
    let label = format!("{ty_name}.{prop}.{tr_id}@{}", spec.from_state);

    let Some((mut bad_values, single_mutation)) = synthesize_guard_violating_at_state(
        ctx.pack,
        ctx.object_type,
        ctx.lifecycle,
        spec.from_state,
        &spec.transition.guards,
        spec.id,
        spec.seq,
    ) else {
        totals.uncovered_guard_refusals.push(label);
        return Ok(());
    };

    let mut neg_host = if spec.prefix.is_empty() {
        let mut host = make_host(&format!("lc-guard-{ty_name}-{}", spec.seq))?;
        bad_values.insert(
            prop.to_owned(),
            JsonValue::String(spec.init_state.to_owned()),
        );
        if !seed_and_walk_law_aware(&mut host, ctx, spec.id, bad_values, &[])? {
            totals.uncovered_guard_refusals.push(label);
            return Ok(());
        }
        host
    } else {
        let Some(host) = reach_guard_violating_source_state(
            make_host,
            ctx,
            spec,
            &bad_values,
            single_mutation.as_ref(),
        )?
        else {
            totals.uncovered_guard_refusals.push(label);
            return Ok(());
        };
        host
    };

    let before = neg_host.ledger_len();
    let res = fire_transition_decision(
        &mut neg_host,
        ctx.catalog,
        ty_name,
        prop,
        spec.id,
        tr_id,
        spec.seq.saturating_add(2000),
    );
    match res {
        Err(DecisionRefusal::GuardFalse { .. }) => {
            assert_zero_ledger_writes(&neg_host, before, ty_name, "guard refusal")?;
            totals.disallowed = totals.disallowed.saturating_add(1);
            neg_host.verify_ledger()
        }
        Err(other) => Err(format!(
            "{label}: expected typed DecisionRefusal::GuardFalse, got {other:?}"
        )),
        Ok(()) => Err(format!(
            "{label}: transition unexpectedly committed despite false guard witness"
        )),
    }
}

fn reach_guard_violating_source_state<H, F>(
    make_host: &mut F,
    ctx: &LifecycleCtx<'_>,
    spec: &GuardCheckSpec<'_>,
    bad_values: &JsonObject,
    single_mutation: Option<&(String, JsonValue)>,
) -> Result<Option<H>, String>
where
    H: PackHost,
    F: FnMut(&str) -> Result<H, String>,
{
    let ty_name = ctx.object_type.name.as_str();
    let prop = ctx.lifecycle.property.as_str();
    let is_immutable_source = ctx
        .lifecycle
        .immutable
        .iter()
        .any(|state| state == spec.from_state);

    let prefix_guards = collect_prefix_guards(ctx.lifecycle, spec.prefix);
    let mut seed_bad = bad_values.clone();
    seed_bad.insert(
        prop.to_owned(),
        JsonValue::String(spec.init_state.to_owned()),
    );
    if prefix_guards.is_empty() || super::synth::guards_hold(&prefix_guards, &seed_bad) {
        let mut direct_host = make_host(&format!("lc-guard-direct-{ty_name}-{}", spec.seq))?;
        if seed_and_walk_law_aware(&mut direct_host, ctx, spec.id, seed_bad, spec.prefix)? {
            return Ok(Some(direct_host));
        }
    }
    if is_immutable_source {
        return Ok(None);
    }
    let Some((mut_prop, mut_val)) = single_mutation else {
        return Ok(None);
    };
    let mut fallback_host = make_host(&format!("lc-guard-fallback-{ty_name}-{}", spec.seq))?;
    let mut good_init = synthesize_guard_satisfying(
        ctx.pack,
        ctx.object_type,
        ctx.lifecycle,
        &prefix_guards,
        spec.id,
        spec.seq,
    );
    good_init.insert(
        prop.to_owned(),
        JsonValue::String(spec.init_state.to_owned()),
    );
    seed_and_walk(&mut fallback_host, ctx, spec.id, good_init, spec.prefix)?;
    match set_property_decision(
        &mut fallback_host,
        ctx.catalog,
        ty_name,
        spec.id,
        mut_prop,
        mut_val.clone(),
        spec.seq.saturating_add(1500),
    ) {
        Ok(()) => Ok(Some(fallback_host)),
        Err(DecisionRefusal::Law { .. } | DecisionRefusal::InvariantViolation { .. }) => Ok(None),
        Err(err) => Err(format!(
            "failed setting guard-violating property {ty_name}.{mut_prop}: {err}"
        )),
    }
}

fn exercise_negative_and_immutable<H, F>(
    ctx: &LifecycleCtx<'_>,
    paths: &BTreeMap<String, (String, Vec<String>)>,
    make_host: &mut F,
    seq: &mut u64,
    totals: &mut LifecycleStats,
) -> Result<(), String>
where
    H: PackHost,
    F: FnMut(&str) -> Result<H, String>,
{
    let ty_name = ctx.object_type.name.as_str();
    let prop = ctx.lifecycle.property.as_str();
    let immutable_set: BTreeSet<&str> =
        ctx.lifecycle.immutable.iter().map(String::as_str).collect();
    let all_guards: Vec<_> = ctx
        .lifecycle
        .transitions
        .iter()
        .flat_map(|tr| tr.guards.iter().cloned())
        .collect();

    for (state, (init_state, prefix)) in paths {
        *seq = seq.saturating_add(1);
        let mut host = make_host(&format!("lc-neg-{ty_name}-{state}-{}", *seq))?;
        let id = format!("neg-{}", *seq);
        let mut init_values = synthesize_guard_satisfying(
            ctx.pack,
            ctx.object_type,
            ctx.lifecycle,
            &all_guards,
            &id,
            *seq,
        );
        init_values.insert(prop.to_owned(), JsonValue::String(init_state.clone()));
        seed_and_walk(&mut host, ctx, &id, init_values.clone(), prefix)?;

        check_disallowed_transitions(&mut host, ctx, state, &id, *seq, totals)?;
        check_direct_lifecycle_write(&mut host, ctx, state, &id, *seq, totals)?;
        if immutable_set.contains(state.as_str()) {
            check_immutable_state_write(&mut host, ctx, state, &id, &init_values, *seq, totals)?;
        }
        host.verify_ledger()?;
    }
    Ok(())
}

fn check_disallowed_transitions<H: PackHost>(
    host: &mut H,
    ctx: &LifecycleCtx<'_>,
    state: &str,
    id: &str,
    seq: u64,
    totals: &mut LifecycleStats,
) -> Result<(), String> {
    let ty_name = ctx.object_type.name.as_str();
    let prop = ctx.lifecycle.property.as_str();
    for transition in &ctx.lifecycle.transitions {
        if transition.from.iter().any(|src| src == state) {
            continue;
        }
        let before = host.ledger_len();
        let result = fire_transition_decision(
            host,
            ctx.catalog,
            ty_name,
            prop,
            id,
            transition.id.as_str(),
            seq.saturating_add(3000),
        );
        match result {
            Err(refusal @ DecisionRefusal::NotFromState { .. }) => {
                host.record_decision_refusal(&refusal);
                assert_zero_ledger_writes(host, before, ty_name, "disallowed transition")?;
                with_cov(ctx.coverage, |cov| {
                    cov.hit_disallowed_transition(DomainTransition {
                        object_type: ty_name.to_owned(),
                        lifecycle: prop.to_owned(),
                        transition: transition.id.as_str().to_owned(),
                        from: state.to_owned(),
                        to: transition.to.clone(),
                    });
                });
                totals.disallowed = totals.disallowed.saturating_add(1);
            }
            other => {
                return Err(format!(
                    "{ty_name}.{prop}: disallowed transition '{}' from '{state}' returned {other:?}",
                    transition.id.as_str()
                ));
            }
        }
    }
    Ok(())
}

fn check_direct_lifecycle_write<H: PackHost>(
    host: &mut H,
    ctx: &LifecycleCtx<'_>,
    state: &str,
    id: &str,
    seq: u64,
    totals: &mut LifecycleStats,
) -> Result<(), String> {
    let ty_name = ctx.object_type.name.as_str();
    let prop = ctx.lifecycle.property.as_str();
    let before = host.ledger_len();
    let direct = set_property_decision(
        host,
        ctx.catalog,
        ty_name,
        id,
        prop,
        JsonValue::String(state.to_owned()),
        seq.saturating_add(4000),
    );
    match direct {
        Err(DecisionRefusal::LifecyclePropertySet { .. }) => {
            assert_zero_ledger_writes(host, before, ty_name, "direct lifecycle set")?;
            totals.disallowed = totals.disallowed.saturating_add(1);
            Ok(())
        }
        other => Err(format!(
            "{ty_name}.{prop}: direct write in '{state}' returned {other:?}"
        )),
    }
}

fn check_immutable_state_write<H: PackHost>(
    host: &mut H,
    ctx: &LifecycleCtx<'_>,
    state: &str,
    id: &str,
    init_values: &JsonObject,
    seq: u64,
    totals: &mut LifecycleStats,
) -> Result<(), String> {
    let ty_name = ctx.object_type.name.as_str();
    let prop = ctx.lifecycle.property.as_str();
    let Some((mut_prop, mut_val)) =
        pick_non_lifecycle_mutation(ctx.pack, ctx.object_type, init_values, seq)
    else {
        return Err(format!(
            "{ty_name}.{prop}: immutable state '{state}' has no writable non-lifecycle property to verify IMMUTABLE_STATE_WRITE"
        ));
    };
    let before = host.ledger_len();
    let res = set_property_decision(
        host,
        ctx.catalog,
        ty_name,
        id,
        &mut_prop,
        mut_val,
        seq.saturating_add(5000),
    );
    match res {
        Err(refusal @ DecisionRefusal::ImmutableStateWrite { .. }) => {
            host.record_decision_refusal(&refusal);
            assert_zero_ledger_writes(host, before, ty_name, "immutable state write")?;
            with_cov(ctx.coverage, |cov| {
                cov.hit_immutable_refusal(ty_name, prop, state);
            });
            totals.immutable = totals.immutable.saturating_add(1);
            Ok(())
        }
        other => Err(format!(
            "{ty_name}.{prop}: mutation in immutable state '{state}' returned {other:?}"
        )),
    }
}

pub(crate) struct SeedWalkSpec<'a> {
    pub(crate) pack: &'a AdmittedPack,
    pub(crate) catalog: &'a TypeCatalog<'a>,
    pub(crate) object_type: &'a ObjectTypeDef,
    pub(crate) prop: &'a str,
    pub(crate) id: &'a str,
    pub(crate) values: JsonObject,
    pub(crate) path: &'a [String],
}

fn seed_and_walk<H: PackHost>(
    host: &mut H,
    ctx: &LifecycleCtx<'_>,
    id: &str,
    values: JsonObject,
    path: &[String],
) -> Result<(), String> {
    seed_and_walk_object(
        host,
        SeedWalkSpec {
            pack: ctx.pack,
            catalog: ctx.catalog,
            object_type: ctx.object_type,
            prop: ctx.lifecycle.property.as_str(),
            id,
            values,
            path,
        },
    )
}

pub(crate) fn seed_and_walk_object<H: PackHost>(
    host: &mut H,
    spec: SeedWalkSpec<'_>,
) -> Result<(), String> {
    seed_and_walk_inner(host, spec, false).map(|_| ())
}

fn seed_and_walk_law_aware<H: PackHost>(
    host: &mut H,
    ctx: &LifecycleCtx<'_>,
    id: &str,
    values: JsonObject,
    path: &[String],
) -> Result<bool, String> {
    seed_and_walk_inner(
        host,
        SeedWalkSpec {
            pack: ctx.pack,
            catalog: ctx.catalog,
            object_type: ctx.object_type,
            prop: ctx.lifecycle.property.as_str(),
            id,
            values,
            path,
        },
        true,
    )
}

fn seed_and_walk_inner<H: PackHost>(
    host: &mut H,
    spec: SeedWalkSpec<'_>,
    allow_law_refusal: bool,
) -> Result<bool, String> {
    let SeedWalkSpec {
        pack,
        catalog,
        object_type,
        prop,
        id,
        values,
        path,
    } = spec;
    let ty_name = object_type.name.as_str();
    let key = FactKey::new(ty_name, id);
    let mut graph = collect_required_link_graph(pack, object_type, &key, 1)?;
    // ACTOR-STAMP-1: a type with protected creators is seeded by its governed create.
    if let Some(created) =
        super::seed::seed_stamped(host, pack, catalog, object_type, id, &values, 1)?
    {
        match created {
            Ok(()) => {}
            Err(DecisionRefusal::Law { .. } | DecisionRefusal::InvariantViolation { .. })
                if allow_law_refusal =>
            {
                return Ok(false);
            }
            Err(err) => return Err(format!("seed {ty_name}/{id}: {err}")),
        }
    } else if graph.direct_links.is_empty() {
        let slice = load_slice(host, [key]).map_err(|err| format!("load_slice: {err}"))?;
        let create = DecisionOutput {
            facts: vec![FactChange::Create {
                object_type: object_type.name.clone(),
                key: id.to_owned(),
                values,
            }],
            links: Vec::new(),
            commands: Vec::new(),
            timers: Vec::new(),
        };
        let turn = DecisionTurn {
            id: format!("seed-{id}"),
            now: host.logical_now(),
            world: World::Main,
            consumes: None,
        };
        match commit_decision(host, catalog, &slice, &turn, &create) {
            Ok(_) => {}
            Err(DecisionRefusal::Law { .. } | DecisionRefusal::InvariantViolation { .. })
                if allow_law_refusal =>
            {
                return Ok(false);
            }
            Err(err) => return Err(format!("seed {ty_name}/{id}: {err}")),
        }
    } else {
        super::seed::stamp_link_targets(host, pack, catalog, &mut graph.setup_writes, 1)?;
        graph.setup_writes.push(FactWrite {
            key,
            expected: FactVersion::ABSENT,
            mutation: FactMutation::Put { values },
        });
        graph.setup_links.extend(graph.direct_links);
        match host.commit(CellCommit {
            writes: graph.setup_writes,
            links: graph.setup_links,
            ..CellCommit::empty()
        }) {
            Ok(_) => {}
            Err(CommitError::Law { .. } | CommitError::InvariantViolation { .. })
                if allow_law_refusal =>
            {
                return Ok(false);
            }
            Err(err) => return Err(format!("seed {ty_name}/{id} with required links: {err}")),
        }
    }
    for (step_idx, transition_id) in path.iter().enumerate() {
        let turn_nonce = u64::try_from(step_idx).unwrap_or(0).saturating_add(1);
        match fire_transition_decision(host, catalog, ty_name, prop, id, transition_id, turn_nonce)
        {
            Ok(()) => {}
            Err(DecisionRefusal::Law { .. } | DecisionRefusal::InvariantViolation { .. })
                if allow_law_refusal =>
            {
                return Ok(false);
            }
            Err(err) => {
                return Err(format!("walk {ty_name}/{id} via {transition_id}: {err}"));
            }
        }
    }
    Ok(true)
}

fn fire_transition_expect_ok<H: PackHost>(
    host: &mut H,
    ctx: &LifecycleCtx<'_>,
    id: &str,
    transition_id: &str,
    expected_to: &str,
    turn_nonce: u64,
) -> Result<(), String> {
    let ty_name = ctx.object_type.name.as_str();
    let prop = ctx.lifecycle.property.as_str();
    let before = host.ledger_len();
    fire_transition_decision(
        host,
        ctx.catalog,
        ty_name,
        prop,
        id,
        transition_id,
        turn_nonce,
    )
    .map_err(|err| format!("{ty_name}.{prop}.{transition_id} failed: {err}"))?;
    if host.ledger_len() != before.saturating_add(1) {
        return Err(format!(
            "{ty_name}.{prop}.{transition_id} did not append exactly 1 ledger entry"
        ));
    }
    let loaded = host
        .load(&FactKey::new(ty_name, id))
        .map_err(|err| format!("load after transition: {err}"))?;
    let actual = loaded
        .values
        .as_ref()
        .and_then(|vals| vals.get(prop))
        .and_then(JsonValue::as_str);
    if actual != Some(expected_to) {
        return Err(format!(
            "{ty_name}.{prop}.{transition_id}: expected state '{expected_to}', got {actual:?}"
        ));
    }
    Ok(())
}

fn fire_transition_decision<H: PackHost>(
    host: &mut H,
    catalog: &TypeCatalog<'_>,
    ty_name: &str,
    prop: &str,
    id: &str,
    transition_id: &str,
    turn_nonce: u64,
) -> Result<(), DecisionRefusal<H::Error>> {
    let key = FactKey::new(ty_name, id);
    let slice = load_slice(host, [key]).map_err(DecisionRefusal::Host)?;
    let output = DecisionOutput {
        facts: vec![FactChange::Transition {
            object_type: ObjectTypeName(ty_name.to_owned()),
            key: id.to_owned(),
            lifecycle: PropertyId(prop.to_owned()),
            transition: TransitionId(transition_id.to_owned()),
        }],
        links: Vec::new(),
        commands: Vec::new(),
        timers: Vec::new(),
    };
    let turn = DecisionTurn {
        id: format!("tr-{id}-{transition_id}-{turn_nonce}"),
        now: host.logical_now(),
        world: World::Main,
        consumes: None,
    };
    commit_decision(host, catalog, &slice, &turn, &output).map(|_| ())
}

fn set_property_decision<H: PackHost>(
    host: &mut H,
    catalog: &TypeCatalog<'_>,
    ty_name: &str,
    id: &str,
    prop: &str,
    value: JsonValue,
    turn_nonce: u64,
) -> Result<(), DecisionRefusal<H::Error>> {
    let key = FactKey::new(ty_name, id);
    let slice = load_slice(host, [key]).map_err(DecisionRefusal::Host)?;
    let output = DecisionOutput {
        facts: vec![FactChange::Set {
            object_type: ObjectTypeName(ty_name.to_owned()),
            key: id.to_owned(),
            property: PropertyId(prop.to_owned()),
            value,
        }],
        links: Vec::new(),
        commands: Vec::new(),
        timers: Vec::new(),
    };
    let turn = DecisionTurn {
        id: format!("set-{id}-{prop}-{turn_nonce}"),
        now: host.logical_now(),
        world: World::Main,
        consumes: None,
    };
    commit_decision(host, catalog, &slice, &turn, &output).map(|_| ())
}

pub(crate) fn assert_zero_ledger_writes<H: PackHost>(
    host: &H,
    before: usize,
    subject: &str,
    context: &str,
) -> Result<(), String> {
    let after = host.ledger_len();
    if after != before {
        return Err(format!(
            "{subject} ({context}): expected zero ledger writes, ledger grew from {before} to {after}"
        ));
    }
    Ok(())
}

pub(crate) fn with_cov(coverage: &Arc<Mutex<DomainCoverage>>, f: impl FnOnce(&mut DomainCoverage)) {
    let mut guard = match coverage.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    };
    f(&mut guard);
}
