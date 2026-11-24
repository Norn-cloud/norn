//! Generic IR-driven Model-Based Testing (MBT) driver (#1656).
//!
//! Given *any* [`AdmittedPack`] and a [`PackHost`] factory (for example, a real
//! `SqliteCell` on disk), executes:
//! 1. **All-states & all-transitions lifecycle walk** (`exhaustive-finite`):
//!    reaches every reachable state and executes every valid transition, plus
//!    verifies false guards refuse with zero ledger writes.
//! 2. **Disallowed transitions & immutable state/entity refusals**
//!    (`exhaustive-finite`): asserts `UNDECLARED_WRITE`, `DIRECT_WRITE`,
//!    `IMMUTABLE_STATE_WRITE`, and `commit.immutable_object` with zero ledger
//!    writes.
//! 3. **Full Cedar authorization & SoD matrix** (`exhaustive-finite`): compiles
//!    `AccessIR` via real `norn-authz` Cedar PDP and tests authorized actors,
//!    unauthorized roles, foreign scopes, and self-approvers (`PermissionDenied`
//!    + zero ledger writes).
//! 4. **`LAW-EXAMPLE-1` → real cell runtime parity** (`runtime-enforced`):
//!    replays every `LawExampleCase` through both `norn_checker::law_examples`
//!    and real `PackHost::commit`, asserting 100% agreement.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::sync::{Arc, Mutex};

use norn_kernel::cell_host::{
    FactKey, FactMutation, FactVersion, FactWrite, JsonObject, JsonValue,
};
use norn_kernel::norn_checker::law_examples::{Verdict, evaluate_runtime as evaluate_law_example};
use norn_kernel::norn_ir::{
    DecisionOutput, FactChange, Lifecycle, ObjectTypeDef, ObjectTypeName, PropertyId, TransitionId,
    lifecycle_codes,
};
use norn_kernel::{
    CellCommit, CommitError, DecisionRefusal, DecisionTurn, TypeCatalog, World, commit_decision,
    load_slice,
};

use super::cedar::run_cedar_and_sod_phase;
use super::coverage::{DomainCoverage, DomainCoveragePlan, DomainCoverageReport, DomainTransition};
use super::synth::{
    pick_non_lifecycle_mutation, synthesize_guard_satisfying, synthesize_guard_violating,
    synthesize_object,
};
use super::{AdmittedPack, EvidenceLevel, LawExampleCase, PackHost, action_call_object_type};

/// Summary of one full IR-driven MBT execution against a [`PackHost`].
#[derive(Debug, Clone)]
pub struct PackMbtReport {
    /// Pack identifier.
    pub pack_name: String,
    /// Evidence levels achieved across executed phases.
    pub evidence: Vec<(&'static str, EvidenceLevel)>,
    /// Number of valid lifecycle transitions executed.
    pub valid_transitions_run: usize,
    /// Number of disallowed transitions verified to refuse with zero ledger writes.
    pub disallowed_transitions_checked: usize,
    /// Number of immutable-state and immutable-entity refusals verified.
    pub immutable_refusals_checked: usize,
    /// Number of Cedar allow/deny/SoD matrix checks executed.
    pub cedar_checks_run: usize,
    /// Number of `LAW-EXAMPLE-1` examples replayed for checker-vs-runtime parity.
    pub law_examples_checked: usize,
    /// Structured domain-model coverage report.
    pub coverage: DomainCoverageReport,
}

#[derive(Default)]
struct LifecycleStats {
    valid: usize,
    disallowed: usize,
    immutable: usize,
}

struct LifecycleCtx<'a> {
    pack: &'a AdmittedPack,
    catalog: &'a TypeCatalog<'a>,
    object_type: &'a ObjectTypeDef,
    lifecycle: &'a Lifecycle,
    coverage: &'a Arc<Mutex<DomainCoverage>>,
}

/// Runs the complete generic IR-driven MBT suite on `pack` using `make_host` to
/// open fresh isolated cells per verification phase.
///
/// # Errors
///
/// Returns a detailed error if any lifecycle walk, refusal invariant, Cedar/SoD
/// check, ledger-zero-write check, `verify_ledger()`, or `LAW-EXAMPLE-1` parity
/// assertion fails.
pub fn run_pack_mbt<H, F>(pack: &AdmittedPack, mut make_host: F) -> Result<PackMbtReport, String>
where
    H: PackHost,
    F: FnMut(&str) -> Result<H, String>,
{
    let plan = DomainCoveragePlan::from_pack(pack);
    let coverage = Arc::new(Mutex::new(DomainCoverage::default()));
    let extra = [action_call_object_type()];
    let catalog = pack.type_catalog(&extra);

    let lc = run_lifecycle_and_immutability_phase(pack, &catalog, &mut make_host, &coverage)?;
    let cedar_checks_run = run_cedar_and_sod_phase(pack, &catalog, &mut make_host, &coverage)?;
    let law_examples_checked = run_law_example_parity_phase(pack, &mut make_host, &coverage)?;

    let snapshot = match coverage.lock() {
        Ok(guard) => guard.clone(),
        Err(poisoned) => poisoned.into_inner().clone(),
    };
    let report = snapshot.report(pack.name.clone(), &plan);

    Ok(PackMbtReport {
        pack_name: pack.name.clone(),
        evidence: vec![
            (
                "lifecycle_walks_and_refusals",
                EvidenceLevel::ExhaustiveFinite,
            ),
            ("immutable_state_refusals", EvidenceLevel::ExhaustiveFinite),
            ("cedar_and_sod_matrix", EvidenceLevel::ExhaustiveFinite),
            ("law_example_runtime_parity", EvidenceLevel::RuntimeEnforced),
        ],
        valid_transitions_run: lc.valid,
        disallowed_transitions_checked: lc.disallowed,
        immutable_refusals_checked: lc.immutable,
        cedar_checks_run,
        law_examples_checked,
        coverage: report,
    })
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
    for object_type in &pack.object_types {
        if object_type.immutable {
            verify_immutable_entity(pack, object_type, make_host, coverage)?;
            totals.immutable = totals.immutable.saturating_add(1);
        }
        let lifecycles: Vec<Lifecycle> = catalog
            .lifecycles(&object_type.name)
            .into_values()
            .collect();
        for lifecycle in &lifecycles {
            let ctx = LifecycleCtx {
                pack,
                catalog,
                object_type,
                lifecycle,
                coverage,
            };
            let paths = bfs_paths_to_states(lifecycle);
            let mut seq = 0u64;
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
) -> Result<(), String>
where
    H: PackHost,
    F: FnMut(&str) -> Result<H, String>,
{
    let ty_name = object_type.name.as_str();
    let mut host = make_host(&format!("immutable-entity-{ty_name}"))?;
    let key = FactKey::new(ty_name, "imm-obj-1");
    let values = synthesize_object(pack, object_type, "imm-obj-1", 1);
    let create_commit = CellCommit {
        writes: vec![FactWrite {
            key: key.clone(),
            expected: FactVersion::ABSENT,
            mutation: FactMutation::Put {
                values: values.clone(),
            },
        }],
        ..CellCommit::empty()
    };
    if host.commit(create_commit).is_err() {
        return Ok(());
    }
    let ledger_before = host.ledger_len();
    let loaded = host.load(&key).map_err(|err| format!("{err}"))?;
    let mut mutated = values;
    if let Some((prop, val)) = pick_non_lifecycle_mutation(pack, object_type, &mutated, 7) {
        mutated.insert(prop, val);
    }
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
                cov.hit_immutable_refusal(ty_name, "<immutable_object>");
            });
            host.verify_ledger()
        }
        other => Err(format!(
            "immutable entity {ty_name} did not refuse mutation with ImmutableObject: {other:?}"
        )),
    }
}

fn bfs_paths_to_states(lifecycle: &Lifecycle) -> BTreeMap<String, (String, Vec<String>)> {
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

fn collect_prefix_guards(
    lifecycle: &Lifecycle,
    prefix: &[String],
) -> Vec<norn_kernel::norn_ir::ZenExpression> {
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
            seed_and_walk(
                &mut host,
                ctx.catalog,
                ctx.object_type,
                prop,
                &id,
                init_vals,
                prefix,
            )?;
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
                    from: from.clone(),
                    to: transition.to.clone(),
                });
            });
            totals.valid = totals.valid.saturating_add(1);
            host.verify_ledger()?;
            let spec = GuardCheckSpec {
                transition,
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

struct GuardCheckSpec<'a> {
    transition: &'a norn_kernel::norn_ir::Transition,
    init_state: &'a str,
    prefix: &'a [String],
    id: &'a str,
    seq: u64,
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
    let Some(mut bad_values) = synthesize_guard_violating(
        ctx.pack,
        ctx.object_type,
        ctx.lifecycle,
        &spec.transition.guards,
        spec.id,
        spec.seq,
    ) else {
        return Ok(());
    };
    let ty_name = ctx.object_type.name.as_str();
    let prop = ctx.lifecycle.property.as_str();
    bad_values.insert(
        prop.to_owned(),
        JsonValue::String(spec.init_state.to_owned()),
    );
    let mut neg_host = make_host(&format!("lc-guard-{ty_name}-{}", spec.seq))?;
    if seed_and_walk(
        &mut neg_host,
        ctx.catalog,
        ctx.object_type,
        prop,
        spec.id,
        bad_values,
        spec.prefix,
    )
    .is_err()
    {
        return Ok(());
    }
    let before = neg_host.ledger_len();
    let res = fire_transition_decision(
        &mut neg_host,
        ctx.catalog,
        ty_name,
        prop,
        spec.id,
        spec.transition.id.as_str(),
        spec.seq.saturating_add(2000),
    );
    if let Err(refusal) = res
        && refusal.code().starts_with("decision.guard")
    {
        assert_zero_ledger_writes(&neg_host, before, ty_name, "guard refusal")?;
        totals.disallowed = totals.disallowed.saturating_add(1);
    }
    Ok(())
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
        seed_and_walk(
            &mut host,
            ctx.catalog,
            ctx.object_type,
            prop,
            &id,
            init_values.clone(),
            prefix,
        )?;

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
            Err(refusal) if refusal.code() == lifecycle_codes::UNDECLARED_WRITE => {
                assert_zero_ledger_writes(host, before, ty_name, "disallowed transition")?;
                with_cov(ctx.coverage, |cov| {
                    cov.hit_disallowed_transition(DomainTransition {
                        object_type: ty_name.to_owned(),
                        lifecycle: prop.to_owned(),
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
        Err(refusal) if refusal.code() == lifecycle_codes::DIRECT_WRITE => {
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
        return Ok(());
    };
    let before_imm = host.ledger_len();
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
        Err(refusal) if refusal.code() == lifecycle_codes::IMMUTABLE_STATE_WRITE => {
            assert_zero_ledger_writes(host, before_imm, ty_name, "immutable state write")?;
            with_cov(ctx.coverage, |cov| {
                cov.hit_immutable_refusal(ty_name, state.to_owned());
            });
            totals.immutable = totals.immutable.saturating_add(1);
            Ok(())
        }
        other => Err(format!(
            "{ty_name}.{prop}: mutation in immutable state '{state}' returned {other:?}"
        )),
    }
}

fn seed_and_walk<H: PackHost>(
    host: &mut H,
    catalog: &TypeCatalog<'_>,
    object_type: &ObjectTypeDef,
    prop: &str,
    id: &str,
    values: JsonObject,
    path: &[String],
) -> Result<(), String> {
    let ty_name = object_type.name.as_str();
    let key = FactKey::new(ty_name, id);
    let slice = load_slice(host, [key]).map_err(|err| format!("load_slice: {err}"))?;
    let create = DecisionOutput {
        facts: vec![FactChange::Create {
            object_type: object_type.name.clone(),
            key: id.to_owned(),
            values,
        }],
        commands: Vec::new(),
        timers: Vec::new(),
    };
    let turn = DecisionTurn {
        id: format!("seed-{id}"),
        now: host.logical_now(),
        world: World::Main,
        consumes: None,
    };
    commit_decision(host, catalog, &slice, &turn, &create)
        .map_err(|err| format!("seed {ty_name}/{id}: {err}"))?;
    for (step_idx, transition_id) in path.iter().enumerate() {
        let turn_nonce = u64::try_from(step_idx).unwrap_or(0).saturating_add(1);
        fire_transition_decision(host, catalog, ty_name, prop, id, transition_id, turn_nonce)
            .map_err(|err| format!("walk {ty_name}/{id} via {transition_id}: {err}"))?;
    }
    Ok(())
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

fn run_law_example_parity_phase<H, F>(
    pack: &AdmittedPack,
    make_host: &mut F,
    coverage: &Arc<Mutex<DomainCoverage>>,
) -> Result<usize, String>
where
    H: PackHost,
    F: FnMut(&str) -> Result<H, String>,
{
    let mut checked = 0usize;
    for (idx, example) in pack.law_examples.iter().enumerate() {
        verify_single_law_example(pack, example, idx, make_host, coverage)?;
        checked = checked.saturating_add(1);
    }
    Ok(checked)
}

fn verify_single_law_example<H, F>(
    pack: &AdmittedPack,
    example: &LawExampleCase,
    idx: usize,
    make_host: &mut F,
    coverage: &Arc<Mutex<DomainCoverage>>,
) -> Result<(), String>
where
    H: PackHost,
    F: FnMut(&str) -> Result<H, String>,
{
    let law = pack
        .laws
        .iter()
        .find(|law| law.id == example.law_id)
        .ok_or_else(|| {
            format!(
                "LAW-EXAMPLE-1 '{}': unknown law '{}'",
                example.name, example.law_id
            )
        })?;
    let checker_verdict = evaluate_law_example(law, &JsonValue::Object(example.subject.clone()))
        .map_err(|err| format!("LAW-EXAMPLE-1 '{}': checker error: {err}", example.name))?;
    if checker_verdict != example.expected {
        return Err(format!(
            "LAW-EXAMPLE-1 '{}': expected {:?}, checker returned {:?}",
            example.name, example.expected, checker_verdict
        ));
    }

    let mut host = make_host(&format!("law-example-{idx}"))?;
    let ledger_before = host.ledger_len();
    let key = FactKey::new(law.subject.as_str(), format!("law-ex-{idx}"));
    let commit = CellCommit {
        writes: vec![FactWrite {
            key,
            expected: FactVersion::ABSENT,
            mutation: FactMutation::Put {
                values: example.subject.clone(),
            },
        }],
        ..CellCommit::empty()
    };
    let runtime_result = host.commit(commit);
    match (&checker_verdict, runtime_result) {
        (Verdict::Holds | Verdict::NotApplicable, Ok(_)) => {
            if host.ledger_len() != ledger_before.saturating_add(1) {
                return Err(format!(
                    "LAW-EXAMPLE-1 '{}': accepted commit did not advance ledger",
                    example.name
                ));
            }
            with_cov(coverage, |cov| cov.hit_law_hold(law.id.clone()));
            host.verify_ledger()
        }
        (Verdict::Violated, Err(CommitError::Law { .. })) => {
            assert_zero_ledger_writes(&host, ledger_before, &example.name, "law violation")?;
            with_cov(coverage, |cov| cov.hit_law_violation(law.id.clone()));
            host.verify_ledger()
        }
        (Verdict::Excepted { .. }, Ok(_)) => {
            with_cov(coverage, |cov| cov.hit_law_hold(law.id.clone()));
            host.verify_ledger()
        }
        (Verdict::Excepted { .. }, Err(CommitError::Law { .. })) => {
            assert_zero_ledger_writes(&host, ledger_before, &example.name, "excepted fail-closed")?;
            host.verify_ledger()
        }
        (verdict, outcome) => Err(format!(
            "LAW-EXAMPLE-1 '{}' parity mismatch: checker={verdict:?}, runtime={outcome:?}",
            example.name
        )),
    }
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
