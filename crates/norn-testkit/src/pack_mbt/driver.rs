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

use norn_kernel::authz::{
    AccessRequest, ActorKind, AttrValue, AuthzSchema, CapabilityGrant, Delegation, Pdp,
    PrincipalInput, ResourceInput, ScopeKey, compile_all_projected, infer_principal_attributes,
};
use norn_kernel::cell_host::{FactKey, FactMutation, FactVersion, FactWrite, JsonObject, JsonValue};
use norn_kernel::norn_authz::ValidatedPolicySet;
use norn_kernel::norn_authz::norn_projections::{
    CedarAttribute, CedarPrincipalAttribute, project_cedar_schema_with_catalog,
    project_cedar_schema_with_catalog_and_principal_attributes, project_host_access_schema,
};
use norn_kernel::norn_checker::law_examples::{Verdict, evaluate_runtime as evaluate_law_example};
use norn_kernel::norn_ir::access::{
    AccessCondition, AccessIr, AccessLiteral, AccessRule, ResourceCompareOp, ResourceOperand,
    ScopeDimension,
};
use norn_kernel::norn_ir::{
    ActionEffectProgram, DecisionOutput, FactChange, Lifecycle, ObjectTypeDef, ObjectTypeName,
    PropertyId, TransitionId, lifecycle_codes,
};
use norn_kernel::{
    ActionCall, ActionDenial, ActionRefusal, CellCommit, CommitError, DecisionRefusal,
    DecisionTurn, TypeCatalog, World, commit_action, commit_decision, load_slice,
};

use super::coverage::{
    DomainCoverage, DomainCoveragePlan, DomainCoverageReport, DomainTransition,
    ObservedPdpAuthorizer, PolicyRuleTarget,
};
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

struct CedarCtx<'a> {
    pack: &'a AdmittedPack,
    catalog: &'a TypeCatalog<'a>,
    policy_set: &'a ValidatedPolicySet,
    projected_attrs: &'a BTreeMap<String, BTreeMap<String, CedarAttribute>>,
    principal_attrs: &'a BTreeMap<String, CedarPrincipalAttribute>,
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
            ("lifecycle_walks_and_refusals", EvidenceLevel::ExhaustiveFinite),
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
        let lifecycles = catalog
            .effective_lifecycles(&object_type.name)
            .unwrap_or_else(|_| object_type.lifecycles.clone());
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
            if transition.from.iter().any(|src| src == &state) && !paths.contains_key(&transition.to)
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
            let mut init_vals = synthesize_guard_satisfying(
                ctx.pack,
                ctx.object_type,
                ctx.lifecycle,
                &transition.guards,
                &id,
                *seq,
            );
            init_vals.insert(prop.to_owned(), JsonValue::String(init_state.clone()));
            seed_and_walk(&mut host, ctx.catalog, ctx.object_type, prop, &id, init_vals, prefix)?;
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
    bad_values.insert(prop.to_owned(), JsonValue::String(spec.init_state.to_owned()));
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
    let immutable_set: BTreeSet<&str> = ctx.lifecycle.immutable.iter().map(String::as_str).collect();
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
        seed_and_walk(&mut host, ctx.catalog, ctx.object_type, prop, &id, init_values.clone(), prefix)?;

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
        links: Vec::new(),
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
    fire_transition_decision(host, ctx.catalog, ty_name, prop, id, transition_id, turn_nonce)
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
        links: Vec::new(),
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
        links: Vec::new(),
    };
    let turn = DecisionTurn {
        id: format!("set-{id}-{prop}-{turn_nonce}"),
        now: host.logical_now(),
        world: World::Main,
        consumes: None,
    };
    commit_decision(host, catalog, &slice, &turn, &output).map(|_| ())
}

fn run_cedar_and_sod_phase<H, F>(
    pack: &AdmittedPack,
    catalog: &TypeCatalog<'_>,
    make_host: &mut F,
    coverage: &Arc<Mutex<DomainCoverage>>,
) -> Result<usize, String>
where
    H: PackHost,
    F: FnMut(&str) -> Result<H, String>,
{
    if pack.access_policies.is_empty() {
        return Ok(0);
    }
    let action_roots: Vec<_> = pack.actions.iter().map(|a| a.root.clone()).collect();
    let plain = project_cedar_schema_with_catalog(
        &pack.object_types,
        &pack.interfaces,
        &pack.property_types,
        &action_roots,
    );
    let inferred = infer_principal_attributes(&pack.access_policies, &plain)
        .map_err(|err| format!("infer_principal_attributes: {err}"))?;
    let projection = project_cedar_schema_with_catalog_and_principal_attributes(
        &pack.object_types,
        &pack.interfaces,
        &pack.property_types,
        &action_roots,
        &inferred,
    );
    let host_schema = project_host_access_schema(&pack.object_types);
    let schema =
        AuthzSchema::from_projected_cedarschema_fragments(&[&projection.schema, &host_schema])
            .map_err(|err| format!("AuthzSchema: {err}"))?;
    let policy_set = compile_all_projected(&schema, &pack.access_policies, &projection)
        .map_err(|err| format!("compile_all_projected: {err}"))?;

    let ctx = CedarCtx {
        pack,
        catalog,
        policy_set: &policy_set,
        projected_attrs: &projection.attributes,
        principal_attrs: &inferred,
        coverage,
    };
    let mut checks = 0usize;
    let mut seq = 0u64;
    for policy in &pack.access_policies {
        for rule in &policy.rules {
            checks = checks.saturating_add(verify_access_rule_matrix(
                &ctx, policy, rule, make_host, &mut seq,
            )?);
        }
    }
    Ok(checks)
}

fn verify_access_rule_matrix<H, F>(
    ctx: &CedarCtx<'_>,
    policy: &AccessIr,
    rule: &AccessRule,
    make_host: &mut F,
    seq: &mut u64,
) -> Result<usize, String>
where
    H: PackHost,
    F: FnMut(&str) -> Result<H, String>,
{
    let scope = build_rule_scope(&rule.scope.dimensions, "org-mbt");
    let foreign_scope = build_rule_scope(&rule.scope.dimensions, "org-foreign");
    let allowed_roles = allowed_roles_for_rule(rule);
    let hint = PolicyRuleTarget {
        policy_id: policy.policy_id.clone(),
        rule_id: rule.rule_id.clone(),
    };
    let (res_ok, prin_ok) =
        build_rule_attributes(rule, ctx.projected_attrs, ctx.principal_attrs, false);
    let mut count = 0usize;

    for action_id in &rule.actions {
        let pdp = build_rule_pdp(ctx.policy_set, action_id, &scope, &foreign_scope)?;

        // 1. Authorized actor => Allow.
        let allow_req = make_access_request(
            "actor-allowed",
            allowed_roles.clone(),
            (&scope, &scope),
            (action_id, rule.resource_type.as_str(), "obj-1"),
            (res_ok.clone(), prin_ok.clone()),
        );
        let allow_expl = pdp.explain(&allow_req);
        if !allow_expl.decision.is_allow() {
            return Err(format!(
                "Cedar matrix: expected Allow for {}/{} action '{action_id}', got {:?}",
                policy.policy_id, rule.rule_id, allow_expl.decision
            ));
        }
        with_cov(ctx.coverage, |cov| {
            cov.hit_cedar_allow(policy.policy_id.clone(), rule.rule_id.clone());
        });
        count = count.saturating_add(1);

        // 2. Unauthorized role => Deny + zero ledger writes.
        if !allowed_roles.is_empty() {
            let deny_req = make_access_request(
                "actor-unauth",
                vec!["__mbt_unauthorized_role__".to_owned()],
                (&scope, &scope),
                (action_id, rule.resource_type.as_str(), "obj-1"),
                (res_ok.clone(), prin_ok.clone()),
            );
            assert_denied(&pdp, &deny_req, "unauthorized role", &hint, action_id)?;
            verify_action_denied_zero_writes(ctx, action_id, &pdp, hint.clone(), deny_req, make_host, seq)?;
            count = count.saturating_add(1);
        }

        // 3. Cross-scope actor => Deny.
        let foreign_req = make_access_request(
            "actor-allowed",
            allowed_roles.clone(),
            (&foreign_scope, &scope),
            (action_id, rule.resource_type.as_str(), "obj-1"),
            (res_ok.clone(), prin_ok.clone()),
        );
        assert_denied(&pdp, &foreign_req, "foreign scope", &hint, action_id)?;
        with_cov(ctx.coverage, |cov| {
            cov.hit_cedar_deny(policy.policy_id.clone(), rule.rule_id.clone());
        });
        count = count.saturating_add(1);

        // 4. Separation of duties (self-approver) => Deny + zero ledger writes.
        if has_sod_condition(rule) {
            let (res_self, prin_self) =
                build_rule_attributes(rule, ctx.projected_attrs, ctx.principal_attrs, true);
            let self_req = make_access_request(
                "actor-requester",
                allowed_roles.clone(),
                (&scope, &scope),
                (action_id, rule.resource_type.as_str(), "obj-1"),
                (res_self, prin_self),
            );
            assert_denied(&pdp, &self_req, "self-approver SoD", &hint, action_id)?;
            verify_action_denied_zero_writes(ctx, action_id, &pdp, hint.clone(), self_req, make_host, seq)?;
            count = count.saturating_add(1);
        }
    }
    Ok(count)
}

fn build_rule_pdp(
    policy_set: &ValidatedPolicySet,
    action_id: &str,
    scope: &ScopeKey,
    foreign_scope: &ScopeKey,
) -> Result<Arc<Pdp>, String> {
    let grants = vec![
        CapabilityGrant {
            subject_id: "actor-allowed".to_owned(),
            scope: scope.clone(),
            session_id: None,
            capability: action_id.to_owned(),
        },
        CapabilityGrant {
            subject_id: "actor-unauth".to_owned(),
            scope: scope.clone(),
            session_id: None,
            capability: action_id.to_owned(),
        },
        CapabilityGrant {
            subject_id: "actor-requester".to_owned(),
            scope: scope.clone(),
            session_id: None,
            capability: action_id.to_owned(),
        },
        CapabilityGrant {
            subject_id: "actor-allowed".to_owned(),
            scope: foreign_scope.clone(),
            session_id: None,
            capability: action_id.to_owned(),
        },
    ];
    Pdp::new(policy_set.clone(), &grants)
        .map(Arc::new)
        .map_err(|err| format!("Pdp: {err}"))
}

fn assert_denied(
    pdp: &Pdp,
    request: &AccessRequest,
    label: &str,
    hint: &PolicyRuleTarget,
    action_id: &str,
) -> Result<(), String> {
    if pdp.explain(request).decision.is_allow() {
        return Err(format!(
            "Cedar matrix ({label}): unexpectedly allowed {}/{} action '{action_id}'",
            hint.policy_id, hint.rule_id
        ));
    }
    Ok(())
}

fn verify_action_denied_zero_writes<H, F>(
    ctx: &CedarCtx<'_>,
    action_id: &str,
    pdp: &Arc<Pdp>,
    hint: PolicyRuleTarget,
    template_request: AccessRequest,
    make_host: &mut F,
    seq: &mut u64,
) -> Result<(), String>
where
    H: PackHost,
    F: FnMut(&str) -> Result<H, String>,
{
    let Some(admitted) = ctx.pack.actions.iter().find(|a| a.root.action_id == action_id) else {
        with_cov(ctx.coverage, |cov| cov.hit_cedar_deny(hint.policy_id, hint.rule_id));
        return Ok(());
    };
    let Some(effect) = &admitted.root.effect_program else {
        with_cov(ctx.coverage, |cov| cov.hit_cedar_deny(hint.policy_id, hint.rule_id));
        return Ok(());
    };
    let target = effect.target();
    let Some(object_type) = ctx
        .pack
        .object_types
        .iter()
        .find(|ty| ty.name.as_str() == target.object_kind)
    else {
        with_cov(ctx.coverage, |cov| cov.hit_cedar_deny(hint.policy_id, hint.rule_id));
        return Ok(());
    };

    *seq = seq.saturating_add(1);
    let mut host = make_host(&format!("cedar-deny-{action_id}-{}", *seq))?;
    let obj_id = format!("cedar-obj-{}", *seq);
    let mut input = synthesize_object(ctx.pack, object_type, &obj_id, *seq);
    input.insert(target.object_id_field.to_owned(), JsonValue::String(obj_id.clone()));
    if let Some(allowed_props) = admitted
        .root
        .input_schema
        .get("properties")
        .and_then(norn_kernel::norn_ir::JsonValue::as_object)
    {
        input.retain(|k, _| allowed_props.contains_key(k));
    }
    if matches!(effect, ActionEffectProgram::FireTransition { .. }) {
        let seeded = synthesize_object(ctx.pack, object_type, &obj_id, *seq);
        let _ = host.commit(CellCommit {
            writes: vec![FactWrite {
                key: FactKey::new(object_type.name.as_str(), &obj_id),
                expected: FactVersion::ABSENT,
                mutation: FactMutation::Put { values: seeded },
            }],
            ..CellCommit::empty()
        });
    }

    let ledger_before = host.ledger_len();
    let scope_map = scope_key_to_map(template_request.scope.as_ref());
    let actor = template_request.principal.subject_id.clone();
    let req_clone = template_request;
    let authorizer = ObservedPdpAuthorizer::new(
        Arc::clone(pdp),
        Arc::clone(ctx.coverage),
        Some(hint.clone()),
        move |auth_req| {
            let mut mapped = req_clone.clone();
            mapped.action_id = auth_req.action.action_id.clone();
            mapped.capability = auth_req.action.action_id.clone();
            mapped.resource.id = auth_req.target.key.clone();
            mapped.resource.object_type = auth_req.target.object_type.as_str().to_owned();
            Ok(mapped)
        },
    );
    let call = ActionCall {
        action: &admitted.root,
        action_artifact_id: &admitted.artifact_id,
        action_version: &admitted.version,
        scope: scope_map,
        actor,
        input,
        idempotency_key: format!("cedar-deny-{}", *seq),
        object_type: None,
        turn: DecisionTurn {
            id: format!("cedar-deny-{}", *seq),
            now: host.logical_now(),
            world: World::Main,
            consumes: None,
        },
        trace: None,
    };
    match commit_action(&mut host, ctx.catalog, &call, &authorizer) {
        Err(ActionRefusal::Denied(ActionDenial::Forbidden { .. })) => {
            assert_zero_ledger_writes(&host, ledger_before, action_id, "Cedar denial")?;
            host.verify_ledger()
        }
        Err(_) => {
            assert_zero_ledger_writes(&host, ledger_before, action_id, "pre-authz refusal")?;
            with_cov(ctx.coverage, |cov| cov.hit_cedar_deny(hint.policy_id, hint.rule_id));
            host.verify_ledger()
        }
        Ok(_) => Err(format!(
            "Cedar denial check failed: action '{action_id}' unexpectedly succeeded"
        )),
    }
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

fn build_rule_scope(dimensions: &[ScopeDimension], org: &str) -> ScopeKey {
    let has = |dim: ScopeDimension| dimensions.contains(&dim);
    ScopeKey {
        org: org.to_owned(),
        tenant: has(ScopeDimension::Tenant).then(|| "tenant-mbt".to_owned()),
        workspace: has(ScopeDimension::Workspace).then(|| "ws-mbt".to_owned()),
        environment: has(ScopeDimension::Environment).then(|| "dev".to_owned()),
        business_unit: has(ScopeDimension::BusinessUnit).then(|| "bu-mbt".to_owned()),
    }
}

fn scope_key_to_map(scope: Option<&ScopeKey>) -> BTreeMap<String, String> {
    let mut map = BTreeMap::new();
    let Some(scope) = scope else {
        return map;
    };
    map.insert("orgId".to_owned(), scope.org.clone());
    if let Some(v) = &scope.tenant {
        map.insert("tenantId".to_owned(), v.clone());
    }
    if let Some(v) = &scope.workspace {
        map.insert("workspaceId".to_owned(), v.clone());
    }
    if let Some(v) = &scope.environment {
        map.insert("environmentId".to_owned(), v.clone());
    }
    if let Some(v) = &scope.business_unit {
        map.insert("businessUnitId".to_owned(), v.clone());
    }
    map
}

fn allowed_roles_for_rule(rule: &AccessRule) -> Vec<String> {
    let mut roles = Vec::new();
    for cond in &rule.all_of {
        if let AccessCondition::Role(role_cond) = cond
            && let Some(first) = role_cond.any_of.first()
        {
            roles.push(first.clone());
        }
    }
    roles
}

fn has_sod_condition(rule: &AccessRule) -> bool {
    rule.all_of.iter().any(|cond| {
        matches!(
            cond,
            AccessCondition::Resource(res)
                if res.op == ResourceCompareOp::Ne
                    && matches!(res.value, ResourceOperand::PrincipalAttribute(_))
        )
    })
}

fn build_rule_attributes(
    rule: &AccessRule,
    projected_attrs: &BTreeMap<String, BTreeMap<String, CedarAttribute>>,
    principal_attrs: &BTreeMap<String, CedarPrincipalAttribute>,
    violate_sod: bool,
) -> (BTreeMap<String, AttrValue>, BTreeMap<String, AttrValue>) {
    let mut res = default_resource_attrs(projected_attrs.get(rule.resource_type.as_str()));
    let mut prin = default_principal_attrs(principal_attrs);
    for cond in &rule.all_of {
        let AccessCondition::Resource(rc) = cond else {
            continue;
        };
        apply_resource_condition(rc, principal_attrs, violate_sod, &mut res, &mut prin);
    }
    (res, prin)
}

fn default_resource_attrs(
    attrs: Option<&BTreeMap<String, CedarAttribute>>,
) -> BTreeMap<String, AttrValue> {
    let mut res = BTreeMap::new();
    let Some(attrs) = attrs else {
        return res;
    };
    for (name, kind) in attrs {
        let val = match kind {
            CedarAttribute::Bool => AttrValue::Bool(true),
            CedarAttribute::Long => AttrValue::Long(10),
            CedarAttribute::String => AttrValue::String(format!("val-{name}")),
            CedarAttribute::Link(_) => continue,
        };
        res.insert(name.clone(), val);
    }
    res
}

fn default_principal_attrs(
    principal_attrs: &BTreeMap<String, CedarPrincipalAttribute>,
) -> BTreeMap<String, AttrValue> {
    let mut prin = BTreeMap::new();
    for (name, kind) in principal_attrs {
        let val = match kind {
            CedarPrincipalAttribute::Bool => AttrValue::Bool(true),
            CedarPrincipalAttribute::Long => AttrValue::Long(10),
            CedarPrincipalAttribute::String => AttrValue::String("actor-allowed".to_owned()),
            CedarPrincipalAttribute::BoolSet => AttrValue::BoolSet(vec![true]),
            CedarPrincipalAttribute::LongSet => AttrValue::LongSet(vec![10]),
            CedarPrincipalAttribute::StringSet => {
                AttrValue::StringSet(vec!["actor-allowed".to_owned()])
            }
        };
        prin.insert(name.clone(), val);
    }
    prin
}

fn apply_resource_condition(
    rc: &norn_kernel::norn_ir::access::ResourceCondition,
    principal_attrs: &BTreeMap<String, CedarPrincipalAttribute>,
    violate_sod: bool,
    res: &mut BTreeMap<String, AttrValue>,
    prin: &mut BTreeMap<String, AttrValue>,
) {
    match &rc.value {
        ResourceOperand::Literal(lit) => {
            let val = match lit {
                AccessLiteral::Bool(b) => {
                    AttrValue::Bool(if rc.op == ResourceCompareOp::Ne { !*b } else { *b })
                }
                AccessLiteral::Long(n) => {
                    let adjusted = match rc.op {
                        ResourceCompareOp::Eq | ResourceCompareOp::Le | ResourceCompareOp::Ge => *n,
                        ResourceCompareOp::Ne | ResourceCompareOp::Gt => n.saturating_add(1),
                        ResourceCompareOp::Lt => n.saturating_sub(1),
                        ResourceCompareOp::In => *n,
                    };
                    AttrValue::Long(adjusted)
                }
                AccessLiteral::String(s) => AttrValue::String(if rc.op == ResourceCompareOp::Ne {
                    format!("not-{s}")
                } else {
                    s.clone()
                }),
            };
            res.insert(rc.name.clone(), val);
        }
        ResourceOperand::PrincipalAttribute(pa) => {
            let p_name = pa.principal_attribute.clone();
            if rc.op == ResourceCompareOp::Ne {
                res.insert(rc.name.clone(), AttrValue::String("actor-requester".to_owned()));
                let caller = if violate_sod {
                    "actor-requester"
                } else {
                    "actor-allowed"
                };
                prin.insert(p_name, AttrValue::String(caller.to_owned()));
            } else {
                res.insert(rc.name.clone(), AttrValue::String("shared-attr".to_owned()));
                if matches!(principal_attrs.get(&p_name), Some(CedarPrincipalAttribute::StringSet))
                {
                    prin.insert(p_name, AttrValue::StringSet(vec!["shared-attr".to_owned()]));
                } else {
                    prin.insert(p_name, AttrValue::String("shared-attr".to_owned()));
                }
            }
        }
    }
}

fn make_access_request(
    actor: &str,
    roles: Vec<String>,
    scopes: (&ScopeKey, &ScopeKey),
    target: (&str, &str, &str),
    attrs: (BTreeMap<String, AttrValue>, BTreeMap<String, AttrValue>),
) -> AccessRequest {
    AccessRequest {
        principal: PrincipalInput {
            subject_id: actor.to_owned(),
            actor_kind: ActorKind::User,
            agent_session_id: None,
            roles,
            attributes: attrs.1,
        },
        scope: Some(scopes.0.clone()),
        capability: target.0.to_owned(),
        action_id: target.0.to_owned(),
        resource: ResourceInput {
            object_type: target.1.to_owned(),
            id: target.2.to_owned(),
            scope: scopes.1.clone(),
            attributes: attrs.0,
        },
        context: BTreeMap::new(),
        binding: None,
        delegation: Delegation::Direct,
    }
}

fn assert_zero_ledger_writes<H: PackHost>(
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

fn with_cov(coverage: &Arc<Mutex<DomainCoverage>>, f: impl FnOnce(&mut DomainCoverage)) {
    let mut guard = match coverage.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    };
    f(&mut guard);
}
