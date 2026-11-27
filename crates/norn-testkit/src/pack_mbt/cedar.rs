//! Full Cedar authorization and Separation-of-Duties (`SoD`) matrix driver (#1656).
//!
//! Compiles an [`AdmittedPack`]'s `AccessIR` policies through `norn-authz` and
//! drives real `commit_action` calls against a [`PackHost`] with attributes
//! derived directly from the committed fixture (`ActionAuthzRequest::target_values`):
//! 1. **Authorized control** (`Ok(_)` + ledger +1 + `verify_ledger()`),
//! 2. **Unauthorized role** (`Err(ActionRefusal::PermissionDenied)` + zero writes),
//! 3. **Foreign scope** (`Err(ActionRefusal::PermissionDenied)` + zero writes),
//! 4. **Self-approver `SoD`** (`Err(ActionRefusal::PermissionDenied)` + zero writes).

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use norn_kernel::authz::{
    AccessRequest, ActorKind, AttrValue, AuthzSchema, Binding, CapabilityGrant, DenyReason, Pdp,
    PdpDecision, ResourceInput, ScopeKey, UnboundProfile, VerifiedPrincipal, compile_all_projected,
    infer_principal_attributes,
};
use norn_kernel::cell_host::{JsonObject, JsonValue};
use norn_kernel::pack_mbt_support::cedar_projection::{
    CedarAttribute, CedarPrincipalAttribute, project_cedar_schema_with_catalog,
    project_cedar_schema_with_catalog_and_principal_attributes, project_host_access_schema,
};
use norn_kernel::pack_mbt_support::{
    AccessCondition, AccessIr, AccessLiteral, AccessRule, ActionEffectProgram, ObjectTypeDef,
    PdpExplanation, PropertyId, ResourceCompareOp, ResourceCondition, ResourceOperand,
    ScopeDimension, TransitionId, ValidatedPolicySet,
};
use norn_kernel::{ActionCall, ActionRefusal, DecisionTurn, TypeCatalog, World, commit_action};

use super::coverage::{DomainCoverage, ObservedPdpAuthorizer, PolicyRuleTarget};
use super::driver::{
    SeedWalkSpec, assert_zero_ledger_writes, bfs_paths_to_states, collect_prefix_guards,
    seed_and_walk_object,
};
use super::seed::{
    encode_action_wire_input, fill_missing_action_input_fields, seed_create_links_and_slice_targets,
};
use super::synth::{synthesize_guard_satisfying, synthesize_object};
use super::{AdmittedAction, AdmittedPack, PackHost};

const READ_ACTION: &str = "norn.read";

fn pack_declares_read_policies(pack: &AdmittedPack) -> bool {
    pack.access_policies.iter().any(|policy| {
        policy
            .rules
            .iter()
            .any(|rule| rule.actions.iter().any(|action| action == READ_ACTION))
    })
}

struct CedarCtx<'a> {
    pack: &'a AdmittedPack,
    catalog: &'a TypeCatalog<'a>,
    policy_set: &'a ValidatedPolicySet,
    projected_attrs: &'a BTreeMap<String, BTreeMap<String, CedarAttribute>>,
    principal_attrs: &'a BTreeMap<String, CedarPrincipalAttribute>,
    coverage: &'a Arc<Mutex<DomainCoverage>>,
    has_read_policies: bool,
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
        has_read_policies: pack_declares_read_policies(pack),
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
        if action_id == READ_ACTION {
            continue;
        }
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
    let pdp = build_rule_pdp(
        rc.cedar.policy_set,
        action_id,
        rc.cedar.has_read_policies,
        rc.scope,
        rc.foreign_scope,
    )?;
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
            actor: "actor-allowed",
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
    let fixture_seq = seq.saturating_add(1);
    for attempt in &attempts {
        run_action_attempt(rc, &target, attempt, fixture_seq, make_host, seq)?;
    }
    Ok(attempts.len())
}

struct AccessRequestTemplate {
    projected_attrs: BTreeMap<String, BTreeMap<String, CedarAttribute>>,
    prin_schema: BTreeMap<String, CedarPrincipalAttribute>,
    rule: AccessRule,
    roles: Vec<String>,
    caller_scope: ScopeKey,
    resource_scope: ScopeKey,
}

impl AccessRequestTemplate {
    fn build(
        &self,
        actor: &str,
        action_id: &str,
        object_type: &str,
        resource_id: &str,
        target_values: Option<&JsonObject>,
    ) -> AccessRequest {
        let res_attrs = self
            .projected_attrs
            .get(object_type)
            .or_else(|| self.projected_attrs.get(self.rule.resource_type.as_str()))
            .map_or_else(BTreeMap::new, |schema| {
                project_resource_attrs_from_values(target_values, schema)
            });
        let prin_attrs = build_principal_attrs_for_actor(actor, &self.rule, &self.prin_schema);
        AccessRequest::new(
            &VerifiedPrincipal::new(actor, ActorKind::User, None, self.roles.clone(), prin_attrs),
            Some(self.caller_scope.clone()),
            action_id,
            action_id,
            ResourceInput {
                object_type: object_type.to_owned(),
                id: resource_id.to_owned(),
                scope: self.resource_scope.clone(),
                attributes: res_attrs,
            },
            BTreeMap::new(),
            Binding::Unbound(UnboundProfile::HostedAction),
        )
    }
}

fn run_action_attempt<H, F>(
    rc: &RuleActionCtx<'_>,
    target: &ActionTarget<'_>,
    attempt: &ActionAttempt<'_>,
    fixture_seq: u64,
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
    let obj_id = format!("cedar-obj-{fixture_seq}");
    let input = prepare_action_fixture(rc, target, &mut host, &obj_id, fixture_seq)?;

    let ledger_before = host.ledger_len();
    let scope_map = scope_key_to_map(attempt.caller_scope);
    let template = Arc::new(AccessRequestTemplate {
        projected_attrs: rc.cedar.projected_attrs.clone(),
        prin_schema: rc.cedar.principal_attrs.clone(),
        rule: rc.rule.clone(),
        roles: attempt.roles.to_vec(),
        caller_scope: attempt.caller_scope.clone(),
        resource_scope: attempt.resource_scope.clone(),
    });
    let action_tpl = Arc::clone(&template);
    let base_authorizer = ObservedPdpAuthorizer::new(
        Arc::clone(target.pdp),
        Arc::clone(rc.cedar.coverage),
        Some(rc.hint.clone()),
        move |auth_req| {
            Ok(action_tpl.build(
                auth_req.actor,
                &auth_req.action.action_id,
                auth_req.target.object_type.as_str(),
                &auth_req.target.key,
                auth_req.target_values,
            ))
        },
    );
    // Only action-only fixtures opt into slice reads; packs declaring `norn.read`
    // policies enforce those policies through the read PDP on every slice read.
    let authorizer = if rc.cedar.has_read_policies {
        let read_tpl = Arc::clone(&template);
        let read_type_tpl = Arc::clone(&template);
        base_authorizer.with_read_pdp(
            move |read_req| {
                Ok(read_tpl.build(
                    read_req.actor,
                    READ_ACTION,
                    read_req.target.object_type.as_str(),
                    &read_req.target.key,
                    read_req.target_values,
                ))
            },
            move |read_type_req| {
                Ok(read_type_tpl.build(
                    read_type_req.actor,
                    READ_ACTION,
                    read_type_req.object_type.as_str(),
                    "*",
                    None,
                ))
            },
        )
    } else {
        base_authorizer.allow_action_only_slice_reads()
    };

    let call = ActionCall {
        action: &target.admitted.root,
        action_artifact_id: &target.admitted.artifact_id,
        action_version: &target.admitted.version,
        scope: scope_map,
        actor: attempt.actor.to_owned(),
        approved_intent: None,
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
    let explanation = authorizer.last_explanation();
    check_action_attempt_outcome(
        rc,
        &host,
        action_id,
        attempt,
        ledger_before,
        explanation.as_ref(),
        outcome,
    )
}

fn check_action_attempt_outcome<H: PackHost>(
    rc: &RuleActionCtx<'_>,
    host: &H,
    action_id: &str,
    attempt: &ActionAttempt<'_>,
    ledger_before: usize,
    explanation: Option<&PdpExplanation>,
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
                let determined_by_rule = explanation.is_some_and(|exp| {
                    exp.decision == PdpDecision::Allow
                        && exp.determining_policies.iter().any(|p| {
                            p.policy_id.as_deref() == Some(rc.hint.policy_id.as_str())
                                && p.rule_id.as_deref() == Some(rc.hint.rule_id.as_str())
                        })
                });
                if !determined_by_rule {
                    return Err(format!(
                        "Cedar matrix ({}): authorized control for '{action_id}' was not determined by {}/{}: explanation={explanation:?}",
                        attempt.label, rc.hint.policy_id, rc.hint.rule_id
                    ));
                }
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
                let denied_by_access_policy = explanation
                    .is_some_and(|exp| exp.decision == PdpDecision::Deny(DenyReason::Access));
                if !denied_by_access_policy {
                    return Err(format!(
                        "Cedar matrix ({}): expected DenyReason::Access for {}/{} action '{action_id}', got explanation={explanation:?}",
                        attempt.label, rc.hint.policy_id, rc.hint.rule_id
                    ));
                }
                assert_zero_ledger_writes(host, ledger_before, action_id, attempt.label)?;
                host.verify_ledger()
            }
            Err(ActionRefusal::TargetNotFound) if rc.cedar.has_read_policies => {
                let denied_by_access_policy = explanation
                    .is_some_and(|exp| exp.decision == PdpDecision::Deny(DenyReason::Access));
                if !denied_by_access_policy {
                    return Err(format!(
                        "Cedar matrix ({}): expected read-policy DenyReason::Access for {}/{} action '{action_id}', got explanation={explanation:?}",
                        attempt.label, rc.hint.policy_id, rc.hint.rule_id
                    ));
                }
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
            seed_create_links_and_slice_targets(
                ctx.pack,
                ctx.catalog,
                admitted,
                &mut values,
                host,
                seq,
            )?;
        }
    }

    encode_action_wire_input(&admitted.root.input_schema, &mut values);
    // ACTION-ACTOR-1: a stamped field comes from the principal, never the input.
    values.retain(|field, _| !admitted.root.actor_stamped_fields().contains(field));
    Ok(values)
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
    include_read_capability: bool,
    scope: &ScopeKey,
    foreign_scope: &ScopeKey,
) -> Result<Arc<Pdp>, String> {
    let mut capabilities = vec![action_id.to_owned()];
    if include_read_capability {
        capabilities.push(READ_ACTION.to_owned());
    }
    let grant = |subject: &str, grant_scope: &ScopeKey| CapabilityGrant {
        subject_id: subject.to_owned(),
        actor_kind: ActorKind::User,
        agent_session_id: None,
        scope: Some(grant_scope.clone()),
        capabilities: capabilities.clone(),
        binding: None,
    };
    let grants = vec![
        grant("actor-allowed", scope),
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
