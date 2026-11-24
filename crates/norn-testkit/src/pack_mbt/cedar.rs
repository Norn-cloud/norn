//! Full Cedar authorization and Separation-of-Duties (`SoD`) matrix driver (#1656).
//!
//! Compiles an [`AdmittedPack`]'s `AccessIR` policies through `norn-authz` and
//! drives real `commit_action` calls against a [`PackHost`] with attributes
//! derived directly from the committed fixture (`ActionAuthzRequest::target_values`):
//! 1. **Authorized control** (`Ok(_)` + ledger +1 + `verify_ledger()`),
//! 2. **Unauthorized role** (`Err(ActionRefusal::PermissionDenied)` + zero writes),
//! 3. **Foreign scope** (`Err(ActionRefusal::PermissionDenied)` + zero writes),
//! 4. **Self-approver `SoD`** (`Err(ActionRefusal::PermissionDenied)` + zero writes).

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex};

use norn_kernel::authz::{
    AccessRequest, ActorKind, AttrValue, AuthzSchema, CapabilityGrant, Delegation, Pdp,
    PrincipalInput, ResourceInput, ScopeKey, compile_all_projected, infer_principal_attributes,
};
use norn_kernel::cell_host::{
    FactKey, FactMutation, FactVersion, FactWrite, JsonObject, JsonValue,
};
use norn_kernel::pack_mbt_support::cedar_projection::{
    CedarAttribute, CedarPrincipalAttribute, project_cedar_schema_with_catalog,
    project_cedar_schema_with_catalog_and_principal_attributes, project_host_access_schema,
};
use norn_kernel::pack_mbt_support::{
    AccessCondition, AccessIr, AccessLiteral, AccessRule, ActionEffectProgram, IrJsonValue,
    ObjectTypeDef, PropertyId, ResourceCompareOp, ResourceCondition, ResourceOperand,
    ScopeDimension, SliceKey, TransitionId, ValidatedPolicySet, ZenExpression,
};
use norn_kernel::{
    ActionCall, ActionRefusal, CellCommit, DecisionTurn, TypeCatalog, World, commit_action,
};

use super::coverage::{DomainCoverage, ObservedPdpAuthorizer, PolicyRuleTarget};
use super::driver::{
    SeedWalkSpec, assert_zero_ledger_writes, bfs_paths_to_states, collect_prefix_guards,
    seed_and_walk_object, with_cov,
};
use super::synth::{collect_required_link_graph, synthesize_guard_satisfying, synthesize_object};
use super::{AdmittedAction, AdmittedPack, PackHost};

struct CedarCtx<'a> {
    pack: &'a AdmittedPack,
    catalog: &'a TypeCatalog<'a>,
    policy_set: &'a ValidatedPolicySet,
    projected_attrs: &'a BTreeMap<String, BTreeMap<String, CedarAttribute>>,
    principal_attrs: &'a BTreeMap<String, CedarPrincipalAttribute>,
    coverage: &'a Arc<Mutex<DomainCoverage>>,
}

struct RuleActionCtx<'a> {
    cedar: &'a CedarCtx<'a>,
    rule: &'a AccessRule,
    scope: &'a ScopeKey,
    foreign_scope: &'a ScopeKey,
    allowed_roles: &'a [String],
    hint: &'a PolicyRuleTarget,
}

struct ActionAttempt<'a> {
    actor: &'a str,
    roles: &'a [String],
    caller_scope: &'a ScopeKey,
    resource_scope: &'a ScopeKey,
    label: &'a str,
    expect_allow: bool,
}

/// Runs the exhaustive Cedar & `SoD` matrix across every `AccessIR` rule in `pack`.
///
/// # Errors
///
/// Returns an error if Cedar compilation fails, fixture setup fails, an
/// authorized control fails, an unauthorized/foreign-scope/self-approver call
/// does not fail with typed `ActionRefusal::PermissionDenied`, or a denial
/// writes to the ledger.
pub(crate) fn run_cedar_and_sod_phase<H, F>(
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
    let allowed_roles = allowed_roles_for_rule(rule);
    let hint = PolicyRuleTarget {
        policy_id: policy.policy_id.clone(),
        rule_id: rule.rule_id.clone(),
    };
    let mut count = 0usize;
    for action_id in &rule.actions {
        let action_opt = ctx
            .pack
            .actions
            .iter()
            .find(|a| a.root.action_id == *action_id);
        let scope = build_effective_scope(
            &rule.scope.dimensions,
            action_opt.map(|a| a.root.scope_required.as_slice()),
            "org-mbt",
        );
        let foreign_scope = build_effective_scope(
            &rule.scope.dimensions,
            action_opt.map(|a| a.root.scope_required.as_slice()),
            "org-foreign",
        );
        let rc = RuleActionCtx {
            cedar: ctx,
            rule,
            scope: &scope,
            foreign_scope: &foreign_scope,
            allowed_roles: &allowed_roles,
            hint: &hint,
        };
        let checked = verify_rule_action_matrix(&rc, action_id, action_opt, make_host, seq)?;
        count = count.saturating_add(checked);
    }
    Ok(count)
}

struct ActionTarget<'a> {
    admitted: &'a AdmittedAction,
    effect: &'a ActionEffectProgram,
    object_type: &'a ObjectTypeDef,
    pdp: &'a Arc<Pdp>,
}

fn resolve_action_target<'a>(
    rc: &'a RuleActionCtx<'a>,
    action_id: &str,
    action_opt: Option<&'a AdmittedAction>,
    pdp: &'a Arc<Pdp>,
) -> Result<ActionTarget<'a>, String> {
    let Some(admitted) = action_opt else {
        return Err(format!(
            "Cedar matrix: policy rule {}/{} references undeclared action '{action_id}'",
            rc.hint.policy_id, rc.hint.rule_id
        ));
    };
    let Some(effect) = &admitted.root.effect_program else {
        return Err(format!(
            "Cedar matrix: action '{action_id}' in {}/{} has no effectProgram for runtime verification",
            rc.hint.policy_id, rc.hint.rule_id
        ));
    };
    let object_type = rc
        .cedar
        .pack
        .object_types
        .iter()
        .find(|ty| ty.name.as_str() == effect.target().object_kind)
        .ok_or_else(|| {
            format!(
                "Cedar matrix: action '{action_id}' targets unknown entity '{}'",
                effect.target().object_kind
            )
        })?;
    Ok(ActionTarget {
        admitted,
        effect,
        object_type,
        pdp,
    })
}

fn verify_rule_action_matrix<H, F>(
    rc: &RuleActionCtx<'_>,
    action_id: &str,
    action_opt: Option<&AdmittedAction>,
    make_host: &mut F,
    seq: &mut u64,
) -> Result<usize, String>
where
    H: PackHost,
    F: FnMut(&str) -> Result<H, String>,
{
    let pdp = build_rule_pdp(rc.cedar.policy_set, action_id, rc.scope, rc.foreign_scope)?;
    let target = resolve_action_target(rc, action_id, action_opt, &pdp)?;
    let unauth_roles = vec!["__mbt_unauthorized_role__".to_owned()];
    let mut attempts = vec![ActionAttempt {
        actor: "actor-allowed",
        roles: rc.allowed_roles,
        caller_scope: rc.scope,
        resource_scope: rc.scope,
        label: "authorized control",
        expect_allow: true,
    }];
    if !rc.allowed_roles.is_empty() {
        attempts.push(ActionAttempt {
            actor: "actor-unauth",
            roles: &unauth_roles,
            caller_scope: rc.scope,
            resource_scope: rc.scope,
            label: "unauthorized role",
            expect_allow: false,
        });
    }
    attempts.push(ActionAttempt {
        actor: "actor-allowed",
        roles: rc.allowed_roles,
        caller_scope: rc.foreign_scope,
        resource_scope: rc.scope,
        label: "foreign scope",
        expect_allow: false,
    });
    if has_sod_condition(rc.rule) {
        attempts.push(ActionAttempt {
            actor: "actor-requester",
            roles: rc.allowed_roles,
            caller_scope: rc.scope,
            resource_scope: rc.scope,
            label: "self-approver SoD",
            expect_allow: false,
        });
    }
    for attempt in &attempts {
        run_action_attempt(rc, &target, attempt, make_host, seq)?;
    }
    Ok(attempts.len())
}

fn run_action_attempt<H, F>(
    rc: &RuleActionCtx<'_>,
    target: &ActionTarget<'_>,
    attempt: &ActionAttempt<'_>,
    make_host: &mut F,
    seq: &mut u64,
) -> Result<(), String>
where
    H: PackHost,
    F: FnMut(&str) -> Result<H, String>,
{
    *seq = seq.saturating_add(1);
    let action_id = target.admitted.root.action_id.as_str();
    let mut host = make_host(&format!("cedar-{action_id}-{}", *seq))?;
    let obj_id = format!("cedar-obj-{}", *seq);
    let input = prepare_action_fixture(rc, target, &mut host, &obj_id, *seq)?;

    let ledger_before = host.ledger_len();
    let scope_map = scope_key_to_map(attempt.caller_scope);
    let res_attrs_schema = rc
        .cedar
        .projected_attrs
        .get(rc.rule.resource_type.as_str())
        .cloned()
        .unwrap_or_default();
    let prin_schema = rc.cedar.principal_attrs.clone();
    let rule_clone = rc.rule.clone();
    let roles = attempt.roles.to_vec();
    let caller_scope = attempt.caller_scope.clone();
    let resource_scope = attempt.resource_scope.clone();
    let authorizer = ObservedPdpAuthorizer::new(
        Arc::clone(target.pdp),
        Arc::clone(rc.cedar.coverage),
        Some(rc.hint.clone()),
        move |auth_req| {
            let res_attrs =
                project_resource_attrs_from_values(auth_req.target_values, &res_attrs_schema);
            let prin_attrs =
                build_principal_attrs_for_actor(auth_req.actor, &rule_clone, &prin_schema);
            Ok(AccessRequest {
                principal: PrincipalInput {
                    subject_id: auth_req.actor.to_owned(),
                    actor_kind: ActorKind::User,
                    agent_session_id: None,
                    roles: roles.clone(),
                    attributes: prin_attrs,
                },
                scope: Some(caller_scope.clone()),
                capability: auth_req.action.action_id.clone(),
                action_id: auth_req.action.action_id.clone(),
                resource: ResourceInput {
                    object_type: auth_req.target.object_type.as_str().to_owned(),
                    id: auth_req.target.key.clone(),
                    scope: resource_scope.clone(),
                    attributes: res_attrs,
                },
                context: BTreeMap::new(),
                binding: None,
                delegation: Delegation::Direct,
            })
        },
    );

    let call = ActionCall {
        action: &target.admitted.root,
        action_artifact_id: &target.admitted.artifact_id,
        action_version: &target.admitted.version,
        scope: scope_map,
        actor: attempt.actor.to_owned(),
        input,
        idempotency_key: format!("cedar-call-{}", *seq),
        object_type: None,
        turn: DecisionTurn {
            id: format!("cedar-turn-{}", *seq),
            now: host.logical_now(),
            world: World::Main,
            consumes: None,
        },
        trace: None,
    };

    let outcome = commit_action(&mut host, rc.cedar.catalog, &call, &authorizer);
    check_action_attempt_outcome(rc, &host, action_id, attempt, ledger_before, outcome)
}

fn check_action_attempt_outcome<H: PackHost>(
    rc: &RuleActionCtx<'_>,
    host: &H,
    action_id: &str,
    attempt: &ActionAttempt<'_>,
    ledger_before: usize,
    outcome: Result<norn_kernel::ActionOutcome, ActionRefusal<H::Error>>,
) -> Result<(), String> {
    if attempt.expect_allow {
        match outcome {
            Ok(_) => {
                if host.ledger_len() <= ledger_before {
                    return Err(format!(
                        "Cedar matrix ({}): authorized control for '{action_id}' did not append to ledger",
                        attempt.label
                    ));
                }
                with_cov(rc.cedar.coverage, |cov| {
                    cov.hit_cedar_allow(rc.hint.policy_id.clone(), rc.hint.rule_id.clone());
                });
                host.verify_ledger()
            }
            Err(err) => Err(format!(
                "Cedar matrix ({}): authorized control for {}/{} action '{action_id}' failed: {err}",
                attempt.label, rc.hint.policy_id, rc.hint.rule_id
            )),
        }
    } else {
        match outcome {
            Err(ActionRefusal::PermissionDenied { .. }) => {
                assert_zero_ledger_writes(host, ledger_before, action_id, attempt.label)?;
                host.verify_ledger()
            }
            Err(other) => Err(format!(
                "Cedar matrix ({}): expected PermissionDenied for {}/{} action '{action_id}', got non-authz error: {other}",
                attempt.label, rc.hint.policy_id, rc.hint.rule_id
            )),
            Ok(_) => Err(format!(
                "Cedar matrix ({}): action '{action_id}' in {}/{} unexpectedly succeeded",
                attempt.label, rc.hint.policy_id, rc.hint.rule_id
            )),
        }
    }
}

fn prepare_action_fixture<H: PackHost>(
    rc: &RuleActionCtx<'_>,
    target: &ActionTarget<'_>,
    host: &mut H,
    obj_id: &str,
    seq: u64,
) -> Result<JsonObject, String> {
    let ctx = rc.cedar;
    let rule = rc.rule;
    let admitted = target.admitted;
    let effect = target.effect;
    let object_type = target.object_type;
    let eff_target = effect.target();
    let mut values = synthesize_object(ctx.pack, object_type, obj_id, seq);
    values.insert(
        eff_target.object_id_field.to_owned(),
        JsonValue::String(obj_id.to_owned()),
    );
    apply_rule_conditions_to_object(rule, &mut values);
    fill_missing_action_input_fields(&admitted.root.input_schema, &mut values, obj_id, seq);

    match effect {
        ActionEffectProgram::FireTransition {
            lifecycle: lc_prop,
            transition: tr_id,
            ..
        } => {
            seed_transition_fixture(
                rc,
                target,
                &TransitionFixtureSpec {
                    lc_prop,
                    tr_id,
                    object_id_field: eff_target.object_id_field,
                    obj_id,
                    seq,
                },
                host,
            )?;
        }
        ActionEffectProgram::UpsertInputObject { .. }
        | ActionEffectProgram::MutateParentWithRows { .. } => {
            seed_create_links_and_slice_targets(ctx, admitted, &values, host, seq)?;
        }
    }

    encode_action_wire_input(&admitted.root.input_schema, &mut values);
    Ok(values)
}

fn fill_missing_action_input_fields(
    schema: &BTreeMap<String, IrJsonValue>,
    values: &mut JsonObject,
    obj_id: &str,
    seq: u64,
) {
    let Some(props) = schema.get("properties").and_then(IrJsonValue::as_object) else {
        return;
    };
    for (name, prop_schema) in props {
        if values.contains_key(name) {
            continue;
        }
        let prop_obj = prop_schema.as_object();
        let is_decimal = prop_obj
            .and_then(|obj| obj.get("nornType"))
            .and_then(IrJsonValue::as_object)
            .and_then(|nt| nt.get("kind"))
            .and_then(IrJsonValue::as_string)
            == Some("decimal");
        let synthesized = if is_decimal {
            JsonValue::String("10.00".to_owned())
        } else {
            match prop_obj
                .and_then(|obj| obj.get("type"))
                .and_then(IrJsonValue::as_string)
            {
                Some("boolean") => JsonValue::Bool(true),
                Some("integer") => JsonValue::from(10_i64),
                Some("number") => JsonValue::String("10.00".to_owned()),
                Some(_) | None => JsonValue::String(format!("{name}-{obj_id}-{seq}")),
            }
        };
        values.insert(name.clone(), synthesized);
    }
}

fn encode_action_wire_input(schema: &BTreeMap<String, IrJsonValue>, values: &mut JsonObject) {
    let Some(props) = schema.get("properties").and_then(IrJsonValue::as_object) else {
        return;
    };
    values.retain(|k, _| props.contains_key(k));
    for (name, prop_schema) in props {
        let is_decimal = prop_schema
            .as_object()
            .and_then(|obj| obj.get("nornType"))
            .and_then(IrJsonValue::as_object)
            .and_then(|nt| nt.get("kind"))
            .and_then(IrJsonValue::as_string)
            == Some("decimal");
        if is_decimal && let Some(JsonValue::Number(dec)) = values.get(name) {
            let wire = dec.to_string();
            values.insert(name.clone(), JsonValue::String(wire));
        }
    }
}

struct TransitionFixtureSpec<'a> {
    lc_prop: &'a PropertyId,
    tr_id: &'a TransitionId,
    object_id_field: &'a str,
    obj_id: &'a str,
    seq: u64,
}

fn seed_transition_fixture<H: PackHost>(
    rc: &RuleActionCtx<'_>,
    target: &ActionTarget<'_>,
    spec: &TransitionFixtureSpec<'_>,
    host: &mut H,
) -> Result<(), String> {
    let ctx = rc.cedar;
    let object_type = target.object_type;
    let lifecycles = ctx.catalog.lifecycles(&object_type.name);
    let lc = lifecycles.get(spec.lc_prop).ok_or_else(|| {
        format!(
            "Cedar fixture: {}.{} lifecycle not found for '{}'",
            object_type.name.as_str(),
            spec.lc_prop.as_str(),
            target.admitted.root.action_id
        )
    })?;
    let tr = lc
        .transitions
        .iter()
        .find(|t| t.id == *spec.tr_id)
        .ok_or_else(|| {
            format!(
                "Cedar fixture: transition '{}' not found on {}.{}",
                spec.tr_id.as_str(),
                object_type.name.as_str(),
                spec.lc_prop.as_str()
            )
        })?;
    let paths = bfs_paths_to_states(lc);
    let (init_state, prefix) = tr
        .from
        .iter()
        .find_map(|from| paths.get(from).cloned())
        .ok_or_else(|| {
            format!(
                "Cedar fixture: no reachable source state for {}.{}",
                object_type.name.as_str(),
                spec.tr_id.as_str()
            )
        })?;
    let mut path_guards = collect_prefix_guards(lc, &prefix);
    path_guards.extend(tr.guards.iter().cloned());
    let mut seeded = synthesize_guard_satisfying(
        ctx.pack,
        object_type,
        lc,
        &path_guards,
        spec.obj_id,
        spec.seq,
    );
    seeded.insert(
        spec.object_id_field.to_owned(),
        JsonValue::String(spec.obj_id.to_owned()),
    );
    seeded.insert(
        spec.lc_prop.as_str().to_owned(),
        JsonValue::String(init_state),
    );
    apply_rule_conditions_to_object(rc.rule, &mut seeded);
    seed_and_walk_object(
        host,
        SeedWalkSpec {
            pack: ctx.pack,
            catalog: ctx.catalog,
            object_type,
            prop: spec.lc_prop.as_str(),
            id: spec.obj_id,
            values: seeded,
            path: &prefix,
        },
    )
}

fn seed_create_links_and_slice_targets<H: PackHost>(
    ctx: &CedarCtx<'_>,
    admitted: &AdmittedAction,
    input: &JsonObject,
    host: &mut H,
    seq: u64,
) -> Result<(), String> {
    let mut seeded_keys: BTreeSet<(String, String)> = BTreeSet::new();
    let desired_states = parse_slice_precondition_states(&admitted.root.preconditions);

    for slice_spec in &admitted.root.slice {
        if let Some(prop_name) = slice_input_key_field(&slice_spec.key)
            && let Some(JsonValue::String(target_id)) = input.get(prop_name)
        {
            let target_ty_name = slice_spec.object_type.as_str();
            if seeded_keys.insert((target_ty_name.to_owned(), target_id.clone())) {
                seed_related_target(
                    ctx,
                    host,
                    target_ty_name,
                    target_id,
                    desired_states.get(&slice_spec.name),
                    seq,
                )?;
            }
        }
    }
    for link in &admitted.root.create_links {
        if let Some(JsonValue::String(target_id)) = input.get(&link.target_id_field) {
            let target_ty_name = link.target_type.as_str();
            if seeded_keys.insert((target_ty_name.to_owned(), target_id.clone())) {
                seed_related_target(ctx, host, target_ty_name, target_id, None, seq)?;
            }
        }
    }
    Ok(())
}

fn slice_input_key_field(key_spec: &SliceKey) -> Option<&str> {
    match key_spec {
        SliceKey::Property { from, property } if from == "input" => Some(property.as_str()),
        SliceKey::Property { .. }
        | SliceKey::Parameter { .. }
        | SliceKey::TriggerObject
        | SliceKey::TriggerBefore
        | SliceKey::TriggerAfter
        | SliceKey::TriggerChanges
        | SliceKey::Link { .. } => None,
    }
}

fn parse_slice_precondition_states(
    preconditions: &[ZenExpression],
) -> BTreeMap<String, (String, String)> {
    let mut out = BTreeMap::new();
    for expr in preconditions {
        for conj in expr.as_str().split(" and ").flat_map(|s| s.split("&&")) {
            let clause = conj
                .trim()
                .trim_start_matches('(')
                .trim_end_matches(')')
                .trim();
            let Some((lhs, rhs)) = clause.split_once("==") else {
                continue;
            };
            let Some(rest) = lhs
                .trim()
                .trim_start_matches('(')
                .trim()
                .strip_prefix("slice.")
            else {
                continue;
            };
            let mut parts = rest.split('.');
            let (Some(slice_name), Some(prop_name)) = (parts.next(), parts.next()) else {
                continue;
            };
            let desired = rhs
                .trim()
                .trim_end_matches(')')
                .trim()
                .trim_matches('\'')
                .trim_matches('"');
            if !desired.is_empty() {
                out.insert(
                    slice_name.to_owned(),
                    (prop_name.to_owned(), desired.to_owned()),
                );
            }
        }
    }
    out
}

fn seed_related_target<H: PackHost>(
    ctx: &CedarCtx<'_>,
    host: &mut H,
    target_ty_name: &str,
    target_id: &str,
    desired_state: Option<&(String, String)>,
    seq: u64,
) -> Result<(), String> {
    let target_ty = ctx
        .pack
        .object_types
        .iter()
        .find(|ty| ty.name.as_str() == target_ty_name)
        .ok_or_else(|| format!("Cedar fixture: related type '{target_ty_name}' not in pack"))?;
    if let Some((lc_prop, target_state)) = desired_state {
        let lifecycles = ctx.catalog.lifecycles(&target_ty.name);
        if let Some(lc) = lifecycles.values().find(|l| l.property.as_str() == lc_prop) {
            let paths = bfs_paths_to_states(lc);
            if let Some((init_state, prefix)) = paths.get(target_state) {
                let guards = collect_prefix_guards(lc, prefix);
                let mut vals =
                    synthesize_guard_satisfying(ctx.pack, target_ty, lc, &guards, target_id, seq);
                vals.insert(lc_prop.clone(), JsonValue::String(init_state.clone()));
                return seed_and_walk_object(
                    host,
                    SeedWalkSpec {
                        pack: ctx.pack,
                        catalog: ctx.catalog,
                        object_type: target_ty,
                        prop: lc_prop,
                        id: target_id,
                        values: vals,
                        path: prefix,
                    },
                );
            }
        }
    }
    let key = FactKey::new(target_ty_name, target_id);
    let mut graph = collect_required_link_graph(ctx.pack, target_ty, &key, seq)?;
    let vals = synthesize_object(ctx.pack, target_ty, target_id, seq);
    graph.setup_writes.push(FactWrite {
        key,
        expected: FactVersion::ABSENT,
        mutation: FactMutation::Put { values: vals },
    });
    graph.setup_links.extend(graph.direct_links);
    host.commit(CellCommit {
        writes: graph.setup_writes,
        links: graph.setup_links,
        ..CellCommit::empty()
    })
    .map(|_| ())
    .map_err(|err| format!("Cedar fixture: seed related {target_ty_name}/{target_id}: {err}"))
}

fn apply_rule_conditions_to_object(rule: &AccessRule, values: &mut JsonObject) {
    for cond in &rule.all_of {
        let AccessCondition::Resource(rc) = cond else {
            continue;
        };
        match &rc.value {
            ResourceOperand::Literal(lit) => {
                let val = match lit {
                    AccessLiteral::Bool(b) => JsonValue::Bool(if rc.op == ResourceCompareOp::Ne {
                        !*b
                    } else {
                        *b
                    }),
                    AccessLiteral::Long(n) => {
                        let adjusted = match rc.op {
                            ResourceCompareOp::Eq
                            | ResourceCompareOp::Le
                            | ResourceCompareOp::Ge
                            | ResourceCompareOp::In => *n,
                            ResourceCompareOp::Ne | ResourceCompareOp::Gt => n.saturating_add(1),
                            ResourceCompareOp::Lt => n.saturating_sub(1),
                        };
                        JsonValue::from(adjusted)
                    }
                    AccessLiteral::String(s) => {
                        JsonValue::String(if rc.op == ResourceCompareOp::Ne {
                            format!("not-{s}")
                        } else {
                            s.clone()
                        })
                    }
                };
                values.insert(rc.name.clone(), val);
            }
            ResourceOperand::PrincipalAttribute(_) => {
                let text = if rc.op == ResourceCompareOp::Ne {
                    "actor-requester"
                } else {
                    "shared-attr"
                };
                values.insert(rc.name.clone(), JsonValue::String(text.to_owned()));
            }
        }
    }
}

fn project_resource_attrs_from_values(
    target_values: Option<&JsonObject>,
    schema: &BTreeMap<String, CedarAttribute>,
) -> BTreeMap<String, AttrValue> {
    let mut out = BTreeMap::new();
    let Some(values) = target_values else {
        return out;
    };
    for (name, kind) in schema {
        let Some(val) = values.get(name) else {
            continue;
        };
        let converted = match (kind, val) {
            (CedarAttribute::Bool, JsonValue::Bool(b)) => Some(AttrValue::Bool(*b)),
            (CedarAttribute::Long, JsonValue::Number(d)) => {
                d.to_string().parse::<i64>().ok().map(AttrValue::Long)
            }
            (CedarAttribute::String, JsonValue::String(s)) => Some(AttrValue::String(s.clone())),
            (
                CedarAttribute::Bool
                | CedarAttribute::Long
                | CedarAttribute::String
                | CedarAttribute::Decimal
                | CedarAttribute::Link { .. },
                _,
            ) => None,
        };
        if let Some(attr) = converted {
            out.insert(name.clone(), attr);
        }
    }
    out
}

fn build_principal_attrs_for_actor(
    actor: &str,
    rule: &AccessRule,
    principal_attrs: &BTreeMap<String, CedarPrincipalAttribute>,
) -> BTreeMap<String, AttrValue> {
    let mut prin = BTreeMap::new();
    for (name, kind) in principal_attrs {
        let val = match kind {
            CedarPrincipalAttribute::Bool => AttrValue::Bool(true),
            CedarPrincipalAttribute::Long => AttrValue::Long(10),
            CedarPrincipalAttribute::String => AttrValue::String(actor.to_owned()),
            CedarPrincipalAttribute::BoolSet => AttrValue::Set(vec![AttrValue::Bool(true)]),
            CedarPrincipalAttribute::LongSet => AttrValue::Set(vec![AttrValue::Long(10)]),
            CedarPrincipalAttribute::StringSet => {
                AttrValue::Set(vec![AttrValue::String(actor.to_owned())])
            }
        };
        prin.insert(name.clone(), val);
    }
    for cond in &rule.all_of {
        if let AccessCondition::Resource(ResourceCondition {
            op,
            value: ResourceOperand::PrincipalAttribute(pa),
            ..
        }) = cond
            && *op != ResourceCompareOp::Ne
        {
            let p_name = pa.principal_attribute.clone();
            if matches!(
                principal_attrs.get(&p_name),
                Some(CedarPrincipalAttribute::StringSet)
            ) {
                prin.insert(
                    p_name,
                    AttrValue::Set(vec![AttrValue::String("shared-attr".to_owned())]),
                );
            } else {
                prin.insert(p_name, AttrValue::String("shared-attr".to_owned()));
            }
        }
    }
    prin
}

fn build_rule_pdp(
    policy_set: &ValidatedPolicySet,
    action_id: &str,
    scope: &ScopeKey,
    foreign_scope: &ScopeKey,
) -> Result<Arc<Pdp>, String> {
    let grant = |subject: &str, grant_scope: &ScopeKey| CapabilityGrant {
        subject_id: subject.to_owned(),
        actor_kind: ActorKind::User,
        agent_session_id: None,
        scope: Some(grant_scope.clone()),
        capabilities: vec![action_id.to_owned()],
        binding: None,
    };
    let grants = vec![
        grant("actor-allowed", scope),
        grant("actor-unauth", scope),
        grant("actor-requester", scope),
        grant("actor-allowed", foreign_scope),
    ];
    Pdp::new(policy_set.clone(), &grants)
        .map(Arc::new)
        .map_err(|err| format!("Pdp: {err}"))
}

fn build_effective_scope(
    dimensions: &[ScopeDimension],
    action_scope_required: Option<&[String]>,
    org: &str,
) -> ScopeKey {
    let has_dim = |dim: ScopeDimension, key: &str| {
        dimensions.contains(&dim)
            || action_scope_required.is_some_and(|req| req.iter().any(|s| s == key))
    };
    ScopeKey {
        org: org.to_owned(),
        tenant: has_dim(ScopeDimension::Tenant, "tenantId").then(|| "tenant-mbt".to_owned()),
        workspace: has_dim(ScopeDimension::Workspace, "workspaceId").then(|| "ws-mbt".to_owned()),
        environment: has_dim(ScopeDimension::Environment, "environmentId")
            .then(|| "dev".to_owned()),
        business_unit: has_dim(ScopeDimension::BusinessUnit, "businessUnitId")
            .then(|| "bu-mbt".to_owned()),
    }
}

fn scope_key_to_map(scope: &ScopeKey) -> BTreeMap<String, String> {
    let mut map = BTreeMap::new();
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
