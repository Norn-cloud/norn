//! Full Cedar authorization and Separation-of-Duties (SoD) matrix driver (#1656).
//!
//! Compiles an [`AdmittedPack`]'s `AccessIR` policies through `norn-authz` and
//! verifies every rule against authorized actors, unauthorized roles, foreign
//! scopes, and self-approvers (`PermissionDenied` + zero ledger writes).

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use norn_kernel::authz::{
    AccessRequest, ActorKind, AttrValue, AuthzSchema, CapabilityGrant, Delegation, Pdp,
    PrincipalInput, ResourceInput, ScopeKey, compile_all_projected, infer_principal_attributes,
};
use norn_kernel::cell_host::{FactKey, FactMutation, FactVersion, FactWrite, JsonValue};
use norn_kernel::norn_authz::ValidatedPolicySet;
use norn_kernel::norn_authz::norn_projections::{
    CedarAttribute, CedarPrincipalAttribute, project_cedar_schema_with_catalog,
    project_cedar_schema_with_catalog_and_principal_attributes, project_host_access_schema,
};
use norn_kernel::norn_ir::ActionEffectProgram;
use norn_kernel::norn_ir::access::{
    AccessCondition, AccessIr, AccessLiteral, AccessRule, ResourceCompareOp, ResourceCondition,
    ResourceOperand, ScopeDimension,
};
use norn_kernel::{
    ActionCall, ActionRefusal, CellCommit, DecisionTurn, TypeCatalog, World, commit_action,
};

use super::coverage::{DomainCoverage, ObservedPdpAuthorizer, PolicyRuleTarget};
use super::driver::{assert_zero_ledger_writes, with_cov};
use super::synth::synthesize_object;
use super::{AdmittedPack, PackHost};

struct CedarCtx<'a> {
    pack: &'a AdmittedPack,
    catalog: &'a TypeCatalog<'a>,
    policy_set: &'a ValidatedPolicySet,
    projected_attrs: &'a BTreeMap<String, BTreeMap<String, CedarAttribute>>,
    principal_attrs: &'a BTreeMap<String, CedarPrincipalAttribute>,
    coverage: &'a Arc<Mutex<DomainCoverage>>,
}

/// Runs the exhaustive Cedar & SoD matrix across every `AccessIR` rule in `pack`.
///
/// # Errors
///
/// Returns an error if Cedar compilation fails, an authorized actor is denied,
/// an unauthorized/foreign-scope/self-approver request is allowed, or a denial
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

    let rule_ctx = RuleActionCtx {
        cedar: ctx,
        rule,
        scope: &scope,
        foreign_scope: &foreign_scope,
        allowed_roles: &allowed_roles,
        hint: &hint,
        res_ok: &res_ok,
        prin_ok: &prin_ok,
    };
    for action_id in &rule.actions {
        let checked = verify_rule_action_matrix(&rule_ctx, action_id, make_host, seq)?;
        count = count.saturating_add(checked);
    }
    Ok(count)
}

struct RuleActionCtx<'a> {
    cedar: &'a CedarCtx<'a>,
    rule: &'a AccessRule,
    scope: &'a ScopeKey,
    foreign_scope: &'a ScopeKey,
    allowed_roles: &'a [String],
    hint: &'a PolicyRuleTarget,
    res_ok: &'a BTreeMap<String, AttrValue>,
    prin_ok: &'a BTreeMap<String, AttrValue>,
}

fn verify_rule_action_matrix<H, F>(
    rc: &RuleActionCtx<'_>,
    action_id: &str,
    make_host: &mut F,
    seq: &mut u64,
) -> Result<usize, String>
where
    H: PackHost,
    F: FnMut(&str) -> Result<H, String>,
{
    let ctx = rc.cedar;
    let rule = rc.rule;
    let scope = rc.scope;
    let foreign_scope = rc.foreign_scope;
    let allowed_roles = rc.allowed_roles;
    let hint = rc.hint;
    let res_ok = rc.res_ok;
    let prin_ok = rc.prin_ok;
    let pdp = build_rule_pdp(ctx.policy_set, action_id, scope, foreign_scope)?;
    let mut count = 0usize;

    let allow_req = make_access_request(
        "actor-allowed",
        allowed_roles.to_vec(),
        (scope, scope),
        (action_id, rule.resource_type.as_str(), "obj-1"),
        (res_ok.clone(), prin_ok.clone()),
    );
    let allow_expl = pdp.explain(&allow_req);
    if !allow_expl.decision.is_allow() {
        return Err(format!(
            "Cedar matrix: expected Allow for {}/{} action '{action_id}', got {:?}",
            hint.policy_id, hint.rule_id, allow_expl.decision
        ));
    }
    with_cov(ctx.coverage, |cov| {
        cov.hit_cedar_allow(hint.policy_id.clone(), hint.rule_id.clone());
    });
    count = count.saturating_add(1);

    if !allowed_roles.is_empty() {
        let deny_req = make_access_request(
            "actor-unauth",
            vec!["__mbt_unauthorized_role__".to_owned()],
            (scope, scope),
            (action_id, rule.resource_type.as_str(), "obj-1"),
            (res_ok.clone(), prin_ok.clone()),
        );
        assert_denied(&pdp, &deny_req, "unauthorized role", hint, action_id)?;
        verify_action_denied_zero_writes(
            ctx,
            action_id,
            &pdp,
            hint.clone(),
            deny_req,
            make_host,
            seq,
        )?;
        count = count.saturating_add(1);
    }

    let foreign_req = make_access_request(
        "actor-allowed",
        allowed_roles.to_vec(),
        (foreign_scope, scope),
        (action_id, rule.resource_type.as_str(), "obj-1"),
        (res_ok.clone(), prin_ok.clone()),
    );
    assert_denied(&pdp, &foreign_req, "foreign scope", hint, action_id)?;
    with_cov(ctx.coverage, |cov| {
        cov.hit_cedar_deny(hint.policy_id.clone(), hint.rule_id.clone());
    });
    count = count.saturating_add(1);

    if has_sod_condition(rule) {
        let (res_self, prin_self) =
            build_rule_attributes(rule, ctx.projected_attrs, ctx.principal_attrs, true);
        let self_req = make_access_request(
            "actor-requester",
            allowed_roles.to_vec(),
            (scope, scope),
            (action_id, rule.resource_type.as_str(), "obj-1"),
            (res_self, prin_self),
        );
        assert_denied(&pdp, &self_req, "self-approver SoD", hint, action_id)?;
        verify_action_denied_zero_writes(
            ctx,
            action_id,
            &pdp,
            hint.clone(),
            self_req,
            make_host,
            seq,
        )?;
        count = count.saturating_add(1);
    }
    Ok(count)
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

fn resolve_denied_action_target<'a>(
    ctx: &'a CedarCtx<'_>,
    action_id: &str,
) -> Option<(
    &'a super::AdmittedAction,
    &'a ActionEffectProgram,
    &'a norn_kernel::norn_ir::ObjectTypeDef,
)> {
    let admitted = ctx
        .pack
        .actions
        .iter()
        .find(|a| a.root.action_id == action_id)?;
    let effect = admitted.root.effect_program.as_ref()?;
    let target = effect.target();
    let object_type = ctx
        .pack
        .object_types
        .iter()
        .find(|ty| ty.name.as_str() == target.object_kind)?;
    Some((admitted, effect, object_type))
}

fn prepare_denied_action_input<H: PackHost>(
    pack: &AdmittedPack,
    admitted: &super::AdmittedAction,
    effect: &ActionEffectProgram,
    object_type: &norn_kernel::norn_ir::ObjectTypeDef,
    host: &mut H,
    seq: u64,
) -> norn_kernel::cell_host::JsonObject {
    let target = effect.target();
    let obj_id = format!("cedar-obj-{seq}");
    let mut input = synthesize_object(pack, object_type, &obj_id, seq);
    input.insert(
        target.object_id_field.to_owned(),
        JsonValue::String(obj_id.clone()),
    );
    if let Some(allowed_props) = admitted
        .root
        .input_schema
        .get("properties")
        .and_then(norn_kernel::norn_ir::JsonValue::as_object)
    {
        input.retain(|k, _| allowed_props.contains_key(k));
    }
    if matches!(effect, ActionEffectProgram::FireTransition { .. }) {
        let seeded = synthesize_object(pack, object_type, &obj_id, seq);
        let _ = host.commit(CellCommit {
            writes: vec![FactWrite {
                key: FactKey::new(object_type.name.as_str(), &obj_id),
                expected: FactVersion::ABSENT,
                mutation: FactMutation::Put { values: seeded },
            }],
            ..CellCommit::empty()
        });
    }
    input
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
    let Some((admitted, effect, object_type)) = resolve_denied_action_target(ctx, action_id) else {
        with_cov(ctx.coverage, |cov| {
            cov.hit_cedar_deny(hint.policy_id, hint.rule_id);
        });
        return Ok(());
    };

    *seq = seq.saturating_add(1);
    let mut host = make_host(&format!("cedar-deny-{action_id}-{}", *seq))?;
    let input =
        prepare_denied_action_input(ctx.pack, admitted, effect, object_type, &mut host, *seq);

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
        Err(ActionRefusal::PermissionDenied { .. }) => {
            assert_zero_ledger_writes(&host, ledger_before, action_id, "Cedar denial")?;
            host.verify_ledger()
        }
        Err(_) => {
            assert_zero_ledger_writes(&host, ledger_before, action_id, "pre-authz refusal")?;
            with_cov(ctx.coverage, |cov| {
                cov.hit_cedar_deny(hint.policy_id, hint.rule_id);
            });
            host.verify_ledger()
        }
        Ok(_) => Err(format!(
            "Cedar denial check failed: action '{action_id}' unexpectedly succeeded"
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
            CedarAttribute::Decimal | CedarAttribute::Link { .. } => continue,
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
            CedarPrincipalAttribute::BoolSet => AttrValue::Set(vec![AttrValue::Bool(true)]),
            CedarPrincipalAttribute::LongSet => AttrValue::Set(vec![AttrValue::Long(10)]),
            CedarPrincipalAttribute::StringSet => {
                AttrValue::Set(vec![AttrValue::String("actor-allowed".to_owned())])
            }
        };
        prin.insert(name.clone(), val);
    }
    prin
}

fn apply_resource_condition(
    rc: &ResourceCondition,
    principal_attrs: &BTreeMap<String, CedarPrincipalAttribute>,
    violate_sod: bool,
    res: &mut BTreeMap<String, AttrValue>,
    prin: &mut BTreeMap<String, AttrValue>,
) {
    match &rc.value {
        ResourceOperand::Literal(lit) => {
            let val = match lit {
                AccessLiteral::Bool(b) => AttrValue::Bool(if rc.op == ResourceCompareOp::Ne {
                    !*b
                } else {
                    *b
                }),
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
                res.insert(
                    rc.name.clone(),
                    AttrValue::String("actor-requester".to_owned()),
                );
                let caller = if violate_sod {
                    "actor-requester"
                } else {
                    "actor-allowed"
                };
                prin.insert(p_name, AttrValue::String(caller.to_owned()));
            } else {
                res.insert(rc.name.clone(), AttrValue::String("shared-attr".to_owned()));
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
